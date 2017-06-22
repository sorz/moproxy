extern crate nix;
extern crate futures;
extern crate tokio_core;
extern crate tokio_io;
extern crate moproxy;
use std::net::{TcpListener, TcpStream, SocketAddrV4};
use std::io::{self, ErrorKind};
use std::error::Error;
use std::os::unix::io::{RawFd, AsRawFd};
use std::{env, thread};
use std::sync::{Mutex, Arc};
use std::time::Duration;
use tokio_core::net as tnet;
use tokio_core::reactor;
use tokio_io::io as tio;
use tokio_io::AsyncRead;
use futures::{Stream, Sink, Future};
use futures::sync::mpsc;
use nix::sys::socket::{getsockopt, sockopt};
use moproxy::{socks5, monitor};


fn main() {
    let port = env::args().nth(1).unwrap_or(String::from("10081")).parse()
        .expect("invalid port number");
    let listener = TcpListener::bind(("0.0.0.0", port))
        .expect("cannot bind to port");

    let mut servers = vec![];
    servers.push("127.0.0.1:8130".parse().unwrap());
    servers.push("127.0.0.1:8131".parse().unwrap());
    servers.push("127.0.0.1:8132".parse().unwrap());
    servers.push("127.0.0.1:8133".parse().unwrap());

    let servers = Arc::new(Mutex::new(servers));
    {
        let servers = servers.clone();
        thread::spawn(move || monitor::monitoring_servers(servers));
    }

    let (tx, rx) = mpsc::channel(1);
    thread::spawn(move || piping_worker(rx));

    for client in listener.incoming() {
        client.and_then(|client| connect_server(client, &servers))
            .and_then(|pair| tx.clone().send(pair).wait().map_err(io_error))
            .map_err(|e| println!("error: {}", e))
            .ok();
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
                println!("{} => {} via :{}", client.peer_addr()?,
                    dest, server.peer_addr()?.port());
                return Ok((client, server));
            },
            Err(_) => println!("fail to connect {}", server),
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

    socks.set_read_timeout(Some(Duration::from_secs(555)))?;
    socks.set_write_timeout(Some(Duration::from_secs(60)))?;
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
            .map(|((tx, ..), (rx, ..))| {
                println!("tx {}, rx {} bytes", tx, rx);
            }).map_err(|e| println!("piping error: {}", e));
        handle.spawn(piping);
        Ok(())
    });
    core.run(server).unwrap();
}
