use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let key_file = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../req_key.txt");
    println!("cargo:rerun-if-changed={}", key_file.display());
    println!("cargo:rerun-if-env-changed=HEBNIX_REQ_KEY");
    let key = env::var("HEBNIX_REQ_KEY")
        .ok()
        .or_else(|| fs::read_to_string(&key_file).ok())
        .unwrap_or_default();
    let key = key.trim();
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    fs::write(out.join("req_key.rs"), format!("pub const KEY: &str = {key:?};\n"))
        .expect("write embedded request key");
}