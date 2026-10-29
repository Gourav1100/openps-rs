//! OAuth client configuration. The client id and secret are Sony's, not this project's, so they are never
//! committed: they are loaded from an external JSON file (`$OPENPS_PSN`, or a path you pass), produced once with
//! `extract-psn-config` from your own copy of the original application.

use serde::{Deserialize, Serialize};

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PsnConfig {
    pub client_id: String,
    pub client_secret: String,
    /// `.../2.0/oauth/token`; the introspection endpoint is `<token_url>/<access_token>`.
    pub token_url: String,
    pub redirect_uri: String,
    /// Space separated.
    pub scope: String,
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

impl PsnConfig {
    pub fn validate(&self) -> Result<(), ConfigError> {
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
        if !self.token_url.starts_with("https://") {
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
        let c = PsnConfig {
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
}
