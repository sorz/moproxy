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
use std::fmt;
use std::net::{TcpListener, TcpStream, SocketAddr, SocketAddrV4};
use std::io::{self, ErrorKind};
use std::error::Error;
use std::os::unix::io::{RawFd, AsRawFd};
use std::thread;
use std::sync::{Mutex, Arc};
use std::time::Duration;
use net2::TcpStreamExt;
use futures::sync::mpsc;
use futures::{Sink, Future};
use nix::sys::socket;
use simplelog::{SimpleLogger, LogLevelFilter};
use moproxy::monitor;
use moproxy::socks5::Socks5Server;
use moproxy::proxy::{self, ProxyServer};


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
        .expect("missing host");
    let port = args.value_of("port")
        .expect("missing port number").parse()
        .expect("invalid port number");
    let listener = TcpListener::bind((host, port))
        .expect("cannot bind to port");
    info!("listen on {}:{}", host, port);

    let mut servers = vec![];
    for addr in args.values_of("servers").expect("missing server list") {
        let addr = if addr.contains(":") {
            addr.parse()
        } else {
            format!("127.0.0.1:{}", addr).parse()
        }.expect("not a valid server address");
        let server = Socks5Server::new(addr);
        debug!("server {} added", server);
        servers.push(server);
    }
    info!("total {} server(s) added", servers.len());

    let servers = Arc::new(Mutex::new(servers));
    {
        let probe = args.value_of("probe-secs")
            .expect("missing probe secs").parse()
            .expect("not a vaild probe secs");
        let servers = servers.clone();
        thread::spawn(move || monitor::monitoring_servers(servers, probe));
    }

    let (tx, rx) = mpsc::channel(1);
    thread::spawn(move || proxy::piping_worker(rx));

    for client in listener.incoming() {
        match client.and_then(|c| connect_server(c, &servers)) {
            Ok(pair) => tx.clone().send(pair).wait()
                .expect("fail send to piping thread"),
            Err(e) => {
                warn!("error: {}", e);
                continue;
            }
        };
    }
}

fn io_error<E: Into<Box<Error + Send + Sync>>>(e: E) -> io::Error {
    io::Error::new(ErrorKind::Other, e)
}

fn connect_server<S>(client: TcpStream, servers: &Arc<Mutex<Vec<S>>>)
        -> io::Result<(TcpStream, TcpStream)>
where S: ProxyServer + fmt::Display {
    let dest = get_original_dest(client.as_raw_fd())?;
    for server in servers.lock().unwrap().iter() {
        match server.connect(dest) {
            Ok(server) => {
                info!("{} => {} via :{}", client.peer_addr()?,
                    dest, server.peer_addr()?.port());
                server.set_keepalive(Some(Duration::from_secs(180)))?;
                client.set_keepalive(Some(Duration::from_secs(180)))?;
                return Ok((client, server));
            },
            Err(_) => warn!("fail to connect {}", server),
        }
    }
    Err(io_error("all socks server down"))
}


fn get_original_dest(fd: RawFd) -> io::Result<SocketAddr> {
    let addr = socket::getsockopt(fd, socket::sockopt::OriginalDst)?;
    let addr = SocketAddrV4::new(addr.sin_addr.s_addr.to_be().into(),
                                 addr.sin_port.to_be());
    // TODO: support IPv6
    Ok(SocketAddr::V4(addr))
}

