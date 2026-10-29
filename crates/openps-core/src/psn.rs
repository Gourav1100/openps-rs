//! Adapts `openps_psn::PsnClient` to the core's `PsnService`.

use crate::core::PsnService;
use crate::types::PsnAccountInfo;
use openps_psn::api::{Api, PsnConsole};
use openps_psn::http::HttpClient;
use openps_psn::oauth::{authorize_url, new_duid, parse_redirect, RedirectOutcome};
use openps_psn::store::SecretStore;
use openps_psn::PsnClient;
use rand_core::OsRng;

pub struct PsnAdapter<H: HttpClient + Send + Sync, S: SecretStore + Send + Sync> {
    client: PsnClient<H, S>,
}

/// The platform name PSN's command API uses for a PS5.
const PLATFORM: &str = "PROSPERO";

impl<H: HttpClient + Send + Sync, S: SecretStore + Send + Sync> PsnAdapter<H, S> {
    pub fn new(client: PsnClient<H, S>) -> Self {
        Self { client }
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl<H: HttpClient + Send + Sync, S: SecretStore + Send + Sync> PsnService for PsnAdapter<H, S> {
    fn authorize_url(&self) -> Result<String, String> {
        Ok(authorize_url(self.client.config(), &new_duid(&mut OsRng)))
    }

    fn complete(&self, redirect_url: &str) -> Result<PsnAccountInfo, String> {
        match parse_redirect(self.client.config(), redirect_url) {
            RedirectOutcome::Code(code) => {
                let a = self.client.login(&code, unix_now()).map_err(|e| e.to_string())?;
                Ok(PsnAccountInfo { online_id: a.online_id, account_id: a.account_id.to_string() })
            }
            RedirectOutcome::Denied(_) => Err("sign-in was cancelled".into()),
            RedirectOutcome::NotRedirect => Err("that is not the PSN sign-in redirect".into()),
        }
    }

    fn is_redirect(&self, url: &str) -> bool {
        openps_psn::oauth::is_redirect(self.client.config(), url)
    }

    fn sign_out(&self) {
        let _ = self.try_sign_out();
    }

    fn try_sign_out(&self) -> Result<(), String> {
        self.client.logout().map_err(|e| e.to_string())
    }

    fn api_available(&self) -> bool {
        self.client.config().api.is_some()
    }

    fn api_sign_in(&self, pasted: &str) -> Result<(), String> {
        self.api()?.sign_in(pasted, unix_now()).map_err(|e| e.to_string())
    }

    fn remote_access_token(&self) -> Result<String, String> {
        match self.client.access_token(unix_now()) {
            Ok(Some(t)) => Ok(t),
            Ok(None) => Err("your PlayStation sign-in has expired: sign in again".into()),
            Err(e) => Err(e.to_string()),
        }
    }

    fn api_signed_in(&self) -> bool {
        self.api().is_ok_and(|a| a.is_signed_in())
    }

    fn api_sign_out(&self) {
        if let Ok(a) = self.api() {
            a.sign_out();
        }
    }

    fn api_consoles(&self) -> Result<Vec<PsnConsole>, String> {
        self.api()?.consoles(unix_now()).map_err(|e| e.to_string())
    }

    fn api_library(&self) -> Result<openps_psn::library::Library, String> {
        self.api()?.library(unix_now()).map_err(|e| e.to_string())
    }

    fn api_launch(&self, duid: &str, title_id: &str) -> Result<String, String> {
        self.api()?.launch_title(unix_now(), PLATFORM, duid, title_id).map_err(|e| e.to_string())
    }

    fn api_command_status(&self, command_id: &str) -> Result<openps_psn::library::CommandStatus, String> {
        self.api()?.command_status(unix_now(), PLATFORM, command_id).map_err(|e| e.to_string())
    }

    fn api_art(&self, url: &str) -> Result<(Vec<u8>, String), String> {
        self.api()?.fetch_art(url)
    }
}

impl<H: HttpClient + Send + Sync, S: SecretStore + Send + Sync> PsnAdapter<H, S> {
    fn api(&self) -> Result<Api<'_, H, S>, String> {
        let cfg = self
            .client
            .config()
            .api
            .as_ref()
            .ok_or("this build has no PSN API credentials: regenerate psn.json with extract-psn-config")?;
        Ok(Api { cfg, http: self.client.http(), store: self.client.secrets() })
    }
}
