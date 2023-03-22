use std::{
    env,
    fs::File,
    io::{self, Write},
    path::{Path, PathBuf},
};
use zip::write::{FileOptions, ZipWriter};

fn main() {
    if env::var("CARGO_FEATURE_RICH_WEB").is_err() {
        return;
    }
    let output_dir = env::var("OUT_DIR").expect("OUT_DIR environment variable not set");
    let zip_path = PathBuf::from(output_dir).join("moproxy-web.zip");
    let dist_path = Path::new("../web/dist/");
    let index_path = dist_path.join("index.html");
    println!("cargo:rerun-if-changed={}", index_path.display());

    if !index_path.exists() {
        panic!(
            "{} not found. Build `web` crate first, or disable `rich-web` feature.",
            index_path.display()
        );
    }
    pack_zip(&dist_path, &zip_path).expect("failed to pack web static files");

    println!(
        "cargo:rustc-env=MOPROXY_WEB_BUNDLE={}",
        zip_path.into_os_string().into_string().unwrap()
    );
}

fn pack_zip(src: &Path, dst: &Path) -> io::Result<()> {
    let mut file = File::create(dst)?;
    let mut zip = ZipWriter::new(&mut file);
    let opts = FileOptions::default()
        .compression_method(zip::CompressionMethod::Zstd)
        .compression_level(Some(12));
    for entry in src.read_dir()? {
        let entry = entry?;
        zip.start_file(entry.file_name().into_string().unwrap(), opts)?;
        let mut input = File::open(entry.path())?;
        io::copy(&mut input, &mut zip)?;
    }
    zip.finish()?.flush()?;
    Ok(())
}
