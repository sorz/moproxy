pub mod socks5;
pub mod http;
pub mod copy;
use std::fmt;
use std::cell::Cell;
use std::str::FromStr;
use std::time::Duration;
use std::io;
use std::net::{SocketAddr, IpAddr};
use futures::Future;
use tokio_core::net::TcpStream;
use tokio_core::reactor::Handle;
use ToMillis;

const DEFAULT_MAX_WAIT_MILLILS: u64 = 4_000;

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
    pub tag: Box<str>,
    pub test_dns: SocketAddr,
    pub max_wait: Duration,
    score_base: i32,
    delay: Cell<Option<Duration>>,
    score: Cell<Option<i32>>,
    tx_bytes: Cell<usize>,
    rx_bytes: Cell<usize>,
    conn_alive: Cell<u32>,
    conn_total: Cell<u32>,
}

#[derive(Clone, Debug)]
pub enum Address {
    Ip(IpAddr),
    Domain(Box<str>),
}

#[derive(Clone, Debug)]
pub struct Destination {
    pub host: Address,
    pub port: u16,
}

impl From<SocketAddr> for Destination {
    fn from(addr: SocketAddr) -> Self {
        Destination {
            host: Address::Ip(addr.ip()),
            port: addr.port(),
        }
    }
}

impl<'a> From<(&'a str, u16)> for Destination {
    fn from(addr: (&'a str, u16)) -> Self {
        let host = String::from(addr.0).into_boxed_str();
        Destination {
            host: Address::Domain(host),
            port: addr.1,
        }
    }
}

impl ProxyServer {
    pub fn new(addr: SocketAddr, proto: ProxyProto, test_dns: SocketAddr,
               tag: Option<&str>, score_base: Option<i32>) -> ProxyServer {
        ProxyServer {
            addr, proto, test_dns,
            tag: match tag {
                None => format!("{}", addr.port()),
                Some(s) => String::from(s),
            }.into_boxed_str(),
            max_wait: Duration::from_millis(DEFAULT_MAX_WAIT_MILLILS),
            score_base: score_base.unwrap_or(0),
            delay: Cell::new(None),
            score: Cell::new(None),
            tx_bytes: Cell::new(0),
            rx_bytes: Cell::new(0),
            conn_alive: Cell::new(0),
            conn_total: Cell::new(0),
        }
    }

    pub fn connect(&self, addr: Destination, handle: &Handle)
            -> Box<Connect> {
        let proto = self.proto;
        let conn = TcpStream::connect(&self.addr, handle);
        let handshake = conn.and_then(move |stream| {
            debug!("connected with {:?}", stream.peer_addr());
            if let Err(e) = stream.set_nodelay(true) {
                warn!("fail to set nodelay: {}", e);
            };
            match proto {
                ProxyProto::Socks5 => socks5::handshake(stream, &addr),
                ProxyProto::Http => http::handshake(stream, &addr),
            }
        });
        Box::new(handshake)
    }

    pub fn delay(&self) -> Option<Duration> {
        self.delay.get()
    }

    pub fn score(&self) -> Option<i32> {
        self.score.get()
    }


    pub fn set_delay(&self, delay: Option<Duration>) {
        self.delay.set(delay);
        self.score.set(delay.map(|t| t.millis() as i32 + self.score_base));
    }

    pub fn update_delay(&self, delay: Option<Duration>) {
        self.delay.set(delay);
        let score = delay
            .map(|t| t.millis() as i32 + self.score_base)
            .map(|new| {
                let old = self.score.get()
                    .unwrap_or_else(|| self.max_wait.millis() as i32);
                // give more weight to delays exceed the mean, to
                // punish for network jitter.
                if new < old {
                    (old * 9 + new * 1) / 10
                } else {
                    (old * 8 + new * 2) / 10
                }
            });
        self.score.set(score);
    }

    pub fn update_traffics(&self, tx: usize, rx: usize) {
        self.tx_bytes.set(self.tx_bytes.get() + tx);
        self.rx_bytes.set(self.rx_bytes.get() + rx);
    }

    pub fn update_stats_conn_open(&self) {
        self.conn_alive.set(self.conn_alive.get() + 1);
        self.conn_total.set(self.conn_total.get() + 1);
    }

    pub fn update_stats_conn_close(&self) {
        self.conn_alive.set(self.conn_alive.get() - 1);
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

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Address::Ip(ref ip) => write!(f, "{}", ip),
            Address::Domain(ref s) => write!(f, "{}", s),
        }
    }
}

impl fmt::Display for Destination {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}:{}", self.host, self.port)
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

