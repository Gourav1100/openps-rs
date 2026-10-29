fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        for f in ["VideoToolbox", "CoreMedia", "CoreVideo", "CoreFoundation", "AVFoundation", "QuartzCore"] {
            println!("cargo:rustc-link-lib=framework={f}");
        }
    }
}
