extern crate nix;
extern crate net2;
extern crate futures;
extern crate tokio_core;
extern crate tokio_io;
extern crate simplelog;
#[macro_use]
extern crate clap;
#[macro_use]
extern crate log;
extern crate moproxy;
use std::net::{SocketAddr, SocketAddrV4};
use std::io::{self, ErrorKind};
use std::os::unix::io::{RawFd, AsRawFd};
use std::thread;
use std::sync::Arc;
use std::time::Duration;
use futures::{future, Future, Stream};
use tokio_core::net as tnet;
use tokio_core::reactor::{Core, Handle};
use nix::sys::socket;
use simplelog::{SimpleLogger, LogLevelFilter};
use moproxy::monitor::{self, ServerList};
use moproxy::proxy::{self, ProxyServer};
use moproxy::socks5::Socks5Server;
use moproxy::http::HttpProxyServer;


fn main() {
    let yaml = load_yaml!("cli.yml");
    let args = clap::App::from_yaml(yaml).get_matches();

    let log_level = match args.value_of("log-level") {
        None => LogLevelFilter::Info,
        Some("off") => LogLevelFilter::Off,
        Some("error") => LogLevelFilter::Error,
        Some("warn") => LogLevelFilter::Warn,
        Some("info") => LogLevelFilter::Info,
        Some("debug") => LogLevelFilter::Debug,
        Some("trace") => LogLevelFilter::Trace,
        Some(_) => panic!("unknown log level"),
    };
    SimpleLogger::init(log_level, simplelog::Config::default())
        .expect("cannot set logger");

    let host = args.value_of("host")
        .expect("missing host").parse()
        .expect("invalid address");
    let port = args.value_of("port")
        .expect("missing port number").parse()
        .expect("invalid port number");
    let addr = SocketAddr::new(host, port);

    let mut servers: Vec<Box<ProxyServer>> = vec![];
    if let Some(s) = args.values_of("socks5-servers") {
        for s in s.map(parse_server) {
            servers.push(Box::new(Socks5Server::new(s)));
        }
    }
    if let Some(s) = args.values_of("http-servers") {
        for s in s.map(parse_server) {
            servers.push(Box::new(HttpProxyServer::new(s)));
        }
    }
    if servers.len() == 0 {
        panic!("missing server list");
    }
    info!("total {} server(s) added", servers.len());
    let servers = Arc::new(ServerList::new(servers));

    {
        let probe = args.value_of("probe-secs")
            .expect("missing probe secs").parse()
            .expect("not a vaild probe secs");
        let servers = servers.clone();
        thread::spawn(move || monitor::monitoring_servers(servers, probe));
    }

    let mut lp = Core::new().expect("fail to create event loop");
    let handle = lp.handle();

    let listener = tnet::TcpListener::bind(&addr, &handle)
        .expect("cannot bind to port");
    info!("listen on {}", addr);
    let server = listener.incoming().for_each(move |(client, addr)| {
        debug!("incoming {}", addr);
        let conn = connect_server(client, servers.clone(), handle.clone());
        let serv = conn.and_then(|(client, proxy)| {
            let timeout = Some(Duration::from_secs(180));
            let result = client.set_keepalive(timeout)
                .and(proxy.set_keepalive(timeout));
            if let Err(e) = result {
                warn!("fail to set keepalive: {}", e);
            }
            proxy::piping(client, proxy)
        });
        handle.spawn(serv);
        Ok(())
    });
    lp.run(server).expect("fail to start event loop");
}

fn parse_server(addr: &str) -> SocketAddr {
    if addr.contains(":") {
        addr.parse()
    } else {
        format!("127.0.0.1:{}", addr).parse()
    }.expect("not a valid server address")
}

fn connect_server(client: tnet::TcpStream, servers: Arc<ServerList>,
                  handle: Handle)
        -> Box<Future<Item=(tnet::TcpStream, tnet::TcpStream), Error=()>> {
    let orig = client.peer_addr().ok();
    let dest = future::result(get_original_dest(client.as_raw_fd()))
            .map_err(|err| warn!("fail to get original destination: {}", err));
    let try_connect_all = dest.and_then(move |dest| {
        let conns: Vec<Box<Future<Item=tnet::TcpStream, Error=()>>> = servers
            .get().iter()
            .map(|info| info.server.clone())
            .map(|server| {
                let s1 = server.clone();
                let s2 = server.clone();
                let conn = server.connect_async(dest, handle.clone())
                    .map_err(move |err|
                             warn!("fail to connect {}: {}", s1.tag(), err))
                    .and_then(move |conn| {
                        let orig = orig.map_or_else(
                                || String::from("(?)"), |o| o.to_string());
                        info!("{} => {} via {}", orig, dest, s2.tag());
                        future::ok(conn)
                    });
                Box::new(conn) as Box<Future<Item=tnet::TcpStream, Error=()>>
            }).collect();
        future::select_ok(conns)
    }).map(|(server, _)| (client, server))
    .map_err(|_| warn!("all proxy server down"));
    Box::new(try_connect_all)
}

fn get_original_dest(fd: RawFd) -> io::Result<SocketAddr> {
    let addr = socket::getsockopt(fd, socket::sockopt::OriginalDst)
        .map_err(|e| match e {
            nix::Error::Sys(err) => io::Error::from(err),
            _ => io::Error::new(ErrorKind::Other, e),
        })?;
    let addr = SocketAddrV4::new(addr.sin_addr.s_addr.to_be().into(),
                                 addr.sin_port.to_be());
    // TODO: support IPv6
    Ok(SocketAddr::V4(addr))
}

