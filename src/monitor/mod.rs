mod traffic;
use std;
use std::io;
use std::time::{Instant, Duration};
use std::sync::{Arc, Mutex};
use std::collections::HashMap;
use rand::{self, Rng};
use tokio_core::reactor::{Handle, Timeout};
use tokio_io::io::read_exact;
use futures::{future, Future};
use futures::future::Loop;
use proxy::ProxyServer;
use ToMillis;
use self::traffic::Meter;
pub use self::traffic::Throughput;


static THROUGHPUT_INTERVAL_SECS: u64 = 1;

pub type ServerList = Vec<Arc<ProxyServer>>;

#[derive(Clone, Debug)]
pub struct Monitor {
    servers: Arc<Mutex<ServerList>>,
    meters: Arc<Mutex<HashMap<Arc<ProxyServer>, Meter>>>,
}

impl Monitor {
    pub fn new(servers: Vec<ProxyServer>) -> Monitor {
        let servers: Vec<_> = servers.into_iter()
            .map(|server| Arc::new(server))
            .collect();
        let meters = servers.iter()
            .map(|server| (server.clone(), Meter::new()))
            .collect();
        Monitor {
            servers: Arc::new(Mutex::new(servers)),
            meters: Arc::new(Mutex::new(meters)),
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

    /// Start monitoring delays.
    /// Returned Future won't return unless error on timer.
    pub fn monitor_delay(&self, probe: u64, handle: &Handle)
            -> impl Future<Item=(), Error=()> {
        let handle = handle.clone();
        let init = test_all(self.clone(), true, &handle);
        let interval = Duration::from_secs(probe);
        init.and_then(move |monitor| {
            future::loop_fn((monitor, handle), move |(monitor, handle)| {
                let wait = Timeout::new(interval, &handle)
                    .expect("error on get timeout from reactor")
                    .map_err(|err| panic!("error on timer: {}", err));
                wait.and_then(move |_| {
                    test_all(monitor, false, &handle)
                        .map(|monitor| (monitor, handle))
                }).and_then(|args| Ok(Loop::Continue(args)))
            })
        }).map_err(|_| ())
    }

    /// Start monitoring throughput.
    /// Returned Future won't return unless error on timer.
    pub fn monitor_throughput(&self, handle: &Handle)
            -> impl Future<Item=(), Error=()> {
        let interval = Duration::from_secs(THROUGHPUT_INTERVAL_SECS);
        let handle = handle.clone();
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

fn test_all(monitor: Monitor, init: bool, handle: &Handle)
        -> impl Future<Item=Monitor, Error=()> {
    debug!("testing all servers...");
    let tests: Vec<_> = monitor.servers().into_iter().map(move |server| {
        let test = alive_test(&server, handle).then(move |result| {
            if init {
                server.set_delay(result.ok());
            } else {
                server.update_delay(result.ok());
            }
            Ok(())
        });
        Box::new(test) as Box<Future<Item=(), Error=()>>
    }).collect();
    future::join_all(tests).then(move |_| {
        monitor.resort();
        Ok(monitor)
    })
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

