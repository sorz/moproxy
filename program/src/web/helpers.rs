use hyper::Request;
use number_prefix::NumberPrefix::{self, Prefixed, Standalone};
use once_cell::sync::Lazy;
use regex::Regex;
use std::{fmt::Write, time::Duration};

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

        static RE: Lazy<Regex> =
            Lazy::new(|| Regex::new(r"(^(curl|Lynx)/|PowerShell/[\d\.]+$)").unwrap());
        if let Some(ua) = self
            .headers()
            .get("user-agent")
            .and_then(|v| v.to_str().ok())
        {
            !RE.is_match(ua)
        } else {
            true
        }
    }
}

pub trait DurationExt {
    fn format(&self) -> String;
    fn format_millis(&self) -> String;
}

impl DurationExt for Duration {
    fn format(&self) -> String {
        let secs = self.as_secs();
        let d = secs / 86400;
        let h = (secs % 86400) / 3600;
        let m = (secs % 3600) / 60;
        let s = secs % 60;
        let mut buf = String::new();
        vec![(d, 'd'), (h, 'h'), (m, 'm'), (s, 's')]
            .into_iter()
            .filter(|(v, _)| *v > 0)
            .take(2)
            .for_each(|(v, u)| {
                write!(&mut buf, "{}{}", v, u).unwrap();
            });
        buf
    }

    fn format_millis(&self) -> String {
        format!("{} ms", self.as_millis())
    }
}

pub fn to_human_bytes(n: usize) -> String {
    if n == 0 {
        String::new()
    } else {
        match NumberPrefix::binary(n as f64) {
            Standalone(bytes) => format!("{} bytes", bytes),
            Prefixed(prefix, n) => format!("{:.1} {}B", n, prefix),
        }
    }
}

pub fn to_human_bps(n: usize) -> String {
    match NumberPrefix::decimal(n as f64) {
        Standalone(n) => format!("{} bps", n),
        Prefixed(prefix, n) => format!("{:.0} {}bps", n, prefix),
    }
}

pub fn to_human_bps_prefix_only(n: usize) -> String {
    match NumberPrefix::decimal(n as f64) {
        Standalone(n) => format!("{} ", n),
        Prefixed(prefix, n) => format!("{:.0}{}", n, prefix),
    }
}
