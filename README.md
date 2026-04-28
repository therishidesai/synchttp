# synchttp

`synchttp` is a small HTTP/1 server library in Rust built on `std`, `http`, `httparse` and `mio`.

It currently focuses on a single-threaded HTTP/1.1 server core with strict request parsing, buffered request bodies, and a minimal exact-path router.

NOTE: This was entirely done by opencode + GPT5.4.

## Install

Add the crate with:

```bash
cargo add synchttp
```

Or add it manually to `Cargo.toml`:

```toml
[dependencies]
synchttp = "0.1.0"
```

## Features

- Single-threaded `mio` event loop
- HTTP/1.1 request parsing
- Conservative HTTP/1.0 support
- `Content-Length` request bodies
- `Transfer-Encoding: chunked` request bodies
- Keep-alive and pipelining support
- Exact-path routing with `404` and `405` handling
- Property tests and live integration tests

## Example

```rust
use synchttp::{Response, Router, Server, StatusCode};

fn main() -> std::io::Result<()> {
    fn text_response(status: StatusCode, body: impl Into<String>) -> Response {
        let mut response = Response::new(body.into().into_bytes());
        *response.status_mut() = status;
        response.headers_mut().insert(
            "content-type",
            "text/plain; charset=utf-8".parse().unwrap(),
        );
        response
    }

    let router = Router::new()
        .get("/health", |_req| text_response(StatusCode::OK, "ok"))
        .post("/echo", |req| {
            let mut response = Response::new(req.body().to_vec());
            *response.status_mut() = StatusCode::OK;
            response
        });

    Server::bind("127.0.0.1:8080")?.serve(router)
}
```

## API

- `Server::bind(...)` creates a server bound to an address
- `Server::serve(router)` runs the event loop
- `Router::new()` creates a router
- `Router::get(...)`, `Router::post(...)`, and `Router::route(...)` register handlers
- Handlers use the shape `FnMut(Request<Vec<u8>>) -> Response<Vec<u8>>`

The crate re-exports the core `http` types, so request/response handling uses the standard API:

- `req.method()`
- `req.uri()`
- `req.headers()`
- `req.body()`
- `Response::builder()`
- `Response::new(...)`

## Testing

Run the full test suite with:

```bash
cargo test
```

The test suite includes:

- unit tests for parser and response behavior
- `proptest` coverage for parser and chunked-body invariants
- randomized server-level tests over real `TcpStream` connections
- live integration tests using both raw TCP and `ureq`

## Benchmarking

Run the built-in throughput and latency benchmark with:

```bash
cargo bench --bench perf
```

Useful environment variables:

- `SYNCHTTP_BENCH_WARMUP_SECS`
- `SYNCHTTP_BENCH_DURATION_SECS`
- `SYNCHTTP_BENCH_THREADS`
- `SYNCHTTP_BENCH_LATENCY_THREADS`
- `SYNCHTTP_BENCH_LATENCY_SAMPLES`
- `SYNCHTTP_BENCH_LATENCY_WARMUP`
- `SYNCHTTP_BENCH_ECHO_BYTES`

Example:

```bash
SYNCHTTP_BENCH_THREADS=16 SYNCHTTP_BENCH_DURATION_SECS=5 cargo bench --bench perf
```

## Current Limits

This is still a small v1 server core. It does not currently provide:

- TLS
- HTTP/2
- path parameters
- middleware
- chunked response streaming
- websocket or upgrade handling
- trailer support

## Status

The crate is implemented and tested, but still intentionally minimal. The focus is correctness and a small API surface rather than framework features.
