use hyper::{Body, Response};
use std::{fmt::Write, time::Instant};

use super::{ServerStatus, Status};
use crate::monitor::Monitor;

fn new_metric(buf: &mut String, name: &str, metric_type: &str, help: &str) {
    writeln!(buf, "# HELP moproxy_{} {}", name, help).unwrap();
    writeln!(buf, "# TYPE moproxy_{} {}", name, metric_type).unwrap();
}

fn each_server<F>(buf: &mut String, name: &str, servers: &[ServerStatus], metric: F)
where
    F: Fn(&ServerStatus) -> usize,
{
    for s in servers {
        writeln!(
            buf,
            "moproxy_{}{{server=\"{}\"}} {}",
            name,
            s.server.tag,
            metric(s)
        )
        .unwrap();
    }
}

pub fn exporter(start_time: &Instant, monitor: &Monitor) -> http::Result<Response<Body>> {
    let status = Status::from(start_time, monitor);
    let mut buf = String::new();

    macro_rules! server_gauge {
        ($name:expr, $help:expr, $func:expr) => {
            new_metric(&mut buf, $name, "gauge", $help);
            each_server(&mut buf, $name, &status.servers, $func);
            writeln!(&mut buf).unwrap();
        };
    }

    server_gauge!(
        "proxy_server_bytes_tx_total",
        "Current total of outgoing bytes",
        |s| s.server.status_snapshot().traffic.tx_bytes
    );
    server_gauge!(
        "proxy_server_bytes_rx_total",
        "Current total of incoming bytes",
        |s| s.server.status_snapshot().traffic.rx_bytes
    );
    server_gauge!(
        "proxy_server_current_alive_connections",
        "Current number of alive connections",
        |s| s.server.status_snapshot().conn_alive as usize
    );

    Response::builder()
        .header("Content-Type", "text/plain; charset=utf-8")
        .body(buf.into())
}
