use crate::{Method, Response, StatusCode, Version};
use http::header::{
    HeaderName, HeaderValue, CONNECTION, CONTENT_LENGTH, CONTENT_TYPE, TRANSFER_ENCODING,
};

pub(crate) fn encode_response(
    version: Version,
    method: &Method,
    close_connection: bool,
    response: Response,
) -> Vec<u8> {
    let (parts, body) = response.into_parts();
    let mut out = Vec::new();
    out.extend_from_slice(version_as_bytes(version));
    out.push(b' ');
    out.extend_from_slice(parts.status.as_str().as_bytes());
    out.push(b' ');
    out.extend_from_slice(
        parts
            .status
            .canonical_reason()
            .unwrap_or("Unknown")
            .as_bytes(),
    );
    out.extend_from_slice(b"\r\n");

    let status = parts.status;
    let status_forbids_body = is_body_forbidden(status);
    let head_only = *method == Method::HEAD;
    let should_send_body = !status_forbids_body && !head_only;

    for (name, value) in &parts.headers {
        if is_reserved_header(name) {
            continue;
        }
        write_header(&mut out, name, value);
    }

    if close_connection {
        write_header(&mut out, &CONNECTION, &HeaderValue::from_static("close"));
    } else if version == Version::HTTP_10 {
        write_header(
            &mut out,
            &CONNECTION,
            &HeaderValue::from_static("keep-alive"),
        );
    }

    if !status_forbids_body {
        let content_length = HeaderValue::from_str(&body.len().to_string()).unwrap();
        write_header(&mut out, &CONTENT_LENGTH, &content_length);
    }

    out.extend_from_slice(b"\r\n");

    if should_send_body {
        out.extend_from_slice(&body);
    }

    out
}

pub(crate) fn text_response(status: StatusCode, body: impl Into<String>) -> Response {
    let body = body.into().into_bytes();
    let mut response = Response::new(body);
    *response.status_mut() = status;
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

fn is_reserved_header(name: &HeaderName) -> bool {
    name == CONTENT_LENGTH || name == TRANSFER_ENCODING || name == CONNECTION
}

fn is_body_forbidden(status: StatusCode) -> bool {
    status.is_informational()
        || status == StatusCode::NO_CONTENT
        || status == StatusCode::NOT_MODIFIED
}

fn write_header(out: &mut Vec<u8>, name: &HeaderName, value: &HeaderValue) {
    out.extend_from_slice(name.as_str().as_bytes());
    out.extend_from_slice(b": ");
    out.extend_from_slice(value.as_bytes());
    out.extend_from_slice(b"\r\n");
}

fn version_as_bytes(version: Version) -> &'static [u8] {
    match version {
        Version::HTTP_10 => b"HTTP/1.0",
        Version::HTTP_11 => b"HTTP/1.1",
        _ => b"HTTP/1.1",
    }
}

#[cfg(test)]
mod tests {
    use super::{encode_response, text_response};
    use crate::{Method, StatusCode, Version};

    #[test]
    fn head_response_suppresses_body_bytes() {
        let response = text_response(StatusCode::OK, "hello");
        let encoded = encode_response(Version::HTTP_11, &Method::HEAD, false, response);
        let text = String::from_utf8(encoded).unwrap();

        assert!(text.contains("content-length: 5\r\n"));
        assert!(!text.ends_with("hello"));
    }

    #[test]
    fn no_content_response_omits_body_and_content_length() {
        let response = text_response(StatusCode::NO_CONTENT, "ignored");
        let encoded = encode_response(Version::HTTP_11, &Method::GET, false, response);
        let text = String::from_utf8(encoded).unwrap();

        assert!(!text.contains("content-length"));
        assert!(!text.ends_with("ignored"));
    }
}
