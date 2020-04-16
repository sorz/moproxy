use log::{debug, warn};
use std::{
    io::{self, Write},
    net::SocketAddr,
    time::{Duration, SystemTime},
};
use tokio::{io::AsyncWriteExt, net::TcpStream, time::timeout};

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

    pub async fn write_records(&mut self, records: Vec<Record>) -> io::Result<()> {
        let Graphite {
            ref server_addr,
            stream: ref mut stream_opt,
        } = self;

        let mut stream = if let Some(stream) = stream_opt.take() {
            stream
        } else {
            debug!("start new connection to graphite server");
            TcpStream::connect(&server_addr).await?
        };

        let mut buf = Vec::new();
        for record in records {
            record.write_paintext(&mut buf).unwrap();
        }

        let max_wait = Duration::from_secs(GRAPHITE_TIMEOUT_SECS);
        match timeout(max_wait, stream.write_all(&buf)).await {
            Err(_) => Err(io::Error::from(io::ErrorKind::TimedOut)),
            Ok(Err(err)) => {
                warn!("fail to send metrics: {}", err);
                self.stream = None;
                Err(err)
            }
            Ok(Ok(_)) => {
                stream_opt.replace(stream);
                Ok(())
            }
        }
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
