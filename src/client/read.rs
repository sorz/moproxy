// Copy from
// https://docs.rs/tokio-io/0.1.4/src/tokio_io/read.rs.html
//
// Modified to support timeout.
use std::{
    future::Future,
    io,
    mem,
    pin::Pin,
    task::{Poll, Context},
};
use tokio::{
    io::AsyncRead,
    timer::Delay,
};

#[derive(Debug)]
enum State<R, T> {
    Pending { rd: R, buf: T, timeout: Delay },
    Empty,
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
    R: AsyncRead + Unpin,
    T: AsMut<[u8]> + Unpin,
{
    type Output = io::Result<(R, T, usize)>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<(R, T, usize)>> {
        let nread = match self.state {
            State::Pending {
                ref mut rd,
                ref mut buf,
                ref mut timeout,
            } => {
                if Pin::new(timeout).poll(cx).is_ready() {
                    0
                } else {
                    match Pin::new(rd).poll_read(cx, &mut buf.as_mut()[..]) {
                        Poll::Ready(Ok(t)) => t,
                        Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                        Poll::Pending => return Poll::Pending,
                    }
                }
            }
            State::Empty => panic!("poll a Read after it's done"),
        };

        match mem::replace(&mut self.state, State::Empty) {
            State::Pending { rd, buf, .. } => Poll::Ready(Ok((rd, buf, nread))),
            State::Empty => panic!("invalid internal state"),
        }
    }
}
