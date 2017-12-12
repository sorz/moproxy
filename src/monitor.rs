extern crate rand;
use std;
use std::time::{Instant, Duration};
use std::io;
use std::rc::Rc;
use std::cell::RefCell;
use self::rand::Rng;
use ::tokio_timer::Timer;
use ::tokio_core::reactor::Handle;
use ::tokio_io::io::{write_all, read_exact};
use ::futures::{future, Future};
use ::proxy::ProxyServer;

pub type ServerList = Vec<Rc<ProxyServer>>;

#[derive(Clone, Debug)]
pub struct Monitor {
    servers: RefCell<ServerList>,
}

impl Monitor {
    pub fn new(servers: Vec<ProxyServer>) -> Monitor {
        let servers: Vec<_> = servers.into_iter()
            .map(|server| Rc::new(server))
            .collect();
        Monitor {
            servers: RefCell::new(servers),
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

    /// Start monitoring.
    pub fn run(self, probe: u64, handle: Handle)
            -> Box<Future<Item=(), Error=()>> {
        let timer = Timer::default();
        let init = test_all(self, 0, &handle, &timer);
        let interval = Duration::from_secs(probe);
        let update = init.and_then(move |monitor| {
            future::loop_fn((monitor, handle, timer),
                      move |(monitor, handle, timer)| {
                timer.sleep(interval).map_err(|err| {
                    error!("error on timer: {}", err);
                }).and_then(move |_| {
                    test_all(monitor, 4000, &handle, &timer)
                        .map(|monitor| (monitor, handle, timer))
                }).and_then(|args| Ok(future::Loop::Continue(args)))
            })
        }).map_err(|_| ());
        Box::new(update)
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

fn test_all(monitor: Monitor, penalty: i32, handle: &Handle, timer: &Timer)
        -> Box<Future<Item=Monitor, Error=()>> {
    debug!("testing all servers...");
    let tests: Vec<_> = monitor.servers().into_iter().map(move |server| {
        let test = alive_test(&server, handle, timer).then(move |result| {
            server.update_delay(result.ok(), penalty);
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
    let try_conn = timer.timeout(conn, Duration::from_secs(4));
    let query = try_conn.and_then(move |stream| {
        write_all(stream, request)
    }).and_then(|(stream, _)| {
        read_exact(stream, [0u8; 12])
    }).and_then(move |(_, buf)| {
        if req_tid == tid(&buf) {
            let delay = now.elapsed();
            debug!("[{}] delay {}ms", tag, to_ms(delay));
            Ok(delay)
        } else {
            Err(io::Error::new(io::ErrorKind::Other, "unknown response"))
        }
    });
    Box::new(query)
}

fn to_ms(t: Duration) -> i32 {
    (t.as_secs() as u32 * 1000 + t.subsec_nanos() / 1_000_000) as i32
}

