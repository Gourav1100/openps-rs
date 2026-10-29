fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        for f in ["AudioToolbox", "CoreAudio", "CoreFoundation"] {
            println!("cargo:rustc-link-lib=framework={f}");
        }
    }
}
