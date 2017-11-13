extern crate rand;
extern crate tokio_core;
extern crate tokio_io;
extern crate tokio_timer;
extern crate futures;
use std;
use std::thread;
use std::time::{Instant, Duration};
use std::sync::{Mutex, Arc, MutexGuard};
use std::io::{self, Write, Read};
use self::rand::Rng;
use self::tokio_timer::Timer;
use self::tokio_core::reactor::Handle;
use self::tokio_io::io::{write_all, read_exact};
use self::futures::{future, Future};
use self::futures::stream::iter_ok;
use ::proxy::ProxyServer;


#[derive(Clone)]
pub struct ServerInfo {
    pub server: Arc<Box<ProxyServer + 'static>>,
    pub delay: Option<u32>,
}

pub struct ServerList {
    inner: Mutex<Vec<ServerInfo>>,
}

impl ServerList {
    pub fn new(servers: Vec<Box<ProxyServer>>) -> ServerList {
        let mut infos: Vec<ServerInfo> = vec![];
        for s in servers {
            let info = ServerInfo {
                server: Arc::new(s),
                delay: None,
            };
            infos.push(info);
        }
        ServerList {
            inner: Mutex::new(infos),
        }
    }

    pub fn get(&self) -> MutexGuard<Vec<ServerInfo>> {
        self.inner.lock().unwrap()
    }

    fn set(&self, infos: Vec<ServerInfo>) {
        *self.inner.lock().unwrap() = infos;
    }
}

pub fn monitoring_servers(servers: Arc<ServerList>, probe: u64,
                          handle: Handle)
        -> Box<Future<Item=(), Error=()>> {
    let handle_ = handle.clone();
    let tests = servers.get().clone().into_iter().map(move |info| {
        // TODO: timeout
        alive_test(&**info.server, &handle_)
            .then(|t| future::ok(t.ok()))
    });
    let servers_ = servers.clone();
    let init = future::join_all(tests).and_then(move |delays| {
        info!("probe init done");
        let mut infos = servers_.get().clone();
        infos.iter_mut().zip(delays.iter()).for_each(|(info, t)| {
            info.delay = *t;
        });
        infos.sort_by_key(|info| info.delay.unwrap_or(std::u32::MAX));
        servers_.set(infos.clone());
        future::ok(infos)
    });
    let servers_ = servers.clone();
    let handle_ = handle.clone();
    let timer = Timer::default();
    let update = init.and_then(move |infos| {
        future::loop_fn((infos, servers_), move |(infos, servers)| {
            let handle_ = handle_.clone();
            timer.sleep(Duration::from_secs(probe)).map_err(|err| {
                io::Error::new(io::ErrorKind::Other, err)
            }).and_then(move |_| {
                let tests = infos.clone().into_iter().map(move |info| {
                    alive_test(&**info.server, &handle_)
                        .then(|t| future::ok(t.ok()))
                });
                future::join_all(tests).map(|ts| (ts, infos, servers))
            }).and_then(move |(ts, mut infos, servers)| {
                infos.iter_mut().zip(ts.iter()).for_each(|(info, t)| {
                    info.delay = t.map(|t| {
                        (info.delay.unwrap_or(t + 1000) * 9 + t) / 10
                    });
                });
                let mut rng = rand::thread_rng();
                infos.sort_by_key(move |info| {
                  info.delay.unwrap_or(std::u32::MAX - 50) +
                  (rng.next_u32() % 20)
                });
                servers.set(infos.clone());
                info!("scores: {}", info_stats(infos.as_slice()));
                future::ok((infos, servers))
            }).and_then(|args| Ok(future::Loop::Continue(args)))
        })
    }).map_err(|_: io::Error| ());
    Box::new(update)
}

fn info_stats(infos: &[ServerInfo]) -> String {
    let mut stats = String::new();
    for info in infos.iter().take(5) {
        stats += &match info.delay {
            None => format!(" {}: --,", info.server.tag()),
            Some(t) => format!(" {}: {}ms,", info.server.tag(), t),
        };
    }
    stats.pop();
    stats
}

pub fn alive_test(server: &ProxyServer, handle: &Handle)
        -> Box<Future<Item=u32, Error=io::Error>> {
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
    let tid = (request[2] as u16) << 8 | (request[3] as u16);

    // TODO: reuse timer?
    let timer = Timer::default();
    let now = Instant::now();
    let addr = "8.8.8.8:53".parse().unwrap();
    let tag = String::from(server.tag());
    let conn = server.connect_async(addr, handle);
    let try_conn = timer.timeout(conn, Duration::from_secs(5))
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut,
                                    "handshake timed out"));
    let query = try_conn.and_then(move |stream| {
        write_all(stream, request)
    }).and_then(|(stream, _)| {
        read_exact(stream, [0u8; 12])
    }).and_then(move |(stream, buf)| {
        let resp_tid = (buf[2] as u16) << 8 | (buf[3] as u16);
        if resp_tid == tid {
            let delay = now.elapsed();
            let t = delay.as_secs() as u32 * 1000 +
                    delay.subsec_nanos() / 1_000_000;
            debug!("[{}] delay {}ms", tag, t);
            future::ok(t)
        } else {
            future::err(io::Error::new(io::ErrorKind::Other,
                                       "unknown response"))
        }
    });
    Box::new(query)
}

