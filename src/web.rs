mod helpers;

use futures::{Future, Stream};
use http;
use hyper::{
    server::conn::Http, service::service_fn_ok, Body, Method, Request, Response, StatusCode,
};
use helpers::{RequestExt, DurationExt, to_human_bytes, to_human_bps};
use log::{debug, error};
use prettytable::{Table, row, cell, format::consts::FORMAT_NO_LINESEP_WITH_TITLE};
use serde_derive::Serialize;
use std::{
    fmt::{Debug, Write},
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


fn home_page(req: &Request<Body>, start_time: &Instant, monitor: &Monitor)
        -> http::Result<Response<Body>>
{
    if req.accept_html() {
        Response::builder()
            .header("Content-Type", "text/html")
            .body(include_str!("web/index.html").into())
    } else {
        plaintext_status(start_time, monitor)
    }
}

fn plaintext_status(start_time: &Instant, monitor: &Monitor)
        -> http::Result<Response<Body>>
{
    let status = Status::from(start_time, monitor);
    let mut buf = String::new();

    writeln!(&mut buf, "moproxy ({}) is running. {}",
        env!("CARGO_PKG_VERSION"), status.uptime.format()
    ).unwrap();

    writeln!(&mut buf, "↑ {} ↓ {}",
        to_human_bps(status.throughput.tx_bps),
        to_human_bps(status.throughput.rx_bps),
    ).unwrap();

    let mut table = Table::new();
    table.add_row(row!["Server", "Score", "Delay", "CUR", "TTL", "Up", "Down", "↑↓"]);
    table.set_format(*FORMAT_NO_LINESEP_WITH_TITLE);

    for ServerStatus { server, throughput } in status.servers {
        let row = table.add_empty_row();
        // Server
        row.add_cell(cell!(l -> server.tag));
        // Score
        if let Some(v) = server.score() {
            row.add_cell(cell!(r -> v));
        } else {
            row.add_cell(cell!(r -> "-"));
        }
        // Delay
        if let Some(v) = server.delay() {
            row.add_cell(cell!(r -> v.format_millis()));
        } else {
            row.add_cell(cell!(r -> "-"));
        }
        // CUR TTL
        row.add_cell(cell!(r -> server.conn_alive()));
        row.add_cell(cell!(r -> server.conn_total()));
        // Up Down
        row.add_cell(cell!(r -> to_human_bytes(server.traffic().tx_bytes)));
        row.add_cell(cell!(r -> to_human_bytes(server.traffic().rx_bytes)));
        // ↑↓
        if let Some(tp) = throughput {
            let sum = tp.tx_bps + tp.rx_bps;
            if sum > 0 {
                row.add_cell(cell!(r -> to_human_bps(sum)));
            }
        }
    }
    write!(&mut buf, "{}", table).unwrap();

    Response::builder()
        .header("Content-Type", "text/plain; charset=utf-8")
        .body(buf.into())
}


fn response(req: &Request<Body>, start_time: &Instant, monitor: &Monitor) -> Response<Body> {
    match (req.method(), req.uri().path()) {
        (&Method::GET, "/") => home_page(req, start_time, monitor),
        (&Method::GET, "/plain") => plaintext_status(start_time, monitor),
        (&Method::GET, "/version") => Response::builder()
            .header("Content-Type", "text/plain")
            .body(env!("CARGO_PKG_VERSION").into()),
        (&Method::GET, "/status") => {
            let json = serde_json::to_string(&Status::from(start_time, monitor))
                .expect("fail to serialize servers to json");
            Response::builder()
                .header("Content-Type", "application/json")
                .body(json.into())
        }
        _ => Response::builder()
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
