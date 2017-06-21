extern crate rand;
use std::net::{TcpStream, SocketAddrV4};
use std::io::{self, Write, Read};
use std::time::{Instant, Duration};

pub fn handshake(stream: &mut TcpStream, addr: SocketAddrV4)
        -> io::Result<()> {
    let mut buffer = Vec::with_capacity(13);
    buffer.write(&[5, 1, 0])?;
    buffer.write(&[5, 1, 0, 1])?;
    buffer.write(&addr.ip().octets())?;
    let port = addr.port();
    buffer.push((port >> 8) as u8);
    buffer.push(port as u8);
    //println!("{:?}", &buffer);
    stream.write_all(&buffer)?;

    let mut buffer = [0; 12];
    stream.read_exact(&mut buffer)?;
    Ok(())
}

pub fn alive_test(server: SocketAddrV4) -> io::Result<u32> {
    let request = [
        0, 17,  // length
        rand::random(), rand::random(),  // transaction ID
        1, 32,  // standard query
        0, 1,  // one query
        0, 0,  // answer
        0, 0,  // authority
        0, 0,  // addition
        0,     // query: root
        0, 1,  // query: type A
        0, 1,  // query: class IN
    ];
    
    let mut socks = TcpStream::connect(server)?;
    socks.set_nodelay(true)?;
    socks.set_read_timeout(Some(Duration::from_secs(5)))?;
    socks.set_write_timeout(Some(Duration::from_secs(5)))?;
    handshake(&mut socks, "8.8.8.8:53".parse().unwrap())?;

    socks.write_all(&request)?;

    let mut buffer = [0; 128];
    let now = Instant::now();
    let n = socks.read(&mut buffer)?;
    if n > 4 && buffer[2..3] == request[2..3] {
        let delay = now.elapsed();
        Ok(delay.as_secs() as u32 * 1000 + delay.subsec_nanos() / 1_000_000)
    } else {
        Err(io::Error::new(io::ErrorKind::Other, "unknown response"))
    }
}

