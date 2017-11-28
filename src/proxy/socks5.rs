extern crate tokio_core;
extern crate tokio_io;
extern crate futures;
use std::net::{SocketAddr, IpAddr};
use std::io::Write;
use self::futures::Future;
use self::tokio_core::net::TcpStream;
use self::tokio_io::io::{read_exact, write_all};
use ::proxy::Connect;


pub fn handshake(stream: TcpStream, addr: SocketAddr) -> Box<Connect> {
    let request = build_request(&addr);
    let handshake = write_all(stream, request).and_then(|(stream, _)| {
        read_exact(stream, vec![0; 12])
    }).map(|(stream, _)| stream);
    return Box::new(handshake)
}

fn build_request(addr: &SocketAddr) -> Vec<u8> {
    let mut buffer = Vec::with_capacity(13);
    buffer.write(&[5, 1, 0]).unwrap();
    buffer.write(&[5, 1, 0]).unwrap();
    match addr.ip() {
        IpAddr::V4(ip) => {
            buffer.write(&[0x01]).unwrap();
            buffer.write(&ip.octets()).unwrap();
        },
        IpAddr::V6(ip) => {
            buffer.write(&[0x04]).unwrap();
            buffer.write(&ip.octets()).unwrap();
        }
    };
    let port = addr.port();
    buffer.push((port >> 8) as u8);
    buffer.push(port as u8);
    buffer
}

