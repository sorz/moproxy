use bytes::Bytes;
use std::io::Cursor;
use zip::read::ZipArchive;


pub struct ResourceBundle {
    zip: ZipArchive<Cursor<Bytes>>,
}

impl ResourceBundle {
    pub fn new() -> Self {
        let bytes = Bytes::from_static(include_bytes!(env!("MOPROXY_WEB_BUNDLE")));
        let zip = ZipArchive::new(Cursor::new(bytes)).expect("broken moproxy-web bundle");
        ResourceBundle { zip }
    }
}

