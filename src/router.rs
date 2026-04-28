use crate::types::{Request, Response, StatusCode};

pub trait Handler: Send {
    fn handle(&mut self, request: Request) -> Response;
}

impl<F> Handler for F
where
    F: FnMut(Request) -> Response + Send,
{
    fn handle(&mut self, request: Request) -> Response {
        self(request)
    }
}

struct Route {
    method: String,
    path: String,
    handler: Box<dyn Handler>,
}

pub struct Router {
    routes: Vec<Route>,
}

impl Router {
    pub fn new() -> Self {
        Self { routes: Vec::new() }
    }

    pub fn route<H>(mut self, method: &str, path: &str, handler: H) -> Self
    where
        H: FnMut(Request) -> Response + Send + 'static,
    {
        self.routes.push(Route {
            method: method.to_string(),
            path: path.to_string(),
            handler: Box::new(handler),
        });
        self
    }

    pub fn get<H>(self, path: &str, handler: H) -> Self
    where
        H: FnMut(Request) -> Response + Send + 'static,
    {
        self.route("GET", path, handler)
    }

    pub fn post<H>(self, path: &str, handler: H) -> Self
    where
        H: FnMut(Request) -> Response + Send + 'static,
    {
        self.route("POST", path, handler)
    }
}

impl Default for Router {
    fn default() -> Self {
        Self::new()
    }
}

impl Handler for Router {
    fn handle(&mut self, request: Request) -> Response {
        let method = request.method().as_str();
        let path = request.path();
        let mut allowed = Vec::new();

        for route in &mut self.routes {
            if route.path == path {
                if route.method == method {
                    return route.handler.handle(request);
                }
                if !allowed.iter().any(|value| value == &route.method) {
                    allowed.push(route.method.clone());
                }
            }
        }

        if !allowed.is_empty() {
            allowed.sort();
            return Response::text(StatusCode::METHOD_NOT_ALLOWED, "method not allowed")
                .header("allow", allowed.join(", "));
        }

        Response::text(StatusCode::NOT_FOUND, "not found")
    }
}

#[cfg(test)]
mod tests {
    use super::{Handler, Router};
    use crate::types::{Method, Request, Response, StatusCode, Version};

    fn request(method: &str, path: &str) -> Request {
        Request::new(
            Method::new(method),
            path.to_string(),
            path.to_string(),
            Version::Http11,
            Vec::new(),
            Vec::new(),
        )
    }

    #[test]
    fn routes_exact_matches() {
        let mut router = Router::new().get("/health", |_req| Response::text(StatusCode::OK, "ok"));
        let response = router.handle(request("GET", "/health"));

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[test]
    fn returns_405_with_allow_header() {
        let mut router = Router::new().post("/health", |_req| Response::text(StatusCode::OK, "ok"));
        let response = router.handle(request("GET", "/health"));

        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(response.headers()[0].name(), "content-type");
        assert_eq!(response.headers()[1].name(), "allow");
    }
}
