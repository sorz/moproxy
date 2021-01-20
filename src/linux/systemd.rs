use log::{debug, trace, warn};
use sd_notify::{notify, NotifyState};
use std::{env, process, time::Duration};
use tokio::time::sleep;

fn notify_enabled() -> bool {
    env::var_os("NOTIFY_SOCKET").is_some()
}

pub fn notify_ready() {
    if notify_enabled() && notify(false, &[NotifyState::Ready]).is_err() {
        warn!("fail to notify systemd (ready)")
    }
}

pub fn notify_realoding() {
    if notify_enabled() && notify(false, &[NotifyState::Reloading]).is_err() {
        warn!("fail to notify systemd (reloading)")
    }
}

/// Return the watchdog timeout if it's enabled by systemd.
pub fn watchdog_timeout() -> Option<Duration> {
    if !notify_enabled() {
        return None;
    }
    let pid: u32 = env::var("WATCHDOG_PID").ok()?.parse().ok()?;
    if pid != process::id() {
        debug!(
            "WATCHDOG_PID was set to {}, not ours {}",
            pid,
            process::id()
        );
        return None;
    }
    let usec: u64 = env::var("WATCHDOG_USEC").ok()?.parse().ok()?;
    Some(Duration::from_micros(usec))
}

pub async fn watchdog_loop(timeout: Duration) -> ! {
    debug!("start poking watchdog for every {}ms", timeout.as_millis());
    loop {
        trace!("poke the watchdog");
        if notify(false, &[NotifyState::Watchdog]).is_err() {
            warn!("fail to poke watchdog");
        }
        sleep(timeout).await;
    }
}
