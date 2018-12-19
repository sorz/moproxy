use crate::proxy::{Address, Destination};
use futures::Future;
use std::io::{self, Write};
use std::net::IpAddr;
use tokio_core::net::TcpStream;
use tokio_io::io::{read_exact, write_all};

pub fn handshake<T>(
    stream: TcpStream,
    addr: &Destination,
    data: Option<T>,
) -> impl Future<Item = TcpStream, Error = io::Error>
where
    T: AsRef<[u8]>,
{
    let mut request = build_request(addr);
    if let Some(data) = data {
        // TODO: remove copying
        request.extend(data.as_ref());
    }
    write_all(stream, request)
        .and_then(|(stream, _)| read_exact(stream, vec![0; 12]))
        .map(|(stream, _)| stream)
}

fn build_request(addr: &Destination) -> Vec<u8> {
    let mut buffer = Vec::with_capacity(13);
    buffer.write_all(&[5, 1, 0]).unwrap();
    buffer.write_all(&[5, 1, 0]).unwrap();
    match addr.host {
        Address::Ip(ip) => match ip {
            IpAddr::V4(ip) => {
                buffer.write_all(&[0x01]).unwrap();
                buffer.write_all(&ip.octets()).unwrap();
            }
            IpAddr::V6(ip) => {
                buffer.write_all(&[0x04]).unwrap();
                buffer.write_all(&ip.octets()).unwrap();
            }
        },
        Address::Domain(ref host) => {
            buffer.write_all(&[0x03]).unwrap();
            buffer.push(host.len() as u8);
            buffer.write_all(host.as_bytes()).unwrap();
        }
    };
    buffer.push((addr.port >> 8) as u8);
    buffer.push(addr.port as u8);
    buffer
}
