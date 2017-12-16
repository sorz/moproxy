extern crate rand;
use std;
use std::time::{Instant, Duration};
use std::io;
use std::rc::Rc;
use std::cell::RefCell;
use std::collections::VecDeque;
use self::rand::Rng;
use tokio_timer::Timer;
use tokio_core::reactor::Handle;
use tokio_io::io::{write_all, read_exact};
use futures::{future, Future};
use futures::future::Loop;
use proxy::ProxyServer;
use ToMillis;

static THROUGHPUT_INTERVAL_SECS: u64 = 2;
static THROUGHPUT_HISTORY_NUM: usize = 5;

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
    pub fn monitor_delay(&self, probe: u64, handle: Handle)
            -> Box<Future<Item=(), Error=()>> {
        let timer = Timer::default();
        let init = test_all(self.clone(), true, &handle, &timer);
        let interval = Duration::from_secs(probe);
        let update = init.and_then(move |monitor| {
            future::loop_fn((monitor, handle, timer),
                      move |(monitor, handle, timer)| {
                timer.sleep(interval).map_err(|err| {
                    error!("error on timer: {}", err);
                }).and_then(move |_| {
                    test_all(monitor, false, &handle, &timer)
                        .map(|monitor| (monitor, handle, timer))
                }).and_then(|args| Ok(Loop::Continue(args)))
            })
        }).map_err(|_| ());
        Box::new(update)
    }

    /// Start monitoring throughput.
    /// Returned Future won't return unless error on timer.
    pub fn monitor_throughput(&self)
            -> Box<Future<Item=(), Error=()>> {
        let timer = Timer::default();
        let sleep = Duration::from_secs(THROUGHPUT_INTERVAL_SECS);
        let lp = future::loop_fn((self.clone(), timer),
                           move |(monitor, timer)| {
            let current = monitor.total_traffics();
            {
                let mut history = monitor.traffics.borrow_mut();
                history.truncate(THROUGHPUT_HISTORY_NUM);
                history.push_front(current);
            }
            timer.sleep(sleep).map_err(|err| {
                error!("error on timer: {}", err);
            }).and_then(move |_| Ok(Loop::Continue((monitor, timer))))
        });
        Box::new(lp)
    }

    /// Return average throughput in the recent monitor period (tx, rx)
    /// in bytes per second. Should start `monitor_throughput()` task
    /// befor call this.
    pub fn throughput(&self) -> (usize, usize) {
        let history = self.traffics.borrow();
        if history.is_empty() {
            return (0, 0);
        }
        let (tx_sum, rx_sum) = history.iter().fold((0, 0),
                |(tx, rx), &(tx1, rx1)| (tx + tx1, rx + rx1));
        let n = history.len();
        (tx_sum / n, rx_sum / n)
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

fn test_all(monitor: Monitor, init: bool, handle: &Handle, timer: &Timer)
        -> Box<Future<Item=Monitor, Error=()>> {
    debug!("testing all servers...");
    let tests: Vec<_> = monitor.servers().into_iter().map(move |server| {
        let test = alive_test(&server, handle, timer).then(move |result| {
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

fn alive_test(server: &ProxyServer, handle: &Handle, timer: &Timer)
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
    let conn = server.connect(server.test_dns.into(), handle);
    let try_conn = timer.timeout(conn, server.max_wait);
    let query = try_conn.and_then(move |stream| {
        write_all(stream, request)
    }).and_then(|(stream, _)| {
        read_exact(stream, [0u8; 12])
    }).and_then(move |(_, buf)| {
        if req_tid == tid(&buf) {
            let delay = now.elapsed();
            debug!("[{}] delay {}ms", tag, delay.millis());
            Ok(delay)
        } else {
            Err(io::Error::new(io::ErrorKind::Other, "unknown response"))
        }
    });
    Box::new(query)
}

