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
    fmt,
    io::{Error as IoError, Result as IoResult},
    mem,
    os::{raw::c_short, unix::io::RawFd},
};
use tokio::io::unix::AsyncFd;

const TUNSETIFF: u64 = 0x4004_54ca;
const CLONE_DEVICE_PATH: &[u8] = b"/dev/net/tun\0";
type IfName = [u8; libc::IFNAMSIZ];

#[repr(C)]
struct Ifreq {
    name: IfName,
    flags: c_short,
    _pad: [u8; 64],
}

// man 7 rtnetlink
// Layout from: https://elixir.bootlin.com/linux/latest/source/include/uapi/linux/rtnetlink.h#L516
#[repr(C)]
struct IfInfomsg {
    ifi_family: libc::c_uchar,
    __ifi_pad: libc::c_uchar,
    ifi_type: libc::c_ushort,
    ifi_index: libc::c_int,
    ifi_flags: libc::c_uint,
    ifi_change: libc::c_uint,
}

#[derive(Debug)]
pub enum TunEvent {
    Up(usize), // interface is up (supply MTU)
    Down,      // interface is down
}

pub struct Tun {
    fd: AsyncFd<RawFd>,
    pub status: TunStatus,
}

pub struct TunStatus {
    events: Vec<TunEvent>,
    index: i32,
    name: IfName,
    fd: AsyncFd<RawFd>,
}

#[derive(Debug)]
pub enum TunError {
    InvalidTunDeviceName,
    FailedToOpenCloneDevice,
    SetIFFIoctlFailed(IoError),
    GetMTUIoctlFailed(IoError),
    SetFlagsFcntlFailed(IoError),
    NetlinkFailure,
    Closed, // TODO
    FailedToRegisterFd(IoError),
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
            TunError::GetMTUIoctlFailed(err) => write!(f, "ifmtu ioctl failed - {}", err),
            TunError::NetlinkFailure => write!(f, "Netlink listener error"),
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

fn get_ifindex(name: &IfName) -> i32 {
    debug_assert_eq!(
        name[libc::IFNAMSIZ - 1],
        0,
        "name buffer not null-terminated"
    );

    let name = *name;
    let idx = unsafe {
        let ptr: *const libc::c_char = mem::transmute(&name);
        libc::if_nametoindex(ptr)
    };
    idx as i32
}

fn set_non_blocking(fd: RawFd) -> Result<(), TunError> {
    let flags = match unsafe { libc::fcntl(fd, libc::F_GETFL) } {
        -1 => 0,
        n => n,
    } | libc::O_NONBLOCK;
    let n = unsafe { libc::fcntl(fd, libc::F_SETFL, flags) };
    if n < 0 {
        Err(TunError::SetFlagsFcntlFailed(IoError::last_os_error()))
    } else {
        Ok(())
    }
}

fn get_mtu(name: &IfName) -> Result<usize, TunError> {
    #[repr(C)]
    struct arg {
        name: IfName,
        mtu: u32,
    }

    debug_assert_eq!(
        name[libc::IFNAMSIZ - 1],
        0,
        "name buffer not null-terminated"
    );

    // create socket
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
    if fd < 0 {
        return Err(TunError::GetMTUIoctlFailed(IoError::last_os_error()));
    }

    // do SIOCGIFMTU ioctl
    let buf = arg {
        name: *name,
        mtu: 0,
    };
    let err = unsafe {
        let ptr: &libc::c_void = &*(&buf as *const _ as *const libc::c_void);
        libc::ioctl(fd, libc::SIOCGIFMTU, ptr)
    };
    // handle error from ioctl
    if err != 0 {
        unsafe { libc::close(fd) };
        return Err(TunError::GetMTUIoctlFailed(IoError::last_os_error()));
    }
    unsafe { libc::close(fd) };

    // upcast to usize
    Ok(buf.mtu as usize)
}

impl TunStatus {
    const RTNLGRP_LINK: libc::c_uint = 1;
    const RTNLGRP_IPV4_IFADDR: libc::c_uint = 5;
    const RTNLGRP_IPV6_IFADDR: libc::c_uint = 9;

    fn new(name: IfName) -> Result<TunStatus, TunError> {
        // create netlink socket
        let fd = unsafe { libc::socket(libc::AF_NETLINK, libc::SOCK_RAW, libc::NETLINK_ROUTE) };
        if fd < 0 {
            return Err(TunError::Closed);
        }

        // prepare address (specify groups)
        let groups = (1 << (Self::RTNLGRP_LINK - 1))
            | (1 << (Self::RTNLGRP_IPV4_IFADDR - 1))
            | (1 << (Self::RTNLGRP_IPV6_IFADDR - 1));

        let mut sockaddr: libc::sockaddr_nl = unsafe { mem::zeroed() };
        sockaddr.nl_family = libc::AF_NETLINK as u16;
        sockaddr.nl_groups = groups;
        sockaddr.nl_pid = 0;

        // attempt to bind
        let res = unsafe {
            libc::bind(
                fd,
                mem::transmute(&mut sockaddr),
                mem::size_of::<libc::sockaddr_nl>() as u32,
            )
        };

        if res != 0 {
            Err(TunError::Closed)
        } else {
            Ok(TunStatus {
                events: vec![
                    #[cfg(feature = "start_up")]
                    TunEvent::Up(1500),
                ],
                index: get_ifindex(&name),
                fd: AsyncFd::new(fd).map_err(TunError::FailedToRegisterFd)?,
                name,
            })
        }
    }

    pub fn event(&mut self) -> Result<TunEvent, TunError> {
        const DONE: u16 = libc::NLMSG_DONE as u16;
        const ERROR: u16 = libc::NLMSG_ERROR as u16;
        const INFO_SIZE: usize = mem::size_of::<IfInfomsg>();
        const HDR_SIZE: usize = mem::size_of::<libc::nlmsghdr>();

        let mut buf = [0u8; 1 << 12];
        log::debug!("netlink, fetch event (fd = {})", self.fd.get_ref());
        loop {
            // attempt to return a buffered event
            if let Some(event) = self.events.pop() {
                return Ok(event);
            }

            // read message
            let size: libc::ssize_t =
                unsafe { libc::recv(*self.fd.get_ref(), mem::transmute(&mut buf), buf.len(), 0) };
            if size < 0 {
                break Err(TunError::NetlinkFailure);
            }

            // cut buffer to size
            let size: usize = size as usize;
            let mut remain = &buf[..size];
            log::debug!("netlink, received message ({} bytes)", size);

            // handle messages
            while remain.len() >= HDR_SIZE {
                // extract the header
                assert!(remain.len() > HDR_SIZE);
                let hdr: libc::nlmsghdr = unsafe {
                    let mut hdr = [0u8; HDR_SIZE];
                    hdr.copy_from_slice(&remain[..HDR_SIZE]);
                    mem::transmute(hdr)
                };

                // upcast length
                let body: &[u8] = &remain[HDR_SIZE..];
                let msg_len: usize = hdr.nlmsg_len as usize;
                assert!(msg_len <= remain.len(), "malformed netlink message");

                // handle message body
                match hdr.nlmsg_type {
                    DONE => break,
                    ERROR => break,
                    libc::RTM_NEWLINK => {
                        // extract info struct
                        if body.len() < INFO_SIZE {
                            return Err(TunError::NetlinkFailure);
                        }
                        let info: IfInfomsg = unsafe {
                            let mut info = [0u8; INFO_SIZE];
                            info.copy_from_slice(&body[..INFO_SIZE]);
                            mem::transmute(info)
                        };

                        // trace log
                        log::trace!(
                            "netlink, IfInfomsg{{ family = {}, type = {}, index = {}, flags = {}, change = {}}}",
                            info.ifi_family,
                            info.ifi_type,
                            info.ifi_index,
                            info.ifi_flags,
                            info.ifi_change,
                        );
                        debug_assert_eq!(info.__ifi_pad, 0);

                        if info.ifi_index == self.index {
                            // handle up / down
                            if info.ifi_flags & (libc::IFF_UP as u32) != 0 {
                                let mtu = get_mtu(&self.name)?;
                                log::trace!("netlink, up event, mtu = {}", mtu);
                                self.events.push(TunEvent::Up(mtu));
                            } else {
                                log::trace!("netlink, down event");
                                self.events.push(TunEvent::Down);
                            }
                        }
                    }
                    _ => (),
                };

                // go to next message
                remain = &remain[msg_len..];
            }
        }
    }
}

impl Tun {
    pub fn create(name: &str) -> Result<Self, TunError> {
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
            return Err(TunError::SetIFFIoctlFailed(IoError::last_os_error()));
        }
        let status = TunStatus::new(req.name)?;

        set_non_blocking(fd)?;
        let fd = AsyncFd::new(fd).map_err(TunError::FailedToRegisterFd)?;

        Ok(Self { fd, status })
    }

    pub async fn read(&self, buf: &mut [u8]) -> IoResult<usize> {
        loop {
            let mut guard = self.fd.readable().await?;
            match guard.try_io(|fd| read_fd(fd.get_ref(), buf)) {
                Ok(n) => break n,
                Err(_) => continue,
            }
        }
    }

    pub async fn write(&self, buf: &[u8]) -> IoResult<()> {
        loop {
            let mut guard = self.fd.writable().await?;
            match guard.try_io(|fd| write_fd(fd.get_ref(), buf)) {
                Ok(_) => break Ok(()),
                Err(_) => continue,
            }
        }
    }
}

fn read_fd(fd: &RawFd, buf: &mut [u8]) -> IoResult<usize> {
    let n = unsafe { libc::read(*fd, buf.as_mut_ptr() as _, buf.len()) };
    if n < 0 {
        Err(IoError::last_os_error())
    } else {
        Ok(n as usize)
    }
}

fn write_fd(fd: &RawFd, buf: &[u8]) -> IoResult<()> {
    match unsafe { libc::write(*fd, buf.as_ptr() as _, buf.len() as _) } {
        -1 => Err(IoError::last_os_error()),
        _ => Ok(()),
    }
}
