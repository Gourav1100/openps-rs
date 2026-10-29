//! Which key-map profile each game uses. Stored as `game-keymaps.json` in the data folder (owner-only), keyed by the game's title id.
//! The profile is chosen on the game's tile and applied automatically when the game is started from the app.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tauri::State;

const MAX_ENTRIES: usize = 2000;

fn file(data: &Path) -> PathBuf {
    data.join("game-keymaps.json")
}

fn valid_key(k: &str) -> bool {
    !k.is_empty() && k.len() <= 32 && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn valid_profile(p: &str) -> bool {
    !p.trim().is_empty() && p.chars().count() <= 64 && !p.chars().any(char::is_control)
}

pub fn load(data: &Path) -> BTreeMap<String, String> {
    let Ok(bytes) = std::fs::read(file(data)) else { return BTreeMap::new() };
    let map: BTreeMap<String, String> = serde_json::from_slice(&bytes).unwrap_or_default(); // a damaged file means "no choices yet"
    map.into_iter().filter(|(k, v)| valid_key(k) && valid_profile(v)).take(MAX_ENTRIES).collect()
}

/// Set (`Some`) or clear (`None`) a game's profile and save. Returns the new map.
pub fn set(data: &Path, key: &str, profile: Option<&str>) -> Result<BTreeMap<String, String>, String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if !valid_key(key) {
        return Err("not a game".into());
    }
    // load -> modify -> write must not interleave with another caller (lost updates, shared temporary file)
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let _guard = LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut map = load(data);
    match profile {
        Some(p) if valid_profile(p) => {
            if map.len() >= MAX_ENTRIES && !map.contains_key(key) {
                return Err("too many saved choices".into());
            }
            map.insert(key.to_string(), p.to_string());
        }
        Some(_) => return Err("not a key map name".into()),
        None => {
            map.remove(key);
        }
    }
    let path = file(data);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_extension(format!("{}.{n}.tmp", std::process::id()));
    let json = serde_json::to_vec_pretty(&map).map_err(|e| e.to_string())?;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|e| e.to_string())?;
    let written = f.write_all(&json).and_then(|()| f.sync_all()).and_then(|()| std::fs::rename(&tmp, &path));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.to_string());
    }
    Ok(map)
}

#[tauri::command]
pub fn game_maps(dir: State<'_, crate::DataDir>) -> BTreeMap<String, String> {
    load(&dir.0)
}

#[tauri::command]
pub fn set_game_map(
    dir: State<'_, crate::DataDir>,
    key: String,
    profile: Option<String>,
) -> Result<BTreeMap<String, String>, String> {
    set(&dir.0, &key, profile.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choices_are_saved_changed_and_cleared() {
        let d = tempfile::tempdir().unwrap();
        assert!(load(d.path()).is_empty());
        let m = set(d.path(), "PPSA01325", Some("racing")).unwrap();
        assert_eq!(m.get("PPSA01325").map(String::as_str), Some("racing"));
        set(d.path(), "PPSA01325", Some("default")).unwrap();
        assert_eq!(load(d.path())["PPSA01325"], "default");
        assert!(set(d.path(), "PPSA01325", None).unwrap().is_empty());
    }

    #[test]
    fn the_file_is_private_and_a_damaged_one_is_ignored() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        set(d.path(), "CUSA1", Some("a")).unwrap();
        assert_eq!(std::fs::metadata(file(d.path())).unwrap().permissions().mode() & 0o777, 0o600);
        std::fs::write(file(d.path()), b"{ nope").unwrap();
        assert!(load(d.path()).is_empty());
    }

    #[test]
    fn bad_keys_and_names_are_refused_and_bad_entries_dropped_on_load() {
        let d = tempfile::tempdir().unwrap();
        for k in ["", "../x", "a b", &"A".repeat(40)] {
            assert!(set(d.path(), k, Some("p")).is_err(), "{k}");
        }
        assert!(set(d.path(), "OK1", Some("")).is_err());
        assert!(set(d.path(), "OK1", Some("a\nb")).is_err());
        assert!(set(d.path(), "OK1", Some(&"p".repeat(100))).is_err());
        std::fs::write(file(d.path()), br#"{"GOOD":"p","../evil":"p","BAD":"x\u0000y"}"#).unwrap();
        assert_eq!(load(d.path()).keys().collect::<Vec<_>>(), ["GOOD"]);
    }
}
