//! Where secrets live. Tokens and registration keys must never be written to a plain file by default.

use std::collections::HashMap;
use std::sync::Mutex;

pub trait SecretStore {
    fn get(&self, key: &str) -> Option<String>;
    fn set(&self, key: &str, value: &str) -> Result<(), String>;
    fn delete(&self, key: &str) -> Result<(), String>;
}

/// In-memory store (tests, and sessions that should not persist).
#[derive(Default)]
pub struct MemoryStore(Mutex<HashMap<String, String>>);

impl SecretStore for MemoryStore {
    fn get(&self, key: &str) -> Option<String> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).get(key).cloned()
    }
    fn set(&self, key: &str, value: &str) -> Result<(), String> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).insert(key.into(), value.into());
        Ok(())
    }
    fn delete(&self, key: &str) -> Result<(), String> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).remove(key);
        Ok(())
    }
}

/// macOS Keychain (generic passwords under a service name).
#[cfg(target_os = "macos")]
pub struct KeychainStore {
    service: String,
}

#[cfg(target_os = "macos")]
impl KeychainStore {
    pub fn new(service: &str) -> Self {
        Self { service: service.into() }
    }
}

#[cfg(target_os = "macos")]
impl SecretStore for KeychainStore {
    fn get(&self, key: &str) -> Option<String> {
        security_framework::passwords::get_generic_password(&self.service, key)
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
    }
    fn set(&self, key: &str, value: &str) -> Result<(), String> {
        security_framework::passwords::set_generic_password(&self.service, key, value.as_bytes())
            .map_err(|e| e.to_string())
    }
    fn delete(&self, key: &str) -> Result<(), String> {
        match security_framework::passwords::delete_generic_password(&self.service, key) {
            Ok(()) => Ok(()),
            Err(e) if e.code() == -25300 => Ok(()), // errSecItemNotFound: already gone
            Err(e) => Err(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_basics() {
        let s = MemoryStore::default();
        assert_eq!(s.get("k"), None);
        s.set("k", "v").unwrap();
        assert_eq!(s.get("k").as_deref(), Some("v"));
        s.set("k", "w").unwrap();
        assert_eq!(s.get("k").as_deref(), Some("w"));
        s.delete("k").unwrap();
        s.delete("k").unwrap(); // deleting a missing key is fine
        assert_eq!(s.get("k"), None);
    }

    /// Touches the real login keychain (may prompt), so it only runs when asked: `cargo test -- --ignored`.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn keychain_roundtrip() {
        let s = KeychainStore::new("com.openps.test");
        s.set("probe", "secret").unwrap();
        assert_eq!(s.get("probe").as_deref(), Some("secret"));
        s.delete("probe").unwrap();
        assert_eq!(s.get("probe"), None);
        s.delete("probe").unwrap();
    }
}

/// Secrets in a JSON file inside the application's own data directory (portable mode: nothing in the OS keychain or
/// any system location). The file is created owner-only (0600) and replaced atomically; a file that exists but cannot
/// be parsed is an error rather than silently treated as empty, so one bad byte cannot wipe stored keys.
pub struct FileStore {
    path: std::path::PathBuf,
    map: Mutex<HashMap<String, String>>,
}

impl FileStore {
    pub fn open(path: impl Into<std::path::PathBuf>) -> Result<Self, String> {
        let path = path.into();
        let map = match std::fs::read_to_string(&path) {
            Ok(s) => serde_json::from_str(&s).map_err(|e| format!("{}: {e}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        Ok(Self { path, map: Mutex::new(map) })
    }

    fn flush(&self, map: &HashMap<String, String>) -> Result<(), String> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let tmp = self.path.with_extension("tmp");
        let json = serde_json::to_vec(map).map_err(|e| e.to_string())?;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|e| e.to_string())?;
        f.write_all(&json).and_then(|_| f.sync_all()).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &self.path).map_err(|e| e.to_string())
    }
}

impl SecretStore for FileStore {
    fn get(&self, key: &str) -> Option<String> {
        self.map.lock().unwrap_or_else(|e| e.into_inner()).get(key).cloned()
    }
    fn set(&self, key: &str, value: &str) -> Result<(), String> {
        let mut m = self.map.lock().unwrap_or_else(|e| e.into_inner());
        let old = m.insert(key.into(), value.into());
        self.flush(&m).inspect_err(|_| {
            // keep memory and disk consistent if the write failed
            match old {
                Some(o) => m.insert(key.into(), o),
                None => m.remove(key),
            };
        })
    }
    fn delete(&self, key: &str) -> Result<(), String> {
        let mut m = self.map.lock().unwrap_or_else(|e| e.into_inner());
        let old = m.remove(key);
        self.flush(&m).inspect_err(|_| {
            if let Some(o) = old {
                m.insert(key.into(), o);
            }
        })
    }
}

#[cfg(test)]
mod file_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn roundtrip_persistence_permissions_and_odd_values() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("secrets.json");
        let s = FileStore::open(&p).unwrap();
        assert_eq!(s.get("a"), None);
        s.set("console/ab:cd", "va\"lue\n\u{1F3AE}").unwrap();
        s.set("b", "2").unwrap();
        s.delete("b").unwrap();
        s.delete("never-existed").unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(!p.with_extension("tmp").exists(), "temp file is renamed away");
        let again = FileStore::open(&p).unwrap();
        assert_eq!(again.get("console/ab:cd").as_deref(), Some("va\"lue\n\u{1F3AE}"));
        assert_eq!(again.get("b"), None);
    }

    #[test]
    fn corrupt_file_is_an_error_not_a_reset() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("secrets.json");
        std::fs::write(&p, b"{\"a\":\"1\"").unwrap();
        assert!(FileStore::open(&p).is_err());
        assert_eq!(std::fs::read(&p).unwrap(), b"{\"a\":\"1\"", "left untouched for recovery");
    }

    #[test]
    fn failed_write_leaves_memory_consistent() {
        let d = tempfile::tempdir().unwrap();
        let s = FileStore::open(d.path().join("sub/secrets.json")).unwrap(); // directory does not exist
        assert!(s.set("k", "v").is_err());
        assert_eq!(s.get("k"), None, "a value that was not persisted is not reported as stored");
    }

    #[test]
    fn concurrent_writers_do_not_lose_entries() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("secrets.json");
        let s = std::sync::Arc::new(FileStore::open(&p).unwrap());
        let hs: Vec<_> = (0..8)
            .map(|t| {
                let s = s.clone();
                std::thread::spawn(move || (0..20).for_each(|i| s.set(&format!("k{t}-{i}"), "v").unwrap()))
            })
            .collect();
        hs.into_iter().for_each(|h| h.join().unwrap());
        assert_eq!(serde_json::from_slice::<HashMap<String, String>>(&std::fs::read(&p).unwrap()).unwrap().len(), 160);
    }
}
