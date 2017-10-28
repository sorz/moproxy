extern crate rand;
use std;
use std::thread;
use std::time::{Instant, Duration};
use std::sync::{Mutex, Arc};
use std::collections::HashMap;
use std::io::{self, Write, Read};
use self::rand::Rng;
use std::hash::Hash;
use ::proxy::ProxyServer;

pub fn monitoring_servers<S>(servers: Arc<Mutex<Vec<S>>>,
                          probe: u64)
where S: ProxyServer + Hash + Eq + Clone {
    let mut rng = rand::thread_rng();
    let mut avg_delay = HashMap::new();
    for server in servers.lock().unwrap().iter() {
        avg_delay.insert(server.clone(), None);
    }
    for (server, delay) in avg_delay.iter_mut() {
        *delay = alive_test(server).ok();
    }

    loop {
        debug!("probing started");
        for (server, delay) in avg_delay.iter_mut() {
            *delay = match alive_test(server) {
                Ok(t) => {
                    debug!("{} up: {}ms", server.tag(), t);
                    Some((delay.unwrap_or(t + 1000) * 9 + t) / 10)
                },
                Err(e) => {
                    debug!("{} down: {}", server.tag(), e);
                    None
                }
            };
        }

        servers.lock().unwrap().sort_by_key(|s|
            avg_delay.get(s).unwrap().unwrap_or(std::u32::MAX - 50)
            + (rng.next_u32() % 20));

        let mut stats = String::new();
        for s in servers.lock().unwrap().iter().take(5) {
            stats += format!(" {}: {}ms,", s.tag(),
                avg_delay.get(s).unwrap().unwrap_or(0)).as_str();
        }
        stats.pop();
        info!("average delay:{}", stats);

        thread::sleep(Duration::from_secs(probe));
    }
}

pub fn alive_test<T: ProxyServer>(server: &T) -> io::Result<u32> {
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

