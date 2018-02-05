use std::mem;
use std::ffi::OsStr;
use std::io::{self, ErrorKind};
use std::net::{SocketAddr, SocketAddrV4};
use std::os::unix::io::AsRawFd;
use nix::{self, sys};
use libc::{c_void, socklen_t, setsockopt, IPPROTO_TCP, TCP_CONGESTION};

pub fn get_original_dest<F>(fd: &F) -> io::Result<SocketAddr>
where F: AsRawFd {
    let addr = sys::socket::getsockopt(fd.as_raw_fd(),
                                       sys::socket::sockopt::OriginalDst)
        .map_err(|e| match e {
            nix::Error::Sys(err) => io::Error::from(err),
            _ => io::Error::new(ErrorKind::Other, e),
        })?;
    let addr = SocketAddrV4::new(addr.sin_addr.s_addr.to_be().into(),
                                 addr.sin_port.to_be());
    // TODO: support IPv6
    Ok(SocketAddr::V4(addr))
}

pub fn set_congestion<F, S>(fd: &F, alg: S) -> io::Result<()>
where F: AsRawFd,
      S: AsRef<OsStr> {
    let alg = alg.as_ref();
    let ret = unsafe {
        setsockopt(
            fd.as_raw_fd(),
            IPPROTO_TCP,
            TCP_CONGESTION,
            alg as *const _ as *const c_void,
            mem::size_of_val(alg) as socklen_t
        )
    };
    if ret == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
