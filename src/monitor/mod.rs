mod graphite;
#[cfg(feature = "score_script")]
use rlua::prelude::*;
mod alive_test;
mod traffic;
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
    time::{Duration, SystemTime},
};
#[cfg(feature = "score_script")]
use std::{error::Error, fs::File, io::Read};
use tokio::time::{interval_at, Instant};

pub use self::traffic::Throughput;
use self::{
    graphite::{Graphite, Record},
    traffic::Meter,
};
#[cfg(all(feature = "systemd", target_os = "linux"))]
use crate::linux::systemd;
use crate::proxy::ProxyServer;

static THROUGHPUT_INTERVAL_SECS: u64 = 1;

pub type ServerList = Vec<Arc<ProxyServer>>;

#[derive(Clone)]
pub struct Monitor {
    servers: Arc<Mutex<ServerList>>,
    meters: Arc<Mutex<HashMap<Arc<ProxyServer>, Meter>>>,
    graphite: Option<SocketAddr>,
    #[cfg(feature = "score_script")]
    lua: Option<Arc<Mutex<Lua>>>,
}

impl Monitor {
    pub fn new(servers: Vec<Arc<ProxyServer>>, graphite: Option<SocketAddr>) -> Monitor {
        let meters = servers
            .iter()
            .map(|server| (server.clone(), Meter::new()))
            .collect();
        #[cfg(all(feature = "systemd", target_os = "linux"))]
        systemd::set_status(
            format!(
                "serving ({} upstream {}, state unknown)",
                servers.len(),
                if servers.len() > 1 {
                    "proxies"
                } else {
                    "proxy"
                }
            )
            .into(),
        );
        Monitor {
            servers: Arc::new(Mutex::new(servers)),
            meters: Arc::new(Mutex::new(meters)),
            graphite,
            #[cfg(feature = "score_script")]
            lua: None,
        }
    }

    #[cfg(feature = "score_script")]
    pub fn load_score_script(&mut self, path: &str) -> Result<(), Box<dyn Error>> {
        let mut buf = Vec::new();
        File::open(path)?.take(2u64.pow(26)).read_to_end(&mut buf)?;

        let lua = Lua::new();
        lua.context(|ctx| -> LuaResult<Result<(), &'static str>> {
            let globals = ctx.globals();
            ctx.load(&buf).exec()?;
            if !globals.contains_key("calc_score")? {
                return Ok(Err("calc_score() not found in Lua globals"));
            }
            let _: LuaFunction = match globals.get("calc_score") {
                Err(LuaError::FromLuaConversionError { .. }) => {
                    return Ok(Err("calc_score is not a function"))
                }
                other => other,
            }?;
            Ok(Ok(()))
        })??;

        self.lua.replace(Arc::new(Mutex::new(lua)));
        Ok(())
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

        // Copy config from new server objects to old ones.
        // That also ensure their `status` remain unchange.
        for server in oldset.intersection(&newset) {
            let old = oldset.get(server).unwrap();
            let new = newset.get(server).unwrap();
            old.copy_config_from(new);
            new_servers.push(old.clone());
        }

        // Add brand new server objects
        new_servers.extend(newset.difference(&oldset).cloned());

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
        debug!("scores:{}", info_stats(&servers));
    }

    /// Start monitoring delays.
    /// Returned Future won't return unless error on timer.
    pub async fn monitor_delay(self, probe: u64) {
        let mut graphite = self.graphite.map(Graphite::new);
        let interval = Duration::from_secs(probe);

        alive_test::test_all(&self).await;

        let mut interval = interval_at(Instant::now() + interval, interval);
        loop {
            interval.tick().await;
            alive_test::test_all(&self).await;
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
        let mut interval = interval_at(Instant::now() + interval, interval);
        loop {
            interval.tick().await;
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

fn info_stats(infos: &[Arc<ProxyServer>]) -> String {
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

// send graphite metrics if need
async fn send_metrics(monitor: &Monitor, graphite: &mut Graphite) -> io::Result<()> {
    let records = monitor
        .servers()
        .iter()
        .flat_map(|server| {
            let now = Some(SystemTime::now());
            let r = |path, value| Record::new(server.graphite_path(path), value, now);
            let status = server.status_snapshot();
            let traffic = server.traffic();
            vec![
                status.delay.map(|t| r("delay", t.as_millis() as u64)),
                status.score.map(|s| r("score", s as u64)),
                Some(r("tx_bytes", traffic.tx_bytes as u64)),
                Some(r("rx_bytes", traffic.rx_bytes as u64)),
                Some(r("conns.total", status.conn_total as u64)),
                Some(r("conns.alive", status.conn_alive as u64)),
                Some(r("conns.error", status.conn_error as u64)),
            ]
        })
        .filter_map(|v| v)
        .collect(); // FIXME: avoid allocate large memory
    graphite.write_records(records).await
}
