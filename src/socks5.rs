use std::fmt;
use std::time::Duration;
use std::net::{TcpStream, SocketAddr, IpAddr};
use std::io::{self, Write, Read};
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

        let mut buffer = Vec::with_capacity(13);
        buffer.write(&[5, 1, 0])?;
        buffer.write(&[5, 1, 0])?;
        match addr.ip() {
            IpAddr::V4(ip) => {
                buffer.write(&[0x01])?;
                buffer.write(&ip.octets())?;
            }
            IpAddr::V6(ip) => {
                buffer.write(&[0x04])?;
                buffer.write(&ip.octets())?;
            }
        };
        let port = addr.port();
        buffer.push((port >> 8) as u8);
        buffer.push(port as u8);
        //println!("{:?}", &buffer);
        stream.write_all(&buffer)?;

        let mut buffer = [0; 12];
        stream.read_exact(&mut buffer)?;
        stream.set_read_timeout(None)?;
        stream.set_write_timeout(None)?;
        Ok(stream)
    }
}

