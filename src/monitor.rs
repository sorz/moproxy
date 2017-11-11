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
    let mut rng = rand::thread_rng();
    let tests = servers.get().clone().into_iter().map(move |info| {
        // TODO: timeout
        alive_test(&**info.server, &handle)
            .then(|t| future::ok(t.ok()))
    });
    let servers_ = servers.clone();
    let init = future::join_all(tests).and_then(move |delays| {
        info!("prob init done");
        let mut infos = servers_.get().clone();
        infos.iter_mut().zip(delays.iter()).for_each(|(info, t)| {
            info.delay = *t;
        });
        infos.sort_by_key(|info|
              info.delay.unwrap_or(std::u32::MAX - 50) +
              (rng.next_u32() % 20));
        servers_.set(infos.clone());
        future::ok(infos)
    }).map(|_| ());
    Box::new(init)

    /*
    let infos = servers.get().clone();
    let udpate = iter_ok(infos).for_each(|info| {
        alive_test(info.server.clone()).then(move |delay| {
            info.delay = match delay {
                Ok(t) => Some((info.delay.unwrap_or(t + 1000) * 9 + t) / 10),
                Err(e) => None,
            };
            future::ok(())
        })
    }).and_then(|_| {

    let timer = Timer::default();
    let forever = future::loop_fn(servers.clone(), move |servers| {
        timer.sleep(Duration::from_secs(probe)).and_then(|_| {
            
            
        })
    });

    loop {
        debug!("probing started");

        infos.sort_by_key(|info|
              info.delay.unwrap_or(std::u32::MAX - 50) +
              (rng.next_u32() % 20));
        servers.set(infos.clone());

        let mut stats = String::new();
        for info in infos.iter().take(5) {
            stats += &match info.delay {
                None => format!(" {}: --,", info.server.tag()),
                Some(t) => format!(" {}: {}ms,", info.server.tag(), t),
            };
        }
        stats.pop();
        info!("average delay:{}", stats);

        thread::sleep(Duration::from_secs(probe));
    }*/
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
            future::ok(delay.as_secs() as u32 * 1000 +
                       delay.subsec_nanos() / 1_000_000)
        } else {
            future::err(io::Error::new(io::ErrorKind::Other,
                                       "unknown response"))
        }
    });
    Box::new(query)
}

