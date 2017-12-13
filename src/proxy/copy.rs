use std::rc::Rc;
use std::net::Shutdown;
use std::io::{self, Read, Write};
use futures::{Async, Poll, Future};
use tokio_core::net::TcpStream;
use proxy::ProxyServer;
use self::Side::{Left, Right};

enum Side { Left, Right }

struct StreamWithBuffer {
    pub stream: TcpStream,
    pub buf: Box<[u8]>,
    pos: usize,
    cap: usize,
    pub read_eof: bool,
    pub all_done: bool,
}

impl StreamWithBuffer {
    pub fn new(stream: TcpStream) -> Self {
        StreamWithBuffer {
            stream,
            buf: Box::new([0; 2048]),
            pos: 0, cap: 0,
            read_eof: false,
            all_done: false,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.pos == self.cap
    }

    pub fn read_to_buffer(&mut self) -> io::Result<usize> {
        let n = self.stream.read(&mut self.buf)?;
        if n == 0 {
            self.read_eof = true;
        } else {
            self.pos = 0;
            self.cap = n;
        }
        Ok(n)
    }

    pub fn write_to(&mut self, writer: &mut TcpStream) -> io::Result<usize> {
        let n = writer.write(&self.buf[self.pos..self.cap])?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::WriteZero,
                                      "write zero byte into writer"));
        } else {
            self.pos += n;
        }
        Ok(n)
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

pub fn pipe(left: TcpStream, right: TcpStream, server: Rc<ProxyServer>)
        -> BiPipe {
    BiPipe {
        left: StreamWithBuffer::new(left),
        right: StreamWithBuffer::new(right),
        server, tx: 0, rx: 0,
    }
}

impl BiPipe {
    fn poll_one_side(&mut self, side: Side) -> Poll<(), io::Error> {
        loop {
            let (reader, writer) = match side {
                Left => (&mut self.left, &mut self.right),
                Right => (&mut self.right, &mut self.left),
            };
            // read something if buffer is empty
            if reader.is_empty() && !reader.read_eof {
                let n = try_nb!(reader.read_to_buffer());
                let (tx, rx) = match side {
                    Left => (n, 0),
                    Right => (0, n),
                };
                self.tx += tx;
                self.rx == rx;
                self.server.update_traffics(tx, rx);
            }
            // write out if buffer is not empty
            while !reader.is_empty() {
                try_nb!(reader.write_to(&mut writer.stream));
            }
            // flush and does half close if seen eof
            if reader.read_eof {
                try_nb!(writer.stream.flush());
                try_nb!(writer.stream.shutdown(Shutdown::Write));
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
        // TODO: log side of io error.
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

