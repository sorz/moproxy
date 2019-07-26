use log::debug;
use std::{
    cell::RefCell,
    fmt,
    future::Future,
    io,
    net::Shutdown,
    ops::Neg,
    pin::Pin,
    rc::Rc,
    sync::Arc,
    task::{Context, Poll},
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

#[derive(Clone)]
struct SharedBuf {
    size: usize,
    buf: Rc<RefCell<Option<Box<[u8]>>>>,
}

impl SharedBuf {
    /// Create a empty buffer.
    pub fn new(size: usize) -> Self {
        SharedBuf {
            size,
            buf: Rc::new(RefCell::new(None)),
        }
    }

    /// Take the inner buffer or allocate a new one.
    fn take(&self) -> Box<[u8]> {
        self.buf.borrow_mut().take().unwrap_or_else(|| {
            debug!("allocate new buffer");
            vec![0; self.size].into_boxed_slice()
        })
    }

    fn is_empty(&self) -> bool {
        self.buf.borrow().is_none()
    }

    /// Return the taken buffer back.
    fn set(&self, buf: Box<[u8]>) {
        self.buf.borrow_mut().get_or_insert(buf);
    }
}

thread_local! {
    static SHARED_BUF: SharedBuf = SharedBuf::new(8192);
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

    fn get_shared_buf(&self) -> SharedBuf {
        SHARED_BUF.with(|buf| buf.clone())
    }

    /// Take buffer from shared_buf or (private) buf.
    fn take_buf(&mut self) -> Box<[u8]> {
        if let Some(buf) = self.buf.take() {
            buf
        } else {
            self.get_shared_buf().take()
        }
    }

    /// Return buffer to shared_buf or (private) buf.
    fn return_buf(&mut self, buf: Box<[u8]>) {
        let shared_buf = self.get_shared_buf();
        if shared_buf.is_empty() && self.is_empty() {
            shared_buf.set(buf);
        } else {
            self.buf.get_or_insert(buf);
        }
    }

    pub fn poll_read_to_buffer(&mut self, cx: &mut Context) -> Poll<io::Result<usize>> {
        let mut buf = self.take_buf();
        let stream = Pin::new(&mut self.stream);

        let n = try_poll!(stream.poll_read(cx, &mut buf));
        if n == 0 {
            self.read_eof = true;
        } else {
            self.pos = 0;
            self.cap = n;
        }
        self.return_buf(buf);
        Poll::Ready(Ok(n))
    }

    pub fn poll_write_buffer_to(
        &mut self,
        cx: &mut Context,
        writer: &mut TcpStream,
    ) -> Poll<io::Result<usize>> {
        let buf = self.buf.take().expect("try to write empty buffer");
        let writer = Pin::new(writer);
        let n = try_poll!(writer.poll_write(cx, &buf[self.pos..self.cap]));
        if n == 0 {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "write zero byte into writer",
            )));
        } else {
            self.pos += n;
        }
        self.return_buf(buf);
        Poll::Ready(Ok(n))
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
            try_poll!(self.poll_one_side(cx, Left));
        }
        if !self.right.all_done {
            try_poll!(self.poll_one_side(cx, Right));
        }
        if self.left.all_done && self.right.all_done {
            Poll::Ready(Ok(self.traffic))
        } else {
            Poll::Pending
        }
    }
}
