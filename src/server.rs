use anyhow::{anyhow, bail, Context};
use futures_util::{stream, StreamExt};
use ini::Ini;
use std::{collections::HashSet, io, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};
use tokio::net::{TcpListener, TcpStream};
use tracing::{info, instrument, warn};

use crate::{cli::CliArgs, FromOptionStr};
use moproxy::{
    client::{Connectable, NewClient},
    futures_stream::TcpListenerStream,
    monitor::{Monitor, ServerList},
    proxy::{ProxyProto, ProxyServer, UserPassAuthCredential},
    web::{self, AutoRemoveFile},
};

#[derive(Clone)]
pub(crate) struct MoProxy {
    cli_args: Arc<CliArgs>,
    server_list_config: Arc<ServerListConfig>,
    monitor: Monitor,
    #[cfg(all(feature = "web_console", unix))]
    _sock_file: Arc<Option<AutoRemoveFile<String>>>,
}

pub(crate) struct MoProxyListener {
    moproxy: MoProxy,
    listeners: Vec<TcpListenerStream>,
}

impl MoProxy {
    pub(crate) async fn new(args: CliArgs) -> anyhow::Result<Self> {
        // Load proxy server list
        let server_list_config = ServerListConfig::new(&args);
        let servers = server_list_config.load().context("fail to load servers")?;

        // Setup proxy monitor
        let graphite = args.graphite;
        #[cfg(feature = "score_script")]
        let mut monitor = Monitor::new(servers, graphite);
        #[cfg(not(feature = "score_script"))]
        let monitor = Monitor::new(servers, graphite);
        #[cfg(feature = "score_script")]
        {
            if let Some(ref path) = args.score_script {
                monitor
                    .load_score_script(path)
                    .context("fail to load Lua script")?;
            }
        }

        // Setup web console
        #[cfg(all(feature = "web_console", unix))]
        let mut sock_file = None;
        #[cfg(feature = "web_console")]
        {
            if let Some(ref http_addr) = args.web_bind {
                info!("http run on {}", http_addr);
                if !http_addr.starts_with('/') || cfg!(not(unix)) {
                    let listener = TcpListener::bind(&http_addr)
                        .await
                        .expect("fail to bind web server");
                    let serv = web::run_server(TcpListenerStream(listener), monitor.clone());
                    tokio::spawn(serv);
                }
                #[cfg(unix)]
                {
                    use moproxy::futures_stream::UnixListenerStream;
                    use tokio::net::UnixListener;
                    if http_addr.starts_with('/') {
                        let sock = web::AutoRemoveFile::new(http_addr.clone());
                        let listener = UnixListener::bind(&sock).expect("fail to bind web server");
                        let serv = web::run_server(UnixListenerStream(listener), monitor.clone());
                        tokio::spawn(serv);
                        sock_file = Some(sock);
                    }
                }
            }
        }

        // Launch monitor
        if args.probe_secs > 0 {
            tokio::spawn(monitor.clone().monitor_delay(args.probe_secs));
        }

        Ok(Self {
            cli_args: Arc::new(args),
            server_list_config: Arc::new(server_list_config),
            monitor,
            _sock_file: Arc::new(sock_file),
        })
    }

    pub(crate) fn reload(&self) -> anyhow::Result<()> {
        // Load proxy server list
        let servers = self.server_list_config.load()?;

        // TODO: reload policy
        // TODO: reload lua script

        // Apply only if no error occur
        self.monitor.update_servers(servers);
        Ok(())
    }

    pub(crate) async fn listen(&self) -> anyhow::Result<MoProxyListener> {
        let ports: HashSet<_> = self.cli_args.port.iter().collect();
        let mut listeners = Vec::with_capacity(ports.len());
        for port in ports {
            let addr = SocketAddr::new(self.cli_args.host, *port);
            let listener = TcpListener::bind(&addr)
                .await
                .context("cannot bind to port")?;
            info!("listen on {}", addr);
            #[cfg(target_os = "linux")]
            if let Some(ref alg) = self.cli_args.cong_local {
                use moproxy::linux::tcp::TcpListenerExt;

                info!("set {} on {}", alg, addr);
                listener.set_congestion(alg).expect(
                    "fail to set tcp congestion algorithm. \
                    check tcp_allowed_congestion_control?",
                );
            }
            listeners.push(TcpListenerStream(listener));
        }
        Ok(MoProxyListener {
            moproxy: self.clone(),
            listeners,
        })
    }
}

impl MoProxyListener {
    pub(crate) async fn handle_forever(mut self) {
        let args = self.moproxy.cli_args;
        let remote_dns = args.remote_dns;
        let n_parallel = args.n_parallel;
        let direct_server = args
            .allow_direct
            .then(|| Arc::new(ProxyServer::direct(args.max_wait)));
        let mut clients = stream::select_all(self.listeners.iter_mut());
        while let Some(sock) = clients.next().await {
            let direct = direct_server.clone();
            let servers = self.moproxy.monitor.servers();
            match sock {
                Ok(sock) => {
                    tokio::spawn(async move {
                        let result =
                            handle_client(sock, servers, remote_dns, n_parallel, direct).await;
                        if let Err(e) = result {
                            info!("error on hanle client: {}", e);
                        }
                    });
                }
                Err(err) => info!("error on accept client: {}", err),
            }
        }
    }
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

struct ServerListConfig {
    default_test_dns: SocketAddr,
    default_max_wait: Duration,
    cli_servers: Vec<Arc<ProxyServer>>,
    path: Option<PathBuf>,
    listen_ports: HashSet<u16>,
    allow_direct: bool,
}

impl ServerListConfig {
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
        Self {
            default_test_dns,
            default_max_wait,
            cli_servers,
            path,
            listen_ports,
            allow_direct: args.allow_direct,
        }
    }

    #[instrument(skip_all)]
    fn load(&self) -> anyhow::Result<Vec<Arc<ProxyServer>>> {
        let mut servers = self.cli_servers.clone();
        if let Some(path) = &self.path {
            let ini = Ini::load_from_file(path).context("cannot read server list file")?;
            for (tag, props) in ini.iter() {
                let tag = props.get("tag").or(tag);
                let addr: SocketAddr = props
                    .get("address")
                    .ok_or(anyhow!("address not specified"))?
                    .parse()
                    .context("not a valid socket address")?;
                let base = props
                    .get("score base")
                    .parse()
                    .context("score base not a integer")?;
                let test_dns = props
                    .get("test dns")
                    .parse()
                    .context("not a valid socket address")?
                    .unwrap_or(self.default_test_dns);
                let max_wait = props
                    .get("max wait")
                    .parse()
                    .context("not a valid number")?
                    .map(Duration::from_secs)
                    .unwrap_or(self.default_max_wait);
                let listen_ports = if let Some(ports) = props.get("listen ports") {
                    let ports = ports
                        .split(|c| c == ' ' || c == ',')
                        .filter(|s| !s.is_empty());
                    let mut port_set = HashSet::new();
                    for port in ports {
                        let port = port.parse().context("not a valid port number")?;
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
                    .context("protocol not specified")?
                    .to_lowercase()
                    .as_str()
                {
                    "socks5" | "socksv5" => {
                        let fake_hs = props
                            .get("socks fake handshaking")
                            .parse()
                            .context("not a boolean value")?
                            .unwrap_or(false);
                        let username = props.get("socks username").unwrap_or("");
                        let password = props.get("socks password").unwrap_or("");
                        match (username.len(), password.len()) {
                            (0, 0) => ProxyProto::socks5(fake_hs),
                            (0, _) | (_, 0) => bail!("socks username/password is empty"),
                            (u, p) if u > 255 || p > 255 => {
                                bail!("socks username/password too long")
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
                            .context("not a boolean value")?
                            .unwrap_or(false);
                        let credential =
                            match (props.get("http username"), props.get("http password")) {
                                (None, None) => None,
                                (Some(user), _) if user.contains(':') => {
                                    bail!("semicolon (:) in http username")
                                }
                                (user, pass) => Some(UserPassAuthCredential::new(
                                    user.unwrap_or(""),
                                    pass.unwrap_or(""),
                                )),
                            };
                        ProxyProto::http(cwp, credential)
                    }
                    _ => bail!("unknown proxy protocol"),
                };
                let server =
                    ProxyServer::new(addr, proto, test_dns, max_wait, listen_ports, tag, base);
                servers.push(Arc::new(server));
            }
        }
        if servers.is_empty() && !self.allow_direct {
            bail!("missing server list");
        }
        info!("total {} server(s) loaded", servers.len());
        Ok(servers)
    }
}
