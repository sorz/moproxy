use log::{debug, trace, warn};
use std::{
    cell::RefCell,
    cmp, fmt,
    future::Future,
    io,
    ops::Neg,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    thread_local,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::TcpStream,
    time::{sleep, Instant, Sleep},
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

// The number of shared buffers is fixed (equals to no. of CPU threads),
// so we can use a larger one for better performance.
const SHARED_BUF_SIZE: usize = 1024 * 64;
const PRIVATE_BUF_SIZE: usize = 1024 * 8;

thread_local!(
    static SHARED_BUFFER: RefCell<[u8; SHARED_BUF_SIZE]> = RefCell::new([0u8; SHARED_BUF_SIZE]);
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
            let mut buf = ReadBuf::new(buf);
            stream
                .poll_read(cx, &mut buf)
                .map_ok(|_| buf.filled().len())
        } else {
            SHARED_BUFFER.with(|buf| {
                let shared_buf = &mut buf.borrow_mut()[..];
                let mut buf = ReadBuf::new(shared_buf);
                stream
                    .poll_read(cx, &mut buf)
                    .map_ok(|_| buf.filled().len())
            })
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
                    let mut buf = vec![0; cmp::max(PRIVATE_BUF_SIZE, n)];
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

    pub fn shrink_private_buffer_if_need(&mut self) {
        assert!(self.is_empty());
        if let Some(ref mut buf) = self.buf {
            if buf.len() > PRIVATE_BUF_SIZE {
                trace!(
                    "shrink private buffer from {} to {} bytes",
                    buf.len(),
                    PRIVATE_BUF_SIZE
                );
                *buf = vec![0; PRIVATE_BUF_SIZE].into_boxed_slice()
            }
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
    half_close_deadline: Option<Pin<Box<Sleep>>>,
}

// Half-closed connections will be forcibly closed if there is no traffic
// after the following duration.
const HALF_CLOSE_TIMEOUT: Duration = Duration::from_secs(60);

pub fn pipe(left: TcpStream, right: TcpStream, server: Arc<ProxyServer>) -> BiPipe {
    let (left, right) = (StreamWithBuffer::new(left), StreamWithBuffer::new(right));
    BiPipe {
        left,
        right,
        server,
        traffic: Default::default(),
        half_close_deadline: Default::default(),
    }
}

impl BiPipe {
    fn poll_one_side(&mut self, cx: &mut Context, side: Side) -> Poll<io::Result<()>> {
        let Self {
            ref mut left,
            ref mut right,
            ref mut server,
            ref mut traffic,
            ..
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
            reader.shrink_private_buffer_if_need();

            // flush and does half close if seen eof
            if reader.read_eof {
                // shutdown implies flush
                match Pin::new(&mut writer.stream).poll_shutdown(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Ok(_)) => (),
                    Poll::Ready(Err(err)) => debug!("fail to shutdown: {}", err),
                }
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
            trace!("(BiPipe) poll left");
            if let Poll::Ready(Err(err)) = self.poll_one_side(cx, Left) {
                return Poll::Ready(Err(err));
            }
        }
        if !self.right.all_done {
            trace!("(BiPipe) poll right");
            if let Poll::Ready(Err(err)) = self.poll_one_side(cx, Right) {
                return Poll::Ready(Err(err));
            }
        }
        match (self.left.all_done, self.right.all_done) {
            (true, true) => Poll::Ready(Ok(self.traffic)),
            (false, false) => Poll::Pending,
            _ => {
                // Half close
                match &mut self.half_close_deadline {
                    None => {
                        // Setup a deadline then poll it
                        let mut deadline = Box::pin(sleep(HALF_CLOSE_TIMEOUT));
                        let _ = deadline.as_mut().poll(cx); // always return pending
                        self.half_close_deadline = Some(deadline);
                        Poll::Pending
                    }
                    Some(deadline) if !deadline.is_elapsed() => {
                        // FIXME: change warn to debug/trace before release
                        warn!("(BiPipe) reset half-close timer");
                        deadline.as_mut().reset(Instant::now() + HALF_CLOSE_TIMEOUT);
                        Poll::Pending
                    }
                    Some(_) => {
                        // FIXME: change warn to debug/trace before release
                        warn!("(BiPipe) half-close conn timed out");
                        Poll::Ready(Ok(self.traffic))
                    }
                }
            }
        }
    }
}
