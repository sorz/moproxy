use bytes::Bytes;
use log::debug;
use std::{
    cmp,
    collections::VecDeque,
    future::Future,
    io::{self, ErrorKind},
    iter::FromIterator,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::{net::TcpStream, time::timeout};

use crate::proxy::{Destination, ProxyServer};

async fn try_connect(
    dest: Destination,
    server: Arc<ProxyServer>,
    pending_data: Option<Bytes>,
    wait_response: bool,
) -> io::Result<TcpStream> {
    let max_wait = server.max_wait();
    // waiting for proxy server connected
    let stream = timeout(max_wait, server.connect(&dest, pending_data)).await??;

    // waiting for response data
    if wait_response {
        let mut buf = [0u8; 8];
        let len = timeout(max_wait, stream.peek(&mut buf)).await??;
        if len == 0 {
            return Err(io::Error::new(ErrorKind::UnexpectedEof, "no response data"));
        }
    }
    Ok(stream)
}

type PinnedConnectFuture = Pin<Box<dyn Future<Output = io::Result<TcpStream>> + Send>>;

/// Try to connect one of the proxy servers.
/// Pick `parallel_n` servers from `queue` to `connecting` and wait for
/// connect. Once any of them connected, move that to `reading` and wait
/// for read respone. Once any of handshakings done, return it and cancel
/// others.
pub struct TryConnectAll<'a> {
    dest: &'a Destination,
    pending_data: Option<Bytes>,
    parallel_n: usize,
    wait_response: bool,
    standby: VecDeque<Arc<ProxyServer>>,
    connects: VecDeque<(Arc<ProxyServer>, PinnedConnectFuture)>,
}

pub fn try_connect_all(
    dest: &Destination,
    servers: Vec<Arc<ProxyServer>>,
    parallel_n: usize,
    wait_response: bool,
    pending_data: Option<Bytes>,
) -> TryConnectAll {
    let parallel_n = cmp::max(1, parallel_n);
    let servers = VecDeque::from_iter(servers.into_iter());
    TryConnectAll {
        dest,
        parallel_n,
        pending_data,
        wait_response,
        standby: servers,
        connects: VecDeque::with_capacity(parallel_n),
    }
}

impl<'a> Future for TryConnectAll<'a> {
    type Output = Option<(Arc<ProxyServer>, TcpStream)>;

    fn poll(
        mut self: Pin<&mut Self>,
        cx: &mut Context,
    ) -> Poll<Option<(Arc<ProxyServer>, TcpStream)>> {
        loop {
            let dest = self.dest.clone();
            // if current connections less than parallel_n,
            // pick servers from queue to connect.
            while !self.standby.is_empty() && self.connects.len() < self.parallel_n {
                let server = self.standby.pop_front().unwrap();
                let data = self.pending_data.clone();
                let conn = try_connect(dest.clone(), server.clone(), data, self.wait_response);
                self.connects.push_back((server, Box::pin(conn)));
            }

            // poll all connects
            let mut i = 0;
            while i < self.connects.len() {
                let (server, conn) = &mut self.connects[i];
                match conn.as_mut().poll(cx) {
                    // error, stop trying, drop it.
                    Poll::Ready(Err(e)) => {
                        debug!("connect {} via {} error: {}", dest, server, e);
                        drop(self.connects.remove(i));
                    }
                    // not ready, keep here, poll next one.
                    Poll::Pending => i += 1,
                    // ready, return it.
                    Poll::Ready(Ok(conn)) => return Poll::Ready(Some((server.clone(), conn))),
                }
            }

            // if all servers failed, return error
            if self.connects.is_empty() && self.standby.is_empty() {
                return Poll::Ready(None);
            }

            // if not need to connect standby server, wait for events.
            if self.connects.len() >= self.parallel_n || self.standby.is_empty() {
                return Poll::Pending;
            }
        }
    }
}
