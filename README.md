# openPS

A Rust + Tauri port of a PlayStation Remote Play client. Work in progress: the protocol stack, input system, pairing
flow, application core, hardware video decode (HEVC preferred, H.264 fallback), audio and the UI exist and are tested
against a built-in demo console. Nothing has been tried against a real console yet.

## Layout

| Crate | Role |
|---|---|
| `openps-fec`, `openps-crypto`, `openps-packets` | FEC (Cauchy Reed-Solomon), Takion crypto, packet layouts |
| `openps-takion` | protobuf, control chunks, handshake, Senkusha network test |
| `openps-stream` | video/audio/haptic receive, FEC recovery, acks, feedback, congestion |
| `openps-session` | session runtime on a UDP link (threads, bounded media delivery) |
| `openps-discovery`, `openps-rp` | discovery/wake; registration, session init, control channel, key derivation |
| `openps-psn` | PSN OAuth sign-in (no live client unless the `live` feature is enabled) |
| `openps-input`, `openps-pad` | keyboard/mouse to pad mapping engine; pad state to Takion feedback |
| `openps-core` | application core: console store, pairing/connect flows, events (no UI dependency) |
| `openps-video` | H.264/HEVC bitstream handling, VideoToolbox decode, AVSampleBufferDisplayLayer renderer (macOS) |
| `openps-audio` | Opus decode (AudioToolbox) and AudioQueue playback with a jitter buffer (macOS) |
| `openps-demo` | pretend PS5 that streams a built-in clip through the real stack (demo and end-to-end tests) |
| `openps-testkit` | mock PlayStation console used by tests |
| `src-tauri` | Tauri shell: commands and events over `openps-core` |

## Running

```sh
cargo test --workspace --exclude openps-app          # all tests (no network access, no hardware)
cargo clippy --workspace --all-targets               # must be warning-free
cargo tauri dev                                      # run the app (needs the setup below to pair a console)

# try everything without a console: a built-in demo PS5 (PIN: any 8 digits except 00000000)
cargo build -p openps-app --features demo,tauri/custom-protocol
OPENPS_DEMO=hevc ./target/debug/openps-app           # or h264; OPENPS_DEMO_LOSS=1 drops packets; OPENPS_DEMO_AUDIO=0 mutes
```

## Setup needed to pair and stream

Two things are deliberately not in the repository:

1. **Key tables** (Sony protocol constants used by the key derivation). Generate once from decompiled source of the
   original application and point `OPENPS_TABLES` at the result. See `crates/openps-rp/README.md`.
2. **PSN client credentials** (only for PS5 registration, which needs your PSN account id). Generate once with
   `extract-psn-config`, set `OPENPS_PSN`, and build the app with `--features psn-live`. Without that feature the app
   never contacts Sony. See `crates/openps-psn/README.md`.

## Where data lives

Everything the app stores (pairings, settings, registration keys, logs) is in `openps-data/` next to the executable, or in
`$OPENPS_HOME`. Secrets are in an owner-only file there, not the Keychain. Delete the folder to remove all traces.

## Status

See `../analysis/spec/05-implementation-status.md` for what is implemented, how each part was verified, and what is
still open. In short: everything is tested against mock consoles written from the same spec; the key derivations are
additionally verified against the original application's own code; nothing has been tried against a real console.
