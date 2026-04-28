use crate::types::{Header, Method, Response, StatusCode, Version};

pub(crate) fn encode_response(
    version: Version,
    method: &Method,
    close_connection: bool,
    response: Response,
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(version.as_str().as_bytes());
    out.push(b' ');
    out.extend_from_slice(response.status().as_u16().to_string().as_bytes());
    out.push(b' ');
    out.extend_from_slice(response.status().reason_phrase().as_bytes());
    out.extend_from_slice(b"\r\n");

    let status = response.status();
    let status_forbids_body = is_body_forbidden(status);
    let head_only = method.as_str().eq_ignore_ascii_case("HEAD");
    let should_send_body = !status_forbids_body && !head_only;

    for header in response.headers() {
        if is_reserved_header(header) {
            continue;
        }
        write_header(&mut out, header.name(), header.value());
    }

    if close_connection {
        write_header(&mut out, "connection", "close");
    } else if version == Version::Http10 {
        write_header(&mut out, "connection", "keep-alive");
    }

    if !status_forbids_body {
        write_header(
            &mut out,
            "content-length",
            response.body().len().to_string().as_str(),
        );
    }

    out.extend_from_slice(b"\r\n");

    if should_send_body {
        out.extend_from_slice(response.body());
    }

    out
}

fn is_reserved_header(header: &Header) -> bool {
    header.name().eq_ignore_ascii_case("content-length")
        || header.name().eq_ignore_ascii_case("transfer-encoding")
        || header.name().eq_ignore_ascii_case("connection")
}

fn is_body_forbidden(status: StatusCode) -> bool {
    (100..200).contains(&status.as_u16()) || matches!(status.as_u16(), 204 | 304)
}

fn write_header(out: &mut Vec<u8>, name: &str, value: &str) {
    out.extend_from_slice(name.as_bytes());
    out.extend_from_slice(b": ");
    out.extend_from_slice(value.as_bytes());
    out.extend_from_slice(b"\r\n");
}

#[cfg(test)]
mod tests {
    use super::encode_response;
    use crate::types::{Method, Response, StatusCode, Version};

    #[test]
    fn head_response_suppresses_body_bytes() {
        let response = Response::text(StatusCode::OK, "hello");
        let encoded = encode_response(Version::Http11, &Method::new("HEAD"), false, response);
        let text = String::from_utf8(encoded).unwrap();

        assert!(text.contains("content-length: 5\r\n"));
        assert!(!text.ends_with("hello"));
    }

    #[test]
    fn no_content_response_omits_body_and_content_length() {
        let response = Response::text(StatusCode::from_u16(204), "ignored");
        let encoded = encode_response(Version::Http11, &Method::new("GET"), false, response);
        let text = String::from_utf8(encoded).unwrap();

        assert!(!text.contains("content-length"));
        assert!(!text.ends_with("ignored"));
    }
}
