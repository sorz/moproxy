extern crate nix;
extern crate mio;
extern crate moproxy;
use std::net::{TcpListener, TcpStream, SocketAddrV4, Shutdown};
use std::io::{self, Write, Read, ErrorKind};
use std::os::unix::io::{RawFd, AsRawFd};
use std::{env, thread};
use std::sync::{Mutex, Arc};
use std::time::Duration;
use mio::{Ready, PollOpt};
use nix::sys::socket::{getsockopt, sockopt};

use moproxy::{socks5, monitor};


fn main() {
    let port = env::args().nth(1).unwrap_or(String::from("10081")).parse()
        .expect("invalid port number");
    let listener = TcpListener::bind(("0.0.0.0", port))
        .expect("cannot bind to port");

    let mut servers = vec![];
    servers.push("127.0.0.1:8130".parse().unwrap());
    servers.push("127.0.0.1:8131".parse().unwrap());
    servers.push("127.0.0.1:8132".parse().unwrap());
    servers.push("127.0.0.1:8133".parse().unwrap());

    let servers = Arc::new(Mutex::new(servers));
    {
        let servers = servers.clone();
        thread::spawn(move || monitor::monitoring_servers(servers));
    }

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(e) = handle_client(stream, &servers) {
                    println!("error: {}", e);
                }
            }
            Err(e) => println!("error: {}", e)
        }
    }
}

fn handle_client(client: TcpStream, servers: &Arc<Mutex<Vec<SocketAddrV4>>>)
        -> io::Result<()> {
    let dest = get_original_dest(client.as_raw_fd())?;
    print!("{} => {} ", client.peer_addr()?.ip(), dest);

    for server in servers.lock().unwrap().iter() {
        if let Ok(socks) = init_socks(*server, dest) {
            println!("via :{}", server.port());
            client.set_read_timeout(Some(Duration::from_secs(555)))?;
            client.set_write_timeout(Some(Duration::from_secs(60)))?;
            pipe_streams(client.try_clone()?, socks.try_clone()?);
            pipe_streams(socks, client);
            return Ok(())
        } else {
            println!("fail to connect {}", server);
        }
    }
    Err(io::Error::new(ErrorKind::Other, "all socks server down"))
}


fn init_socks(server: SocketAddrV4, dest: SocketAddrV4)
        -> io::Result<TcpStream> {
    let mut socks = TcpStream::connect(server)?;
    socks.set_nodelay(true)?;
    socks.set_read_timeout(Some(Duration::from_millis(100)))?;
    socks.set_write_timeout(Some(Duration::from_millis(100)))?;
    socks5::handshake(&mut socks, dest)?;

    socks.set_read_timeout(Some(Duration::from_secs(555)))?;
    socks.set_write_timeout(Some(Duration::from_secs(60)))?;
    Ok(socks)
}


fn get_original_dest(fd: RawFd) -> io::Result<SocketAddrV4> {
    let addr = getsockopt(fd, sockopt::OriginalDst)?;
    Ok(SocketAddrV4::new(addr.sin_addr.s_addr.to_be().into(),
                         addr.sin_port.to_be()))
}

fn pipe_streams(mut a: TcpStream, mut b: TcpStream) -> io::Result<()> {
    let a = mio::net::TcpStream::from_stream(a)?;
    let b = mio::net::TcpStream::from_stream(b)?;
    const A: Token = mio::Token(0);
    const B: Token = mio::Token(1);
    let poll = mio::Poll::new()?;
    let poll_opt = PollOpt::level();
    pool.register(&a, A, Ready::readable(), poll_opt)?;
    pool.register(&b, B, Ready::readable(), poll_opt)?;
    let mut events = Events::with_capacity(8);
    let mut buf_a = vec![0; 8192];
    let mut buf_b = vec![0; 8192];
    loop {
        poll.poll(&mut events, Duration::from_secs(300))?;
        for event in events.iter() {
            let (left, right, l_token, r_token,
                 mut l_buf, mut r_buf) = match event.token() {
                A => (a, b, A, B, buf_a, buf_b),
                B => (b, a, B, A, buf_b, buf_a),
            };
            if event.readiness().is_writable() {
                poll.register(&right, r_token, Ready::readable(), poll_opt)?;
            } else if event.readiness().is_readable() {
                r_buf.resize(8192);
                let n = left.read(&mut r_buf)?;
                r_buf.truncate(n);
                let n = right.write(&r_buf)?;
                if n >= r_buf.len() {
                    r_buf.clear();
                } else {
                    r_buf = r_buf.drain(n..).collect();
                    pool.deregister(l_token)?;
                    pool.reregister(r_token)
                }

            }
        }
    }

}
/*
fn pipe_streams(mut from: TcpStream, mut to: TcpStream) {
    thread::spawn(move || {
        let mut buffer = [0; 8192];
        loop {
            let n = match from.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => {
                    println!("error on read: {}", e);
                    break;
                }
            };
            match to.write_all(&buffer[..n]) {
                Err(e) => {
                    println!("error on write: {}", e);
                    break;
                },
                Ok(_) => (),
            }
        }
        from.shutdown(Shutdown::Read).ok();
        to.shutdown(Shutdown::Write).ok();
    });
}
*/
