pub mod socks5;
pub mod http;
pub mod copy;
use std::io;
use std::fmt;
use std::ops::{Add, AddAssign};
use std::sync::{Mutex, MutexGuard};
use std::str::FromStr;
use std::time::Duration;
use std::net::{SocketAddr, IpAddr};
use std::hash::{Hash, Hasher};
use futures::{Future, future::Either};
use tokio_core::net::TcpStream;
use tokio_core::reactor::Handle;
use ToMillis;

const DEFAULT_MAX_WAIT_MILLILS: u64 = 4_000;

pub type Connect = Future<Item=TcpStream, Error=io::Error>;

#[derive(Hash, Eq, PartialEq, Copy, Clone, Debug, Serialize)]
pub enum ProxyProto {
    #[serde(rename = "SOCKSv5")]
    Socks5,
    #[serde(rename = "HTTP")]
    Http {
        /// Allow to send app-level data as payload on CONNECT request.
        /// This can eliminate 1 round-trip delay but may cause problem on
        /// some servers.
        ///
        /// RFC 7231:
        /// >> A payload within a CONNECT request message has no defined
        /// >> semantics; sending a payload body on a CONNECT request might
        /// cause some existing implementations to reject the request.
        connect_with_payload: bool,
    },
}

#[derive(Debug, Serialize)]
pub struct ProxyServer {
    pub addr: SocketAddr,
    pub proto: ProxyProto,
    pub tag: Box<str>,
    pub test_dns: SocketAddr,
    pub max_wait: Duration,
    score_base: i32,
    status: Mutex<ProxyServerStatus>,
}

#[derive(Debug, Serialize, Clone, Copy, Default)]
struct ProxyServerStatus {
    delay: Option<Duration>,
    score: Option<i32>,
    traffic: Traffic,
    conn_alive: u32,
    conn_total: u32,
}

impl Hash for ProxyServer {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.addr.hash(state);
        self.proto.hash(state);
        self.tag.hash(state);
    }
}

impl PartialEq for ProxyServer {
    fn eq(&self, other: &ProxyServer) -> bool {
        self.addr == other.addr &&
        self.proto == other.proto &&
        self.tag == other.tag
    }
}

impl Eq for ProxyServer {}

#[derive(Hash, Clone, Debug)]
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

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct Traffic {
    pub tx_bytes: usize,
    pub rx_bytes: usize,
}

impl Into<Traffic> for (usize, usize) {
    fn into(self) -> Traffic {
        Traffic { tx_bytes: self.0, rx_bytes: self.1 }
    }
}

impl Add for Traffic {
    type Output = Traffic;

    fn add(self, other: Traffic) -> Traffic {
        Traffic {
            tx_bytes: self.tx_bytes + other.tx_bytes,
            rx_bytes: self.rx_bytes + other.rx_bytes,
        }
    }
}

impl AddAssign for Traffic {
    fn add_assign(&mut self, other: Traffic) {
        *self = *self + other;
    }
}

impl ProxyProto {
    pub fn socks5() -> Self {
        ProxyProto::Socks5
    }

    pub fn http(connect_with_payload: bool) -> Self {
        ProxyProto::Http { connect_with_payload }
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
            status: Default::default(),
        }
    }

    pub fn connect<T>(&self, addr: Destination, data: Option<T>,
                   handle: &Handle) -> Box<Connect>
    where T: AsRef<[u8]> + 'static {
        let proto = self.proto;
        let conn = TcpStream::connect(&self.addr, handle);
        let handshake = conn.and_then(move |stream| {
            debug!("connected with {:?}", stream.peer_addr());
            if let Err(e) = stream.set_nodelay(true) {
                warn!("fail to set nodelay: {}", e);
            };
            match proto {
                ProxyProto::Socks5 =>
                    Either::A(socks5::handshake(stream, &addr, data)),
                ProxyProto::Http { connect_with_payload } =>
                    Either::B(http::handshake(stream, &addr, data,
                                    connect_with_payload)),
            }
        });
        Box::new(handshake)
    }

    fn status(&self) -> MutexGuard<ProxyServerStatus> {
        self.status.lock().unwrap()
    }

    pub fn delay(&self) -> Option<Duration> {
        self.status().delay
    }

    pub fn score(&self) -> Option<i32> {
        self.status().score
    }

    pub fn set_delay(&self, delay: Option<Duration>) {
        self.status().delay = delay;
        self.status().score =
            delay.map(|t| t.millis() as i32 + self.score_base);
    }

    pub fn update_delay(&self, delay: Option<Duration>) {
        let mut status = self.status();
        status.delay = delay;
        let score = delay
            .map(|t| t.millis() as i32 + self.score_base)
            .map(|new| {
                let old = status.score
                    .unwrap_or_else(|| self.max_wait.millis() as i32);
                // give more weight to delays exceed the mean, to
                // punish for network jitter.
                if new < old {
                    (old * 9 + new * 1) / 10
                } else {
                    (old * 8 + new * 2) / 10
                }
            });
        status.score = score;
    }

    pub fn add_traffic(&self, traffic: Traffic) {
        self.status().traffic += traffic;
    }

    pub fn update_stats_conn_open(&self) {
        self.status().conn_alive += 1;
        self.status().conn_total += 1;
    }

    pub fn update_stats_conn_close(&self) {
        self.status().conn_alive -= 1;
    }

    pub fn traffic(&self) -> Traffic {
        self.status().traffic
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
            ProxyProto::Http { .. } => write!(f, "HTTP"),
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
            "socks5" => Ok(ProxyProto::socks5()),
            "socksv5" => Ok(ProxyProto::socks5()),
            // default to disable connect with payload
            "http" => Ok(ProxyProto::http(false)),
            _ => Err(()),
        }
    }
}

