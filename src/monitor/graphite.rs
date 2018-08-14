use std::io::{self, Write};
use std::time::SystemTime;


#[derive(Clone, Debug)]
pub struct Record<'a> {
    path: &'a str,
    value: i32,
    time: Option<SystemTime>,
}

impl<'a> Record<'a> {
    pub fn new(path: &'a str, value: i32, time: Option<SystemTime>) -> Self {
        if !path.is_ascii() || path.contains(' ') || path.contains('\n') {
            panic!("Graphite path contains space, line break, \
                    or non-ASCII characters.");
        }
        Record { path, value, time }
    }

    fn write_paintext<B>(&self, buf: &mut B) -> io::Result<()>
    where B: Write {
        let time = match self.time {
            None => -1.0,
            Some(time) => {
                let t = time.duration_since(SystemTime::UNIX_EPOCH)
                    .expect("SystemTime before UNIX EPOCH");
                (t.as_secs() as f64) + (t.subsec_micros() as f64) / 1_000_000.0
            }
        };
        writeln!(buf, "{} {} {}", self.path, self.value, time)
    }
}

pub fn write_records<'a, B, R>(buf: &mut B, records: R) -> io::Result<()>
where B: Write,
      R: Iterator<Item=Record<'a>> {
    records.map(|record| record.write_paintext(buf))
        .find(|result| result.is_err())
        .unwrap_or(Ok(()))
}