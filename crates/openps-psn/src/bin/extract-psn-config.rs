//! `extract-psn-config <decompiled PsnConstants.java> <out psn.json>`
//!
//! The OAuth client id and secret belong to Sony, not to this project, so they are not committed. Run this once
//! against decompiled source of your own copy of the original application, keep the JSON OUTSIDE the repository
//! and set `OPENPS_PSN=/path/to/psn.json`. The file contains a secret: restrict its permissions.
use openps_psn::config::PsnConfig;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [src, out] = args.as_slice() else {
        eprintln!("usage: extract-psn-config <decompiled PsnConstants.java> <out psn.json>");
        std::process::exit(2);
    };
    let text = std::fs::read_to_string(src).unwrap_or_else(|e| {
        eprintln!("cannot read {src}: {e}");
        std::process::exit(1);
    });
    let cfg = PsnConfig::from_decompiled_constants(&text).unwrap_or_else(|e| {
        eprintln!("extraction failed: {e}");
        std::process::exit(1);
    });
    std::fs::write(out, cfg.to_json()).unwrap_or_else(|e| {
        eprintln!("cannot write {out}: {e}");
        std::process::exit(1);
    });
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(out, std::fs::Permissions::from_mode(0o600));
    }
    println!("wrote {out} (mode 0600; contains a secret, keep it out of version control)");
}
