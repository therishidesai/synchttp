use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use synchttp::{Response, Router, Server, ServerConfig, StatusCode};

fn spawn_server(router: Router) -> (String, Arc<AtomicBool>, thread::JoinHandle<()>) {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_thread = Arc::clone(&stop);
    let server = Server::bind("127.0.0.1:0")
        .unwrap()
        .with_config(ServerConfig::default().poll_timeout(std::time::Duration::from_millis(10)));
    let addr = server.local_addr().unwrap();

    let handle = thread::spawn(move || {
        server
            .serve_until(router, || stop_for_thread.load(Ordering::Relaxed))
            .unwrap();
    });

    (format!("http://{}", addr), stop, handle)
}

#[test]
fn serves_basic_route_with_ureq() {
    let router = Router::new().get("/health", |_req| Response::text(StatusCode::OK, "ok"));
    let (base_url, stop, handle) = spawn_server(router);

    let response = ureq::get(&format!("{}/health", base_url)).call().unwrap();
    let body = response.into_string().unwrap();

    stop.store(true, Ordering::Relaxed);
    handle.join().unwrap();

    assert_eq!(body, "ok");
}

#[test]
fn buffers_request_body_for_handlers() {
    let router = Router::new().post("/echo", |req| {
        Response::bytes(StatusCode::OK, req.body().to_vec())
    });
    let (base_url, stop, handle) = spawn_server(router);

    let response = ureq::post(&format!("{}/echo", base_url))
        .send_string("payload")
        .unwrap();
    let body = response.into_string().unwrap();

    stop.store(true, Ordering::Relaxed);
    handle.join().unwrap();

    assert_eq!(body, "payload");
}

#[test]
fn returns_bad_request_for_malformed_http() {
    let router = Router::new().get("/health", |_req| Response::text(StatusCode::OK, "ok"));
    let (base_url, stop, handle) = spawn_server(router);

    let address = base_url.trim_start_matches("http://");
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .write_all(b"GET / HTTP/1.1\nHost: example.test\n\n")
        .unwrap();
    stream.shutdown(std::net::Shutdown::Write).unwrap();

    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();

    stop.store(true, Ordering::Relaxed);
    handle.join().unwrap();

    assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
}

#[test]
fn keeps_connections_alive_for_pipelined_requests() {
    let router = Router::new().get("/health", |_req| Response::text(StatusCode::OK, "ok"));
    let (base_url, stop, handle) = spawn_server(router);

    let address = base_url.trim_start_matches("http://");
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .write_all(
            b"GET /health HTTP/1.1\r\nHost: example.test\r\n\r\nGET /health HTTP/1.1\r\nHost: example.test\r\n\r\n",
        )
        .unwrap();
    stream.shutdown(std::net::Shutdown::Write).unwrap();

    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();

    stop.store(true, Ordering::Relaxed);
    handle.join().unwrap();

    assert_eq!(response.matches("HTTP/1.1 200 OK").count(), 2);
}
