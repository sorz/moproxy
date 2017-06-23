extern crate rand;
use std;
use std::thread;
use std::net::SocketAddrV4;
use std::time::Duration;
use std::sync::{Mutex, Arc};
use std::collections::HashMap;
use self::rand::Rng;
use ::socks5;

pub fn monitoring_servers(servers: Arc<Mutex<Vec<SocketAddrV4>>>,
                          probe: u64) {
    let mut rng = rand::thread_rng();
    let mut avg_delay = HashMap::new();
    for server in servers.lock().unwrap().iter() {
        avg_delay.insert(server.clone(), None);
    }
    for (server, delay) in avg_delay.iter_mut() {
        *delay = socks5::alive_test(*server).ok();
    }

    loop {
        debug!("probing started");
        for (server, delay) in avg_delay.iter_mut() {
            *delay = match socks5::alive_test(*server) {
                Ok(t) => {
                    debug!("{} up: {}ms", server, t);
                    Some((delay.unwrap_or(t + 1000) * 9 + t) / 10)
                },
                Err(e) => {
                    debug!("{} down: {}", server, e);
                    None
                }
            };
        }

        servers.lock().unwrap().sort_by_key(|s|
            avg_delay.get(s).unwrap().unwrap_or(std::u32::MAX - 50)
            + (rng.next_u32() % 20));

        let mut stats = String::new();
        for s in servers.lock().unwrap().iter().take(5) {
            stats += format!(" {}: {}ms,", s.port(),
                avg_delay.get(s).unwrap().unwrap_or(0)).as_str();
        }
        stats.pop();
        info!("average delay:{}", stats);

        thread::sleep(Duration::from_secs(probe));
    }
}

