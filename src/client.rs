use std::cmp;
use std::io::{self, ErrorKind};
use std::sync::Arc;
use std::time::Duration;
use std::net::{SocketAddr, SocketAddrV4};
use std::os::unix::io::{RawFd, AsRawFd};
use ::nix::{self, sys};
use ::tokio_core::net::TcpStream;
use ::tokio_core::reactor::Handle;
use ::tokio_timer::Timer;
use ::tokio_io::io::{read, write_all};
use ::futures::{future, stream, Future, Stream, IntoFuture};
use ::monitor::{ServerList, ServerInfo};
use ::proxy::{self, Destination};
use ::tls::{self, TlsClientHello};


#[derive(Debug)]
pub struct Client {
    left: TcpStream,
    src: SocketAddr,
    pub dest: Destination,
    right: Option<TcpStream>,
    proxy: Option<ServerInfo>,
    pending_data: Option<Vec<u8>>,
    list: Arc<ServerList>,
    handle: Handle,
}


impl Client {
    fn new(left: TcpStream, src: SocketAddr, dest: Destination,
           list: Arc<ServerList>, handle: Handle) -> Self {
        Client {
            left, src, dest,
            right: None, proxy: None, pending_data: None,
            list, handle,
        }
    }

    pub fn from_socket(left: TcpStream, list: Arc<ServerList>, handle: Handle)
            -> Box<Future<Item=Self, Error=()>> {
        let src_dest = future::result(left.peer_addr())
            .join(future::result(get_original_dest(left.as_raw_fd())))
            .map_err(|err| warn!("fail to get original destination: {}", err));
        Box::new(src_dest.map(move |(src, dest)| {
            Client::new(left, src, dest.into(), list, handle)
        }))
    }

    pub fn retrive_dest(self) -> Box<Future<Item=Self, Error=()>> {
        let Client { left, src, mut dest, list, handle, .. } = self; 
        // FIXME: may accidentally lost other field
        let timer = Timer::default();
        let wait = Duration::from_millis(200);
        let data = read(left, vec![0u8; 768])
            .map_err(|err| warn!("fail to read hello from client: {}", err));
        let result = timer.timeout(data, wait).map(move |(left, data, len)| {
            match tls::parse_client_hello(&data[..len]) {
                Err(err) => info!("fail to parse hello: {}", err),
                Ok(TlsClientHello { server_name: None, .. } ) =>
                    debug!("not SNI found in client hello"),
                Ok(TlsClientHello { server_name: Some(name), .. } ) => {
                    debug!("SNI found: {}", name);
                    dest = (name, dest.port).into();
                },
            };
            let mut client = Client::new(left, src, dest, list, handle);
            client.pending_data = Some(data[..len].to_vec());
            client
        }).map_err(|_| info!("no tls request received before timeout"));
        Box::new(result)
    }

    pub fn connect_server(self) -> Box<Future<Item=Self, Error=()>> {
        let infos = self.list.get_infos().clone();
        let dest = self.dest.clone();
        let try_all = self.try_connect_seq(dest, infos)
            .map(move |(client, tag)| {
                info!("{} => {} via {}", client.src, client.dest, tag);
                client
            }).map_err(|_| warn!("all proxy server down"));
        Box::new(try_all)
    }

    fn try_connect_seq(mut self, dest: Destination, infos: Vec<ServerInfo>)
            -> Box<Future<Item=(Self, String), Error=Self>> {
        let timer = Timer::default();
        let list = self.list.clone();
        let handle = self.handle.clone();
        let try_all = stream::iter_ok(infos).for_each(move |info| {
            let server = list.servers[info.idx].clone();
            let right = server.connect(dest.clone(), &handle);
            let wait = if let Some(delay) = info.delay {
                cmp::max(Duration::from_secs(3), delay * 2)
            } else {
                Duration::from_secs(3)
            };
            // Standard proxy server need more time (e.g. DNS resolving)
            timer.timeout(right, wait).then(move |result| match result {
                Ok(right) => Err((right, info, server.tag)),
                Err(err) => {
                    warn!("fail to connect {}: {}", server.tag, err);
                    Ok(())
                },
            })
        }).then(move |result| match result {
            Err((right, info, tag)) => {
                self.right = Some(right);
                self.proxy = Some(info);
                Ok((self, tag))
            },
            Ok(_) => Err(self),
        });
        Box::new(try_all)
    }

    pub fn serve(self) -> Box<Future<Item=(), Error=()>> {
        let Client { left, right, dest, proxy: info, list,
                     pending_data, .. } = self;
        let right = right.expect("client not connected");
        let info = info.expect("client not connected");

        // TODO: make keepalive configurable
        let timeout = Some(Duration::from_secs(300));
        if let Err(e) = left.set_keepalive(timeout)
                .and(right.set_keepalive(timeout)) {
            warn!("fail to set keepalive: {}", e);
        }

        list.update_stats_conn_open(&info);
        let sent = if let Some(data) = pending_data {
            Box::new(write_all(right, data).map(|(right, _)| right))
                as Box<Future<Item=TcpStream, Error=io::Error>>
        } else {
            Box::new(Ok(right).into_future())
        };

        let serve = sent.and_then(move |right| {
            proxy::piping(left, right)
        }).then(move |result| match result {
            Ok((tx, rx)) => {
                list.update_stats_conn_close(&info, tx, rx);
                debug!("tx {}, rx {} bytes ({} => {})",
                    tx, rx, list.servers[info.idx].tag, dest);
                Ok(())
            },
            Err(e) => {
                list.update_stats_conn_close(&info, 0, 0);
                warn!("{} (=> {}) piping error: {}",
                    list.servers[info.idx].tag, dest, e);
                Err(())
            }
        });
        Box::new(serve)
    }
}



fn get_original_dest(fd: RawFd) -> io::Result<SocketAddr> {
    let addr = sys::socket::getsockopt(fd, sys::socket::sockopt::OriginalDst)
        .map_err(|e| match e {
            nix::Error::Sys(err) => io::Error::from(err),
            _ => io::Error::new(ErrorKind::Other, e),
        })?;
    let addr = SocketAddrV4::new(addr.sin_addr.s_addr.to_be().into(),
                                 addr.sin_port.to_be());
    // TODO: support IPv6
    Ok(SocketAddr::V4(addr))
}

