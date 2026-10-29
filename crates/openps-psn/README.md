# openps-psn

PSN sign-in via the OAuth authorization-code flow, used to learn the **PSN account id** that PS5 registration
needs (and to hold tokens for later features). The flow: open `authorize_url` in a web view, intercept the redirect
with `parse_redirect`, then `PsnClient::login(code)` exchanges it for tokens, looks up the account, and stores the
tokens in a `SecretStore`. `access_token()` refreshes automatically.

## Credentials are external

The OAuth client id and secret belong to Sony, not to this project. They are **not** in this repository. Create the
config once from your own copy of the original application and keep it outside the source tree:

```sh
cargo run -p openps-psn --bin extract-psn-config -- /path/to/decompiled/PsnConstants.java /path/to/psn.json
export OPENPS_PSN=/path/to/psn.json     # the file is written with mode 0600
```

`PsnConfig` redacts the secret from `Debug`, and `psn.json` is git-ignored.

## No accidental network access

All requests go through the `HttpClient` trait. A real HTTPS client exists only behind the `live` cargo feature
(`--features live`), which is off by default and never used by tests, so no test or default build can contact
Sony's servers. Tests use a scripted fake. Whether and how to use the live client against a real account is a
deliberate decision for the application, not something this crate does on its own.

## Secrets

Tokens are stored through `SecretStore`: `MemoryStore` (non-persistent) or `KeychainStore` (macOS Keychain).
Never persist them in a plain file. The Keychain round-trip test is `#[ignore]` because it touches the real
keychain: `cargo test -p openps-psn -- --ignored`.
