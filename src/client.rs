mod connect;
mod tls;
use log::{debug, info, warn};
use std::{
    io,
    cmp,
    net::SocketAddr,
    sync::Arc,
    time::Duration,
    pin::Pin,
    future::Future,
};
use tokio::{
    util::FutureExt as TokioFutureExt,
    io::AsyncReadExt,
    net::TcpStream,
};

use crate::{
    client::connect::try_connect_all,
    client::tls::parse_client_hello,
    monitor::ServerList,
    proxy::copy::pipe,
    proxy::{Destination, ProxyServer},
    tcp::{get_original_dest, get_original_dest6},
    ArcBox,
};

#[derive(Debug)]
pub struct NewClient {
    left: TcpStream,
    src: SocketAddr,
    pub dest: Destination,
    list: ServerList,
}

#[derive(Debug)]
pub struct NewClientWithData {
    client: NewClient,
    pending_data: Option<Box<[u8]>>,
    allow_parallel: bool,
}

#[derive(Debug)]
pub struct ConnectedClient {
    left: TcpStream,
    right: TcpStream,
    dest: Destination,
    server: Arc<ProxyServer>,
}

type ConnectServer = Pin<Box<dyn Future<Output=Option<ConnectedClient>> + Send>>;

pub trait Connectable {
    fn connect_server(self, n_parallel: usize) -> ConnectServer;
}

impl NewClient {
    pub fn from_socket(left: TcpStream, list: ServerList) -> io::Result<Self> {
        let src = left.peer_addr()?;
        // TODO: call either v6 or v4 according to our socket
        let dest = get_original_dest(&left).map(SocketAddr::V4)
            .or_else(|_| get_original_dest6(&left).map(SocketAddr::V6))?
            .into();
        debug!("dest {:?}", dest);
        Ok(NewClient { left, src, dest, list })
    }
}

impl NewClient {
    pub async fn retrive_dest(self) -> io::Result<NewClientWithData> {
        let NewClient { mut left, src, mut dest, list } = self;
        let wait = Duration::from_millis(500);
        // try to read TLS ClientHello for
        //   1. --remote-dns: parse host name from SNI
        //   2. --n-parallel: need the whole request to be forwarded
        let mut allow_parallel = false;
        let mut pending_data = None;
        let mut buf = vec![0u8; 2048];
        if let Ok(len) = left.read(&mut buf).timeout(wait).await {
            buf.truncate(len?);
            // only TLS is safe to duplicate requests.
            match parse_client_hello(&buf) {
                Err(err) => info!("fail to parse hello: {}", err),
                Ok(hello) => {
                    allow_parallel = true;
                    if let Some(name) = hello.server_name {
                        dest = (name, dest.port).into();
                        debug!("SNI found: {}", name);
                    }
                    if hello.early_data {
                        debug!("TLS with early data");
                    }
                }
            }
            pending_data = Some(buf.into_boxed_slice());
        } else {
            info!("no tls request received before timeout");
        }
        Ok(NewClientWithData {
            client: NewClient {
                left,
                src,
                dest,
                list,
            },
            allow_parallel,
            pending_data,
        })
    }

    async fn connect_server(
        self,
        n_parallel: usize,
        wait_response: bool,
        pending_data: Option<Box<[u8]>>,
    ) -> Option<ConnectedClient> {
        let NewClient { left, src, dest, list } = self;
        let pending_data = pending_data.map(ArcBox::new);
        let (server, right) = try_connect_all(
            dest.clone(),
            list,
            n_parallel,
            wait_response,
            pending_data,
        ).await?;
        info!("{} => {} via {}", src, dest, server.tag);
        Some(ConnectedClient { left, right, dest, server})
    }
}

impl Connectable for NewClient {
    fn connect_server(self, _n_parallel: usize) -> ConnectServer {
        Box::pin(self.connect_server(1, false, None))
    }
}

impl Connectable for NewClientWithData {
    fn connect_server(self, n_parallel: usize) -> ConnectServer {
        let NewClientWithData {
            client,
            pending_data,
            allow_parallel,
        } = self;
        let n_parallel = if allow_parallel {
            cmp::min(client.list.len(), n_parallel)
        } else {
            1
        };
        Box::pin(client.connect_server(n_parallel, true, pending_data))
    }
}

impl ConnectedClient {
    pub async fn serve(self) -> io::Result<()> {
        let ConnectedClient { left, right, dest, server } = self;
        // TODO: make keepalive configurable
        let timeout = Some(Duration::from_secs(180));
        if let Err(e) = left.set_keepalive(timeout)
                .and(right.set_keepalive(timeout)) {
            warn!("fail to set keepalive: {}", e);
        }
        server.update_stats_conn_open();
        match pipe(left, right, server.clone()).await {
            Ok(amt) => {
                server.update_stats_conn_close(false);
                debug!(
                    "tx {}, rx {} bytes ({} => {})",
                    amt.tx_bytes, amt.rx_bytes, server.tag, dest
                );
                Ok(())
            }
            Err(err) => {
                server.update_stats_conn_close(true);
                warn!("{} (=> {}) close with error", server.tag, dest);
                Err(err)
            }
        }
    }
}
