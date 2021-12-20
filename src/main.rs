use clap::{load_yaml, AppSettings};
use futures_util::{stream, StreamExt};
use ini::Ini;
use std::{
    collections::HashSet, env, io, net::SocketAddr, str::FromStr, sync::Arc, time::Duration,
};
#[cfg(all(feature = "web_console", unix))]
use tokio::net::UnixListener;
#[cfg(unix)]
use tokio::signal::unix::{signal, SignalKind};
use tokio::{
    self,
    net::{TcpListener, TcpStream},
};
use tracing::{debug, error, info, instrument, warn, Level};

#[cfg(all(unix, feature = "web_console"))]
use moproxy::futures_stream::UnixListenerStream;
#[cfg(all(feature = "systemd", target_os = "linux"))]
use moproxy::linux::systemd;
#[cfg(target_os = "linux")]
use moproxy::linux::tcp::set_congestion;
#[cfg(feature = "web_console")]
use moproxy::web;
use moproxy::{
    client::{Connectable, NewClient},
    futures_stream::TcpListenerStream,
    monitor::{Monitor, ServerList},
    proxy::{ProxyProto, ProxyServer, UserPassAuthCredential},
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
async fn main() {
    let yaml = load_yaml!("cli.yml");
    let args = clap::App::from_yaml(yaml)
        .version(env!("CARGO_PKG_VERSION"))
        .setting(AppSettings::ColoredHelp)
        .setting(AppSettings::UnifiedHelpMessage)
        .get_matches();

    let log_level: Level = args
        .value_of("log-level")
        .unwrap_or("info")
        .parse()
        .expect("unknown log level");
    tracing_subscriber::fmt()
        .without_time()
        .with_max_level(log_level)
        .init();

    let host = args
        .value_of("host")
        .expect("missing host")
        .parse()
        .expect("invalid address");
    let ports: HashSet<u16> = args
        .values_of("port")
        .expect("missing port number")
        .map(|p| p.parse().expect("invalid port number"))
        .collect();
    let probe = args
        .value_of("probe-secs")
        .expect("missing probe secs")
        .parse()
        .expect("not a vaild probe secs");
    let remote_dns = args.is_present("remote-dns");
    let n_parallel = args
        .value_of("n-parallel")
        .parse()
        .expect("not a valid number")
        .unwrap_or(0_usize);
    let cong_local = args.value_of("cong-local");
    let allow_direct = args.is_present("allow-direct");
    let graphite = args
        .value_of("graphite")
        .parse()
        .expect("not a valid address");
    let servers_cfg = ServerListCfg::new(&args);
    let servers = servers_cfg.load().expect("fail to load servers from file");

    #[cfg(feature = "score_script")]
    let mut monitor = Monitor::new(servers, graphite);
    #[cfg(not(feature = "score_script"))]
    let monitor = Monitor::new(servers, graphite);

    // Setup score script
    if !cfg!(feature = "score_script") && args.is_present("score-script") {
        panic!("score script has been disabled during compiling");
    };
    #[cfg(feature = "score_script")]
    {
        if let Some(path) = args.value_of("score-script") {
            monitor
                .load_score_script(path)
                .expect("fail to load Lua script");
        }
    }

    // Setup web server
    if !cfg!(feature = "web_console") && args.is_present("web-bind") {
        panic!("web console has been disabled during compiling");
    };
    #[cfg(all(feature = "web_console", unix))]
    let mut sock_file = None;
    #[cfg(feature = "web_console")]
    {
        if let Some(http_addr) = args.value_of("web-bind") {
            info!("http run on {}", http_addr);
            #[cfg(unix)]
            {
                if http_addr.starts_with('/') {
                    let sock = web::AutoRemoveFile::new(http_addr);
                    let listener = UnixListener::bind(&sock).expect("fail to bind web server");
                    let serv = web::run_server(UnixListenerStream(listener), monitor.clone());
                    tokio::spawn(serv);
                    sock_file = Some(sock);
                }
            }
            if !http_addr.starts_with('/') || cfg!(not(unix)) {
                let listener = TcpListener::bind(&http_addr)
                    .await
                    .expect("fail to bind web server");
                let serv = web::run_server(TcpListenerStream(listener), monitor.clone());
                tokio::spawn(serv);
            }
        }
    }

    // Setup monitor
    if probe > 0 {
        tokio::spawn(monitor.clone().monitor_delay(probe));
    }

    // Setup signal listener for reloading server list
    #[cfg(unix)]
    let monitor_ = monitor.clone();
    #[cfg(unix)]
    let mut signals = signal(SignalKind::hangup()).expect("cannot catch signal");
    #[cfg(unix)]
    tokio::spawn(async move {
        while signals.recv().await.is_some() {
            reload_daemon(&servers_cfg, &monitor_);
        }
    });

    // Optional direct connect
    let direct_server = if allow_direct {
        Some(Arc::new(ProxyServer::direct(parse_max_wait(&args))))
    } else {
        None
    };

    // Setup proxy server
    let mut listeners = Vec::with_capacity(ports.len());
    if cong_local.is_some() && cfg!(not(target_os = "linux")) {
        panic!("--cong-local can only be used on Linux");
    }
    for port in ports {
        let addr = SocketAddr::new(host, port);
        let listener = TcpListener::bind(&addr).await.expect("cannot bind to port");
        info!("listen on {}", addr);
        if let Some(alg) = cong_local {
            info!("set {} on {}", alg, addr);
            #[cfg(target_os = "linux")]
            set_congestion(&listener, alg).expect(
                "fail to set tcp congestion algorithm. \
                check tcp_allowed_congestion_control?",
            );
        }
        listeners.push(TcpListenerStream(listener));
    }

    // Watchdog
    #[cfg(all(feature = "systemd", target_os = "linux"))]
    {
        if let Some(timeout) = systemd::watchdog_timeout() {
            tokio::spawn(systemd::watchdog_loop(timeout / 2));
        }
    }

    // Notify systemd
    #[cfg(all(feature = "systemd", target_os = "linux"))]
    systemd::notify_ready();

    // The proxy server
    let mut clients = stream::select_all(listeners.iter_mut());
    while let Some(sock) = clients.next().await {
        let direct = direct_server.clone();
        let servers = monitor.servers();
        match sock {
            Ok(sock) => {
                tokio::spawn(async move {
                    let result = handle_client(sock, servers, remote_dns, n_parallel, direct).await;
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
    #[cfg(all(feature = "web_console", unix))]
    drop(sock_file);
}

#[instrument(level = "debug", skip_all)]
fn reload_daemon(servers_cfg: &ServerListCfg, monitor: &Monitor) {
    #[cfg(all(feature = "systemd", target_os = "linux"))]
    systemd::notify_realoding();

    // feature: check deadlocks on signal
    let deadlocks = parking_lot::deadlock::check_deadlock();
    if !deadlocks.is_empty() {
        error!("{} deadlocks detected!", deadlocks.len());
        for (i, threads) in deadlocks.iter().enumerate() {
            debug!("Deadlock #{}", i);
            for t in threads {
                debug!("Thread Id {:#?}", t.thread_id());
                debug!("{:#?}", t.backtrace());
            }
        }
    }

    // actual reload
    debug!("SIGHUP received, reload server list.");
    match servers_cfg.load() {
        Ok(servers) => monitor.update_servers(servers),
        Err(err) => error!("fail to reload servers: {}", err),
    }
    // TODO: reload lua script?

    #[cfg(all(feature = "systemd", target_os = "linux"))]
    systemd::notify_ready();
}

#[instrument(level = "debug", skip_all, fields(on_port=sock.local_addr()?.port(), peer=?sock.peer_addr()?))]
async fn handle_client(
    sock: TcpStream,
    servers: ServerList,
    remote_dns: bool,
    n_parallel: usize,
    direct_server: Option<Arc<ProxyServer>>,
) -> io::Result<()> {
    let client = NewClient::from_socket(sock, servers).await?;
    let client = if remote_dns && client.dest.port == 443 {
        client
            .retrieve_dest_from_sni()
            .await?
            .connect_server(n_parallel)
            .await
    } else {
        client.connect_server(0).await
    };
    match client {
        Ok(client) => client.serve().await?,
        Err(client) => {
            if let Some(server) = direct_server {
                client.direct_connect(server).await?.serve().await?
            }
        }
    }
    Ok(())
}

struct ServerListCfg {
    default_test_dns: SocketAddr,
    default_max_wait: Duration,
    cli_servers: Vec<Arc<ProxyServer>>,
    path: Option<String>,
    listen_ports: HashSet<u16>,
}

impl ServerListCfg {
    fn new(args: &clap::ArgMatches) -> Self {
        let default_test_dns = args
            .value_of("test-dns")
            .unwrap()
            .parse()
            .expect("not a valid socket address");
        let default_max_wait = parse_max_wait(args);

        let mut cli_servers = vec![];
        if let Some(s) = args.values_of("socks5-servers") {
            for s in s.map(parse_server) {
                cli_servers.push(Arc::new(ProxyServer::new(
                    s.expect("not a valid SOCKSv5 server"),
                    ProxyProto::socks5(false),
                    default_test_dns,
                    default_max_wait,
                    None,
                    None,
                    None,
                )));
            }
        }
        if let Some(s) = args.values_of("http-servers") {
            for s in s.map(parse_server) {
                cli_servers.push(Arc::new(ProxyServer::new(
                    s.expect("not a valid HTTP server"),
                    ProxyProto::http(false, None),
                    default_test_dns,
                    default_max_wait,
                    None,
                    None,
                    None,
                )));
            }
        }
        let path = args.value_of("server-list").map(|s| s.to_string());
        let listen_ports = args
            .values_of("port")
            .expect("missing port number")
            .map(|p| p.parse().expect("invalid port number"))
            .collect();

        ServerListCfg {
            default_test_dns,
            default_max_wait,
            cli_servers,
            path,
            listen_ports,
        }
    }

    #[instrument(level = "debug", skip_all)]
    fn load(&self) -> Result<Vec<Arc<ProxyServer>>, &'static str> {
        let mut servers = self.cli_servers.clone();
        if let Some(path) = &self.path {
            let ini = Ini::load_from_file(path).or(Err("cannot read server list file"))?;
            for (tag, props) in ini.iter() {
                let tag = props.get("tag").or(tag);
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
                let max_wait = props
                    .get("max wait")
                    .parse()
                    .or(Err("not a valid number"))?
                    .map(Duration::from_secs)
                    .unwrap_or(self.default_max_wait);
                let listen_ports = if let Some(ports) = props.get("listen ports") {
                    let ports = ports
                        .split(|c| c == ' ' || c == ',')
                        .filter(|s| !s.is_empty());
                    let mut port_set = HashSet::new();
                    for port in ports {
                        let port = port.parse().or(Err("not a valid port number"))?;
                        port_set.insert(port);
                    }
                    let surplus_ports: Vec<_> = port_set.difference(&self.listen_ports).collect();
                    if !surplus_ports.is_empty() {
                        warn!("{:?}: Surplus listen ports {:?}", addr, surplus_ports);
                    }
                    Some(port_set)
                } else {
                    None
                };
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
                        let username = props.get("socks username").unwrap_or("");
                        let password = props.get("socks password").unwrap_or("");
                        match (username.len(), password.len()) {
                            (0, 0) => ProxyProto::socks5(fake_hs),
                            (0, _) | (_, 0) => return Err("socks username/password is empty"),
                            (u, p) if u > 255 || p > 255 => {
                                return Err("socks username/password too long")
                            }
                            _ => ProxyProto::socks5_with_auth(UserPassAuthCredential::new(
                                username, password,
                            )),
                        }
                    }
                    "http" => {
                        let cwp = props
                            .get("http allow connect payload")
                            .parse()
                            .or(Err("not a boolean value"))?
                            .unwrap_or(false);
                        let credential =
                            match (props.get("http username"), props.get("http password")) {
                                (None, None) => None,
                                (Some(user), _) if user.contains(':') => {
                                    return Err("semicolon (:) in http username")
                                }
                                (user, pass) => Some(UserPassAuthCredential::new(
                                    user.unwrap_or(""),
                                    pass.unwrap_or(""),
                                )),
                            };
                        ProxyProto::http(cwp, credential)
                    }
                    _ => return Err("unknown proxy protocol"),
                };
                let server =
                    ProxyServer::new(addr, proto, test_dns, max_wait, listen_ports, tag, base);
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

fn parse_max_wait(args: &clap::ArgMatches) -> Duration {
    args.value_of("max-wait")
        .parse()
        .expect("not a valid number")
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(4))
}
