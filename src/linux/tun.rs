// MIT License

// Copyright (c) 2020 Mathias Hall-Andersen
// Copyright (c) 2021 Shell Chen (github.com/sorz)

// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:

// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.

// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.
use std::{
    error::Error,
    fmt, io,
    os::{raw::c_short, unix::io::RawFd},
};
use tokio::io::unix::AsyncFd;

const TUNSETIFF: u64 = 0x4004_54ca;
const CLONE_DEVICE_PATH: &[u8] = b"/dev/net/tun\0";
type IfName = [u8; libc::IFNAMSIZ];

#[repr(C)]
pub struct Ifreq {
    name: IfName,
    flags: c_short,
    _pad: [u8; 64],
}

pub struct Tun {
    fd: AsyncFd<RawFd>,
}

#[derive(Debug)]
pub enum TunError {
    InvalidTunDeviceName,
    FailedToOpenCloneDevice,
    SetIFFIoctlFailed(io::Error),
    SetFlagsFcntlFailed(io::Error),
    SetTunIfUpFailed,
    Closed, // TODO
    FailedToRegisterFd(io::Error),
}

impl fmt::Display for TunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TunError::InvalidTunDeviceName => write!(f, "Invalid name (too long)"),
            TunError::FailedToOpenCloneDevice => {
                write!(f, "Failed to obtain fd for clone device")
            }
            TunError::SetIFFIoctlFailed(err) => {
                write!(f, "set_iff ioctl failed - {}", err)
            }
            TunError::SetFlagsFcntlFailed(err) => {
                write!(f, "f_setfl fcntl failed - {}", err)
            }
            TunError::Closed => write!(f, "The tunnel has been closed"),
            TunError::SetTunIfUpFailed => write!(f, "Failed to set tun interface up"),
            TunError::FailedToRegisterFd(err) => {
                write!(f, "Failed to register fd to loop - {}", err)
            }
        }
    }
}

impl Error for TunError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        unimplemented!()
    }

    fn description(&self) -> &str {
        unimplemented!()
    }
}

fn set_non_blocking(fd: RawFd) -> Result<(), TunError> {
    let flags = match unsafe { libc::fcntl(fd, libc::F_GETFL) } {
        -1 => 0,
        n => n,
    } | libc::O_NONBLOCK;
    let n = unsafe { libc::fcntl(fd, libc::F_SETFL, flags) };
    if n < 0 {
        Err(TunError::SetFlagsFcntlFailed(io::Error::last_os_error()))
    } else {
        Ok(())
    }
}

fn set_if_up(name: &IfName) -> Result<(), TunError> {
    // create socket
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
    if fd < 0 {
        return Err(TunError::SetTunIfUpFailed);
    }

    let buf = Ifreq {
        name: *name,
        flags: (libc::IFF_UP | libc::IFF_RUNNING) as c_short,
        _pad: [0u8; 64],
    };

    // do SIOCSIFFLAGS ioctl
    let err = unsafe { libc::ioctl(fd, libc::SIOCSIFFLAGS, &buf) };

    // close socket
    unsafe { libc::close(fd) };

    // handle error from ioctl
    if err != 0 {
        return Err(TunError::SetTunIfUpFailed);
    }

    Ok(())
}

impl Tun {
    pub fn new(name: &str) -> Result<Self, TunError> {
        // construct request struct
        let mut req = Ifreq {
            name: [0u8; libc::IFNAMSIZ],
            flags: (libc::IFF_TUN | libc::IFF_NO_PI) as c_short,
            _pad: [0u8; 64],
        };

        // sanity check length of device name
        let bs = name.as_bytes();
        if bs.len() > libc::IFNAMSIZ - 1 {
            return Err(TunError::InvalidTunDeviceName);
        }
        req.name[..bs.len()].copy_from_slice(bs);

        // open clone device
        let fd: RawFd = match unsafe { libc::open(CLONE_DEVICE_PATH.as_ptr() as _, libc::O_RDWR) } {
            -1 => return Err(TunError::FailedToOpenCloneDevice),
            fd => fd,
        };
        assert!(fd >= 0);

        // create TUN device
        if unsafe { libc::ioctl(fd, TUNSETIFF as _, &req) } < 0 {
            return Err(TunError::SetIFFIoctlFailed(io::Error::last_os_error()));
        }

        // set device up
        set_if_up(&req.name).unwrap();

        set_non_blocking(fd)?;
        let fd = AsyncFd::new(fd).map_err(TunError::FailedToRegisterFd)?;

        Ok(Self { fd })
    }

    pub async fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let mut guard = self.fd.readable().await?;
            match guard.try_io(|fd| read_fd(fd.get_ref(), buf)) {
                Ok(n) => break n,
                Err(_) => continue,
            }
        }
    }

    pub async fn write(&self, buf: &[u8]) -> io::Result<()> {
        loop {
            let mut guard = self.fd.writable().await?;
            match guard.try_io(|fd| write_fd(fd.get_ref(), buf)) {
                Ok(_) => break Ok(()),
                Err(_) => continue,
            }
        }
    }
}

fn read_fd(fd: &RawFd, buf: &mut [u8]) -> io::Result<usize> {
    let n = unsafe { libc::read(*fd, buf.as_mut_ptr() as _, buf.len()) };
    if n < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(n as usize)
    }
}

fn write_fd(fd: &RawFd, buf: &[u8]) -> io::Result<()> {
    match unsafe { libc::write(*fd, buf.as_ptr() as _, buf.len() as _) } {
        -1 => Err(io::Error::last_os_error()),
        _ => Ok(()),
    }
}
