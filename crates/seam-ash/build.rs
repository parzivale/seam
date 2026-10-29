//! `cfg(drm)`: the systems with dmabufs and DRM fourcc formats.
fn main() {
    println!("cargo::rustc-check-cfg=cfg(drm)");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if matches!(
        os.as_str(),
        "linux" | "android" | "freebsd" | "openbsd" | "netbsd" | "dragonfly"
    ) {
        println!("cargo::rustc-cfg=drm");
    }
}
