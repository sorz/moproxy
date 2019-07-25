use log::{debug, trace};
use std::io::{self, ErrorKind};
use std::net::IpAddr;
use tokio::{
    io::{AsyncWriteExt, AsyncReadExt},
    net::TcpStream,
};
use httparse::{Response, EMPTY_HEADER, Status};

use crate::proxy::{Address, Destination};


macro_rules! ensure_200 {
    ($code:expr) => {
        if $code != 200 {
            return Err(io::Error::new(
                ErrorKind::Other,
                format!("proxy return error: {}", $code)
            ))
        }
    };
}

pub async fn handshake<T>(
    stream: &mut TcpStream,
    addr: &Destination,
    data: Option<T>,
    with_playload: bool,
) -> io::Result<()>
where
    T: AsRef<[u8]> + 'static,
{
    let mut buf = build_request(addr).into_bytes();
    stream.write_all(&buf).await?;

    if with_playload {
        // violate the protocol but save latency
        if let Some(ref data) = data {
            stream.write_all(data.as_ref()).await?;
        }
    }

    // Parse HTTP response
    buf.clear();
    let mut bytes_read = 0;
    let bytes_request = loop {
        let mut headers = [EMPTY_HEADER; 16];
        let mut response = Response::new(&mut headers);
        buf.resize(bytes_read + 1024, 0);
        bytes_read += stream.read(&mut buf).await?;
        trace!("bytes read: {}", bytes_read);

        match response.parse(&mut buf[..bytes_read]) {
            Err(e) => return Err(io::Error::new(ErrorKind::Other, e)),
            Ok(Status::Partial) => {
                debug!("partial http reponse read; wait for more data");
                if let Some(code) = response.code {
                    ensure_200!(code);
                }
                if bytes_read > 64_000 {
                    return Err(io::Error::new(
                        ErrorKind::Other, "response too large"));
                }
            }
            Ok(Status::Complete(n)) => {
                ensure_200!(response.code.unwrap());
                break n;
            }
        }
    };

    // Pass received body to the left
    if bytes_request < bytes_read {
        // TODO: return read payload
        return Err(io::Error::new(
            ErrorKind::Other,
            "unimplemented function"
        ));
    }

    // Write out payload if exist
    if !with_playload {
        if let Some(ref data) = data {
            stream.write_all(data.as_ref()).await?;
        }
    }
    Ok(())
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
