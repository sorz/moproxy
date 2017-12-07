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


#[derive(Clone, Debug, Default, Serialize)]
pub struct ServerInfo {
    pub idx: usize,
    pub delay: Option<Duration>,
    pub score: Option<i32>,
    pub tx_bytes: u64,
    pub rx_bytes: u64,
    pub conn_alive: u32,
    pub conn_total: u32,
}

pub struct ServerList {
    pub servers: Vec<ProxyServer>,
    infos: Mutex<Vec<ServerInfo>>,
}

impl ServerList {
    pub fn new(servers: Vec<ProxyServer>) -> ServerList {
        let infos = (0..servers.len()).map(|idx|
            ServerInfo {
                idx: idx,
                ..Default::default()
            }).collect();
        ServerList {
            servers: servers,
            infos: Mutex::new(infos),
        }
    }

    pub fn get_infos(&self) -> MutexGuard<Vec<ServerInfo>> {
        self.infos.lock().unwrap()
    }

    /// Update delays and scores, and resort server list.
    ///
    /// `delays` must have the same order of inner server list.
    /// `penalty` would be added to score when the last score is `None`.
    fn update_delay(&self, delays: Vec<Option<Duration>>, penalty: i32) {
        let mut infos = self.get_infos();
        infos.iter_mut().for_each(|info| {
            let server = &self.servers[info.idx];
            info.delay = delays[info.idx];
            info.score = info.delay.map(|t| to_ms(t) + server.score_base)
                .map(|new| {
                    let old = info.score.unwrap_or(new + penalty);
                    // give more weight to delays exceed the mean, to
                    // punish for network jitter.
                    if new < old {
                        (old * 9 + new * 1) / 10
                    } else {
                        (old * 8 + new * 2) / 10
                    }
                });
        });
        let mut rng = rand::thread_rng();
        infos.sort_by_key(move |info| {
            info.score.unwrap_or(std::i32::MAX) -
            (rng.next_u32() % 30) as i32
        });
        info!("scores:{}", info_stats(&self.servers, &*infos));
    }

    pub fn update_stats_conn_open(&self, idx: usize) {
        self.get_infos().iter_mut().find(|ref i| i.idx == idx)
            .map(|ref mut info| {
                info.conn_alive += 1;
                info.conn_total += 1;
        });
    }

    pub fn update_stats_conn_close(&self, idx: usize, tx: u64, rx: u64) {
        self.get_infos().iter_mut().find(|ref i| i.idx == idx)
            .map(|ref mut info| {
                info.conn_alive -= 1;
                info.tx_bytes += tx;
                info.rx_bytes += rx;
        });
    }
}

pub fn monitoring_servers(servers: Arc<ServerList>, probe: u64,
                          handle: Handle)
        -> Box<Future<Item=(), Error=()>> {
    let timer = Timer::default();
    let init = test_all(&servers, &handle, &timer).and_then(move |delays| {
        servers.update_delay(delays, 0);
        Ok(servers)
    });
    let interval = Duration::from_secs(probe);
    let update = init.and_then(move |servers| {
        future::loop_fn((servers, (handle, timer)),
                  move |(servers, (handle, timer))| {
            timer.sleep(interval).map_err(|err| {
                io::Error::new(io::ErrorKind::Other, err)
            }).and_then(move |_| {
                test_all(&servers, &handle, &timer)
                    .map(|ts| (ts, servers, (handle, timer)))
            }).and_then(|(ts, servers, args)| {
                servers.update_delay(ts, 2000);
                Ok((servers, args))
            }).and_then(|args| Ok(future::Loop::Continue(args)))
        })
    }).map_err(|_| ());
    Box::new(update)
}

fn info_stats(servers: &[ProxyServer], infos: &[ServerInfo]) -> String {
    let mut stats = String::new();
    for info in infos.iter().take(5) {
        stats += &match info.score {
            None => format!(" {}: --,", servers[info.idx].tag),
            Some(t) => format!(" {}: {},", servers[info.idx].tag, t),
        };
    }
    stats.pop();
    stats
}

fn test_all(list: &ServerList, handle: &Handle, timer: &Timer)
        -> Box<Future<Item=Vec<Option<Duration>>, Error=io::Error>> {
    debug!("testing all servers...");
    let tests: Vec<_> = list.servers.clone().into_iter().map(move |server| {
        alive_test(&server, handle, timer).then(|t| Ok(t.ok()))
    }).collect();
    Box::new(future::join_all(tests))
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
    let conn = server.connect(server.test_dns, handle);
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

fn to_ms(t: Duration) -> i32 {
    (t.as_secs() as u32 * 1000 + t.subsec_nanos() / 1_000_000) as i32
}

