use crate::proxy::{Address, Destination};
use log::trace;
use std::io::{self, ErrorKind};
use std::net::IpAddr;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

use super::SocksUserPassAuthCredential;

pub async fn handshake<T>(
    stream: &mut TcpStream,
    addr: &Destination,
    data: Option<T>,
    fake_handshaking: bool,
    user_pass_auth: &Option<SocksUserPassAuthCredential>,
) -> io::Result<()>
where
    T: AsRef<[u8]>,
{
    if fake_handshaking && user_pass_auth.is_none() {
        trace!("socks: do FAKE handshake w/ {:?}", addr);
        fake_handshake(stream, addr, data).await
    } else {
        trace!("socks: do FULL handshake w/ {:?}", addr);
        full_handshake(stream, addr, data, user_pass_auth).await
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
    buf.extend_from_slice(&[5, 1, 0]);
    build_request(&mut buf, addr);
    stream.write_all(&buf).await?;
    if let Some(data) = data {
        stream.write_all(data.as_ref()).await?;
    }
    buf.resize(12, 0);
    stream.read_exact(&mut buf).await?;
    Ok(())
}

macro_rules! err {
    ($msg:expr) => {
        return Err(io::Error::new(ErrorKind::Other, $msg));
    };
}

pub async fn full_handshake<T>(
    stream: &mut TcpStream,
    addr: &Destination,
    data: Option<T>,
    user_pass_auth: &Option<SocksUserPassAuthCredential>,
) -> io::Result<()>
where
    T: AsRef<[u8]>,
{
    let mut buf = vec![];
    if user_pass_auth.is_none() {
        // Send request w/ auth method 0x00 (no auth)
        buf.extend(&[0x05, 0x01, 0x00])
    } else {
        // Or, include 0x02 (username/password auth)
        buf.extend(&[0x05, 0x01, 0x00, 0x02])
    };
    trace!("socks: write {:?}", buf);
    stream.write_all(&buf).await?;

    // Server select auth method
    let mut buf = vec![0; 2];
    stream.read_exact(&mut buf).await?;
    trace!("socks: read {:?}", buf);
    match buf[..2] {
        // 0xff: no acceptable method
        [0x05, 0xff] => err!("auth required by socks server"),
        // 0x00: no auth required
        [0x05, 0x00] => (),
        // 0x02: username/password method
        [0x05, 0x02] => {
            if let Some(auth) = user_pass_auth {
                if auth.username.len() > 255 || auth.password.len() > 255 {
                    panic!("SOCKSv5 username/password exceeds 255 bytes");
                }
                buf.clear();
                buf.push(0x05);
                buf.push(auth.username.len() as u8);
                buf.extend(auth.username.as_bytes());
                buf.push(auth.password.len() as u8);
                buf.extend(auth.password.as_bytes());
                trace!("socks: write auth {:?}", buf);
                stream.write_all(&buf).await?;

                // Parse response
                buf.resize(2, 0);
                stream.read_exact(&mut buf).await?;
                trace!("socks: read {:?}", buf);
                if buf != [0x05, 0x00] {
                    err!("auth rejected by SOCKSv5 server")
                }
            } else {
                err!("missing username/password required by socks server");
            }
        },
        _ => err!("unrecognized reply from socks server"),
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
       err!("socks server reply error");
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
    buffer.extend_from_slice(&[5, 1, 0]);
    match addr.host {
        Address::Ip(ip) => match ip {
            IpAddr::V4(ip) => {
                buffer.push(0x01);
                buffer.extend_from_slice(&ip.octets());
            }
            IpAddr::V6(ip) => {
                buffer.push(0x04);
                buffer.extend_from_slice(&ip.octets());
            }
        },
        Address::Domain(ref host) => {
            buffer.push(0x03);
            buffer.push(host.len() as u8);
            buffer.extend_from_slice(host.as_bytes());
        }
    };
    buffer.push((addr.port >> 8) as u8);
    buffer.push(addr.port as u8);
}
