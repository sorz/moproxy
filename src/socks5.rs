extern crate tokio_core;
extern crate tokio_io;
extern crate futures;
use std::fmt;
use std::net::{SocketAddr, IpAddr};
use std::io::Write;
use self::futures::Future;
use self::tokio_core::reactor::Handle;
use self::tokio_core::net as tnet;
use self::tokio_io::io::{read_exact, write_all};
use ::proxy::{ProxyServer, Connect};


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

    fn connect(&self, addr: SocketAddr, handle: &Handle) -> Box<Connect> {
        let conn = tnet::TcpStream::connect(&self.addr, handle);
        Box::new(conn.and_then(move |stream| {
            debug!("connected with {:?}", stream.peer_addr());
            if let Err(e) = stream.set_nodelay(true) {
                warn!("fail to set nodelay: {}", e);
            };
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

