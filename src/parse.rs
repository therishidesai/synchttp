use crate::body::parse_chunked_body;
use crate::types::{Header, Method, ParseError, Request, ServerConfig, Version};

#[derive(Debug)]
pub(crate) struct ParsedRequest {
    pub(crate) request: Request,
    pub(crate) consumed: usize,
    pub(crate) connection_close: bool,
}

pub(crate) fn try_parse_request(
    bytes: &[u8],
    config: &ServerConfig,
) -> Result<Option<ParsedRequest>, ParseError> {
    let head_end = match find_double_crlf(bytes) {
        Some(index) => index + 4,
        None => {
            if bytes.len() > config.max_header_bytes {
                return Err(ParseError::HeaderTooLarge);
            }
            return Ok(None);
        }
    };

    if head_end > config.max_header_bytes {
        return Err(ParseError::HeaderTooLarge);
    }

    let head = &bytes[..head_end - 4];
    let line_slices = split_head_lines(head);
    let mut lines = line_slices.into_iter();
    let request_line = lines
        .next()
        .ok_or(ParseError::BadRequest("missing request line"))?;

    if request_line.len() > config.max_request_line_bytes {
        return Err(ParseError::BadRequest("request line too long"));
    }

    let request_line = std::str::from_utf8(request_line)
        .map_err(|_| ParseError::BadRequest("request line must be utf-8 compatible"))?;
    let mut parts = request_line.split(' ');
    let method = parts
        .next()
        .ok_or(ParseError::BadRequest("missing method"))?;
    let target = parts
        .next()
        .ok_or(ParseError::BadRequest("missing request target"))?;
    let version = parts
        .next()
        .ok_or(ParseError::BadRequest("missing version"))?;

    if parts.next().is_some() || method.is_empty() || target.is_empty() {
        return Err(ParseError::BadRequest("malformed request line"));
    }

    if !method.bytes().all(is_tchar) {
        return Err(ParseError::BadRequest("invalid method token"));
    }

    let version = match version {
        "HTTP/1.0" => Version::Http10,
        "HTTP/1.1" => Version::Http11,
        _ => return Err(ParseError::BadRequest("unsupported HTTP version")),
    };

    let path = normalize_path(target)?;
    let mut headers = Vec::new();
    let mut host_count = 0usize;
    let mut content_length_values = Vec::new();
    let mut transfer_encoding = None;
    let mut connection_values = Vec::new();

    for line in lines {
        if line.is_empty() {
            continue;
        }

        if matches!(line.first(), Some(b' ' | b'\t')) {
            return Err(ParseError::BadRequest(
                "obsolete line folding is not supported",
            ));
        }

        if headers.len() >= config.max_headers {
            return Err(ParseError::HeaderTooLarge);
        }

        let colon = line
            .iter()
            .position(|byte| *byte == b':')
            .ok_or(ParseError::BadRequest("header missing colon"))?;
        let name = &line[..colon];
        let value = trim_ows(&line[colon + 1..]);

        if name.is_empty() || !name.iter().copied().all(is_tchar) {
            return Err(ParseError::BadRequest("invalid header name"));
        }

        let name_text = std::str::from_utf8(name)
            .map_err(|_| ParseError::BadRequest("header name must be ascii"))?;
        let value_text = String::from_utf8_lossy(value).into_owned();

        if name_text.eq_ignore_ascii_case("host") {
            host_count += 1;
        } else if name_text.eq_ignore_ascii_case("content-length") {
            content_length_values.push(value_text.clone());
        } else if name_text.eq_ignore_ascii_case("transfer-encoding") {
            transfer_encoding = Some(value_text.clone());
        } else if name_text.eq_ignore_ascii_case("connection") {
            connection_values.push(value_text.clone());
        }

        headers.push(Header::new(name_text, value_text));
    }

    if version == Version::Http11 && host_count != 1 {
        return Err(ParseError::BadRequest(
            "HTTP/1.1 requests require exactly one Host header",
        ));
    }

    let content_length = parse_content_length(&content_length_values)?;
    let body_bytes = &bytes[head_end..];

    let (body, body_consumed) = match (transfer_encoding.as_deref(), content_length) {
        (Some(_), Some(_)) => {
            return Err(ParseError::BadRequest(
                "Transfer-Encoding and Content-Length cannot both be present",
            ))
        }
        (Some(value), None) => {
            if !is_chunked_transfer_encoding(value) {
                return Err(ParseError::NotImplemented(
                    "only Transfer-Encoding: chunked is supported",
                ));
            }

            match parse_chunked_body(body_bytes, config)? {
                Some((decoded, consumed)) => (decoded, consumed),
                None => return Ok(None),
            }
        }
        (None, Some(length)) => {
            if length > config.max_body_bytes {
                return Err(ParseError::PayloadTooLarge);
            }

            if body_bytes.len() < length {
                return Ok(None);
            }

            (body_bytes[..length].to_vec(), length)
        }
        (None, None) => (Vec::new(), 0),
    };

    let connection_close = should_close_connection(version, &connection_values);
    let request = Request::new(
        Method::new(method),
        target.to_string(),
        path,
        version,
        headers,
        body,
    );

    Ok(Some(ParsedRequest {
        request,
        consumed: head_end + body_consumed,
        connection_close,
    }))
}

fn normalize_path(target: &str) -> Result<String, ParseError> {
    if target == "*" {
        return Ok(String::from("*"));
    }

    if let Some(path) = target.strip_prefix('/') {
        let path_end = path.find('?').unwrap_or(path.len());
        return Ok(format!("/{}", &path[..path_end]));
    }

    if let Some(rest) = target
        .strip_prefix("http://")
        .or_else(|| target.strip_prefix("https://"))
    {
        let slash_index = rest.find('/');
        return Ok(match slash_index {
            Some(index) => {
                let suffix = &rest[index..];
                let path_end = suffix.find('?').unwrap_or(suffix.len());
                suffix[..path_end].to_string()
            }
            None => String::from("/"),
        });
    }

    Err(ParseError::BadRequest(
        "only origin-form, absolute-form, and asterisk-form targets are supported",
    ))
}

fn parse_content_length(values: &[String]) -> Result<Option<usize>, ParseError> {
    if values.is_empty() {
        return Ok(None);
    }

    let mut parsed_value = None;
    for value in values {
        for piece in value.split(',') {
            let piece = piece.trim();
            if piece.is_empty() || !piece.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(ParseError::BadRequest("invalid Content-Length"));
            }

            let number = piece
                .parse::<usize>()
                .map_err(|_| ParseError::BadRequest("invalid Content-Length"))?;

            match parsed_value {
                Some(existing) if existing != number => {
                    return Err(ParseError::BadRequest("conflicting Content-Length headers"))
                }
                None => parsed_value = Some(number),
                _ => {}
            }
        }
    }

    Ok(parsed_value)
}

fn should_close_connection(version: Version, values: &[String]) -> bool {
    let has_close = values.iter().any(|value| header_has_token(value, "close"));
    let has_keep_alive = values
        .iter()
        .any(|value| header_has_token(value, "keep-alive"));

    match version {
        Version::Http11 => has_close,
        Version::Http10 => !has_keep_alive,
    }
}

fn is_chunked_transfer_encoding(value: &str) -> bool {
    let mut tokens = value
        .split(',')
        .map(|token| token.trim())
        .filter(|token| !token.is_empty());
    matches!(tokens.next(), Some(token) if token.eq_ignore_ascii_case("chunked"))
        && tokens.next().is_none()
}

fn header_has_token(value: &str, wanted: &str) -> bool {
    value
        .split(',')
        .map(|token| token.trim())
        .any(|token| token.eq_ignore_ascii_case(wanted))
}

fn trim_ows(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !matches!(byte, b' ' | b'\t'))
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !matches!(byte, b' ' | b'\t'))
        .map(|index| index + 1)
        .unwrap_or(start);
    &bytes[start..end]
}

fn find_double_crlf(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}

fn split_head_lines(head: &[u8]) -> Vec<&[u8]> {
    let mut lines = Vec::new();
    let mut start = 0usize;

    while let Some(offset) = head[start..]
        .windows(2)
        .position(|window| window == b"\r\n")
    {
        let end = start + offset;
        lines.push(&head[start..end]);
        start = end + 2;
    }

    lines.push(&head[start..]);
    lines
}

fn is_tchar(byte: u8) -> bool {
    matches!(
        byte,
        b'!' | b'#'
            | b'$'
            | b'%'
            | b'&'
            | b'\''
            | b'*'
            | b'+'
            | b'-'
            | b'.'
            | b'^'
            | b'_'
            | b'`'
            | b'|'
            | b'~'
    ) || byte.is_ascii_alphanumeric()
}

#[cfg(test)]
mod tests {
    use super::try_parse_request;
    use crate::types::{ParseError, ServerConfig, Version};
    use proptest::prelude::*;

    fn build_request(
        method: &str,
        target: &str,
        version: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Vec<u8> {
        let mut request = format!("{} {} {}\r\n", method, target, version).into_bytes();
        for (name, value) in headers {
            request.extend_from_slice(name.as_bytes());
            request.extend_from_slice(b": ");
            request.extend_from_slice(value.as_bytes());
            request.extend_from_slice(b"\r\n");
        }
        request.extend_from_slice(b"\r\n");
        request.extend_from_slice(body);
        request
    }

    #[test]
    fn parses_simple_get_request() {
        let config = ServerConfig::default();
        let input = b"GET /health HTTP/1.1\r\nHost: example.test\r\n\r\n";
        let parsed = try_parse_request(input, &config).unwrap().unwrap();

        assert_eq!(parsed.request.method().as_str(), "GET");
        assert_eq!(parsed.request.path(), "/health");
        assert_eq!(parsed.request.version(), Version::Http11);
        assert_eq!(parsed.request.header("host"), Some("example.test"));
        assert!(parsed.request.body().is_empty());
    }

    #[test]
    fn rejects_conflicting_content_length() {
        let config = ServerConfig::default();
        let input = b"POST / HTTP/1.1\r\nHost: example.test\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\nhi";
        let error = try_parse_request(input, &config).unwrap_err();

        assert!(matches!(error, ParseError::BadRequest(_)));
    }

    #[test]
    fn parses_chunked_body() {
        let config = ServerConfig::default();
        let input = b"POST /echo HTTP/1.1\r\nHost: example.test\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n";
        let parsed = try_parse_request(input, &config).unwrap().unwrap();

        assert_eq!(parsed.request.body(), b"hello");
    }

    #[test]
    fn rejects_missing_host_for_http11() {
        let config = ServerConfig::default();
        let input = b"GET / HTTP/1.1\r\n\r\n";
        let error = try_parse_request(input, &config).unwrap_err();

        assert!(matches!(error, ParseError::BadRequest(_)));
    }

    proptest! {
        #[test]
        fn parser_never_panics_on_random_bytes(data in proptest::collection::vec(any::<u8>(), 0..512)) {
            let config = ServerConfig::default();
            let _ = try_parse_request(&data, &config);
        }

        #[test]
        fn valid_requests_parse_the_same_across_chunk_boundaries(
            method in prop_oneof![Just("GET".to_string()), Just("POST".to_string())],
            path in "(/[a-z]{0,8}){1,3}",
            body in proptest::collection::vec(any::<u8>(), 0..64),
            split_points in proptest::collection::vec(0usize..256usize, 0..8),
        ) {
            let mut request = format!("{} {} HTTP/1.1\r\nHost: example.test\r\n", method, path);
            request.push_str(&format!("Content-Length: {}\r\n", body.len()));
            request.push_str("\r\n");
            let mut bytes = request.into_bytes();
            bytes.extend_from_slice(&body);

            let config = ServerConfig::default();
            let direct = try_parse_request(&bytes, &config).unwrap().unwrap();
            let mut buffered = Vec::new();
            let mut offset = 0usize;
            let mut parsed = None;

            let mut split_points = split_points;
            split_points.push(bytes.len());
            split_points.sort_unstable();
            split_points.dedup();

            for point in split_points {
                let end = point.min(bytes.len());
                if end <= offset {
                    continue;
                }

                buffered.extend_from_slice(&bytes[offset..end]);
                offset = end;
                match try_parse_request(&buffered, &config).unwrap() {
                    Some(value) => {
                        parsed = Some(value);
                        break;
                    }
                    None => {}
                }
            }

            if parsed.is_none() && offset < bytes.len() {
                buffered.extend_from_slice(&bytes[offset..]);
                parsed = try_parse_request(&buffered, &config).unwrap();
            }

            let parsed = parsed.expect("request should parse once all bytes are buffered");
            prop_assert_eq!(direct.request.method(), parsed.request.method());
            prop_assert_eq!(direct.request.path(), parsed.request.path());
            prop_assert_eq!(direct.request.body(), parsed.request.body());
            prop_assert_eq!(direct.consumed, parsed.consumed);
        }

        #[test]
        fn complete_valid_requests_consume_all_bytes_and_prefixes_need_more_data(
            method in prop_oneof![Just("GET".to_string()), Just("POST".to_string()), Just("PUT".to_string())],
            path in "(/[a-z]{0,8}){1,3}",
            body in proptest::collection::vec(any::<u8>(), 0..32),
        ) {
            let request = build_request(
                &method,
                &path,
                "HTTP/1.1",
                &[
                    ("Host".to_string(), "example.test".to_string()),
                    ("Content-Length".to_string(), body.len().to_string()),
                ],
                &body,
            );
            let config = ServerConfig::default();
            let parsed = try_parse_request(&request, &config).unwrap().unwrap();

            prop_assert_eq!(parsed.consumed, request.len());
            prop_assert_eq!(parsed.request.body(), body.as_slice());

            for prefix_len in 0..request.len() {
                let prefix = &request[..prefix_len];
                prop_assert!(matches!(try_parse_request(prefix, &config), Ok(None)));
            }
        }

        #[test]
        fn matching_duplicate_content_length_headers_are_accepted(
            path in "(/[a-z]{0,8}){1,3}",
            body in proptest::collection::vec(any::<u8>(), 0..48),
        ) {
            let request = build_request(
                "POST",
                &path,
                "HTTP/1.1",
                &[
                    ("Host".to_string(), "example.test".to_string()),
                    ("Content-Length".to_string(), body.len().to_string()),
                    ("Content-Length".to_string(), body.len().to_string()),
                ],
                &body,
            );
            let config = ServerConfig::default();
            let parsed = try_parse_request(&request, &config).unwrap().unwrap();

            prop_assert_eq!(parsed.request.path(), path);
            prop_assert_eq!(parsed.request.body(), body.as_slice());
        }

        #[test]
        fn conflicting_content_length_headers_are_rejected(
            path in "(/[a-z]{0,8}){1,3}",
            first in 0usize..32,
            second in 0usize..32,
        ) {
            prop_assume!(first != second);
            let request = build_request(
                "POST",
                &path,
                "HTTP/1.1",
                &[
                    ("Host".to_string(), "example.test".to_string()),
                    ("Content-Length".to_string(), first.to_string()),
                    ("Content-Length".to_string(), second.to_string()),
                ],
                &[],
            );
            let config = ServerConfig::default();
            let error = try_parse_request(&request, &config).unwrap_err();

            prop_assert!(matches!(error, ParseError::BadRequest(_)));
        }

        #[test]
        fn transfer_encoding_and_content_length_are_rejected_together(
            path in "(/[a-z]{0,8}){1,3}",
        ) {
            let request = build_request(
                "POST",
                &path,
                "HTTP/1.1",
                &[
                    ("Host".to_string(), "example.test".to_string()),
                    ("Transfer-Encoding".to_string(), "chunked".to_string()),
                    ("Content-Length".to_string(), "0".to_string()),
                ],
                b"0\r\n\r\n",
            );
            let config = ServerConfig::default();
            let error = try_parse_request(&request, &config).unwrap_err();

            prop_assert!(matches!(error, ParseError::BadRequest(_)));
        }

        #[test]
        fn unsupported_transfer_encoding_is_rejected(
            path in "(/[a-z]{0,8}){1,3}",
            body in proptest::collection::vec(any::<u8>(), 0..24),
        ) {
            let request = build_request(
                "POST",
                &path,
                "HTTP/1.1",
                &[
                    ("Host".to_string(), "example.test".to_string()),
                    ("Transfer-Encoding".to_string(), "gzip".to_string()),
                ],
                &body,
            );
            let config = ServerConfig::default();
            let error = try_parse_request(&request, &config).unwrap_err();

            prop_assert!(matches!(error, ParseError::NotImplemented(_)));
        }

        #[test]
        fn absolute_form_targets_normalize_to_their_path(
            path in "(/[a-z]{0,8}){1,3}",
        ) {
            let target = format!("http://example.test{}?debug=true", path);
            let request = build_request(
                "GET",
                &target,
                "HTTP/1.1",
                &[("Host".to_string(), "example.test".to_string())],
                &[],
            );
            let config = ServerConfig::default();
            let parsed = try_parse_request(&request, &config).unwrap().unwrap();

            prop_assert_eq!(parsed.request.path(), path);
            prop_assert_eq!(parsed.request.target(), target);
        }

        #[test]
        fn http11_requires_exactly_one_host_header(
            path in "(/[a-z]{0,8}){1,3}",
            host_count in 0usize..4,
        ) {
            prop_assume!(host_count != 1);
            let mut headers = Vec::new();
            for index in 0..host_count {
                headers.push(("Host".to_string(), format!("example{}.test", index)));
            }
            let request = build_request("GET", &path, "HTTP/1.1", &headers, &[]);
            let config = ServerConfig::default();
            let error = try_parse_request(&request, &config).unwrap_err();

            prop_assert!(matches!(error, ParseError::BadRequest(_)));
        }
    }
}
