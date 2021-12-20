use futures_util::future::join_all;
use std::{self, io, net::Shutdown, time::Duration};
#[cfg(all(feature = "systemd", target_os = "linux"))]
use std::{
    fmt,
    sync::atomic::{AtomicUsize, Ordering},
};
use tokio::{
    io::AsyncReadExt,
    time::{timeout, Instant},
};
use tracing::{debug, instrument, warn};

use super::Monitor;
#[cfg(all(feature = "systemd", target_os = "linux"))]
use crate::linux::systemd;
use crate::proxy::ProxyServer;

#[cfg(all(feature = "systemd", target_os = "linux"))]
struct TestProgress {
    total: usize,
    pass: AtomicUsize,
    fail: AtomicUsize,
}

#[cfg(all(feature = "systemd", target_os = "linux"))]
impl TestProgress {
    fn new(total: usize) -> Self {
        Self {
            total,
            pass: AtomicUsize::new(0),
            fail: AtomicUsize::new(0),
        }
    }

    fn increase(&self, passed: bool) {
        let counter = if passed { &self.pass } else { &self.fail };
        counter.fetch_add(1, Ordering::Relaxed);
        systemd::set_status(format!("{}", self).into());
    }
}

#[cfg(all(feature = "systemd", target_os = "linux"))]
impl fmt::Display for TestProgress {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        let pass = self.pass.load(Ordering::Relaxed);
        let fail = self.fail.load(Ordering::Relaxed);
        if (pass + fail) < self.total {
            write!(fmt, "probing ({}/{} done)", pass + fail, self.total)
        } else {
            write!(
                fmt,
                "serving ({}/{} upstream {} up)",
                pass,
                self.total,
                if pass > 1 { "proxies" } else { "proxy" }
            )
        }
    }
}

#[instrument(level = "debug", skip_all)]
pub(crate) async fn test_all(monitor: &Monitor) {
    debug!("Start testing all servers");
    #[cfg(all(feature = "systemd", target_os = "linux"))]
    let progress = TestProgress::new(monitor.servers().len());
    #[cfg(all(feature = "systemd", target_os = "linux"))]
    let progress_ref = &progress;
    let tests: Vec<_> = monitor
        .servers()
        .into_iter()
        .map(move |server| {
            Box::pin(async move {
                let delay = alive_test(&server).await.ok();

                #[cfg(all(feature = "systemd", target_os = "linux"))]
                progress_ref.increase(delay.is_some());

                #[cfg(feature = "score_script")]
                {
                    let mut caculated = false;
                    if let Some(lua) = &monitor.lua {
                        match lua
                            .lock()
                            .context(|ctx| server.update_delay_with_lua(delay, ctx))
                        {
                            Ok(()) => caculated = true,
                            Err(err) => warn!("fail to update score w/ Lua script: {}", err),
                        }
                    }
                    if !caculated {
                        server.update_delay(delay);
                    }
                }
                #[cfg(not(feature = "score_script"))]
                server.update_delay(delay);
            })
        })
        .collect();

    join_all(tests).await;
    monitor.resort();
}

#[instrument(level = "debug", skip_all, fields(proxy = %server.tag))]
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
    let test_dns = server.test_dns().into();
    let result = timeout(server.max_wait(), async {
        let mut stream = server.connect(&test_dns, Some(request)).await?;
        stream.read_exact(&mut buf).await?;
        stream.into_std()?.shutdown(Shutdown::Both)
    })
    .await;

    match result {
        Err(_) => return Err(io::Error::new(io::ErrorKind::TimedOut, "test timeout")),
        Ok(Err(e)) => return Err(e),
        Ok(Ok(_)) => (),
    }

    if req_tid == tid(&buf) {
        let t = now.elapsed();
        debug!("{}ms", t.as_millis());
        Ok(t)
    } else {
        Err(io::Error::new(io::ErrorKind::Other, "unknown response"))
    }
}
