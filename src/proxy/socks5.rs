use crate::proxy::{Address, Destination};
use log::trace;
use std::io::{self, ErrorKind, Write};
use std::net::IpAddr;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

pub async fn handshake<T>(
    stream: &mut TcpStream,
    addr: &Destination,
    data: Option<T>,
    fake_handshaking: bool,
) -> io::Result<()>
where
    T: AsRef<[u8]>,
{
    if fake_handshaking {
        trace!("socks: do FAKE handshake w/ {:?}", addr);
        fake_handshake(stream, addr, data).await
    } else {
        trace!("socks: do FULL handshake w/ {:?}", addr);
        full_handshake(stream, addr, data).await
    }
}

pub async fn fake_handshake<T>(
    stream: &mut TcpStream,
    addr: &Destination,
    data: Option<T>,
) -> io::Result<()>
where
    T: AsRef<[u8]>,
{
    let mut buf = Vec::with_capacity(16);
    buf.write_all(&[5, 1, 0]).unwrap();
    build_request(&mut buf, addr);
    stream.write_all(&buf).await?;
    if let Some(data) = data {
        stream.write_all(data.as_ref()).await?;
    }
    buf.resize(12, 0);
    stream.read_exact(&mut buf).await?;
    Ok(())
}

pub async fn full_handshake<T>(
    stream: &mut TcpStream,
    addr: &Destination,
    data: Option<T>,
) -> io::Result<()>
where
    T: AsRef<[u8]>,
{
    // Send request w/ auth method 0x00 (no auth)
    trace!("socks: write [5, 1, 0]");
    stream.write_all(&[0x05, 0x01, 0x00]).await?;

    // Server should select 0x00 as auth method
    let mut buf = vec![0; 2];
    stream.read_exact(&mut buf).await?;
    trace!("socks: read {:?}", buf);
    match buf[..2] {
        [0x05, 0xff] => {
            return Err(io::Error::new(
                ErrorKind::Other,
                "auth required by socks server",
            ))
        }
        [0x05, 0x00] => (),
        _ => {
            return Err(io::Error::new(
                ErrorKind::Other,
                "unrecognized reply from socks server",
            ))
        }
    }

    // Write the actual request
    buf.clear();
    build_request(&mut buf, addr);
    trace!("socks: write request {:?}", buf);
    stream.write_all(&buf).await?;

    // Check server's reply
    buf.resize(10, 0);
    stream.read_exact(&mut buf).await?;
    trace!("socks: read reply {:?}", buf);
    if !buf.starts_with(&[0x05, 0x00]) {
        return Err(io::Error::new(ErrorKind::Other, "socks server reply error"));
    }
    if buf[3] == 4 {
        // Consume truncted IPv6 address
        buf.resize(16 - 4, 0);
        stream.read_exact(&mut buf).await?;
    }

    // Write out payload if exist
    if let Some(data) = data {
        trace!("socks: write payload {:?}", data.as_ref());
        stream.write_all(data.as_ref()).await?;
    }
    Ok(())
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
