use bytes::Bytes;
use std::{
    collections::VecDeque,
    future::Future,
    io::{self, ErrorKind},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::{net::TcpStream, time::timeout};
use tracing::{info, instrument};

use crate::proxy::{Destination, ProxyServer};

#[derive(Debug, Clone)]
struct Request {
    dest: Destination,
    pending_data: Option<Bytes>,
    wait_response: bool,
}

#[instrument(skip_all, fields(proxy = %server.tag))]
async fn try_connect(request: Request, server: Arc<ProxyServer>) -> io::Result<TcpStream> {
    let max_wait = server.max_wait();
    // waiting for proxy server connected
    let stream = timeout(
        max_wait,
        server.connect(&request.dest, request.pending_data),
    )
    .await??;

    // waiting for response data
    if request.wait_response {
        let mut buf = [0u8; 4];
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
pub struct TryConnectAll {
    request: Request,
    parallel_n: usize,
    standby: VecDeque<Arc<ProxyServer>>,
    connects: VecDeque<(Arc<ProxyServer>, PinnedConnectFuture)>,
    last_error: Option<io::Error>,
}

pub fn try_connect_all(
    dest: &Destination,
    servers: Vec<Arc<ProxyServer>>,
    parallel_n: usize,
    wait_response: bool,
    pending_data: Option<Bytes>,
) -> TryConnectAll {
    let parallel_n = parallel_n.clamp(1, if wait_response { servers.len() } else { 1 });
    let servers = servers.into_iter().collect();
    let request = Request {
        dest: dest.clone(),
        pending_data,
        wait_response,
    };
    TryConnectAll {
        request,
        parallel_n,
        standby: servers,
        connects: VecDeque::with_capacity(parallel_n),
        last_error: None,
    }
}

impl Future for TryConnectAll {
    type Output = io::Result<(Arc<ProxyServer>, TcpStream)>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        loop {
            // if current connections less than parallel_n,
            // pick servers from queue to connect.
            while !self.standby.is_empty() && self.connects.len() < self.parallel_n {
                let server = self.standby.pop_front().unwrap();
                let conn = try_connect(self.request.clone(), server.clone());
                self.connects.push_back((server, Box::pin(conn)));
            }

            // poll all connects
            let mut i = 0;
            while i < self.connects.len() {
                let (server, conn) = &mut self.connects[i];
                match conn.as_mut().poll(cx) {
                    // error, stop trying, drop it.
                    Poll::Ready(Err(err)) => {
                        info!(proxy = %server.tag, ?err, "Failed to connect upstream proxy");
                        self.last_error = Some(err);
                        drop(self.connects.remove(i));
                    }
                    // not ready, keep here, poll next one.
                    Poll::Pending => i += 1,
                    // ready, return it.
                    Poll::Ready(Ok(conn)) => return Poll::Ready(Ok((server.clone(), conn))),
                }
            }

            // if all servers failed, return error
            if self.connects.is_empty() && self.standby.is_empty() {
                let err = self.last_error.take().unwrap_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "no upstream proxy")
                });
                return Poll::Ready(Err(err));
            }

            // if not need to connect standby server, wait for events.
            if self.connects.len() >= self.parallel_n || self.standby.is_empty() {
                return Poll::Pending;
            }
        }
    }
}
