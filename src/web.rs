use futures::{Future, Stream};
use http;
use hyper::{
    http::response::Builder as ResponseBuilder,
    server::conn::Http, service::service_fn_ok, Body, Method, Request, Response, StatusCode,
};
use log::{debug, error};
use serde_derive::Serialize;
use std::{
    fmt::Debug,
    io,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio_core::reactor::Handle;
use tokio_io::{AsyncRead, AsyncWrite};

use crate::{
    monitor::{Monitor, Throughput},
    proxy::ProxyServer,
};

#[derive(Debug, Serialize)]
struct ServerStatus {
    server: Arc<ProxyServer>,
    throughput: Option<Throughput>,
}

#[derive(Debug, Serialize)]
struct Status {
    servers: Vec<ServerStatus>,
    uptime: Duration,
    throughput: Throughput,
}

impl Status {
    fn from(start_time: &Instant, monitor: &Monitor) -> Self {
        let mut thps = monitor.throughputs();
        let throughput = thps.values().fold(Default::default(), |a, b| a + *b);
        let servers = monitor
            .servers()
            .into_iter()
            .map(|server| ServerStatus {
                throughput: thps.remove(&server),
                server,
            })
            .collect();
        Status {
            servers, throughput,
            uptime: start_time.elapsed(),
        }
    }
}


fn home_page(req: &Request<Body>, mut resp: ResponseBuilder,
             start_time: &Instant, monitor: &Monitor) -> http::Result<Response<Body>> {
    resp
        .header("Content-Type", "text/html")
        .body(include_str!("web/index.html").into())
}

fn response(req: &Request<Body>, start_time: &Instant, monitor: &Monitor) -> Response<Body> {
    let mut resp = Response::builder();
    match (req.method(), req.uri().path()) {
        (&Method::GET, "/") => home_page(req, resp, start_time, monitor),
        (&Method::GET, "/version") => resp
            .header("Content-Type", "text/plain")
            .body(env!("CARGO_PKG_VERSION").into()),

        (&Method::GET, "/status") => {
            let json = serde_json::to_string(&Status::from(start_time, monitor))
                .expect("fail to serialize servers to json");
            resp.header("Content-Type", "application/json")
                .body(json.into())
        }
        _ => resp
            .status(StatusCode::NOT_FOUND)
            .header("Content-Type", "text/plain")
            .body("page not found".into()),
    }
    .unwrap()
}

pub fn run_server<I, S, A>(
    incoming: I,
    monitor: Monitor,
    handle: &Handle,
) -> impl Future<Item = (), Error = ()>
where
    I: Stream<Item = (S, A), Error = io::Error> + 'static,
    S: AsyncRead + AsyncWrite + Send + 'static,
    A: Debug,
{
    handle.spawn(monitor.monitor_throughput(handle.clone()));
    let start_time = Instant::now();

    let new_service = move || {
        let monitor = monitor.clone();
        service_fn_ok(move |req| response(&req, &start_time, &monitor))
    };

    let incoming = incoming.map(|(conn, addr)| {
        debug!("web server connected with {:?}", addr);
        conn
    });
    let http = Http::new().with_executor(handle.remote().clone());
    let server = hyper::server::Builder::new(incoming, http).serve(new_service);

    server.map_err(|err| error!("error on http server: {}", err))
}
