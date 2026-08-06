//! End-to-end tests against a mocked Gmail API.
//!
//! These drive the real client and the real scanner; only Google is fake. They
//! cover the three things that actually go wrong in the field — paging through
//! a large mailbox, being throttled, and a connection that has expired — plus
//! the promise that message bodies are never requested.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use hush_lib::error::{Error, Result};
use hush_lib::gmail::{Cancel, GmailClient, TokenSource};
use hush_lib::model::ScanDepth;
use hush_lib::ratelimit::AdaptiveLimiter;
use hush_lib::scan::Scanner;
use hush_lib::store::Store;
use serde_json::json;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const ACCOUNT: &str = "me@example.com";

/// A token source that counts refreshes, so tests can prove one happened.
struct FakeTokens {
    refreshes: AtomicUsize,
    /// When true, the first token handed out is stale and gets a 401.
    starts_stale: bool,
}

impl FakeTokens {
    fn new(starts_stale: bool) -> Arc<Self> {
        Arc::new(Self {
            refreshes: AtomicUsize::new(0),
            starts_stale,
        })
    }
    fn current(&self) -> String {
        if self.starts_stale && self.refreshes.load(Ordering::SeqCst) == 0 {
            "stale-token".into()
        } else {
            "fresh-token".into()
        }
    }
}

impl TokenSource for FakeTokens {
    fn access_token(&self) -> Pin<Box<dyn Future<Output = Result<String>> + Send + '_>> {
        Box::pin(async move { Ok(self.current()) })
    }
    fn force_refresh(&self) -> Pin<Box<dyn Future<Output = Result<String>> + Send + '_>> {
        Box::pin(async move {
            self.refreshes.fetch_add(1, Ordering::SeqCst);
            Ok(self.current())
        })
    }
}

fn client(server: &MockServer, tokens: Arc<FakeTokens>) -> Arc<GmailClient> {
    Arc::new(
        GmailClient::with_base(
            &server.uri(),
            tokens as Arc<dyn TokenSource>,
            // A mock server has no quota; the limiter is exercised in its own tests.
            Arc::new(AdaptiveLimiter::with_rate(100_000.0)),
        )
        .unwrap(),
    )
}

/// A metadata response for one message, as Gmail would return it.
fn message_body(id: &str, sender: &str, subject: &str, unsubscribable: bool) -> serde_json::Value {
    let mut headers = vec![
        json!({"name": "From", "value": format!("Acme <{sender}>")}),
        json!({"name": "Subject", "value": subject}),
    ];
    if unsubscribable {
        headers.push(json!({
            "name": "List-Unsubscribe",
            "value": format!("<https://acme.example/u/{id}>")
        }));
        headers.push(json!({
            "name": "List-Unsubscribe-Post",
            "value": "List-Unsubscribe=One-Click"
        }));
    }
    json!({
        "id": id,
        "internalDate": "1700000000000",
        "payload": { "headers": headers }
    })
}

/// Serve every `messages/{id}` request by echoing the id back.
async fn mount_message_endpoint(server: &MockServer, unsubscribable: bool) {
    Mock::given(method("GET"))
        .and(wiremock::matchers::path_regex(
            r"^/gmail/v1/users/me/messages/[^/]+$",
        ))
        .respond_with(move |req: &Request| {
            let id = req.url.path().rsplit('/').next().unwrap_or("x").to_string();
            // Two senders, so grouping has something to do. Split on the last
            // digit: id length is identical across m1..m4.
            let last_digit = id.chars().last().and_then(|c| c.to_digit(10)).unwrap_or(0);
            let sender = if last_digit % 2 == 0 {
                "news@acme.example"
            } else {
                "offers@shop.example"
            };
            ResponseTemplate::new(200).set_body_json(message_body(
                &id,
                sender,
                "Hello there",
                unsubscribable,
            ))
        })
        .mount(server)
        .await;
}

async fn mount_profile(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/profile"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "emailAddress": ACCOUNT,
            "messagesTotal": 1234,
            "historyId": "999000"
        })))
        .mount(server)
        .await;
}

// --- pagination -----------------------------------------------------------

#[tokio::test]
async fn a_scan_follows_every_page_of_results() {
    let server = MockServer::start().await;
    mount_profile(&server).await;
    mount_message_endpoint(&server, true).await;

    // Page one hands back a token; page two ends the run.
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages"))
        .and(query_param("pageToken", "page-2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "messages": [{"id": "m3"}, {"id": "m4"}],
            "resultSizeEstimate": 4
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "messages": [{"id": "m1"}, {"id": "m2"}],
            "nextPageToken": "page-2",
            "resultSizeEstimate": 4
        })))
        .mount(&server)
        .await;

    let store = Arc::new(Store::open_in_memory().unwrap());
    let scanner = Scanner::new(
        client(&server, FakeTokens::new(false)),
        store.clone(),
        ACCOUNT.into(),
    );

    let progress = scanner
        .full_scan(ScanDepth::Everything, Cancel::new(), |_| {})
        .await
        .unwrap();

    assert_eq!(progress.scanned, 4, "every page must be walked");
    assert!(progress.finished && !progress.cancelled);
    assert_eq!(store.message_count(ACCOUNT).unwrap(), 4);
    assert_eq!(
        store.senders(ACCOUNT).unwrap().len(),
        2,
        "grouped by sender"
    );
}

#[tokio::test]
async fn an_empty_mailbox_finishes_cleanly() {
    let server = MockServer::start().await;
    mount_profile(&server).await;
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"resultSizeEstimate": 0})))
        .mount(&server)
        .await;

    let store = Arc::new(Store::open_in_memory().unwrap());
    let scanner = Scanner::new(
        client(&server, FakeTokens::new(false)),
        store.clone(),
        ACCOUNT.into(),
    );
    let p = scanner
        .full_scan(ScanDepth::SixMonths, Cancel::new(), |_| {})
        .await
        .unwrap();
    assert_eq!(p.scanned, 0);
    assert!(p.finished);
    assert!(store.senders(ACCOUNT).unwrap().is_empty());
}

// --- the safety gate, end to end ------------------------------------------

#[tokio::test]
async fn messages_without_an_unsubscribe_header_produce_no_senders() {
    let server = MockServer::start().await;
    mount_profile(&server).await;
    mount_message_endpoint(&server, false).await;
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "messages": [{"id": "m1"}, {"id": "m2"}, {"id": "m3"}],
            "resultSizeEstimate": 3
        })))
        .mount(&server)
        .await;

    let store = Arc::new(Store::open_in_memory().unwrap());
    Scanner::new(
        client(&server, FakeTokens::new(false)),
        store.clone(),
        ACCOUNT.into(),
    )
    .full_scan(ScanDepth::Everything, Cancel::new(), |_| {})
    .await
    .unwrap();

    assert_eq!(store.message_count(ACCOUNT).unwrap(), 3, "all were read");
    assert!(
        store.senders(ACCOUNT).unwrap().is_empty(),
        "but none may be offered for unsubscribe"
    );
}

// --- metadata only --------------------------------------------------------

#[tokio::test]
async fn the_client_only_ever_asks_for_metadata() {
    let server = MockServer::start().await;
    mount_message_endpoint(&server, true).await;

    let gmail = client(&server, FakeTokens::new(false));
    gmail.get_metadata("m1", &Cancel::new()).await.unwrap();

    let requests = server.received_requests().await.unwrap();
    let msg_request = requests
        .iter()
        .find(|r| r.url.path().contains("/messages/"))
        .expect("a message was fetched");

    let query = msg_request.url.query().unwrap_or_default();
    assert!(query.contains("format=metadata"), "{query}");
    assert!(!query.contains("format=full"), "{query}");
    assert!(!query.contains("format=raw"), "{query}");

    // And exactly the headers we declared, nothing more.
    for header in ["From", "List-Unsubscribe", "Subject"] {
        assert!(
            query.contains(&format!("metadataHeaders={header}")),
            "missing {header} in {query}"
        );
    }
}

// --- throttling -----------------------------------------------------------

#[tokio::test]
async fn a_throttled_request_is_retried_and_succeeds() {
    let server = MockServer::start().await;

    // Two refusals, then the real answer.
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages/m1"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "0")
                .set_body_json(json!({
                    "error": {"errors": [{"reason": "rateLimitExceeded"}], "code": 429}
                })),
        )
        .up_to_n_times(2)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages/m1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(message_body(
            "m1",
            "news@acme.example",
            "Hi",
            true,
        )))
        .mount(&server)
        .await;

    let gmail = client(&server, FakeTokens::new(false));
    let meta = gmail.get_metadata("m1", &Cancel::new()).await.unwrap();

    assert_eq!(meta.sender_address, "news@acme.example");
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        3,
        "two refusals then one success"
    );
}

#[tokio::test]
async fn a_403_that_means_throttling_is_also_retried() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages/m1"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "error": {"errors": [{"reason": "userRateLimitExceeded"}], "code": 403}
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages/m1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(message_body(
            "m1",
            "news@acme.example",
            "Hi",
            true,
        )))
        .mount(&server)
        .await;

    let gmail = client(&server, FakeTokens::new(false));
    assert!(gmail.get_metadata("m1", &Cancel::new()).await.is_ok());
}

#[tokio::test]
async fn a_403_that_means_a_missing_permission_is_not_retried() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages/m1"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "error": {"message": "Gmail API has not been used in project 123 before"}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let gmail = client(&server, FakeTokens::new(false));
    let err = gmail.get_metadata("m1", &Cancel::new()).await.unwrap_err();

    match err {
        Error::Setup(msg) => {
            assert!(msg.contains("switched on"), "{msg}");
            // Setup advice is the one place jargon is allowed, but not codes.
            assert!(!msg.contains("403"), "{msg}");
        }
        other => panic!("expected setup advice, got {other:?}"),
    }
}

#[tokio::test]
async fn a_server_error_eventually_gives_up_rather_than_hanging() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages/m1"))
        .respond_with(ResponseTemplate::new(500).insert_header("retry-after", "0"))
        .mount(&server)
        .await;

    let gmail = client(&server, FakeTokens::new(false));
    let err = gmail.get_metadata("m1", &Cancel::new()).await.unwrap_err();
    assert!(matches!(err, Error::RateLimited), "{err:?}");
}

// --- expiry ---------------------------------------------------------------

#[tokio::test]
async fn an_expired_connection_is_renewed_once_and_the_request_retried() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages/m1"))
        .and(wiremock::matchers::header(
            "authorization",
            "Bearer stale-token",
        ))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": {"code": 401, "message": "Invalid Credentials"}
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages/m1"))
        .and(wiremock::matchers::header(
            "authorization",
            "Bearer fresh-token",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(message_body(
            "m1",
            "news@acme.example",
            "Hi",
            true,
        )))
        .mount(&server)
        .await;

    let tokens = FakeTokens::new(true);
    let gmail = client(&server, tokens.clone());

    let meta = gmail.get_metadata("m1", &Cancel::new()).await.unwrap();
    assert_eq!(meta.id, "m1");
    assert_eq!(
        tokens.refreshes.load(Ordering::SeqCst),
        1,
        "exactly one renewal"
    );
}

#[tokio::test]
async fn a_connection_that_cannot_be_renewed_fails_instead_of_looping() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages/m1"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;

    let tokens = FakeTokens::new(false);
    let gmail = client(&server, tokens.clone());
    let err = gmail.get_metadata("m1", &Cancel::new()).await.unwrap_err();

    assert!(matches!(err, Error::Unauthorized), "{err:?}");
    assert_eq!(
        tokens.refreshes.load(Ordering::SeqCst),
        1,
        "renewal is attempted once, not repeatedly"
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

// --- cancelling -----------------------------------------------------------

#[tokio::test]
async fn cancelling_stops_the_scan_and_keeps_what_was_found() {
    let server = MockServer::start().await;
    mount_profile(&server).await;
    mount_message_endpoint(&server, true).await;

    let ids: Vec<_> = (0..400).map(|i| json!({"id": format!("m{i}")})).collect();
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "messages": ids,
            "resultSizeEstimate": 400
        })))
        .mount(&server)
        .await;

    let store = Arc::new(Store::open_in_memory().unwrap());
    let cancel = Cancel::new();
    let scanner = Scanner::new(
        client(&server, FakeTokens::new(false)),
        store.clone(),
        ACCOUNT.into(),
    );

    // Stop as soon as the scan has visibly started.
    let trigger = cancel.clone();
    let progress = scanner
        .full_scan(ScanDepth::Everything, cancel.clone(), move |p| {
            if p.scanned > 0 {
                trigger.cancel();
            }
        })
        .await
        .unwrap();

    assert!(progress.cancelled, "the scan reports being stopped");
    assert!(
        progress.scanned < 400,
        "it stopped early, got {}",
        progress.scanned
    );
    assert_eq!(
        store.message_count(ACCOUNT).unwrap(),
        progress.scanned,
        "everything read before stopping was kept"
    );

    // And the run is resumable rather than complete.
    assert!(!store.scan_state(ACCOUNT).unwrap().complete);
}

// --- incremental ----------------------------------------------------------

#[tokio::test]
async fn a_second_scan_asks_only_for_what_changed() {
    let server = MockServer::start().await;
    mount_profile(&server).await;
    mount_message_endpoint(&server, true).await;

    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "messages": [{"id": "m1"}],
            "resultSizeEstimate": 1
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/history"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "history": [{"messagesAdded": [{"message": {"id": "m2"}}]}],
            "historyId": "999500"
        })))
        .mount(&server)
        .await;

    let store = Arc::new(Store::open_in_memory().unwrap());
    let scanner = Scanner::new(
        client(&server, FakeTokens::new(false)),
        store.clone(),
        ACCOUNT.into(),
    );

    scanner
        .full_scan(ScanDepth::Everything, Cancel::new(), |_| {})
        .await
        .unwrap();
    assert_eq!(store.message_count(ACCOUNT).unwrap(), 1);
    assert_eq!(
        store.scan_state(ACCOUNT).unwrap().history_id.as_deref(),
        Some("999000"),
        "the history marker is saved for next time"
    );

    scanner
        .incremental_scan(Cancel::new(), |_| {})
        .await
        .unwrap();

    assert_eq!(
        store.message_count(ACCOUNT).unwrap(),
        2,
        "the new message was picked up"
    );
    assert_eq!(
        store.scan_state(ACCOUNT).unwrap().history_id.as_deref(),
        Some("999500"),
        "and the marker moved forward"
    );
}

#[tokio::test]
async fn a_rescan_drops_mail_that_has_left_the_mailbox() {
    // A scan used to only ever add, so anything deleted in Gmail lingered here
    // forever. A completed sweep now reconciles.
    let server = MockServer::start().await;
    mount_profile(&server).await;
    mount_message_endpoint(&server, true).await;

    let store = Arc::new(Store::open_in_memory().unwrap());
    let scanner = Scanner::new(
        client(&server, FakeTokens::new(false)),
        store.clone(),
        ACCOUNT.into(),
    );

    // First sweep: three messages.
    let first = Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "messages": [{"id": "m1"}, {"id": "m2"}, {"id": "m3"}],
            "resultSizeEstimate": 3
        })))
        .up_to_n_times(2)
        .mount_as_scoped(&server)
        .await;

    scanner
        .full_scan(ScanDepth::Everything, Cancel::new(), |_| {})
        .await
        .unwrap();
    assert_eq!(store.message_count(ACCOUNT).unwrap(), 3);
    drop(first);

    // Second sweep: one of them is gone from Gmail.
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "messages": [{"id": "m1"}, {"id": "m3"}],
            "resultSizeEstimate": 2
        })))
        .mount(&server)
        .await;

    scanner
        .full_scan(ScanDepth::Everything, Cancel::new(), |_| {})
        .await
        .unwrap();

    assert_eq!(
        store.message_count(ACCOUNT).unwrap(),
        2,
        "the message removed in Gmail should be gone here too"
    );
}

#[tokio::test]
async fn an_incremental_scan_never_deletes_what_it_did_not_look_for() {
    // The dangerous mistake: a history sweep reports only what *changed*, so
    // reconciling from it would delete nearly the whole database.
    let server = MockServer::start().await;
    mount_profile(&server).await;
    mount_message_endpoint(&server, true).await;

    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "messages": [{"id": "m1"}, {"id": "m2"}, {"id": "m3"}],
            "resultSizeEstimate": 3
        })))
        .mount(&server)
        .await;
    // History mentions exactly one new message and nothing else.
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/history"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "history": [{"messagesAdded": [{"message": {"id": "m9"}}]}],
            "historyId": "999500"
        })))
        .mount(&server)
        .await;

    let store = Arc::new(Store::open_in_memory().unwrap());
    let scanner = Scanner::new(
        client(&server, FakeTokens::new(false)),
        store.clone(),
        ACCOUNT.into(),
    );

    scanner
        .full_scan(ScanDepth::Everything, Cancel::new(), |_| {})
        .await
        .unwrap();
    assert_eq!(store.message_count(ACCOUNT).unwrap(), 3);

    scanner
        .incremental_scan(Cancel::new(), |_| {})
        .await
        .unwrap();

    assert_eq!(
        store.message_count(ACCOUNT).unwrap(),
        4,
        "the three existing messages must survive a history sweep"
    );
}

#[tokio::test]
async fn an_incremental_scan_needs_a_previous_full_one() {
    let server = MockServer::start().await;
    let store = Arc::new(Store::open_in_memory().unwrap());
    let scanner = Scanner::new(
        client(&server, FakeTokens::new(false)),
        store,
        ACCOUNT.into(),
    );
    assert!(scanner
        .incremental_scan(Cancel::new(), |_| {})
        .await
        .is_err());
}

// --- resilience -----------------------------------------------------------

#[tokio::test]
async fn one_unreadable_message_does_not_sink_the_scan() {
    let server = MockServer::start().await;
    mount_profile(&server).await;

    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages/m2"))
        .respond_with(ResponseTemplate::new(404).set_body_string("gone"))
        .mount(&server)
        .await;
    mount_message_endpoint(&server, true).await;

    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "messages": [{"id": "m1"}, {"id": "m2"}, {"id": "m3"}],
            "resultSizeEstimate": 3
        })))
        .mount(&server)
        .await;

    let store = Arc::new(Store::open_in_memory().unwrap());
    let progress = Scanner::new(
        client(&server, FakeTokens::new(false)),
        store.clone(),
        ACCOUNT.into(),
    )
    .full_scan(ScanDepth::Everything, Cancel::new(), |_| {})
    .await
    .unwrap();

    assert!(progress.finished);
    assert_eq!(progress.scanned, 2, "the readable ones still landed");
    assert_eq!(store.message_count(ACCOUNT).unwrap(), 2);
}

// --- managing the filters that blocking creates ------------------------------
//
// The rule these protect is the one with teeth: Hush removes filters it made
// and refuses to touch anything else. Getting that wrong deletes a rule
// somebody wrote by hand and relies on.

/// Serve a label list and a filter list, the two reads every filter operation
/// starts from.
async fn account_with(server: &MockServer, labels: serde_json::Value, filters: serde_json::Value) {
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/labels"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "labels": labels })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/settings/filters"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "filter": filters })))
        .mount(server)
        .await;
}

fn hush_label() -> serde_json::Value {
    json!([{"id": "Label_7", "name": "Hush"}])
}

#[tokio::test]
async fn a_filter_hush_created_round_trips_and_is_recognised_as_ours() {
    // The marker has to survive being written to Gmail and read back. This is
    // the mocked half of that check; the real half was run against a live
    // account, because a mock will agree with whatever we send it.
    let server = MockServer::start().await;
    account_with(
        &server,
        hush_label(),
        json!([{
            "id": "f-mine",
            "criteria": {"from": "news@shop.example"},
            "action": {"addLabelIds": ["Label_7"], "removeLabelIds": ["INBOX"]}
        }]),
    )
    .await;

    let gmail = client(&server, FakeTokens::new(false));
    let listed = hush_lib::filters::list(&gmail, &Cancel::new())
        .await
        .unwrap();

    assert_eq!(listed.len(), 1);
    assert!(listed[0].mine);
    assert_eq!(listed[0].address, "news@shop.example");
    assert!(listed[0].summary.contains("Nothing is deleted"));
}

#[tokio::test]
async fn a_filter_the_user_wrote_by_hand_is_listed_but_never_removed() {
    let server = MockServer::start().await;
    account_with(
        &server,
        hush_label(),
        json!([{
            "id": "f-theirs",
            "criteria": {"from": "boss@work.example"},
            // Identical in effect to one of ours, minus the marker.
            "action": {"addLabelIds": ["TRASH"], "removeLabelIds": ["INBOX"]}
        }]),
    )
    .await;
    // Deliberately unmounted: any DELETE at all fails the test, because
    // wiremock answers an unmatched request with a 404 and the removal would
    // report a failure rather than silently succeeding.
    let gmail = client(&server, FakeTokens::new(false));

    let listed = hush_lib::filters::list(&gmail, &Cancel::new())
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert!(!listed[0].mine, "no marker, not ours");

    let refused = hush_lib::filters::remove(&gmail, "f-theirs", false, &Cancel::new()).await;
    assert!(refused.is_err(), "a foreign filter must not be deleted");
    assert!(
        refused
            .unwrap_err()
            .to_string()
            .contains("wasn't created by Hush"),
        "and the refusal has to say why"
    );

    assert!(
        !server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .any(|r| r.method == wiremock::http::Method::DELETE),
        "not one delete may reach Google"
    );
}

#[tokio::test]
async fn removing_a_block_puts_back_the_mail_it_caught() {
    let server = MockServer::start().await;
    account_with(
        &server,
        hush_label(),
        json!([{
            "id": "f-mine",
            "criteria": {"from": "news@shop.example"},
            "action": {"addLabelIds": ["TRASH", "Label_7"], "removeLabelIds": ["INBOX"]}
        }]),
    )
    .await;

    Mock::given(method("DELETE"))
        .and(path("/gmail/v1/users/me/settings/filters/f-mine"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    // One message in Trash, one merely archived.
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages"))
        .and(query_param(
            "q",
            "in:trash label:Hush from:\"news@shop.example\"",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"messages": [{"id": "t1"}]})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages"))
        .and(query_param(
            "q",
            "-in:trash -in:inbox label:Hush from:\"news@shop.example\"",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"messages": [{"id": "a1"}]})))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/gmail/v1/users/me/messages/t1/untrash"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "t1"})))
        .expect(1)
        .mount(&server)
        .await;
    for id in ["t1", "a1"] {
        Mock::given(method("POST"))
            .and(path(format!("/gmail/v1/users/me/messages/{id}/modify")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": id})))
            .expect(1)
            .mount(&server)
            .await;
    }

    let gmail = client(&server, FakeTokens::new(false));
    let report = hush_lib::filters::remove(&gmail, "f-mine", true, &Cancel::new())
        .await
        .unwrap();

    assert!(report.filter_removed);
    assert_eq!(report.restored, 2, "both the trashed and the archived one");
    assert_eq!(report.restore_failed, 0);
}

#[tokio::test]
async fn the_filter_goes_even_if_putting_the_mail_back_fails() {
    // Half-done is the likely real-world shape of this — a quota limit part way
    // through. The filter must still be gone, because that is the thing the
    // user asked for, and the shortfall must be reported rather than rounded up.
    let server = MockServer::start().await;
    account_with(
        &server,
        hush_label(),
        json!([{
            "id": "f-mine",
            "criteria": {"from": "news@shop.example"},
            "action": {"addLabelIds": ["Label_7"], "removeLabelIds": ["INBOX"]}
        }]),
    )
    .await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"messages": [{"id": "a1"}]})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/gmail/v1/users/me/messages/a1/untrash"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "a1"})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/gmail/v1/users/me/messages/a1/modify"))
        .respond_with(ResponseTemplate::new(403).set_body_string("no"))
        .mount(&server)
        .await;

    let gmail = client(&server, FakeTokens::new(false));
    let report = hush_lib::filters::remove(&gmail, "f-mine", true, &Cancel::new())
        .await
        .unwrap();

    assert!(report.filter_removed, "the block is lifted regardless");
    assert_eq!(report.restored, 0);
    assert!(report.restore_failed > 0);
    assert!(report.problem.is_some());
}

#[tokio::test]
async fn nothing_is_ours_once_the_label_is_gone() {
    // Someone deletes the Hush label in Gmail. Every filter becomes foreign,
    // which is the safe direction to fail in: read-only, not delete-happy.
    let server = MockServer::start().await;
    account_with(
        &server,
        json!([]),
        json!([{
            "id": "f-mine",
            "criteria": {"from": "news@shop.example"},
            "action": {"addLabelIds": ["Label_7"], "removeLabelIds": ["INBOX"]}
        }]),
    )
    .await;

    let gmail = client(&server, FakeTokens::new(false));
    assert!(
        !hush_lib::filters::list(&gmail, &Cancel::new())
            .await
            .unwrap()[0]
            .mine
    );
    assert!(
        hush_lib::filters::remove(&gmail, "f-mine", false, &Cancel::new())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn the_likely_bulk_mail_is_read_first_and_never_read_twice() {
    // Reading costs 20 quota units against a ceiling of 100 a second, so the
    // order of a 43-minute scan is the only thing available to optimise.
    // `label:^unsub` picks out likely bulk mail, and goes first.
    //
    // It is an ordering hint only. Probing a real account found it misses
    // header-carrying mail, so excluding on it would hide senders — the second
    // pass has to cover the rest, and an id in both must be read once.
    let server = MockServer::start().await;
    mount_profile(&server).await;
    mount_message_endpoint(&server, true).await;

    let seen_queries = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let record = seen_queries.clone();

    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages"))
        .respond_with(move |req: &Request| {
            let q = req
                .url
                .query_pairs()
                .find(|(k, _)| k == "q")
                .map(|(_, v)| v.to_string())
                .unwrap_or_default();
            record.lock().unwrap().push(q.clone());
            // m2 appears in both halves, as it would if Gmail re-labelled it
            // between the two calls.
            let ids = if q.contains("-label:^unsub") {
                json!([{"id": "m2"}, {"id": "m3"}])
            } else {
                json!([{"id": "m1"}, {"id": "m2"}])
            };
            ResponseTemplate::new(200).set_body_json(json!({ "messages": ids }))
        })
        .mount(&server)
        .await;

    let store = Arc::new(Store::open_in_memory().unwrap());
    let progress = Scanner::new(
        client(&server, FakeTokens::new(false)),
        store.clone(),
        ACCOUNT.into(),
    )
    .full_scan(ScanDepth::Everything, Cancel::new(), |_| {})
    .await
    .unwrap();

    let queries = seen_queries.lock().unwrap().clone();
    assert_eq!(queries.len(), 2, "one pass each, no more");
    assert!(
        queries[0].contains("label:^unsub") && !queries[0].contains("-label:^unsub"),
        "the likely-bulk pass goes first, got {:?}",
        queries
    );
    assert!(
        queries[1].contains("-label:^unsub"),
        "and the remainder second, got {:?}",
        queries
    );

    assert_eq!(progress.total, 3, "m2 is counted once, not twice");
    assert_eq!(store.message_count(ACCOUNT).unwrap(), 3);
}

#[tokio::test]
async fn a_scan_still_covers_the_mailbox_if_the_ordering_hint_breaks() {
    // `label:^unsub` is undocumented. If Google drops it the first pass fails,
    // and the scan must carry on rather than dying with it.
    let server = MockServer::start().await;
    mount_profile(&server).await;
    mount_message_endpoint(&server, true).await;

    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages"))
        .and(query_param(
            "q",
            "-in:sent -in:drafts -in:chats -label:Hush label:^unsub",
        ))
        .respond_with(ResponseTemplate::new(400).set_body_string("bad operator"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "messages": [{"id": "m1"}, {"id": "m2"}, {"id": "m3"}]
        })))
        .mount(&server)
        .await;

    let store = Arc::new(Store::open_in_memory().unwrap());
    let progress = Scanner::new(
        client(&server, FakeTokens::new(false)),
        store.clone(),
        ACCOUNT.into(),
    )
    .full_scan(ScanDepth::Everything, Cancel::new(), |_| {})
    .await
    .unwrap();

    assert!(progress.finished, "a broken hint is not a broken scan");
    assert_eq!(
        store.message_count(ACCOUNT).unwrap(),
        3,
        "nothing was missed"
    );
}
