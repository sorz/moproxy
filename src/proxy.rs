extern crate tokio_core;
extern crate tokio_io;
extern crate futures;
use std::fmt;
use std::io::{self, Read, Write};
use std::net::{TcpStream, Shutdown, SocketAddr};
use std::sync::Arc;
use self::futures::{Stream,  Future, Poll};
use self::futures::sync::mpsc;
use self::tokio_core::net as tnet;
use self::tokio_core::reactor::{Core, Handle};
use self::tokio_io::io as tio;
use self::tokio_io::{AsyncRead, AsyncWrite};


pub trait ProxyServer: Send + Sync + fmt::Display {
    fn tag(&self) -> &str;
    fn connect(&self, addr: SocketAddr) -> io::Result<TcpStream>;
}

pub fn piping_worker(rx: mpsc::Receiver<(TcpStream, TcpStream)>) {
    let mut core = Core::new().unwrap();
    let handle = core.handle();
    let server = rx.for_each(|(local, remote)| {
        let local = match tnet::TcpStream::from_stream(local, &handle) {
            Ok(s) => s,
            Err(_) => {
                warn!("cannot create asycn tcp stream");
                return Ok(());
            },
        };
        let remote = match tnet::TcpStream::from_stream(remote, &handle) {
            Ok(s) => s,
            Err(_) => {
                warn!("cannot create asycn tcp stream");
                return Ok(());
            },
        };
        handle.spawn(piping(local, remote));
        Ok(())
    });
    core.run(server).unwrap();
}

pub fn piping(local: tnet::TcpStream, remote: tnet::TcpStream)
        -> Box<Future<Item=(), Error=()>> {
    let local_r = HalfTcpStream::new(local);
    let remote_r = HalfTcpStream::new(remote);
    let local_w = local_r.clone();
    let remote_w = remote_r.clone();

    let to_remote = tio::copy(local_r, remote_w)
        .and_then(|(n, _, remote_w)| {
            tio::shutdown(remote_w).map(move |_| n)
        });
    let to_local = tio::copy(remote_r, local_w)
        .and_then(|(n, _, local_w)| {
            tio::shutdown(local_w).map(move |_| n)
        });

    let piping = to_remote.join(to_local)
        .map(|(tx, rx)| debug!("tx {}, rx {} bytes", tx, rx))
        .map_err(|e| warn!("piping error: {}", e));
    Box::new(piping)
}

// The default `AsyncWrite::shutdown` for TcpStream does nothing.
// Here overrite it to shutdown write half of TCP connection.
// Modified on:
// https://github.com/tokio-rs/tokio-core/blob/master/examples/proxy.rs
#[derive(Clone)]
struct HalfTcpStream(Arc<tnet::TcpStream>);

impl HalfTcpStream {
    fn new(stream: tnet::TcpStream) -> HalfTcpStream {
        HalfTcpStream(Arc::new(stream))
    }
}

impl Read for HalfTcpStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        (&*self.0).read(buf)
    }
}

impl Write for HalfTcpStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        (&*self.0).write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl AsyncRead for HalfTcpStream {}

impl AsyncWrite for HalfTcpStream {
    fn shutdown(&mut self) -> Poll<(), io::Error> {
        self.0.shutdown(Shutdown::Write)?;
        Ok(().into())
    }
}
