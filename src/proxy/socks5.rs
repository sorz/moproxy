use std::net::IpAddr;
use std::io::Write;
use futures::Future;
use tokio_core::net::TcpStream;
use tokio_io::io::{read_exact, write_all};
use proxy::{Connect, Destination, Address};


pub fn handshake(stream: TcpStream, addr: &Destination) -> Box<Connect> {
    let request = build_request(addr);
    let handshake = write_all(stream, request).and_then(|(stream, _)| {
        read_exact(stream, vec![0; 12])
    }).map(|(stream, _)| stream);
    return Box::new(handshake)
}

fn build_request(addr: &Destination) -> Vec<u8> {
    let mut buffer = Vec::with_capacity(13);
    buffer.write(&[5, 1, 0]).unwrap();
    buffer.write(&[5, 1, 0]).unwrap();
    match addr.host {
        Address::Ip(ip) => match ip {
            IpAddr::V4(ip) => {
                buffer.write(&[0x01]).unwrap();
                buffer.write(&ip.octets()).unwrap();
            },
            IpAddr::V6(ip) => {
                buffer.write(&[0x04]).unwrap();
                buffer.write(&ip.octets()).unwrap();
            }
        },
        Address::Domain(ref host) => {
            buffer.write(&[0x03]).unwrap();
            buffer.push(host.len() as u8);
            buffer.write(host.as_bytes()).unwrap();
        },
    };
    buffer.push((addr.port >> 8) as u8);
    buffer.push(addr.port as u8);
    buffer
}

