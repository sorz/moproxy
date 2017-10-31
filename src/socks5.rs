extern crate tokio_core;
extern crate tokio_io;
extern crate futures;
use std::fmt;
use std::time::Duration;
use std::net::{TcpStream, SocketAddr, IpAddr};
use std::io::{self, Write, Read};
use self::futures::{future, Future};
use self::tokio_core::reactor::{Handle, Timeout};
use self::tokio_core::net as tnet;
use self::tokio_io::io::{read_exact, write_all};
use ::proxy::ProxyServer;


#[derive(Debug, PartialEq, Eq, Clone, Hash)]
pub struct Socks5Server {
    tag: String,
    addr: SocketAddr,
}

impl Socks5Server {
    pub fn new(addr: SocketAddr) -> Socks5Server {
        let tag = format!("{}", addr.port());
        Socks5Server {
            tag: tag,
            addr: addr,
        }
    }
}

impl fmt::Display for Socks5Server {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{} (SOCKSv5 {})", self.tag, self.addr)
    }
}

impl ProxyServer for Socks5Server {
    fn tag(&self) -> &str {
        &self.tag
    }

    fn connect(&self, addr: SocketAddr) -> io::Result<TcpStream> {
        let mut stream = TcpStream::connect(self.addr)?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(Duration::from_millis(100)))?;
        stream.set_write_timeout(Some(Duration::from_millis(100)))?;

        let request = build_request(&addr);
        stream.write_all(&request)?;

        let mut buffer = [0; 12];
        stream.read_exact(&mut buffer)?;
        stream.set_read_timeout(None)?;
        stream.set_write_timeout(None)?;
        Ok(stream)
    }
}

impl Socks5Server {

    fn connect_async(&self, addr: SocketAddr, handle: Handle)
            -> Box<Future<Item=tnet::TcpStream, Error=io::Error>> {
        let conn = tnet::TcpStream::connect(&self.addr, &handle);
        Box::new(conn.and_then(move |stream| {
            if let Err(e) = stream.set_nodelay(true) {
                warn!("fail to set nodelay: {}", e);
            }
            write_all(stream, build_request(&addr))
        }).and_then(|(stream, _)| {
            read_exact(stream, vec![0; 12])
        }).map(|(stream, _)| stream))
    }
}

fn build_request(addr: &SocketAddr) -> Vec<u8> {
    let mut buffer = Vec::with_capacity(13);
    buffer.write(&[5, 1, 0]).unwrap();
    buffer.write(&[5, 1, 0]).unwrap();
    match addr.ip() {
        IpAddr::V4(ip) => {
            buffer.write(&[0x01]).unwrap();
            buffer.write(&ip.octets()).unwrap();
        },
        IpAddr::V6(ip) => {
            buffer.write(&[0x04]).unwrap();
            buffer.write(&ip.octets()).unwrap();
        }
    };
    let port = addr.port();
    buffer.push((port >> 8) as u8);
    buffer.push(port as u8);
    buffer
}

