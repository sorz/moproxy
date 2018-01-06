use std::io;
use std::fmt::Debug;
use std::time::{Instant, Duration};
use futures::{future, Future, Stream};
use tokio_core::reactor::Handle;
use tokio_io::{AsyncRead, AsyncWrite};
use hyper::{self, Method, StatusCode};
use hyper::header::ContentType;
use hyper::server::{Http, Request, Response, Service};
use serde_json;
use monitor::{Monitor, ServerList, Throughput};


struct StatusPages {
    start_time: Instant,
    monitor: Monitor,
}

#[derive(Debug, Serialize)]
struct Status {
    servers: ServerList,
    uptime: Duration,
    throughput: Throughput,
}

impl StatusPages {
    fn new(start_time: Instant, monitor: Monitor) -> StatusPages {
        StatusPages { start_time,  monitor }
    }

    fn status_json(&self) -> serde_json::Result<String> {
        serde_json::to_string(&Status {
            servers: self.monitor.servers(),
            uptime: self.start_time.elapsed(),
            throughput: self.monitor.throughput(),
        })
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

pub fn run_server<I, S, A>(incoming: I, monitor: Monitor, handle: &Handle)
        -> Box<Future<Item=(), Error=()>>
where I: Stream<Item=(S, A), Error=io::Error> + 'static,
      S: AsyncRead + AsyncWrite + 'static,
      A: Debug {
    handle.spawn(monitor.monitor_throughput(handle));
    let start_time = Instant::now();
    let new_service = move ||
        Ok(StatusPages::new(start_time, monitor.clone()));
    let incoming = incoming.map(|(stream, addr)| {
        debug!("web server connected with {:?}", addr);
        stream
    });
    let serve = Http::new()
        .serve_incoming(incoming, new_service);
    let handle = handle.clone();
    let run = serve.for_each(move |conn| {
        handle.spawn(
            conn.map(|_| ())
                .map_err(|err| info!("http: {}", err))
        );
        Ok(())
    }).map_err(|err| error!("error on http server: {}", err));
    Box::new(run)
}

