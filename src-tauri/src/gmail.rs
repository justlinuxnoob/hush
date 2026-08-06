//! A thin Gmail API client.
//!
//! Two things about this client matter more than the code:
//!
//! 1. It only ever asks for `format=metadata` with an explicit list of header
//!    names. Message bodies are never requested, so they never cross the
//!    network and cannot be stored. This is checkable — grep for `format=`.
//! 2. `base` is injectable, which is how the tests point the whole client at a
//!    local mock server and exercise pagination, throttling and expiry without
//!    touching Google.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::model::{extract_address, extract_display_name, normalise_address, MessageMeta};
use crate::ratelimit::{backoff_delay, AdaptiveLimiter, MAX_RETRIES};

pub const GMAIL_BASE: &str = "https://gmail.googleapis.com";

/// The exact set of headers Hush asks for. Nothing else is fetched.
pub const METADATA_HEADERS: &[&str] = &[
    "From",
    "List-Unsubscribe",
    "List-Unsubscribe-Post",
    "Subject",
    "Date",
];

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Supplies access tokens and knows how to renew them.
///
/// A trait rather than a concrete type so the client can be tested without an
/// OAuth flow, and so token storage stays entirely in `auth`.
pub trait TokenSource: Send + Sync {
    fn access_token(&self) -> BoxFuture<'_, Result<String>>;
    /// Exchange the refresh token for a new access token.
    fn force_refresh(&self) -> BoxFuture<'_, Result<String>>;
}

/// A flag the interface can flip to stop a long scan.
#[derive(Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
    /// Whether these two handles control the same operation.
    ///
    /// Needed because a finishing scan must only clear the shared handle if it
    /// is still its own — otherwise it wipes the handle belonging to a scan
    /// that started after it, and Stop has nothing left to signal.
    pub fn is_same(&self, other: &Cancel) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }
}

pub struct GmailClient {
    http: reqwest::Client,
    base: String,
    tokens: Arc<dyn TokenSource>,
    pub limiter: Arc<AdaptiveLimiter>,
}

#[derive(Debug, Default)]
pub struct MessagePage {
    pub ids: Vec<String>,
    pub next_page_token: Option<String>,
    pub total_estimate: u64,
}

#[derive(Debug, Deserialize)]
pub struct Profile {
    #[serde(rename = "emailAddress")]
    pub email_address: String,
    #[serde(rename = "messagesTotal", default)]
    pub messages_total: u64,
    #[serde(rename = "historyId", default)]
    pub history_id: String,
}

#[derive(Debug, Default)]
pub struct HistoryPage {
    /// Ids of messages added since the starting point.
    pub added_ids: Vec<String>,
    pub next_page_token: Option<String>,
    pub history_id: Option<String>,
}

/// What a block does to the sender's future mail.
///
/// There is deliberately no third option. Permanently deleting mail needs the
/// `https://mail.google.com/` scope, which Hush does not ask for and will not,
/// so "delete forever" is not a thing the app is able to do even by mistake.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BlockAction {
    /// Skip the inbox. The mail is still in the account, still searchable,
    /// still there in a year. Nothing is ever deleted.
    #[default]
    Archive,
    /// Move to Trash, which Gmail empties after 30 days.
    Trash,
}

impl BlockAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Archive => "archive",
            Self::Trash => "trash",
        }
    }

    /// Parse a stored preference. Anything unrecognised — including a value
    /// written by a future version — falls back to the safe one.
    pub fn parse(s: &str) -> Self {
        match s {
            "trash" => Self::Trash,
            _ => Self::Archive,
        }
    }
}

/// A Gmail filter, as the settings API reports it.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Filter {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub criteria: FilterCriteria,
    #[serde(default)]
    pub action: FilterAction,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct FilterCriteria {
    #[serde(default)]
    pub from: String,
    #[serde(default)]
    pub to: String,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub query: String,
    #[serde(rename = "negatedQuery", default)]
    pub negated_query: String,
    #[serde(rename = "hasAttachment", default)]
    pub has_attachment: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct FilterAction {
    #[serde(rename = "addLabelIds", default)]
    pub add_label_ids: Vec<String>,
    #[serde(rename = "removeLabelIds", default)]
    pub remove_label_ids: Vec<String>,
    #[serde(default)]
    pub forward: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Label {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
}

impl GmailClient {
    pub fn new(tokens: Arc<dyn TokenSource>, limiter: Arc<AdaptiveLimiter>) -> Result<Self> {
        Self::with_base(GMAIL_BASE, tokens, limiter)
    }

    pub fn with_base(
        base: &str,
        tokens: Arc<dyn TokenSource>,
        limiter: Arc<AdaptiveLimiter>,
    ) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("hush/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(45))
            .build()?;
        Ok(Self {
            http,
            base: base.trim_end_matches('/').to_string(),
            tokens,
            limiter,
        })
    }

    pub async fn profile(&self) -> Result<Profile> {
        let url = format!("{}/gmail/v1/users/me/profile", self.base);
        let body = self
            .get(&url, &[], crate::ratelimit::COST_PROFILE, &Cancel::new())
            .await?;
        Ok(serde_json::from_str(&body)?)
    }

    /// One page of message ids matching `query`.
    pub async fn list_messages(
        &self,
        query: &str,
        page_token: Option<&str>,
        page_size: u32,
        cancel: &Cancel,
    ) -> Result<MessagePage> {
        let url = format!("{}/gmail/v1/users/me/messages", self.base);
        let size = page_size.to_string();
        let mut params: Vec<(&str, &str)> = vec![("maxResults", &size)];
        if !query.is_empty() {
            params.push(("q", query));
        }
        if let Some(t) = page_token {
            params.push(("pageToken", t));
        }

        let body = self
            .get(&url, &params, crate::ratelimit::COST_MESSAGES_LIST, cancel)
            .await?;

        #[derive(Deserialize)]
        struct Resp {
            #[serde(default)]
            messages: Vec<IdOnly>,
            #[serde(rename = "nextPageToken")]
            next_page_token: Option<String>,
            #[serde(rename = "resultSizeEstimate", default)]
            result_size_estimate: u64,
        }
        #[derive(Deserialize)]
        struct IdOnly {
            id: String,
        }

        let resp: Resp = serde_json::from_str(&body)?;
        Ok(MessagePage {
            ids: resp.messages.into_iter().map(|m| m.id).collect(),
            next_page_token: resp.next_page_token,
            total_estimate: resp.result_size_estimate,
        })
    }

    /// Fetch one message's metadata. Never its body.
    pub async fn get_metadata(&self, id: &str, cancel: &Cancel) -> Result<MessageMeta> {
        let url = format!("{}/gmail/v1/users/me/messages/{}", self.base, id);
        let mut params: Vec<(&str, &str)> = vec![("format", "metadata")];
        for h in METADATA_HEADERS {
            params.push(("metadataHeaders", h));
        }

        let body = self
            .get(&url, &params, crate::ratelimit::COST_MESSAGES_GET, cancel)
            .await?;
        let raw: RawMessage = serde_json::from_str(&body)?;
        Ok(raw.into_meta())
    }

    /// Changes since `start_history_id`, used to refresh a cached scan cheaply.
    pub async fn list_history(
        &self,
        start_history_id: &str,
        page_token: Option<&str>,
        cancel: &Cancel,
    ) -> Result<HistoryPage> {
        let url = format!("{}/gmail/v1/users/me/history", self.base);
        let mut params: Vec<(&str, &str)> = vec![
            ("startHistoryId", start_history_id),
            ("historyTypes", "messageAdded"),
        ];
        if let Some(t) = page_token {
            params.push(("pageToken", t));
        }

        let body = self
            .get(&url, &params, crate::ratelimit::COST_HISTORY_LIST, cancel)
            .await?;

        #[derive(Deserialize)]
        struct Resp {
            #[serde(default)]
            history: Vec<HistoryItem>,
            #[serde(rename = "nextPageToken")]
            next_page_token: Option<String>,
            #[serde(rename = "historyId")]
            history_id: Option<String>,
        }
        #[derive(Deserialize)]
        struct HistoryItem {
            #[serde(rename = "messagesAdded", default)]
            messages_added: Vec<Added>,
        }
        #[derive(Deserialize)]
        struct Added {
            message: IdOnly,
        }
        #[derive(Deserialize)]
        struct IdOnly {
            id: String,
        }

        let resp: Resp = serde_json::from_str(&body)?;
        let mut added_ids = Vec::new();
        for h in resp.history {
            for a in h.messages_added {
                added_ids.push(a.message.id);
            }
        }
        Ok(HistoryPage {
            added_ids,
            next_page_token: resp.next_page_token,
            history_id: resp.history_id,
        })
    }

    /// Move one message to Gmail's Trash, where it stays recoverable for 30
    /// days. Requires the modify permission, which the user opts into.
    ///
    /// The dedicated endpoint is used rather than adding a `TRASH` label
    /// through `batchModify` — batching a thousand at a time would be far
    /// cheaper in quota, but whether the API accepts `TRASH` that way is
    /// disputed, and a disputed reading is not something to lean on when the
    /// operation removes someone's mail.
    pub async fn trash_message(&self, id: &str, cancel: &Cancel) -> Result<()> {
        let url = format!("{}/gmail/v1/users/me/messages/{}/trash", self.base, id);
        self.request(crate::ratelimit::COST_MESSAGES_TRASH, cancel, |token| {
            // The empty body is load-bearing. Gmail's trash endpoint takes no
            // request body, but rejects a POST that arrives without a
            // Content-Length header with `411 Length Required` — and a
            // bodyless reqwest POST omits the header entirely rather than
            // sending zero. Setting an explicit empty body produces
            // `Content-Length: 0` and Google accepts it.
            //
            // Every trash request this app made before this line failed.
            self.http
                .post(&url)
                .bearer_auth(token)
                .header(reqwest::header::CONTENT_LENGTH, "0")
                .body("")
        })
        .await
        .map(|_| ())
    }

    /// Create a Gmail filter that keeps this sender's future mail out of the
    /// inbox.
    ///
    /// This is the difference between asking and deciding. An unsubscribe is a
    /// request to a sender who may ignore it, may take a fortnight, or may have
    /// you on four other lists. A filter is a rule in the user's own account
    /// that applies to every message from that address from now on.
    ///
    /// Which is exactly why the action matters. A filter is not header-gated the
    /// way binning the backlog is: it catches *everything* that address sends,
    /// including the order confirmation from a shop that also sends newsletters.
    /// `Archive` is therefore the honest default — the mail leaves the inbox and
    /// stays searchable forever. `Trash` is available, and it is on a 30-day
    /// fuse, so the user has to ask for it in as many words.
    ///
    /// `marker_label` is the label id that identifies this filter as Hush's
    /// later on, and the mail it caught along with it. Passing `None` still
    /// creates a working filter — it just becomes indistinguishable from one the
    /// user wrote by hand, and Hush will refuse to touch it afterwards.
    pub async fn block_sender(
        &self,
        address: &str,
        action: BlockAction,
        marker_label: Option<&str>,
        cancel: &Cancel,
    ) -> Result<String> {
        let url = format!("{}/gmail/v1/users/me/settings/filters", self.base);

        let mut add: Vec<&str> = Vec::new();
        if action == BlockAction::Trash {
            add.push("TRASH");
        }
        if let Some(label) = marker_label {
            add.push(label);
        }

        let payload = serde_json::json!({
            "criteria": { "from": address },
            "action": { "addLabelIds": add, "removeLabelIds": ["INBOX"] }
        });

        let body = self
            .request(crate::ratelimit::COST_FILTER_CREATE, cancel, |token| {
                self.http.post(&url).bearer_auth(token).json(&payload)
            })
            .await?;

        #[derive(Deserialize)]
        struct Created {
            #[serde(default)]
            id: String,
        }
        Ok(serde_json::from_str::<Created>(&body)
            .map(|c| c.id)
            .unwrap_or_default())
    }

    /// Every filter currently on the account, as Gmail reports it.
    ///
    /// Gmail is the authority on what filters exist; our own 200 only says a
    /// request was accepted. It is also the only place they are stored — Hush
    /// keeps no list of what it blocked, so this is read back fresh each time
    /// and works the same on a machine that has never seen the account before.
    pub async fn list_filters(&self, cancel: &Cancel) -> Result<Vec<Filter>> {
        let url = format!("{}/gmail/v1/users/me/settings/filters", self.base);
        let body = self
            .get(&url, &[], crate::ratelimit::COST_PROFILE, cancel)
            .await?;

        #[derive(Deserialize)]
        struct Resp {
            #[serde(default)]
            filter: Vec<Filter>,
        }
        Ok(serde_json::from_str::<Resp>(&body)?.filter)
    }

    /// The `from` address of every filter on the account. Used to confirm a
    /// block landed.
    pub async fn list_filter_senders(&self, cancel: &Cancel) -> Result<Vec<String>> {
        Ok(self
            .list_filters(cancel)
            .await?
            .into_iter()
            .map(|f| f.criteria.from)
            .filter(|f| !f.is_empty())
            .collect())
    }

    /// Delete one filter. The caller is responsible for having established that
    /// it is one of ours; this function will delete whatever it is given.
    pub async fn delete_filter(&self, id: &str, cancel: &Cancel) -> Result<()> {
        let url = format!("{}/gmail/v1/users/me/settings/filters/{}", self.base, id);
        self.request(crate::ratelimit::COST_FILTER_DELETE, cancel, |token| {
            self.http.delete(&url).bearer_auth(token)
        })
        .await
        .map(|_| ())
    }

    /// Every label on the account, id and name.
    pub async fn list_labels(&self, cancel: &Cancel) -> Result<Vec<Label>> {
        let url = format!("{}/gmail/v1/users/me/labels", self.base);
        let body = self
            .get(&url, &[], crate::ratelimit::COST_LABELS_LIST, cancel)
            .await?;

        #[derive(Deserialize)]
        struct Resp {
            #[serde(default)]
            labels: Vec<Label>,
        }
        Ok(serde_json::from_str::<Resp>(&body)?.labels)
    }

    /// Create a label, returning its id.
    pub async fn create_label(&self, name: &str, cancel: &Cancel) -> Result<String> {
        let url = format!("{}/gmail/v1/users/me/labels", self.base);
        let payload = serde_json::json!({
            "name": name,
            "labelListVisibility": "labelShow",
            "messageListVisibility": "show",
        });

        let body = self
            .request(crate::ratelimit::COST_LABELS_CREATE, cancel, |token| {
                self.http.post(&url).bearer_auth(token).json(&payload)
            })
            .await?;
        Ok(serde_json::from_str::<Label>(&body)?.id)
    }

    /// Take one message back out of Trash.
    pub async fn untrash_message(&self, id: &str, cancel: &Cancel) -> Result<()> {
        let url = format!("{}/gmail/v1/users/me/messages/{}/untrash", self.base, id);
        self.request(crate::ratelimit::COST_MESSAGES_TRASH, cancel, |token| {
            // Same empty body as `trash_message`, for the same reason: without
            // an explicit `Content-Length: 0` Google answers 411.
            self.http
                .post(&url)
                .bearer_auth(token)
                .header(reqwest::header::CONTENT_LENGTH, "0")
                .body("")
        })
        .await
        .map(|_| ())
    }

    /// Add and remove labels on one message. Used to put mail back in the inbox
    /// and to take Hush's marker off it again.
    pub async fn modify_message(
        &self,
        id: &str,
        add: &[&str],
        remove: &[&str],
        cancel: &Cancel,
    ) -> Result<()> {
        let url = format!("{}/gmail/v1/users/me/messages/{}/modify", self.base, id);
        let payload = serde_json::json!({ "addLabelIds": add, "removeLabelIds": remove });
        self.request(crate::ratelimit::COST_MESSAGES_MODIFY, cancel, |token| {
            self.http.post(&url).bearer_auth(token).json(&payload)
        })
        .await
        .map(|_| ())
    }

    async fn get(
        &self,
        url: &str,
        params: &[(&str, &str)],
        cost: f64,
        cancel: &Cancel,
    ) -> Result<String> {
        self.request(cost, cancel, |token| {
            self.http.get(url).query(params).bearer_auth(token)
        })
        .await
    }

    /// Issue a request, respecting the rate limiter and retrying the failures
    /// that are worth retrying.
    ///
    /// The retry policy is deliberately narrow. A 401 is retried exactly once,
    /// after renewing the token — retrying it further would just hammer Google
    /// with credentials it has already rejected. Throttling and server errors
    /// are retried with backoff. Everything else fails immediately, because a
    /// 400 will still be a 400 on the fifth attempt.
    async fn request<F>(&self, cost: f64, cancel: &Cancel, build: F) -> Result<String>
    where
        F: Fn(&str) -> reqwest::RequestBuilder,
    {
        // Captured for the log only; the token never reaches it.
        let url_for_log = build("")
            .build()
            .map(|r| r.url().to_string())
            .unwrap_or_default();

        let mut refreshed = false;

        for attempt in 0..MAX_RETRIES {
            cancel.check()?;
            self.limiter.acquire(cost).await;
            cancel.check()?;

            let token = self.tokens.access_token().await?;
            let response = match build(&token).send().await {
                Ok(r) => r,
                Err(e) => {
                    // A transport error is usually a blip; a timeout on every
                    // attempt is not, and the loop bound handles that.
                    if attempt + 1 == MAX_RETRIES {
                        return Err(Error::Network(e.to_string()));
                    }
                    tokio::time::sleep(backoff_delay(attempt, None)).await;
                    continue;
                }
            };

            let status = response.status();
            let retry_after = parse_retry_after(&response);

            if status.is_success() {
                self.limiter.on_success().await;
                return Ok(response.text().await?);
            }

            let body = response.text().await.unwrap_or_default();

            if status == reqwest::StatusCode::UNAUTHORIZED {
                if refreshed {
                    return Err(Error::Unauthorized);
                }
                refreshed = true;
                self.tokens.force_refresh().await?;
                continue;
            }

            if status == reqwest::StatusCode::TOO_MANY_REQUESTS
                || (status == reqwest::StatusCode::FORBIDDEN && is_rate_limit_body(&body))
                || status.is_server_error()
            {
                self.limiter.on_throttled().await;
                if attempt + 1 == MAX_RETRIES {
                    return Err(Error::RateLimited);
                }
                tokio::time::sleep(backoff_delay(attempt, retry_after)).await;
                continue;
            }

            if status == reqwest::StatusCode::FORBIDDEN {
                // Not throttling: a missing permission or a disabled API.
                return Err(Error::Setup(friendly_forbidden(&body)));
            }

            // The status and Google's own words go to the log. The user gets a
            // plain sentence; whoever is debugging gets the fact. Keeping only
            // the friendly version is what made this failure unexplainable.
            log::warn!(
                "unexpected {} from {}: {}",
                status.as_u16(),
                redact_url(url_for_log),
                truncate(&body, 400)
            );
            return Err(Error::UnexpectedResponse(format!(
                "{} {}",
                status.as_u16(),
                truncate(&body, 200)
            )));
        }

        Err(Error::RateLimited)
    }
}

/// Google signals throttling with a 403 as well as a 429, distinguished only by
/// the reason string in the body.
fn is_rate_limit_body(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("ratelimitexceeded")
        || lower.contains("userratelimitexceeded")
        || lower.contains("quotaexceeded")
        || lower.contains("backenderror")
}

fn friendly_forbidden(body: &str) -> String {
    let lower = body.to_ascii_lowercase();
    if lower.contains("has not been used") || lower.contains("is disabled") {
        "Gmail access isn't switched on for your Google project yet. \
         Step 2 of setup covers this."
            .to_string()
    } else if lower.contains("insufficient") || lower.contains("scope") {
        "Hush doesn't have permission for that yet. Reconnect your account to grant it.".to_string()
    } else {
        "Google refused the request. Reconnecting your account usually fixes this.".to_string()
    }
}

fn parse_retry_after(response: &reqwest::Response) -> Option<Duration> {
    response
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

/// Message ids are not secret, but a log is something people paste in public,
/// so the path is kept and anything that looks like a query string is not.
fn redact_url(url: String) -> String {
    match url.split_once('?') {
        Some((path, _)) => format!("{path}?…"),
        None => url,
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        // Slice on a char boundary so a multi-byte body cannot panic here.
        let end = (0..=n).rev().find(|i| s.is_char_boundary(*i)).unwrap_or(0);
        format!("{}…", &s[..end])
    }
}

#[derive(Debug, Deserialize)]
struct RawMessage {
    id: String,
    #[serde(rename = "internalDate", default)]
    internal_date: String,
    #[serde(default)]
    payload: RawPayload,
}

#[derive(Debug, Default, Deserialize)]
struct RawPayload {
    #[serde(default)]
    headers: Vec<RawHeader>,
}

#[derive(Debug, Deserialize)]
struct RawHeader {
    name: String,
    #[serde(default)]
    value: String,
}

impl RawMessage {
    fn header(&self, name: &str) -> Option<&str> {
        self.payload
            .headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case(name))
            .map(|h| h.value.as_str())
    }

    fn into_meta(self) -> MessageMeta {
        let from = self.header("From").unwrap_or_default().to_string();
        let subject = self.header("Subject").unwrap_or_default().to_string();
        let list_unsubscribe = self.header("List-Unsubscribe").map(str::to_string);
        let list_unsubscribe_post = self.header("List-Unsubscribe-Post").map(str::to_string);
        let date_ms = self.internal_date.parse::<i64>().unwrap_or(0);

        MessageMeta {
            id: self.id,
            sender_address: normalise_address(&from),
            sender_name: if from.is_empty() {
                String::new()
            } else {
                extract_display_name(&from)
            },
            subject,
            date_ms,
            list_unsubscribe,
            list_unsubscribe_post,
        }
        .tap_raw_address(&from)
    }
}

impl MessageMeta {
    /// Keep the exact address alongside the normalised one where they differ
    /// only by case, so what we show matches what the user would see in Gmail.
    fn tap_raw_address(mut self, from: &str) -> Self {
        let raw = extract_address(from);
        if raw.to_ascii_lowercase() == self.sender_address {
            self.sender_address = raw.to_ascii_lowercase();
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_request_never_asks_for_a_body() {
        // A guard against someone "fixing" a parsing bug by fetching `full`.
        assert!(!METADATA_HEADERS.is_empty());
        assert!(METADATA_HEADERS.contains(&"List-Unsubscribe"));
        assert!(METADATA_HEADERS.contains(&"List-Unsubscribe-Post"));
        let src = include_str!("gmail.rs");
        assert!(
            !src.contains("\"format\", \"full\"") && !src.contains("\"format\", \"raw\""),
            "the client must only ever request format=metadata"
        );
    }

    #[test]
    fn raw_messages_become_metadata() {
        let json = r#"{
            "id": "abc",
            "internalDate": "1700000000000",
            "payload": { "headers": [
                {"name": "From", "value": "Acme Weekly <News+x@Acme.com>"},
                {"name": "Subject", "value": "Hello"},
                {"name": "list-unsubscribe", "value": "<https://acme.com/u>"},
                {"name": "List-Unsubscribe-Post", "value": "List-Unsubscribe=One-Click"}
            ]}
        }"#;
        let raw: RawMessage = serde_json::from_str(json).unwrap();
        let m = raw.into_meta();
        assert_eq!(m.sender_address, "news@acme.com");
        assert_eq!(m.sender_name, "Acme Weekly");
        assert_eq!(m.subject, "Hello");
        assert_eq!(m.date_ms, 1_700_000_000_000);
        assert_eq!(m.list_unsubscribe.as_deref(), Some("<https://acme.com/u>"));
    }

    #[test]
    fn missing_headers_are_tolerated() {
        let raw: RawMessage = serde_json::from_str(r#"{"id":"x"}"#).unwrap();
        let m = raw.into_meta();
        assert_eq!(m.id, "x");
        assert_eq!(m.date_ms, 0);
        assert!(m.list_unsubscribe.is_none());
        assert!(m.sender_address.is_empty());
    }

    #[test]
    fn throttle_bodies_are_recognised() {
        assert!(is_rate_limit_body(
            r#"{"error":{"errors":[{"reason":"rateLimitExceeded"}]}}"#
        ));
        assert!(is_rate_limit_body(
            r#"{"error":{"errors":[{"reason":"userRateLimitExceeded"}]}}"#
        ));
        assert!(!is_rate_limit_body(
            r#"{"error":{"errors":[{"reason":"insufficientPermissions"}]}}"#
        ));
    }

    #[test]
    fn forbidden_messages_avoid_jargon() {
        let m = friendly_forbidden(r#"{"error":{"message":"Gmail API has not been used"}}"#);
        assert!(m.contains("switched on"));
        for jargon in ["403", "OAuth", "scope."] {
            assert!(!m.contains(jargon), "{m}");
        }
    }

    #[test]
    fn truncate_does_not_split_multibyte_characters() {
        let s = "é".repeat(500);
        let t = truncate(&s, 201);
        assert!(t.ends_with('…'));
        assert!(t.chars().count() < 200);
    }

    #[test]
    fn a_cancel_knows_which_operation_it_belongs_to() {
        // The bug this guards: a finishing scan cleared the shared handle
        // unconditionally, wiping the one belonging to a scan that started
        // after it — leaving a running scan whose Stop button did nothing.
        let a = Cancel::new();
        let b = Cancel::new();
        assert!(a.is_same(&a.clone()), "a clone controls the same operation");
        assert!(!a.is_same(&b), "two separate scans are not interchangeable");
    }

    #[test]
    fn cancel_is_shared_between_clones() {
        let c = Cancel::new();
        let c2 = c.clone();
        assert!(!c.is_cancelled());
        c2.cancel();
        assert!(c.is_cancelled());
        assert!(matches!(c.check(), Err(Error::Cancelled)));
    }
}
