mod helpers;
mod prometheus;
#[cfg(feature = "rich_web")]
mod rich;
use futures::Stream;
use helpers::{DurationExt, RequestExt};
use hyper::{
    server::{accept::from_stream, conn::Http},
    service::{make_service_fn, service_fn},
    Body, Method, Request, Response, StatusCode,
};
#[cfg(feature = "rich_web")]
use lazy_static::lazy_static;
use log::warn;
use prettytable::{cell, format::consts::FORMAT_NO_LINESEP_WITH_TITLE, row, Table};
use serde_derive::Serialize;
use std::{
    error::Error,
    fmt::Write,
    fs,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};
#[cfg(unix)]
use tokio::{
    self,
    io::{AsyncRead, AsyncWrite},
};

use crate::{
    monitor::{Monitor, Throughput},
    proxy::{Delay, ProxyServer},
};

#[cfg(feature = "rich_web")]
lazy_static! {
    static ref BUNDLE: rich::ResourceBundle = rich::ResourceBundle::new();
}

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
            servers,
            throughput,
            uptime: start_time.elapsed(),
        }
    }
}

fn home_page(req: &Request<Body>, start_time: &Instant, monitor: &Monitor) -> Response<Body> {
    if req.accept_html() {
        #[cfg(feature = "rich_web")]
        let resp = BUNDLE.get("/index.html").map(|(mime, content)| {
            Response::builder()
                .header("Content-Type", mime)
                .body(content.into())
                .unwrap()
        });
        #[cfg(not(feature = "rich_web"))]
        let resp = None;
        resp.unwrap_or_else(|| {
            Response::builder()
                .header("Content-Type", "text/html")
                .body(include_str!("index.html").into())
                .unwrap()
        })
    } else {
        plaintext_status_response(start_time, monitor)
    }
}

fn plaintext_status(start_time: &Instant, monitor: &Monitor) -> String {
    let status = Status::from(start_time, monitor);
    let mut buf = String::new();

    writeln!(
        &mut buf,
        "moproxy ({}) is running. {}",
        env!("CARGO_PKG_VERSION"),
        status.uptime.format()
    )
    .unwrap();

    let mut table = Table::new();
    table.add_row(row![
        "Server",
        "Score",
        "Delay",
        "CUR",
        "TTL",
        "E16:64",
        "Up",
        "Down",
        "↑↓ bps"
    ]);
    table.set_format(*FORMAT_NO_LINESEP_WITH_TITLE);
    let mut total_alive_conns = 0;
    for ServerStatus { server, throughput } in status.servers {
        let status = server.status_snapshot();
        let traffic = server.traffic();
        total_alive_conns += status.conn_alive;
        let row = table.add_empty_row();
        // Server
        row.add_cell(cell!(l -> server.tag));
        // Score
        if let Some(v) = status.score {
            row.add_cell(cell!(r -> v));
        } else {
            row.add_cell(cell!(r -> "-"));
        }
        // Delay
        if let Delay::Some(v) = status.delay {
            row.add_cell(cell!(r -> v.format_millis()));
        } else {
            row.add_cell(cell!(r -> "-"));
        }
        // CUR TTL
        row.add_cell(cell!(r -> status.conn_alive));
        row.add_cell(cell!(r -> status.conn_total));
        // Error rate
        // TODO: document the two columns
        row.add_cell(cell!(r ->
            format!("{:02}:{:02}",
                status.recent_error_count( 16),
                status.recent_error_count(64),
            )
        ));
        // Up Down
        row.add_cell(cell!(r -> helpers::to_human_bytes(traffic.tx_bytes)));
        row.add_cell(cell!(r -> helpers::to_human_bytes(traffic.rx_bytes)));
        // ↑↓
        if let Some(tp) = throughput {
            let sum = tp.tx_bps + tp.rx_bps;
            if sum > 0 {
                row.add_cell(cell!(r -> helpers::to_human_bps_prefix_only(sum)));
            }
        }
    }

    writeln!(
        &mut buf,
        "[{}] ↑ {} ↓ {}\n{}",
        total_alive_conns,
        helpers::to_human_bps(status.throughput.tx_bps),
        helpers::to_human_bps(status.throughput.rx_bps),
        table
    )
    .unwrap();
    buf
}

fn plaintext_status_response(start_time: &Instant, monitor: &Monitor) -> Response<Body> {
    Response::builder()
        .header("Content-Type", "text/plain; charset=utf-8")
        .body(plaintext_status(start_time, monitor).into())
        .unwrap()
}

fn response(req: &Request<Body>, start_time: &Instant, monitor: &Monitor) -> Response<Body> {
    if req.method() != Method::GET {
        return Response::builder()
            .status(StatusCode::METHOD_NOT_ALLOWED)
            .header("Allow", "GET")
            .header("Content-Type", "text/plain")
            .body("only GET is allowed".into())
            .unwrap();
    }

    match req.uri().path() {
        "/" | "/index.html" => home_page(req, start_time, monitor),
        "/plain" => plaintext_status_response(start_time, monitor),
        "/version" => Response::builder()
            .header("Content-Type", "text/plain")
            .body(env!("CARGO_PKG_VERSION").into())
            .unwrap(),
        "/status" => {
            let json = serde_json::to_string(&Status::from(start_time, monitor))
                .expect("fail to serialize servers to json");
            Response::builder()
                .header("Content-Type", "application/json")
                .body(json.into())
                .unwrap()
        }
        "/metrics" => prometheus::exporter(start_time, monitor),
        path => {
            #[cfg(feature = "rich_web")]
            let resp = BUNDLE.get(path).map(|(mime, body)| {
                Response::builder()
                    .header("Content-Type", mime)
                    .body(body.into())
                    .unwrap()
            });
            #[cfg(not(feature = "rich_web"))]
            let resp = None;
            resp.unwrap_or_else(|| {
                Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .header("Content-Type", "text/plain")
                    .body("page not found".into())
                    .unwrap()
            })
        }
    }
}

pub async fn run_server<S, IO, E>(stream: S, monitor: Monitor)
where
    S: Stream<Item = Result<IO, E>>,
    E: Into<Box<dyn Error + Send + Sync>>,
    IO: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    tokio::spawn(monitor.clone().monitor_throughput());
    let start_time = Instant::now();

    let make_svc = make_service_fn(move |_sock| {
        let monitor = monitor.clone();
        async move {
            Ok::<_, hyper::Error>(service_fn(move |req| {
                let monitor = monitor.clone();
                async move { Ok::<_, hyper::Error>(response(&req, &start_time, &monitor)) }
            }))
        }
    });
    let http = Http::new();
    let accept = from_stream(stream);
    let server = hyper::server::Builder::new(accept, http).serve(make_svc);
    if let Err(e) = server.await {
        warn!("web server error: {}", e);
    }
    warn!("web server stopped");
}

/// File on this path will be removed on `drop()`.
pub struct AutoRemoveFile<'a> {
    path: &'a str,
}

impl<'a> AutoRemoveFile<'a> {
    pub fn new(path: &'a str) -> Self {
        AutoRemoveFile { path }
    }
}

impl<'a> Drop for AutoRemoveFile<'a> {
    fn drop(&mut self) {
        if let Err(err) = fs::remove_file(&self.path) {
            warn!("fail to remove {}: {}", self.path, err);
        }
    }
}

impl<'a> AsRef<Path> for &'a AutoRemoveFile<'a> {
    fn as_ref(&self) -> &Path {
        self.path.as_ref()
    }
}
