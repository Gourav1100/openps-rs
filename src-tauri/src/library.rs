//! The games shelf: the account's games and trophies, and their cover images, cached on disk so the shelf looks the same on every
//! start and works without a network. Layout inside the data folder: `cache/library.json` and `cache/art-v2/<key>.<ext>`.

use openps_core::Core;
use openps_psn::library::{art_url_allowed, Library};
use serde::Serialize;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use tauri::State;

fn cache_dir(data: &Path) -> PathBuf {
    data.join("cache")
}

/// A stable name for a URL (two 64-bit FNV-1a hashes with different seeds, 128 bits).
pub fn cache_key(url: &str) -> String {
    let fnv = |seed: u64| url.bytes().fold(seed, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3));
    format!("{:016x}{:016x}", fnv(0xcbf2_9ce4_8422_2325), fnv(0x8422_2325_cbf2_9ce4))
}

pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n =
            (u32::from(c[0]) << 16) | (u32::from(*c.get(1).unwrap_or(&0)) << 8) | u32::from(*c.get(2).unwrap_or(&0));
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

fn ext_for(mime: &str) -> Option<&'static str> {
    Some(match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        _ => return None,
    })
}

fn mime_for(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "jpg" => "image/jpeg",
        "webp" => "image/webp",
        _ => "image/svg+xml",
    }
}

/// Write through a temporary file so a crash never leaves a half-written cache entry; owner-only, like the rest of the data folder.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    if let Some(dir) = path.parent() {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    }
    // a name no other writer uses: two callers for the same entry must not share one temporary file
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_extension(format!("{}.{n}.tmp", std::process::id()));
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
    let done = f.write_all(bytes).and_then(|()| f.sync_all()).and_then(|()| std::fs::rename(&tmp, path));
    if done.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    done
}

/// Largest cached cover we will read back (the downloader caps what it stores far below this).
const MAX_ART_BYTES: u64 = 8 * 1024 * 1024;

fn read_capped(path: &Path) -> Option<Vec<u8>> {
    let f = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    std::io::Read::take(f, MAX_ART_BYTES + 1).read_to_end(&mut buf).ok()?;
    (buf.len() as u64 <= MAX_ART_BYTES).then_some(buf)
}

/// Ids that go to Sony: ASCII letters, digits, `-`, `_`, `.`, bounded.
pub fn plain_id(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
}

#[derive(Serialize)]
pub struct LibraryReply {
    pub library: Option<Library>,
    /// Unix seconds when the data was fetched (0 = unknown).
    pub fetched_at: u64,
    pub from_cache: bool,
    pub error: Option<String>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Stored {
    fetched_at: u64,
    library: Library,
}

fn load_cached(data: &Path) -> Option<Stored> {
    let bytes = std::fs::read(cache_dir(data).join("library.json")).ok()?;
    serde_json::from_slice(&bytes).ok() // a damaged cache is simply ignored and replaced on the next fetch
}

fn now_s() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Cached copy first (instant, identical on every start); `refresh` asks Sony again and replaces the cache. A failed refresh keeps
/// showing the cached copy and says why.
#[tauri::command]
pub async fn psn_library(
    core: State<'_, Core>,
    dir: State<'_, crate::DataDir>,
    refresh: bool,
) -> Result<LibraryReply, String> {
    let (core, data) = (core.inner().clone(), dir.0.clone());
    tauri::async_runtime::spawn_blocking(move || {
        let cached = load_cached(&data);
        if !refresh {
            if let Some(c) = cached {
                return LibraryReply {
                    library: Some(c.library),
                    fetched_at: c.fetched_at,
                    from_cache: true,
                    error: None,
                };
            }
        }
        match core.psn_library() {
            Ok(lib) => {
                let stored = Stored { fetched_at: now_s(), library: lib.clone() };
                if let Ok(json) = serde_json::to_vec(&stored) {
                    if let Err(e) = write_private(&cache_dir(&data).join("library.json"), &json) {
                        log::warn!("games cache: {e}");
                    }
                }
                LibraryReply { library: Some(lib), fetched_at: stored.fetched_at, from_cache: false, error: None }
            }
            Err(e) => {
                log::warn!("games: {e}");
                let (library, fetched_at) = cached.map_or((None, 0), |c| (Some(c.library), c.fetched_at));
                LibraryReply { library, fetched_at, from_cache: true, error: Some(e.to_string()) }
            }
        }
    })
    .await
    .map_err(|e| e.to_string())
}

#[derive(Serialize)]
pub struct LaunchReply {
    pub command_id: String,
}

/// Ask a console to start a title. `id` is the paired console: its PSN identity (duid) is looked up from the account's list.
#[tauri::command]
pub async fn launch_game(core: State<'_, Core>, id: String, title_id: String) -> Result<LaunchReply, String> {
    if !plain_id(&id, 128) {
        return Err("not a console".into());
    }
    let title_id = title_id.trim().to_uppercase();
    if !plain_id(&title_id, 32) {
        return Err("not a game".into());
    }
    let core = core.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let duid = psn_duid_for(&core, &id)?;
        core.psn_launch(&duid, &title_id).map(|command_id| LaunchReply { command_id }).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn launch_status(
    core: State<'_, Core>,
    command_id: String,
) -> Result<openps_psn::library::CommandStatus, String> {
    if !plain_id(&command_id, 128) {
        return Err("not a launch request".into());
    }
    let core = core.inner().clone();
    tauri::async_runtime::spawn_blocking(move || core.psn_command_status(&command_id).map_err(|e| e.to_string()))
        .await
        .map_err(|e| e.to_string())?
}

/// Which of the account's consoles a paired console is: by name, or the only one on the account.
pub fn pick_duid(paired_name: &str, psn: &[(String, String)]) -> Option<String> {
    let n = paired_name.trim().to_lowercase();
    if let Some((d, _)) = psn.iter().find(|(_, name)| !n.is_empty() && name.trim().to_lowercase() == n) {
        return Some(d.clone());
    }
    (psn.len() == 1).then(|| psn[0].0.clone())
}

fn psn_duid_for(core: &Core, id: &str) -> Result<String, String> {
    let rec = core.consoles().into_iter().find(|c| c.id == id).ok_or("unknown console")?;
    let list: Vec<(String, String)> =
        core.psn_consoles().map_err(|e| e.to_string())?.into_iter().map(|c| (c.duid, c.name)).collect();
    pick_duid(&rec.nickname, &list)
        .ok_or_else(|| "could not tell which PlayStation Network console this is".to_string())
}

/// A cover image as a `data:` URL. The first call downloads it; every later call (and every later start) reads the saved copy, so the
/// picture never changes under the user.
#[tauri::command]
pub async fn game_art(core: State<'_, Core>, dir: State<'_, crate::DataDir>, url: String) -> Result<String, String> {
    if url.len() > 2048 || (!art_url_allowed(&url) && !url.starts_with("https://image.api.playstation.com/demo/")) {
        return Err("that image host is not allowed".into());
    }
    let (core, data) = (core.inner().clone(), dir.0.clone());
    tauri::async_runtime::spawn_blocking(move || {
        // "v2": the first version stored square-cropped thumbnails; those entries are simply never read again
        let key = cache_key(&format!("v2|{url}"));
        let art = cache_dir(&data).join("art-v2");
        static LEGACY: std::sync::Once = std::sync::Once::new();
        LEGACY.call_once(|| {
            let _ = std::fs::remove_dir_all(cache_dir(&data).join("art")); // the square-cropped first version
        });
        for ext in ["png", "jpg", "webp", "svg"] {
            if let Some(bytes) = read_capped(&art.join(format!("{key}.{ext}"))) {
                return Ok(format!("data:{};base64,{}", mime_for(ext), base64(&bytes)));
            }
        }
        let (bytes, mime) = core.psn_art(&url).map_err(|e| e.to_string())?;
        let ext = ext_for(&mime).ok_or("unsupported image type")?;
        if let Err(e) = write_private(&art.join(format!("{key}.{ext}")), &bytes) {
            log::warn!("art cache: {e}");
        }
        Ok(format!("data:{mime};base64,{}", base64(&bytes)))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_paired_console_is_matched_to_the_account_by_name_or_as_the_only_one() {
        let l = vec![("d1".to_string(), "PS5-591".to_string()), ("d2".to_string(), "Living room".to_string())];
        assert_eq!(pick_duid("ps5-591", &l).as_deref(), Some("d1"));
        assert_eq!(pick_duid("unknown", &l), None);
        assert_eq!(pick_duid("anything", &l[..1]).as_deref(), Some("d1"));
        assert_eq!(pick_duid("", &[]), None);
    }

    #[test]
    fn base64_matches_the_standard_vectors() {
        for (i, o) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(i.as_bytes()), o);
        }
    }

    #[test]
    fn cache_keys_are_stable_distinct_and_filename_safe() {
        let a = cache_key("https://image.api.playstation.com/a.png");
        assert_eq!(a, cache_key("https://image.api.playstation.com/a.png"));
        assert_ne!(a, cache_key("https://image.api.playstation.com/b.png"));
        assert!(a.len() == 32 && a.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn written_files_are_private_and_replace_atomically() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("cache/art/x.png");
        write_private(&p, b"one").unwrap();
        write_private(&p, b"two").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"two");
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(p.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
        assert!(!p.with_extension("tmp").exists());
    }

    #[test]
    fn a_damaged_library_cache_is_ignored() {
        let d = tempfile::tempdir().unwrap();
        write_private(&cache_dir(d.path()).join("library.json"), b"{ not json").unwrap();
        assert!(load_cached(d.path()).is_none());
    }
}
