extern crate nix;
extern crate moproxy;
use std::net::{TcpListener, TcpStream, SocketAddrV4, Shutdown};
use std::io::{self, Write, Read, ErrorKind};
use std::os::unix::io::{RawFd, AsRawFd};
use std::{env, thread};
use std::sync::{Mutex, Arc};
use std::time::Duration;
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
