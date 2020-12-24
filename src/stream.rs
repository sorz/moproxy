use futures_core::{ready, Stream};
use std::{
    io::Result,
    task::{Context, Poll},
};
use tokio::net::{TcpListener, TcpStream};
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};

macro_rules! impl_stream {
    ($name:ident : $listener:ty => $stream:ty) => {
        pub struct $name(pub $listener);

        impl Stream for $name {
            type Item = Result<$stream>;

            fn poll_next(
                self: std::pin::Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<Option<Self::Item>> {
                let (stream, _) = ready!(self.0.poll_accept(cx))?;
                Poll::Ready(Some(Ok(stream)))
            }
        }
    };
}

impl_stream!(TcpListenerStream: TcpListener => TcpStream);

#[cfg(unix)]
impl_stream!(UnixListenerStream: UnixListener => UnixStream);
