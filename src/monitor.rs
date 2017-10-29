extern crate rand;
use std;
use std::thread;
use std::time::{Instant, Duration};
use std::sync::{Mutex, Arc, MutexGuard};
use std::io::{self, Write, Read};
use self::rand::Rng;
use ::proxy::ProxyServer;


#[derive(Clone)]
pub struct ServerInfo<'a> {
    pub server: Arc<Box<ProxyServer + 'a>>,
    pub delay: Option<u32>,
}

pub struct ServerList<'a> {
    inner: Mutex<Vec<ServerInfo<'a>>>,
}

impl<'a> ServerList<'a> {
    pub fn new(servers: Vec<Box<ProxyServer>>) -> ServerList<'a> {
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

    pub fn get(&self) -> MutexGuard<Vec<ServerInfo<'a>>> {
        self.inner.lock().unwrap()
    }

    fn set(&self, infos: Vec<ServerInfo<'a>>) {
        *self.inner.lock().unwrap() = infos;
    }
}

pub fn monitoring_servers(servers: Arc<ServerList>, probe: u64) {
    let mut rng = rand::thread_rng();
    let mut infos = servers.get().clone();
    for info in infos.iter_mut() {
        info.delay = alive_test(&**info.server).ok();
    }

    loop {
        debug!("probing started");
        for info in infos.iter_mut() {
            info.delay = match alive_test(&**info.server) {
                Ok(t) => {
                    debug!("{} up: {}ms", info.server.tag(), t);
                    Some((info.delay.unwrap_or(t + 1000) * 9 + t) / 10)
                },
                Err(e) => {
                    debug!("{} down: {}", info.server.tag(), e);
                    None
                }
            };
        }

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
    }
}

pub fn alive_test(server: &ProxyServer) -> io::Result<u32> {
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

    let mut socks = server.connect("8.8.8.8:53".parse().unwrap())?;
    socks.set_read_timeout(Some(Duration::from_secs(5)))?;
    socks.set_write_timeout(Some(Duration::from_secs(5)))?;

    socks.write_all(&request)?;

    let mut buffer = [0; 128];
    let now = Instant::now();
    let n = socks.read(&mut buffer)?;
    if n > 4 && buffer[2..3] == request[2..3] {
        let delay = now.elapsed();
        Ok(delay.as_secs() as u32 * 1000 + delay.subsec_nanos() / 1_000_000)
    } else {
        Err(io::Error::new(io::ErrorKind::Other, "unknown response"))
    }
}

