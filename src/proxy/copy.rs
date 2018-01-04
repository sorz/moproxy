use std::fmt;
use std::rc::Rc;
use std::cell::RefCell;
use std::net::Shutdown;
use std::io::{self, Read, Write};
use std::io::ErrorKind::WouldBlock;
use futures::{Async, Poll, Future};
use tokio_core::net::TcpStream;
use proxy::ProxyServer;
use self::Side::{Left, Right};

#[derive(Debug, Clone)]
enum Side { Left, Right }

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Left => write!(f, "local"),
            Right => write!(f, "remote"),
        }
    }
}

#[derive(Clone)]
pub struct SharedBuf {
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
        self.buf.borrow_mut().take()
            .unwrap_or_else(|| {
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

struct StreamWithBuffer {
    pub stream: TcpStream,
    buf: Option<Box<[u8]>>,
    shared_buf: SharedBuf,
    pos: usize,
    cap: usize,
    pub read_eof: bool,
    pub all_done: bool,
}

impl StreamWithBuffer {
    pub fn new(stream: TcpStream, shared_buf: SharedBuf) -> Self {
        StreamWithBuffer {
            stream, shared_buf,
            buf: None,
            pos: 0, cap: 0,
            read_eof: false,
            all_done: false,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.pos == self.cap
    }

    /// Take buffer from shared_buf or (private) buf.
    fn take_buf(&mut self) -> Box<[u8]> {
        if let Some(buf) = self.buf.take() {
            buf
        } else {
            self.shared_buf.take()
        }
    }

    /// Return buffer to shared_buf or (private) buf.
    fn return_buf(&mut self, buf: Box<[u8]>) {
        if self.shared_buf.is_empty() && self.is_empty() {
            self.shared_buf.set(buf);
        } else {
            self.buf.get_or_insert(buf);
        }
    }

    pub fn read_to_buffer(&mut self) -> io::Result<usize> {
        let mut buf = self.take_buf();
        let result = (|buf| {
            let n = self.stream.read(buf)?;
            if n == 0 {
                self.read_eof = true;
            } else {
                self.pos = 0;
                self.cap = n;
            }
            Ok(n)
        })(&mut buf);
        self.return_buf(buf);
        return result;
    }

    pub fn write_to(&mut self, writer: &mut TcpStream) -> io::Result<usize> {
        let buf = self.buf.take().expect("try to write empty buffer");
        let result = (|buf: &Box<[u8]>| {
            let n = writer.write(&buf[self.pos..self.cap])?;
            if n == 0 {
                return Err(io::Error::new(io::ErrorKind::WriteZero,
                                          "write zero byte into writer"));
            } else {
                self.pos += n;
            }
            Ok(n)
        })(&buf);
        self.return_buf(buf);
        return result;
    }
}


// Pipe two TcpStream in both direction,
// update traffic amount to ProxyServer on the fly.
pub struct BiPipe {
    left: StreamWithBuffer,
    right: StreamWithBuffer,
    server: Rc<ProxyServer>,
    tx: usize,
    rx: usize,
}

pub fn pipe(left: TcpStream, right: TcpStream, server: Rc<ProxyServer>,
            shared_buf: SharedBuf)
        -> BiPipe {
    BiPipe {
        left: StreamWithBuffer::new(left, shared_buf.clone()),
        right: StreamWithBuffer::new(right, shared_buf),
        server, tx: 0, rx: 0,
    }
}

/// try_nb! with custom log for error.
macro_rules! try_nb_log {
    ($e:expr, $( $p:expr ),+) => (match $e {
        Ok(t) => t,
        Err(ref e) if e.kind() == WouldBlock => return Ok(Async::NotReady),
        Err(e) => {
            info!($( $p ),+, e);
            return Err(e.into());
        },
    })
}

impl BiPipe {
    fn poll_one_side(&mut self, side: Side) -> Poll<(), io::Error> {
        let (reader, writer) = match side {
            Left => (&mut self.left, &mut self.right),
            Right => (&mut self.right, &mut self.left),
        };
        loop {
            // read something if buffer is empty
            if reader.is_empty() && !reader.read_eof {
                let n = try_nb_log!(reader.read_to_buffer(),
                        "error on read {}: {}", side);
                let (tx, rx) = match side {
                    Left => (n, 0),
                    Right => (0, n),
                };
                self.tx += tx;
                self.rx += rx;
                self.server.update_traffics(tx, rx);
            }
            // write out if buffer is not empty
            while !reader.is_empty() {
                try_nb_log!(reader.write_to(&mut writer.stream),
                        "error on write {}: {}", side);
            }
            // flush and does half close if seen eof
            if reader.read_eof {
                try_nb_log!(writer.stream.flush(),
                        "error on flush {}: {}", side);
                try_nb_log!(writer.stream.shutdown(Shutdown::Write),
                        "error on shutdown {}: {}", side);
                reader.all_done = true;
                return Ok(().into())
            }
        }
    }
}


impl Future for BiPipe {
    type Item = (usize, usize);
    type Error = io::Error;

    fn poll(&mut self) -> Poll<(usize, usize), io::Error> {
        if !self.left.all_done {
            self.poll_one_side(Left)?;
        }
        if !self.right.all_done {
            self.poll_one_side(Right)?;
        }
        if self.left.all_done && self.right.all_done {
            Ok((self.tx, self.rx).into())
        } else {
            Ok(Async::NotReady)
        }
    }
}

