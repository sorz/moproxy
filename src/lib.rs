#![feature(async_await)]
#![feature(async_closure)]
pub mod client;
pub mod monitor;
pub mod proxy;
pub mod tcp;
mod tcp_stream_ext;
#[cfg(feature = "web_console")]
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
pub struct ArcBox<T: ?Sized> {
    item: std::sync::Arc<Box<T>>,
}
impl<T: ?Sized> ArcBox<T> {
    fn new(item: Box<T>) -> Self {
        ArcBox {
            item: std::sync::Arc::new(item),
        }
    }
}
impl<T: ?Sized> AsRef<T> for ArcBox<T> {
    fn as_ref(&self) -> &T {
        &self.item
    }
}
impl<T: ?Sized> Clone for ArcBox<T> {
    fn clone(&self) -> Self {
        ArcBox {
            item: self.item.clone(),
        }
    }
}
