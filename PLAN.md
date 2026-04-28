# HTTP/1 Server Plan

## Goal

Build a Rust HTTP/1 server library using only `std` and `mio` at runtime, following RFC 9110 and RFC 9112. Add integration and property-based randomized tests using dev-dependencies only.

## Runtime Scope

- Single-threaded event-loop server using `mio::Poll`
- HTTP/1.1 server core
- Conservative HTTP/1.0 compatibility
- Request parsing
- Response writing
- Keep-alive support
- Fixed-length and chunked request bodies
- No TLS, HTTP/2, websocket, large framework features, or thread pool

## Dependencies

- Runtime:
  - `mio`
- Dev-dependencies:
  - `ureq`
  - `proptest`

## Crate Layout

- `src/lib.rs`
- `src/types.rs`
- `src/parse.rs`
- `src/body.rs`
- `src/response.rs`
- `src/router.rs`
- `src/conn.rs`
- `src/server.rs`

## Core Types

- `Request`
- `Response`
- `Header`
- `Version`
- `Method`
- `StatusCode`
- `ServerConfig`
- `ParseError`
- `BodyKind`
- `Router`
- `Handler`

## Parser Requirements

- Incremental request-line parsing
- Incremental header parsing
- Strict CRLF handling
- Reject obsolete line folding
- Reject malformed header whitespace
- Header names compared ASCII-case-insensitively
- Enforce HTTP/1.1 `Host` requirements

## Framing Rules

- Support `Content-Length`
- Support `Transfer-Encoding: chunked`
- Reject conflicting `Content-Length`
- Reject `Transfer-Encoding` plus `Content-Length`
- Reject unsupported transfer codings
- Bound all numeric parsing and buffer growth

## Response Rules

- Encode status line and headers
- Auto-manage framing headers when appropriate
- No body for `1xx`, `204`, `304`
- Suppress response body for `HEAD`
- Respect `Connection: close`

## Connection Model

- Single-threaded `mio::Poll` loop
- One listener token and one token per connection
- Per connection:
  - read buffer
  - parse state
  - write queue
  - keep-alive/close flags
- Read until `WouldBlock`
- Parse incrementally
- Write until `WouldBlock`
- Enable writable interest only when output is pending
- Preserve response ordering per connection

## State Machine

- `ReadingHead`
- `ReadingBody`
- `RequestReady`
- `WritingResponse`
- `Closing`

## Initial Public API

```rust
Server::bind(addr)?.run(|req| -> Response {
    Response::ok().with_body("hello")
})
```

Handler model for v1:

- `FnMut(Request) -> Response`

Recommended high-level endpoint API:

```rust
let router = Router::new()
    .get("/health", |_req| Response::text(StatusCode::OK, "ok"))
    .post("/echo", |req| Response::bytes(StatusCode::OK, req.body().to_vec()));

Server::bind("127.0.0.1:8080")?
    .serve(router)
```

Router behavior for v1:

- Match on method + exact path
- Return `404 Not Found` when no path matches
- Return `405 Method Not Allowed` when the path matches but the method does not
- Set `Allow` automatically on `405`
- No regex, globs, or macros
- No path parameters in the first version

Low-level dispatch remains available through `Server::run(...)`.

Request handling model for v1:

- The server buffers the full request body before invoking the handler
- `Request` exposes ergonomic accessors such as `method()`, `path()`, `header()`, and `body()`
- `Response` provides helpers like `new`, `text`, `bytes`, and header/body builder methods

Suggested configuration API:

```rust
let config = ServerConfig::default()
    .max_request_line_bytes(8 * 1024)
    .max_header_bytes(64 * 1024)
    .max_body_bytes(1024 * 1024);

Server::bind("127.0.0.1:8080")?
    .with_config(config)
    .serve(router)
```

## Safety and Compliance

- Reject ambiguous framing to avoid request smuggling
- Reject duplicate `Content-Length` with differing values
- Require exact or conservative parsing behavior
- Hard limits for:
  - request line bytes
  - total header bytes
  - header count
  - body bytes
  - chunk metadata bytes
- Close or error cleanly on malformed requests

## Testing Strategy

### Unit Tests

- Request-line parsing
- Header parsing
- Framing resolution
- Chunked body parsing
- Response encoding

### Integration Tests

- Start real localhost server
- Use `ureq` as a valid client
- Use raw `TcpStream` for malformed and partial-input cases
- Test keep-alive
- Test fixed-length bodies
- Test chunked request bodies
- Test malformed requests
- Test HTTP/1.0 behavior

### Property Tests

- Parser never panics on generated inputs
- Same request parsed identically across arbitrary chunk boundaries
- Framing invariants hold
- Chunked parser handles partial delivery safely
- Random malformed byte streams never produce panics or invalid state transitions
- Generated header sets and body framing combinations exercise parser edge cases

## Implementation Order

1. Create crate and `Cargo.toml`
2. Define core HTTP types and limits
3. Implement request-line parser
4. Implement header parser
5. Implement framing resolver
6. Implement fixed-length body parsing
7. Implement chunked body parsing
8. Implement response encoder
9. Implement connection state machine
10. Implement `mio` event loop
11. Implement `Router` and endpoint dispatch
12. Add integration tests
13. Add property tests
14. Harden based on failures and edge cases

## Non-Goals for v1

- TLS
- HTTP/2
- WebSocket upgrades
- CONNECT tunneling
- Compression
- Complex routing features like parameters, regex matching, or middleware stacks
- Thread pool
- File serving helpers
