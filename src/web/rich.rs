use bytes::Bytes;
use parking_lot::Mutex;
use std::io::{Cursor, Read};
use zip::read::ZipArchive;

pub struct ResourceBundle {
    zip: Mutex<ZipArchive<Cursor<Bytes>>>,
}

impl ResourceBundle {
    pub fn new() -> Self {
        let bytes = Bytes::from_static(include_bytes!(env!("MOPROXY_WEB_BUNDLE")));
        let zip = ZipArchive::new(Cursor::new(bytes))
            .expect("broken moproxy-web bundle")
            .into();
        ResourceBundle { zip }
    }

    pub fn get(&self, path: &str) -> Option<(&'static str, Vec<u8>)> {
        let name = if path.starts_with('/') {
            &path[1..]
        } else {
            &path
        };

        let mut zip = self.zip.lock();
        let mut file = zip.by_name(name).ok()?;
        if !file.is_file() {
            return None;
        }
        let mut content = Vec::with_capacity(file.size() as usize);
        file.read_to_end(&mut content)
            .expect("error on read moproxy-web bundle");

        let name_ext = name.rsplitn(2, '.').next();
        let mime = match name_ext {
            Some("html") => "text/html",
            Some("js") => "application/javascript",
            Some("css") => "text/css",
            Some("txt") => "text/plain",
            Some("json") | Some("map") => "application/json",
            _ => "application/octet-stream",
        };

        (mime, content).into()
    }
}
