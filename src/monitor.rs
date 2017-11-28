extern crate rand;
use std;
use std::time::{Instant, Duration};
use std::io;
use std::sync::{Mutex, Arc, MutexGuard};
use self::rand::Rng;
use ::tokio_timer::Timer;
use ::tokio_core::reactor::Handle;
use ::tokio_io::io::{write_all, read_exact};
use ::futures::{future, Future};
use ::proxy::ProxyServer;


#[derive(Clone)]
pub struct ServerInfo {
    pub server: Arc<ProxyServer>,
    pub delay: Option<Duration>,
    pub score: Option<u32>,
}

pub struct ServerList {
    inner: Mutex<Vec<ServerInfo>>,
}

impl ServerList {
    pub fn new(servers: Vec<ProxyServer>) -> ServerList {
        let mut infos: Vec<ServerInfo> = vec![];
        for s in servers {
            let info = ServerInfo {
                server: Arc::new(s),
                delay: None,
                score: None,
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

pub fn monitoring_servers(servers: Arc<ServerList>, probe: u64, handle: Handle)
        -> Box<Future<Item=(), Error=()>> {
    let init = test_all(&servers, &handle).and_then(move |delays| {
        let mut infos = servers.get().clone();
        infos.iter_mut().zip(delays.iter()).for_each(|(info, t)| {
            info.delay = *t;
            info.score = t.map(to_ms);
        });
        infos.sort_by_key(|info| info.score.unwrap_or(std::u32::MAX));
        info!("scores:{}", info_stats(infos.as_slice()));
        servers.set(infos.clone());
        Ok(servers)
    });
    let timer = Timer::default();
    let interval = Duration::from_secs(probe);
    let update = init.and_then(move |servers| {
        future::loop_fn((servers, handle), move |(servers, handle)| {
            timer.sleep(interval).map_err(|err| {
                io::Error::new(io::ErrorKind::Other, err)
            }).and_then(move |_| {
                debug!("testing all servers...");
                test_all(&servers, &handle)
                    .map(|ts| (ts, servers, handle))
            }).and_then(|(ts, servers, handle)| {
                let mut infos = servers.get().clone();
                infos.iter_mut().zip(ts.iter()).for_each(|(info, t)| {
                    info.delay = *t;
                    info.score = t.map(to_ms).map(|t| {
                        (info.score.unwrap_or(t + 1000) * 9 + t) / 10
                    });
                });
                let mut rng = rand::thread_rng();
                infos.sort_by_key(move |info| {
                    info.score.unwrap_or(std::u32::MAX - 50) +
                    (rng.next_u32() % 20)
                });
                info!("scores:{}", info_stats(infos.as_slice()));
                servers.set(infos);
                Ok((servers, handle))
            }).and_then(|args| Ok(future::Loop::Continue(args)))
        })
    }).map_err(|_| ());
    Box::new(update)
}

fn info_stats(infos: &[ServerInfo]) -> String {
    let mut stats = String::new();
    for info in infos.iter().take(5) {
        stats += &match info.score {
            None => format!(" {}: --,", info.server.tag),
            Some(t) => format!(" {}: {},", info.server.tag, t),
        };
    }
    stats.pop();
    stats
}

fn test_all(servers: &ServerList, handle: &Handle)
        -> Box<Future<Item=Vec<Option<Duration>>, Error=io::Error>> {
    let tests: Vec<_> = servers.get().clone().into_iter().map(move |info| {
        alive_test(&*info.server, handle).then(|t| Ok(t.ok()))
    }).collect();
    Box::new(future::join_all(tests))
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

    // TODO: reuse timer?
    let timer = Timer::default();
    let now = Instant::now();
    let addr = "8.8.8.8:53".parse().unwrap();
    let tag = server.tag.clone();
    let conn = server.connect(addr, handle);
    let try_conn = timer.timeout(conn, Duration::from_secs(5));
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

fn to_ms(t: Duration) -> u32 {
    t.as_secs() as u32 * 1000 + t.subsec_nanos() / 1_000_000
}

