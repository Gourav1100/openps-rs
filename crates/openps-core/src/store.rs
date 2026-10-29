//! Persistent app state: paired consoles and the PSN account (non-secret), in a JSON file written atomically.
//! Registration keys go to the secret store instead, never into this file.

use crate::types::{normalise_id, ConsoleRecord, ConsoleSecrets, PsnAccountInfo, StreamOptions};
use openps_psn::store::SecretStore;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Default, Serialize, Deserialize)]
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
        let data = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| StoreError(format!("{}: {e}", path.display())))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Persisted::default(),
            Err(e) => return Err(StoreError(format!("{}: {e}", path.display()))),
        };
        Ok(Self { path, data })
    }

    pub fn save(&self) -> Result<(), StoreError> {
        let text = serde_json::to_string_pretty(&self.data).map_err(|e| StoreError(e.to_string()))?;
        if let Some(dir) = self.path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| StoreError(e.to_string()))?;
        }
        let tmp = self.path.with_extension("json.tmp");
        write_private(&tmp, text.as_bytes()).map_err(|e| StoreError(e.to_string()))?;
        std::fs::rename(&tmp, &self.path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            StoreError(e.to_string())
        })
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
        match self.data.consoles.iter_mut().find(|c| c.id == rec.id) {
            Some(slot) => *slot = rec,
            None => self.data.consoles.push(rec),
        }
        self.save()
    }

    pub fn remove(&mut self, id: &str) -> Result<bool, StoreError> {
        let id = normalise_id(id);
        let before = self.data.consoles.len();
        self.data.consoles.retain(|c| c.id != id);
        let removed = self.data.consoles.len() != before;
        if removed {
            self.save()?;
        }
        Ok(removed)
    }

    pub fn set_host(&mut self, id: &str, host: &str) -> Result<(), StoreError> {
        let id = normalise_id(id);
        if let Some(c) = self.data.consoles.iter_mut().find(|c| c.id == id) {
            if c.host.as_deref() != Some(host) {
                c.host = Some(host.to_string());
                return self.save();
            }
        }
        Ok(())
    }

    pub fn psn_account(&self) -> Option<&PsnAccountInfo> {
        self.data.psn_account.as_ref()
    }

    pub fn set_psn_account(&mut self, a: Option<PsnAccountInfo>) -> Result<(), StoreError> {
        self.data.psn_account = a;
        self.save()
    }

    pub fn settings(&self) -> Option<&StreamOptions> {
        self.data.settings.as_ref()
    }

    pub fn set_settings(&mut self, o: StreamOptions) -> Result<(), StoreError> {
        let old = self.data.settings.replace(o);
        self.save().inspect_err(|_| self.data.settings = old)
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

/// Write a file readable only by its owner. It holds console addresses and the PSN account, which are private even
/// though they are not secrets in the cryptographic sense.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
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
}
