use log::trace;
use std::{
    cell::RefCell,
    fmt,
    future::Future,
    io,
    net::Shutdown,
    ops::Neg,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    thread_local,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
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

const BUF_SIZE: usize = 4096;

thread_local!(
    static SHARED_BUFFER: RefCell<[u8; BUF_SIZE]> = RefCell::new([0u8; BUF_SIZE]);
);

struct StreamWithBuffer {
    pub stream: TcpStream,
    buf: Option<Box<[u8]>>,
    pos: usize,
    cap: usize,
    pub read_eof: bool,
    pub all_done: bool,
}

impl StreamWithBuffer {
    pub fn new(stream: TcpStream) -> Self {
        StreamWithBuffer {
            stream,
            buf: None,
            pos: 0,
            cap: 0,
            read_eof: false,
            all_done: false,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.pos == self.cap
    }

    pub fn poll_read_to_buffer(&mut self, cx: &mut Context) -> Poll<io::Result<usize>> {
        let stream = Pin::new(&mut self.stream);

        let n = try_poll!(if let Some(ref mut buf) = self.buf {
            stream.poll_read(cx, buf)
        } else {
            SHARED_BUFFER.with(|buf| stream.poll_read(cx, &mut buf.borrow_mut()[..]))
        });

        if n == 0 {
            self.read_eof = true;
        } else {
            self.pos = 0;
            self.cap = n;
        }
        trace!("{} bytes read", n);
        Poll::Ready(Ok(n))
    }

    pub fn poll_write_buffer_to(
        &mut self,
        cx: &mut Context,
        writer: &mut TcpStream,
    ) -> Poll<io::Result<usize>> {
        let writer = Pin::new(writer);

        let result = if let Some(ref buf) = self.buf {
            writer.poll_write(cx, &buf[self.pos..self.cap])
        } else {
            SHARED_BUFFER.with(|buf| writer.poll_write(cx, &buf.borrow_mut()[self.pos..self.cap]))
        };
        match result {
            Poll::Ready(Ok(0)) => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "write zero byte into writer",
            ))),
            Poll::Ready(Ok(n)) => {
                self.pos += n;
                trace!("{} bytes written", n);
                Poll::Ready(Ok(n))
            }
            Poll::Pending if self.buf.is_none() => {
                // Move remaining data to the private buffer
                let n = self.cap - self.pos;
                trace!("allocate private buffer for {} bytes", n);
                SHARED_BUFFER.with(|shared_buf| {
                    let shared_buf = shared_buf.borrow();
                    let mut buf = vec![0; shared_buf.len()];
                    buf[..n].copy_from_slice(&shared_buf[self.pos..self.cap]);
                    self.pos = 0;
                    self.cap = n;
                    self.buf = Some(buf.into_boxed_slice());
                });
                Poll::Pending
            }
            _ => result,
        }
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

pub fn pipe(left: TcpStream, right: TcpStream, server: Arc<ProxyServer>) -> BiPipe {
    let (left, right) = (StreamWithBuffer::new(left), StreamWithBuffer::new(right));
    BiPipe {
        left,
        right,
        server,
        traffic: Default::default(),
    }
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
                let n = try_poll!(reader.poll_read_to_buffer(cx));
                let amt = match side {
                    Left => (n, 0),
                    Right => (0, n),
                }
                .into();
                server.add_traffic(amt);
                *traffic += amt;
            }
            // write out if buffer is not empty
            while !reader.is_empty() {
                try_poll!(reader.poll_write_buffer_to(cx, &mut writer.stream));
            }
            // flush and does half close if seen eof
            if reader.read_eof {
                try_poll!(Pin::new(&mut writer.stream).poll_flush(cx));
                drop(writer.stream.shutdown(Shutdown::Write));
                reader.all_done = true;
                return Poll::Ready(Ok(()));
            }
        }
    }
}

impl Future for BiPipe {
    type Output = io::Result<Traffic>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<Traffic>> {
        if !self.left.all_done {
            trace!("poll left");
            if let Poll::Ready(Err(err)) = self.poll_one_side(cx, Left) {
                return Poll::Ready(Err(err));
            }
        }
        if !self.right.all_done {
            trace!("poll right");
            if let Poll::Ready(Err(err)) = self.poll_one_side(cx, Right) {
                return Poll::Ready(Err(err));
            }
        }
        if self.left.all_done && self.right.all_done {
            trace!("all done");
            Poll::Ready(Ok(self.traffic))
        } else {
            trace!("pending");
            Poll::Pending
        }
    }
}
