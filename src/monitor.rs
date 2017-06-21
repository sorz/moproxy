extern crate rand;
use std;
use std::thread;
use std::net::SocketAddrV4;
use std::time::Duration;
use std::sync::{Mutex, Arc};
use std::collections::HashMap;
use self::rand::Rng;
use ::socks5;

pub fn monitoring_servers(servers: Arc<Mutex<Vec<SocketAddrV4>>>) {
    let mut rng = rand::thread_rng();
    let mut avg_delay = HashMap::new();
    for server in servers.lock().unwrap().iter() {
        avg_delay.insert(server.clone(), None);
    }
    for (server, delay) in avg_delay.iter_mut() {
        *delay = socks5::alive_test(*server).ok();
    }

    loop {
        for (server, delay) in avg_delay.iter_mut() {
            *delay = match socks5::alive_test(*server) {
                Ok(t) => Some((delay.unwrap_or(t + 1000) * 9 + t) / 10),
                Err(_) => None,
            };
        }

        servers.lock().unwrap().sort_by_key(|s|
            avg_delay.get(s).unwrap().unwrap_or(std::u32::MAX - 50)
            + (rng.next_u32() % 20));

        for s in servers.lock().unwrap().iter() {
            print!("{}: {}ms\t", s.port(),
                avg_delay.get(s).unwrap().unwrap_or(0));
        }
        println!();
        thread::sleep(Duration::from_secs(30));
    }
}

