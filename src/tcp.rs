use libc::{self, c_void, setsockopt, socklen_t, IPPROTO_TCP, TCP_CONGESTION};
use nix::{self, sys};
use std::{
    ffi::OsStr,
    io::{self, ErrorKind},
    mem,
    net::{SocketAddrV4, SocketAddrV6},
    os::unix::io::AsRawFd,
};

pub fn get_original_dest<F>(fd: &F) -> io::Result<SocketAddrV4>
where
    F: AsRawFd,
{
    let addr = sys::socket::getsockopt(fd.as_raw_fd(), sys::socket::sockopt::OriginalDst).map_err(
        |e| match e {
            nix::Error::Sys(err) => io::Error::from(err),
            _ => io::Error::new(ErrorKind::Other, e),
        },
    )?;
    let addr = SocketAddrV4::new(addr.sin_addr.s_addr.to_be().into(), addr.sin_port.to_be());
    Ok(addr)
}

pub fn get_original_dest6<F>(fd: &F) -> io::Result<SocketAddrV6>
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
        return Err(io::Error::new(ErrorKind::Other, "getsockopt fail"));
    }
    let addr = SocketAddrV6::new(
        sockaddr.sin6_addr.s6_addr.into(),
        sockaddr.sin6_port,
        sockaddr.sin6_flowinfo,
        sockaddr.sin6_scope_id,
    );
    Ok(addr)
}

pub fn set_congestion<F, S>(fd: &F, alg: S) -> io::Result<()>
where
    F: AsRawFd,
    S: AsRef<OsStr>,
{
    let alg = alg.as_ref();
    let ret = unsafe {
        setsockopt(
            fd.as_raw_fd(),
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
