use log::{debug, warn};
use std::io::{self, BufReader, ErrorKind, Read};
use std::net::IpAddr;
use std::str;
use tokio::{
    io::{AsyncWriteExt, AsyncReadExt},
    net::TcpStream,
};

use crate::proxy::{Address, Destination};

pub async fn handshake<T>(
    stream: &mut TcpStream,
    addr: &Destination,
    mut data: Option<T>,
    with_playload: bool,
) -> io::Result<()>
where
    T: AsRef<[u8]> + 'static,
{
    let mut buf = build_request(addr).into_bytes();
    stream.write_all(&buf).await?;
    if with_playload && data.is_some() {
        let data = data.take().unwrap();
        stream.write_all(data.as_ref()).await?;
    }

    // Read status line
    unimplemented!();
    /*
    let mut reader = BufReader::new(stream).take(512);
    buf.clear();
    reader.read_until(0x0a, &mut buf).await?;
    match str::from_utf8(&buf) {
        Ok(status) => {
            debug!("recv response: {}", status.trim());
            if !status.starts_with("HTTP/1.1 2") {
                let err = format!("proxy return error: {}", status.trim());
                return Err(io::Error::new(ErrorKind::Other, err));
            }
        }
        Err(e) => {
            return Err(io::Error::new(
                ErrorKind::Other,
                format!("fail to parse http response: {}", e),
            ))
        }
    };

    // Skip headers
    loop {
        buf.clear();
        reader.read_until(0x0a, &mut buf).await?;
        match str::from_utf8(&buf) {
            Err(e) => warn!("cannot parse http header: {}", e),
            Ok(header) => {
                let header = header.trim();
                if header.is_empty() {
                    debug!("all headers passed, pipe established");
                    break;
                } else {
                    debug!("recv header: {}", header);
                }
            }
        };
    }

    // Write out payload if exist
    if !with_playload && data.is_some() {
        stream.write_all(data.take().unwrap().as_ref()).await?;
    }
    Ok(())
    */
}

fn build_request(addr: &Destination) -> String {
    let port = addr.port;
    let host = match addr.host {
        Address::Ip(ip) => match ip {
            IpAddr::V4(ip) => format!("{}:{}", ip, port),
            IpAddr::V6(ip) => format!("[{}]:{}", ip, port),
        },
        Address::Domain(ref s) => format!("{}:{}", s, port),
    };
    format!(
        "CONNECT {host} HTTP/1.1\r\n\
         Host: {host}\r\n\
         Connection: close\r\n\r\n",
        host = host
    )
}
