use std::{
    net::{IpAddr, Ipv6Addr, SocketAddr},
    path::PathBuf,
    time::Duration,
};

use clap::{arg, command, Parser};
use tracing::metadata::LevelFilter;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub(crate) struct CliArgs {
    /// Address to bind on
    #[arg(short = 'b', long, value_name = "IP-ADDRESS")]
    #[arg(default_value_t = Ipv6Addr::UNSPECIFIED.into())]
    pub(crate) host: IpAddr,

    /// Port number to bind on. Multiple ports may be given (will selected
    /// accroding to the SERVER-LIST ini config).
    #[arg(short = 'p', long, value_name = "PORT", required = true)]
    #[arg(num_args = 1..)]
    pub(crate) port: Vec<u16>,

    /// SOCKSv5 server list. IP address can omit for localhost.
    #[arg(short = 's', long = "socks5", value_name = "SOCKS5-SERVERS")]
    #[arg(value_parser = parse_socket_addr_default_on_localhost)]
    #[arg(num_args = 0..)]
    pub(crate) socks5_servers: Vec<SocketAddr>,

    /// HTTP proxy server list. IP address can omit for localhost.
    #[arg(short = 't', long = "http", value_name = "HTTP-SERVERS")]
    #[arg(value_parser = parse_socket_addr_default_on_localhost)]
    #[arg(num_args = 0..)]
    pub(crate) http_servers: Vec<SocketAddr>,

    /// INI file contains list of proxy servers.
    #[arg(short = 'l', long = "list", value_name = "SERVER-LIST")]
    pub(crate) server_list: Option<PathBuf>,

    #[cfg(feature = "policy")]
    #[arg(short = 'l', long = "list", value_name = "POLICY")]
    pub(crate) policy: Option<PathBuf>,

    /// Period of time to make one probe.
    #[arg(short = 'i', long = "probe", value_name = "SECONDS")]
    #[arg(default_value_t = 30)]
    pub(crate) probe_secs: u64,

    /// Address of a DNS server with TCP support to do delay probing.
    #[arg(long, value_name = "IP-ADDR:PORT")]
    #[arg(default_value = "8.8.8.8:53")]
    pub(crate) test_dns: SocketAddr,

    /// Where the web server that shows statistics bind.
    #[cfg(feature = "web_console")]
    #[arg(long = "stats-bind", value_name = "IP-ADDR:PORT")]
    pub(crate) web_bind: Option<String>,

    /// Try to obtain domain name from TLS SNI, and sent it to remote
    /// proxy server. Only apply for port number 443.
    #[arg(long)]
    pub(crate) remote_dns: bool,

    /// Connect and send application data to N proxies in parallel, use
    /// the first proxy that return valid data. Currently only support
    /// TLS as application layer. Must turn on --remote-dns otherwise it
    /// will be ignored.
    #[arg(long, value_name = "N")]
    #[arg(default_value_t = 0)]
    pub(crate) n_parallel: usize,

    /// Set TCP congestion control algorithm on local (client) side.
    #[cfg(target_os = "linux")]
    #[arg(long = "congestion-local", value_name = "ALG-NAME")]
    pub(crate) cong_local: Option<String>,

    /// Fallback to direct connect (without proxy) if all proxies failed.
    #[arg(long)]
    pub(crate) allow_direct: bool,

    /// Send metrics to graphite (carbon) daemon in plaintext format with
    /// TCP.
    #[arg(long, value_name = "IP-ADDR:PORT")]
    pub(crate) graphite: Option<SocketAddr>,

    /// Level of verbosity [possible values: off, error, warn, info, debug,
    /// trace]
    #[arg(long)]
    #[arg(default_value = "info")]
    pub(crate) log_level: LevelFilter,

    /// Lua script that customize proxy score
    #[cfg(feature = "score_script")]
    #[arg(long, value_name = "LUA-SCRIPT")]
    pub(crate) score_script: Option<PathBuf>,

    /// Max waiting time in seconds for connection establishment before
    /// timeout. Applied for both probe & normal proxy connections.
    #[arg(long, value_name = "SECONDS")]
    #[arg(value_parser = parse_duration_in_seconds)]
    #[arg(default_value = "4")]
    pub(crate) max_wait: Duration,
}

fn parse_duration_in_seconds(s: &str) -> Result<Duration, String> {
    s.parse()
        .map_err(|_| format!("`{}` isn't a number", s))
        .map(Duration::from_secs)
}

fn parse_socket_addr_default_on_localhost(addr: &str) -> Result<SocketAddr, String> {
    if addr.contains(':') {
        addr.parse()
    } else {
        format!("127.0.0.1:{}", addr).parse()
    }
    .map_err(|_| format!("`{}` isn't a valid server address", addr))
}

#[test]
fn verify_cli() {
    use clap::CommandFactory;
    CliArgs::command().debug_assert()
}
