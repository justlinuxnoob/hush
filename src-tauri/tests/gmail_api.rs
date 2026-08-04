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
