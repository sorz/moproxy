extern crate net2;
extern crate futures;
extern crate tokio_core;
extern crate tokio_io;
extern crate tokio_timer;
extern crate tokio_uds;
extern crate env_logger;
extern crate ini;
#[macro_use]
extern crate clap;
#[macro_use]
extern crate log;
extern crate systemd;
extern crate moproxy;

use std::fs;
use std::env;
use std::path::Path;
use std::net::SocketAddr;
use std::collections::HashMap;
use ini::Ini;
use futures::{Future, Stream};
use tokio_core::net::TcpListener;
use tokio_core::reactor::Core;
use tokio_uds::UnixListener;
use log::LogLevelFilter;
use env_logger::{LogBuilder, LogTarget};
use systemd::daemon;

use moproxy::monitor::Monitor;
use moproxy::proxy::ProxyServer;
use moproxy::proxy::copy::SharedBuf;
use moproxy::proxy::ProxyProto::{Socks5, Http};
use moproxy::client::{NewClient, Connectable};
use moproxy::web;


fn main() {
    let yaml = load_yaml!("cli.yml");
    let args = clap::App::from_yaml(yaml)
        .version(env!("CARGO_PKG_VERSION"))
        .get_matches();

    let mut logger = LogBuilder::new();
    if let Ok(env_log) = env::var("RUST_LOG") {
        logger.parse(&env_log);
    }
    let log_level = args.value_of("log-level")
        .unwrap_or("info").parse()
        .expect("unknown log level");
    logger.filter(None, log_level)
        .filter(Some("tokio_core"), LogLevelFilter::Warn)
        .filter(Some("hyper"), LogLevelFilter::Warn)
        .filter(Some("ini"), LogLevelFilter::Warn)
        .target(LogTarget::Stdout)
        .format(|r| format!("[{}] {}", r.level(), r.args()))
        .init()
        .expect("cannot set logger");

    let host = args.value_of("host")
        .expect("missing host").parse()
        .expect("invalid address");
    let port = args.value_of("port")
        .expect("missing port number").parse()
        .expect("invalid port number");
    let bind_addr = SocketAddr::new(host, port);
    let probe = args.value_of("probe-secs")
        .expect("missing probe secs").parse()
        .expect("not a vaild probe secs");
    let remote_dns = args.is_present("remote-dns");
    let n_parallel = args.value_of("n-parallel")
        .map(|v| v.parse().expect("not a valid number"))
        .unwrap_or(0 as usize);
    let systemd = args.is_present("systemd");

    let servers = parse_servers(&args);
    if servers.len() == 0 {
        panic!("missing server list");
    }
    info!("total {} server(s) added", servers.len());
    let monitor = Monitor::new(servers);

    let mut lp = Core::new().expect("fail to create event loop");
    let handle = lp.handle();

    let mut sock_file = None;
    if let Some(http_addr) = args.value_of("web-bind") {
        let monitor = monitor.clone();
        let serv = if http_addr.starts_with("/") {
            let sock = AutoRemoveFile::new(&http_addr);
            let incoming = UnixListener::bind(&sock, &handle)
                .expect("fail to bind web server")
                .incoming();
            sock_file = Some(sock);
            web::run_server(incoming, monitor, &handle)
        } else {
            // FIXME: remove duplicate code
            let addr = http_addr.parse()
                .expect("not a valid address of TCP socket");
            let incoming = TcpListener::bind(&addr, &handle)
                .expect("fail to bind web server")
                .incoming();
            web::run_server(incoming, monitor, &handle)
        };
        handle.spawn(serv);
        info!("http run on {}", http_addr);
    }

    let listener = TcpListener::bind(&bind_addr, &handle)
        .expect("cannot bind to port");
    info!("listen on {}", bind_addr);
    handle.spawn(monitor.monitor_delay(probe, lp.handle()));
    let shared_buf = SharedBuf::new(8192);
    let server = listener.incoming().for_each(move |(sock, addr)| {
        debug!("incoming {}", addr);
        let client = NewClient::from_socket(
            sock, monitor.servers(), handle.clone());
        let conn = client.and_then(move |client| 
            if remote_dns && client.dest.port == 443 {
                Box::new(client.retrive_dest().and_then(move |client| {
                    client.connect_server(n_parallel)
                }))
            } else {
                client.connect_server(0)
            });
        let buf = shared_buf.clone();
        let serv = conn.and_then(|client| client.serve(buf));
        handle.spawn(serv);
        Ok(())
    });
    
    if systemd {
        let status = format!("Proxy listen on {}", bind_addr);
        let state: HashMap<&str, &str> = 
            [(daemon::STATE_READY, "1"),
             (daemon::STATE_STATUS, &status)
            ].iter().cloned().collect();
        daemon::notify(true, state)
            .expect("fail to notify systemd");
    }
    lp.run(server).expect("error on event loop");

    // make sure socket file will be deleted on exit.
    // unnecessary drop() but make complier happy about unused var.
    drop(sock_file);
}

fn parse_servers(args: &clap::ArgMatches) -> Vec<ProxyServer> {
    let default_test_dns = args.value_of("test-dns")
        .expect("missing test-dns").parse()
        .expect("not a valid socket address");
    let mut servers: Vec<ProxyServer> = vec![];
    if let Some(s) = args.values_of("socks5-servers") {
        for s in s.map(parse_server) {
            servers.push(ProxyServer::new(
                    s, Socks5, default_test_dns, None, None));
        }
    }
    if let Some(s) = args.values_of("http-servers") {
        for s in s.map(parse_server) {
            servers.push(ProxyServer::new(
                    s, Http, default_test_dns, None, None));
        }
    }
    if let Some(path) = args.value_of("server-list") {
        let ini = Ini::load_from_file(path)
            .expect("cannot read server list file");
        for (tag, props) in ini.iter() {
            let tag = if let Some(s) = props.get("tag") {
                Some(s.as_str())
            } else if let Some(ref s) = *tag {
                Some(s.as_str())
            } else {
                None
            };
            let addr: SocketAddr = props.get("address")
                .expect("address not specified").parse()
                .expect("not a valid socket address");
            let proto = props.get("protocol")
                .expect("protocol not specified").parse()
                .expect("unknown proxy protocol");
            let base = props.get("score base").map(|i| i.parse()
                .expect("score base not a integer"));
            let test_dns = props.get("test dns").map(|i| i.parse()
                .expect("not a valid socket address"))
                .unwrap_or(default_test_dns);
            servers.push(ProxyServer::new(addr, proto, test_dns, tag, base));
        }
    }
    servers
}

fn parse_server(addr: &str) -> SocketAddr {
    if addr.contains(":") {
        addr.parse()
    } else {
        format!("127.0.0.1:{}", addr).parse()
    }.expect("not a valid server address")
}

/// File on this path will be removed on `drop()`.
struct AutoRemoveFile<'a> {
    path: &'a str,
}

impl<'a> AutoRemoveFile<'a> {
    fn new(path: &'a str) -> Self {
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

