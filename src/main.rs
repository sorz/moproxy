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
use std::net::{TcpListener, TcpStream, SocketAddrV4};
use std::io::{self, ErrorKind};
use std::error::Error;
use std::os::unix::io::{RawFd, AsRawFd};
use std::thread;
use std::sync::{Mutex, Arc};
use std::time::Duration;
use net2::TcpStreamExt;
use tokio_core::net as tnet;
use tokio_core::reactor;
use tokio_io::io as tio;
use tokio_io::AsyncRead;
use futures::{Stream, Sink, Future};
use futures::sync::mpsc;
use nix::sys::socket::{getsockopt, sockopt};
use simplelog::{SimpleLogger, LogLevelFilter};
use moproxy::{socks5, monitor};


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
    for server in args.values_of("servers").expect("missing server list") {
        let server = if server.contains(":") {
            server.parse()
        } else {
            format!("127.0.0.1:{}", server).parse()
        }.expect("not a valid server address");
        servers.push(server);
        debug!("server {} added", server);
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
    thread::spawn(move || piping_worker(rx));

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

fn connect_server(client: TcpStream, servers: &Arc<Mutex<Vec<SocketAddrV4>>>)
        -> io::Result<(TcpStream, TcpStream)> {
    let dest = get_original_dest(client.as_raw_fd())?;
    for server in servers.lock().unwrap().iter() {
        match init_socks(*server, dest) {
            Ok(server) => {
                info!("{} => {} via :{}", client.peer_addr()?,
                    dest, server.peer_addr()?.port());
                server.set_keepalive(Some(Duration::from_secs(300)))?;
                client.set_keepalive(Some(Duration::from_secs(300)))?;
                return Ok((client, server));
            },
            Err(_) => warn!("fail to connect {}", server),
        }
    }
    Err(io_error("all socks server down"))
}

fn init_socks(server: SocketAddrV4, dest: SocketAddrV4)
        -> io::Result<TcpStream> {
    let mut socks = TcpStream::connect(server)?;
    socks.set_nodelay(true)?;
    socks.set_read_timeout(Some(Duration::from_millis(100)))?;
    socks.set_write_timeout(Some(Duration::from_millis(100)))?;
    socks5::handshake(&mut socks, dest)?;
    Ok(socks)
}


fn get_original_dest(fd: RawFd) -> io::Result<SocketAddrV4> {
    let addr = getsockopt(fd, sockopt::OriginalDst)?;
    Ok(SocketAddrV4::new(addr.sin_addr.s_addr.to_be().into(),
                         addr.sin_port.to_be()))
}

fn piping_worker(rx: mpsc::Receiver<(TcpStream, TcpStream)>) {
    let mut core = reactor::Core::new().unwrap();
    let handle = core.handle();
    let server = rx.for_each(|(local, remote)| {
        let (lr, lw) = tnet::TcpStream::from_stream(local, &handle)
            .expect("cannot create asycn tcp stream").split();
        let (rr, rw) = tnet::TcpStream::from_stream(remote, &handle)
            .expect("cannot create asycn tcp stream").split();
        let piping = tio::copy(lr, rw)
            .join(tio::copy(rr, lw))
            .and_then(|((tx, _, rw), (rx, _, lw))| {
                debug!("tx {}, rx {} bytes", tx, rx);
                tio::shutdown(rw)
                    .join(tio::shutdown(lw))
            }).map(|_| ()).map_err(|e| warn!("piping error: {}", e));
        handle.spawn(piping);
        Ok(())
    });
    core.run(server).unwrap();
}
