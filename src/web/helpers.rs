use hyper::Request;
use std::{
    fmt::Write,
    time::Duration,
};
use lazy_static::lazy_static;
use regex::Regex;

pub trait RequestExt {
    fn accept_html(&self) -> bool;
}

impl<T> RequestExt for Request<T> {
    fn accept_html(&self) -> bool {
        if let Some(accpet) = self.headers().get("accpet").and_then(|v| v.to_str().ok()) {
            if accpet.starts_with("text/html") {
                return true;
            } else if accpet.starts_with("text/plain") {
                return false;
            }
        }

        lazy_static! {
            static ref RE: Regex = Regex::new(
                r"(^(curl|Lynx)/|PowerShell/[\d\.]+$)"
            ).unwrap();
        }
        if let Some(ua) = self.headers().get("user-agent").and_then(|v| v.to_str().ok()) {
            !RE.is_match(ua)
        } else {
            true
        }
    }
}

pub trait DurationExt {
    fn format(&self) -> String;
}

impl DurationExt for Duration {
    fn format(&self) -> String {
        let secs = self.as_secs();
        let d = secs / 86400;
        let h = (secs % 86400) / 3600;
        let m = (secs % 3600) / 60;
        let s = secs % 60;
        let mut buf = String::new();
        vec![(d, 'd'), (h, 'h'), (m, 'm'), (s, 's')].into_iter()
            .filter(|(v, _)| *v > 0)
            .take(2)
            .for_each(|(v, u)| {
                write!(&mut buf, "{}{}", v, u).unwrap();
            });
        buf
    }
}
