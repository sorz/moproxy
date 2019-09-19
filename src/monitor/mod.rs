mod graphite;
mod traffic;
use futures::future::join_all;
use log::{debug, warn};
use parking_lot::Mutex;
use rand::{self, Rng};
use std::{
    self,
    collections::{HashMap, HashSet},
    io,
    iter::FromIterator,
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};
use tokio::{future::FutureExt, io::AsyncReadExt, timer::Interval};

use self::graphite::{Graphite, Record};
use self::traffic::Meter;
pub use self::traffic::Throughput;
use crate::{proxy::ProxyServer, ToMillis};

static THROUGHPUT_INTERVAL_SECS: u64 = 1;

pub type ServerList = Vec<Arc<ProxyServer>>;

#[derive(Clone, Debug)]
pub struct Monitor {
    servers: Arc<Mutex<ServerList>>,
    meters: Arc<Mutex<HashMap<Arc<ProxyServer>, Meter>>>,
    graphite: Option<SocketAddr>,
}

impl Monitor {
    pub fn new(servers: Vec<Arc<ProxyServer>>, graphite: Option<SocketAddr>) -> Monitor {
        let meters = servers
            .iter()
            .map(|server| (server.clone(), Meter::new()))
            .collect();
        Monitor {
            servers: Arc::new(Mutex::new(servers)),
            meters: Arc::new(Mutex::new(meters)),
            graphite,
        }
    }

    /// Return an ordered list of servers.
    pub fn servers(&self) -> ServerList {
        self.servers.lock().clone()
    }

    /// Replace internal servers with provided list.
    pub fn update_servers(&self, new_servers: Vec<Arc<ProxyServer>>) {
        let oldset: HashSet<_> = self.servers().into_iter().collect();
        let newset = HashSet::from_iter(new_servers);
        let mut new_servers = Vec::with_capacity(newset.len());

        // Use old server object if unchange
        for server in oldset.intersection(&newset) {
            new_servers.push(oldset.get(server).unwrap().clone());
        }

        // Use new server object and try to copy status from old ones.
        let oldmap: HashMap<_, _> = oldset.difference(&newset)
            .map(|s| ((&s.tag, &s.proto, &s.addr), s))
            .collect();
        for new in newset.difference(&oldset) {
            if let Some(old) = oldmap.get(&(&new.tag, &new.proto, &new.addr)) {
                new.copy_status_from(old);
                new_servers.push(new.clone());
            }
        }

        // Create new meters
        let mut meters = self.meters.lock();
        meters.clear();
        for server in new_servers.iter() {
            meters.insert(server.clone(), Meter::new());
        }

        *self.servers.lock() = new_servers;
        self.resort();
    }

    fn resort(&self) {
        let mut rng = rand::thread_rng();
        let mut servers = self.servers.lock();
        servers.sort_by_key(move |server| {
            server.score().unwrap_or(std::i32::MAX) - (rng.gen::<u8>() % 30) as i32
        });
        debug!("scores:{}", info_stats(&*self.servers.lock()));
    }

    /// Start monitoring delays.
    /// Returned Future won't return unless error on timer.
    pub async fn monitor_delay(self, probe: u64) {
        let mut graphite = self.graphite.map(Graphite::new);
        let interval = Duration::from_secs(probe);

        test_all(&self).await;

        let mut interval = Interval::new_interval(interval);
        loop {
            interval.next().await;
            test_all(&self).await;
            if let Some(ref mut graphite) = graphite {
                match send_metrics(&self, graphite).await {
                    Ok(_) => debug!("metrics sent"),
                    Err(e) => warn!("fail to send metrics {:?}", e),
                }
            }
        }
    }

    /// Start monitoring throughput.
    /// Returned Future won't return unless error on timer.
    pub async fn monitor_throughput(self) {
        let interval = Duration::from_secs(THROUGHPUT_INTERVAL_SECS);
        let mut interval = Interval::new_interval(interval);
        loop {
            interval.next().await;
            for (server, meter) in self.meters.lock().iter_mut() {
                meter.add_sample(server.traffic());
            }
        }
    }

    /// Return average throughputs of all servers in the recent monitor
    /// period. Should start `monitor_throughput()` task before call this.
    pub fn throughputs(&self) -> HashMap<Arc<ProxyServer>, Throughput> {
        self.meters
            .lock()
            .iter()
            .map(|(server, meter)| (server.clone(), meter.throughput(server.traffic())))
            .collect()
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

async fn test_all(monitor: &Monitor) {
    debug!("testing all servers...");
    let tests: Vec<_> = monitor
        .servers()
        .into_iter()
        .map(move |server| {
            Box::pin(async move {
                server.update_delay(alive_test(&server).await.ok());
            })
        })
        .collect();

    join_all(tests).await;
    monitor.resort();
}

// send graphite metrics if need
async fn send_metrics(monitor: &Monitor, graphite: &mut Graphite) -> io::Result<()> {
    let records = monitor
        .servers()
        .iter()
        .flat_map(|server| {
            let now = Some(SystemTime::now());
            let r = |path, value| Record::new(server.graphite_path(path), value, now);
            let status = server.status_snapshot();
            vec![
                status.delay.map(|t| r("delay", t.millis() as u64)),
                status.score.map(|s| r("score", s as u64)),
                Some(r("tx_bytes", status.traffic.tx_bytes as u64)),
                Some(r("rx_bytes", status.traffic.rx_bytes as u64)),
                Some(r("conns.total", status.conn_total as u64)),
                Some(r("conns.alive", status.conn_alive as u64)),
                Some(r("conns.error", status.conn_error as u64)),
            ]
        })
        .filter_map(|v| v)
        .collect(); // FIXME: avoid allocate large memory
    graphite.write_records(records).await
}

async fn alive_test(server: &ProxyServer) -> io::Result<Duration> {
    let request = [
        0,
        17, // length
        rand::random(),
        rand::random(), // transaction ID
        1,
        32, // standard query
        0,
        1, // one query
        0,
        0, // answer
        0,
        0, // authority
        0,
        0, // addition
        0, // query: root
        0,
        1, // query: type A
        0,
        1, // query: class IN
    ];
    let tid = |req: &[u8]| (req[2] as u16) << 8 | (req[3] as u16);
    let req_tid = tid(&request);
    let now = Instant::now();

    let mut buf = [0u8; 12];
    let test_dns = server.test_dns.into();
    let result = async {
        let mut stream = server.connect(&test_dns, Some(request)).await?;
        stream.read_exact(&mut buf).await?;
        Ok(())
    }
        .timeout(server.max_wait)
        .await;

    match result {
        Err(_) => return Err(io::Error::new(io::ErrorKind::TimedOut, "test timeout")),
        Ok(Err(e)) => return Err(e),
        Ok(Ok(_)) => (),
    }

    if req_tid == tid(&buf) {
        let t = now.elapsed();
        debug!("[{}] delay {}ms", server.tag, t.millis());
        Ok(t)
    } else {
        Err(io::Error::new(io::ErrorKind::Other, "unknown response"))
    }
}
