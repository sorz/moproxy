mod cli;
mod server;

use clap::Parser;
use cli::Commands;
use server::MoProxy;
use std::str::FromStr;
#[cfg(unix)]
use tokio::signal::unix::{signal, SignalKind};
use tracing::{debug, error, info, instrument, warn};

#[cfg(all(feature = "systemd", target_os = "linux"))]
use moproxy::linux::systemd;
use tracing_subscriber::prelude::*;

trait FromOptionStr<E, T: FromStr<Err = E>> {
    fn parse(&self) -> Result<Option<T>, E>;
}

impl<E, T, S> FromOptionStr<E, T> for Option<S>
where
    T: FromStr<Err = E>,
    S: AsRef<str>,
{
    fn parse(&self) -> Result<Option<T>, E> {
        if let Some(s) = self {
            let t = T::from_str(s.as_ref())?;
            Ok(Some(t))
        } else {
            Ok(None)
        }
    }
}

#[tokio::main]
async fn main() {
    let mut args = cli::CliArgs::parse();
    let command = args.command.take();
    let mut log_registry: Option<_> = tracing_subscriber::registry().with(args.log_level).into();

    #[cfg(all(feature = "systemd", target_os = "linux"))]
    {
        if systemd::is_stderr_connected_to_journal() {
            match tracing_journald::layer() {
                Ok(layer) => {
                    log_registry.take().unwrap().with(layer).init();
                    debug!("Use native journal protocol");
                }
                Err(err) => eprintln!(
                    "Failed to connect systemd-journald: {}; fallback to STDERR.",
                    err
                ),
            }
        }
    }
    if let Some(registry) = log_registry {
        registry.with(tracing_subscriber::fmt::layer()).init();
    }

    // Init moproxy (read config files, etc.)
    let moproxy = MoProxy::new(args).await.expect("failed to start moproxy");

    // Setup signal listener for reloading server list
    #[cfg(unix)]
    {
        let moproxy = moproxy.clone();
        let mut signals = signal(SignalKind::hangup()).expect("cannot catch signal");
        tokio::spawn(async move {
            while signals.recv().await.is_some() {
                reload_daemon(&moproxy);
            }
        });
    }

    match &command {
        Some(Commands::Check { no_bind }) if *no_bind => {
            info!("Configuration checked");
            return;
        }
        _ => {}
    }

    // Listen on TCP ports
    let listener = moproxy
        .listen()
        .await
        .expect("cannot listen on given TCP port");

    // Watchdog
    #[cfg(all(feature = "systemd", target_os = "linux"))]
    {
        if let Some(timeout) = systemd::watchdog_timeout() {
            tokio::spawn(systemd::watchdog_loop(timeout / 2));
        }
    }

    // Notify systemd
    #[cfg(all(feature = "systemd", target_os = "linux"))]
    systemd::notify_ready();

    match &command {
        None => listener.handle_forever().await,
        Some(Commands::Check { .. }) => info!("Configuration checked"),
    }
}

#[instrument(skip_all)]
fn reload_daemon(moproxy: &MoProxy) {
    #[cfg(all(feature = "systemd", target_os = "linux"))]
    systemd::notify_realoding();

    // feature: check deadlocks on signal
    let deadlocks = parking_lot::deadlock::check_deadlock();
    if !deadlocks.is_empty() {
        error!("{} deadlocks detected!", deadlocks.len());
        for (i, threads) in deadlocks.iter().enumerate() {
            debug!("Deadlock #{}", i);
            for t in threads {
                debug!("Thread Id {:#?}", t.thread_id());
                debug!("{:#?}", t.backtrace());
            }
        }
    }

    // actual reload
    debug!("SIGHUP received, reload server list.");
    if let Err(err) = moproxy.reload() {
        error!("fail to reload servers: {}", err);
    }

    #[cfg(all(feature = "systemd", target_os = "linux"))]
    systemd::notify_ready();
}
