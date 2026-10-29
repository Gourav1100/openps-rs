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
    // Owner-only from the first byte: create fresh (never reuse a file with looser permissions).
    let _ = std::fs::remove_file(out);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let written = opts.open(out).and_then(|mut f| std::io::Write::write_all(&mut f, cfg.to_json().as_bytes()));
    if let Err(e) = written {
        eprintln!("cannot write {out}: {e}");
        std::process::exit(1);
    }
    println!("wrote {out} (mode 0600; contains a secret, keep it out of version control)");
}
