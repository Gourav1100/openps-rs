//! OAuth client configuration. The client id and secret are those of Sony's own Remote Play apps, not this project's. They are compiled in
//! (`builtin_psn.json`), as other open Remote Play clients do. A JSON file can override them (`$OPENPS_PSN`, or `psn.json` in the data
//! folder); `extract-psn-config` can produce one from decompiled source.

use serde::{Deserialize, Serialize};

/// The mobile-app client used for the PSN API (console list). Sony's constants again: external, never committed.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiConfig {
    pub client_id: String,
    /// The complete `Authorization` header value (`Basic ...`).
    pub basic_auth: String,
    pub redirect_uri: String,
    pub scope: String,
    /// `https://.../api/authz/v3/oauth`: `/authorize` and `/token` hang off it.
    pub auth_base: String,
}

impl std::fmt::Debug for ApiConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiConfig")
            .field("client_id", &self.client_id)
            .field("basic_auth", &"<redacted>")
            .field("redirect_uri", &self.redirect_uri)
            .field("scope", &self.scope)
            .field("auth_base", &self.auth_base)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PsnConfig {
    pub client_id: String,
    pub client_secret: String,
    /// `.../2.0/oauth/token`; the introspection endpoint is `<token_url>/<access_token>`.
    pub token_url: String,
    pub redirect_uri: String,
    /// Space separated.
    pub scope: String,
    /// Needed only for the PSN API (listing your consoles for remote registration); absent in older files.
    #[serde(default)]
    pub api: Option<ApiConfig>,
}

// Secrets never appear in logs or panic messages.
impl std::fmt::Debug for PsnConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PsnConfig")
            .field("client_id", &self.client_id)
            .field("client_secret", &"<redacted>")
            .field("token_url", &self.token_url)
            .field("redirect_uri", &self.redirect_uri)
            .field("scope", &self.scope)
            .field("api", &self.api)
            .finish()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ConfigError {
    Missing(&'static str),
    Invalid(&'static str),
    Json(String),
    Io(String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Missing(n) => write!(f, "PSN config: {n} is missing"),
            ConfigError::Invalid(n) => write!(f, "PSN config: {n} is invalid"),
            ConfigError::Json(e) => write!(f, "PSN config: invalid JSON: {e}"),
            ConfigError::Io(e) => write!(f, "PSN config: {e}"),
        }
    }
}
impl std::error::Error for ConfigError {}

/// An `https` URL that really names a host.
fn https_with_host(s: &str) -> bool {
    url::Url::parse(s).is_ok_and(|u| u.scheme() == "https" && u.host_str().is_some_and(|h| !h.is_empty()))
}

impl PsnConfig {
    pub fn validate(&self) -> Result<(), ConfigError> {
        if let Some(a) = &self.api {
            for (name, v) in
                [("api.client_id", &a.client_id), ("api.basic_auth", &a.basic_auth), ("api.scope", &a.scope)]
            {
                if v.trim().is_empty() {
                    return Err(ConfigError::Missing(name));
                }
            }
            if !https_with_host(&a.auth_base)
                || a.auth_base.ends_with('/')
                || !a.redirect_uri.contains("://")
                || url::Url::parse(&a.redirect_uri).is_err()
                || !a.basic_auth.starts_with("Basic ")
            {
                return Err(ConfigError::Invalid("api"));
            }
        }
        for (name, v) in [
            ("client_id", &self.client_id),
            ("client_secret", &self.client_secret),
            ("token_url", &self.token_url),
            ("redirect_uri", &self.redirect_uri),
            ("scope", &self.scope),
        ] {
            if v.trim().is_empty() {
                return Err(ConfigError::Missing(name));
            }
        }
        if !https_with_host(&self.token_url) {
            return Err(ConfigError::Invalid("token_url (must be https)")); // never send credentials in clear text
        }
        if self.token_url.ends_with('/') {
            return Err(ConfigError::Invalid("token_url (no trailing slash)"));
        }
        Ok(())
    }

    pub fn from_json(text: &str) -> Result<Self, ConfigError> {
        let c: PsnConfig = serde_json::from_str(text).map_err(|e| ConfigError::Json(e.to_string()))?;
        c.validate()?;
        Ok(c)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("serialisable")
    }

    pub fn load(path: &std::path::Path) -> Result<Self, ConfigError> {
        Self::from_json(
            &std::fs::read_to_string(path).map_err(|e| ConfigError::Io(format!("{}: {e}", path.display())))?,
        )
    }

    pub fn from_env() -> Option<Result<Self, ConfigError>> {
        std::env::var_os("OPENPS_PSN").map(|p| Self::load(std::path::Path::new(&p)))
    }

    /// The credentials compiled into the program.
    pub fn builtin() -> Self {
        Self::from_json(include_str!("builtin_psn.json")).expect("the built-in PSN configuration is valid")
    }

    /// An override file if there is one: `$OPENPS_PSN` (a wrong path is an error, never silently ignored), else `psn.json` in `data_dir`.
    /// `None` means no override: use [`PsnConfig::builtin`].
    pub fn locate(data_dir: &std::path::Path) -> Option<Result<Self, ConfigError>> {
        if let Some(r) = Self::from_env() {
            return Some(r);
        }
        let p = data_dir.join("psn.json");
        p.is_file().then(|| Self::load(&p))
    }

    /// `.../oauth/token` -> `.../oauth/authorize`.
    pub fn authorize_url_base(&self) -> String {
        match self.token_url.strip_suffix("/token") {
            Some(base) => format!("{base}/authorize"),
            None => self.token_url.clone(),
        }
    }

    /// Read the constants out of decompiled source of the original application's `PsnConstants` class.
    pub fn from_decompiled_constants(src: &str) -> Result<Self, ConfigError> {
        let get = |name: &'static str| -> Result<String, ConfigError> {
            let key = format!("String {name} = \"");
            let i = src.find(&key).ok_or(ConfigError::Missing(name))? + key.len();
            let end = src[i..].find('"').ok_or(ConfigError::Invalid(name))?;
            Ok(src[i..i + end].to_string())
        };
        // the mobile-app client of the PSN API: optional, only if all five constants are present
        let api = (|| {
            Some(ApiConfig {
                client_id: get("PSN_API_ACCESS_CODE_CLIENT_ID").ok()?,
                basic_auth: get("PSN_API_ACCESS_CODE_TOKEN_EXCHANGE_BASIC_AUTH").ok()?,
                redirect_uri: get("PSN_API_ACCESS_CODE_REDIRECT_URI").ok()?,
                scope: get("PSN_API_ACCESS_CODE_SCOPE").ok()?,
                auth_base: get("PSN_API_AUTH_BASE_URL").ok()?,
            })
        })();
        let c = PsnConfig {
            api,
            client_id: get("CLIENT_ID")?,
            client_secret: get("CLIENT_SECRET")?,
            token_url: get("TOKEN_URL")?,
            redirect_uri: get("REDIRECT_URI")?,
            scope: get("TOKEN_SCOPE")?,
        };
        c.validate()?;
        Ok(c)
    }
}

#[cfg(test)]
pub(crate) fn test_config() -> PsnConfig {
    PsnConfig {
        client_id: "test-client".into(),
        client_secret: "test-secret".into(),
        token_url: "https://auth.example.test/2.0/oauth/token".into(),
        redirect_uri: "https://example.test/redirect".into(),
        scope: "psn:clientapp psn:other".into(),
        api: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_prints_the_secret() {
        let c = test_config();
        let shown = format!("{c:?}");
        assert!(!shown.contains("test-secret") && shown.contains("<redacted>") && shown.contains("test-client"));
    }

    #[test]
    fn validation() {
        assert!(test_config().validate().is_ok());
        let mut c = test_config();
        c.client_secret = " ".into();
        assert_eq!(c.validate(), Err(ConfigError::Missing("client_secret")));
        let mut c = test_config();
        c.token_url = "http://insecure.test/token".into();
        assert!(matches!(c.validate(), Err(ConfigError::Invalid(_))));
        let mut c = test_config();
        c.token_url.push('/');
        assert!(matches!(c.validate(), Err(ConfigError::Invalid(_))));
        assert!(matches!(PsnConfig::from_json("{"), Err(ConfigError::Json(_))));
        assert_eq!(PsnConfig::from_json(&test_config().to_json()).unwrap(), test_config());
    }

    #[test]
    fn authorize_base_and_extraction_from_decompiled_source() {
        assert_eq!(test_config().authorize_url_base(), "https://auth.example.test/2.0/oauth/authorize");
        let src = r#"public class PsnConstants {
   public static final String CLIENT_ID = "id-1";
   public static final String CLIENT_SECRET = "sec-1";
   public static final String TOKEN_SCOPE = "a b c";
   public static final String TOKEN_URL = "https://auth.example.test/2.0/oauth/token";
   public static final String REDIRECT_URI = "https://example.test/redirect";
}"#;
        let c = PsnConfig::from_decompiled_constants(src).unwrap();
        assert_eq!((c.client_id.as_str(), c.scope.as_str()), ("id-1", "a b c"));
        assert_eq!(PsnConfig::from_decompiled_constants("class X {}"), Err(ConfigError::Missing("CLIENT_ID")));
    }

    /// `OPENPS_PSN` is process-wide: tests that touch it take this lock and restore it.
    static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_env<T>(value: Option<&std::path::Path>, f: impl FnOnce() -> T) -> T {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let old = std::env::var_os("OPENPS_PSN");
        match value {
            Some(v) => std::env::set_var("OPENPS_PSN", v),
            None => std::env::remove_var("OPENPS_PSN"),
        }
        let r = f();
        match old {
            Some(v) => std::env::set_var("OPENPS_PSN", v),
            None => std::env::remove_var("OPENPS_PSN"),
        }
        r
    }

    #[test]
    fn credentials_are_found_in_the_data_folder_without_any_environment_variable() {
        let dir = tempfile::tempdir().unwrap();
        // nothing anywhere: not configured (and not an error)
        assert!(with_env(None, || PsnConfig::locate(dir.path())).is_none());
        // psn.json in the data folder is picked up
        std::fs::write(dir.path().join("psn.json"), test_config().to_json()).unwrap();
        let c = with_env(None, || PsnConfig::locate(dir.path())).expect("found").expect("valid");
        assert_eq!(c.client_id, test_config().client_id);
    }

    #[test]
    fn the_environment_variable_wins_and_a_wrong_path_is_an_error_not_a_silent_fallback() {
        let data = tempfile::tempdir().unwrap();
        std::fs::write(data.path().join("psn.json"), test_config().to_json()).unwrap();
        let other = tempfile::tempdir().unwrap();
        let mut c2 = test_config();
        c2.client_id = "from-env".into();
        let env_file = other.path().join("e.json");
        std::fs::write(&env_file, c2.to_json()).unwrap();
        let got = with_env(Some(&env_file), || PsnConfig::locate(data.path())).unwrap().unwrap();
        assert_eq!(got.client_id, "from-env");
        // a path that does not exist must not quietly fall back to the data folder file
        let missing = other.path().join("nope.json");
        assert!(with_env(Some(&missing), || PsnConfig::locate(data.path())).unwrap().is_err());
    }

    #[test]
    fn a_damaged_file_in_the_data_folder_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        for bad in ["", "not json", "{}", r#"{"client_id":"x"}"#] {
            std::fs::write(dir.path().join("psn.json"), bad).unwrap();
            assert!(with_env(None, || PsnConfig::locate(dir.path())).unwrap().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_api_client_is_read_from_the_constants_when_present_and_optional_otherwise() {
        let base = r#"
            public static final String CLIENT_ID = "cid";
            public static final String CLIENT_SECRET = "sec";
            public static final String TOKEN_URL = "https://a.test/2.0/oauth/token";
            public static final String REDIRECT_URI = "https://a.test/r";
            public static final String TOKEN_SCOPE = "psn:clientapp x";
        "#;
        assert_eq!(PsnConfig::from_decompiled_constants(base).unwrap().api, None);
        let with = format!(
            r#"{base}
            public static final String PSN_API_AUTH_BASE_URL = "https://ca.test/api/authz/v3/oauth";
            public static final String PSN_API_ACCESS_CODE_CLIENT_ID = "mobile";
            public static final String PSN_API_ACCESS_CODE_REDIRECT_URI = "com.app://redirect";
            public static final String PSN_API_ACCESS_CODE_SCOPE = "psn:mobile.v2.core psn:clientapp";
            public static final String PSN_API_ACCESS_CODE_TOKEN_EXCHANGE_BASIC_AUTH = "Basic QUJD";
        "#
        );
        let c = PsnConfig::from_decompiled_constants(&with).unwrap();
        let a = c.api.clone().unwrap();
        assert_eq!((a.client_id.as_str(), a.auth_base.as_str()), ("mobile", "https://ca.test/api/authz/v3/oauth"));
        assert!(!format!("{c:?}").contains("QUJD"), "the Basic credential never prints");
        // survives the JSON round trip, and old files without the section still load
        assert_eq!(PsnConfig::from_json(&c.to_json()).unwrap(), c);
        assert_eq!(
            PsnConfig::from_json(&PsnConfig { api: None, ..c }.to_json().replace(",\n  \"api\": null", ""))
                .unwrap()
                .api,
            None
        );
        // a malformed API section is rejected
        let bad = with.replace("Basic QUJD", "QUJD");
        assert!(PsnConfig::from_decompiled_constants(&bad).is_err());
    }
}
