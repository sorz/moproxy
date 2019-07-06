mod graphite;
mod traffic;
use futures::future::Loop;
use futures::{future, Future};
use log::debug;
use rand::{self, Rng};
use std;
use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};
use tokio_core::reactor::{Handle, Timeout};
use tokio_io::io::read_exact;

use self::graphite::{Graphite, Record};
use self::traffic::Meter;
pub use self::traffic::Throughput;
use crate::{proxy::ProxyServer, ToMillis};

static THROUGHPUT_INTERVAL_SECS: u64 = 1;

pub type ServerList = Vec<Arc<ProxyServer>>;

#[derive(Clone, Debug)]
pub struct Monitor {
    servers: Arc<Mutex<ServerList>>,
    meters: Arc<Mutex<HashMap<Arc<ProxyServer>, Meter>>>,
    graphite: Option<SocketAddr>,
}

impl Monitor {
    pub fn new(servers: Vec<Arc<ProxyServer>>, graphite: Option<SocketAddr>) -> Monitor {
        let meters = servers
            .iter()
            .map(|server| (server.clone(), Meter::new()))
            .collect();
        Monitor {
            servers: Arc::new(Mutex::new(servers)),
            meters: Arc::new(Mutex::new(meters)),
            graphite,
        }
    }

    /// Return an ordered list of servers.
    pub fn servers(&self) -> ServerList {
        self.servers.lock().unwrap().clone()
    }

    /// Replace internal servers with provided list.
    pub fn update_servers(&self, mut new_servers: Vec<Arc<ProxyServer>>) {
        let mut servers = self.servers.lock().unwrap();
        for server in new_servers.iter_mut() {
            let old = servers.iter().find(|s| *s == server);
            if let Some(old) = old {
                server.replace_status(old);
            }
        }
        *servers = new_servers;

        let mut meters = self.meters.lock().unwrap();
        meters.clear();
        for server in servers.iter() {
            meters.insert(server.clone(), Meter::new());
        }
        self.resort();
    }

    fn resort(&self) {
        let mut rng = rand::thread_rng();
        self.servers.lock().unwrap().sort_by_key(move |server| {
            server.score().unwrap_or(std::i32::MAX) - (rng.gen::<u8>() % 30) as i32
        });
        debug!("scores:{}", info_stats(&*self.servers.lock().unwrap()));
    }

    /// Start monitoring delays.
    /// Returned Future won't return unless error on timer.
    pub fn monitor_delay(&self, probe: u64, handle: Handle) -> impl Future<Item = (), Error = ()> {
        let graphite = self.graphite.map(Graphite::new);
        let interval = Duration::from_secs(probe);
        let init = test_all(self.clone(), handle);
        init.and_then(move |(mon, hd)| {
            future::loop_fn((mon, hd, graphite), move |(mon, hd, graphite)| {
                let wait = Timeout::new(interval, &hd)
                    .expect("error on get timeout from reactor")
                    .map_err(|err| panic!("error on timer: {}", err));
                wait.and_then(move |_| {
                    let test = test_all(mon, hd);
                    if let Some(graphite) = graphite {
                        let send = test
                            .and_then(|(mon, hd)| send_metrics(mon, hd, graphite))
                            .map(|(mon, hd, graphite)| (mon, hd, Some(graphite)));
                        Box::new(send) as Box<dyn Future<Item = _, Error = ()>>
                    } else {
                        Box::new(test.map(|(mon, hd)| (mon, hd, None)))
                    }
                })
                .and_then(|args| Ok(Loop::Continue(args)))
            })
        })
        .map_err(|_| ())
    }

    /// Start monitoring throughput.
    /// Returned Future won't return unless error on timer.
    pub fn monitor_throughput(&self, handle: Handle) -> impl Future<Item = (), Error = ()> {
        let interval = Duration::from_secs(THROUGHPUT_INTERVAL_SECS);
        future::loop_fn(self.clone(), move |monitor| {
            for (server, meter) in monitor.meters.lock().unwrap().iter_mut() {
                meter.add_sample(server.traffic());
            }
            Timeout::new(interval, &handle)
                .expect("error on get timeout from reactor")
                .map_err(|err| panic!("error on timer: {}", err))
                .map(move |_| Loop::Continue(monitor))
        })
    }

    /// Return average throughputs of all servers in the recent monitor
    /// period. Should start `monitor_throughput()` task before call this.
    pub fn throughputs(&self) -> HashMap<Arc<ProxyServer>, Throughput> {
        self.meters
            .lock()
            .unwrap()
            .iter()
            .map(|(server, meter)| (server.clone(), meter.throughput(server.traffic())))
            .collect()
    }
}

fn info_stats(infos: &ServerList) -> String {
    let mut stats = String::new();
    for info in infos.iter().take(5) {
        stats += &match info.score() {
            None => format!(" {}: --,", info.tag),
            Some(t) => format!(" {}: {},", info.tag, t),
        };
    }
    stats.pop();
    stats
}

fn test_all(monitor: Monitor, handle: Handle) -> impl Future<Item = (Monitor, Handle), Error = ()> {
    debug!("testing all servers...");
    let handle_ = handle.clone();
    let tests: Vec<_> = monitor
        .servers()
        .into_iter()
        .map(move |server| {
            let test = alive_test(&server, &handle_).then(move |result| {
                server.update_delay(result.ok());
                Ok(())
            });
            Box::new(test) as Box<dyn Future<Item = (), Error = ()>>
        })
        .collect();

    future::join_all(tests).map(move |_| {
        monitor.resort();
        (monitor, handle)
    })
}

// send graphite metrics if need
fn send_metrics(
    monitor: Monitor,
    handle: Handle,
    graphite: Graphite,
) -> impl Future<Item = (Monitor, Handle, Graphite), Error = ()> {
    let records = monitor
        .servers()
        .iter()
        .flat_map(|server| {
            let now = Some(SystemTime::now());
            let r = |path, value| Record::new(server.graphite_path(path), value, now);
            let traffic = server.traffic();
            vec![
                server.delay().map(|t| r("delay", t.millis() as u64)),
                server.score().map(|s| r("score", s as u64)),
                Some(r("tx_bytes", traffic.tx_bytes as u64)),
                Some(r("rx_bytes", traffic.rx_bytes as u64)),
                Some(r("conns.total", server.conn_total() as u64)),
                Some(r("conns.alive", server.conn_alive() as u64)),
                Some(r("conns.error", server.conn_error() as u64)),
            ]
        })
        .filter_map(|v| v)
        .collect(); // FIXME: avoid allocate large memory
    graphite
        .write_records(records, &handle)
        .map(move |graphite| (monitor, handle, graphite))
}

fn alive_test(
    server: &ProxyServer,
    handle: &Handle,
) -> impl Future<Item = Duration, Error = io::Error> {
    let request = [
        0,
        17, // length
        rand::random(),
        rand::random(), // transaction ID
        1,
        32, // standard query
        0,
        1, // one query
        0,
        0, // answer
        0,
        0, // authority
        0,
        0, // addition
        0, // query: root
        0,
        1, // query: type A
        0,
        1, // query: class IN
    ];
    let tid = |req: &[u8]| (req[2] as u16) << 8 | (req[3] as u16);
    let req_tid = tid(&request);

    let now = Instant::now();
    let tag = server.tag.clone();
    let timeout = Timeout::new(server.max_wait, handle)
        .expect("error on get timeout from reactor")
        .map(|_| None);
    let conn = server.connect(server.test_dns.into(), Some(request), handle);
    let query = conn
        .and_then(|stream| read_exact(stream, [0u8; 12]))
        .and_then(move |(_, buf)| {
            if req_tid == tid(&buf) {
                Ok(Some(now.elapsed()))
            } else {
                Err(io::Error::new(io::ErrorKind::Other, "unknown response"))
            }
        });
    let wait = query.select(timeout).map_err(|(err, _)| err);
    wait.and_then(|(result, _)| match result {
        Some(stream) => Ok(stream),
        None => Err(io::Error::new(io::ErrorKind::TimedOut, "test timeout")),
    })
    .inspect(move |t| debug!("[{}] delay {}ms", tag, t.millis()))
}
