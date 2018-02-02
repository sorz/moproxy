#[macro_use]
extern crate log;
#[macro_use]
extern crate serde_derive;
extern crate serde_json;
extern crate serde;
#[macro_use]
extern crate futures;
extern crate tokio_core;
#[macro_use]
extern crate tokio_io;
extern crate hyper;
extern crate nix;
extern crate rand;
pub mod client;
pub mod monitor;
pub mod proxy;
pub mod web;

pub trait ToMillis {
    fn millis(&self) -> u32;
}

impl ToMillis for std::time::Duration {
    fn millis(&self) -> u32 {
        self.as_secs() as u32 * 1000 + self.subsec_nanos() / 1_000_000
    }
}

#[derive(Debug)]
pub struct RcBox<T: ?Sized> {
    item: std::rc::Rc<Box<T>>,
}
impl<T: ?Sized> RcBox<T> {
    fn new(item: Box<T>) -> Self {
        RcBox { item: std::rc::Rc::new(item) }
    }
}
impl<T: ?Sized> AsRef<T> for RcBox<T> {
    fn as_ref(&self) -> &T {
        &self.item
    }
}
impl<T: ?Sized> Clone for RcBox<T> {
    fn clone(&self) -> Self {
        RcBox { item: self.item.clone() }
    }
}

