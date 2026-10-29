//! PSN sign-in for OpenPS: the OAuth authorization-code flow used to learn the account id that PS5 registration
//! needs. All network access goes through [`http::HttpClient`]; no live client exists unless the `live`
//! feature is enabled, and the credentials come from an external, git-ignored file ([`config::PsnConfig`]).
pub mod api;
pub mod client;
pub mod config;
pub mod http;
pub mod library;
pub mod oauth;
pub mod store;

pub use client::PsnClient;
pub use config::PsnConfig;
pub use oauth::{Account, PsnError, Tokens};
