extern crate tokio_core;
extern crate tokio_io;
extern crate futures;
use std::fmt;
use std::rc::Rc;
use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr};
use self::futures::{Future, Poll};
use self::tokio_core::net::TcpStream;
use self::tokio_core::reactor::Handle;
use self::tokio_io::io as tio;
use self::tokio_io::{AsyncRead, AsyncWrite};


pub type Connect = Future<Item=TcpStream, Error=io::Error>;
pub trait ProxyServer: Send + Sync + fmt::Display {
    fn tag(&self) -> &str;
    fn connect(&self, addr: SocketAddr, handle: &Handle) -> Box<Connect>;
}


pub fn piping(local: TcpStream, remote: TcpStream)
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
struct HalfTcpStream(Rc<TcpStream>);

impl HalfTcpStream {
    fn new(stream: TcpStream) -> HalfTcpStream {
        HalfTcpStream(Rc::new(stream))
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
