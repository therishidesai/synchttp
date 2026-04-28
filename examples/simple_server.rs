use synchttp::{Response, Router, Server, StatusCode};

fn main() -> std::io::Result<()> {
    let addr = "127.0.0.1:8080";
    let router = Router::new()
        .get("/hello", |_req| Response::text(StatusCode::OK, "hello"))
        .post("/echo", |req| {
            println!("json body: {}", String::from_utf8_lossy(req.body()));

            Response::bytes(StatusCode::OK, req.body().to_vec())
                .header("content-type", "application/json")
        });

    println!("listening on http://{}", addr);
    Server::bind(addr)?.serve(router)
}
