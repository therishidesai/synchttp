use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use http::header::CONTENT_TYPE;
use http::HeaderValue;
use proptest::prelude::*;
use synchttp::{Response, Router, Server, ServerConfig, StatusCode};

struct TestServer {
    base_url: String,
    address: String,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl TestServer {
    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn address(&self) -> &str {
        &self.address
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn text_response(status: StatusCode, body: impl Into<String>) -> Response {
    let mut response = Response::new(body.into().into_bytes());
    *response.status_mut() = status;
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

fn bytes_response(status: StatusCode, body: impl Into<Vec<u8>>) -> Response {
    let mut response = Response::new(body.into());
    *response.status_mut() = status;
    response
}

fn spawn_server(router: Router) -> TestServer {
    spawn_server_with_config(router, ServerConfig::default())
}

fn spawn_server_with_config(router: Router, config: ServerConfig) -> TestServer {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_thread = Arc::clone(&stop);
    let server = Server::bind("127.0.0.1:0")
        .unwrap()
        .with_config(config.poll_timeout(Duration::from_millis(10)));
    let addr = server.local_addr().unwrap();

    let handle = thread::spawn(move || {
        server
            .serve_until(router, || stop_for_thread.load(Ordering::Relaxed))
            .unwrap();
    });

    TestServer {
        base_url: format!("http://{}", addr),
        address: addr.to_string(),
        stop,
        handle: Some(handle),
    }
}

fn raw_http_exchange(address: &str, request: &[u8]) -> Vec<u8> {
    let mut stream = TcpStream::connect(address).unwrap();
    stream.write_all(request).unwrap();
    stream.shutdown(Shutdown::Write).unwrap();
    read_all(&mut stream)
}

fn raw_http_exchange_in_chunks(address: &str, chunks: &[Vec<u8>]) -> Vec<u8> {
    let mut stream = TcpStream::connect(address).unwrap();
    for chunk in chunks {
        stream.write_all(chunk).unwrap();
        stream.flush().unwrap();
    }
    stream.shutdown(Shutdown::Write).unwrap();
    read_all(&mut stream)
}

fn read_all(stream: &mut TcpStream) -> Vec<u8> {
    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    response
}

fn response_text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn response_body(bytes: &[u8]) -> &[u8] {
    let header_end = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap();
    &bytes[header_end + 4..]
}

fn split_bytes(bytes: &[u8], split_points: &[usize]) -> Vec<Vec<u8>> {
    let mut points = split_points.to_vec();
    points.push(bytes.len());
    points.sort_unstable();
    points.dedup();

    let mut chunks = Vec::new();
    let mut start = 0usize;

    for point in points {
        let end = point.min(bytes.len());
        if end <= start {
            continue;
        }
        chunks.push(bytes[start..end].to_vec());
        start = end;
    }

    if chunks.is_empty() {
        chunks.push(bytes.to_vec());
    }

    chunks
}

fn encode_chunked_body(body: &[u8], split_points: &[usize]) -> Vec<u8> {
    let mut points = split_points.to_vec();
    points.sort_unstable();
    points.dedup();

    let mut encoded = Vec::new();
    let mut start = 0usize;

    for point in points {
        let end = point.min(body.len());
        if end <= start {
            continue;
        }
        let chunk = &body[start..end];
        encoded.extend_from_slice(format!("{:X}\r\n", chunk.len()).as_bytes());
        encoded.extend_from_slice(chunk);
        encoded.extend_from_slice(b"\r\n");
        start = end;
    }

    if start < body.len() {
        let chunk = &body[start..];
        encoded.extend_from_slice(format!("{:X}\r\n", chunk.len()).as_bytes());
        encoded.extend_from_slice(chunk);
        encoded.extend_from_slice(b"\r\n");
    }

    encoded.extend_from_slice(b"0\r\n\r\n");
    encoded
}

fn parse_content_length(response: &[u8]) -> usize {
    let text = std::str::from_utf8(response).unwrap();
    for line in text.split("\r\n") {
        if let Some(value) = line.strip_prefix("content-length: ") {
            return value.parse().unwrap();
        }
    }
    0
}

fn split_http_responses(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut responses = Vec::new();
    let mut offset = 0usize;

    while offset < bytes.len() {
        let header_len = bytes[offset..]
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap();
        let head_end = offset + header_len + 4;
        let content_length = parse_content_length(&bytes[offset..head_end]);
        let end = head_end + content_length;
        responses.push(bytes[offset..end].to_vec());
        offset = end;
    }

    responses
}

#[test]
fn serves_basic_route_with_ureq() {
    let router = Router::new().get("/health", |_req| text_response(StatusCode::OK, "ok"));
    let server = spawn_server(router);

    let response = ureq::get(&format!("{}/health", server.base_url()))
        .call()
        .unwrap();
    let body = response.into_string().unwrap();

    assert_eq!(body, "ok");
}

#[test]
fn buffers_request_body_for_handlers() {
    let router = Router::new().post("/echo", |req| {
        bytes_response(StatusCode::OK, req.body().to_vec())
    });
    let server = spawn_server(router);

    let response = ureq::post(&format!("{}/echo", server.base_url()))
        .send_string("payload")
        .unwrap();
    let body = response.into_string().unwrap();

    assert_eq!(body, "payload");
}

#[test]
fn returns_bad_request_for_malformed_http() {
    let router = Router::new().get("/health", |_req| text_response(StatusCode::OK, "ok"));
    let server = spawn_server(router);
    let response = raw_http_exchange(server.address(), b"GET / HTTP/1.1\nHost: example.test\n\n");
    let response = response_text(&response);

    assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
}

#[test]
fn returns_404_for_unknown_route() {
    let router = Router::new().get("/health", |_req| text_response(StatusCode::OK, "ok"));
    let server = spawn_server(router);
    let response = raw_http_exchange(
        server.address(),
        b"GET /missing HTTP/1.1\r\nHost: example.test\r\n\r\n",
    );
    let response = response_text(&response);

    assert!(response.starts_with("HTTP/1.1 404 Not Found\r\n"));
    assert!(response.ends_with("not found"));
}

#[test]
fn returns_405_and_allow_header_for_wrong_method() {
    let router = Router::new().post("/echo", |req| {
        bytes_response(StatusCode::OK, req.body().to_vec())
    });
    let server = spawn_server(router);
    let response = raw_http_exchange(
        server.address(),
        b"GET /echo HTTP/1.1\r\nHost: example.test\r\n\r\n",
    );
    let response = response_text(&response);

    assert!(response.starts_with("HTTP/1.1 405 Method Not Allowed\r\n"));
    assert!(response.contains("allow: POST\r\n"));
}

#[test]
fn rejects_missing_host_for_http11() {
    let router = Router::new().get("/health", |_req| text_response(StatusCode::OK, "ok"));
    let server = spawn_server(router);
    let response = raw_http_exchange(server.address(), b"GET / HTTP/1.1\r\n\r\n");
    let response = response_text(&response);

    assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
}

#[test]
fn rejects_obs_fold_header_lines() {
    let router = Router::new().get("/health", |_req| text_response(StatusCode::OK, "ok"));
    let server = spawn_server(router);
    let response = raw_http_exchange(
        server.address(),
        b"GET /health HTTP/1.1\r\nHost: example.test\r\nX-Test: one\r\n folded\r\n\r\n",
    );
    let response = response_text(&response);

    assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
}

#[test]
fn accepts_absolute_form_targets() {
    let router = Router::new().get("/health", |_req| text_response(StatusCode::OK, "ok"));
    let server = spawn_server(router);
    let response = raw_http_exchange(
        server.address(),
        b"GET http://example.test/health?debug=true HTTP/1.1\r\nHost: example.test\r\n\r\n",
    );
    let response = response_text(&response);

    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(response.ends_with("ok"));
}

#[test]
fn accepts_matching_duplicate_content_length_headers() {
    let router = Router::new().post("/echo", |req| {
        bytes_response(StatusCode::OK, req.body().to_vec())
    });
    let server = spawn_server(router);
    let response = raw_http_exchange(
        server.address(),
        b"POST /echo HTTP/1.1\r\nHost: example.test\r\nContent-Length: 7\r\nContent-Length: 7\r\n\r\npayload",
    );
    let response = response_text(&response);

    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(response.ends_with("payload"));
}

#[test]
fn rejects_conflicting_content_length_headers() {
    let router = Router::new().post("/echo", |req| {
        bytes_response(StatusCode::OK, req.body().to_vec())
    });
    let server = spawn_server(router);
    let response = raw_http_exchange(
        server.address(),
        b"POST /echo HTTP/1.1\r\nHost: example.test\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\nhi",
    );
    let response = response_text(&response);

    assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
}

#[test]
fn rejects_transfer_encoding_and_content_length_together() {
    let router = Router::new().post("/echo", |req| {
        bytes_response(StatusCode::OK, req.body().to_vec())
    });
    let server = spawn_server(router);
    let response = raw_http_exchange(
        server.address(),
        b"POST /echo HTTP/1.1\r\nHost: example.test\r\nTransfer-Encoding: chunked\r\nContent-Length: 0\r\n\r\n0\r\n\r\n",
    );
    let response = response_text(&response);

    assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
}

#[test]
fn accepts_chunked_request_body() {
    let router = Router::new().post("/echo", |req| {
        bytes_response(StatusCode::OK, req.body().to_vec())
    });
    let server = spawn_server(router);
    let response = raw_http_exchange(
        server.address(),
        b"POST /echo HTTP/1.1\r\nHost: example.test\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n",
    );
    let response = response_text(&response);

    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(response.ends_with("hello world"));
}

#[test]
fn rejects_unsupported_transfer_encoding() {
    let router = Router::new().post("/echo", |req| {
        bytes_response(StatusCode::OK, req.body().to_vec())
    });
    let server = spawn_server(router);
    let response = raw_http_exchange(
        server.address(),
        b"POST /echo HTTP/1.1\r\nHost: example.test\r\nTransfer-Encoding: gzip\r\n\r\npayload",
    );
    let response = response_text(&response);

    assert!(response.starts_with("HTTP/1.1 501 Not Implemented\r\n"));
}

#[test]
fn http_10_closes_by_default_and_stops_after_first_request() {
    let router = Router::new().get("/health", |_req| text_response(StatusCode::OK, "ok"));
    let server = spawn_server(router);
    let response = raw_http_exchange(
        server.address(),
        b"GET /health HTTP/1.0\r\n\r\nGET /health HTTP/1.0\r\n\r\n",
    );
    let response = response_text(&response);

    assert_eq!(response.matches("HTTP/1.0 200 OK").count(), 1);
    assert!(!response.contains("connection: keep-alive\r\n"));
}

#[test]
fn http_10_keep_alive_allows_multiple_requests() {
    let router = Router::new().get("/health", |_req| text_response(StatusCode::OK, "ok"));
    let server = spawn_server(router);
    let response = raw_http_exchange(
        server.address(),
        b"GET /health HTTP/1.0\r\nConnection: keep-alive\r\n\r\nGET /health HTTP/1.0\r\nConnection: keep-alive\r\n\r\n",
    );
    let response = response_text(&response);

    assert_eq!(response.matches("HTTP/1.0 200 OK").count(), 2);
    assert_eq!(response.matches("connection: keep-alive\r\n").count(), 2);
}

#[test]
fn connection_close_stops_after_current_response() {
    let router = Router::new().get("/health", |_req| text_response(StatusCode::OK, "ok"));
    let server = spawn_server(router);
    let response = raw_http_exchange(
        server.address(),
        b"GET /health HTTP/1.1\r\nHost: example.test\r\nConnection: close\r\n\r\nGET /health HTTP/1.1\r\nHost: example.test\r\n\r\n",
    );
    let response = response_text(&response);

    assert_eq!(response.matches("HTTP/1.1 200 OK").count(), 1);
    assert!(response.contains("connection: close\r\n"));
}

#[test]
fn head_request_suppresses_body_bytes() {
    let router = Router::new().route("HEAD", "/health", |_req| {
        text_response(StatusCode::OK, "hello")
    });
    let server = spawn_server(router);
    let response = raw_http_exchange(
        server.address(),
        b"HEAD /health HTTP/1.1\r\nHost: example.test\r\n\r\n",
    );
    let response_text = response_text(&response);

    assert!(response_text.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(response_text.contains("content-length: 5\r\n"));
    assert!(response_body(&response).is_empty());
}

#[test]
fn parses_requests_sent_in_small_chunks() {
    let router = Router::new().post("/echo", |req| {
        bytes_response(StatusCode::OK, req.body().to_vec())
    });
    let server = spawn_server(router);
    let request = b"POST /echo HTTP/1.1\r\nHost: example.test\r\nContent-Length: 7\r\n\r\npayload";
    let chunks = request.iter().map(|byte| vec![*byte]).collect::<Vec<_>>();
    let response = raw_http_exchange_in_chunks(server.address(), &chunks);
    let response = response_text(&response);

    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(response.ends_with("payload"));
}

#[test]
fn preserves_response_order_for_pipelined_requests() {
    let router = Router::new()
        .get("/first", |_req| text_response(StatusCode::OK, "first"))
        .get("/second", |_req| text_response(StatusCode::OK, "second"));
    let server = spawn_server(router);
    let response = raw_http_exchange(
        server.address(),
        b"GET /first HTTP/1.1\r\nHost: example.test\r\n\r\nGET /second HTTP/1.1\r\nHost: example.test\r\n\r\n",
    );
    let response = response_text(&response);

    assert_eq!(response.matches("HTTP/1.1 200 OK").count(), 2);
    assert!(response.find("first").unwrap() < response.find("second").unwrap());
}

#[test]
fn returns_413_when_content_length_exceeds_limit() {
    let router = Router::new().post("/echo", |req| {
        bytes_response(StatusCode::OK, req.body().to_vec())
    });
    let server = spawn_server_with_config(router, ServerConfig::default().max_body_bytes(4));
    let response = raw_http_exchange(
        server.address(),
        b"POST /echo HTTP/1.1\r\nHost: example.test\r\nContent-Length: 7\r\n\r\npayload",
    );
    let response = response_text(&response);

    assert!(response.starts_with("HTTP/1.1 413 Payload Too Large\r\n"));
}

#[test]
fn returns_413_when_chunked_body_exceeds_limit() {
    let router = Router::new().post("/echo", |req| {
        bytes_response(StatusCode::OK, req.body().to_vec())
    });
    let server = spawn_server_with_config(router, ServerConfig::default().max_body_bytes(4));
    let response = raw_http_exchange(
        server.address(),
        b"POST /echo HTTP/1.1\r\nHost: example.test\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n",
    );
    let response = response_text(&response);

    assert!(response.starts_with("HTTP/1.1 413 Payload Too Large\r\n"));
}

#[test]
fn returns_431_when_header_count_exceeds_limit() {
    let router = Router::new().get("/health", |_req| text_response(StatusCode::OK, "ok"));
    let server = spawn_server_with_config(router, ServerConfig::default().max_headers(1));
    let response = raw_http_exchange(
        server.address(),
        b"GET /health HTTP/1.1\r\nHost: example.test\r\nX-Test: value\r\n\r\n",
    );
    let response = response_text(&response);

    assert!(response.starts_with("HTTP/1.1 431 Request Header Fields Too Large\r\n"));
}

#[test]
fn returns_431_when_header_block_exceeds_limit() {
    let router = Router::new().get("/health", |_req| text_response(StatusCode::OK, "ok"));
    let server = spawn_server_with_config(router, ServerConfig::default().max_header_bytes(32));
    let response = raw_http_exchange(
        server.address(),
        b"GET /health HTTP/1.1\r\nHost: example.test\r\nX-Test: a-very-long-value\r\n\r\n",
    );
    let response = response_text(&response);

    assert!(response.starts_with("HTTP/1.1 431 Request Header Fields Too Large\r\n"));
}

#[test]
fn keeps_connections_alive_for_pipelined_requests() {
    let router = Router::new().get("/health", |_req| text_response(StatusCode::OK, "ok"));
    let server = spawn_server(router);
    let response = raw_http_exchange(
        server.address(),
        b"GET /health HTTP/1.1\r\nHost: example.test\r\n\r\nGET /health HTTP/1.1\r\nHost: example.test\r\n\r\n",
    );
    let response = response_text(&response);

    assert_eq!(response.matches("HTTP/1.1 200 OK").count(), 2);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    #[test]
    fn content_length_requests_round_trip_over_random_delivery(
        body in proptest::collection::vec(any::<u8>(), 0..96),
        request_splits in proptest::collection::vec(0usize..256usize, 0..12),
    ) {
        let router = Router::new().post("/echo", |req| bytes_response(StatusCode::OK, req.body().to_vec()));
        let server = spawn_server(router);

        let mut request = format!(
            "POST /echo HTTP/1.1\r\nHost: example.test\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .into_bytes();
        request.extend_from_slice(&body);

        let chunks = split_bytes(&request, &request_splits);
        let response = raw_http_exchange_in_chunks(server.address(), &chunks);

        prop_assert!(response.starts_with(b"HTTP/1.1 200 OK\r\n"));
        prop_assert_eq!(response_body(&response), body.as_slice());
    }

    #[test]
    fn chunked_requests_round_trip_over_random_delivery(
        body in proptest::collection::vec(any::<u8>(), 0..96),
        chunk_splits in proptest::collection::vec(0usize..96usize, 0..8),
        request_splits in proptest::collection::vec(0usize..320usize, 0..14),
    ) {
        let router = Router::new().post("/echo", |req| bytes_response(StatusCode::OK, req.body().to_vec()));
        let server = spawn_server(router);

        let mut request = b"POST /echo HTTP/1.1\r\nHost: example.test\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
        request.extend_from_slice(&encode_chunked_body(&body, &chunk_splits));

        let chunks = split_bytes(&request, &request_splits);
        let response = raw_http_exchange_in_chunks(server.address(), &chunks);

        prop_assert!(response.starts_with(b"HTTP/1.1 200 OK\r\n"));
        prop_assert_eq!(response_body(&response), body.as_slice());
    }

    #[test]
    fn pipelined_requests_preserve_response_order(
        route_ids in proptest::collection::vec(0usize..4usize, 1..8),
        request_splits in proptest::collection::vec(0usize..512usize, 0..16),
    ) {
        let router = Router::new()
            .get("/r0", |_req| text_response(StatusCode::OK, "route-0"))
            .get("/r1", |_req| text_response(StatusCode::OK, "route-1"))
            .get("/r2", |_req| text_response(StatusCode::OK, "route-2"))
            .get("/r3", |_req| text_response(StatusCode::OK, "route-3"));
        let server = spawn_server(router);

        let mut request = Vec::new();
        let mut expected_bodies = Vec::new();

        for route_id in &route_ids {
            request.extend_from_slice(
                format!(
                    "GET /r{} HTTP/1.1\r\nHost: example.test\r\n\r\n",
                    route_id
                )
                .as_bytes(),
            );
            expected_bodies.push(format!("route-{}", route_id).into_bytes());
        }

        let chunks = split_bytes(&request, &request_splits);
        let response = raw_http_exchange_in_chunks(server.address(), &chunks);
        let responses = split_http_responses(&response);

        prop_assert_eq!(responses.len(), expected_bodies.len());

        for (response, expected_body) in responses.iter().zip(expected_bodies.iter()) {
            let text = response_text(response);
            prop_assert!(text.starts_with("HTTP/1.1 200 OK\r\n"));
            prop_assert_eq!(response_body(response), expected_body.as_slice());
        }
    }
}
