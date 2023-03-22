use base64::prelude::{Engine, BASE64_STANDARD};
use httparse::{Response, Status, EMPTY_HEADER};
use std::io::{self, ErrorKind};
use std::net::IpAddr;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};
use tracing::{debug, instrument, trace};

use crate::proxy::{Address, Destination};

use super::UserPassAuthCredential;

macro_rules! ensure_200 {
    ($code:expr) => {
        if $code != 200 {
            return Err(io::Error::new(
                ErrorKind::Other,
                format!("proxy return error: {}", $code),
            ));
        }
    };
}

const BUF_LEN: usize = 1024;

#[instrument(name = "http_handshake", skip_all)]
pub async fn handshake<T>(
    stream: &mut TcpStream,
    addr: &Destination,
    data: Option<T>,
    with_playload: bool,
    user_pass_auth: &Option<UserPassAuthCredential>,
) -> io::Result<()>
where
    T: AsRef<[u8]> + 'static,
{
    let mut buf = build_request(addr, user_pass_auth).into_bytes();
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
    let mut sink = [0u8; BUF_LEN];
    loop {
        let mut headers = [EMPTY_HEADER; 16];
        let mut response = Response::new(&mut headers);
        buf.resize(bytes_read + BUF_LEN, 0);
        let peek_len = stream.peek(&mut buf).await?;
        bytes_read += peek_len;
        trace!("bytes peek: {}", bytes_read);

        match response.parse(&buf[..bytes_read]) {
            Err(e) => return Err(io::Error::new(ErrorKind::Other, e)),
            Ok(Status::Partial) => {
                debug!("partial http reponse read; wait for more data");
                if let Some(code) = response.code {
                    ensure_200!(code);
                }
                if bytes_read > 64_000 {
                    return Err(io::Error::new(ErrorKind::Other, "response too large"));
                }
                // Drop peeked data from socket buffer
                stream.read_exact(&mut sink[..peek_len]).await?;
            }
            Ok(Status::Complete(bytes_request)) => {
                trace!(
                    "response {}, {} bytes",
                    response.code.unwrap(),
                    bytes_request
                );
                ensure_200!(response.code.unwrap());
                let len = peek_len - (bytes_read - bytes_request);
                stream.read_exact(&mut sink[..len]).await?;
                break;
            }
        }
    }

    // Write out payload if exist
    if !with_playload {
        if let Some(ref data) = data {
            stream.write_all(data.as_ref()).await?;
        }
    }
    trace!("HTTP CONNECT handshaking done");
    Ok(())
}

fn build_request(addr: &Destination, user_pass_auth: &Option<UserPassAuthCredential>) -> String {
    let port = addr.port;
    let host = match addr.host {
        Address::Ip(ip) => match ip {
            IpAddr::V4(ip) => format!("{}:{}", ip, port),
            IpAddr::V6(ip) => format!("[{}]:{}", ip, port),
        },
        Address::Domain(ref s) => format!("{}:{}", s, port),
    };

    if let Some(user_pass_auth) = user_pass_auth {
        let auth = format!(
            "{username}:{password}",
            username = user_pass_auth.username,
            password = user_pass_auth.password
        );
        let basic_auth = BASE64_STANDARD.encode(auth);
        format!(
            "CONNECT {host} HTTP/1.1\r\n\
             Host: {host}\r\n\
             Proxy-Authorization: Basic {basic_auth}\r\n\r\n",
            host = host,
            basic_auth = basic_auth,
        )
    } else {
        format!(
            "CONNECT {host} HTTP/1.1\r\n\
             Host: {host}\r\n\r\n",
            host = host
        )
    }
}
