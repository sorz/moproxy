use futures::{Async, Future as Future01};
use log::info;
use std::{
    cmp,
    collections::VecDeque,
    future::Future,
    io::{self, ErrorKind},
    iter::FromIterator,
    pin::Pin,
    sync::Arc,
    task::{Poll, Context},
};
use tokio_core::net::TcpStream;
use tokio_core::reactor::{Handle, Timeout};

use crate::proxy::{Connect, Destination, ProxyServer};
use crate::RcBox;

struct TryConnect {
    server: Arc<ProxyServer>,
    state: TryConnectState,
    timer: Timeout,
}

enum TryConnectState {
    Connecting {
        connect: Box<Connect>,
        wait_response: bool,
    },
    Waiting {
        conn: Option<TcpStream>,
    },
}

fn try_connect(
    dest: &Destination,
    server: Arc<ProxyServer>,
    pending_data: Option<RcBox<[u8]>>,
    wait_response: bool,
    handle: &Handle,
) -> TryConnect {
    let state = TryConnectState::Connecting {
        connect: server.connect(dest.clone(), pending_data, &handle),
        wait_response,
    };
    let timer = Timeout::new(server.max_wait, &handle).expect("error on get timeout from reactor");
    TryConnect {
        server,
        state,
        timer,
    }
}

impl Future for TryConnect {
    type Output = io::Result<(Arc<ProxyServer>, TcpStream)>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context) 
            -> Poll<io::Result<(Arc<ProxyServer>, TcpStream)>> {
        if self.timer.poll()?.is_ready() {
            return Poll::Ready(Err(io::Error::new(
                    ErrorKind::TimedOut, "connect timeout")));
        }
        let new_state = match self.state {
            // waiting for proxy server connected
            TryConnectState::Connecting {
                ref mut connect,
                wait_response,
            } => {
                let conn = match connect.poll() {
                    Ok(Async::NotReady) => return Poll::Pending,
                    Err(e) => return Poll::Ready(Err(e)),
                    Ok(Async::Ready(v)) => v,
                };
                if !wait_response || conn.poll_read().is_ready() {
                    return Poll::Ready(Ok((self.server.clone(), conn)));
                } else {
                    Some(TryConnectState::Waiting { conn: Some(conn) })
                }
            }
            // waiting for response data
            TryConnectState::Waiting { ref mut conn } => {
                if conn.as_mut().unwrap().poll_read().is_ready() {
                    let conn = conn.take().unwrap();
                    let mut buf = [0; 8];
                    let len = conn.peek(&mut buf)?;
                    return Poll::Ready(
                        if len == 0 {
                            Err(io::Error::new(
                                ErrorKind::UnexpectedEof,
                                "not response data",
                            ))
                        } else {
                            Ok((self.server.clone(), conn))
                        }
                    );
                }
                None
            }
        };
        if let Some(state) = new_state {
            self.state = state;
        }
        Poll::Pending
    }
}

/// Try to connect one of the proxy servers.
/// Pick `parallel_n` servers from `queue` to `connecting` and wait for
/// connect. Once any of them connected, move that to `reading` and wait
/// for read respone. Once any of handshakings done, return it and cancel
/// others.
pub struct TryConnectAll {
    dest: Destination,
    pending_data: Option<RcBox<[u8]>>,
    parallel_n: usize,
    wait_response: bool,
    standby: VecDeque<Arc<ProxyServer>>,
    connects: VecDeque<Pin<Box<TryConnect>>>,
    handle: Handle,
}

pub fn try_connect_all(
    dest: Destination,
    servers: Vec<Arc<ProxyServer>>,
    parallel_n: usize,
    wait_response: bool,
    pending_data: Option<RcBox<[u8]>>,
    handle: Handle,
) -> TryConnectAll {
    let parallel_n = cmp::max(1, parallel_n);
    let servers = VecDeque::from_iter(servers.into_iter());
    TryConnectAll {
        dest,
        parallel_n,
        pending_data,
        wait_response,
        handle,
        standby: servers,
        connects: VecDeque::with_capacity(parallel_n),
    }
}

impl Future for TryConnectAll {
    type Output = Option<(Arc<ProxyServer>, TcpStream)>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context)
            -> Poll<Option<(Arc<ProxyServer>, TcpStream)>> {
        loop {
            // if current connections less than parallel_n,
            // pick servers from queue to connect.
            while !self.standby.is_empty() && self.connects.len() < self.parallel_n {
                let server = self.standby.pop_front().unwrap();
                let data = self.pending_data.clone();
                let conn = try_connect(&self.dest, server, data, self.wait_response, &self.handle);
                self.connects.push_back(Box::pin(conn));
            }

            // poll all connects
            let mut i = 0;
            while i < self.connects.len() {
                let mut conn = self.connects[i];
                match conn.as_mut().poll(&mut cx) {
                    // error, stop trying, drop it.
                    Poll::Ready(Err(e)) => {
                        info!("connect proxy error: {}", e);
                        drop(self.connects.remove(i));
                    }
                    // not ready, keep here, poll next one.
                    Poll::Pending => i += 1,
                    // ready, return it.
                    Poll::Ready(Ok(item)) => return Poll::Ready(Some(item)),
                }
            }

            // if all servers failed, return error
            if self.connects.is_empty() && self.standby.is_empty() {
                return Poll::Ready(None)
            }

            // if not need to connect standby server, wait for events.
            if self.connects.len() >= self.parallel_n || self.standby.is_empty() {
                return Poll::Pending;
            }
        }
    }
}
