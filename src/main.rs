use clap::load_yaml;
use futures::{future::Either, Future, Stream};
use ini::Ini;
use log::{debug, info, warn, LevelFilter};
use std::{env, fs, io::Write, net::SocketAddr, path::Path, sync::Arc};
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

fn main() {
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
        .expect("unknown log level");
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
        .expect("missing host")
        .parse()
        .expect("invalid address");
    let port = args
        .value_of("port")
        .expect("missing port number")
        .parse()
        .expect("invalid port number");
    let bind_addr = SocketAddr::new(host, port);
    let probe = args
        .value_of("probe-secs")
        .expect("missing probe secs")
        .parse()
        .expect("not a vaild probe secs");
    let remote_dns = args.is_present("remote-dns");
    let n_parallel = args
        .value_of("n-parallel")
        .map(|v| v.parse().expect("not a valid number"))
        .unwrap_or(0 as usize);
    let cong_local = args.value_of("cong-local");
    let graphite = args
        .value_of("graphite")
        .map(|addr| addr.parse().expect("not a valid address"));
    let servers_cfg = ServerListCfg::new(&args);

    let servers = servers_cfg.load();
    let monitor = Monitor::new(servers, graphite);

    let mut lp = Core::new().expect("fail to create event loop");
    let handle = lp.handle();

    // Setup monitor & web server
    let mut sock_file = None;
    if let Some(http_addr) = args.value_of("web-bind") {
        if !cfg!(feature = "web_console") {
            panic!("web console has been disabled during compiling");
        };
        let monitor = monitor.clone();
        if http_addr.starts_with("/") {
            let sock = AutoRemoveFile::new(&http_addr);
            let incoming = UnixListener::bind(&sock)
                .expect("fail to bind web server")
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
                .expect("not a valid address of TCP socket");
            let incoming = TcpListener::bind(&addr, &handle)
                .expect("fail to bind web server")
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
            // TODO: avoid panic when fail on paring
            let servers = servers_cfg.load();
            monitor_.update_servers(servers);
            Ok(())
        })
        .map_err(|err| warn!("fail to listen SIGHUP: {}", err));
    handle.spawn(list_reloader);

    // Setup proxy server
    let listener = TcpListener::bind(&bind_addr, &handle).expect("cannot bind to port");
    info!("listen on {}", bind_addr);
    if let Some(alg) = cong_local {
        info!("set {} on {}", alg, bind_addr);
        set_congestion(&listener, alg).expect(
            "fail to set tcp congestion algorithm. \
             check tcp_allowed_congestion_control?",
        );
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
    lp.run(server).expect("error on event loop");

    // make sure socket file will be deleted on exit.
    // unnecessary drop() but make complier happy about unused var.
    drop(sock_file);
}

struct ServerListCfg {
    default_test_dns: SocketAddr,
    cli_servers: Vec<Arc<ProxyServer>>,
    path: Option<String>,
}

impl ServerListCfg {
    fn new(args: &clap::ArgMatches) -> Self {
        let default_test_dns = args
            .value_of("test-dns")
            .expect("missing test-dns")
            .parse()
            .expect("not a valid socket address");
        let mut cli_servers = vec![];
        if let Some(s) = args.values_of("socks5-servers") {
            for s in s.map(parse_server) {
                cli_servers.push(Arc::new(ProxyServer::new(
                    s,
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
                    s,
                    ProxyProto::http(false),
                    default_test_dns,
                    None,
                    None,
                )));
            }
        }
        let path = args.value_of("server-list").map(|s| s.to_string());

        ServerListCfg {
            default_test_dns,
            cli_servers,
            path,
        }
    }

    fn load(&self) -> Vec<Arc<ProxyServer>> {
        let mut servers = self.cli_servers.clone();
        if let Some(path) = &self.path {
            let ini = Ini::load_from_file(path).expect("cannot read server list file");
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
                    .expect("address not specified")
                    .parse()
                    .expect("not a valid socket address");
                let base = props
                    .get("score base")
                    .map(|i| i.parse().expect("score base not a integer"));
                let test_dns = props
                    .get("test dns")
                    .map(|i| i.parse().expect("not a valid socket address"))
                    .unwrap_or(self.default_test_dns);
                let proto = match props
                    .get("protocol")
                    .expect("protocol not specified")
                    .to_lowercase()
                    .as_str()
                {
                    "socks5" => ProxyProto::socks5(),
                    "socksv5" => ProxyProto::socks5(),
                    "http" => {
                        let cwp = props
                            .get("http allow connect payload")
                            .map_or(false, |v| v.parse().expect("not a boolean value"));
                        ProxyProto::http(cwp)
                    }
                    _ => panic!("unknown proxy protocol"),
                };
                let server = ProxyServer::new(addr, proto, test_dns, tag, base);
                servers.push(Arc::new(server));
            }
        }
        if servers.len() == 0 {
            panic!("missing server list");
        }
        info!("total {} server(s) loaded", servers.len());
        servers
    }
}

fn parse_server(addr: &str) -> SocketAddr {
    if addr.contains(":") {
        addr.parse()
    } else {
        format!("127.0.0.1:{}", addr).parse()
    }
    .expect("not a valid server address")
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
