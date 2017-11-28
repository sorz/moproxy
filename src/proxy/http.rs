extern crate tokio_core;
extern crate tokio_io;
extern crate futures;
use std::str;
use std::net::SocketAddr;
use std::io::{self, Read, BufReader, ErrorKind};
use self::futures::{future, Future};
use self::tokio_core::net::TcpStream;
use self::tokio_io::io::{write_all, read_until};
use ::proxy::Connect;


pub fn handshake(stream: TcpStream, addr: SocketAddr) -> Box<Connect> {
    let request = build_request(&addr);
    let response = write_all(stream, request).and_then(|(stream, _)| {
        let reader = BufReader::new(stream).take(512);
        read_until(reader, 0x0a, Vec::with_capacity(32))
    }).and_then(|(reader, status)| {
        let status = match str::from_utf8(&status) {
            Ok(s) => s,
            Err(e) => return Err(io::Error::new(ErrorKind::Other,
                    format!("fail to parse http response: {}", e))),
        };
        debug!("recv response: {}", status.trim());
        if status.starts_with("HTTP/1.1 2") {
            Ok(reader.into_inner())
        } else {
            let err = format!("proxy return error: {}", status.trim());
            Err(io::Error::new(ErrorKind::Other, err))
        }
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
    let skip = response.and_then(|reader| {
        let buf = Vec::with_capacity(64);
        let reader = reader.take(8 * 1024);
        future::loop_fn((reader, buf), skip_headers)
            .map(|reader| reader.into_inner())
    });
    // FIXME: may lost data in buffer?
    Box::new(skip.map(|reader| reader.into_inner()))
}

fn build_request(addr: &SocketAddr) -> String {
        let host = match *addr {
            SocketAddr::V4(s) => format!("{}:{}", s.ip(), s.port()),
            SocketAddr::V6(s) => format!("[{}]:{}", s.ip(), s.port()),
        };
        let request = format!(
            "CONNECT {host} HTTP/1.1\r\n\
            Host: {host}\r\n\
            Connection: close\r\n\r\n",
            host=host
        );
        return request;
}

