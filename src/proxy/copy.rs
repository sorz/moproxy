use log::trace;
use nix::{
    Error,
    errno::EWOULDBLOCK,
    fcntl::{SpliceFFlags, splice as unix_splice, OFlag},
    unistd::pipe2 as unix_pipe,
};
use std::{
    fmt,
    future::Future,
    io,
    net::Shutdown,
    ops::Neg,
    os::unix::io::{RawFd, AsRawFd},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::{
    io::AsyncWrite,
    net::TcpStream,
};

use self::Side::{Left, Right};
use crate::proxy::{ProxyServer, Traffic};

#[derive(Debug, Clone)]
enum Side {
    Left,
    Right,
}

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Left => write!(f, "local"),
            Right => write!(f, "remote"),
        }
    }
}

impl Neg for Side {
    type Output = Side;

    fn neg(self) -> Side {
        match self {
            Left => Right,
            Right => Left,
        }
    }
}

macro_rules! try_poll {
    ($expr:expr) => {
        match $expr {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
            Poll::Ready(Ok(v)) => v,
        }
    };
}

const CHUNK_SIZE: usize = 64 * 1024;

struct StreamWithBuffer {
    pub stream: TcpStream,
    pipe_in: RawFd,
    pipe_out: RawFd,
    bytes_in_pipe: usize,
    pub read_eof: bool,
}

fn splice(input: RawFd, output: RawFd) -> Poll<io::Result<usize>> {
    let flags = SpliceFFlags::SPLICE_F_NONBLOCK;
    trace!("splice {} to {}",input, output);
    match unix_splice(input, None, output, None, CHUNK_SIZE, flags) {
        Ok(n) => Poll::Ready(Ok(n)),
        Err(Error::Sys(EWOULDBLOCK)) => Poll::Pending,
        Err(Error::Sys(err)) => Poll::Ready(Err(err.into())),
        Err(err) => Poll::Ready(Err(io::Error::new(io::ErrorKind::Other, err))),
    }
}

impl StreamWithBuffer {
    pub fn new(stream: TcpStream) -> io::Result<Self> {
        let (pipe_in, pipe_out) = match unix_pipe(OFlag::O_NONBLOCK | OFlag::O_CLOEXEC) {
            Ok(v) => v,
            Err(Error::Sys(e)) => return Err(e.into()),
            Err(e) => return Err(io::Error::new(io::ErrorKind::Other, e)),
        };
        trace!("pipe ({}, {}) created", pipe_in, pipe_out);
        Ok(Self {
            stream, pipe_in, pipe_out,
            bytes_in_pipe: 0,
            read_eof: false,
        })
    }

    pub fn all_done(&self) -> bool {
        self.read_eof && self.is_empty()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes_in_pipe == 0
    }

    pub fn poll_read_to_buffer(&mut self) -> Poll<io::Result<usize>> {
        let n = try_poll!(splice(self.stream.as_raw_fd(), self.pipe_out));
        if n == 0 {
            self.read_eof = true;
        } else {
            self.bytes_in_pipe += n;
        }
        trace!("{} bytes read", n);
        Poll::Ready(Ok(n))
    }

    pub fn poll_write_buffer_to(
        &mut self,
        writer: &mut TcpStream
    ) -> Poll<io::Result<usize>> {
        let n = try_poll!(splice(self.pipe_in, writer.as_raw_fd()));
        Poll::Ready(if n == 0 {
            Err(io::Error::from(io::ErrorKind::WriteZero))
        } else {
            trace!("{} bytes written", n);
            self.bytes_in_pipe -= n;
            Ok(n)
        })
    }
}

// Pipe two TcpStream in both direction,
// update traffic amount to ProxyServer on the fly.
pub struct BiPipe {
    left: StreamWithBuffer,
    right: StreamWithBuffer,
    server: Arc<ProxyServer>,
    traffic: Traffic,
}

pub fn pipe(left: TcpStream, right: TcpStream, server: Arc<ProxyServer>) -> io::Result<BiPipe> {
    let (left, right) = (StreamWithBuffer::new(left)?, StreamWithBuffer::new(right)?);
    Ok(BiPipe {
        left,
        right,
        server,
        traffic: Default::default(),
    })
}

impl BiPipe {
    fn poll_one_side(&mut self, cx: &mut Context, side: Side) -> Poll<io::Result<()>> {
        let Self {
            ref mut left,
            ref mut right,
            ref mut server,
            ref mut traffic,
        } = *self;
        let (reader, writer) = match side {
            Left => (left, right),
            Right => (right, left),
        };
        loop {
            // read something if buffer is empty
            if reader.is_empty() && !reader.read_eof {
                let n = try_poll!(reader.poll_read_to_buffer());
                let amt = match side {
                    Left => (n, 0),
                    Right => (0, n),
                }
                .into();
                server.add_traffic(amt);
                *traffic += amt;
            }
            // write out unitl buffer is not empty
            while !reader.is_empty() {
                try_poll!(reader.poll_write_buffer_to(&mut writer.stream));
            }
            // flush and does half close if seen eof
            if reader.read_eof {
                try_poll!(Pin::new(&mut writer.stream).poll_flush(cx));
                drop(writer.stream.shutdown(Shutdown::Write));
                return Poll::Ready(Ok(()));
            }
        }
    }
}

impl Future for BiPipe {
    type Output = io::Result<Traffic>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<Traffic>> {
        if !self.left.all_done() {
            trace!("poll left");
            if let Poll::Ready(Err(err)) = self.poll_one_side(cx, Left) {
                return Poll::Ready(Err(err));
            }
        }
        if !self.right.all_done() {
            trace!("poll right");
            if let Poll::Ready(Err(err)) = self.poll_one_side(cx, Right) {
                return Poll::Ready(Err(err));
            }
        }

        if self.left.all_done() && self.right.all_done() {
            trace!("all done");
            Poll::Ready(Ok(self.traffic))
        } else {
            trace!("pending");
            Poll::Pending
        }
    }
}
