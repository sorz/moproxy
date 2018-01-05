mod tls;
mod read;
mod connect;
use std::cmp;
use std::rc::Rc;
use std::io::{self, ErrorKind};
use std::time::Duration;
use std::net::{SocketAddr, SocketAddrV4};
use std::os::unix::io::{RawFd, AsRawFd};
use nix::{self, sys};
use tokio_core::net::TcpStream;
use tokio_core::reactor::Handle;
use futures::{future, Future};
use proxy::{ProxyServer, Destination};
use proxy::copy::{pipe, SharedBuf};
use monitor::ServerList;
use client::connect::try_connect_all;
use client::read::read_with_timeout;
use client::tls::parse_client_hello;
use RcBox;


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
    client: NewClient,
    pending_data: Option<Box<[u8]>>,
    allow_parallel: bool,
}

#[derive(Debug)]
pub struct ConnectedClient {
    left: TcpStream,
    right: TcpStream,
    dest: Destination,
    server: Rc<ProxyServer>,
}

type ConnectServer = Box<Future<Item=ConnectedClient, Error=()>>;

pub trait Connectable {
    fn connect_server(self, n_parallel: usize) -> ConnectServer;
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
        let wait = Duration::from_millis(500);
        // try to read TLS ClientHello for
        //   1. --remote-dns: parse host name from SNI
        //   2. --n-parallel: need the whole request to be forwarded
        let read = read_with_timeout(left, vec![0u8; 2048], wait, &handle);
        let result = read.map(move |(left, mut data, len)| {
            let (allow_parallel, pending_data) = if len == 0 {
                info!("no tls request received before timeout");
                (false, None)
            } else {
                data.truncate(len);
                // only TLS is safe to duplicate requests.
                let allow_parallel = match parse_client_hello(&data) {
                    Err(err) => {
                        info!("fail to parse hello: {}", err);
                        false
                    },
                    Ok(hello) => {
                        if let Some(name) = hello.server_name {
                            dest = (name, dest.port).into();
                            debug!("SNI found: {}", name);
                        }
                        if hello.early_data {
                            debug!("TLS with early data");
                        }
                        true
                    },
                };
                (allow_parallel, Some(data.into_boxed_slice()))
            };
            NewClientWithData {
                client: NewClient { left, src, dest, list, handle },
                allow_parallel, pending_data,
            }
        }).map_err(|err| warn!("fail to read hello from client: {}", err));
        Box::new(result)
    }

    fn connect_server(self, n_parallel: usize, wait_response: bool,
                      pending_data: Option<Box<[u8]>>) -> ConnectServer {
        let NewClient { left, src, dest, list, handle } = self;
        let pending_data = pending_data.map(|v| RcBox::new(v));
        let conn = try_connect_all(dest.clone(), list, n_parallel,
                                   wait_response, pending_data, handle);
        let client = conn.map(move |(server, right)| {
            info!("{} => {} via {}", src, dest, server.tag);
            ConnectedClient { left, right, dest, server }
        }).map_err(|_| warn!("all proxy server down"));
        Box::new(client)
    }
}

impl Connectable for NewClient {
    fn connect_server(self, _n_parallel: usize) -> ConnectServer {
        self.connect_server(1, false, None)
    }
}

impl Connectable for NewClientWithData {
    fn connect_server(self, n_parallel: usize) -> ConnectServer {
        let NewClientWithData {
            client, pending_data, allow_parallel } = self;
        let n_parallel = if allow_parallel {
            cmp::min(client.list.len(), n_parallel)
        } else {
            1
        };
        client.connect_server(n_parallel, true, pending_data)
    }
}

impl ConnectedClient {
    pub fn serve(self, shared_buf: SharedBuf)
            -> Box<Future<Item=(), Error=()>> {
        let ConnectedClient { left, right, dest, server } = self;
        // TODO: make keepalive configurable
        let timeout = Some(Duration::from_secs(300));
        if let Err(e) = left.set_keepalive(timeout)
                .and(right.set_keepalive(timeout)) {
            warn!("fail to set keepalive: {}", e);
        }

        server.update_stats_conn_open();
        let serve = pipe(left, right, server.clone(), shared_buf)
            .then(move |result| match result {
                Ok((tx, rx)) => {
                    server.update_stats_conn_close();
                    debug!("tx {}, rx {} bytes ({} => {})",
                        tx, rx, server.tag, dest);
                    Ok(())
                },
                Err(_) => {
                    server.update_stats_conn_close();
                    warn!("{} (=> {}) close with error",
                        server.tag, dest);
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

