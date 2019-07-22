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
use tokio::{
    util::FutureExt as TokioFutureExt,
    net::TcpStream,
};

use crate::{
    proxy::{Destination, ProxyServer},
    ArcBox,
    tcp_stream_ext::TcpStreamExt,
};


async fn try_connect(
    dest: Destination,
    server: Arc<ProxyServer>,
    pending_data: Option<ArcBox<[u8]>>,
    wait_response: bool,
) -> io::Result<(Arc<ProxyServer>, TcpStream)> {
    // waiting for proxy server connected
    let mut stream = server
        .connect(&dest, pending_data)
        .timeout(server.max_wait)
        .await??;

    // waiting for response data
    if wait_response {
        let mut buf = [0u8; 8];
        let len = stream.peek(&mut buf)
            .timeout(server.max_wait)
            .await??;
        if len <= 0 {
            return Err(io::Error::new(
                ErrorKind::UnexpectedEof,
                "no response data",
            ));
        }
    }
    Ok((server, stream))
}

/// Try to connect one of the proxy servers.
/// Pick `parallel_n` servers from `queue` to `connecting` and wait for
/// connect. Once any of them connected, move that to `reading` and wait
/// for read respone. Once any of handshakings done, return it and cancel
/// others.
pub struct TryConnectAll {
    dest: Destination,
    pending_data: Option<ArcBox<[u8]>>,
    parallel_n: usize,
    wait_response: bool,
    standby: VecDeque<Arc<ProxyServer>>,
    connects: VecDeque<Pin<Box<dyn Future<Output=io::Result<(Arc<ProxyServer>, TcpStream)>> + Send>>>,
}

pub fn try_connect_all(
    dest: Destination,
    servers: Vec<Arc<ProxyServer>>,
    parallel_n: usize,
    wait_response: bool,
    pending_data: Option<ArcBox<[u8]>>,
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

impl Future for TryConnectAll {
    type Output = Option<(Arc<ProxyServer>, TcpStream)>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context)
            -> Poll<Option<(Arc<ProxyServer>, TcpStream)>> {
        loop {
            // if current connections less than parallel_n,
            // pick servers from queue to connect.
            while !self.standby.is_empty() && self.connects.len() < self.parallel_n {
                let server = self.standby.pop_front().unwrap();
                let data = self.pending_data.clone();
                let conn = try_connect(self.dest.clone(), server, data, self.wait_response);
                self.connects.push_back(Box::pin(conn));
            }

            // poll all connects
            let mut i = 0;
            while i < self.connects.len() {
                let conn = &mut self.connects[i];
                match conn.as_mut().poll(cx) {
                    // error, stop trying, drop it.
                    Poll::Ready(Err(e)) => {
                        info!("connect proxy error: {}", e);
                        drop(self.connects.remove(i));
                    }
                    // not ready, keep here, poll next one.
                    Poll::Pending => i += 1,
                    // ready, return it.
                    Poll::Ready(Ok(item)) =>
                        return Poll::Ready(Some(item)),
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
