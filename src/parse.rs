use crate::body::parse_chunked_body;
use crate::types::{ParseError, ServerConfig};
use crate::{Method, Request, Uri, Version};
use http::header::{CONNECTION, CONTENT_LENGTH, HOST, TRANSFER_ENCODING};
use http::{HeaderMap, HeaderName, HeaderValue};

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
    let request_line_end = match find_crlf(bytes, 0) {
        Some(end) => end,
        None => {
            if bytes.len() > config.max_request_line_bytes {
                return Err(ParseError::BadRequest("request line too long"));
            }
            if bytes.len() > config.max_header_bytes {
                return Err(ParseError::HeaderTooLarge);
            }
            return Ok(None);
        }
    };

    if request_line_end > config.max_request_line_bytes {
        return Err(ParseError::BadRequest("request line too long"));
    }

    let mut raw_headers = vec![httparse::EMPTY_HEADER; config.max_headers];
    let mut parsed = httparse::Request::new(&mut raw_headers);
    let head_end = match parsed.parse(bytes) {
        Ok(httparse::Status::Complete(consumed)) => consumed,
        Ok(httparse::Status::Partial) => {
            if bytes.len() > config.max_header_bytes {
                return Err(ParseError::HeaderTooLarge);
            }
            return Ok(None);
        }
        Err(httparse::Error::TooManyHeaders) => return Err(ParseError::HeaderTooLarge),
        Err(_) => return Err(ParseError::BadRequest("malformed request")),
    };

    if head_end > config.max_header_bytes {
        return Err(ParseError::HeaderTooLarge);
    }

    let method = Method::from_bytes(
        parsed
            .method
            .ok_or(ParseError::BadRequest("missing method"))?
            .as_bytes(),
    )
    .map_err(|_| ParseError::BadRequest("invalid method token"))?;
    let target = parsed
        .path
        .ok_or(ParseError::BadRequest("missing request target"))?;
    let uri = normalize_target(target)?;
    let version = match parsed.version {
        Some(0) => Version::HTTP_10,
        Some(1) => Version::HTTP_11,
        _ => return Err(ParseError::BadRequest("unsupported HTTP version")),
    };

    let headers = convert_headers(parsed.headers)?;

    if version == Version::HTTP_11 && headers.get_all(HOST).iter().count() != 1 {
        return Err(ParseError::BadRequest(
            "HTTP/1.1 requests require exactly one Host header",
        ));
    }

    let content_length = parse_content_length(&headers)?;
    let body_bytes = &bytes[head_end..];
    let transfer_encoding_values = header_values(&headers, TRANSFER_ENCODING)?;

    let (body, body_consumed) = match (!transfer_encoding_values.is_empty(), content_length) {
        (true, Some(_)) => {
            return Err(ParseError::BadRequest(
                "Transfer-Encoding and Content-Length cannot both be present",
            ))
        }
        (true, None) => {
            if !is_chunked_transfer_encoding(&transfer_encoding_values) {
                return Err(ParseError::NotImplemented(
                    "only Transfer-Encoding: chunked is supported",
                ));
            }

            match parse_chunked_body(body_bytes, config)? {
                Some((decoded, consumed)) => (decoded, consumed),
                None => return Ok(None),
            }
        }
        (false, Some(length)) => {
            if length > config.max_body_bytes {
                return Err(ParseError::PayloadTooLarge);
            }

            if body_bytes.len() < length {
                return Ok(None);
            }

            (body_bytes[..length].to_vec(), length)
        }
        (false, None) => (Vec::new(), 0),
    };

    let connection_values = header_values(&headers, CONNECTION)?;
    let connection_close = should_close_connection(version, &connection_values);
    let mut request = Request::new(body);
    *request.method_mut() = method;
    *request.uri_mut() = uri;
    *request.version_mut() = version;
    *request.headers_mut() = headers;

    Ok(Some(ParsedRequest {
        request,
        consumed: head_end + body_consumed,
        connection_close,
    }))
}

fn normalize_target(target: &str) -> Result<Uri, ParseError> {
    if target == "*"
        || target.starts_with('/')
        || target.starts_with("http://")
        || target.starts_with("https://")
    {
        return target
            .parse::<Uri>()
            .map_err(|_| ParseError::BadRequest("invalid request target"));
    }

    Err(ParseError::BadRequest(
        "only origin-form, absolute-form, and asterisk-form targets are supported",
    ))
}

fn convert_headers(raw_headers: &[httparse::Header<'_>]) -> Result<HeaderMap, ParseError> {
    let mut headers = HeaderMap::with_capacity(raw_headers.len());

    for header in raw_headers {
        let name = HeaderName::from_bytes(header.name.as_bytes())
            .map_err(|_| ParseError::BadRequest("invalid header name"))?;
        let value = HeaderValue::from_bytes(header.value)
            .map_err(|_| ParseError::BadRequest("invalid header value"))?;
        headers.append(name, value);
    }

    Ok(headers)
}

fn parse_content_length(headers: &HeaderMap) -> Result<Option<usize>, ParseError> {
    let values = header_values(headers, CONTENT_LENGTH)?;
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

fn header_values<'a>(headers: &'a HeaderMap, name: HeaderName) -> Result<Vec<&'a str>, ParseError> {
    headers
        .get_all(name)
        .iter()
        .map(|value| {
            value
                .to_str()
                .map_err(|_| ParseError::BadRequest("header value must be visible ascii"))
        })
        .collect()
}

fn should_close_connection(version: Version, values: &[&str]) -> bool {
    let has_close = values.iter().any(|value| header_has_token(value, "close"));
    let has_keep_alive = values
        .iter()
        .any(|value| header_has_token(value, "keep-alive"));

    match version {
        Version::HTTP_11 => has_close,
        Version::HTTP_10 => !has_keep_alive,
        _ => true,
    }
}

fn is_chunked_transfer_encoding(values: &[&str]) -> bool {
    let mut tokens = values
        .iter()
        .flat_map(|value| value.split(','))
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

fn find_crlf(bytes: &[u8], start: usize) -> Option<usize> {
    bytes[start..]
        .windows(2)
        .position(|window| window == b"\r\n")
        .map(|position| start + position)
}

#[cfg(test)]
mod tests {
    use super::try_parse_request;
    use crate::types::{ParseError, ServerConfig};
    use crate::Version;
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
        assert_eq!(parsed.request.uri().path(), "/health");
        assert_eq!(parsed.request.version(), Version::HTTP_11);
        assert_eq!(
            parsed.request.headers().get("host").unwrap(),
            "example.test"
        );
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
            prop_assert_eq!(direct.request.uri(), parsed.request.uri());
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

            prop_assert_eq!(parsed.request.uri().path(), path);
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

            prop_assert_eq!(parsed.request.uri().path(), path);
            prop_assert_eq!(parsed.request.uri().to_string(), target);
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
