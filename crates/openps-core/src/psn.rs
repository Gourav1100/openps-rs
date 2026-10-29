//! Adapts `openps_psn::PsnClient` to the core's `PsnService`.

use crate::core::PsnService;
use crate::types::PsnAccountInfo;
use openps_psn::http::HttpClient;
use openps_psn::oauth::{authorize_url, new_duid, parse_redirect, RedirectOutcome};
use openps_psn::store::SecretStore;
use openps_psn::PsnClient;
use rand_core::OsRng;

pub struct PsnAdapter<H: HttpClient + Send + Sync, S: SecretStore + Send + Sync> {
    client: PsnClient<H, S>,
}

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
        let _ = self.client.logout();
    }
}
