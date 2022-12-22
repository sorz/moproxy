mod cli;

use clap::Parser;
use cli::CliArgs;
use futures_util::{stream, StreamExt};
use ini::Ini;
use std::{collections::HashSet, io, net::SocketAddr, str::FromStr, sync::Arc, time::Duration};
#[cfg(all(feature = "web_console", unix))]
use tokio::net::UnixListener;
#[cfg(unix)]
use tokio::signal::unix::{signal, SignalKind};
use tokio::{
    self,
    net::{TcpListener, TcpStream},
};
use tracing::{debug, error, info, instrument, warn};

#[cfg(all(unix, feature = "web_console"))]
use moproxy::futures_stream::UnixListenerStream;
#[cfg(all(feature = "systemd", target_os = "linux"))]
use moproxy::linux::systemd;
#[cfg(target_os = "linux")]
use moproxy::linux::tcp::TcpListenerExt;
#[cfg(feature = "web_console")]
use moproxy::web;
use moproxy::{
    client::{Connectable, NewClient},
    futures_stream::TcpListenerStream,
    monitor::{Monitor, ServerList},
    proxy::{ProxyProto, ProxyServer, UserPassAuthCredential},
};
use tracing_subscriber::prelude::*;

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
    let args = cli::CliArgs::parse();
    let mut log_registry: Option<_> = tracing_subscriber::registry().with(args.log_level).into();

    #[cfg(all(feature = "systemd", target_os = "linux"))]
    {
        if systemd::is_stderr_connected_to_journal() {
            match tracing_journald::layer() {
                Ok(layer) => {
                    log_registry.take().unwrap().with(layer).init();
                    debug!("Use native journal protocol");
                }
                Err(err) => eprintln!(
                    "Failed to connect systemd-journald: {}; fallback to STDERR.",
                    err
                ),
            }
        }
    }
    if let Some(registry) = log_registry {
        registry.with(tracing_subscriber::fmt::layer()).init();
    }

    let graphite = args.graphite;
    let servers_cfg = ServerListCfg::new(&args);
    let servers = servers_cfg.load().expect("fail to load servers from file");

    #[cfg(feature = "score_script")]
    let mut monitor = Monitor::new(servers, graphite);
    #[cfg(not(feature = "score_script"))]
    let monitor = Monitor::new(servers, graphite);

    #[cfg(feature = "score_script")]
    {
        if let Some(path) = args.score_script {
            monitor
                .load_score_script(&path)
                .expect("fail to load Lua script");
        }
    }

    #[cfg(all(feature = "web_console", unix))]
    let mut sock_file = None;
    #[cfg(feature = "web_console")]
    {
        if let Some(http_addr) = &args.web_bind {
            info!("http run on {}", http_addr);
            #[cfg(unix)]
            {
                if http_addr.starts_with('/') {
                    let sock = web::AutoRemoveFile::new(&http_addr);
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
    if args.probe_secs > 0 {
        tokio::spawn(monitor.clone().monitor_delay(args.probe_secs));
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
    let direct_server = if args.allow_direct {
        Some(Arc::new(ProxyServer::direct(args.max_wait)))
    } else {
        None
    };

    // Setup proxy server
    let ports: HashSet<_> = args.port.into_iter().collect();
    let mut listeners = Vec::with_capacity(ports.len());
    for port in ports {
        let addr = SocketAddr::new(args.host, port);
        let listener = TcpListener::bind(&addr).await.expect("cannot bind to port");
        info!("listen on {}", addr);
        #[cfg(target_os = "linux")]
        if let Some(ref alg) = args.cong_local {
            info!("set {} on {}", alg, addr);
            listener.set_congestion(alg).expect(
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
    let remote_dns = args.remote_dns;
    let n_parallel = args.n_parallel;
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

#[instrument(skip_all)]
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

#[instrument(level = "error", skip_all, fields(on_port=sock.local_addr()?.port(), peer=?sock.peer_addr()?))]
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
    allow_direct: bool,
}

impl ServerListCfg {
    fn new(args: &CliArgs) -> Self {
        let default_test_dns = args.test_dns;
        let default_max_wait = args.max_wait;

        let mut cli_servers = vec![];
        for addr in &args.socks5_servers {
            cli_servers.push(Arc::new(ProxyServer::new(
                addr.clone(),
                ProxyProto::socks5(false),
                default_test_dns,
                default_max_wait,
                None,
                None,
                None,
            )));
        }

        for addr in &args.http_servers {
            cli_servers.push(Arc::new(ProxyServer::new(
                addr.clone(),
                ProxyProto::http(false, None),
                default_test_dns,
                default_max_wait,
                None,
                None,
                None,
            )));
        }

        let path = args.server_list.clone();
        let listen_ports = args.port.iter().cloned().collect();
        ServerListCfg {
            default_test_dns,
            default_max_wait,
            cli_servers,
            path,
            listen_ports,
            allow_direct: args.allow_direct,
        }
    }

    #[instrument(skip_all)]
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
        if servers.is_empty() && !self.allow_direct {
            return Err("missing server list");
        }
        info!("total {} server(s) loaded", servers.len());
        Ok(servers)
    }
}
