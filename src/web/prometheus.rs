use hyper::{Body, Response};
use std::{
    fmt::{Display, Write},
    time::Instant,
};

use super::{ServerStatus, Status};
use crate::{monitor::Monitor, proxy::Delay};

fn new_metric(buf: &mut String, name: &str, metric_type: &str, help: &str) {
    writeln!(buf, "# HELP moproxy_{} {}", name, help).unwrap();
    writeln!(buf, "# TYPE moproxy_{} {}", name, metric_type).unwrap();
}

fn each_server<F, D>(buf: &mut String, name: &str, servers: &[ServerStatus], metric: F)
where
    F: Fn(&ServerStatus) -> Option<D>,
    D: Display,
{
    for s in servers {
        if let Some(value) = metric(s) {
            writeln!(
                buf,
                "moproxy_{}{{server=\"{}\"}} {}",
                name, s.server.tag, value
            )
            .unwrap();
        }
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
        |s| Some(s.server.status_snapshot().traffic.tx_bytes)
    );
    server_gauge!(
        "proxy_server_bytes_rx_total",
        "Current total of incoming bytes",
        |s| Some(s.server.status_snapshot().traffic.rx_bytes)
    );
    server_gauge!(
        "proxy_server_connections_alive",
        "Current number of alive connections",
        |s| Some(s.server.status_snapshot().conn_alive)
    );
    server_gauge!(
        "proxy_server_connections_error",
        "Current number of connections closed with error",
        |s| Some(s.server.status_snapshot().conn_error)
    );
    server_gauge!(
        "proxy_server_connections_total",
        "Current total number of connections",
        |s| Some(s.server.status_snapshot().conn_total)
    );
    server_gauge!(
        "proxy_server_dns_delay_seconds",
        "Total seconds for the last DNS query test",
        |s| match s.server.status_snapshot().delay {
            Delay::Some(d) => Some(d.as_secs() as f32 + d.subsec_millis() as f32 / 1000.0),
            _ => None,
        }
    );
    server_gauge!(
        "proxy_server_score",
        "Score of server based on the last DNS query test",
        |s| s.server.status_snapshot().score
    );

    Response::builder()
        .header("Content-Type", "text/plain; charset=utf-8")
        .body(buf.into())
}
