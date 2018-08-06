use std::sync::Arc;
use std::net::SocketAddr;
use std::time::{Instant, Duration};
use futures::Future;
use tokio_core::reactor::Handle;
use hyper::{Body, Request, Response, Server, StatusCode, Method};
use hyper::service::service_fn_ok;
use serde_json;

use monitor::{Monitor, Throughput};
use proxy::ProxyServer;


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

fn status_json(start_time: &Instant, monitor: &Monitor)
        -> serde_json::Result<String> {
    let mut thps = monitor.throughputs();
    let throughput = thps.values().fold(Default::default(), |a, b| a + *b);
    let servers = monitor.servers().into_iter().map(|server| ServerStatus {
        throughput: thps.remove(&server), server,
    }).collect();
    serde_json::to_string(&Status {
        servers, throughput,
        uptime: start_time.elapsed(),
    })
}

fn response(req: Request<Body>, start_time: &Instant, monitor: &Monitor)
        -> Response<Body> {
    let mut resp = Response::builder();
    match (req.method(), req.uri().path()) {
        (&Method::GET, "/") => resp
            .header("Content-Type", "text/html")
            .body(include_str!("index.html").into()),

        (&Method::GET, "/version") => resp
            .header("Content-Type", "text/plain")
            .body(env!("CARGO_PKG_VERSION").into()),

        (&Method::GET, "/status") => match status_json(start_time, monitor) {
            Ok(json) => resp
                .header("Content-Type", "application/json")
                .body(json.into()),
            Err(e) => {
                error!("fail to serialize servers to json: {}", e);
                resp.status(StatusCode::INTERNAL_SERVER_ERROR)
                    .header("Content-Type", "text/plain")
                    .body(format!("internal error: {}", e).into())
            },
        },
        _ => resp.status(StatusCode::NOT_FOUND)
                .header("Content-Type", "text/plain")
                .body("page not found".into()),
    }.unwrap()
}

pub fn run_server(addr: SocketAddr, monitor: Monitor, handle: &Handle)
        -> impl Future<Item=(), Error=()> {
    handle.spawn(monitor.monitor_throughput(handle));
    let start_time = Instant::now();

    let new_service = move || {
        let monitor = monitor.clone();
        service_fn_ok(move |req| {
            response(req, &start_time, &monitor)
        })
    };
    let server = Server::bind(&addr)
        .serve(new_service);

    server.map_err(|err| error!("error on http server: {}", err))
}
