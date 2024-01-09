use nix::sys::socket::{
    getsockopt, setsockopt,
    sockopt::{Ip6tOriginalDst, OriginalDst, TcpCongestion},
};
use std::{
    ffi::OsStr,
    io::{self, ErrorKind},
    net::{SocketAddr, SocketAddrV4, SocketAddrV6},
    os::fd::AsFd,
};
use tokio::net::{TcpListener, TcpStream};

pub trait TcpStreamExt {
    fn get_original_dest(&self) -> io::Result<Option<SocketAddr>>;
}

pub trait TcpListenerExt {
    fn set_congestion<S: AsRef<OsStr>>(&self, alg: S) -> io::Result<()>;
}

impl TcpStreamExt for TcpStream {
    fn get_original_dest(&self) -> io::Result<Option<SocketAddr>> {
        match get_original_dest_v4(self) {
            Ok(addr) => Ok(Some(SocketAddr::V4(addr))),
            Err(err) if err.kind() == ErrorKind::NotFound => match get_original_dest_v6(self) {
                Ok(addr) => Ok(Some(SocketAddr::V6(addr))),
                Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
                Err(err) => Err(err),
            },
            Err(err) => Err(err),
        }
    }
}

impl TcpListenerExt for TcpListener {
    fn set_congestion<S: AsRef<OsStr>>(&self, alg: S) -> io::Result<()> {
        let val = alg.as_ref().into();
        setsockopt(self, TcpCongestion, &val)?;
        Ok(())
    }
}

fn get_original_dest_v4<F>(fd: &F) -> io::Result<SocketAddrV4>
where
    F: AsFd,
{
    let addr = getsockopt(fd, OriginalDst)?;
    Ok(SocketAddrV4::new(
        u32::from_be(addr.sin_addr.s_addr).into(),
        u16::from_be(addr.sin_port),
    ))
}

fn get_original_dest_v6<F>(fd: &F) -> io::Result<SocketAddrV6>
where
    F: AsFd,
{
    let sockaddr = getsockopt(fd, Ip6tOriginalDst)?;
    Ok(SocketAddrV6::new(
        sockaddr.sin6_addr.s6_addr.into(),
        u16::from_be(sockaddr.sin6_port),
        sockaddr.sin6_flowinfo,
        sockaddr.sin6_scope_id,
    ))
}
