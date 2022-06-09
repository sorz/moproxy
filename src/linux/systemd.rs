use libc::{dev_t as Dev, ino_t as Inode};
use nix::sys::stat::fstat;
use sd_notify::{notify, NotifyState};
use std::{borrow::Cow, env, io, os::unix::prelude::AsRawFd, process, time::Duration};
use tokio::time::sleep;
use tracing::{info, instrument, trace, warn};

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

pub fn set_status(status: Cow<str>) {
    if notify_enabled() && notify(false, &[NotifyState::Status(&status)]).is_err() {
        warn!("fail to notify systemd (set status)");
    }
}

/// Return the watchdog timeout if it's enabled by systemd.
pub fn watchdog_timeout() -> Option<Duration> {
    if !notify_enabled() {
        return None;
    }
    let pid: u32 = env::var("WATCHDOG_PID").ok()?.parse().ok()?;
    if pid != process::id() {
        info!(
            "WATCHDOG_PID was set to {}, not ours {}",
            pid,
            process::id()
        );
        return None;
    }
    let usec: u64 = env::var("WATCHDOG_USEC").ok()?.parse().ok()?;
    Some(Duration::from_micros(usec))
}

#[instrument(skip_all)]
pub async fn watchdog_loop(timeout: Duration) -> ! {
    info!("Watchdog enabled, poke for every {}ms", timeout.as_millis());
    loop {
        trace!("poke the watchdog");
        if notify(false, &[NotifyState::Watchdog]).is_err() {
            warn!("fail to poke watchdog");
        }
        sleep(timeout).await;
    }
}

/// Try to read the device & inode number from environment variable `JOURNAL_STREAM`.
fn get_journal_stream_dev_ino() -> Option<(Dev, Inode)> {
    let stream_env = env::var_os("JOURNAL_STREAM")?;
    let (dev, ino) = stream_env.to_str()?.split_once(':')?;
    Some((dev.parse().ok()?, ino.parse().ok()?))
}

/// Check if STDERR is connected with systemd's journal service.
pub fn is_stderr_connected_to_journal() -> bool {
    if let Some((dev, ino)) = get_journal_stream_dev_ino() {
        if let Ok(stat) = fstat(io::stderr().as_raw_fd()) {
            return stat.st_dev == dev && stat.st_ino == ino;
        }
    }
    false
}
