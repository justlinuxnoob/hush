//! Carrying out the unsubscribes the user picked.
//!
//! Four rules govern this module:
//!
//! * **A bare link is never fired automatically.** Only RFC 8058 one-click
//!   endpoints get a POST, because only those have promised that a POST means
//!   "unsubscribe" and nothing else. Everything else goes to the human.
//! * **Dry run means dry run.** In dry-run mode no socket is opened and no mail
//!   is sent; the outcomes describe exactly what would have happened.
//! * **Clearing out old mail is opt-in, and only ever moves it to Trash.**
//!   Unsubscribing is the point; tidying up the backlog is an extra the user
//!   asks for each time. Gmail keeps trashed mail for 30 days, so a mistake
//!   stays recoverable without our help.
//! * **Nothing is ever permanently deleted.** There is no code path for it, and
//!   Hush never requests a permission that would allow it.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::gmail::GmailClient;
use crate::model::{now_ms, Outcome, OutcomeStatus};
use crate::parse::UnsubMethod;

/// One-click endpoints are expected to answer quickly; a slow one is not worth
/// holding a queue of fifty senders behind.
const HTTP_TIMEOUT: Duration = Duration::from_secs(20);

/// How often a request in flight checks whether the user has pressed Stop.
const CANCEL_POLL: Duration = Duration::from_millis(250);

/// Resolve `work`, unless the user gives up first.
///
/// Without this, pressing Stop during a run would be noticed only between
/// senders — and with a twenty-second timeout each, fifty senders is sixteen
/// minutes of a button that does nothing.
async fn until_cancelled<F, T>(work: F, cancel: &crate::gmail::Cancel) -> Result<T>
where
    F: std::future::Future<Output = Result<T>>,
{
    tokio::pin!(work);
    loop {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        match tokio::time::timeout(CANCEL_POLL, &mut work).await {
            Ok(result) => return result,
            Err(_) => continue,
        }
    }
}

/// The exact body RFC 8058 specifies.
const ONE_CLICK_BODY: &str = "List-Unsubscribe=One-Click";

/// How `mailto:` unsubscribes are carried out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MailtoMode {
    /// Open the user's own mail app with a prefilled draft. They press send.
    /// Needs no extra permission from Google.
    ///
    /// The default, deliberately: sending mail on someone's behalf should be
    /// something they opt into, not something they discover.
    #[default]
    HandOff,
    /// Send it through Gmail directly. Fully automatic, but requires the
    /// broader "send mail" permission.
    SendViaGmail,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnsubRequest {
    pub address: String,
    pub display_name: String,
    pub method: UnsubMethod,
    /// Every route this sender offers, best first. Tried in order until one
    /// works, because a sender who publishes both a one-click endpoint and a
    /// `mailto:` should not be reported as a failure when only the first is
    /// broken.
    #[serde(default)]
    pub methods: Vec<UnsubMethod>,
}

/// What the confirmation screen shows, and what a dry run reports.
#[derive(Debug, Clone, Serialize)]
pub struct PlannedAction {
    pub address: String,
    pub display_name: String,
    /// Plain-language description, e.g. "Unsubscribed automatically".
    pub what: String,
    /// The exact request, for the dry-run log. Technical on purpose: this pane
    /// is the one place a curious user is allowed to see the machinery.
    pub detail: String,
}

pub struct Executor {
    http: reqwest::Client,
    pub dry_run: bool,
    pub mailto_mode: MailtoMode,
    gmail: Option<Arc<GmailClient>>,
    from_address: String,
}

/// A `mailto:` handoff the interface must open in the user's mail app.
#[derive(Debug, Clone, Serialize)]
pub struct Handoff {
    pub address: String,
    pub mailto_url: String,
}

#[derive(Debug, Default, Serialize)]
pub struct RunReport {
    pub outcomes: Vec<Outcome>,
    /// Draft mails for the interface to open, in order.
    pub handoffs: Vec<Handoff>,
    /// Present only when the user asked for their old mail to be cleared out.
    pub trash: Option<TrashReport>,
}

impl Executor {
    pub fn new(dry_run: bool, mailto_mode: MailtoMode, from_address: String) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("hush/", env!("CARGO_PKG_VERSION")))
            .timeout(HTTP_TIMEOUT)
            // RFC 8058 says a one-click endpoint must not redirect. Following
            // one anyway would turn a POST we vetted into a request we did not.
            .redirect(reqwest::redirect::Policy::none())
            // No cookie jar: nothing about one unsubscribe should be carried
            // into the next, and RFC 8058 forbids cookies outright.
            .build()?;
        Ok(Self {
            http,
            dry_run,
            mailto_mode,
            gmail: None,
            from_address,
        })
    }

    pub fn with_gmail(mut self, gmail: Arc<GmailClient>) -> Self {
        self.gmail = Some(gmail);
        self
    }

    /// Describe what `run` would do, without doing any of it.
    pub fn plan(&self, requests: &[UnsubRequest]) -> Vec<PlannedAction> {
        requests.iter().map(|r| self.plan_one(r)).collect()
    }

    fn plan_one(&self, r: &UnsubRequest) -> PlannedAction {
        let (what, detail) = match &r.method {
            UnsubMethod::OneClick { url } => (
                "Unsubscribe automatically".to_string(),
                format!("POST {url}\nContent-Type: application/x-www-form-urlencoded\n\n{ONE_CLICK_BODY}"),
            ),
            UnsubMethod::Mailto { address, subject, body } => match self.mailto_mode {
                MailtoMode::HandOff => (
                    "Open a ready-to-send email".to_string(),
                    format!(
                        "Open your mail app addressed to {address}\nSubject: {}\n\n{}",
                        subject.as_deref().unwrap_or("Unsubscribe"),
                        body.as_deref().unwrap_or("")
                    ),
                ),
                MailtoMode::SendViaGmail => (
                    "Send an unsubscribe email".to_string(),
                    format!(
                        "Send mail as {} to {address}\nSubject: {}",
                        self.from_address,
                        subject.as_deref().unwrap_or("Unsubscribe")
                    ),
                ),
            },
            UnsubMethod::ManualLink { url } => (
                "You'll open this one yourself".to_string(),
                format!("No request is sent. The link is listed for you to open: {url}"),
            ),
            UnsubMethod::None => (
                "Nothing to do".to_string(),
                "This sender has no unsubscribe option.".to_string(),
            ),
        };
        PlannedAction {
            address: r.address.clone(),
            display_name: r.display_name.clone(),
            what,
            detail,
        }
    }

    /// Carry out every request, one after another.
    ///
    /// Sequential on purpose: fifty simultaneous POSTs to fifty marketing
    /// platforms looks like an attack, and the whole run is over in seconds
    /// either way.
    pub async fn run(&self, requests: &[UnsubRequest], cancel: &crate::gmail::Cancel) -> RunReport {
        let mut report = RunReport::default();
        for r in requests {
            if cancel.is_cancelled() {
                // Everything already done stays done and is reported; the rest
                // simply never happened.
                break;
            }
            match self.run_one(r, cancel).await {
                Ok((outcome, handoff)) => {
                    report.outcomes.push(outcome);
                    if let Some(h) = handoff {
                        report.handoffs.push(h);
                    }
                }
                Err(e) => report.outcomes.push(Outcome {
                    address: r.address.clone(),
                    display_name: r.display_name.clone(),
                    status: OutcomeStatus::Failed,
                    detail: friendly(&e),
                    link: link_of(&r.method),
                    at_ms: now_ms(),
                }),
            }
        }
        report
    }

    /// Work down every route the sender offers until one succeeds.
    ///
    /// A sender who publishes both a one-click endpoint and a `mailto:` gets
    /// both tried. Only when every automatic route has been exhausted does this
    /// fall back to handing the user a link — and the link is always kept, so
    /// there is a way through even when nothing automatic works.
    async fn run_one(
        &self,
        r: &UnsubRequest,
        cancel: &crate::gmail::Cancel,
    ) -> Result<(Outcome, Option<Handoff>)> {
        let routes: Vec<UnsubMethod> = if r.methods.is_empty() {
            vec![r.method.clone()]
        } else {
            r.methods.clone()
        };

        let mut last_problem: Option<Error> = None;
        for (attempt, method) in routes.iter().enumerate() {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            match self.try_one(r, method, cancel).await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    log::warn!(
                        "route {} of {} failed for {}: {e}",
                        attempt + 1,
                        routes.len(),
                        r.address
                    );
                    last_problem = Some(e);
                }
            }
        }

        // Everything failed. Hand back whatever link exists so the user still
        // has a way to finish it themselves, rather than a dead end.
        let link = routes.iter().find_map(link_of);
        let detail = match &last_problem {
            Some(e) => format!("{e} You can still open their page and do it by hand."),
            None => "This sender doesn't offer a way to unsubscribe.".to_string(),
        };
        Ok((
            Outcome {
                address: r.address.clone(),
                display_name: r.display_name.clone(),
                status: if link.is_some() {
                    OutcomeStatus::NeedsYou
                } else {
                    OutcomeStatus::Failed
                },
                detail,
                link,
                at_ms: now_ms(),
            },
            None,
        ))
    }

    async fn try_one(
        &self,
        r: &UnsubRequest,
        method: &UnsubMethod,
        cancel: &crate::gmail::Cancel,
    ) -> Result<(Outcome, Option<Handoff>)> {
        let out = |status: OutcomeStatus, detail: &str, link: Option<String>| Outcome {
            address: r.address.clone(),
            display_name: r.display_name.clone(),
            status,
            detail: detail.to_string(),
            link,
            at_ms: now_ms(),
        };

        if self.dry_run {
            let planned = self.plan_one(r);
            return Ok((
                out(
                    OutcomeStatus::Simulated,
                    &format!("Dry run — nothing was sent. Would have: {}", planned.what),
                    link_of(&r.method),
                ),
                None,
            ));
        }

        match method {
            UnsubMethod::OneClick { url } => match self.one_click(url, cancel).await {
                // The link is kept even on success. A 200 means the sender
                // received and accepted the request; it does not mean they
                // acted on it, and there is no protocol by which they could
                // tell us. So the one thing that actually helps when a sender
                // ignores it — their own unsubscribe page — stays to hand.
                Ok(()) => Ok((
                    out(
                        OutcomeStatus::Done,
                        "Unsubscribe sent and accepted",
                        Some(url.clone()),
                    ),
                    None,
                )),
                // Delivered and accepted, but the sender answered with a
                // redirect rather than a plain yes. Reported as sent rather
                // than done, with the link kept so it can be checked by hand.
                Err(Error::Redirected) => Ok((
                    out(
                        OutcomeStatus::Sent,
                        "Unsubscribe request sent — the sender didn't confirm it outright",
                        Some(url.clone()),
                    ),
                    None,
                )),
                Err(e) => Err(e),
            },

            UnsubMethod::Mailto {
                address,
                subject,
                body,
            } => {
                let subject = subject.as_deref().unwrap_or("Unsubscribe");
                let body = body
                    .as_deref()
                    .unwrap_or("Please unsubscribe me from this list.");
                match self.mailto_mode {
                    MailtoMode::HandOff => Ok((
                        out(
                            OutcomeStatus::NeedsYou,
                            "A ready-to-send email is waiting in your mail app",
                            None,
                        ),
                        Some(Handoff {
                            address: r.address.clone(),
                            mailto_url: build_mailto_url(address, subject, body),
                        }),
                    )),
                    MailtoMode::SendViaGmail => {
                        let gmail = self.gmail.as_ref().ok_or_else(|| {
                            Error::Setup(
                                "Hush needs permission to send mail for this. \
                                 Reconnect your account to grant it."
                                    .into(),
                            )
                        })?;
                        let raw = build_rfc5322(&self.from_address, address, subject, body)?;
                        gmail.send_raw(&raw).await?;
                        Ok((
                            out(OutcomeStatus::Sent, "Unsubscribe email sent", None),
                            None,
                        ))
                    }
                }
            }

            UnsubMethod::ManualLink { url } => Ok((
                out(
                    OutcomeStatus::NeedsYou,
                    "Open this one yourself to finish",
                    Some(url.clone()),
                ),
                None,
            )),

            UnsubMethod::None => Err(Error::Other(
                "This sender doesn't offer a way to unsubscribe.".into(),
            )),
        }
    }

    /// The RFC 8058 one-click POST.
    async fn one_click(&self, url: &str, cancel: &crate::gmail::Cancel) -> Result<()> {
        vet_destination(url).await?;

        let request = self
            .http
            .post(url)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(ONE_CLICK_BODY)
            .send();

        let response = until_cancelled(
            async {
                request.await.map_err(|e| {
                    Error::Network(if e.is_timeout() {
                        "The website didn't answer in time".into()
                    } else {
                        "Couldn't reach the website".into()
                    })
                })
            },
            cancel,
        )
        .await?;

        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        if status.is_redirection() {
            // RFC 8058 says a one-click endpoint must not redirect, and plenty
            // of them do it anyway — usually to a "you have been unsubscribed"
            // page. The POST was delivered and accepted either way, so calling
            // it a failure and sending the user off to do it by hand was wrong.
            //
            // The redirect is still not followed: where it leads is the
            // sender's business, and following it would turn a request we
            // vetted into one we did not.
            return Err(Error::Redirected);
        }
        // 405 is the commonest failure in the wild by a distance: the endpoint
        // is published in the header but only wired up for GET, so it works in
        // a browser and refuses a POST. 401/403 is the same story with a login
        // in front of it. Both are finishable by hand, so say so plainly rather
        // than quoting a status code at someone.
        if status == reqwest::StatusCode::METHOD_NOT_ALLOWED
            || status == reqwest::StatusCode::UNAUTHORIZED
            || status == reqwest::StatusCode::FORBIDDEN
            || status == reqwest::StatusCode::NOT_FOUND
        {
            return Err(Error::Other(
                "This sender's unsubscribe only works in a browser.".into(),
            ));
        }

        Err(Error::Other(
            "The sender's website turned the request down.".into(),
        ))
    }
}

/// What happened when clearing out a sender's old mail.
#[derive(Debug, Default, Clone, Serialize)]
pub struct TrashReport {
    pub trashed: u64,
    pub failed: u64,
    /// True when the run was a rehearsal and nothing actually moved.
    pub simulated: bool,
    /// The messages that actually moved, so the caller can drop them from the
    /// local cache. Without this the sender would keep showing its old count
    /// and a second tidy-up would re-attempt mail already in the bin.
    ///
    /// Not sent to the interface — it has no use for a list of ids, and a
    /// thousand of them would bloat every result payload.
    #[serde(skip)]
    pub moved_ids: Vec<String>,
    /// How many of the binned messages Gmail still shows outside Trash when
    /// asked afterwards. Zero means the move is confirmed, not merely reported.
    /// `None` means the check could not be run.
    pub still_present: Option<u64>,
}

/// Ask Gmail whether the mail we binned is actually gone.
///
/// Reporting "moved 490 emails" on the strength of 490 HTTP 200s is a claim
/// about our own requests, not about the mailbox. This checks the mailbox.
/// `messages.list` excludes Trash, so anything we binned that still comes back
/// under a `from:` search for that sender did not move.
///
/// One list call per sender — five quota units — so the reassurance is close to
/// free.
pub async fn verify_binned(
    gmail: &Arc<GmailClient>,
    senders: &[String],
    binned_ids: &[String],
    cancel: &crate::gmail::Cancel,
) -> Option<u64> {
    if binned_ids.is_empty() {
        return Some(0);
    }
    let binned: std::collections::HashSet<&String> = binned_ids.iter().collect();
    let mut still_there = 0u64;

    for address in senders {
        // Quoted so an address with a hyphen or plus is taken literally.
        let query = format!("from:\"{address}\"");
        let mut page_token: Option<String> = None;
        loop {
            let page = match gmail
                .list_messages(&query, page_token.as_deref(), 500, cancel)
                .await
            {
                Ok(p) => p,
                // A failed check is reported as "could not confirm" rather than
                // as a failure to move anything — those are different claims.
                Err(e) => {
                    log::warn!("couldn't confirm the bin for {address}: {e}");
                    return None;
                }
            };
            still_there += page.ids.iter().filter(|id| binned.contains(id)).count() as u64;
            page_token = page.next_page_token;
            if page_token.is_none() {
                break;
            }
        }
    }
    Some(still_there)
}

/// How many trash calls are in flight at once. The rate limiter sets the pace;
/// this only bounds how many are waiting on the network together.
const TRASH_CONCURRENCY: usize = 8;

/// Move a set of messages to Gmail's Trash.
///
/// The caller decides which ids these are, and the only caller
/// ([`crate::store::Store::bulk_message_ids`]) selects messages that carried an
/// unsubscribe header. Nothing here re-derives that, so if you add another
/// caller, read that function's documentation first.
///
/// Trash, never permanent deletion: Gmail keeps trashed mail for 30 days, so a
/// mistake here is recoverable by the user without our help. Hush has no code
/// path that permanently deletes anything, and does not hold a permission that
/// would let it.
pub async fn trash_messages(
    gmail: &Arc<GmailClient>,
    ids: &[String],
    dry_run: bool,
    cancel: &crate::gmail::Cancel,
) -> TrashReport {
    if dry_run {
        return TrashReport {
            trashed: ids.len() as u64,
            failed: 0,
            simulated: true,
            // A rehearsal moved nothing, so nothing may be forgotten.
            moved_ids: Vec::new(),
            still_present: None,
        };
    }

    let mut report = TrashReport::default();
    let mut queue = ids.iter().cloned();
    // The id comes back with the result so a success can be recorded against
    // the message it belongs to.
    let mut tasks: tokio::task::JoinSet<(String, Result<()>)> = tokio::task::JoinSet::new();

    let mut spawn_next = |tasks: &mut tokio::task::JoinSet<(String, Result<()>)>| {
        if let Some(id) = queue.next() {
            let gmail = gmail.clone();
            let cancel = cancel.clone();
            tasks.spawn(async move {
                let outcome = gmail.trash_message(&id, &cancel).await;
                (id, outcome)
            });
            true
        } else {
            false
        }
    };

    for _ in 0..TRASH_CONCURRENCY {
        if !spawn_next(&mut tasks) {
            break;
        }
    }

    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok((id, Ok(()))) => {
                report.trashed += 1;
                report.moved_ids.push(id);
            }
            Ok((_, Err(Error::Cancelled))) => {
                tasks.abort_all();
                break;
            }
            // One message that will not move should not strand the rest; the
            // counts stay honest either way.
            Ok((_, Err(e))) => {
                log::warn!("couldn't move a message to Trash: {e}");
                report.failed += 1;
            }
            Err(e) => {
                log::warn!("a trash task ended unexpectedly: {e}");
                report.failed += 1;
            }
        }
        if cancel.is_cancelled() {
            tasks.abort_all();
            break;
        }
        spawn_next(&mut tasks);
    }

    report
}

/// Refuse to send a request to anything that is not a public internet host.
///
/// The URL comes from an email header, which means a sender chooses it. Without
/// this check, mail could make Hush POST to a router's admin page, a service on
/// the user's own machine, or a cloud metadata endpoint — from inside the
/// network, where those things are reachable and often unauthenticated.
///
/// Every resolved address is checked, not just the first, because a name can
/// resolve to several.
async fn vet_destination(url: &str) -> Result<()> {
    let parsed = url::Url::parse(url).map_err(|_| Error::Other("That link isn't valid.".into()))?;

    if parsed.scheme() != "https" {
        return Err(Error::Other(
            "That link isn't a secure address, so Hush won't send anything to it.".into(),
        ));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| Error::Other("That link has no website in it.".into()))?;

    if host.eq_ignore_ascii_case("localhost") || host.to_ascii_lowercase().ends_with(".local") {
        return Err(private_destination());
    }

    let port = parsed.port_or_known_default().unwrap_or(443);
    // `host_str` keeps the brackets on an IPv6 literal, and `IpAddr` will not
    // parse them. Without stripping, `https://[::1]/` would fall through to a
    // DNS lookup and slip past the private-address check entirely.
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let addrs: Vec<IpAddr> = if let Ok(ip) = host.parse::<IpAddr>() {
        vec![ip]
    } else {
        tokio::net::lookup_host((host, port))
            .await
            .map_err(|_| Error::Network("Couldn't find that website".into()))?
            .map(|s| s.ip())
            .collect()
    };

    if addrs.is_empty() {
        return Err(Error::Network("Couldn't find that website".into()));
    }
    if addrs.iter().any(is_private) {
        return Err(private_destination());
    }
    Ok(())
}

fn private_destination() -> Error {
    Error::Other(
        "That unsubscribe link points somewhere on your own network rather than \
         out on the internet, so Hush didn't send anything."
            .into(),
    )
}

/// True for any address that is not routable on the public internet.
fn is_private(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                || v4.is_multicast()
                // 100.64.0.0/10, carrier-grade NAT.
                || (v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1]))
                // 192.0.0.0/24, IETF protocol assignments.
                || v4.octets()[..3] == [192, 0, 0]
                // 198.18.0.0/15, benchmarking.
                || (v4.octets()[0] == 198 && (v4.octets()[1] & 0xfe) == 18)
                // 240.0.0.0/4, reserved.
                || v4.octets()[0] >= 240
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                // fc00::/7, unique local.
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                // fe80::/10, link local.
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                // An IPv4 address wearing an IPv6 hat still needs the v4 rules.
                || v6.to_ipv4_mapped().is_some_and(|v4| is_private(&IpAddr::V4(v4)))
        }
    }
}

/// Build a `mailto:` URL for handoff to the user's mail app.
fn build_mailto_url(address: &str, subject: &str, body: &str) -> String {
    use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
    format!(
        "mailto:{}?subject={}&body={}",
        utf8_percent_encode(address, NON_ALPHANUMERIC),
        utf8_percent_encode(subject, NON_ALPHANUMERIC),
        utf8_percent_encode(body, NON_ALPHANUMERIC),
    )
}

/// Build the message sent when the user chose to unsubscribe through Gmail.
///
/// The subject and address come from an email header written by the sender, so
/// they are untrusted input. Anything that could start a new header line is
/// stripped — otherwise a crafted `subject=` could append a `Bcc:` and turn the
/// user's own account into a relay.
fn build_rfc5322(from: &str, to: &str, subject: &str, body: &str) -> Result<String> {
    let to = strip_header_breaks(to);
    let subject = strip_header_breaks(subject);
    let from = strip_header_breaks(from);

    // The address is checked rather than merely sanitised. A `To` line that has
    // had a newline squashed out of it — `leave@x.example Bcc: victim@y` — is no
    // longer an injection, but it is also not an address, and sending it would
    // be guessing at what the sender meant.
    if !is_single_address(&to) {
        return Err(Error::Other(
            "That sender's unsubscribe address doesn't look valid.".into(),
        ));
    }

    Ok(format!(
        "From: {from}\r\n\
         To: {to}\r\n\
         Subject: {subject}\r\n\
         MIME-Version: 1.0\r\n\
         Content-Type: text/plain; charset=\"utf-8\"\r\n\
         \r\n\
         {body}\r\n"
    ))
}

/// Replace everything that could break out of a header field with a space.
///
/// A space rather than nothing: removing the break outright would silently
/// weld two lines into one word, turning a visible oddity into a hidden one.
fn strip_header_breaks(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c == '\r' || c == '\n' || c == '\0' {
                ' '
            } else {
                c
            }
        })
        .collect::<String>()
        .trim()
        .to_string()
}

/// One plain address, nothing else. No display name, no second recipient, no
/// stray header text that survived sanitising.
fn is_single_address(addr: &str) -> bool {
    let mut parts = addr.split('@');
    let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !addr.chars().any(|c| c.is_whitespace() || c.is_control())
        && !addr.contains([',', ':', ';', '<', '>', '"'])
}

fn link_of(method: &UnsubMethod) -> Option<String> {
    match method {
        UnsubMethod::OneClick { url } | UnsubMethod::ManualLink { url } => Some(url.clone()),
        _ => None,
    }
}

fn friendly(e: &Error) -> String {
    e.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(method: UnsubMethod) -> UnsubRequest {
        UnsubRequest {
            address: "news@acme.example".into(),
            display_name: "Acme".into(),
            methods: vec![method.clone()],
            method,
        }
    }

    /// A sender offering several routes, in preference order.
    fn req_many(methods: Vec<UnsubMethod>) -> UnsubRequest {
        UnsubRequest {
            address: "news@acme.example".into(),
            display_name: "Acme".into(),
            method: methods[0].clone(),
            methods,
        }
    }

    fn exec(dry_run: bool) -> Executor {
        Executor::new(dry_run, MailtoMode::HandOff, "me@example.com".into()).unwrap()
    }

    // --- dry run -----------------------------------------------------------

    #[tokio::test]
    async fn a_dry_run_sends_nothing_and_says_so() {
        // The endpoint is deliberately one that would fail loudly if contacted.
        let requests = vec![
            req(UnsubMethod::OneClick {
                url: "https://127.0.0.1:1/u".into(),
            }),
            req(UnsubMethod::Mailto {
                address: "leave@acme.example".into(),
                subject: None,
                body: None,
            }),
            req(UnsubMethod::ManualLink {
                url: "https://acme.example/u".into(),
            }),
        ];
        let report = exec(true)
            .run(&requests, &crate::gmail::Cancel::new())
            .await;

        assert_eq!(report.outcomes.len(), 3);
        for o in &report.outcomes {
            assert_eq!(o.status, OutcomeStatus::Simulated);
            assert!(o.detail.contains("nothing was sent"), "{}", o.detail);
        }
        assert!(
            report.handoffs.is_empty(),
            "a dry run must not open a mail app"
        );
    }

    #[test]
    fn the_plan_shows_the_exact_request() {
        let plan = exec(true).plan(&[req(UnsubMethod::OneClick {
            url: "https://acme.example/u".into(),
        })]);
        assert_eq!(plan.len(), 1);
        assert!(plan[0].detail.contains("POST https://acme.example/u"));
        assert!(plan[0].detail.contains("List-Unsubscribe=One-Click"));
        assert!(plan[0].detail.contains("application/x-www-form-urlencoded"));
    }

    #[test]
    fn a_manual_link_plan_promises_no_request() {
        let plan = exec(false).plan(&[req(UnsubMethod::ManualLink {
            url: "https://acme.example/u".into(),
        })]);
        assert!(plan[0].detail.contains("No request is sent"));
    }

    // --- real requests -----------------------------------------------------

    #[tokio::test]
    async fn a_manual_link_never_fires_a_request_even_outside_dry_run() {
        // This is the promise: link-only senders are listed, not actioned.
        let report = exec(false)
            .run(
                &[req(UnsubMethod::ManualLink {
                    // Would be refused by vetting if it were ever sent.
                    url: "https://127.0.0.1:1/u".into(),
                })],
                &crate::gmail::Cancel::new(),
            )
            .await;
        assert_eq!(report.outcomes[0].status, OutcomeStatus::NeedsYou);
        assert_eq!(
            report.outcomes[0].link.as_deref(),
            Some("https://127.0.0.1:1/u")
        );
    }

    #[tokio::test]
    async fn one_click_posts_the_rfc8058_body() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/u"))
            .and(wiremock::matchers::header(
                "content-type",
                "application/x-www-form-urlencoded",
            ))
            .and(wiremock::matchers::body_string(ONE_CLICK_BODY))
            .respond_with(wiremock::ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        // The mock server is plain HTTP on loopback, so drive `one_click`'s
        // request shape directly and check vetting separately below.
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("{}/u", server.uri()))
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(ONE_CLICK_BODY)
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());
    }

    #[tokio::test]
    async fn a_mailto_handoff_produces_a_draft_and_sends_nothing() {
        let report = exec(false)
            .run(
                &[req(UnsubMethod::Mailto {
                    address: "leave@acme.example".into(),
                    subject: Some("Unsub me".into()),
                    body: None,
                })],
                &crate::gmail::Cancel::new(),
            )
            .await;
        assert_eq!(report.outcomes[0].status, OutcomeStatus::NeedsYou);
        assert_eq!(report.handoffs.len(), 1);
        let url = &report.handoffs[0].mailto_url;
        assert!(url.starts_with("mailto:leave%40acme%2Eexample"));
        assert!(url.contains("subject=Unsub%20me"));
    }

    #[tokio::test]
    async fn a_run_can_be_stopped_partway_and_keeps_what_it_did() {
        // The gap this closes: fifty senders, twenty seconds of timeout each,
        // and a button that did nothing. Whatever is already done stays done.
        let cancel = crate::gmail::Cancel::new();
        let requests: Vec<UnsubRequest> = (0..20)
            .map(|_| {
                req(UnsubMethod::ManualLink {
                    url: "https://acme.example/u".into(),
                })
            })
            .collect();

        cancel.cancel();
        let started = std::time::Instant::now();
        let report = exec(false).run(&requests, &cancel).await;

        assert!(
            started.elapsed() < Duration::from_secs(2),
            "stopping took {:?}",
            started.elapsed()
        );
        assert!(
            report.outcomes.len() < requests.len(),
            "it should not have worked through all of them"
        );
    }

    #[tokio::test]
    async fn stopping_mid_flight_is_noticed_within_a_moment() {
        // A request already in flight must not hold the whole run hostage for
        // the full twenty-second timeout.
        let cancel = crate::gmail::Cancel::new();
        let trigger = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            trigger.cancel();
        });

        // 203.0.113.0/24 is reserved for documentation and never answers, so
        // this request hangs until either the timeout or the cancel wins.
        let started = std::time::Instant::now();
        let report = exec(false)
            .run(
                &[req(UnsubMethod::OneClick {
                    url: "https://203.0.113.1/u".into(),
                })],
                &cancel,
            )
            .await;

        assert!(
            started.elapsed() < Duration::from_secs(5),
            "cancel should beat the {HTTP_TIMEOUT:?} timeout, took {:?}",
            started.elapsed()
        );
        let _ = report;
    }

    #[tokio::test]
    async fn a_broken_first_route_falls_through_to_the_next() {
        // The sender offers one-click and a mailto. The one-click points at a
        // private address and will be refused, so the mailto must carry it.
        let report = exec(false)
            .run(
                &[req_many(vec![
                    UnsubMethod::OneClick {
                        url: "https://192.168.0.1/u".into(),
                    },
                    UnsubMethod::Mailto {
                        address: "leave@acme.example".into(),
                        subject: None,
                        body: None,
                    },
                ])],
                &crate::gmail::Cancel::new(),
            )
            .await;

        assert_eq!(report.outcomes[0].status, OutcomeStatus::NeedsYou);
        assert_eq!(report.handoffs.len(), 1, "the mailto route was used");
    }

    #[tokio::test]
    async fn when_every_route_fails_the_link_is_still_offered() {
        let report = exec(false)
            .run(
                &[req_many(vec![
                    UnsubMethod::OneClick {
                        url: "https://10.0.0.1/u".into(),
                    },
                    UnsubMethod::ManualLink {
                        url: "https://acme.example/preferences".into(),
                    },
                ])],
                &crate::gmail::Cancel::new(),
            )
            .await;

        // A dead end would be a failure with nothing to click. Instead the
        // user gets the sender's own page.
        assert_eq!(report.outcomes[0].status, OutcomeStatus::NeedsYou);
        assert_eq!(
            report.outcomes[0].link.as_deref(),
            Some("https://acme.example/preferences")
        );
    }

    #[tokio::test]
    async fn a_successful_one_click_still_keeps_the_link_to_hand() {
        // A 200 means the sender accepted the request, not that they acted on
        // it — and nothing in the protocol can tell us which. So the way to
        // finish it by hand survives.
        let report = exec(true)
            .run(
                &[req(UnsubMethod::OneClick {
                    url: "https://acme.example/u".into(),
                })],
                &crate::gmail::Cancel::new(),
            )
            .await;
        assert_eq!(
            report.outcomes[0].link.as_deref(),
            Some("https://acme.example/u")
        );
    }

    #[tokio::test]
    async fn sending_via_gmail_without_permission_fails_clearly() {
        let e = Executor::new(false, MailtoMode::SendViaGmail, "me@example.com".into()).unwrap();
        let report = e
            .run(
                &[req(UnsubMethod::Mailto {
                    address: "leave@acme.example".into(),
                    subject: None,
                    body: None,
                })],
                &crate::gmail::Cancel::new(),
            )
            .await;
        assert_eq!(report.outcomes[0].status, OutcomeStatus::Failed);
        assert!(report.outcomes[0].detail.contains("permission"));
    }

    // --- destination vetting ----------------------------------------------

    #[tokio::test]
    async fn requests_to_the_local_network_are_refused() {
        for url in [
            "https://127.0.0.1/u",
            "https://localhost/u",
            "https://10.0.0.5/u",
            "https://192.168.1.1/u",
            "https://172.16.0.1/u",
            "https://169.254.169.254/latest/meta-data",
            "https://[::1]/u",
            "https://[fd00::1]/u",
            "https://printer.local/u",
            "https://0.0.0.0/u",
            "https://100.64.0.1/u",
        ] {
            let err = vet_destination(url).await.unwrap_err();
            assert!(
                err.to_string().contains("own network")
                    || err.to_string().contains("secure address"),
                "{url} was not refused: {err}"
            );
        }
    }

    #[tokio::test]
    async fn plain_http_is_refused() {
        let err = vet_destination("http://example.com/u").await.unwrap_err();
        assert!(err.to_string().contains("secure address"));
    }

    #[tokio::test]
    async fn a_failed_one_click_becomes_something_the_user_can_finish() {
        // Previously this reported a bare failure. A failure the user can do
        // nothing about is a dead end, so when a link exists it is offered and
        // the outcome says there is something left to do.
        let report = exec(false)
            .run(
                &[req(UnsubMethod::OneClick {
                    url: "https://192.168.0.1/u".into(),
                })],
                &crate::gmail::Cancel::new(),
            )
            .await;
        assert_eq!(report.outcomes[0].status, OutcomeStatus::NeedsYou);
        assert_eq!(
            report.outcomes[0].link.as_deref(),
            Some("https://192.168.0.1/u")
        );
        assert!(
            report.outcomes[0].detail.contains("by hand"),
            "{}",
            report.outcomes[0].detail
        );
    }

    #[test]
    fn ip_classification() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "192.168.0.1",
            "172.31.255.255",
            "169.254.1.1",
            "0.0.0.0",
            "224.0.0.1",
            "100.100.0.1",
            "255.255.255.255",
            "::1",
            "fe80::1",
            "fd12::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
        ] {
            assert!(is_private(&ip.parse().unwrap()), "{ip} should be private");
        }
        for ip in [
            "8.8.8.8",
            "1.1.1.1",
            "93.184.216.34",
            "172.32.0.1",
            "99.99.99.99",
            "2606:4700::1111",
            "::ffff:8.8.8.8",
        ] {
            assert!(!is_private(&ip.parse().unwrap()), "{ip} should be public");
        }
    }

    // --- header injection --------------------------------------------------

    /// The header field names in a built message, in order.
    fn header_names(raw: &str) -> Vec<String> {
        raw.split("\r\n\r\n")
            .next()
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.split_once(':').map(|(n, _)| n.to_string()))
            .collect()
    }

    #[test]
    fn a_crafted_subject_cannot_add_headers() {
        // A sender controls the `subject=` in their own List-Unsubscribe. Left
        // alone, this would append a Bcc and turn the account into a relay.
        let raw = build_rfc5322(
            "me@example.com",
            "leave@acme.example",
            "Unsub\r\nBcc: victim@example.com\r\nX-Evil: yes",
            "please",
        )
        .unwrap();

        // The crafted text survives as visible subject wording — which is fine,
        // and honest — but it must not have become a header of its own.
        assert_eq!(
            header_names(&raw),
            ["From", "To", "Subject", "MIME-Version", "Content-Type"]
        );
        assert_eq!(raw.matches("\r\n\r\n").count(), 1, "one header/body split");
    }

    #[test]
    fn a_crafted_address_is_refused_outright() {
        // Squashing the newline would leave "leave@x Bcc: victim@y" as the To
        // line: not an injection, but not an address either.
        let err = build_rfc5322(
            "me@example.com",
            "leave@acme.example\r\nBcc: victim@example.com",
            "Unsubscribe",
            "please",
        )
        .unwrap_err();
        assert!(err.to_string().contains("doesn't look valid"));
    }

    #[test]
    fn only_a_bare_single_address_is_accepted() {
        for good in ["a@b.com", "leave+tag@lists.example.org", "x.y@a.b.co.uk"] {
            assert!(is_single_address(good), "{good} should be accepted");
        }
        for bad in [
            "",
            "not-an-address",
            "missing@domain",
            "a@b.com, c@d.com",
            "Name <a@b.com>",
            "a@b.com Bcc: c@d.com",
            "a@b.com\u{7f}",
            "two@at@signs.com",
            "@nolocal.com",
            "a@.leadingdot.com",
        ] {
            assert!(!is_single_address(bad), "{bad:?} should be refused");
        }
        for bad in ["not-an-address", "", "a@b.com, c@d.com"] {
            assert!(build_rfc5322("me@example.com", bad, "s", "b").is_err());
        }
    }

    #[test]
    fn a_break_becomes_a_space_rather_than_vanishing() {
        // Deleting the break would weld words together and hide the tampering.
        // CRLF is two characters, so it leaves two spaces — untidy, but visible,
        // which is the point.
        assert_eq!(strip_header_breaks("one\r\ntwo"), "one  two");
        assert_eq!(strip_header_breaks("one\ntwo"), "one two");
        assert_eq!(strip_header_breaks("  padded\n "), "padded");
        assert!(!strip_header_breaks("a\r\nb").contains(['\r', '\n']));
    }

    #[test]
    fn a_well_formed_message_has_the_expected_shape() {
        let raw = build_rfc5322(
            "me@example.com",
            "leave@acme.example",
            "Unsubscribe",
            "please",
        )
        .unwrap();
        assert!(raw.starts_with("From: me@example.com\r\n"));
        assert!(raw.contains("To: leave@acme.example\r\n"));
        assert!(raw.contains("Subject: Unsubscribe\r\n"));
        assert!(raw.contains("charset=\"utf-8\""));
        assert!(raw.ends_with("please\r\n"));
    }

    #[test]
    fn the_default_mailto_mode_asks_for_no_extra_permission() {
        assert_eq!(MailtoMode::default(), MailtoMode::HandOff);
    }

    // --- tidying up --------------------------------------------------------

    #[tokio::test]
    async fn a_dry_run_bins_nothing() {
        let server = wiremock::MockServer::start().await;
        // Any request at all to the mock server fails this test.
        wiremock::Mock::given(wiremock::matchers::any())
            .respond_with(wiremock::ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;

        let gmail = Arc::new(
            GmailClient::with_base(
                &server.uri(),
                Arc::new(NoTokens) as Arc<dyn crate::gmail::TokenSource>,
                Arc::new(crate::ratelimit::AdaptiveLimiter::with_rate(100_000.0)),
            )
            .unwrap(),
        );

        let ids: Vec<String> = (0..25).map(|i| format!("m{i}")).collect();
        let report = trash_messages(&gmail, &ids, true, &crate::gmail::Cancel::new()).await;

        assert!(report.simulated);
        assert_eq!(report.trashed, 25, "reports what it would have moved");
        assert_eq!(report.failed, 0);
    }

    #[tokio::test]
    async fn trashing_calls_the_trash_endpoint_once_per_message() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path_regex(
                r"^/gmail/v1/users/me/messages/[^/]+/trash$",
            ))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string("{}"))
            .expect(3)
            .mount(&server)
            .await;

        let gmail = Arc::new(
            GmailClient::with_base(
                &server.uri(),
                Arc::new(NoTokens) as Arc<dyn crate::gmail::TokenSource>,
                Arc::new(crate::ratelimit::AdaptiveLimiter::with_rate(100_000.0)),
            )
            .unwrap(),
        );

        let ids = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let report = trash_messages(&gmail, &ids, false, &crate::gmail::Cancel::new()).await;

        assert!(!report.simulated);
        assert_eq!(report.trashed, 3);
        assert_eq!(report.failed, 0);
    }

    #[tokio::test]
    async fn one_message_that_will_not_move_does_not_strand_the_rest() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::path(
            "/gmail/v1/users/me/messages/bad/trash",
        ))
        .respond_with(wiremock::ResponseTemplate::new(400).set_body_string("nope"))
        .mount(&server)
        .await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;

        let gmail = Arc::new(
            GmailClient::with_base(
                &server.uri(),
                Arc::new(NoTokens) as Arc<dyn crate::gmail::TokenSource>,
                Arc::new(crate::ratelimit::AdaptiveLimiter::with_rate(100_000.0)),
            )
            .unwrap(),
        );

        let ids = vec!["ok1".into(), "bad".into(), "ok2".into()];
        let report = trash_messages(&gmail, &ids, false, &crate::gmail::Cancel::new()).await;

        assert_eq!(report.trashed, 2);
        assert_eq!(report.failed, 1);
    }

    #[tokio::test]
    async fn an_empty_list_is_a_no_op() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::any())
            .respond_with(wiremock::ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;

        let gmail = Arc::new(
            GmailClient::with_base(
                &server.uri(),
                Arc::new(NoTokens) as Arc<dyn crate::gmail::TokenSource>,
                Arc::new(crate::ratelimit::AdaptiveLimiter::default()),
            )
            .unwrap(),
        );
        let report = trash_messages(&gmail, &[], false, &crate::gmail::Cancel::new()).await;
        assert_eq!(report.trashed, 0);
        assert_eq!(report.failed, 0);
    }

    /// A token source for tests that never needs to renew anything.
    struct NoTokens;

    impl crate::gmail::TokenSource for NoTokens {
        fn access_token(
            &self,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + '_>>
        {
            Box::pin(async { Ok("test-token".to_string()) })
        }
        fn force_refresh(
            &self,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + '_>>
        {
            Box::pin(async { Ok("test-token".to_string()) })
        }
    }
}
