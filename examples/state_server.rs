use std::sync::{Arc, Mutex};

use synchttp::{Response, Router, Server, StatusCode};

#[derive(Default)]
struct AppState {
    hello_count: usize,
    echo_count: usize,
    last_echo_body: String,
}

fn text_response(status: StatusCode, body: impl Into<String>) -> Response {
    let mut response = Response::new(body.into().into_bytes());
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert("content-type", "text/plain; charset=utf-8".parse().unwrap());
    response
}

fn main() -> std::io::Result<()> {
    let addr = "127.0.0.1:8080";
    let state = Arc::new(Mutex::new(AppState::default()));

    let hello_state = Arc::clone(&state);
    let echo_state = Arc::clone(&state);
    let stats_state = Arc::clone(&state);

    let router = Router::new()
        .get("/hello", move |_req| {
            let mut state = hello_state.lock().unwrap();
            state.hello_count += 1;

            text_response(StatusCode::OK, format!("hello {}", state.hello_count))
        })
        .post("/echo", move |req| {
            let body = String::from_utf8_lossy(req.body()).into_owned();
            let mut state = echo_state.lock().unwrap();
            state.echo_count += 1;
            state.last_echo_body = body.clone();

            println!("echo request {} body: {}", state.echo_count, body);

            let mut response = Response::new(req.body().to_vec());
            *response.status_mut() = StatusCode::OK;
            response
                .headers_mut()
                .insert("content-type", "application/json".parse().unwrap());
            response
        })
        .get("/stats", move |_req| {
            let state = stats_state.lock().unwrap();
            let body = format!(
                "{{\"hello_count\":{},\"echo_count\":{},\"last_echo_body\":{:?}}}",
                state.hello_count, state.echo_count, state.last_echo_body,
            );

            let mut response = Response::new(body.into_bytes());
            *response.status_mut() = StatusCode::OK;
            response
                .headers_mut()
                .insert("content-type", "application/json".parse().unwrap());
            response
        });

    println!("listening on http://{}", addr);
    Server::bind(addr)?.serve(router)
}
