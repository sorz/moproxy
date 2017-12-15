use std::str;
use std::net::IpAddr;
use std::io::{self, Read, BufReader, ErrorKind};
use futures::{future, Future};
use tokio_core::net::TcpStream;
use tokio_io::io::{write_all, read_until};
use proxy::{Connect, Destination, Address};


pub fn handshake(stream: TcpStream, addr: &Destination) -> Box<Connect> {
    let request = build_request(addr);
    let response = write_all(stream, request).and_then(|(stream, _)| {
        let reader = BufReader::new(stream).take(512);
        read_until(reader, 0x0a, Vec::with_capacity(64))
    }).and_then(|(reader, buf)| {
        match str::from_utf8(&buf) {
            Ok(status) => {
                debug!("recv response: {}", status.trim());
                if !status.starts_with("HTTP/1.1 2") {
                    let err = format!("proxy return error: {}", status.trim());
                    return Err(io::Error::new(ErrorKind::Other, err));
                }
            },
            Err(e) => return Err(io::Error::new(ErrorKind::Other,
                    format!("fail to parse http response: {}", e))),
        };
        Ok((reader.into_inner(), buf))
    });
    let skip_headers = |(reader, mut buf): (io::Take<_>, Vec<u8>)| {
        buf.clear();
        read_until(reader, 0x0a, buf).and_then(|(reader, buf)| {
            match str::from_utf8(&buf) {
                Err(e) => warn!("cannot parse http header: {}", e),
                Ok(header) => {
                    let header = header.trim();
                    if header.is_empty() {
                        debug!("all headers passed, pipe established");
                        return Ok(future::Loop::Break(reader));
                    } else {
                        debug!("recv header: {}", header);
                    }
                }
            };
            Ok(future::Loop::Continue((reader, buf)))
        })
    };
    let skip = response.and_then(|(reader, buf)| {
        let reader = reader.take(8 * 1024);
        future::loop_fn((reader, buf), skip_headers)
            .map(|reader| reader.into_inner())
    });
    // FIXME: may lost data in buffer?
    Box::new(skip.map(|reader| reader.into_inner()))
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
        let request = format!(
            "CONNECT {host} HTTP/1.1\r\n\
            Host: {host}\r\n\
            Connection: close\r\n\r\n",
            host=host
        );
        return request;
}

