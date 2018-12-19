use futures::{future::Either, Future, IntoFuture};
use log::{debug, warn};
use std::{
    io::{self, Write},
    net::SocketAddr,
    time::{Duration, SystemTime},
};
use tokio_core::{
    net::TcpStream,
    reactor::{Handle, Timeout},
};
use tokio_io::io::write_all;

static GRAPHITE_TIMEOUT_SECS: u64 = 5;

#[derive(Debug)]
pub struct Graphite {
    server_addr: SocketAddr,
    stream: Option<TcpStream>,
}

#[derive(Clone, Debug)]
pub struct Record {
    path: String,
    value: u64,
    time: Option<SystemTime>,
}

impl Graphite {
    pub fn new(server_addr: SocketAddr) -> Self {
        Graphite {
            stream: None,
            server_addr,
        }
    }

    // Should always return ok
    pub fn write_records(
        self,
        records: Vec<Record>,
        handle: &Handle,
    ) -> impl Future<Item = Self, Error = ()> {
        let Graphite {
            server_addr,
            stream,
        } = self;
        let stream = stream.ok_or(()).into_future().or_else(move |_| {
            debug!("start new connection to graphite server");
            TcpStream::connect2(&server_addr)
        });

        let mut buf = Vec::new();
        for record in records {
            record.write_paintext(&mut buf).unwrap();
        }

        let timeout = Duration::from_secs(GRAPHITE_TIMEOUT_SECS);
        let timeout = Timeout::new(timeout, handle).expect("error on get timeout from reactor");

        stream
            .and_then(move |stream| write_all(stream, buf))
            .map(|(stream, _buf)| stream) // TODO: keep buf
            .select2(timeout)
            .map_err(|err| err.split().0)
            .then(move |result| match result {
                Ok(Either::A((stream, _))) => Ok(Graphite {
                    server_addr,
                    stream: Some(stream),
                }),
                Err(err) => {
                    warn!("fail to send metrics: {}", err);
                    Ok(Graphite {
                        server_addr,
                        stream: None,
                    })
                }
                Ok(Either::B(_)) => panic!("timeout return ok"),
            })
    }
}

impl Record {
    pub fn new(path: String, value: u64, time: Option<SystemTime>) -> Self {
        if !path.is_ascii() || path.contains(' ') || path.contains('\n') {
            panic!(
                "Graphite path contains space, line break, \
                 or non-ASCII characters."
            );
        }
        Record { path, value, time }
    }

    fn write_paintext<B>(&self, buf: &mut B) -> io::Result<()>
    where
        B: Write,
    {
        let time = match self.time {
            None => -1.0,
            Some(time) => {
                let t = time
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .expect("SystemTime before UNIX EPOCH");
                (t.as_secs() as f64) + (t.subsec_micros() as f64) / 1_000_000.0
            }
        };
        writeln!(buf, "{} {} {}", self.path, self.value, time)
    }
}
