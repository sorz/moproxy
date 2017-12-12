use std::net::SocketAddr;
use std::sync::Arc;
use ::futures::{future, Future, Stream};
use ::tokio_core::reactor::Handle;
use ::hyper::{self, Method, StatusCode};
use ::hyper::header::ContentType;
use ::hyper::server::{Http, Request, Response, Service};
use ::serde_json;
use ::monitor::ServerList;


struct StatsPages {
    servers: Arc<ServerList>,
}

impl StatsPages {
    fn new(servers: Arc<ServerList>) -> StatsPages {
        StatsPages { servers }
    }

    fn servers_json(&self) -> serde_json::Result<String> {
        let infos = self.servers.get_infos();
        serde_json::to_string(&*infos)
    }
}

impl Service for StatsPages {
    type Request = Request;
    type Response = Response;
    type Error = hyper::Error;
    type Future = Box<Future<Item=Self::Response, Error=Self::Error>>;

    fn call(&self, req: Request) -> Self::Future {
        let resp = Response::new();
        let resp = match (req.method(), req.path()) {
            (&Method::Get, "/") => {
                resp.with_body(include_str!("index.html"))
                    .with_header(ContentType::html())
            },
            (&Method::Get, "/version") => {
                resp.with_body(env!("CARGO_PKG_VERSION"))
                    .with_header(ContentType::plaintext())
            },
            (&Method::Get, "/servers") => match self.servers_json() {
                Ok(json) => resp.with_body(json)
                                .with_header(ContentType::json()),
                Err(e) => {
                    error!("fail to serialize servers to json: {}", e);
                    resp.with_status(StatusCode::InternalServerError)
                        .with_header(ContentType::plaintext())
                        .with_body(format!("internal error: {}", e))
                },
            },
            _ => resp.with_status(StatusCode::NotFound)
                     .with_body("page not found")
                     .with_header(ContentType::plaintext()),
        };
        debug!("{} {} [{}]", req.method(), req.path(), resp.status());
        return Box::new(future::ok(resp));
    }
}

pub fn run_server(bind: &SocketAddr, servers: Arc<ServerList>,
                  handle: &Handle)
        -> Box<Future<Item=(), Error=()>> {
    let new_service = move || Ok(StatsPages::new(servers.clone()));
    let serve = Http::new()
        .serve_addr_handle(bind, handle, new_service)
        .expect("fail to start web server");
    let handle_ = handle.clone();
    let run = serve.for_each(move |conn| {
        handle_.spawn(
            conn.map(|_| ())
                .map_err(|err| info!("http: {}", err))
        );
        Ok(())
    }).map_err(|err| error!("error on http server: {}", err));
    Box::new(run)
}

