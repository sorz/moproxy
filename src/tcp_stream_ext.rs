use std::{
    future::Future,
    io,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::net::TcpStream;

pub struct Peek<'a> {
    stream: &'a mut TcpStream,
    buf: &'a mut [u8],
}

impl Future for Peek<'_> {
    type Output = io::Result<usize>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<usize>> {
        let me = &mut *self;
        Pin::new(&mut *me.stream).poll_peek(cx, me.buf)
    }
}

pub trait TcpStreamExt {
    fn peek<'a>(&'a mut self, buf: &'a mut [u8]) -> Peek<'a>;
}

impl TcpStreamExt for TcpStream {
    fn peek<'a>(&'a mut self, buf: &'a mut [u8]) -> Peek<'a> {
        Peek { stream: self, buf }
    }
}
