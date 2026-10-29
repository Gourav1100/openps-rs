# openps-rp

Local-network Remote Play protocol: key tables and derivation, registration (pairing), session init and control
channel, and a blocking client (`client::register`, `client::open_session`).

## Key tables (not in this repository)

The key derivation uses constant tables that are embedded in the original application. They are not committed
here. Generate them once from decompiled source of the original `gb` class and keep the file outside the tree:

```sh
cargo run -p openps-rp --bin extract-tables -- /path/to/decompiled/gb.java /path/to/tables.json
export OPENPS_TABLES=/path/to/tables.json
```

`KeyTables::from_env()` / `KeyTables::load()` read it. Tests that need the tables look at `$OPENPS_TABLES`, then
at `analysis/tables/tables.json` (next to the repository), and print `SKIPPED` if neither exists, so a plain
`cargo test` works on a clean checkout.

## What is verified against what

* `kdf` (session and registration ciphers for PS5, PS4 and legacy firmware) is checked against known-answer
  vectors produced by running the original application's own classes, so it matches the real implementation.
* Request building, header formats and the control-channel codec follow the decompiled code and are tested
  against a fake console written from the same spec. That proves internal consistency, not wire compatibility
  with a real console; a captured session is still needed for that.
