use crate::response::text_response;
use crate::{Method, Request, Response, StatusCode};
use http::header::ALLOW;
use http::HeaderValue;

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
    method: Method,
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
            method: method.parse().expect("invalid HTTP method"),
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
        let method = request.method().clone();
        let path = request.uri().path().to_string();
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
            allowed.sort_by(|left, right| left.as_str().cmp(right.as_str()));
            let mut response = text_response(StatusCode::METHOD_NOT_ALLOWED, "method not allowed");
            response.headers_mut().insert(
                ALLOW,
                HeaderValue::from_str(
                    &allowed
                        .iter()
                        .map(Method::as_str)
                        .collect::<Vec<_>>()
                        .join(", "),
                )
                .unwrap(),
            );
            return response;
        }

        text_response(StatusCode::NOT_FOUND, "not found")
    }
}

#[cfg(test)]
mod tests {
    use super::{Handler, Router};
    use crate::response::text_response;
    use crate::{Request, StatusCode, Version};
    use http::header::{ALLOW, CONTENT_TYPE};

    fn request(method: &str, path: &str) -> Request {
        let mut request = Request::new(Vec::new());
        *request.method_mut() = method.parse().unwrap();
        *request.uri_mut() = path.parse().unwrap();
        *request.version_mut() = Version::HTTP_11;
        request
    }

    #[test]
    fn routes_exact_matches() {
        let mut router = Router::new().get("/health", |_req| text_response(StatusCode::OK, "ok"));
        let response = router.handle(request("GET", "/health"));

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[test]
    fn returns_405_with_allow_header() {
        let mut router = Router::new().post("/health", |_req| text_response(StatusCode::OK, "ok"));
        let response = router.handle(request("GET", "/health"));

        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(
            response.headers().get(CONTENT_TYPE).unwrap(),
            "text/plain; charset=utf-8"
        );
        assert_eq!(response.headers().get(ALLOW).unwrap(), "POST");
    }
}
