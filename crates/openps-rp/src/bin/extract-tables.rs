//! `extract-tables <decompiled gb.java> <out tables.json>`
//!
//! The key tables are constants inside the original application and are intentionally NOT committed to this
//! repository. Run this once against decompiled source of the original `gb` class (for example the output of
//! Vineflower on the app's remote-play library), keep the JSON outside the source tree, and set
//! `OPENPS_TABLES=/path/to/tables.json` for the tests and the app.
use openps_rp::tables::KeyTables;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [src, out] = args.as_slice() else {
        eprintln!("usage: extract-tables <decompiled gb.java> <out tables.json>");
        std::process::exit(2);
    };
    let text = std::fs::read_to_string(src).unwrap_or_else(|e| {
        eprintln!("cannot read {src}: {e}");
        std::process::exit(1);
    });
    match KeyTables::from_gb_source(&text) {
        Ok(t) => {
            std::fs::write(out, t.to_json()).unwrap_or_else(|e| {
                eprintln!("cannot write {out}: {e}");
                std::process::exit(1);
            });
            println!("wrote {out} (8 tables validated)");
        }
        Err(e) => {
            eprintln!("extraction failed: {e}");
            std::process::exit(1);
        }
    }
}
