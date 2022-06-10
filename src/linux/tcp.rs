use libc::{self, c_void, setsockopt, socklen_t, IPPROTO_TCP, TCP_CONGESTION};
use nix::{
    self,
    sys::socket::{getsockopt, sockopt::OriginalDst},
};
use std::{
    ffi::OsStr,
    io::{self, ErrorKind},
    mem,
    net::{SocketAddr, SocketAddrV4, SocketAddrV6},
    os::unix::io::AsRawFd,
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
        let alg = alg.as_ref();
        let ret = unsafe {
            setsockopt(
                self.as_raw_fd(),
                IPPROTO_TCP,
                TCP_CONGESTION,
                alg as *const _ as *const c_void,
                mem::size_of_val(alg) as socklen_t,
            )
        };
        if ret == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

fn get_original_dest_v4<F>(fd: &F) -> io::Result<SocketAddrV4>
where
    F: AsRawFd,
{
    let addr = getsockopt(fd.as_raw_fd(), OriginalDst)?;
    Ok(SocketAddrV4::new(
        u32::from_be(addr.sin_addr.s_addr).into(),
        u16::from_be(addr.sin_port),
    ))
}

fn get_original_dest_v6<F>(fd: &F) -> io::Result<SocketAddrV6>
where
    F: AsRawFd,
{
    let mut sockaddr: libc::sockaddr_in6 = unsafe { mem::zeroed() };
    let mut socklen = mem::size_of::<libc::sockaddr_in6>();
    let res = unsafe {
        libc::getsockopt(
            fd.as_raw_fd(),
            libc::SOL_IPV6,
            libc::SO_ORIGINAL_DST,
            &mut sockaddr as *mut _ as *mut c_void,
            &mut socklen as *mut _ as *mut socklen_t,
        )
    };
    if res != 0 {
        return Err(io::Error::last_os_error());
    }
    let addr = SocketAddrV6::new(
        sockaddr.sin6_addr.s6_addr.into(),
        u16::from_be(sockaddr.sin6_port),
        sockaddr.sin6_flowinfo,
        sockaddr.sin6_scope_id,
    );
    Ok(addr)
}
