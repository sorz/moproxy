pub mod client;
pub mod monitor;
pub mod proxy;
#[cfg(all(feature = "systemd", target_os = "linux"))]
pub mod systemd;
#[cfg(target_os = "linux")]
pub mod tcp;
#[cfg(feature = "web_console")]
pub mod web;

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
