pub mod socks5;
pub mod http;
use std::fmt;
use std::rc::Rc;
use std::str::FromStr;
use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr};
use ::futures::{Future, Poll};
use ::tokio_core::net::TcpStream;
use ::tokio_core::reactor::Handle;
use ::tokio_io::io as tio;
use ::tokio_io::{AsyncRead, AsyncWrite};

pub type Connect = Future<Item=TcpStream, Error=io::Error>;

#[derive(Copy, Clone, Debug, Serialize)]
pub enum ProxyProto {
    #[serde(rename = "SOCKSv5")]
    Socks5,
    #[serde(rename = "HTTP")]
    Http,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProxyServer {
    pub addr: SocketAddr,
    pub proto: ProxyProto,
    pub tag: String,
    pub score_base: i32,
}

impl ProxyServer {
    pub fn new(addr: SocketAddr, proto: ProxyProto, tag: Option<&str>,
               score_base: Option<i32>) -> ProxyServer {
        ProxyServer {
            addr: addr,
            proto: proto,
            tag: match tag {
                None => format!("{}", addr.port()),
                Some(s) => String::from(s),
            },
            score_base: score_base.unwrap_or(0),
        }
    }

    pub fn connect(&self, addr: SocketAddr, handle: &Handle) -> Box<Connect> {
        let proto = self.proto;
        let conn = TcpStream::connect(&self.addr, handle);
        let handshake = conn.and_then(move |stream| {
            debug!("connected with {:?}", stream.peer_addr());
            if let Err(e) = stream.set_nodelay(true) {
                warn!("fail to set nodelay: {}", e);
            };
            match proto {
                ProxyProto::Socks5 => socks5::handshake(stream, addr),
                ProxyProto::Http => http::handshake(stream, addr),
            }
        });
        Box::new(handshake)
    }
}

impl fmt::Display for ProxyServer {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{} ({} {})", self.tag, self.proto, self.addr)
    }
}

impl fmt::Display for ProxyProto {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            ProxyProto::Socks5 => write!(f, "SOCKSv5"),
            ProxyProto::Http => write!(f, "HTTP"),
        }
    }
}

impl FromStr for ProxyProto {
    type Err = ();
    fn from_str(s: &str) -> Result<ProxyProto, ()> {
        match s.to_lowercase().as_str() {
            "socks5" => Ok(ProxyProto::Socks5),
            "socksv5" => Ok(ProxyProto::Socks5),
            "http" => Ok(ProxyProto::Http),
            _ => Err(()),
        }
    }
}

pub fn piping(local: TcpStream, remote: TcpStream)
        -> Box<Future<Item=(u64, u64), Error=io::Error>> {
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
    Box::new(to_remote.join(to_local))
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

