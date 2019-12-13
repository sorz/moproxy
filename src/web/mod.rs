mod helpers;

use futures::pin_mut;
use helpers::{DurationExt, RequestExt};
use http;
use hyper::{
    server::{accept::Accept, conn::Http},
    service::{make_service_fn, service_fn},
    Body, Method, Request, Response, StatusCode,
};
use log::warn;
use prettytable::{cell, format::consts::FORMAT_NO_LINESEP_WITH_TITLE, row, Table};
use serde_derive::Serialize;
use std::{
    error::Error,
    fmt::Write,
    fs,
    future::Future,
    io,
    path::Path,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tokio::{
    self,
    io::{AsyncRead, AsyncWrite},
    net::{TcpListener, TcpStream, UnixListener, UnixStream},
};

use crate::{
    monitor::{Monitor, Throughput},
    proxy::{Delay, ProxyServer},
};

pub use hyper::server::accept::from_stream;

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

fn home_page(
    req: &Request<Body>,
    start_time: &Instant,
    monitor: &Monitor,
) -> http::Result<Response<Body>> {
    if req.accept_html() {
        Response::builder()
            .header("Content-Type", "text/html")
            .body(include_str!("index.html").into())
    } else {
        plaintext_status(start_time, monitor)
    }
}

fn plaintext_status(start_time: &Instant, monitor: &Monitor) -> http::Result<Response<Body>> {
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
        "Server", "Score", "Delay", "CUR", "TTL", "E16", "E64", "Up", "Down", "↑↓ bps"
    ]);
    table.set_format(*FORMAT_NO_LINESEP_WITH_TITLE);
    let mut total_alive_conns = 0;
    for ServerStatus { server, throughput } in status.servers {
        let status = server.status_snapshot();
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
        row.add_cell(cell!(r -> status.recent_error_count(16)));
        row.add_cell(cell!(r -> status.recent_error_count(64)));
        // Up Down
        row.add_cell(cell!(r -> helpers::to_human_bytes(status.traffic.tx_bytes)));
        row.add_cell(cell!(r -> helpers::to_human_bytes(status.traffic.rx_bytes)));
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

pub async fn run_server<A>(accept: A, monitor: Monitor)
where
    A: Accept,
    A::Error: Into<Box<dyn Error + Send + Sync>>,
    A::Conn: AsyncRead + AsyncWrite + Unpin + Send + 'static,
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
    let server = hyper::server::Builder::new(accept, http).serve(make_svc);
    if let Err(e) = server.await {
        warn!("web server error: {}", e);
    }
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

pub struct TcpAccept(TcpListener);
pub struct UnixAccept(UnixListener);

impl TcpAccept {
    pub fn new(listener: TcpListener) -> Self {
        Self(listener)
    }
}

impl UnixAccept {
    pub fn new(listener: UnixListener) -> Self {
        Self(listener)
    }
}

impl Accept for TcpAccept {
    type Conn = TcpStream;
    type Error = io::Error;

    fn poll_accept(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Self::Conn, Self::Error>>> {
        let accept = self.get_mut().0.accept();
        pin_mut!(accept);
        match accept.poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) => Poll::Ready(Some(result.map(|(stream, _)| stream))),
        }
    }
}

impl Accept for UnixAccept {
    type Conn = UnixStream;
    type Error = io::Error;

    fn poll_accept(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Self::Conn, Self::Error>>> {
        let accept = self.get_mut().0.accept();
        pin_mut!(accept);
        match accept.poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) => Poll::Ready(Some(result.map(|(stream, _)| stream))),
        }
    }
}
