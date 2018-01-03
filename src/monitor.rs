extern crate rand;
use std;
use std::time::{Instant, Duration};
use std::io;
use std::rc::Rc;
use std::cell::RefCell;
use std::collections::VecDeque;
use self::rand::Rng;
use tokio_core::reactor::{Handle, Timeout};
use tokio_io::io::{write_all, read_exact};
use futures::{future, Future};
use futures::future::Loop;
use proxy::ProxyServer;
use ToMillis;

static THROUGHPUT_INTERVAL_SECS: u64 = 1;
static THROUGHPUT_HISTORY_NUM: usize = 3;

pub type ServerList = Vec<Rc<ProxyServer>>;

#[derive(Clone, Debug)]
pub struct Monitor {
    servers: Rc<RefCell<ServerList>>,
    traffics: Rc<RefCell<VecDeque<(usize, usize)>>>,
}

impl Monitor {
    pub fn new(servers: Vec<ProxyServer>) -> Monitor {
        let servers: Vec<_> = servers.into_iter()
            .map(|server| Rc::new(server))
            .collect();
        let traffics = VecDeque::with_capacity(THROUGHPUT_HISTORY_NUM);
        Monitor {
            servers: Rc::new(RefCell::new(servers)),
            traffics: Rc::new(RefCell::new(traffics)),
        }
    }

    /// Return an ordered list of servers.
    pub fn servers(&self) -> ServerList {
        self.servers.borrow().clone()
    }

    fn resort(&self) {
        let mut rng = rand::thread_rng();
        self.servers.borrow_mut().sort_by_key(move |server| {
            server.score().unwrap_or(std::i32::MAX) -
                (rng.next_u32() % 30) as i32
        });
        debug!("scores:{}", info_stats(&*self.servers.borrow()));
    }

    /// Return total traffic amount (tx, rx).
    fn total_traffics(&self) -> (usize, usize) {
        self.servers.borrow().iter().fold((0, 0), |(tx, rx), server| {
            let (tx1, rx1) = server.traffics();
            (tx + tx1, rx + rx1)
        })
    }

    /// Start monitoring delays.
    /// Returned Future won't return unless error on timer.
    pub fn monitor_delay(&self, probe: u64, handle: &Handle)
            -> Box<Future<Item=(), Error=()>> {
        let handle = handle.clone();
        let init = test_all(self.clone(), true, &handle);
        let interval = Duration::from_secs(probe);
        let update = init.and_then(move |monitor| {
            future::loop_fn((monitor, handle), move |(monitor, handle)| {
                let wait = Timeout::new(interval, &handle)
                    .expect("error on get timeout from reactor")
                    .map_err(|err| panic!("error on timer: {}", err));
                wait.and_then(move |_| {
                    test_all(monitor, false, &handle)
                        .map(|monitor| (monitor, handle))
                }).and_then(|args| Ok(Loop::Continue(args)))
            })
        }).map_err(|_| ());
        Box::new(update)
    }

    /// Start monitoring throughput.
    /// Returned Future won't return unless error on timer.
    pub fn monitor_throughput(&self, handle: &Handle)
            -> Box<Future<Item=(), Error=()>> {
        let interval = Duration::from_secs(THROUGHPUT_INTERVAL_SECS);
        let handle = handle.clone();
        let lp = future::loop_fn(self.clone(), move |monitor| {
            let current = monitor.total_traffics();
            {
                let mut history = monitor.traffics.borrow_mut();
                history.truncate(THROUGHPUT_HISTORY_NUM);
                history.push_front(current);
            }
            Timeout::new(interval, &handle)
                .expect("error on get timeout from reactor")
                .map_err(|err| panic!("error on timer: {}", err))
                .map(move |_| Loop::Continue(monitor))
        });
        Box::new(lp)
    }

    /// Return average throughput in the recent monitor period (tx, rx)
    /// in bytes per second. Should start `monitor_throughput()` task
    /// befor call this.
    pub fn throughput(&self) -> (usize, usize) {
        let history = self.traffics.borrow();
        let &(tx0, rx0) = history.back().unwrap_or(&(0, 0));
        let &(tx1, rx1) = history.front().unwrap_or(&(0, 0));
        let t = (history.len() * THROUGHPUT_INTERVAL_SECS as usize) as f64;
        let f = |x0, x1| (((x1 - x0) as f64) / t).round() as usize;
        (f(tx0, tx1), f(rx0, rx1))
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
        -> Box<Future<Item=Monitor, Error=()>> {
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
    let sort = future::join_all(tests).then(move |_| {
        monitor.resort();
        Ok(monitor)
    });
    Box::new(sort)
}

fn alive_test(server: &ProxyServer, handle: &Handle)
        -> Box<Future<Item=Duration, Error=io::Error>> {
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
    let conn = server.connect(server.test_dns.into(), None, handle);
    let query = conn.and_then(move |stream| {
        write_all(stream, request)
    }).and_then(|(stream, _)| {
        read_exact(stream, [0u8; 12])
    }).and_then(move |(_, buf)| {
        if req_tid == tid(&buf) {
            Ok(Some(now.elapsed()))
        } else {
            Err(io::Error::new(io::ErrorKind::Other, "unknown response"))
        }
    });
    let wait = query.select(timeout).map_err(|(err, _)| err);
    let delay = wait.and_then(|(result, _)| match result {
        Some(stream) => Ok(stream),
        None => Err(io::Error::new(io::ErrorKind::TimedOut, "test timeout")),
    }).inspect(move |t| debug!("[{}] delay {}ms", tag, t.millis()));
    Box::new(delay)
}

