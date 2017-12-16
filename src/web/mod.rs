use std::net::SocketAddr;
use futures::{future, Future, Stream};
use tokio_core::reactor::Handle;
use hyper::{self, Method, StatusCode};
use hyper::header::ContentType;
use hyper::server::{Http, Request, Response, Service};
use serde_json;
use monitor::{Monitor, ServerList};


struct StatusPages {
    monitor: Monitor,
}

#[derive(Debug, Serialize)]
struct Status {
    servers: ServerList,
    tx_speed: usize,
    rx_speed: usize,
}

impl StatusPages {
    fn new(monitor: Monitor) -> StatusPages {
        StatusPages { monitor }
    }

    fn status_json(&self) -> serde_json::Result<String> {
        let servers = self.monitor.servers();
        let (tx_speed, rx_speed) = self.monitor.throughput();
        let status = Status {
            servers, tx_speed, rx_speed,
        };
        serde_json::to_string(&status)
    }
}

impl Service for StatusPages {
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
            (&Method::Get, "/status") => match self.status_json() {
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

pub fn run_server(bind: &SocketAddr, monitor: Monitor, handle: &Handle)
        -> Box<Future<Item=(), Error=()>> {
    handle.spawn(monitor.monitor_throughput());
    let new_service = move || Ok(StatusPages::new(monitor.clone()));
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

