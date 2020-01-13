pub mod copy;
pub mod http;
#[cfg(feature = "score_script")]
use rlua::prelude::*;
pub mod socks5;
use log::debug;
use parking_lot::{Mutex, RwLock};
use serde_derive::Serialize;
use std::{
    cmp, fmt,
    hash::{Hash, Hasher},
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    ops::{Add, AddAssign},
    str::FromStr,
    time::Duration,
};
use tokio::net::TcpStream;

const DEFAULT_MAX_WAIT_MILLILS: u64 = 4_000;
const GRAPHITE_PATH_PREFIX: &str = "moproxy.proxy_servers";

#[derive(Hash, Eq, PartialEq, Copy, Clone, Debug, Serialize)]
pub enum ProxyProto {
    #[serde(rename = "SOCKSv5")]
    Socks5 {
        /// Not actually do the SOCKSv5 handshaking but sending all bytes as
        /// per protocol specified in once followed by the actual request.
        /// This saves [TODO] round-trip delay but may cause problem on some
        /// servers.
        fake_handshaking: bool,
    },
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
    Direct,
}

#[derive(Debug, Serialize)]
pub struct ProxyServer {
    pub addr: SocketAddr,
    pub proto: ProxyProto,
    pub tag: Box<str>,
    config: RwLock<ProxyServerConfig>,
    status: Mutex<ProxyServerStatus>,
}

#[derive(Debug, Serialize, Clone, Copy)]
pub struct ProxyServerConfig {
    pub test_dns: SocketAddr,
    pub max_wait: Duration,
    score_base: i32,
}

#[cfg(feature = "score_script")]
impl ToLua<'_> for ProxyServerConfig {
    fn to_lua(self, ctx: LuaContext<'_>) -> LuaResult<LuaValue<'_>> {
        let table = ctx.create_table()?;
        table.set("test_dns", self.test_dns.to_string())?;
        table.set("max_wait", self.max_wait.as_secs_f32())?;
        table.set("score_base", self.score_base)?;
        table.to_lua(ctx)
    }
}

#[derive(Debug, Serialize, Clone, Copy)]
pub enum Delay {
    Unknown,
    Some(Duration),
    TimedOut,
}

impl Default for Delay {
    fn default() -> Self {
        Delay::Unknown
    }
}

impl Delay {
    pub fn map<T, F>(self, func: F) -> Option<T>
    where
        F: FnOnce(Duration) -> T,
    {
        if let Delay::Some(d) = self {
            Some(func(d))
        } else {
            None
        }
    }
}

impl From<Option<Duration>> for Delay {
    fn from(d: Option<Duration>) -> Self {
        d.map(|d| Self::Some(d)).unwrap_or(Self::TimedOut)
    }
}

#[cfg(feature = "score_script")]
impl ToLua<'_> for Delay {
    fn to_lua(self, ctx: LuaContext<'_>) -> LuaResult<LuaValue<'_>> {
        match self {
            Delay::Some(d) => Some(d.as_secs_f32()),
            Delay::TimedOut => Some(-1f32),
            Delay::Unknown => None,
        }
        .to_lua(ctx)
    }
}

#[derive(Debug, Serialize, Clone, Copy, Default)]
pub struct ProxyServerStatus {
    pub delay: Delay,
    pub score: Option<i32>,
    pub traffic: Traffic,
    pub conn_alive: u32,
    pub conn_total: u32,
    pub conn_error: u32,
    pub close_history: u64,
}

#[cfg(feature = "score_script")]
impl ToLua<'_> for ProxyServerStatus {
    fn to_lua(self, ctx: LuaContext<'_>) -> LuaResult<LuaValue<'_>> {
        let status = ctx.create_table()?;
        status.set("delay", self.delay)?;
        status.set("score", self.score)?;
        status.set("score", self.traffic)?;
        status.set("conn_alive", self.conn_alive)?;
        status.set("conn_total", self.conn_total)?;
        status.set("conn_error", self.conn_error)?;
        status.set("close_history", self.close_history)?;
        status.to_lua(ctx)
    }
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
        self.addr == other.addr && self.proto == other.proto && self.tag == other.tag
    }
}

impl Eq for ProxyServer {}

#[cfg(feature = "score_script")]
impl ToLua<'_> for &ProxyServer {
    fn to_lua(self, ctx: LuaContext<'_>) -> LuaResult<LuaValue<'_>> {
        let table = ctx.create_table()?;
        table.set("addr", self.addr.to_string())?;
        table.set("proto", self.proto.to_string())?;
        table.set("tag", self.tag.to_string())?;
        table.set("config", *self.config.read())?;
        table.set("status", *self.status.lock())?;
        table.to_lua(ctx)
    }
}

#[derive(Hash, Clone, Debug)]
pub enum Address {
    Ip(IpAddr),
    Domain(Box<str>),
}

impl From<[u8; 4]> for Address {
    fn from(buf: [u8; 4]) -> Self {
        Address::Ip(IpAddr::V4(Ipv4Addr::from(buf)))
    }
}

impl From<[u8; 16]> for Address {
    fn from(buf: [u8; 16]) -> Self {
        Address::Ip(IpAddr::V6(Ipv6Addr::from(buf)))
    }
}

impl From<String> for Address {
    fn from(s: String) -> Self {
        Address::Domain(s.into_boxed_str())
    }
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

impl From<(Address, u16)> for Destination {
    fn from(addr_port: (Address, u16)) -> Self {
        Destination {
            host: addr_port.0,
            port: addr_port.1,
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
        Traffic {
            tx_bytes: self.0,
            rx_bytes: self.1,
        }
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

#[cfg(feature = "score_script")]
impl ToLua<'_> for Traffic {
    fn to_lua(self, ctx: LuaContext<'_>) -> LuaResult<LuaValue<'_>> {
        let table = ctx.create_table()?;
        table.set("tx_bytes", self.tx_bytes)?;
        table.set("rx_bytes", self.rx_bytes)?;
        table.to_lua(ctx)
    }
}

impl ProxyProto {
    pub fn socks5(fake_handshaking: bool) -> Self {
        ProxyProto::Socks5 { fake_handshaking }
    }

    pub fn http(connect_with_payload: bool) -> Self {
        ProxyProto::Http {
            connect_with_payload,
        }
    }
}

impl ProxyServerConfig {
    fn new(test_dns: SocketAddr, score_base: Option<i32>) -> Self {
        Self {
            test_dns,
            max_wait: Duration::from_millis(DEFAULT_MAX_WAIT_MILLILS),
            score_base: score_base.unwrap_or(0),
        }
    }
}

impl ProxyServer {
    pub fn new(
        addr: SocketAddr,
        proto: ProxyProto,
        test_dns: SocketAddr,
        tag: Option<&str>,
        score_base: Option<i32>,
    ) -> ProxyServer {
        ProxyServer {
            addr,
            proto,
            tag: match tag {
                None => format!("{}", addr.port()),
                Some(s) => {
                    if !s.is_ascii() || s.contains(' ') || s.contains('\n') {
                        panic!(
                            "Tag \"{}\" contains white spaces, line \
                             breaks, or non-ASCII characters.",
                            s
                        );
                    }
                    String::from(s)
                }
            }
            .into_boxed_str(),
            config: ProxyServerConfig::new(test_dns, score_base).into(),
            status: Default::default(),
        }
    }

    pub fn direct() -> Self {
        let stub_addr = "0.0.0.0:0".parse().unwrap();
        Self {
            addr: stub_addr,
            proto: ProxyProto::Direct,
            tag: "DIRECT".into(),
            config: ProxyServerConfig::new(stub_addr, None).into(),
            status: Default::default(),
        }
    }

    pub fn copy_config_from(&self, from: &Self) {
        *self.config.write() = *from.config.read();
    }

    pub async fn connect<T>(&self, addr: &Destination, data: Option<T>) -> io::Result<TcpStream>
    where
        T: AsRef<[u8]> + 'static,
    {
        let proto = self.proto;
        let mut stream = TcpStream::connect(&self.addr).await?;
        debug!("connected with {:?}", stream.peer_addr());
        stream.set_nodelay(true)?;

        match proto {
            ProxyProto::Direct => unimplemented!(),
            ProxyProto::Socks5 { fake_handshaking } => {
                socks5::handshake(&mut stream, &addr, data, fake_handshaking).await?
            }
            ProxyProto::Http {
                connect_with_payload,
            } => http::handshake(&mut stream, &addr, data, connect_with_payload).await?,
        }
        Ok(stream)
    }

    pub fn status_snapshot(&self) -> ProxyServerStatus {
        *self.status.lock()
    }

    pub fn config_snapshot(&self) -> ProxyServerConfig {
        *self.config.read()
    }

    pub fn score(&self) -> Option<i32> {
        self.status.lock().score
    }

    pub fn traffic(&self) -> Traffic {
        self.status.lock().traffic
    }

    pub fn update_delay(&self, delay: Option<Duration>) {
        let mut status = self.status.lock();
        let config = self.config_snapshot();

        if let Some(delay) = delay {
            let last_score = status.score.unwrap_or_else(|| {
                match status.delay {
                    Delay::Some(d) => d,
                    Delay::Unknown => delay,
                    Delay::TimedOut => config.max_wait,
                }
                .as_millis() as i32
                    + config.score_base
            });
            let err_rate = status
                .recent_error_rate(16)
                .min(status.recent_error_rate(64));

            let score = delay.as_millis() as i32 + config.score_base;
            // give penalty for continuous errors
            let score = score + (score as f32 * err_rate * 10f32).round() as i32;
            // moving average on score
            // give more weight to delays exceed the mean for network jitter penalty
            let score = if score < last_score {
                (last_score * 9 + score) / 10
            } else {
                (last_score * 8 + score * 2) / 10
            };
            status.score = Some(score);
            status.delay = Delay::Some(delay);

            // Shift error history
            // This give the server with high error penalty a chance to recovery.
            status.close_history <<= 1;
        } else {
            // Timed out
            status.delay = Delay::TimedOut;
            status.score = None;
        };
    }

    #[cfg(feature = "score_script")]
    pub fn update_delay_with_lua(&self, delay: Option<Duration>, ctx: LuaContext) -> LuaResult<()> {
        let func: LuaFunction = ctx.globals().get("calc_score")?;
        let delay_secs = delay.map(|t| t.as_secs_f32());
        let score: Option<i32> = func.call((self, delay_secs))?;

        let mut status = self.status.lock();
        status.score = score;
        status.delay = delay.into();
        Ok(())
    }

    pub fn add_traffic(&self, traffic: Traffic) {
        self.status.lock().traffic += traffic;
    }

    pub fn update_stats_conn_open(&self) {
        let mut status = self.status.lock();
        status.conn_alive += 1;
        status.conn_total += 1;
    }

    pub fn update_stats_conn_close(&self, has_error: bool) {
        let mut status = self.status.lock();
        status.conn_alive -= 1;
        status.close_history <<= 1;
        if has_error {
            status.conn_error += 1;
            status.close_history += 1;
        }
    }

    pub fn graphite_path(&self, suffix: &str) -> String {
        format!(
            "{}.{}.{}",
            GRAPHITE_PATH_PREFIX,
            self.tag.replace('.', "_"),
            suffix
        )
    }
}

impl ProxyServerStatus {
    pub fn recent_error_count(&self, n: u8) -> u8 {
        let n = 64 - cmp::min(n, 64);
        (self.close_history << n).count_ones() as u8
    }

    pub fn recent_error_rate(&self, n: u8) -> f32 {
        self.recent_error_count(n) as f32 / (cmp::min(n, 64) as f32)
    }
}

impl fmt::Display for ProxyServer {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.proto == ProxyProto::Direct {
            f.write_str("DIRECT")
        } else {
            write!(f, "{} ({} {})", self.tag, self.proto, self.addr)
        }
    }
}

impl fmt::Display for ProxyProto {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            ProxyProto::Socks5 { .. } => write!(f, "SOCKSv5"),
            ProxyProto::Http { .. } => write!(f, "HTTP"),
            ProxyProto::Direct { .. } => write!(f, "DIRECT"),
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
            // default to disable fake handshaking
            "socks5" | "socksv5" => Ok(ProxyProto::socks5(false)),
            // default to disable connect with payload
            "http" => Ok(ProxyProto::http(false)),
            _ => Err(()),
        }
    }
}
