//! Ask the real Gmail API questions that no mock can answer.
//!
//! Read-only. Never writes to the account. Run with:
//!
//! ```text
//! HUSH_LIVE=1 cargo test --test live_probe -- --ignored --nocapture
//! ```
//!
//! The question that matters here: scanning costs 20 quota units per message
//! and the ceiling is 100 units a second, so Hush can read **five messages a
//! second** and no faster. A twenty-thousand-message mailbox is over an hour.
//! Anything that lets Gmail rule messages out server-side, before we spend 20
//! units finding out, is worth more than every other optimisation combined.
//!
//! `label:^unsub` is an undocumented internal Gmail label for mail carrying an
//! unsubscribe option. If it exists over the API and does not *miss* mail that
//! really has the header, it is a free prefilter. The false-negative direction
//! is the only one that matters: a message wrongly excluded is a sender the
//! user never sees.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use hush_lib::auth::{ClientCredentials, GoogleAuth};
use hush_lib::gmail::{Cancel, GmailClient, TokenSource};
use hush_lib::ratelimit::AdaptiveLimiter;
use hush_lib::state::{SETTING_ACCOUNT, SETTING_CLIENT_ID, SETTING_CLIENT_SECRET};
use hush_lib::store::Store;

const BASE: &str = "-in:sent -in:drafts -in:chats";

#[tokio::test]
#[ignore = "talks to a real Gmail account; set HUSH_LIVE=1"]
async fn does_gmail_let_us_filter_by_unsubscribe_server_side() {
    if std::env::var("HUSH_LIVE").ok().as_deref() != Some("1") {
        eprintln!("set HUSH_LIVE=1 to run this");
        return;
    }
    let gmail = connect().await;
    let cancel = Cancel::new();

    // Does the operator exist at all? A bad operator is not an error to Gmail;
    // it either matches nothing or is treated as a literal word, so "it
    // returned something" is the only signal available.
    let unsub = gmail
        .list_messages(&format!("{BASE} label:^unsub"), None, 500, &cancel)
        .await
        .expect("Gmail accepted the query");
    println!("label:^unsub returned {} ids on page one", unsub.ids.len());
    if unsub.ids.is_empty() {
        println!("=> the operator matches nothing here. Not usable.");
        return;
    }

    // The safe direction: everything it returns really does carry the header.
    let mut checked = 0;
    let mut carried = 0;
    for id in unsub.ids.iter().take(15) {
        let meta = gmail.get_metadata(id, &cancel).await.expect("metadata");
        checked += 1;
        if meta.list_unsubscribe.is_some() {
            carried += 1;
        }
    }
    println!("of {checked} sampled ^unsub messages, {carried} carry List-Unsubscribe");

    // The dangerous direction: does it MISS mail that has the header? Anything
    // excluded here is a sender the user would never be shown.
    let excluded = gmail
        .list_messages(&format!("{BASE} -label:^unsub"), None, 500, &cancel)
        .await
        .expect("list the excluded side");
    println!(
        "{} ids on page one of the excluded side",
        excluded.ids.len()
    );

    let mut missed = 0;
    let mut sampled = 0;
    for id in excluded.ids.iter().take(40) {
        let meta = gmail.get_metadata(id, &cancel).await.expect("metadata");
        sampled += 1;
        if meta.list_unsubscribe.is_some() {
            missed += 1;
            println!(
                "  MISSED: {} carries the header but is not ^unsub",
                meta.sender_address
            );
        }
    }
    println!("of {sampled} sampled non-^unsub messages, {missed} carry the header");
    println!(
        "\n=> {}",
        if missed == 0 {
            "usable as a prefilter on this evidence"
        } else {
            "NOT usable — it hides senders"
        }
    );
}

/// How much of the mailbox the prefilter would let us skip.
#[tokio::test]
#[ignore = "talks to a real Gmail account; set HUSH_LIVE=1"]
async fn how_much_of_a_real_mailbox_would_it_skip() {
    if std::env::var("HUSH_LIVE").ok().as_deref() != Some("1") {
        return;
    }
    let gmail = connect().await;
    let cancel = Cancel::new();

    for (label, q) in [
        ("everything", BASE.to_string()),
        ("with ^unsub", format!("{BASE} label:^unsub")),
    ] {
        let mut total = 0u64;
        let mut token = None;
        let mut pages = 0;
        loop {
            let page = gmail
                .list_messages(&q, token.as_deref(), 500, &cancel)
                .await
                .expect("list");
            total += page.ids.len() as u64;
            pages += 1;
            match page.next_page_token {
                Some(t) if pages < 60 => token = Some(t),
                _ => break,
            }
        }
        // 20 units per message, 100 units a second.
        println!("{label}: {total} messages ≈ {}s to read", total * 20 / 100);
    }
}

async fn connect() -> Arc<GmailClient> {
    let base = std::env::var("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::path::PathBuf::from(std::env::var("HOME").unwrap()).join(".local/share")
        })
        .join("dev.hush.desktop");
    let store = Store::open(&base.join("hush.sqlite3")).expect("the app's database");
    let creds = ClientCredentials {
        client_id: store.get_setting(SETTING_CLIENT_ID).unwrap().unwrap(),
        client_secret: store.get_setting(SETTING_CLIENT_SECRET).unwrap().unwrap(),
    };
    let email = store.get_setting(SETTING_ACCOUNT).unwrap().unwrap();
    let auth = GoogleAuth::new(creds).unwrap();
    assert!(
        auth.restore(&email).await.unwrap_or(false),
        "connect in the app first"
    );
    Arc::new(
        GmailClient::new(
            auth as Arc<dyn TokenSource>,
            Arc::new(AdaptiveLimiter::default()),
        )
        .unwrap(),
    )
}

// Keeps rustc quiet about the unused import when the tests bail out early.
#[allow(dead_code)]
type Never = Pin<Box<dyn Future<Output = ()>>>;
