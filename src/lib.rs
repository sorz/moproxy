#![feature(async_await)]
#![feature(async_closure)]
pub mod client;
pub mod monitor;
pub mod proxy;
pub mod tcp;
#[cfg(feature = "web_console")]
pub mod web;
mod tcp_stream_ext;

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
        RcBox {
            item: std::rc::Rc::new(item),
        }
    }
}
impl<T: ?Sized> AsRef<T> for RcBox<T> {
    fn as_ref(&self) -> &T {
        &self.item
    }
}
impl<T: ?Sized> Clone for RcBox<T> {
    fn clone(&self) -> Self {
        RcBox {
            item: self.item.clone(),
        }
    }
}
