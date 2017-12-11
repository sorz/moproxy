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
use ::futures::{future, stream, Future, Stream};
use ::monitor::{ServerList, ServerInfo};
use ::proxy::{self, Destination};
use ::tls::{self, TlsClientHello};


#[derive(Debug)]
pub struct NewClient {
    left: TcpStream,
    src: SocketAddr,
    pub dest: Destination,
    list: Arc<ServerList>,
    handle: Handle,
}

#[derive(Debug)]
pub struct NewClientWithData {
    left: TcpStream,
    src: SocketAddr,
    dest: Destination,
    pending_data: Vec<u8>,
    list: Arc<ServerList>,
    handle: Handle,
}

#[derive(Debug)]
pub struct ConnectedClient {
    left: TcpStream,
    right: TcpStream,
    src: SocketAddr,
    dest: Destination,
    proxy: ServerInfo,
    list: Arc<ServerList>,
    handle: Handle,
}

pub trait Connectable {
    fn connect_server(self) -> Box<Future<Item=ConnectedClient, Error=()>>;
}

impl NewClient {
    pub fn from_socket(left: TcpStream, list: Arc<ServerList>, handle: Handle)
            -> Box<Future<Item=Self, Error=()>> {
        let src_dest = future::result(left.peer_addr())
            .join(future::result(get_original_dest(left.as_raw_fd())))
            .map_err(|err| warn!("fail to get original destination: {}", err));
        Box::new(src_dest.map(move |(src, dest)| {
            NewClient {
                left, src, dest: dest.into(), list, handle,
            }
        }))
    }
}

impl NewClient {
    pub fn retrive_dest(self)
            -> Box<Future<Item=NewClientWithData, Error=()>> {
        let NewClient { left, src, mut dest, list, handle } = self; 
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
            NewClientWithData {
                left, src, dest, list, handle,
                pending_data: data[..len].to_vec(),
            }
        }).map_err(|_| info!("no tls request received before timeout"));
        Box::new(result)
    }
}

impl Connectable for NewClient {
    fn connect_server(self)
            -> Box<Future<Item=ConnectedClient, Error=()>> {
        let NewClient { left, src, dest, list, handle } = self;
        let infos = list.get_infos().clone();
        let seq = try_connect_seq(dest.clone(), infos, 
                                  list.clone(), handle.clone())
            .map(move |(right, info, tag)| {
                info!("{} => {} via {}", src, dest, tag);
                ConnectedClient {
                    left, right, src, dest, proxy: info, list, handle
                }
            }).map_err(|_| warn!("all proxy server down"));
        Box::new(seq)
    }
}

impl Connectable for NewClientWithData {
    fn connect_server(self)
            -> Box<Future<Item=ConnectedClient, Error=()>> {
        let NewClientWithData {
            left, src, dest, list, handle, pending_data } = self;
        let infos = list.get_infos().clone();
        let seq = try_connect_seq(dest.clone(), infos,
                                  list.clone(), handle.clone())
            .and_then(move |(right, info, tag)| {
                write_all(right, pending_data)
                    .map(move |(right, _)| (right, info, tag))
                    .map_err(|err| warn!("fail to write: {}", err))
            }).map(move |(right, info, tag)| {
                info!("{} => {} via {}", src, dest, tag);
                ConnectedClient {
                    left, right, src, dest, proxy: info, list, handle
                }
            }).map_err(|_| warn!("all proxy server down"));
        Box::new(seq)
    }
}

fn try_connect_seq(dest: Destination, servers: Vec<ServerInfo>,
                   list: Arc<ServerList>, handle: Handle)
        -> Box<Future<Item=(TcpStream, ServerInfo, String), Error=()>> {
    let timer = Timer::default();
    let try_all = stream::iter_ok(servers).for_each(move |info| {
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
        Err(args) => Ok(args),
        Ok(_) => Err(()),
    });
    Box::new(try_all)
}

impl ConnectedClient {
    pub fn serve(self) -> Box<Future<Item=(), Error=()>> {
        let ConnectedClient { left, right, dest,
            proxy: info, list, .. } = self;
        // TODO: make keepalive configurable
        let timeout = Some(Duration::from_secs(300));
        if let Err(e) = left.set_keepalive(timeout)
                .and(right.set_keepalive(timeout)) {
            warn!("fail to set keepalive: {}", e);
        }

        list.update_stats_conn_open(&info);
        let serve = proxy::piping(left, right).then(move |result| {
            match result {
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

