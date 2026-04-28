use std::fmt;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Version {
    Http10,
    Http11,
}

impl Version {
    pub fn as_str(self) -> &'static str {
        match self {
            Version::Http10 => "HTTP/1.0",
            Version::Http11 => "HTTP/1.1",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Method(String);

impl Method {
    pub fn new(method: impl Into<String>) -> Self {
        Self(method.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    name: String,
    value: String,
}

impl Header {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn value(&self) -> &str {
        &self.value
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StatusCode(u16);

impl StatusCode {
    pub const CONTINUE: Self = Self(100);
    pub const OK: Self = Self(200);
    pub const BAD_REQUEST: Self = Self(400);
    pub const NOT_FOUND: Self = Self(404);
    pub const METHOD_NOT_ALLOWED: Self = Self(405);
    pub const PAYLOAD_TOO_LARGE: Self = Self(413);
    pub const REQUEST_HEADER_FIELDS_TOO_LARGE: Self = Self(431);
    pub const INTERNAL_SERVER_ERROR: Self = Self(500);
    pub const NOT_IMPLEMENTED: Self = Self(501);

    pub fn from_u16(code: u16) -> Self {
        Self(code)
    }

    pub fn as_u16(self) -> u16 {
        self.0
    }

    pub fn reason_phrase(self) -> &'static str {
        match self.0 {
            100 => "Continue",
            200 => "OK",
            204 => "No Content",
            304 => "Not Modified",
            400 => "Bad Request",
            404 => "Not Found",
            405 => "Method Not Allowed",
            413 => "Payload Too Large",
            431 => "Request Header Fields Too Large",
            500 => "Internal Server Error",
            501 => "Not Implemented",
            _ => "Unknown",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    method: Method,
    target: String,
    path: String,
    version: Version,
    headers: Vec<Header>,
    body: Vec<u8>,
}

impl Request {
    pub(crate) fn new(
        method: Method,
        target: String,
        path: String,
        version: Version,
        headers: Vec<Header>,
        body: Vec<u8>,
    ) -> Self {
        Self {
            method,
            target,
            path,
            version,
            headers,
            body,
        }
    }

    pub fn method(&self) -> &Method {
        &self.method
    }

    pub fn target(&self) -> &str {
        &self.target
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn version(&self) -> Version {
        self.version
    }

    pub fn headers(&self) -> &[Header] {
        &self.headers
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|header| header.name.eq_ignore_ascii_case(name))
            .map(Header::value)
    }

    pub fn body(&self) -> &[u8] {
        &self.body
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    status: StatusCode,
    headers: Vec<Header>,
    body: Vec<u8>,
}

impl Response {
    pub fn new(status: StatusCode) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    pub fn ok() -> Self {
        Self::new(StatusCode::OK)
    }

    pub fn text(status: StatusCode, body: impl Into<String>) -> Self {
        let body = body.into().into_bytes();
        Self::new(status)
            .header("content-type", "text/plain; charset=utf-8")
            .with_body(body)
    }

    pub fn bytes(status: StatusCode, body: impl Into<Vec<u8>>) -> Self {
        Self::new(status).with_body(body)
    }

    pub fn status(&self) -> StatusCode {
        self.status
    }

    pub fn headers(&self) -> &[Header] {
        &self.headers
    }

    pub fn body(&self) -> &[u8] {
        &self.body
    }

    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push(Header::new(name, value));
        self
    }

    pub fn with_body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseError {
    BadRequest(&'static str),
    HeaderTooLarge,
    PayloadTooLarge,
    NotImplemented(&'static str),
}

impl ParseError {
    pub fn status_code(&self) -> StatusCode {
        match self {
            ParseError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ParseError::HeaderTooLarge => StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE,
            ParseError::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            ParseError::NotImplemented(_) => StatusCode::NOT_IMPLEMENTED,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub(crate) max_request_line_bytes: usize,
    pub(crate) max_header_bytes: usize,
    pub(crate) max_headers: usize,
    pub(crate) max_body_bytes: usize,
    pub(crate) max_pending_write_bytes: usize,
    pub(crate) read_buffer_capacity: usize,
    pub(crate) poll_timeout: Duration,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            max_request_line_bytes: 8 * 1024,
            max_header_bytes: 64 * 1024,
            max_headers: 128,
            max_body_bytes: 1024 * 1024,
            max_pending_write_bytes: 1024 * 1024,
            read_buffer_capacity: 8 * 1024,
            poll_timeout: Duration::from_millis(50),
        }
    }
}

impl ServerConfig {
    pub fn max_request_line_bytes(mut self, bytes: usize) -> Self {
        self.max_request_line_bytes = bytes;
        self
    }

    pub fn max_header_bytes(mut self, bytes: usize) -> Self {
        self.max_header_bytes = bytes;
        self
    }

    pub fn max_headers(mut self, count: usize) -> Self {
        self.max_headers = count;
        self
    }

    pub fn max_body_bytes(mut self, bytes: usize) -> Self {
        self.max_body_bytes = bytes;
        self
    }

    pub fn max_pending_write_bytes(mut self, bytes: usize) -> Self {
        self.max_pending_write_bytes = bytes;
        self
    }

    pub fn read_buffer_capacity(mut self, bytes: usize) -> Self {
        self.read_buffer_capacity = bytes;
        self
    }

    pub fn poll_timeout(mut self, timeout: Duration) -> Self {
        self.poll_timeout = timeout;
        self
    }
}
