use clap::load_yaml;
use futures::stream::StreamExt;
use ini::Ini;
use log::{debug, error, info, LevelFilter};
use std::{env, io, io::Write, net::SocketAddr, str::FromStr, sync::Arc};
#[cfg(feature = "web_console")]
use tokio::net::unix::UnixListener;
use tokio::{self, net::tcp::TcpListener};
use tokio_net::signal::unix::{Signal, SignalKind};

#[cfg(feature = "web_console")]
use moproxy::web;
use moproxy::{
    client::{Connectable, NewClient},
    monitor::Monitor,
    proxy::{ProxyProto, ProxyServer},
    tcp::set_congestion,
};

trait FromOptionStr<E, T: FromStr<Err = E>> {
    fn parse(&self) -> Result<Option<T>, E>;
}

impl<E, T, S> FromOptionStr<E, T> for Option<S>
where
    T: FromStr<Err = E>,
    S: AsRef<str>,
{
    fn parse(&self) -> Result<Option<T>, E> {
        if let Some(s) = self {
            let t = T::from_str(s.as_ref())?;
            Ok(Some(t))
        } else {
            Ok(None)
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), &'static str> {
    let yaml = load_yaml!("cli.yml");
    let args = clap::App::from_yaml(yaml)
        .version(env!("CARGO_PKG_VERSION"))
        .get_matches();

    let mut logger = env_logger::Builder::new();
    if let Ok(env_log) = env::var("RUST_LOG") {
        logger.parse_filters(&env_log);
    }
    let log_level = args
        .value_of("log-level")
        .unwrap_or("info")
        .parse()
        .map_err(|_| "unknown log level")?;
    logger
        .filter(None, log_level)
        .filter_module("tokio_executor", LevelFilter::Warn)
        .filter_module("tokio_net", LevelFilter::Warn)
        .filter_module("hyper", LevelFilter::Warn)
        .filter_module("mio", LevelFilter::Warn)
        .filter_module("ini", LevelFilter::Warn)
        .target(env_logger::Target::Stdout)
        .format(|buf, r| writeln!(buf, "[{}] {}", r.level(), r.args()))
        .init();

    let host = args
        .value_of("host")
        .ok_or("missing host")?
        .parse()
        .or(Err("invalid address"))?;
    let port = args
        .value_of("port")
        .ok_or("missing port number")?
        .parse()
        .or(Err("invalid port number"))?;
    let bind_addr = SocketAddr::new(host, port);
    let probe = args
        .value_of("probe-secs")
        .ok_or("missing probe secs")?
        .parse()
        .or(Err("not a vaild probe secs"))?;
    let remote_dns = args.is_present("remote-dns");
    let n_parallel = args
        .value_of("n-parallel")
        .parse()
        .or(Err("not a valid number"))?
        .unwrap_or(0 as usize);
    let cong_local = args.value_of("cong-local");
    let allow_direct = args.is_present("allow-direct");
    let graphite = args
        .value_of("graphite")
        .parse()
        .or(Err("not a valid address"))?;
    let servers_cfg = ServerListCfg::new(&args)?;

    let servers = servers_cfg.load()?;
    let monitor = Monitor::new(servers, graphite);

    // Setup web server
    if !cfg!(feature = "web_console") && args.is_present("web-bind") {
        return Err("web console has been disabled during compiling");
    };
    #[cfg(feature = "web_console")]
    let sock_file = {
        if let Some(http_addr) = args.value_of("web-bind") {
            info!("http run on {}", http_addr);
            if http_addr.starts_with('/') {
                let sock = web::AutoRemoveFile::new(&http_addr);
                let incoming = UnixListener::bind(&sock)
                    .or(Err("fail to bind web server"))?
                    .incoming();
                let serv = web::run_server(incoming, monitor.clone());
                tokio::spawn(serv);
                Some(sock)
            } else {
                let addr = http_addr
                    .parse()
                    .or(Err("not a valid address of TCP socket"))?;
                let incoming = TcpListener::bind(&addr)
                    .or(Err("fail to bind web server"))?
                    .incoming();
                let serv = web::run_server(incoming, monitor.clone());
                tokio::spawn(serv);
                None
            }
        } else {
            None
        }
    };

    // Setup monitor
    tokio::spawn(monitor.clone().monitor_delay(probe));

    // Setup signal listener for reloading server list
    let monitor_ = monitor.clone();
    let mut signals = Signal::new(SignalKind::hangup()).or(Err("cannot catch signal"))?;
    tokio::spawn(async move {
        while let Some(_) = signals.next().await {
            debug!("SIGHUP received, reload server list.");
            match servers_cfg.load() {
                Ok(servers) => monitor_.update_servers(servers),
                Err(err) => error!("fail to reload servers: {}", err),
            }
        }
    });

    // Optional direct connect
    let direct_server = if allow_direct {
        Some(Arc::new(ProxyServer::direct()))
    } else {
        None
    };

    // Setup proxy server
    let listener = TcpListener::bind(&bind_addr).or(Err("cannot bind to port"))?;
    info!("listen on {}", bind_addr);
    if let Some(alg) = cong_local {
        info!("set {} on {}", alg, bind_addr);
        set_congestion(&listener, alg).or(Err("fail to set tcp congestion algorithm. \
                                                check tcp_allowed_congestion_control?"))?;
    }
    let mut clients = listener.incoming();
    while let Some(sock) = clients.next().await {
        let client = sock.and_then(|sock| NewClient::from_socket(sock, monitor.servers()));
        let direct = direct_server.clone();
        match client {
            Ok(client) => {
                tokio::spawn(async move {
                    let result = handle_client(client, remote_dns, n_parallel, direct).await;
                    if let Err(e) = result {
                        info!("error on hanle client: {}", e);
                    }
                });
            }
            Err(err) => info!("error on accept client: {}", err),
        }
    }

    // make sure socket file will be deleted on exit.
    // unnecessary drop() but make complier happy about unused var.
    #[cfg(feature = "web_console")]
    drop(sock_file);
    Ok(())
}

async fn handle_client(
    client: NewClient,
    remote_dns: bool,
    n_parallel: usize,
    direct_server: Option<Arc<ProxyServer>>,
) -> io::Result<()> {
    let client = if remote_dns && client.dest.port == 443 {
        client
            .retrive_dest()
            .await?
            .connect_server(n_parallel)
            .await
    } else {
        client.connect_server(0).await
    };
    match client {
        Ok(client) => client.serve().await?,
        Err(client) => if let Some(server) = direct_server {
            client.direct_connect(server).await?.serve().await?
        }
    }
    Ok(())
}

struct ServerListCfg {
    default_test_dns: SocketAddr,
    cli_servers: Vec<Arc<ProxyServer>>,
    path: Option<String>,
}

impl ServerListCfg {
    fn new(args: &clap::ArgMatches) -> Result<Self, &'static str> {
        let default_test_dns = args
            .value_of("test-dns")
            .unwrap()
            .parse()
            .or(Err("not a valid socket address"))?;
        let mut cli_servers = vec![];
        if let Some(s) = args.values_of("socks5-servers") {
            for s in s.map(parse_server) {
                cli_servers.push(Arc::new(ProxyServer::new(
                    s?,
                    ProxyProto::socks5(false),
                    default_test_dns,
                    None,
                    None,
                )));
            }
        }
        if let Some(s) = args.values_of("http-servers") {
            for s in s.map(parse_server) {
                cli_servers.push(Arc::new(ProxyServer::new(
                    s?,
                    ProxyProto::http(false),
                    default_test_dns,
                    None,
                    None,
                )));
            }
        }
        let path = args.value_of("server-list").map(|s| s.to_string());

        Ok(ServerListCfg {
            default_test_dns,
            cli_servers,
            path,
        })
    }

    fn load(&self) -> Result<Vec<Arc<ProxyServer>>, &'static str> {
        let mut servers = self.cli_servers.clone();
        if let Some(path) = &self.path {
            let ini = Ini::load_from_file(path).or(Err("cannot read server list file"))?;
            for (tag, props) in ini.iter() {
                let tag = if let Some(s) = props.get("tag") {
                    Some(s.as_str())
                } else if let Some(ref s) = *tag {
                    Some(s.as_str())
                } else {
                    None
                };
                let addr: SocketAddr = props
                    .get("address")
                    .ok_or("address not specified")?
                    .parse()
                    .or(Err("not a valid socket address"))?;
                let base = props
                    .get("score base")
                    .parse()
                    .or(Err("score base not a integer"))?;
                let test_dns = props
                    .get("test dns")
                    .parse()
                    .or(Err("not a valid socket address"))?
                    .unwrap_or(self.default_test_dns);
                let proto = match props
                    .get("protocol")
                    .ok_or("protocol not specified")?
                    .to_lowercase()
                    .as_str()
                {
                    "socks5" | "socksv5" => {
                        let fake_hs = props
                            .get("socks fake handshaking")
                            .parse()
                            .or(Err("not a boolean value"))?
                            .unwrap_or(false);
                        ProxyProto::socks5(fake_hs)
                    }
                    "http" => {
                        let cwp = props
                            .get("http allow connect payload")
                            .parse()
                            .or(Err("not a boolean value"))?
                            .unwrap_or(false);
                        ProxyProto::http(cwp)
                    }
                    _ => return Err("unknown proxy protocol"),
                };
                let server = ProxyServer::new(addr, proto, test_dns, tag, base);
                servers.push(Arc::new(server));
            }
        }
        if servers.is_empty() {
            return Err("missing server list");
        }
        info!("total {} server(s) loaded", servers.len());
        Ok(servers)
    }
}

fn parse_server(addr: &str) -> Result<SocketAddr, &'static str> {
    if addr.contains(':') {
        addr.parse()
    } else {
        format!("127.0.0.1:{}", addr).parse()
    }
    .or(Err("not a valid server address"))
}
