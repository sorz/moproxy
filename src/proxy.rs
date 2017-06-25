extern crate tokio_core;
extern crate tokio_io;
extern crate futures;
use std::net::TcpStream;
use self::futures::{Stream,  Future};
use self::futures::sync::mpsc;
use self::tokio_core::net as tnet;
use self::tokio_core::reactor;
use self::tokio_io::io as tio;
use self::tokio_io::AsyncRead;


pub fn piping_worker(rx: mpsc::Receiver<(TcpStream, TcpStream)>) {
    let mut core = reactor::Core::new().unwrap();
    let handle = core.handle();
    let server = rx.for_each(|(local, remote)| {
        let (lr, lw) = tnet::TcpStream::from_stream(local, &handle)
            .expect("cannot create asycn tcp stream").split();
        let (rr, rw) = tnet::TcpStream::from_stream(remote, &handle)
            .expect("cannot create asycn tcp stream").split();
        let piping = tio::copy(lr, rw)
            .join(tio::copy(rr, lw))
            .and_then(|((tx, _, rw), (rx, _, lw))| {
                debug!("tx {}, rx {} bytes", tx, rx);
                tio::shutdown(rw)
                    .join(tio::shutdown(lw))
            }).map(|_| ()).map_err(|e| warn!("piping error: {}", e));
        handle.spawn(piping);
        Ok(())
    });
    core.run(server).unwrap();
}
