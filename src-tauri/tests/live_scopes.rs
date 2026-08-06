//! Can Hush ask Google what permissions it actually holds?
//!
//! Read-only. `HUSH_LIVE=1 cargo test --test live_scopes -- --ignored --nocapture`
//!
//! Today the app trusts a string it cached when the user connected. If they
//! revoke access in their Google account settings, or approve fewer boxes than
//! Hush recorded, nothing notices until an operation fails halfway through.
//! Google's tokeninfo endpoint reports the live token's real scopes, which is
//! the only authoritative answer.

use std::sync::Arc;

use hush_lib::auth::{ClientCredentials, GoogleAuth};
use hush_lib::gmail::TokenSource;
use hush_lib::state::{SETTING_ACCOUNT, SETTING_CLIENT_ID, SETTING_CLIENT_SECRET};
use hush_lib::store::Store;

#[tokio::test]
#[ignore = "talks to a real Google account; set HUSH_LIVE=1"]
async fn tokeninfo_reports_the_real_scopes() {
    if std::env::var("HUSH_LIVE").ok().as_deref() != Some("1") {
        return;
    }
    let base = std::path::PathBuf::from(std::env::var("HOME").unwrap())
        .join(".local/share/dev.hush.desktop");
    let store = Store::open(&base.join("hush.sqlite3")).unwrap();
    let creds = ClientCredentials {
        client_id: store.get_setting(SETTING_CLIENT_ID).unwrap().unwrap(),
        client_secret: store.get_setting(SETTING_CLIENT_SECRET).unwrap().unwrap(),
    };
    let email = store.get_setting(SETTING_ACCOUNT).unwrap().unwrap();
    let auth = GoogleAuth::new(creds).unwrap();
    assert!(auth.restore(&email).await.unwrap_or(false), "connect first");

    let token = (auth.clone() as Arc<dyn TokenSource>)
        .access_token()
        .await
        .unwrap();

    let body = reqwest::Client::new()
        .get("https://oauth2.googleapis.com/tokeninfo")
        .query(&[("access_token", token.as_str())])
        .send()
        .await
        .expect("tokeninfo reachable")
        .text()
        .await
        .unwrap();

    println!("tokeninfo says:\n{body}");
    assert!(
        body.contains("scope"),
        "no scope field — this endpoint is not usable for the check"
    );

    let cached = store
        .get_setting("granted_scopes")
        .unwrap()
        .unwrap_or_default();
    println!("\nHush had cached:\n{cached}");
}
