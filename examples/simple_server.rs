use synchttp::{Response, Router, Server, StatusCode};

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
    let router = Router::new()
        .get("/hello", |_req| text_response(StatusCode::OK, "hello"))
        .post("/echo", |req| {
            println!("json body: {}", String::from_utf8_lossy(req.body()));

            let mut response = Response::new(req.body().to_vec());
            *response.status_mut() = StatusCode::OK;
            response
                .headers_mut()
                .insert("content-type", "application/json".parse().unwrap());
            response
        });

    println!("listening on http://{}", addr);
    Server::bind(addr)?.serve(router)
}
