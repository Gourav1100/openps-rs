//! Persistent app state: paired consoles and the PSN account (non-secret), in a JSON file written atomically.
//! Registration keys go to the secret store instead, never into this file.

use crate::types::{canonical_host, normalise_id, ConsoleRecord, ConsoleSecrets, PsnAccountInfo, StreamOptions};
use openps_psn::store::SecretStore;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Default, Clone, Serialize, Deserialize)]
struct Persisted {
    #[serde(default)]
    consoles: Vec<ConsoleRecord>,
    #[serde(default)]
    psn_account: Option<PsnAccountInfo>,
    /// Stream settings chosen by the user; `None` until they change something.
    #[serde(default)]
    settings: Option<StreamOptions>,
}

pub struct ConsoleStore {
    path: PathBuf,
    data: Persisted,
    dropped: usize,
}

#[derive(Debug)]
pub struct StoreError(pub String);
impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for StoreError {}

fn secret_key(id: &str) -> String {
    format!("console/{}", normalise_id(id))
}

impl ConsoleStore {
    /// Load `path`, or start empty if it does not exist. A corrupt file is an error, never silently reset:
    /// the user's pairings must not vanish because of a stray byte.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let path = path.into();
        let (data, dropped) = read_file(&path)?.unwrap_or_default();
        Ok(Self { path, data, dropped })
    }

    /// How many saved consoles were ignored on load because their data was invalid.
    pub fn dropped(&self) -> usize {
        self.dropped
    }

    /// Write the current in-memory state to disk (atomically). The mutators below do this themselves.
    pub fn save(&self) -> Result<(), StoreError> {
        write_file(&self.path, &self.data)
    }

    /// One transaction: take the directory lock (shared with other processes using the same file), re-read the file so changes made elsewhere are kept, apply `change` to a copy, write it, and only
    /// then adopt it. A failure at any step leaves memory as it was.
    fn update<T>(&mut self, change: impl FnOnce(&mut Persisted) -> T) -> Result<T, StoreError> {
        let err = |e: std::io::Error| StoreError(format!("{}: {e}", self.path.display()));
        let dir = self.path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir).map_err(err)?;
        let lock = std::fs::File::open(dir).map_err(err)?;
        lock.lock().map_err(err)?; // released when `lock` is dropped
        let (mut work, dropped) = match read_file(&self.path)? {
            Some(d) => d,
            None => (self.data.clone(), self.dropped), // file not there (yet): our copy is the truth
        };
        let out = change(&mut work);
        write_file(&self.path, &work)?;
        self.data = work;
        self.dropped = dropped;
        Ok(out)
    }

    pub fn consoles(&self) -> &[ConsoleRecord] {
        &self.data.consoles
    }

    pub fn get(&self, id: &str) -> Option<&ConsoleRecord> {
        let id = normalise_id(id);
        self.data.consoles.iter().find(|c| c.id == id)
    }

    /// Insert or replace (by normalised id). Saves immediately.
    pub fn upsert(&mut self, mut rec: ConsoleRecord) -> Result<(), StoreError> {
        rec.id = normalise_id(&rec.id);
        self.update(|d| match d.consoles.iter_mut().find(|c| c.id == rec.id) {
            Some(slot) => *slot = rec,
            None => d.consoles.push(rec),
        })
    }

    pub fn remove(&mut self, id: &str) -> Result<bool, StoreError> {
        let id = normalise_id(id);
        if !self.data.consoles.iter().any(|c| c.id == id) {
            return Ok(false);
        }
        self.update(|d| {
            let before = d.consoles.len();
            d.consoles.retain(|c| c.id != id);
            d.consoles.len() != before
        })
    }

    pub fn set_host(&mut self, id: &str, host: &str) -> Result<(), StoreError> {
        self.set_hosts(&[(id.to_string(), host.to_string())])
    }

    /// Update the addresses of several consoles with one write. Unknown ids and unchanged addresses are ignored.
    pub fn set_hosts(&mut self, updates: &[(String, String)]) -> Result<(), StoreError> {
        let wanted: Vec<(String, String)> = updates.iter().map(|(i, h)| (normalise_id(i), canonical_host(h))).collect();
        let needed =
            wanted.iter().any(|(i, h)| self.data.consoles.iter().any(|c| c.id == *i && c.host.as_deref() != Some(h)));
        if !needed {
            return Ok(());
        }
        self.update(|d| {
            for (id, h) in &wanted {
                if let Some(c) = d.consoles.iter_mut().find(|c| c.id == *id) {
                    c.host = Some(h.clone());
                }
            }
        })
    }

    /// Save the record first, then the keys; if the keys cannot be stored the record change is undone, so a failure never
    /// leaves a record without keys or replaces working keys with ones that belong to a record that was not saved.
    pub fn upsert_with_secrets(
        &mut self,
        rec: ConsoleRecord,
        secrets: &dyn SecretStore,
        s: &ConsoleSecrets,
    ) -> Result<(), StoreError> {
        let old = self.get(&rec.id).cloned();
        let id = normalise_id(&rec.id);
        self.upsert(rec)?;
        if let Err(e) = Self::put_secrets(secrets, &id, s) {
            let _ = match old {
                Some(o) => self.upsert(o),
                None => self.remove(&id).map(|_| ()),
            };
            return Err(e);
        }
        Ok(())
    }

    pub fn psn_account(&self) -> Option<&PsnAccountInfo> {
        self.data.psn_account.as_ref()
    }

    pub fn set_psn_account(&mut self, a: Option<PsnAccountInfo>) -> Result<(), StoreError> {
        self.update(|d| d.psn_account = a)
    }

    pub fn settings(&self) -> Option<&StreamOptions> {
        self.data.settings.as_ref()
    }

    pub fn set_settings(&mut self, o: StreamOptions) -> Result<(), StoreError> {
        self.update(|d| d.settings = Some(o))
    }

    // ---- secrets ----------------------------------------------------------------------------------------------------
    pub fn put_secrets(secrets: &dyn SecretStore, id: &str, s: &ConsoleSecrets) -> Result<(), StoreError> {
        secrets.set(&secret_key(id), &serde_json::to_string(s).expect("serialisable")).map_err(StoreError)
    }

    pub fn get_secrets(secrets: &dyn SecretStore, id: &str) -> Option<ConsoleSecrets> {
        secrets.get(&secret_key(id)).and_then(|j| serde_json::from_str(&j).ok())
    }

    pub fn delete_secrets(secrets: &dyn SecretStore, id: &str) -> Result<(), StoreError> {
        secrets.delete(&secret_key(id)).map_err(StoreError)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Read and sanitise the state file. `None` if it does not exist. A file that is not a JSON object is an error.
fn read_file(path: &Path) -> Result<Option<(Persisted, usize)>, StoreError> {
    let err = |e: String| StoreError(format!("{}: {e}", path.display()));
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(err(e.to_string())),
    };
    let value: serde_json::Value = serde_json::from_str(&text).map_err(|e| err(e.to_string()))?;
    if !value.is_object() {
        return Err(err("the state file is not a JSON object".into()));
    }
    let mut data: Persisted = serde_json::from_value(value).map_err(|e| err(e.to_string()))?;
    let dropped = sanitise(&mut data);
    Ok(Some((data, dropped)))
}

/// A hand-edited or damaged file must not produce unusable entries: records with an invalid address or id are dropped
/// (returned count, see [`ConsoleStore::dropped`]); ids and addresses are put in canonical form, duplicates collapse (the
/// last one wins), a protocol version that is not one we speak falls back to the device default, names are cleaned and
/// length-capped, and saved settings that would not pass validation are discarded.
fn sanitise(data: &mut Persisted) -> usize {
    let before = data.consoles.len();
    data.consoles.retain(|c| {
        let id = normalise_id(&c.id);
        id.len() == 12
            && id.bytes().all(|b| b.is_ascii_hexdigit())
            && c.host.as_deref().is_none_or(|h| crate::types::validate_host(h).is_ok())
    });
    for c in &mut data.consoles {
        c.id = normalise_id(&c.id);
        c.host = c.host.as_deref().map(canonical_host);
        if crate::types::validate_version(&c.version).is_err() {
            c.version = c.device.default_version().to_string();
        }
        c.nickname = crate::types::clean_text(&c.nickname, crate::types::MAX_NAME_CHARS);
        c.ap_name = crate::types::clean_text(&c.ap_name, crate::types::MAX_NAME_CHARS);
    }
    let mut seen = std::collections::HashSet::new();
    let mut kept: Vec<ConsoleRecord> = Vec::with_capacity(data.consoles.len());
    for c in std::mem::take(&mut data.consoles).into_iter().rev() {
        if seen.insert(c.id.clone()) {
            kept.push(c);
        }
    }
    kept.reverse();
    data.consoles = kept;
    if data.settings.as_ref().is_some_and(|s| s.validate().is_err()) {
        data.settings = None;
    }
    before - data.consoles.len()
}

fn write_file(path: &Path, data: &Persisted) -> Result<(), StoreError> {
    let text = serde_json::to_string_pretty(data).map_err(|e| StoreError(e.to_string()))?;
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| StoreError(e.to_string()))?;
    }
    let tmp = path.with_extension("json.tmp");
    write_private(&tmp, text.as_bytes()).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        StoreError(e.to_string())
    })?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        StoreError(e.to_string())
    })
}

/// Write a file readable only by its owner. It holds console addresses and the PSN account, which are private even
/// though they are not secrets in the cryptographic sense. A stale file of the same name is removed first, so its old
/// (possibly loose) permissions cannot carry over.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Device;
    use openps_psn::store::MemoryStore;

    fn rec(id: &str) -> ConsoleRecord {
        ConsoleRecord {
            id: id.into(),
            device: Device::Ps5,
            nickname: "Den".into(),
            ap_name: "x".into(),
            version: "1.0".into(),
            host: None,
        }
    }

    #[test]
    fn the_state_file_is_owner_only_and_a_pre_existing_loose_file_is_tightened() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("state.json");
        let mut st = ConsoleStore::open(&p).unwrap();
        st.set_psn_account(Some(PsnAccountInfo { online_id: "me".into(), account_id: "1".into() })).unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        // a file left world-readable by an older version is replaced by an owner-only one on the next save
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        st.set_psn_account(None).unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(!p.with_extension("json.tmp").exists());
    }

    #[test]
    fn upsert_get_remove_and_reload() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("sub/state.json");
        let mut s = ConsoleStore::open(&p).unwrap();
        assert!(s.consoles().is_empty());
        s.upsert(rec("aa:bb:cc:dd:ee:ff")).unwrap();
        assert_eq!(s.get("AABBCCDDEEFF").unwrap().id, "AABBCCDDEEFF"); // normalised, found by any spelling
        s.upsert(ConsoleRecord { nickname: "Renamed".into(), ..rec("AABBCCDDEEFF") }).unwrap(); // replaces, no duplicate
        assert_eq!((s.consoles().len(), s.get("aabbccddeeff").unwrap().nickname.as_str()), (1, "Renamed"));
        s.set_host("AABBCCDDEEFF", "10.0.0.7").unwrap();
        s.set_psn_account(Some(PsnAccountInfo { online_id: "P".into(), account_id: "18446744073709551615".into() }))
            .unwrap();
        let again = ConsoleStore::open(&p).unwrap();
        assert_eq!(again.get("AABBCCDDEEFF").unwrap().host.as_deref(), Some("10.0.0.7"));
        assert_eq!(again.psn_account().unwrap().account_id, "18446744073709551615");
        let mut s = again;
        assert!(s.remove("AA-BB-CC-DD-EE-FF").unwrap());
        assert!(!s.remove("AABBCCDDEEFF").unwrap());
        assert!(ConsoleStore::open(&p).unwrap().consoles().is_empty());
    }

    #[test]
    fn a_corrupt_file_is_an_error_not_a_silent_reset() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("state.json");
        std::fs::write(&p, "{ not json").unwrap();
        assert!(ConsoleStore::open(&p).is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{ not json"); // untouched
    }

    #[test]
    fn secrets_are_kept_out_of_the_json_file() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("state.json");
        let mem = MemoryStore::default();
        let mut s = ConsoleStore::open(&p).unwrap();
        s.upsert(rec("AABBCCDDEEFF")).unwrap();
        ConsoleStore::put_secrets(
            &mem,
            "aa:bb:cc:dd:ee:ff",
            &ConsoleSecrets { regist_key: "TOP-SECRET-REG".into(), rp_key: "TOP-SECRET-RP".into() },
        )
        .unwrap();
        let file = std::fs::read_to_string(&p).unwrap();
        assert!(!file.contains("SECRET") && !file.to_lowercase().contains("regist_key"));
        let got = ConsoleStore::get_secrets(&mem, "AABBCCDDEEFF").unwrap();
        assert_eq!((got.regist_key.as_str(), got.rp_key.as_str()), ("TOP-SECRET-REG", "TOP-SECRET-RP"));
        ConsoleStore::delete_secrets(&mem, "AABBCCDDEEFF").unwrap();
        assert!(ConsoleStore::get_secrets(&mem, "AABBCCDDEEFF").is_none());
    }

    #[test]
    fn no_leftover_temp_file_after_saving() {
        let d = tempfile::tempdir().unwrap();
        let mut s = ConsoleStore::open(d.path().join("state.json")).unwrap();
        s.upsert(rec("A")).unwrap();
        let names: Vec<String> = std::fs::read_dir(d.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["state.json"]);
    }

    #[test]
    fn damaged_records_are_dropped_and_names_capped_on_load() {
        let dir = std::env::temp_dir().join(format!("openps-store-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("state.json");
        let long = "A".repeat(200);
        std::fs::write(
            &p,
            format!(
                r#"{{"consoles":[
                {{"id":"AABBCCDDEE01","device":"ps5","nickname":"ok","ap_name":"x","version":"1.0","host":"192.168.1.20"}},
                {{"id":"AABBCCDDEE02","device":"ps4","nickname":"{long}","ap_name":"x","version":"1.0","host":"10.0.0.2"}},
                {{"id":"AABBCCDDEE03","device":"ps4","nickname":"bad host","ap_name":"x","version":"1.0","host":"not a host\nwith newline"}},
                {{"id":"short","device":"ps5","nickname":"bad id","ap_name":"x","version":"1.0","host":null}}
            ]}}"#
            ),
        )
        .unwrap();
        let s = ConsoleStore::open(&p).unwrap();
        assert_eq!(s.dropped(), 2);
        assert_eq!(s.consoles().len(), 2);
        assert!(s.consoles().iter().all(|c| c.nickname.chars().count() <= crate::types::MAX_NAME_CHARS));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
