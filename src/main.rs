extern crate net2;
extern crate futures;
extern crate tokio_core;
extern crate tokio_io;
extern crate tokio_timer;
extern crate env_logger;
extern crate ini;
#[macro_use]
extern crate clap;
#[macro_use]
extern crate log;
extern crate moproxy;
use std::env;
use std::thread;
use std::sync::Arc;
use std::net::SocketAddr;
use ini::Ini;
use futures::{Future, Stream};
use tokio_core::net::TcpListener;
use tokio_core::reactor::Core;
use log::LogLevelFilter;
use env_logger::{LogBuilder, LogTarget};
use moproxy::monitor::{self, ServerList};
use moproxy::proxy::ProxyServer;
use moproxy::proxy::ProxyProto::{Socks5, Http};
use moproxy::client::{NewClient, Connectable};
use moproxy::web;


fn main() {
    let yaml = load_yaml!("cli.yml");
    let args = clap::App::from_yaml(yaml).get_matches();

    let mut logger = LogBuilder::new();
    if let Ok(env_log) = env::var("RUST_LOG") {
        logger.parse(&env_log);
    }
    let log_level = args.value_of("log-level")
        .unwrap_or("info").parse()
        .expect("unknown log level");
    logger.filter(None, log_level)
        .filter(Some("tokio_core"), LogLevelFilter::Warn)
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
    let addr = SocketAddr::new(host, port);
    let probe = args.value_of("probe-secs")
        .expect("missing probe secs").parse()
        .expect("not a vaild probe secs");
    let remote_dns = args.is_present("remote-dns");
    let n_parallel = args.value_of("n-parallel")
        .map(|v| v.parse().expect("not a valid number"))
        .unwrap_or(0 as usize);

    let servers = parse_servers(&args);
    if servers.len() == 0 {
        panic!("missing server list");
    }
    info!("total {} server(s) added", servers.len());
    let servers = Arc::new(ServerList::new(servers));

    if let Some(addr) = args.value_of("web-bind") {
        let servers = servers.clone();
        let addr = addr.parse()
            .expect("not a valid address");
        thread::spawn(move || web::run_server(addr, servers));
    }

    let mut lp = Core::new().expect("fail to create event loop");
    let handle = lp.handle();

    let listener = TcpListener::bind(&addr, &handle)
        .expect("cannot bind to port");
    info!("listen on {}", addr);
    let mon = monitor::monitoring_servers(
        servers.clone(), probe, lp.handle());
    handle.spawn(mon);
    let server = listener.incoming().for_each(move |(sock, addr)| {
        debug!("incoming {}", addr);
        let client = NewClient::from_socket(
            sock, servers.clone(), handle.clone());
        let conn = client.and_then(move |client| 
            if remote_dns && client.dest.port == 443 {
                Box::new(client.retrive_dest().and_then(move |client| {
                    client.connect_server(n_parallel)
                }))
            } else {
                client.connect_server(0)
            });
        let serv = conn.and_then(|client| client.serve());
        handle.spawn(serv);
        Ok(())
    });
    lp.run(server).expect("error on event loop");
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

