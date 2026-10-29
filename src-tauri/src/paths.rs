//! Where the application keeps its data. Portable mode: everything lives inside the application itself, so deleting
//! the application deletes all traces and nothing is written to system locations (no `~/Library`, no Keychain).

use std::path::{Path, PathBuf};

pub const DIR_NAME: &str = "openps-data";

/// `$OPENPS_HOME` if set and non-empty, else `openps-data` next to the executable (inside the `.app` bundle when
/// bundled: `Contents/MacOS/openps-data`).
pub fn data_dir(env_override: Option<&str>, exe: &Path) -> PathBuf {
    match env_override {
        Some(p) if !p.trim().is_empty() => PathBuf::from(p),
        _ => exe.parent().unwrap_or_else(|| Path::new(".")).join(DIR_NAME),
    }
}

/// Create the directory owner-only; it holds secrets.
pub fn ensure(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    // an existing directory keeps its mode from creation time; tighten it (best effort on directories we do not own)
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resolution_and_creation() {
        let exe = Path::new("/apps/openPS.app/Contents/MacOS/openps");
        assert_eq!(data_dir(None, exe), Path::new("/apps/openPS.app/Contents/MacOS/openps-data"));
        assert_eq!(data_dir(Some(""), exe), data_dir(None, exe));
        assert_eq!(data_dir(Some("  "), exe), data_dir(None, exe));
        assert_eq!(data_dir(Some("/x/y"), exe), Path::new("/x/y"));
        assert_eq!(data_dir(None, Path::new("openps")), Path::new("openps-data")); // bare exe name

        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("a/b/data");
        ensure(&d).unwrap();
        ensure(&d).unwrap(); // idempotent
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&d).unwrap().permissions().mode() & 0o777, 0o700);
    }
}
