mod traffic;
mod graphite;
use std;
use std::io;
use std::net::SocketAddr;
use std::time::{Instant, Duration};
use std::sync::{Arc, Mutex};
use std::collections::HashMap;
use rand::{self, Rng};
use tokio_core::reactor::{Handle, Timeout, Remote};
use tokio_core::net::TcpStream;
use tokio_io::io::{read_exact, write_all};
use futures::{future, Future, IntoFuture};
use futures::future::Loop;
use proxy::ProxyServer;
use ToMillis;
use self::traffic::Meter;
use self::graphite::{Record, write_records};
pub use self::traffic::Throughput;


static THROUGHPUT_INTERVAL_SECS: u64 = 1;
static GRAPHITE_TIMEOUT_SECS: u64 = 5;

pub type ServerList = Vec<Arc<ProxyServer>>;

#[derive(Clone, Debug)]
pub struct Monitor {
    servers: Arc<Mutex<ServerList>>,
    meters: Arc<Mutex<HashMap<Arc<ProxyServer>, Meter>>>,
    graphite: Option<SocketAddr>,
    remote: Remote,
}

impl Monitor {
    pub fn new(servers: Vec<ProxyServer>,
               graphite: Option<SocketAddr>,
               handle: Handle) -> Monitor {
        let servers: Vec<_> = servers.into_iter()
            .map(|server| Arc::new(server))
            .collect();
        let meters = servers.iter()
            .map(|server| (server.clone(), Meter::new()))
            .collect();
        Monitor {
            servers: Arc::new(Mutex::new(servers)),
            meters: Arc::new(Mutex::new(meters)),
            remote: handle.remote().clone(),
            graphite,
        }
    }

    /// Return an ordered list of servers.
    pub fn servers(&self) -> ServerList {
        self.servers.lock().unwrap().clone()
    }

    fn resort(&self) {
        let mut rng = rand::thread_rng();
        self.servers.lock().unwrap().sort_by_key(move |server| {
            server.score().unwrap_or(std::i32::MAX) -
                (rng.gen::<u8>() % 30) as i32
        });
        debug!("scores:{}", info_stats(&*self.servers.lock().unwrap()));
    }

    fn handle(&self) -> Handle {
        self.remote.handle()
            .expect("Should run in the same thread.")
    }

    /// Start monitoring delays.
    /// Returned Future won't return unless error on timer.
    pub fn monitor_delay(&self, probe: u64)
            -> impl Future<Item=(), Error=()> {
        let init = test_all(self.clone(), true);
        let interval = Duration::from_secs(probe);
        let handle = self.handle();
        init.and_then(move |monitor| {
            future::loop_fn(monitor, move |monitor| {
                let wait = Timeout::new(interval, &handle)
                    .expect("error on get timeout from reactor")
                    .map_err(|err| panic!("error on timer: {}", err));
                wait.and_then(move |_| {
                    test_all(monitor, false).and_then(send_metrics)
                }).and_then(|args| Ok(Loop::Continue(args)))
            })
        }).map_err(|_| ())
    }

    /// Start monitoring throughput.
    /// Returned Future won't return unless error on timer.
    pub fn monitor_throughput(&self)
            -> impl Future<Item=(), Error=()> {
        let interval = Duration::from_secs(THROUGHPUT_INTERVAL_SECS);
        let handle = self.handle();
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
        self.meters.lock().unwrap().iter().map(|(server, meter)| {
            (server.clone(), meter.throughput(server.traffic()))
        }).collect()
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

fn test_all(monitor: Monitor, init: bool)
        -> impl Future<Item=Monitor, Error=()> {
    debug!("testing all servers...");
    let handle = monitor.handle();
    let tests: Vec<_> = monitor.servers().into_iter().map(move |server| {
        let test = alive_test(&server, &handle).then(move |result| {
            if init {
                server.set_delay(result.ok());
            } else {
                server.update_delay(result.ok());
            }
            Ok(())
        });
        Box::new(test) as Box<Future<Item=(), Error=()>>
    }).collect();

    future::join_all(tests).map(move |_| {
        monitor.resort();
        monitor
    })
}

// send graphite metrics if need
fn send_metrics(monitor: Monitor)
        -> impl Future<Item=Monitor, Error=()> {
    let handle = monitor.handle();
    let servers = monitor.servers();
    if let Some(ref addr) = monitor.graphite {
        let records = servers.iter().flat_map(|server| {
            let delay = server.delay().map(|t| {
                let ms = t.millis() as i32;
                Record::new(server.graphite_path("delay"), ms, None)
            });
            let score = server.score().map(|s| {
                Record::new(server.graphite_path("score"), s as i32, None)
            });
            vec![delay, score]
        }).filter_map(|v| v);
        let mut buf = Vec::new();
        write_records(&mut buf, records).unwrap();

        let timeout = Duration::from_secs(GRAPHITE_TIMEOUT_SECS);
        let timeout = Timeout::new(timeout, &handle)
            .expect("error on get timeout from reactor");

        let send = TcpStream::connect(addr, &handle)
            .and_then(move |conn| write_all(conn, buf)).map(|_| ())
            .select(timeout).map_err(|(err, _)| err)
            .then(|result| {
                if let Err(err) = result {
                    warn!("fail to send metrics: {}", err);
                }
                Ok(())
            });
        Box::new(send) as Box<Future<Item=(), Error=()>>
    } else {
        Box::new(Ok(()).into_future()) as Box<Future<Item=(), Error=()>>
    }.map(|()| monitor)
}

fn alive_test(server: &ProxyServer, handle: &Handle)
        -> impl Future<Item=Duration, Error=io::Error> {
    let request = [
        0, 17,  // length
        rand::random(), rand::random(),  // transaction ID
        1, 32,  // standard query
        0, 1,  // one query
        0, 0,  // answer
        0, 0,  // authority
        0, 0,  // addition
        0,     // query: root
        0, 1,  // query: type A
        0, 1,  // query: class IN
    ];
    let tid = |req: &[u8]| (req[2] as u16) << 8 | (req[3] as u16);
    let req_tid = tid(&request);

    let now = Instant::now();
    let tag = server.tag.clone();
    let timeout = Timeout::new(server.max_wait, handle)
        .expect("error on get timeout from reactor")
        .map(|_| None);
    let conn = server.connect(server.test_dns.into(), Some(request), handle);
    let query = conn.and_then(|stream| {
        read_exact(stream, [0u8; 12])
    }).and_then(move |(_, buf)| {
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
    }).inspect(move |t| debug!("[{}] delay {}ms", tag, t.millis()))
}

