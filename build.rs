use std::{
    env,
    fs::File,
    path::{Path, PathBuf},
};

const ZIP_URL: &str = "https://github.com/sorz/moproxy-web/releases/download/{VERSION}/build.zip";
const VERSION: &str = "v0.1.7";

fn main() {
    if env::var("CARGO_FEATURE_RICH_WEB").is_err() {
        return;
    }
    let output_dir = env::var("OUT_DIR").expect("OUT_DIR environment variable not set");
    let zip_path = PathBuf::from(output_dir).join(format!("moproxy-web-{}.zip", VERSION));
    if !zip_path.exists() {
        download_zip(&zip_path);
    }
    println!(
        "cargo:rustc-env=MOPROXY_WEB_BUNDLE={}",
        zip_path.into_os_string().into_string().unwrap()
    );
}

fn download_zip(path: &Path) {
    let url = ZIP_URL.replace("{VERSION}", VERSION);
    let mut resp = reqwest::blocking::get(&url)
        .expect("error on get moproxy-web bundle")
        .error_for_status()
        .expect("unexpect HTTP response");
    let mut zip = File::create(path).expect("cannot create file");
    resp.copy_to(&mut zip)
        .expect("error on download/write out moproxy-web bundle");
}
