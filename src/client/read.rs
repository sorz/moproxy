// Copy from
// https://docs.rs/tokio-io/0.1.4/src/tokio_io/read.rs.html
//
// Modified to support timeout.
use std::io;
use std::mem;
use std::time::Duration;

use futures::{Future, Poll};
use tokio_core::reactor::{Handle, Timeout};
use tokio_io::{try_nb, AsyncRead};

#[derive(Debug)]
enum State<R, T> {
    Pending { rd: R, buf: T, t: Timeout },
    Empty,
}

/// Tries to read some bytes directly into the given `buf` in asynchronous
/// manner, returning a future type.
///
/// The returned future will resolve to both the I/O stream and the buffer
/// as well as the number of bytes read once the read operation is completed.
///
/// MODIFIED: When timed out, the future will resolve as normal expect the
/// number of bytes is zero.
pub fn read_with_timeout<R, T>(rd: R, buf: T, t: Duration, handle: &Handle) -> Read<R, T>
where
    R: AsyncRead,
    T: AsMut<[u8]>,
{
    let t = Timeout::new(t, handle).expect("error on get timeout from reactor");
    Read {
        state: State::Pending { rd, buf, t },
    }
}

/// A future which can be used to easily read available number of bytes to fill
/// a buffer.
///
/// Created by the [`read`] function.
#[derive(Debug)]
pub struct Read<R, T> {
    state: State<R, T>,
}

impl<R, T> Future for Read<R, T>
where
    R: AsyncRead,
    T: AsMut<[u8]>,
{
    type Item = (R, T, usize);
    type Error = io::Error;

    fn poll(&mut self) -> Poll<(R, T, usize), io::Error> {
        let nread = match self.state {
            State::Pending {
                ref mut rd,
                ref mut buf,
                ref mut t,
            } => {
                if t.poll()?.is_ready() {
                    0
                } else {
                    try_nb!(rd.read(&mut buf.as_mut()[..]))
                }
            }
            State::Empty => panic!("poll a Read after it's done"),
        };

        match mem::replace(&mut self.state, State::Empty) {
            State::Pending { rd, buf, .. } => Ok((rd, buf, nread).into()),
            State::Empty => panic!("invalid internal state"),
        }
    }
}
