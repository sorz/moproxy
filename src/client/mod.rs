mod connect;
mod tls_parser;
use bytes::{Bytes, BytesMut};
use flexstr::SharedStr;
use std::{
    borrow::Cow,
    io,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time::timeout,
};
use tracing::{debug, info, instrument, warn};

#[cfg(target_os = "linux")]
use crate::linux::tcp::TcpStreamExt;
use crate::{
    client::connect::try_connect_all,
    policy::RequestFeatures,
    proxy::{copy::pipe, Traffic},
    proxy::{Address, Destination, ProxyServer},
};

#[derive(Debug, Default)]
pub struct TlsData {
    pending_data: Option<Bytes>,
    has_full_tls_hello: bool,
    pub sni: Option<SharedStr>,
}

#[derive(Debug)]
pub struct NewClient {
    left: TcpStream,
    /// Destination IP address or domain name with port number.
    /// Retrived from firewall or SOCKSv5 request initially, may be override
    /// by TLS SNI.
    pub dest: Destination,
    /// Destination IP address. Unlike `dest`, it won't be override by SNI.
    dest_ip_addr: Option<IpAddr>,
    /// Server's TCP port number.
    from_port: u16,
    pub tls: Option<TlsData>,
}

#[derive(Debug)]
pub struct ConnectedClient {
    orig: NewClient,
    right: TcpStream,
    server: Arc<ProxyServer>,
}

#[derive(Debug)]
pub enum FailedClient {
    Recoverable(NewClient),
    Unrecoverable(io::Error),
}

impl From<io::Error> for FailedClient {
    fn from(value: io::Error) -> Self {
        Self::Unrecoverable(value)
    }
}

fn error_invalid_input<T>(msg: &'static str) -> io::Result<T> {
    Err(io::Error::new(io::ErrorKind::InvalidInput, msg))
}

trait SocketAddrExt {
    fn normalize(&self) -> Cow<SocketAddr>;
}

impl SocketAddrExt for SocketAddr {
    fn normalize(&self) -> Cow<SocketAddr> {
        match self {
            SocketAddr::V4(sock) => {
                let addr = sock.ip().to_ipv6_mapped();
                let sock = SocketAddr::new(addr.into(), sock.port());
                Cow::Owned(sock)
            }
            _ => Cow::Borrowed(self),
        }
    }
}

#[instrument(skip_all)]
async fn accept_socks5(client: &mut TcpStream) -> io::Result<Destination> {
    // Not a NATed connection, treated as SOCKSv5
    // Parse version
    // TODO: add timeout
    // TODO: use buffered reader
    let ver = client.read_u8().await?;
    if ver != 0x05 {
        return error_invalid_input("Neither a NATed or SOCKSv5 connection");
    }
    // Parse auth methods
    let n_methods = client.read_u8().await?;
    let mut buf = vec![0u8; n_methods as usize];
    client.read_exact(&mut buf).await?;
    if !buf.iter().any(|&m| m == 0) {
        return error_invalid_input("SOCKSv5: No auth is required");
    }
    // Select no auth
    client.write_all(&[0x05, 0x00]).await?;
    // Parse request
    buf.resize(4, 0);
    client.read_exact(&mut buf).await?;
    if buf[0..2] != [0x05, 0x01] {
        return error_invalid_input("SOCKSv5: CONNECT is required");
    }
    let addr: Address = match buf[3] {
        0x01 => {
            // IPv4
            let mut buf = [0u8; 4];
            client.read_exact(&mut buf).await?;
            buf.into()
        }
        0x03 => {
            // Domain name
            let len = client.read_u8().await? as usize;
            buf.resize(len, 0);
            client.read_exact(&mut buf).await?;

            let domain = std::str::from_utf8(&buf).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "SOCKSv5: Invalid domain name")
            })?;
            Address::Domain(domain.into())
        }
        0x04 => {
            // IPv6
            let mut buf = [0u8; 16];
            client.read_exact(&mut buf).await?;
            buf.into()
        }
        _ => return error_invalid_input("SOCKSv5: unknown address type"),
    };
    let port = client.read_u16().await?;
    // Send response
    client.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
    Ok((addr, port).into())
}

async fn transparent_dest(client: &mut TcpStream) -> io::Result<Destination> {
    // Transparent firewall redirection (unmodified destination address)
    // (for example, ipfw firewall with 'fwd' rule)
    Ok((client.local_addr()?).into())
}

impl NewClient {
    #[instrument(name = "retrieve_dest", skip_all)]
    pub async fn from_socket(mut left: TcpStream) -> io::Result<Self> {
        let from_port = left.local_addr()?.port();

        // Try to get original destination before NAT
        #[cfg(target_os = "linux")]
        let dest = match left.get_original_dest()? {
            // Redirecting to itself is possible. Treat it as non-redirect.
            Some(dest) if dest.normalize() != left.local_addr()?.normalize() => Some(dest),
            _ => None,
        };

        // No NAT supported
        #[cfg(not(target_os = "linux"))]
        let dest: Option<SocketAddr> = None;

        #[cfg(target_os = "freebsd")]
        let dest = (transparent_dest(&mut left).await?).into();

        let dest = if let Some(dest) = dest {
            debug!(?dest, "Retrived destination via NAT info");
            dest.into()
        } else {
            let dest = accept_socks5(&mut left).await?;
            debug!(?dest, "Retrived destination via SOCKSv5");
            dest
        };

        let dest_ip_addr = match dest.host {
            Address::Ip(ip) => Some(ip),
            Address::Domain(_) => None,
        };

        Ok(NewClient {
            left,
            dest,
            dest_ip_addr,
            from_port,
            tls: None,
        })
    }

    fn pending_data(&self) -> Option<Bytes> {
        Some(self.tls.as_ref()?.pending_data.as_ref()?.clone())
    }

    pub fn features(&self) -> RequestFeatures<SharedStr> {
        RequestFeatures {
            listen_port: Some(self.from_port),
            dst_domain: self.dest.host.domain(),
            dst_ip: self.dest_ip_addr,
        }
    }

    pub fn override_dest_with_sni(&mut self) -> bool {
        match (
            &mut self.dest.host,
            &self.tls.as_ref().and_then(|tls| tls.sni.clone()),
        ) {
            (Address::Domain(_), _) => false,
            (_, None) => false,
            (dst, Some(host)) => {
                *dst = Address::Domain(host.clone());
                true
            }
        }
    }

    #[instrument(level = "error", skip_all, fields(dest=?self.dest))]
    pub async fn direct_connect(
        self,
        pseudo_server: Arc<ProxyServer>,
    ) -> io::Result<ConnectedClient> {
        let mut right = match self.dest.host {
            Address::Ip(addr) => TcpStream::connect((addr, self.dest.port)).await?,
            Address::Domain(ref name) => {
                TcpStream::connect((name.as_ref(), self.dest.port)).await?
            }
        };
        right.set_nodelay(true)?;

        if let Some(data) = self.pending_data() {
            right.write_all(&data).await?;
        }

        info!(remote = %right.peer_addr()?, "Connected w/o proxy");
        Ok(ConnectedClient {
            orig: self,
            right,
            server: pseudo_server,
        })
    }

    #[instrument(level = "error", skip_all, fields(dest=?self.dest))]
    pub async fn retrieve_dest_from_sni(&mut self) -> io::Result<()> {
        if self.tls.is_some() {
            return Ok(());
        }
        let mut tls = TlsData::default();
        let wait = Duration::from_millis(500);
        let mut buf = BytesMut::with_capacity(2048);
        buf.resize(buf.capacity(), 0);
        if let Ok(len) = timeout(wait, self.left.read(&mut buf)).await {
            buf.truncate(len?);
            // only TLS is safe to duplicate requests.
            match tls_parser::parse_client_hello(&buf) {
                Err(err) => info!("fail to parse hello: {}", err),
                Ok(hello) => {
                    tls.has_full_tls_hello = true;
                    if let Some(name) = hello.server_name {
                        tls.sni = Some(name.into());
                        debug!(sni = name, "SNI found");
                    }
                    if hello.early_data {
                        debug!("TLS with early data");
                    }
                }
            }
            tls.pending_data = Some(buf.freeze());
        } else {
            info!("no tls request received before timeout");
        }
        self.tls = Some(tls);
        Ok(())
    }

    #[instrument(level = "error", skip_all, fields(dest=?self.dest))]
    pub async fn connect_server(
        self,
        proxies: Vec<Arc<ProxyServer>>,
        n_parallel: usize,
    ) -> Result<ConnectedClient, FailedClient> {
        if proxies.is_empty() {
            warn!("No avaiable proxy");
            return Err(FailedClient::Recoverable(self));
        }
        let (n_parallel, wait_response) = match self.tls {
            Some(ref tls) if tls.has_full_tls_hello => (n_parallel.clamp(1, proxies.len()), true),
            _ => (1, false),
        };
        let proxies_len = proxies.len();
        match try_connect_all(
            &self.dest,
            proxies,
            n_parallel,
            wait_response,
            self.pending_data(),
        )
        .await
        {
            Ok((server, right)) => {
                info!(proxy = %server.tag, "Proxy connected");
                Ok(ConnectedClient {
                    orig: self,
                    right,
                    server,
                })
            }
            Err(err) => {
                warn!("Tried {} proxies but failed: {}", proxies_len, err);
                Err(FailedClient::Recoverable(self))
            }
        }
    }
}

impl FailedClient {
    pub fn recovery(self) -> io::Result<NewClient> {
        match self {
            Self::Recoverable(client) => Ok(client),
            Self::Unrecoverable(err) => Err(err),
        }
    }
}

impl ConnectedClient {
    #[instrument(level = "error", skip_all, fields(dest=?self.orig.dest, proxy=%self.server.tag))]
    pub async fn serve(self) -> io::Result<()> {
        let ConnectedClient {
            orig,
            right,
            server,
            ..
        } = self;
        // TODO: make keepalive configurable
        // FIXME: set_cookies
        /*
        let timeout = Some(Duration::from_secs(180));
        FIXME: keepalive
        https://github.com/tokio-rs/tokio/issues/3109

        if let Err(e) = left
            .set_keepalive(timeout)
            .and(right.set_keepalive(timeout))
        {
            warn!("fail to set keepalive: {}", e);
        }
        */
        server.update_stats_conn_open();
        match pipe(orig.left, right, server.clone()).await {
            Ok(Traffic { tx_bytes, rx_bytes }) => {
                server.update_stats_conn_close(false);
                debug!(tx_bytes, rx_bytes, "Closed");
                Ok(())
            }
            Err(err) => {
                server.update_stats_conn_close(true);
                info!(?err, "Closed");
                Err(err)
            }
        }
    }
}
