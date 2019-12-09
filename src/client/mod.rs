mod connect;
mod tls;
use log::{debug, info, warn};
use std::{
    cmp,
    future::Future,
    io,
    net::SocketAddr,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    future::FutureExt,
    io::{AsyncReadExt, AsyncWriteExt},
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
    has_full_tls_hello: bool,
}

#[derive(Debug)]
pub struct ConnectedClient {
    left: TcpStream,
    right: TcpStream,
    dest: Destination,
    server: Arc<ProxyServer>,
    connected_at: Instant,
}

#[derive(Debug)]
pub struct FailedClient {
    pub left: TcpStream,
    pub pending_data: Option<Box<[u8]>>,
}

type ConnectServer = Pin<Box<dyn Future<Output = Result<ConnectedClient, FailedClient>> + Send>>;

pub trait Connectable {
    fn connect_server(self, n_parallel: usize) -> ConnectServer;
}

impl NewClient {
    pub fn from_socket(left: TcpStream, list: ServerList) -> io::Result<Self> {
        let src = left.peer_addr()?;
        // TODO: call either v6 or v4 according to our socket
        let dest = get_original_dest(&left)
            .map(SocketAddr::V4)
            .or_else(|_| get_original_dest6(&left).map(SocketAddr::V6))?
            .into();
        debug!("dest {:?}", dest);
        Ok(NewClient {
            left,
            src,
            dest,
            list,
        })
    }
}

impl NewClient {
    pub async fn retrive_dest(self) -> io::Result<NewClientWithData> {
        let NewClient {
            mut left,
            src,
            mut dest,
            list,
        } = self;
        let wait = Duration::from_millis(500);
        // try to read TLS ClientHello for
        //   1. --remote-dns: parse host name from SNI
        //   2. --n-parallel: need the whole request to be forwarded
        let mut has_full_tls_hello = false;
        let mut pending_data = None;
        let mut buf = vec![0u8; 2048];
        if let Ok(len) = left.read(&mut buf).timeout(wait).await {
            buf.truncate(len?);
            // only TLS is safe to duplicate requests.
            match parse_client_hello(&buf) {
                Err(err) => info!("fail to parse hello: {}", err),
                Ok(hello) => {
                    has_full_tls_hello = true;
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
            has_full_tls_hello,
            pending_data,
        })
    }

    async fn connect_server(
        self,
        n_parallel: usize,
        wait_response: bool,
        pending_data: Option<Box<[u8]>>,
    ) -> Result<ConnectedClient, FailedClient> {
        let NewClient {
            left,
            src,
            dest,
            list,
        } = self;
        let pending_data = pending_data.map(ArcBox::new);
        let result = try_connect_all(&dest, list, n_parallel, wait_response, pending_data).await;
        if let Some((server, right)) = result {
            info!("{} => {} via {}", src, dest, server.tag);
            Ok(ConnectedClient {
                left,
                right,
                dest,
                server,
                connected_at: Instant::now(),
            })
        } else {
            warn!("{} => {} no avaiable proxy", src, dest);
            Err(FailedClient {
                left,
                pending_data: None,
            })
        }
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
            has_full_tls_hello,
        } = self;
        let n_parallel = if has_full_tls_hello {
            cmp::min(client.list.len(), n_parallel)
        } else {
            1
        };
        Box::pin(client.connect_server(n_parallel, has_full_tls_hello, pending_data))
    }
}

impl FailedClient {
    pub async fn direct_connect(
        self,
        pseudo_server: Arc<ProxyServer>,
    ) -> io::Result<ConnectedClient> {
        let Self { left, pending_data } = self;
        // TODO: call either v6 or v4 according to our socket
        let dest: SocketAddr = get_original_dest(&left)
            .map(SocketAddr::V4)
            .or_else(|_| get_original_dest6(&left).map(SocketAddr::V6))?
            .into();

        let mut right = TcpStream::connect(&dest).await?;
        debug!("connected with {:?}", right.peer_addr());
        right.set_nodelay(true)?;

        if let Some(data) = pending_data {
            right.write_all(&data).await?;
        }

        debug!(
            "{} => {} via {}",
            left.peer_addr()?,
            dest,
            pseudo_server.tag
        );
        Ok(ConnectedClient {
            left,
            right,
            dest: dest.into(),
            server: pseudo_server,
            connected_at: Instant::now(),
        })
    }
}

impl ConnectedClient {
    pub async fn serve(self) -> io::Result<()> {
        let ConnectedClient {
            left,
            right,
            dest,
            server,
            connected_at,
        } = self;
        // TODO: make keepalive configurable
        let timeout = Some(Duration::from_secs(180));
        if let Err(e) = left
            .set_keepalive(timeout)
            .and(right.set_keepalive(timeout))
        {
            warn!("fail to set keepalive: {}", e);
        }
        let left_addr = left.peer_addr()?;
        server.update_stats_conn_open();
        match pipe(left, right, server.clone()).await {
            Ok(amt) => {
                server.update_stats_conn_close(false);
                let secs = connected_at.elapsed().as_secs();
                info!(
                    "{} => {} => {} closed, tx {}, rx {} bytes, {} secs",
                    left_addr, server, dest, amt.tx_bytes, amt.rx_bytes, secs,
                );
                Ok(())
            }
            Err(err) => {
                server.update_stats_conn_close(true);
                warn!("{} (=> {}) close with error", server, dest);
                Err(err)
            }
        }
    }
}
