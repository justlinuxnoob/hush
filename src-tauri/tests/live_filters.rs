//! A round-trip against the real Gmail API, run by hand.
//!
//! Ignored by default and skipped unless `HUSH_LIVE=1`, because it needs a
//! connected account and it writes to it. Run with:
//!
//! ```text
//! HUSH_LIVE=1 cargo test --test live_filters -- --ignored --nocapture
//! ```
//!
//! It exists because of the most expensive lesson in this project: **a mock
//! agrees with whatever you send it**. The whole test suite was green while the
//! trash endpoint answered `411 Length Required` to every real request, because
//! wiremock happily accepted the bodyless POST that Google rejects.
//!
//! Marking Hush's own filters with a label is exactly that shape of risk. The
//! question "does a user label survive being written into a filter action and
//! read back" cannot be answered by a server that echoes our own assumptions.
//! So this asks Google.
//!
//! It cleans up after itself: the filter it creates is deleted before it
//! returns, including when an assertion fails partway.

use std::sync::Arc;

use hush_lib::auth::{ClientCredentials, GoogleAuth};
use hush_lib::filters;
use hush_lib::gmail::{BlockAction, Cancel, GmailClient, TokenSource};
use hush_lib::ratelimit::AdaptiveLimiter;
use hush_lib::state::{SETTING_ACCOUNT, SETTING_CLIENT_ID, SETTING_CLIENT_SECRET};
use hush_lib::store::Store;

/// An address that cannot receive mail, so the filter is inert while it exists.
const PROBE: &str = "hush-round-trip-probe@invalid.example";

#[tokio::test]
#[ignore = "writes to a real Gmail account; set HUSH_LIVE=1"]
async fn the_marker_label_survives_a_real_round_trip() {
    if std::env::var("HUSH_LIVE").ok().as_deref() != Some("1") {
        eprintln!("set HUSH_LIVE=1 to run this");
        return;
    }

    let store = Store::open(&data_dir().join("hush.sqlite3")).expect("open the app's database");
    let creds = ClientCredentials {
        client_id: store
            .get_setting(SETTING_CLIENT_ID)
            .unwrap()
            .expect("client id"),
        client_secret: store
            .get_setting(SETTING_CLIENT_SECRET)
            .unwrap()
            .expect("client secret"),
    };
    let email = store
        .get_setting(SETTING_ACCOUNT)
        .unwrap()
        .expect("an account");

    let auth = GoogleAuth::new(creds).unwrap();
    assert!(
        auth.restore(&email).await.unwrap_or(false),
        "no usable connection in the keychain — connect in the app first"
    );

    let gmail = Arc::new(
        GmailClient::new(
            auth as Arc<dyn TokenSource>,
            Arc::new(AdaptiveLimiter::default()),
        )
        .unwrap(),
    );
    let cancel = Cancel::new();

    let marker = filters::ensure_label(&gmail, &cancel)
        .await
        .expect("the Hush label");
    println!("label id: {marker}");

    let id = gmail
        .block_sender(PROBE, BlockAction::Archive, Some(&marker), &cancel)
        .await
        .expect("create the filter");
    println!("filter id: {id}");

    // Read everything first and assert nothing yet. A failed assertion here
    // would abort the test and leave a live filter on someone's real account,
    // so the tidy-up has to happen before any of it can fire.
    let listed = filters::list(&gmail, &cancel).await.expect("list filters");
    let ours = listed.iter().find(|f| f.id == id).cloned();
    // The negative half: what else on this account does Hush claim? Printed
    // rather than asserted, because a real account's other filters are not
    // ours to have opinions about.
    for f in &listed {
        if f.id != id && f.mine {
            println!("also recognised as Hush's: {} ({})", f.address, f.id);
        }
    }

    gmail
        .delete_filter(&id, &cancel)
        .await
        .expect("clean up the probe filter");
    let after = filters::list(&gmail, &cancel).await.unwrap();

    let ours = ours.expect("the filter we just made was not in the list at all");
    assert!(
        ours.mine,
        "the label did not survive the round trip — the marker is not usable"
    );
    assert_eq!(ours.action, Some(BlockAction::Archive));
    assert_eq!(ours.address.to_lowercase(), PROBE);
    assert!(
        !after.iter().any(|f| f.id == id),
        "the probe filter is still on the account"
    );
}

/// Where the app keeps its database, matching `lib.rs`.
fn data_dir() -> std::path::PathBuf {
    if let Ok(dir) = std::env::var("HUSH_DATA_DIR") {
        return dir.into();
    }
    let base = std::env::var("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::path::PathBuf::from(std::env::var("HOME").expect("HOME")).join(".local/share")
        });
    base.join("dev.hush.desktop")
}
