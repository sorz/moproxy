use clap::load_yaml;
use futures::{future::Either, Future, Stream};
use ini::Ini;
use log::{debug, error, info, warn, LevelFilter};
use std::{env, fs, io::Write, net::SocketAddr, path::Path, str::FromStr, sync::Arc};
use tokio_core::net::TcpListener;
use tokio_core::reactor::Core;
use tokio_signal::unix::{Signal, SIGHUP};
use tokio_uds::UnixListener;

#[cfg(feature = "web_console")]
use moproxy::web;
use moproxy::{
    client::{Connectable, NewClient},
    monitor::Monitor,
    proxy::copy::SharedBuf,
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

fn main() -> Result<(), &'static str> {
    let yaml = load_yaml!("cli.yml");
    let args = clap::App::from_yaml(yaml)
        .version(env!("CARGO_PKG_VERSION"))
        .get_matches();

    let mut logger = env_logger::Builder::new();
    if let Ok(env_log) = env::var("RUST_LOG") {
        logger.parse(&env_log);
    }
    let log_level = args
        .value_of("log-level")
        .unwrap_or("info")
        .parse()
        .map_err(|_| "unknown log level")?;
    logger
        .filter(None, log_level)
        .filter(Some("tokio_reactor"), LevelFilter::Warn)
        .filter(Some("tokio_core"), LevelFilter::Warn)
        .filter(Some("hyper"), LevelFilter::Warn)
        .filter(Some("ini"), LevelFilter::Warn)
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
    let graphite = args
        .value_of("graphite")
        .parse()
        .or(Err("not a valid address"))?;
    let servers_cfg = ServerListCfg::new(&args)?;

    let servers = servers_cfg.load()?;
    let monitor = Monitor::new(servers, graphite);

    let mut lp = Core::new().or(Err("fail to create event loop"))?;
    let handle = lp.handle();

    // Setup monitor & web server
    let mut sock_file = None;
    if let Some(http_addr) = args.value_of("web-bind") {
        if !cfg!(feature = "web_console") {
            return Err("web console has been disabled during compiling");
        };
        let monitor = monitor.clone();
        if http_addr.starts_with('/') {
            let sock = AutoRemoveFile::new(&http_addr);
            let incoming = UnixListener::bind(&sock)
                .or(Err("fail to bind web server"))?
                .incoming()
                .and_then(|s| s.peer_addr().map(|addr| (s, addr)));
            sock_file = Some(sock);
            #[cfg(feature = "web_console")]
            {
                let serv = web::run_server(incoming, monitor, &handle);
                handle.spawn(serv);
            }
        } else {
            // FIXME: remove duplicate code
            let addr = http_addr
                .parse()
                .or(Err("not a valid address of TCP socket"))?;
            let incoming = TcpListener::bind(&addr, &handle)
                .or(Err("fail to bind web server"))?
                .incoming();
            #[cfg(feature = "web_console")]
            {
                let serv = web::run_server(incoming, monitor, &handle);
                handle.spawn(serv);
            }
        }
        info!("http run on {}", http_addr);
    }
    handle.spawn(monitor.monitor_delay(probe, handle.clone()));

    // Setup signal listener for reloading server list
    let monitor_ = monitor.clone();
    let list_reloader = Signal::new(SIGHUP)
        .flatten_stream()
        .for_each(move |_sig| {
            debug!("SIGHUP received, reload server list.");
            match servers_cfg.load() {
                Ok(servers) => monitor_.update_servers(servers),
                Err(err) => error!("fail to reload servers: {}", err),
            }
            Ok(())
        })
        .map_err(|err| warn!("fail to listen SIGHUP: {}", err));
    handle.spawn(list_reloader);

    // Setup proxy server
    let listener = TcpListener::bind(&bind_addr, &handle).or(Err("cannot bind to port"))?;
    info!("listen on {}", bind_addr);
    if let Some(alg) = cong_local {
        info!("set {} on {}", alg, bind_addr);
        set_congestion(&listener, alg).or(Err("fail to set tcp congestion algorithm. \
                                                check tcp_allowed_congestion_control?"))?;
    }
    let shared_buf = SharedBuf::new(8192);
    let server = listener.incoming().for_each(move |(sock, addr)| {
        debug!("incoming {}", addr);
        let client = NewClient::from_socket(sock, monitor.servers(), handle.clone());
        let conn = client.and_then(move |client| {
            if remote_dns && client.dest.port == 443 {
                Either::A(
                    client
                        .retrive_dest()
                        .and_then(move |client| client.connect_server(n_parallel)),
                )
            } else {
                Either::B(client.connect_server(0))
            }
        });
        let buf = shared_buf.clone();
        let serv = conn.and_then(|client| client.serve(buf));
        handle.spawn(serv);
        Ok(())
    });
    lp.run(server).or(Err("error on event loop"))?;

    // make sure socket file will be deleted on exit.
    // unnecessary drop() but make complier happy about unused var.
    drop(sock_file);
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
                    ProxyProto::socks5(),
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
                    "socks5" => ProxyProto::socks5(),
                    "socksv5" => ProxyProto::socks5(),
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

/// File on this path will be removed on `drop()`.
struct AutoRemoveFile<'a> {
    path: &'a str,
}

impl<'a> AutoRemoveFile<'a> {
    fn new(path: &'a str) -> Self {
        AutoRemoveFile { path }
    }
}

impl<'a> Drop for AutoRemoveFile<'a> {
    fn drop(&mut self) {
        if let Err(err) = fs::remove_file(&self.path) {
            warn!("fail to remove {}: {}", self.path, err);
        }
    }
}

impl<'a> AsRef<Path> for &'a AutoRemoveFile<'a> {
    fn as_ref(&self) -> &Path {
        self.path.as_ref()
    }
}
