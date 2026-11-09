//! Lets the linker find Homebrew's SDL2. Recent macOS toolchains no longer
//! search /usr/local/lib (Intel) or /opt/homebrew/lib (Apple Silicon) by default.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        for dir in ["/opt/homebrew/lib", "/usr/local/lib"] {
            if std::path::Path::new(dir).join("libSDL2.dylib").exists() {
                println!("cargo:rustc-link-search=native={dir}");
            }
        }
    }
}
