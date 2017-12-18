use std::cmp;
use std::rc::Rc;
use std::io::{self, Write, ErrorKind};
use std::iter::FromIterator;
use std::collections::VecDeque;
use tokio_core::net::TcpStream;
use tokio_core::reactor::{Handle, Timeout};
use futures::{Future, Poll, Async};
use proxy::{ProxyServer, Destination, Connect};
use client::RcBox;

struct TryConnect {
    server: Rc<ProxyServer>,
    state: TryConnectState,
    timer: Timeout,
}

enum TryConnectState {
    Connecting {
        connect: Box<Connect>,
        pending_data: Option<RcBox<[u8]>>,
        wait_response: bool,
    },
    Waiting {
        conn: Option<TcpStream>,
    },
}

fn try_connect(dest: &Destination, server: Rc<ProxyServer>,
               pending_data: Option<RcBox<[u8]>>, wait_response: bool,
               handle: &Handle) -> TryConnect {
    let state = TryConnectState::Connecting {
        connect: server.connect(dest.clone(), &handle),
        wait_response, pending_data,
    };
    let timer = Timeout::new(server.max_wait, &handle)
        .expect("error on get timeout from reactor");
    TryConnect { server, state, timer }
}

impl Future for TryConnect {
    type Item = (Rc<ProxyServer>, TcpStream);
    type Error = io::Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        if self.timer.poll()?.is_ready() {
            return Err(io::Error::new(ErrorKind::TimedOut, "connect timeout"));
        }
        let new_state = match self.state {
            // waiting for proxy server connected
            TryConnectState::Connecting { ref mut connect, ref pending_data,
                                          wait_response } => {
                let mut conn = try_ready!(connect.poll());
                if let Some(ref data) = *pending_data {
                    // we need write out all pending data, which should
                    // be small enough to fit into the buffer without
                    // blocking.
                    let n = conn.write(data.as_ref())?;
                    if n < data.as_ref().len() {
                        return Err(io::Error::new(ErrorKind::WouldBlock,
                                                  "partially write"));
                    }
                }
                if !wait_response || conn.poll_read().is_ready() {
                    return Ok((self.server.clone(), conn).into());
                } else {
                    Some(TryConnectState::Waiting {
                        conn: Some(conn),
                    })
                }
            },
            // waiting for response data
            TryConnectState::Waiting { ref mut conn } => {
                if conn.as_mut().unwrap().poll_read().is_ready() {
                    let conn = conn.take().unwrap();
                    return Ok((self.server.clone(), conn).into());
                }
                None
            },
        };
        if let Some(state) = new_state {
            self.state = state;
        }
        Ok(Async::NotReady)
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
    standby: VecDeque<Rc<ProxyServer>>,
    connects: VecDeque<TryConnect>,
    handle: Handle,
}

pub fn try_connect_all(dest: Destination, servers: Vec<Rc<ProxyServer>>,
                   parallel_n: usize, wait_response: bool,
                   pending_data: Option<RcBox<[u8]>>, handle: Handle)
        -> TryConnectAll {
    let parallel_n = cmp::max(1, parallel_n);
    let servers = VecDeque::from_iter(servers.into_iter());
    TryConnectAll {
        dest, parallel_n, pending_data, wait_response, handle,
        standby: servers,
        connects: VecDeque::with_capacity(parallel_n),
    }
}

impl Future for TryConnectAll {
    type Item = (Rc<ProxyServer>, TcpStream);
    type Error = ();

    fn poll(&mut self) -> Poll<Self::Item, ()> {
        loop {
            // if current connections less than parallel_n,
            // pick servers from queue to connect.
            while !self.standby.is_empty() &&
                    self.connects.len() < self.parallel_n {
                let server = self.standby.pop_front().unwrap();
                let data = self.pending_data.clone();
                let conn = try_connect(&self.dest, server, data,
                                       self.wait_response, &self.handle);
                self.connects.push_back(conn);
            }

            // poll all connects
            let mut i = 0;
            while i < self.connects.len() {
                let result = self.connects[i].poll();
                match result {
                    // error, stop trying, drop it.
                    Err(e) => {
                        info!("connect proxy error: {}", e);
                        drop(self.connects.remove(i));
                    },
                    // not ready, keep here, poll next one.
                    Ok(Async::NotReady) => i += 1,
                    // ready, return it.
                    Ok(Async::Ready(item)) => return Ok(item.into()),
                }
            }

            // if all servers failed, return error
            if self.connects.is_empty() && self.standby.is_empty() {
                return Err(());
            }

            // if not need to connect standby server, wait for events.
            if self.connects.len() >= self.parallel_n
                || self.standby.is_empty() {
                return Ok(Async::NotReady);
            }
        }
    }
}

