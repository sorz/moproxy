use crate::proxy::{Address, Destination};
use futures::{future::Either, Future, IntoFuture};
use std::io::{self, Write, ErrorKind};
use std::net::IpAddr;
use tokio_core::net::TcpStream;
use tokio_io::io::{read_exact, write_all};

pub fn handshake<T>(
    stream: TcpStream,
    addr: &Destination,
    data: Option<T>,
    fake_handshaking: bool,
) -> impl Future<Item = TcpStream, Error = io::Error>
where
    T: AsRef<[u8]>,
{
    if fake_handshaking {
        Either::A(fake_handshake(stream, addr, data))
    } else {
        Either::B(full_handshake(stream, addr, data))
    }
}

pub fn fake_handshake<T>(
    stream: TcpStream,
    addr: &Destination,
    data: Option<T>,
) -> impl Future<Item = TcpStream, Error = io::Error>
where
    T: AsRef<[u8]>,
{
    let mut request = Vec::with_capacity(16);
    request.write_all(&[5, 1, 0]).unwrap();
    build_request(&mut request, addr);
    if let Some(data) = data {
        // TODO: remove copying
        request.extend(data.as_ref());
    }
    write_all(stream, request)
        .and_then(|(stream, _)| read_exact(stream, vec![0; 12]))
        .map(|(stream, _)| stream)
}

pub fn full_handshake<T>(
    stream: TcpStream,
    addr: &Destination,
    data: Option<T>,
) -> impl Future<Item = TcpStream, Error = io::Error>
where
    T: AsRef<[u8]>,
{
    let mut request = Vec::with_capacity(13);
    build_request(&mut request, addr);

    // Send request w/ auth method 0x00 (no auth)
    write_all(stream, [0x05, 0x01, 0x00]).and_then(|(stream, _)| {
        read_exact(stream, [0; 2])
    }).and_then(|(stream, buf)| {
        // Check: server should select 0x00 as auth method
        match buf {
            [0x05, 0xff] => Err(io::Error::new(ErrorKind::Other,
                    "auth required by socks server")),
            [0x05, 0x00] => Ok(stream),
            _ => Err(io::Error::new(ErrorKind::Other,
                    "unrecognized reply from socks server"))
        }
    }).and_then(|stream| {
        // Write the actual request
        write_all(stream, request)
    }).and_then(|(stream, mut buf)| {
        buf.resize_with(10, Default::default);
        read_exact(stream, buf)
    }).and_then(|(stream, buf)| {
        // Check server's reply
        if buf.starts_with(&[0x05, 0x00]) {
            Ok((stream, buf))
        } else {
            Err(io::Error::new(ErrorKind::Other,
                    "socks server reply error"))
        }
    }).and_then(|(stream, mut buf)| {
        // Write out payload if exist
        if let Some(data) = data {
            buf.clear();
            buf.extend(data.as_ref());
            Either::A(write_all(stream, buf))
        } else {
            Either::B(Ok((stream, buf)).into_future())
        }
    }).map(|(stream, _)| stream)
}

fn build_request(buffer: &mut Vec<u8>, addr: &Destination) {
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
}
