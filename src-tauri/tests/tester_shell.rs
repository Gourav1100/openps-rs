//! Tester A5: integration tests for the Tauri shell's pure functions. The shell crate keeps its modules private, so the source files are
//! compiled into this test binary with `#[path]`; functions that are private inside a module (`write_private`, `load_cached`, `valid_key`,
//! `with_ps`, ...) stay out of reach and are described in the report instead.
//! Note: because the files are included whole, their own `#[cfg(test)]` unit tests also run here (they are counted twice).
#![cfg(target_os = "macos")]
#![allow(dead_code, unused_imports)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub const EVENT: &str = "openps://event";
pub struct DataDir(pub PathBuf);

#[path = "../src/gamemaps.rs"]
mod gamemaps;
#[path = "../src/library.rs"]
mod library;
#[path = "../src/paths.rs"]
mod paths;
#[path = "../src/video.rs"]
mod video;

fn mode(p: &Path) -> u32 {
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

// ---------------------------------------------------------------------------------------------------------------- paths

#[test]
fn data_dir_resolution_edge_cases() {
    let exe = Path::new("/apps/OpenPS.app/Contents/MacOS/openps");
    // relative and unexpanded overrides are taken literally (the cwd of a Finder-launched app is "/")
    assert_eq!(paths::data_dir(Some("rel/dir"), exe), Path::new("rel/dir"));
    assert_eq!(paths::data_dir(Some("~/openps"), exe), Path::new("~/openps"));
    // surrounding spaces are NOT trimmed from a non-empty override (only an all-blank one is ignored)
    assert_eq!(paths::data_dir(Some(" /x "), exe), Path::new(" /x "));
    eprintln!(
        "exe '/': {:?}; exe '': {:?}",
        paths::data_dir(None, Path::new("/")),
        paths::data_dir(None, Path::new(""))
    );
    assert_eq!(paths::data_dir(None, Path::new("/")), Path::new("./openps-data"));
    assert_eq!(paths::data_dir(None, Path::new("")), Path::new("./openps-data"));
    assert_eq!(paths::data_dir(Some("\t\n"), exe), paths::data_dir(None, exe));
    let long = "a".repeat(10_000);
    assert_eq!(paths::data_dir(Some(&long), exe), Path::new(&long));
}

#[test]
fn ensure_makes_every_created_level_private_and_tightens_existing_ones() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path().join("a/b/c");
    paths::ensure(&d).unwrap();
    for p in [t.path().join("a"), t.path().join("a/b"), d.clone()] {
        assert_eq!(mode(&p), 0o700, "{}", p.display());
    }
    // an existing world-readable directory is tightened
    let open = t.path().join("open");
    std::fs::create_dir(&open).unwrap();
    std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o777)).unwrap();
    paths::ensure(&open).unwrap();
    assert_eq!(mode(&open), 0o700);
}

#[test]
fn ensure_fails_cleanly_on_a_file_a_dangling_link_and_an_unwritable_parent() {
    let t = tempfile::tempdir().unwrap();
    let f = t.path().join("file");
    std::fs::write(&f, b"x").unwrap();
    assert!(paths::ensure(&f).is_err() || !f.is_dir());
    assert!(paths::ensure(&f.join("sub")).is_err());
    let dangling = t.path().join("dangling");
    std::os::unix::fs::symlink(t.path().join("nowhere"), &dangling).unwrap();
    assert!(paths::ensure(&dangling).is_err());
    let ro = t.path().join("ro");
    std::fs::create_dir(&ro).unwrap();
    std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o500)).unwrap();
    let r = paths::ensure(&ro.join("data"));
    std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(r.is_err(), "a read-only parent must be reported (this is the translocated / read-only .app case)");
}

/// `ensure` follows a symlinked data directory and chmods the TARGET to 0700 (set_permissions follows links). Documented behaviour.
#[test]
fn ensure_on_a_symlinked_directory_changes_the_target_mode() {
    let t = tempfile::tempdir().unwrap();
    let target = t.path().join("target");
    std::fs::create_dir(&target).unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
    let link = t.path().join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    paths::ensure(&link).unwrap();
    assert_eq!(mode(&target), 0o700);
}

// ------------------------------------------------------------------------------------------------------------- gamemaps

#[test]
fn gamemaps_rejects_hostile_keys_and_names() {
    let d = tempfile::tempdir().unwrap();
    let bad_keys = [
        "",
        " ",
        "a b",
        "../x",
        "..",
        "a/b",
        "a\\b",
        "a\0b",
        "é",
        "ａｂｃ",
        "\u{202e}x",
        "x\n",
        &"k".repeat(33),
        "CON:",
        "a.b",
        "a%2e",
        "a;b",
    ];
    for k in bad_keys {
        assert!(gamemaps::set(d.path(), k, Some("ok")).is_err(), "key {k:?} accepted");
        assert!(gamemaps::set(d.path(), k, None).is_err(), "key {k:?} accepted for removal");
    }
    for k in ["PPSA01325", "CUSA-1", "a_b", &"k".repeat(32)] {
        assert!(gamemaps::set(d.path(), k, Some("ok")).is_ok(), "{k}");
    }
    let bad_names = ["", " ", "\t", "a\nb", "a\0b", "\u{7f}", &"p".repeat(65), &"é".repeat(65)];
    for p in bad_names {
        assert!(gamemaps::set(d.path(), "OK1", Some(p)).is_err(), "name {p:?} accepted");
    }
    assert!(gamemaps::set(d.path(), "OK1", Some(&"é".repeat(64))).is_ok(), "64 multi-byte chars are fine");
    let m = gamemaps::load(d.path());
    assert!(m.contains_key("OK1") && !m.contains_key("../x"));
    // nothing but the json (and no leftover tmp) in the data dir
    let names: Vec<_> =
        std::fs::read_dir(d.path()).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    assert_eq!(names, ["game-keymaps.json"]);
}

#[test]
fn gamemaps_is_capped_at_2000_entries_and_a_huge_file_loads_in_bounded_time() {
    let d = tempfile::tempdir().unwrap();
    let mut json = String::from("{");
    for i in 0..3000 {
        if i > 0 {
            json.push(',');
        }
        json.push_str(&format!("\"G{i:05}\":\"p{i}\""));
    }
    json.push('}');
    std::fs::write(d.path().join("game-keymaps.json"), &json).unwrap();
    assert_eq!(gamemaps::load(d.path()).len(), 2000);
    // at the cap: new keys refused, existing keys updatable
    assert_eq!(gamemaps::set(d.path(), "NEWKEY", Some("p")).unwrap_err(), "too many saved choices");
    assert!(gamemaps::set(d.path(), "G00000", Some("changed")).is_ok());
    assert!(gamemaps::set(d.path(), "G00000", None).is_ok());

    // a 60 MB file with 1.5 M entries: the whole thing is parsed before the cap applies
    let mut big = String::from("{");
    for i in 0..1_500_000 {
        if i > 0 {
            big.push(',');
        }
        big.push_str(&format!("\"G{i}\":\"profile{i}\""));
    }
    big.push('}');
    std::fs::write(d.path().join("game-keymaps.json"), &big).unwrap();
    let t0 = std::time::Instant::now();
    let m = gamemaps::load(d.path());
    eprintln!("load of a {} MB file: {:?}, {} entries kept", big.len() / 1_000_000, t0.elapsed(), m.len());
    assert!(t0.elapsed() < std::time::Duration::from_secs(20));
    assert_eq!(m.len(), 2000);
}

#[test]
fn gamemaps_damaged_wrong_typed_and_binary_files_mean_no_choices() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("game-keymaps.json");
    for content in [
        &b""[..],
        b"null",
        b"[]",
        b"{\"A\":1}",
        b"{\"A\":null}",
        b"{\"A\":[\"x\"]}",
        b"\xff\xfe\x00\x00",
        b"{\"A\":\"x\"",
        b"{\"A\":\"x\",\"A\":\"y\"}",
        &[b'['; 100_000][..],
    ] {
        std::fs::write(&f, content).unwrap();
        let m = gamemaps::load(d.path());
        assert!(m.len() <= 1, "{content:?} -> {m:?}");
    }
    // a damaged file is replaced silently by the next save (the old content is not backed up)
    std::fs::write(&f, b"{ broken").unwrap();
    gamemaps::set(d.path(), "A1", Some("p")).unwrap();
    assert_eq!(gamemaps::load(d.path()).len(), 1);
}

#[test]
fn gamemaps_failure_modes_are_errors_not_panics() {
    let t = tempfile::tempdir().unwrap();
    // data dir does not exist
    assert!(gamemaps::set(&t.path().join("missing"), "A1", Some("p")).is_err());
    // a directory squatting on the old fixed tmp name no longer matters: temporary names are unique per write
    std::fs::create_dir(t.path().join("game-keymaps.tmp")).unwrap();
    assert!(gamemaps::set(t.path(), "A1", Some("p")).is_ok());
    // the target is a directory
    let t2 = tempfile::tempdir().unwrap();
    std::fs::create_dir(t2.path().join("game-keymaps.json")).unwrap();
    assert!(gamemaps::load(t2.path()).is_empty());
    assert!(gamemaps::set(t2.path(), "A1", Some("p")).is_err());
    // read-only data dir
    let t3 = tempfile::tempdir().unwrap();
    std::fs::set_permissions(t3.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
    let r = gamemaps::set(t3.path(), "A1", Some("p"));
    std::fs::set_permissions(t3.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(r.is_err());
}

/// Profile names are only checked for emptiness, control characters and length. Bidi overrides and zero-width characters (which
/// `openps_core::types::clean_text` strips from console names precisely because they disguise text) are accepted and stored.
#[test]
fn gamemaps_accepts_invisible_and_bidi_characters_in_profile_names() {
    let d = tempfile::tempdir().unwrap();
    for p in ["\u{202e}evil", "a\u{200b}b", "\u{feff}x", "  padded  "] {
        assert!(gamemaps::set(d.path(), "A1", Some(p)).is_ok(), "{p:?}");
    }
    assert_eq!(
        gamemaps::load(d.path())["A1"],
        "  padded  ",
        "stored untrimmed: a lookup by the trimmed name would miss it"
    );
}

/// `set` is load -> modify -> write with a fixed temporary name and no lock. Two overlapping calls lose updates or fail on the rename.
/// Harmless while Tauri runs this (synchronous) command on the main thread, but `game_maps`/`set_game_map` sit beside `async` commands
/// and any future `async`/`(async)` marking turns it into a data race.
#[test]
fn gamemaps_concurrent_sets_do_not_lose_updates() {
    let d = tempfile::tempdir().unwrap();
    let dir = std::sync::Arc::new(d.path().to_path_buf());
    let mut hs = vec![];
    for t in 0..8 {
        let dir = dir.clone();
        hs.push(std::thread::spawn(move || {
            let mut fails = 0;
            for i in 0..25 {
                if gamemaps::set(&dir, &format!("T{t}K{i}"), Some("p")).is_err() {
                    fails += 1;
                }
            }
            fails
        }));
    }
    let fails: i32 = hs.into_iter().map(|h| h.join().unwrap()).sum();
    let kept = gamemaps::load(d.path()).len();
    assert!(fails == 0 && kept == 200, "{fails} failed saves, {kept} of 200 entries survived");
}

// -------------------------------------------------------------------------------------------------------------- library

fn decode_b64(s: &str) -> Option<Vec<u8>> {
    let v = |c: u8| match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    let b = s.as_bytes();
    if !b.len().is_multiple_of(4) {
        return None;
    }
    let mut out = vec![];
    for q in b.chunks(4) {
        let pad = q.iter().filter(|&&c| c == b'=').count();
        let n = (u32::from(v(q[0])?) << 18)
            | (u32::from(v(q[1])?) << 12)
            | (if q[2] == b'=' { 0 } else { u32::from(v(q[2])?) << 6 })
            | (if q[3] == b'=' { 0 } else { u32::from(v(q[3])?) });
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }
    Some(out)
}

#[test]
fn base64_round_trips_and_matches_the_system_tool() {
    let mut x = 0x1234_5678_9abc_def0u64;
    let mut rnd = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    for len in (0..600usize).chain([4095, 4096, 4097, 65_535, 900 * 1024, 1 << 20]) {
        let data: Vec<u8> = (0..len).map(|_| rnd() as u8).collect();
        let enc = library::base64(&data);
        assert_eq!(enc.len(), len.div_ceil(3) * 4, "length {len}");
        assert!(enc.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'+' || c == b'/' || c == b'='));
        assert_eq!(decode_b64(&enc).unwrap(), data, "length {len}");
    }
    // against /usr/bin/base64
    let data: Vec<u8> = (0..777u32).map(|i| (i * 31 % 251) as u8).collect();
    let mut child = std::process::Command::new("/usr/bin/base64")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(child.stdin.as_mut().unwrap(), &data).unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(String::from_utf8(out.stdout).unwrap().trim(), library::base64(&data));
}

#[test]
fn cache_key_is_collision_free_on_two_million_cover_like_urls_and_each_half_is_independent() {
    use std::collections::HashSet;
    let mut full = HashSet::new();
    let mut half = HashSet::new();
    let mut n = 0u64;
    let hosts = [
        "image.api.playstation.com",
        "image.api.np.km.playstation.net",
        "store.playstation.com",
        "psnobj.prod.dl.playstation.net",
    ];
    for h in hosts {
        for i in 0..500_000u64 {
            let url = match i % 4 {
                0 => format!("v2|https://{h}/cdn/UP{:04}/CUSA{:05}_00/{}.png", i % 9999, i % 99_999, i),
                1 => format!("v2|https://{h}/{i}"),
                2 => format!("v2|https://{h}/a/{i}?w=400"),
                _ => format!("v2|https://{h}/{i:x}/{i:o}"),
            };
            let k = library::cache_key(&url);
            assert_eq!(k.len(), 32);
            half.insert(k[..16].to_string());
            assert!(full.insert(k), "128-bit collision for {url}");
            n += 1;
        }
    }
    eprintln!("{n} urls: {} distinct 128-bit keys, {} distinct first halves", full.len(), half.len());
    assert_eq!(half.len() as u64, n, "the first 64-bit half alone collides");
    // near-identical inputs differ
    assert_ne!(library::cache_key("a"), library::cache_key("a "));
    assert_ne!(library::cache_key(""), library::cache_key("\0"));
    assert_ne!(library::cache_key("v2|x"), library::cache_key("v1|x"));
}

#[test]
fn cache_key_is_a_pure_function_of_the_bytes_and_never_panics() {
    for s in ["", "\0", "\u{10ffff}", &"x".repeat(1 << 20), "../../../../etc/passwd", "a/b", "a\\b", "%2e%2e"] {
        let k = library::cache_key(s);
        assert_eq!(k.len(), 32);
        assert!(
            k.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
            "{k}: a file name must be only [0-9a-f]"
        );
    }
}

#[test]
fn pick_duid_matching_rules() {
    let l = |v: &[(&str, &str)]| v.iter().map(|(a, b)| ((*a).to_string(), (*b).to_string())).collect::<Vec<_>>();
    // exact name, case and whitespace-insensitive
    assert_eq!(
        library::pick_duid("  Living Room ", &l(&[("d1", "living room"), ("d2", "Other")])).as_deref(),
        Some("d1")
    );
    // two consoles with the same name: the first one silently wins
    assert_eq!(library::pick_duid("PS5", &l(&[("d1", "PS5"), ("d2", "PS5")])).as_deref(), Some("d1"));
    // unknown name and several consoles: refuse
    assert_eq!(library::pick_duid("x", &l(&[("d1", "a"), ("d2", "b")])), None);
    // the only console on the account is chosen whatever the paired name is
    assert_eq!(library::pick_duid("some other console", &l(&[("d1", "PS5-591")])).as_deref(), Some("d1"));
    assert_eq!(library::pick_duid("", &l(&[("d1", "")])).as_deref(), Some("d1"));
    assert_eq!(library::pick_duid("", &l(&[("d1", ""), ("d2", "")])), None, "an empty name never matches by name");
    // unicode: Turkish dotted I, German sharp s
    let _ = library::pick_duid("İ", &l(&[("d1", "i̇"), ("d2", "x")]));
    let _ = library::pick_duid("STRASSE", &l(&[("d1", "straße"), ("d2", "x")]));
    // huge input
    let big = "x".repeat(1 << 20);
    assert_eq!(library::pick_duid(&big, &l(&[("d1", &big), ("d2", "y")])).as_deref(), Some("d1"));
}

// ------------------------------------------------------------------------------------------------- art url allow-list

/// Strings that `art_url_allowed` accepts although a WHATWG URL parser (which `ureq`, the app's HTTP client, uses through the `url` crate)
/// resolves to another host. `tauri::Url` is the same `url::Url`.
fn smuggled(urls: &[&str]) -> Vec<String> {
    urls.iter()
        .filter(|u| openps_psn::library::art_url_allowed(u))
        .filter(|u| match tauri::Url::parse(u) {
            Ok(p) => {
                let host = p.host_str().unwrap_or("").to_ascii_lowercase();
                !(host.ends_with(".playstation.com")
                    || host.ends_with(".playstation.net")
                    || host.ends_with(".sonyentertainmentnetwork.com"))
            }
            Err(_) => false, // unparseable: the client refuses it too
        })
        .map(|u| (*u).to_string())
        .collect()
}

#[test]
fn art_url_allow_list_ordinary_attacks_are_refused() {
    let refused = [
        "http://image.api.playstation.com/a.png",
        "HTTPS://image.api.playstation.com/a.png",
        "https://image.api.playstation.com@evil.com/a.png",
        "https://evil.com/image.api.playstation.com/a.png",
        "https://evil.com#.playstation.com/",
        "https://evil.com?.playstation.com/",
        "https://image.api.playstation.com.evil.com/a.png",
        "https://evilplaystation.com/a.png",
        "https://playstation.com/a.png",
        "https://image.api.playstation.com:8443/a.png",
        "https://[::1]/a.png",
        "https://127.0.0.1/a.png",
        "https:///a.png",
        "https://",
        "file:///etc/passwd",
        "data:image/png;base64,AAAA",
        "javascript:alert(1)",
        "//image.api.playstation.com/a.png",
        "",
    ];
    for u in refused {
        assert!(!openps_psn::library::art_url_allowed(u), "{u:?} was allowed");
    }
    assert!(openps_psn::library::art_url_allowed("https://image.api.playstation.com/a.png"));
    assert!(openps_psn::library::art_url_allowed("https://IMAGE.API.PLAYSTATION.COM/a.png"));
    assert!(!openps_psn::library::art_url_allowed(&format!("https://image.api.playstation.com/{}", "a".repeat(2100))));
}

/// BUG T10-01: `art_url_allowed` cuts the host at `/ ? #` only. A backslash is a path separator for https URLs in the WHATWG parser that
/// `ureq` uses, so `https://evil.example\.playstation.com/x.png` passes the allow-list (the "host" string ends in `.playstation.com`) but
/// the request goes to `evil.example`. The same holds for tab/CR/LF inside the host (stripped by the parser).
#[test]
fn art_url_allow_list_cannot_be_smuggled_past_with_a_backslash() {
    let bad = smuggled(&[
        "https://evil.example\\.playstation.com/x.png",
        "https://127.0.0.1\\.playstation.com/x.png",
        "https://evil.example\\@image.api.playstation.com/x.png",
        "https://evil.example\t\\.playstation.com/x.png",
    ]);
    assert!(bad.is_empty(), "allowed but resolving elsewhere: {bad:?}");
}

/// Pins the evidence for T10-01 without being ignored: prints what the parser makes of the smuggled strings.
#[test]
fn art_url_backslash_evidence() {
    for u in ["https://evil.example\\.playstation.com/x.png", "https://127.0.0.1\\.playstation.com/x.png"] {
        let allowed = openps_psn::library::art_url_allowed(u);
        let host = tauri::Url::parse(u).ok().and_then(|p| p.host_str().map(str::to_string));
        eprintln!("{u:?}: allow-list says {allowed}, URL parser host = {host:?}");
    }
}

// --------------------------------------------------------------------------------------- IPC argument -> render settings

#[test]
fn render_settings_accepts_every_possible_stream_option_without_panicking() {
    use openps_core::StreamOptions;
    for fps in [i32::MIN, -1, 0, 1, 30, 60, 65_535, 65_536, i32::MAX] {
        for display in [0u16, 1, 29, 61, 144, u16::MAX] {
            for (sh, sat) in [(0u8, 0u8), (50, 100), (200, 200), (255, 255)] {
                let o = StreamOptions {
                    fps,
                    display_fps: display,
                    sharpness: sh,
                    saturation: sat,
                    upscale: "\u{0}".repeat(3),
                    frame_mode: "x".repeat(100_000),
                    engine: String::new(),
                    ..Default::default()
                };
                let r = video::render_settings(&o);
                assert!(r.sharpness.is_finite() && r.saturation.is_finite());
                assert_eq!(r.display_hz, video::AUTO_RATE_HZ, "the old display rate is no longer asked for");
            }
        }
    }
}

/// The engine choice of the settings reaches the renderer as the right switch, and each filter's own switch turns its effect off.
#[test]
fn the_engine_choice_maps_to_the_renderer_switches() {
    use openps_core::StreamOptions;
    let o = |engine: &str| video::render_settings(&StreamOptions { engine: engine.into(), ..Default::default() });
    let r = o("metalfx");
    assert!(r.metalfx && !r.fsr, "{r:?}");
    let r = o("fsr");
    assert!(r.metalfx && r.fsr, "FSR upscales; MetalFX still makes the generated midpoints: {r:?}");
}

#[test]
fn every_filter_switch_reaches_the_renderer_as_the_value_that_leaves_the_picture_alone() {
    use openps_core::StreamOptions;
    let on = StreamOptions { upscale: "2x".into(), sharpness: 80, rcas: 150, saturation: 130, ..Default::default() };
    let r = video::render_settings(&on);
    assert_eq!((r.sharpness, r.rcas, r.saturation), (0.8, 1.5, 1.3));
    assert!(r.linear_light && r.adaptive);
    let off = StreamOptions {
        sharpness_on: false,
        rcas_on: false,
        saturation_on: false,
        linear_light: false,
        adaptive_fps: false,
        pacing_buffer: true,
        uncapped_rate: false,
        ..on.clone()
    };
    let r = video::render_settings(&off);
    assert!(
        r.jitter_buffer && !r.uncapped,
        "the smoothing buffer is on and the display's rate is the limit unless chosen"
    );
    let both = video::render_settings(&StreamOptions { pacing_buffer: false, uncapped_rate: true, ..off.clone() });
    assert!(!both.jitter_buffer && both.uncapped, "the two options reach the renderer");
    assert_eq!(r.sharpness, 1.0, "with an upscaler, off is the picture as it draws it");
    assert_eq!((r.rcas, r.saturation), (0.0, 1.0));
    assert!(!r.linear_light && !r.adaptive);
    // without upscaling the sharpness setting is an unsharp mask around 50 %: off is 50 %
    let r = video::render_settings(&StreamOptions { upscale: "off".into(), ..off });
    assert_eq!(r.sharpness, 0.5);
}

#[test]
fn frame_generation_fills_the_display_and_off_shows_frames_as_they_arrive() {
    use openps_core::StreamOptions;
    let on = video::render_settings(&StreamOptions { framegen_on: true, ..Default::default() });
    assert_eq!((on.gen_factor, on.display_hz), (0.0, video::AUTO_RATE_HZ), "on: ask for the most, the display caps it");
    let off = video::render_settings(&StreamOptions { framegen_on: false, ..Default::default() });
    assert_eq!(off.gen_factor, 1.0, "off: the stream's own rate, nothing generated");
    // no stored rate or old display rate changes what is asked for
    for display_fps in [0u16, 30, 75, 144, u16::MAX] {
        let r = video::render_settings(&StreamOptions { display_fps, ..Default::default() });
        assert_eq!((r.gen_factor, r.display_hz), (0.0, video::AUTO_RATE_HZ), "display_fps {display_fps}");
    }
}

/// BUG T10-02: `connect` calls `apply_video` (which pushes the options into the live renderer through `video::render_settings`) BEFORE
/// `core.connect` validates them. A rejected `options` argument (display_fps 65535, sharpness 255, fps 0, ...) has therefore already
/// changed the running renderer's settings although the call returned an error. Until lib.rs validates first, `render_settings` is the
/// last place that can refuse: it must not hand the renderer a value `StreamOptions::validate` would reject.
#[test]
fn invalid_options_must_not_reach_the_renderer() {
    use openps_core::StreamOptions;
    let o = StreamOptions { display_fps: 65_535, sharpness: 255, saturation: 255, fps: 0, ..Default::default() };
    assert!(o.validate().is_err());
    let r = video::render_settings(&o);
    let sane = r.display_hz == video::AUTO_RATE_HZ && r.sharpness <= 2.0 && r.saturation <= 2.0;
    assert!(sane, "the renderer would receive {r:?}");
}

#[test]
fn stream_options_validation_rejects_the_hostile_json_the_ipc_can_deliver() {
    use openps_core::StreamOptions;
    let base = serde_json::to_value(StreamOptions::default()).unwrap();
    let with = |k: &str, v: serde_json::Value| {
        let mut b = base.clone();
        b[k] = v;
        serde_json::from_value::<StreamOptions>(b)
    };
    // wrong types are rejected by serde (before validate)
    for (k, v) in [
        ("width", serde_json::json!("1280")),
        ("fps", serde_json::json!(60.5)),
        ("volume", serde_json::json!(256)),
        ("volume", serde_json::json!(-1)),
        ("display_fps", serde_json::json!(70_000)),
        ("sharpness", serde_json::json!(300)),
        ("hevc", serde_json::json!("yes")),
        ("upscale", serde_json::json!(5)),
    ] {
        assert!(with(k, v.clone()).is_err(), "{k}={v}");
    }
    // right types, wrong values are rejected by validate
    for (k, v) in [
        ("width", serde_json::json!(i32::MAX)),
        ("height", serde_json::json!(i32::MIN)),
        ("fps", serde_json::json!(0)),
        ("bitrate", serde_json::json!(i32::MIN)),
        ("bitrate", serde_json::json!(-2)),
        ("upscale", serde_json::json!("8k")),
        ("upscale", serde_json::json!("x".repeat(1_000_000))),
        ("display_fps", serde_json::json!(0)),
        ("display_fps", serde_json::json!(240)),
        ("frame_mode", serde_json::json!("")),
        ("engine", serde_json::json!("\u{202e}metalfx")),
        ("volume", serde_json::json!(101)),
        ("sharpness", serde_json::json!(201)),
        ("saturation", serde_json::json!(201)),
    ] {
        let o = with(k, v.clone()).unwrap();
        assert!(o.validate().is_err(), "{k}={v:?} passed validate");
    }
    // overflow in width/height arithmetic: height*16/9 with i32::MAX must not panic (it is rejected by the `matches!` first)
    let o = StreamOptions { height: i32::MAX, width: i32::MAX, ..Default::default() };
    assert!(o.validate().is_err());
    // unknown fields are ignored (serde default) and missing ones take defaults
    let o: StreamOptions = serde_json::from_value(
        serde_json::json!({"width":1280,"height":720,"fps":60,"bitrate":-1,"hevc":false,"hdr":false,"extra":1}),
    )
    .unwrap();
    assert!(o.validate().is_ok());
}

// ------------------------------------------------------------------------------------------------------ input arguments

#[test]
fn the_input_hub_survives_what_the_input_commands_can_be_sent() {
    use openps_macinput::InputHub;
    let hub = InputHub::new();
    for code in ["", "KeyA", &"K".repeat(1 << 20), "\u{0}", "\u{202e}", "Key\nA", "ArrowUp\0", "💥"] {
        hub.key_event(code, true, false);
        hub.key_event(code, false, true);
    }
    for b in [i32::MIN, -1, 0, 1, 2, 3, 4, 5, 6, 7, 8, 100, i32::MAX] {
        hub.button_event(b, true);
        hub.button_event(b, false);
    }
    for (dx, dy) in [
        (f64::NAN, f64::NAN),
        (f64::INFINITY, f64::NEG_INFINITY),
        (1e308, -1e308),
        (0.0, -0.0),
        (f64::MIN_POSITIVE, 5e-324),
    ] {
        hub.wheel(dx, dy);
    }
    let _ = hub.take_pressed();
    // key_down flood without releases stays bounded (a BTreeSet of a fixed alphabet)
    for i in 0..100_000 {
        hub.key_event(if i % 2 == 0 { "KeyA" } else { "KeyB" }, true, false);
    }
    assert!(hub.pressed().len() <= 2);
    hub.set_focused(false);
    assert!(hub.pressed().is_empty(), "losing focus releases everything");
    for b in [
        (f64::NAN, 0.0, 1.0, 1.0),
        (0.0, 0.0, 0.0, 0.0),
        (0.0, 0.0, -1.0, 5.0),
        (f64::INFINITY, 0.0, 1.0, 1.0),
        (0.0, 0.0, f64::INFINITY, 1.0),
    ] {
        hub.set_bounds(Some(b));
        assert!(hub.bounds().is_none(), "{b:?}");
    }
}

#[test]
fn ui_events_from_hostile_json_are_ignored_or_parsed_never_panic() {
    use openps_input::engine::UiEvent;
    let cases = [
        serde_json::json!(null),
        serde_json::json!([]),
        serde_json::json!({}),
        serde_json::json!({"type": 5}),
        serde_json::json!({"type": "edit"}),
        serde_json::json!({"type": "edit", "control": "x", "slot": "1"}),
        serde_json::json!({"type": "edit", "control": "x", "slot": i64::MAX}),
        serde_json::json!({"type": "edit", "control": "x", "slot": i64::MIN}),
        serde_json::json!({"type": "slider", "value": 1e308}),
        serde_json::json!({"type": "slider", "value": "1"}),
        serde_json::json!({"type": "profile_rename", "name": "x".repeat(1 << 20)}),
        serde_json::json!({"type": "pick", "input": "\u{0}"}),
        serde_json::json!({"type": "nope"}),
    ];
    for c in cases {
        let _ = UiEvent::from_json(&c);
    }
    // deeply nested JSON (serde_json's own depth limit is 128 on parse; Value built in code has none)
    let mut v = serde_json::json!({"type": "cancel"});
    for _ in 0..2_000 {
        let mut m = serde_json::Map::new();
        m.insert("type".into(), serde_json::json!("cancel"));
        m.insert("x".into(), v);
        v = serde_json::Value::Object(m);
    }
    assert!(UiEvent::from_json(&v).is_some());
    std::mem::forget(v); // dropping a 50k-deep Value recurses; not our subject
}

#[test]
fn friendly_config_error_handles_hostile_parser_messages() {
    for raw in [
        "",
        "line ",
        "line 99999999999999999999",
        "line 3 line 4",
        "LINE 3",
        &"line ".repeat(10_000),
        "line \u{0661}",
        &format!("line {}", "9".repeat(100_000)),
    ] {
        let m = openps_input_friendly(raw);
        assert!(m.contains("key map"), "{m}");
    }
}

fn openps_input_friendly(raw: &str) -> String {
    // `input::friendly_config_error` lives in input.rs, which needs the whole CoreGraphics/AppKit input stack to link; replicate its contract
    // through the public pieces instead: it must never include the raw text, whatever the raw text is.
    let line = raw
        .split("line ")
        .nth(1)
        .and_then(|r| r.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|n| n.parse::<u32>().ok());
    match line {
        Some(n) => format!("Your key map (input.toml) has a mistake on line {n}, so the default keys are in use."),
        None => "Your key map (input.toml) could not be read, so the default keys are in use.".into(),
    }
}

/// `input_profile_names` / `input_select_profile` parse `input.toml` from disk on the main thread on every call. A pathologically nested
/// or huge file must not crash the app (stack overflow = abort).
#[test]
fn the_key_map_parser_survives_deeply_nested_and_huge_toml() {
    let deep_array = format!("a = {}{}", "[".repeat(5_000), "]".repeat(5_000));
    let deep_tables = (0..3_000)
        .map(|i| format!("[{}]\n", (0..=i % 50).map(|j| format!("k{j}")).collect::<Vec<_>>().join(".")))
        .collect::<String>();
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024) // the main thread's size
        .spawn(move || {
            for src in [deep_array, deep_tables, "x".repeat(5_000_000), "[profile]\n".repeat(100_000)] {
                let _ = openps_input::load_layers(Some(&src), true);
            }
        })
        .unwrap();
    handle.join().expect("parser panicked");
}

// ----------------------------------------------------------------------------------------- data files created by build_core

#[test]
fn secrets_file_is_owner_only_survives_hostile_keys_and_values_and_reports_damage() {
    use openps_psn::store::{FileStore, SecretStore};
    let t = tempfile::tempdir().unwrap();
    paths::ensure(t.path()).unwrap();
    let p = t.path().join("secrets.json");
    let s = FileStore::open(&p).unwrap();
    for k in ["../../etc/passwd", "", "a\0b", "\u{202e}", &"k".repeat(100_000)] {
        s.set(k, "v").unwrap();
        assert_eq!(s.get(k).as_deref(), Some("v"));
    }
    let big = "x".repeat(20 << 20);
    s.set("big", &big).unwrap();
    assert_eq!(mode(&p), 0o600);
    assert_eq!(mode(t.path()), 0o700);
    assert!(!p.with_extension("tmp").exists());
    let s2 = FileStore::open(&p).unwrap();
    assert_eq!(s2.get("big").map(|v| v.len()), Some(big.len()));
    // a damaged file is an error (so that one bad byte cannot wipe the keys) ...
    std::fs::write(&p, b"{\"a\":").unwrap();
    assert!(FileStore::open(&p).is_err());
    std::fs::write(&p, b"\xff\xff").unwrap();
    assert!(FileStore::open(&p).is_err());
    std::fs::write(&p, b"[]").unwrap();
    assert!(FileStore::open(&p).is_err());
    // ... and an unreadable one too
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o000)).unwrap();
    assert!(FileStore::open(&p).is_err());
}

/// `build_core` does `ConsoleStore::open(state.json)?` and `secret_store(..)?`, and `setup` turns an `Err` into
/// "cannot start": the app exits at launch. This test shows the precondition: a state file that is merely damaged (zero bytes after a full
/// disk, a stray editor save, a sync-tool conflict marker) is an `Err`, not an empty store.
#[test]
fn a_damaged_state_file_makes_the_store_refuse_to_open_and_so_the_app_refuse_to_start() {
    use openps_core::store::ConsoleStore;
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("state.json");
    for content in [&b""[..], b"{", b"null", b"\xff", b"<<<<<<< HEAD\n{}\n=======\n{}\n>>>>>>>"] {
        std::fs::write(&p, content).unwrap();
        assert!(ConsoleStore::open(&p).is_err(), "{content:?} opened");
    }
    std::fs::write(&p, b"{}").unwrap();
    assert!(ConsoleStore::open(&p).is_ok());
}

#[test]
fn state_file_sanitises_hostile_records_and_is_private() {
    use openps_core::store::ConsoleStore;
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("state.json");
    let hostile = serde_json::json!({
        "consoles": [
            {"id": "AA:BB:CC:DD:EE:FF", "device": "ps5", "nickname": "ok\u{202e}evil", "ap_name": "x", "version": "1.0", "host": "192.168.1.5"},
            {"id": "../../x", "device": "ps5", "nickname": "n", "ap_name": "x", "version": "1.0", "host": null},
            {"id": "AABBCCDDEEFF", "device": "ps5", "nickname": "n", "ap_name": "x", "version": "1.0", "host": "evil.com/;rm -rf"},
            {"id": "AABBCCDDEE00", "device": "ps5", "nickname": "n".repeat(100_000), "ap_name": "x", "version": "1.0", "host": "-bad-.example"},
        ]
    });
    std::fs::write(&p, hostile.to_string()).unwrap();
    match ConsoleStore::open(&p) {
        Ok(st) => {
            let cs = st.consoles();
            assert!(cs.iter().all(|c| !c.nickname.contains('\u{202e}') && c.nickname.chars().count() <= 64));
            assert!(cs.iter().all(|c| c.host.as_deref().is_none_or(|h| openps_core::types::validate_host(h).is_ok())));
            st.save().unwrap();
            assert_eq!(mode(&p), 0o600);
            eprintln!("kept {} record(s), dropped {}", cs.len(), st.dropped());
        }
        Err(e) => eprintln!("hostile state file rejected: {e}"),
    }
}

/// BUG T10-04: `lib.rs::setup` pre-creates `logs/openps.log` with mode 0600, but `tauri-plugin-log` (default `RotationStrategy::KeepOne`,
/// 40 000 bytes) rotates by `remove_file` + `OpenOptions::new().create(true).append(true).open()` (tauri-plugin-log 2.10.1 lib.rs:232-262),
/// which creates the replacement with the default mode (0666 & ~umask = 0644). After the first rotation the log (console addresses,
/// session events) is world-readable inside a 0700 folder - the folder still protects it, but the stated 0600 invariant is gone, and the log
/// file is created 0644 whenever the plugin has to re-create it (e.g. the user deletes it while the app runs).
/// This test replays the plugin's exact open sequence against a 0600 file.
#[test]
fn the_rotated_log_file_stays_private() {
    use std::os::unix::fs::OpenOptionsExt;
    let t = tempfile::tempdir().unwrap();
    let log = t.path().join("openps.log");
    // what `setup` does before the log plugin is built: an owner-only process umask
    unsafe {
        set_private_umask();
    }
    drop(std::fs::OpenOptions::new().append(true).create(true).mode(0o600).open(&log).unwrap()); // what setup does
    assert_eq!(mode(&log), 0o600);
    std::fs::remove_file(&log).unwrap(); // RotationStrategy::KeepOne
    drop(std::fs::OpenOptions::new().create(true).append(true).open(&log).unwrap()); // Rotator::open_file
    assert_eq!(mode(&log), 0o600, "the rotated log is mode {:o} (process umask {:o})", mode(&log), unsafe {
        umask_peek()
    });
}

unsafe fn umask_peek() -> u32 {
    extern "C" {
        fn umask(m: u16) -> u16;
    }
    let m = umask(0o077);
    umask(m);
    u32::from(m)
}

unsafe fn set_private_umask() {
    extern "C" {
        fn umask(m: u16) -> u16;
    }
    umask(0o077);
}
