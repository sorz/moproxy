use std::fmt;
use std::time::Duration;
use std::net::{TcpStream, SocketAddr};
use std::io::{self, Write, BufReader, BufRead, ErrorKind};
use ::proxy::ProxyServer;


#[derive(Debug, PartialEq, Eq, Clone, Hash)]
pub struct HttpProxyServer {
    tag: String,
    addr: SocketAddr,
}

impl HttpProxyServer {
    pub fn new(addr: SocketAddr) -> HttpProxyServer {
        let tag = format!("{}", addr.port());
        HttpProxyServer {
            tag: tag,
            addr: addr,
        }
    }
}

impl fmt::Display for HttpProxyServer {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{} (HTTP {})", self.tag, self.addr)
    }
}

impl ProxyServer for HttpProxyServer {
    fn tag(&self) -> &str {
        &self.tag
    }

    fn connect(&self, addr: SocketAddr) -> io::Result<TcpStream> {
        let mut stream = TcpStream::connect(self.addr)?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(Duration::from_secs(1)))?;
        debug!("creating proxy tunnel to {} via {}", addr, self.tag());

        let host = match addr {
            SocketAddr::V4(s) => format!("{}:{}", s.ip(), s.port()),
            SocketAddr::V6(s) => format!("[{}]:{}", s.ip(), s.port()),
        };
        let request = format!(
            "CONNECT {host} HTTP/1.1\r\nHost: {host}\r\n\r\n",
            host=host
        );
        stream.write_all(request.as_bytes())?;

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line)?;
        if !line.starts_with("HTTP/1.1 2") {
            info!("{} return {}", self.tag(), line.trim());
            let err = format!("proxy server return error: {}", line.trim());
            return Err(io::Error::new(ErrorKind::Other, err));
        }
        loop {
            line.clear();
            reader.read_line(&mut line)?;
            if line == "\r\n" || line == "\n" {
                break;
            }
        }
        // TODO: may lost data in buffer
        let stream = reader.into_inner();

        debug!("proxy tunnel connected");
        stream.set_read_timeout(None)?;
        stream.set_write_timeout(None)?;
        Ok(stream)
    }
}

