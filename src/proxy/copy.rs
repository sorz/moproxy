use log::trace;
use nix::{
    Error,
    errno::EWOULDBLOCK,
    fcntl::{SpliceFFlags, splice},
    unistd::pipe as unix_pipe,
};
use std::{
    fmt,
    future::Future,
    io,
    net::Shutdown,
    ops::Neg,
    os::unix::io::AsRawFd,
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

// Pipe two TcpStream in both direction,
// update traffic amount to ProxyServer on the fly.
pub struct BiPipe {
    left: TcpStream,
    right: TcpStream,
    left_done: bool,
    right_done: bool,
    server: Arc<ProxyServer>,
    traffic: Traffic,
}

pub fn pipe(left: TcpStream, right: TcpStream, server: Arc<ProxyServer>) -> BiPipe {
    BiPipe {
        left,
        right,
        server,
        traffic: Default::default(),
        left_done: false,
        right_done: false,
    }
}

impl BiPipe {
    fn poll_one_side(&mut self, cx: &mut Context, side: Side) -> Poll<io::Result<usize>> {
        let Self {
            ref mut left,
            ref mut right,
            ref mut server,
            ref mut traffic,
            ..
        } = *self;
        let (reader, mut writer) = match side {
            Left => (left, right),
            Right => (right, left),
        };

        match splice(
            reader.as_raw_fd(), None,
            writer.as_raw_fd(), None,
            CHUNK_SIZE,
            SpliceFFlags::SPLICE_F_NONBLOCK,
        ) {
            Ok(0) => {
                // flush and does half close on EoF
                try_poll!(Pin::new(&mut writer).poll_flush(cx));
                drop(writer.shutdown(Shutdown::Write));
                Poll::Ready(Ok(0))
            }
            Ok(n) => {
                let amt = match side {
                    Left => (n, 0),
                    Right => (0, n),
                }
                .into();
                server.add_traffic(amt);
                *traffic += amt;
                Poll::Ready(Ok(0))
            }
            Err(Error::Sys(EWOULDBLOCK)) => Poll::Pending,
            Err(Error::Sys(err)) => Poll::Ready(Err(err.into())),
            Err(err) => Poll::Ready(Err(io::Error::new(io::ErrorKind::Other, err))),
        }
    }
}

impl Future for BiPipe {
    type Output = io::Result<Traffic>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<Traffic>> {
        while !self.left_done {
            trace!("poll left");
            match self.poll_one_side(cx, Left) {
                Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
                Poll::Ready(Ok(0)) => self.left_done = true,
                Poll::Ready(Ok(n)) => trace!("{} bytes copied", n),
                Poll::Pending => break,
            }
        }
        while !self.right_done {
            trace!("poll left");
            match self.poll_one_side(cx, Right) {
                Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
                Poll::Ready(Ok(0)) => self.right_done = true,
                Poll::Ready(Ok(n)) => trace!("{} bytes copied", n),
                Poll::Pending => break,
            }
        }
        if self.left_done && self.right_done {
            trace!("all done");
            Poll::Ready(Ok(self.traffic))
        } else {
            trace!("pending");
            Poll::Pending
        }
    }
}
