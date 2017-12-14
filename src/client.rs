use std::cmp;
use std::rc::Rc;
use std::io::{self, ErrorKind};
use std::time::Duration;
use std::net::{SocketAddr, SocketAddrV4};
use std::os::unix::io::{RawFd, AsRawFd};
use nix::{self, sys};
use tokio_core::net::TcpStream;
use tokio_core::reactor::Handle;
use tokio_timer::Timer;
use tokio_io::io::{read, write_all};
use futures::{future, stream, Future, Stream, Poll};
use proxy::{ProxyServer, Destination};
use proxy::copy::pipe;
use monitor::ServerList;
use tls::{self, TlsClientHello};


#[derive(Debug)]
pub struct NewClient {
    left: TcpStream,
    src: SocketAddr,
    pub dest: Destination,
    list: ServerList,
    handle: Handle,
}

#[derive(Debug)]
pub struct NewClientWithData {
    left: TcpStream,
    src: SocketAddr,
    dest: Destination,
    pending_data: Box<[u8]>,
    is_tls: bool,
    list: ServerList,
    handle: Handle,
}

#[derive(Debug)]
pub struct ConnectedClient {
    left: TcpStream,
    right: TcpStream,
    src: SocketAddr,
    dest: Destination,
    server: Rc<ProxyServer>,
    handle: Handle,
}

pub trait Connectable {
    fn connect_server(self, n_parallel: usize)
        -> Box<Future<Item=ConnectedClient, Error=()>>;
}

impl NewClient {
    pub fn from_socket(left: TcpStream, list: ServerList, handle: Handle)
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
            let is_tls = match tls::parse_client_hello(&data[..len]) {
                Err(err) => {
                    info!("fail to parse hello: {}", err);
                    false
                },
                Ok(TlsClientHello { server_name: None, .. } ) => {
                    debug!("not SNI found in client hello");
                    true
                },
                Ok(TlsClientHello { server_name: Some(name), .. } ) => {
                    debug!("SNI found: {}", name);
                    dest = (name, dest.port).into();
                    true
                },
            };
            NewClientWithData {
                left, src, dest, list, handle, is_tls,
                pending_data: data[..len].to_vec().into_boxed_slice(),
            }
        }).map_err(|_| info!("no tls request received before timeout"));
        Box::new(result)
    }
}

impl Connectable for NewClient {
    fn connect_server(self, _n_parallel: usize)
            -> Box<Future<Item=ConnectedClient, Error=()>> {
        let NewClient { left, src, dest, list, handle } = self;
        let seq = try_connect_seq(dest.clone(), list, handle.clone())
            .map(move |(right, server)| {
                info!("{} => {} via {}", src, dest, server.tag);
                ConnectedClient {
                    left, right, src, dest, server, handle
                }
            }).map_err(|_| warn!("all proxy server down"));
        Box::new(seq)
    }
}

impl Connectable for NewClientWithData {
    fn connect_server(self, n_parallel: usize)
            -> Box<Future<Item=ConnectedClient, Error=()>> {
        let NewClientWithData {
            left, src, dest, list, handle, pending_data, is_tls } = self;
        let n_parallel = if is_tls {
            cmp::min(list.len(), n_parallel)
        } else {
            0
        };
        let pending_data = RcBox::new(pending_data);
        let (list_par, list_seq) = list.split_at(
            cmp::min(list.len(), n_parallel));
        let conn_par = try_connect_par(dest.clone(), list_par.to_vec(),
                                       pending_data.clone(), handle.clone());
        let conn_seq = try_connect_seq(dest.clone(), list_seq.to_vec(),
                                       handle.clone());
        let conn = conn_par.then(move |result| match result {
            Ok((right, server)) => {
                Box::new(future::ok((left, right, server)))
                    as Box<Future<Item=_, Error=()>>
            },
            Err(_) => {
                let seq = conn_seq.and_then(move |(right, server)| {
                    write_all(right, pending_data)
                        .map(move |(right, _)| (left, right, server))
                        .map_err(|err| warn!("fail to write: {}", err))
                });
                Box::new(seq)
            },
        });
        let client = conn.map(move |(left, right, server)| {
            info!("{} => {} via {}", src, dest, server.tag);
            ConnectedClient {
                left, right, src, dest, server, handle
            }
        }).map_err(|_| warn!("all proxy server down"));
        Box::new(client)
    }
}

fn try_connect_seq(dest: Destination, servers: ServerList, handle: Handle)
        -> Box<Future<Item=(TcpStream, Rc<ProxyServer>), Error=()>> {
    let timer = Timer::default();
    let try_all = stream::iter_ok(servers).for_each(move |server| {
        let right = server.connect(dest.clone(), &handle);
        timer.timeout(right, server.max_wait).then(move |rst| match rst {
            Ok(right) => Err((right, server)),
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

fn try_connect_par(dest: Destination, servers: ServerList,
                   pending_data: RcBox<[u8]>, handle: Handle)
        -> Box<Future<Item=(TcpStream, Rc<ProxyServer>), Error=()>> {
    let timer = Timer::default();
    let conns: Vec<_> = servers.into_iter().map(move |server| {
        let right = server.connect(dest.clone(), &handle);
        let tag = server.tag.clone();
        let data = pending_data.clone();
        timer.timeout(right, server.max_wait).and_then(move |right| {
            write_all(right, data)
        }).and_then(|(right, _)| {
            ready_to_read(right)
        }).map(|right| {
            (right, server)
        }).map_err(move |err| {
           warn!("fail to connect {}: {}", tag, err);
        })
    }).collect();
    if conns.is_empty() {
        Box::new(future::err(()))
    } else {
        debug!("try to connect {} servers in parallel", conns.len());
        Box::new(future::select_ok(conns)
            .map(|(result, _)| result))
    }
}

impl ConnectedClient {
    pub fn serve(self) -> Box<Future<Item=(), Error=()>> {
        let ConnectedClient { left, right, dest, server, .. } = self;
        // TODO: make keepalive configurable
        let timeout = Some(Duration::from_secs(300));
        if let Err(e) = left.set_keepalive(timeout)
                .and(right.set_keepalive(timeout)) {
            warn!("fail to set keepalive: {}", e);
        }

        server.update_stats_conn_open();
        let serve = pipe(left, right, server.clone()).then(move |result| {
            match result {
                Ok((tx, rx)) => {
                    server.update_stats_conn_close();
                    debug!("tx {}, rx {} bytes ({} => {})",
                        tx, rx, server.tag, dest);
                    Ok(())
                },
                Err(e) => {
                    server.update_stats_conn_close();
                    warn!("{} (=> {}) piping error: {}",
                        server.tag, dest, e);
                    Err(())
                }
            }
        });
        Box::new(serve)
    }
}

struct ReadyToRead {
    conn: Option<TcpStream>,
}

fn ready_to_read(conn: TcpStream) -> ReadyToRead {
    ReadyToRead { conn: Some(conn) }
}

impl Future for ReadyToRead {
    type Item = TcpStream;
    type Error = io::Error;

    fn poll(&mut self) -> Poll<TcpStream, Self::Error> {
        Ok(self.conn.as_ref().unwrap()
           .poll_read().map(|_| self.conn.take().unwrap()))
    }
}


#[derive(Debug)]
struct RcBox<T: ?Sized> {
    item: Rc<Box<T>>,
}
impl<T: ?Sized> RcBox<T> {
    fn new(item: Box<T>) -> Self {
        RcBox { item: Rc::new(item) }
    }
}
impl<T: ?Sized> AsRef<T> for RcBox<T> {
    fn as_ref(&self) -> &T {
        &self.item
    }
}
impl<T: ?Sized> Clone for RcBox<T> {
    fn clone(&self) -> Self {
        RcBox { item: self.item.clone() }
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

