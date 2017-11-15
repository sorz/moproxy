extern crate nix;
extern crate net2;
extern crate futures;
extern crate tokio_core;
extern crate tokio_io;
extern crate tokio_timer;
extern crate env_logger;
#[macro_use]
extern crate clap;
#[macro_use]
extern crate log;
extern crate moproxy;
use std::cmp;
use std::env;
use std::rc::Rc;
use std::time::Duration;
use std::net::{SocketAddr, SocketAddrV4};
use std::io::{self, ErrorKind};
use std::os::unix::io::{RawFd, AsRawFd};
use futures::{future, stream, Future, Stream};
use tokio_core::net::{TcpListener, TcpStream};
use tokio_core::reactor::{Core, Handle};
use tokio_timer::Timer;
use nix::sys::socket;
use log::LogLevelFilter;
use env_logger::{LogBuilder, LogTarget};
use moproxy::monitor::{self, ServerList};
use moproxy::proxy::{self, ProxyServer};
use moproxy::socks5::Socks5Server;
use moproxy::http::HttpProxyServer;


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
    let servers = Rc::new(ServerList::new(servers));
    let probe = args.value_of("probe-secs")
        .expect("missing probe secs").parse()
        .expect("not a vaild probe secs");

    let mut lp = Core::new().expect("fail to create event loop");
    let handle = lp.handle();

    let listener = TcpListener::bind(&addr, &handle)
        .expect("cannot bind to port");
    info!("listen on {}", addr);
    let mon = monitor::monitoring_servers(
        servers.clone(), probe, lp.handle());
    handle.spawn(mon);
    let server = listener.incoming().for_each(move |(client, addr)| {
        debug!("incoming {}", addr);
        let conn = connect_server(client, &servers, handle.clone());
        let serv = conn.and_then(|(client, proxy)| {
            let timeout = Some(Duration::from_secs(180));
            if let Err(e) = client.set_keepalive(timeout)
                    .and(proxy.set_keepalive(timeout)) {
                warn!("fail to set keepalive: {}", e);
            }
            proxy::piping(client, proxy)
        });
        handle.spawn(serv);
        Ok(())
    });
    lp.run(server).expect("error on event loop");
}

fn parse_server(addr: &str) -> SocketAddr {
    if addr.contains(":") {
        addr.parse()
    } else {
        format!("127.0.0.1:{}", addr).parse()
    }.expect("not a valid server address")
}

fn connect_server(client: TcpStream, servers: &ServerList, handle: Handle)
        -> Box<Future<Item=(TcpStream, TcpStream), Error=()>> {
    let src_dst = future::result(client.peer_addr())
        .join(future::result(get_original_dest(client.as_raw_fd())))
        .map_err(|err| warn!("fail to get original destination: {}", err));
    // TODO: reuse timer?
    let timer = Timer::default();
    let infos = servers.get().clone();
    let try_connect_all = src_dst.and_then(move |(src, dest)| {
        stream::iter_ok(infos).for_each(move |info| {
            let server = info.server;
            let conn = server.connect(dest, &handle);
            let wait = Duration::from_millis(
                cmp::max(3_000, info.delay.unwrap_or(1_000) as u64 * 2));
            // Standard proxy server need more time (e.g. DNS resolving)
            timer.timeout(conn, wait).then(move |result| match result {
                Ok(conn) => {
                    info!("{} => {} via {}", src, dest, server.tag());
                    Err(conn)
                },
                Err(err) => {
                    warn!("fail to connect {}: {}", server.tag(), err);
                    Ok(())
                }
            })
        }).then(|result| match result {
            Err(conn) => Ok(conn),
            Ok(_) => {
                warn!("all proxy server down");
                Err(())
            },
        })
    }).map(|server| (client, server));
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

