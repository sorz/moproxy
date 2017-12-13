#[macro_use]
extern crate log;
#[macro_use]
extern crate serde_derive;
extern crate serde_json;
extern crate serde;
extern crate futures;
extern crate tokio_core;
#[macro_use]
extern crate tokio_io;
extern crate tokio_timer;
extern crate hyper;
extern crate nix;
pub mod client;
pub mod monitor;
pub mod proxy;
pub mod web;
mod tls;

pub trait ToMillis {
    fn millis(&self) -> u32;
}

impl ToMillis for std::time::Duration {
    fn millis(&self) -> u32 {
        self.as_secs() as u32 * 1000 + self.subsec_nanos() / 1_000_000
    }
}
