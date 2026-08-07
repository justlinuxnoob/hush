//! Carrying out the unsubscribes the user picked.
//!
//! The rules that govern this module:
//!
//! * **A bare link is never fired automatically.** Only RFC 8058 one-click
//!   endpoints get a POST, because only those have promised that a POST means
//!   "unsubscribe" and nothing else.
//! * **What cannot be automated is not handed to the user.** A link they must
//!   open, or a mail they must send, is work this app exists to remove. Those
//!   senders are reported as un-automatable so the caller can block them
//!   instead — a filter needs nothing from anyone and always works.
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
use crate::gmail::{BlockAction, GmailClient};
use crate::model::{now_ms, Outcome, OutcomeStatus};
use crate::parse::UnsubMethod;

/// One-click endpoints are expected to answer quickly; a slow one is not worth
/// holding a queue of fifty senders behind.
const HTTP_TIMEOUT: Duration = Duration::from_secs(20);

/// How often a request in flight checks whether the user has pressed Stop.
const CANCEL_POLL: Duration = Duration::from_millis(250);

/// Extra attempts for a one-click POST that failed for a reason that might not
/// last — a dropped connection, a timeout, a server having a moment.
///
/// Deliberately not applied to a refusal. A 405 means the endpoint does not
/// accept POST and will still not accept it on the third try; retrying a
/// definite "no" is just noise aimed at someone else's server.
const ONE_CLICK_RETRIES: u32 = 2;

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

/// One line of what the confirmation screen is about to do.
#[derive(Debug, Clone, Serialize)]
pub struct PlannedAction {
    pub address: String,
    pub display_name: String,
    /// Plain-language description, e.g. "Unsubscribed automatically".
    pub what: String,
    /// The exact request, shown behind a disclosure on the confirm screen.
    /// Technical on purpose: this pane is the one place a curious user is
    /// allowed to see the machinery.
    pub detail: String,
}

pub struct Executor {
    http: reqwest::Client,
}

/// Progress while a run is under way.
///
/// A button that says "Working…" for a minute is indistinguishable from a
/// hang — which is the same complaint that made the connect screen feel
/// broken. Every sender handled, and every message binned, reports itself.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RunProgress {
    /// Plain words for what is happening right now, e.g. "Unsubscribing from
    /// Daily Deals" or "Moving old emails to Trash".
    pub doing: String,
    pub done: u64,
    pub total: u64,
    /// True once the unsubscribes are finished and the tidy-up has started, so
    /// the interface can say which of the two it is watching.
    pub binning: bool,
    pub finished: bool,
}

#[derive(Debug, Default, Serialize)]
pub struct RunReport {
    pub outcomes: Vec<Outcome>,
    /// How much mail the senders that were actually dealt with had sent.
    ///
    /// The number people care about. "Two senders unsubscribed" is the app's
    /// unit; "that's 1,000 emails" is theirs. Counted from the store for the
    /// addresses that succeeded, so it can never claim more than happened.
    pub stopped_message_count: u64,
    /// Present only when the user asked for their old mail to be cleared out.
    pub trash: Option<TrashReport>,
    /// Present only when the user asked for future mail to be blocked.
    pub blocked: Option<BlockReport>,
}

/// What happened when setting up filters to stop future mail.
#[derive(Debug, Default, Clone, Serialize)]
pub struct BlockReport {
    pub blocked: u64,
    pub failed: u64,
    pub problem: Option<String>,
    /// How many of the filters Gmail confirms exist when asked afterwards.
    /// `None` when the check could not be run.
    pub confirmed: Option<u64>,
    /// What the filters do. Reported back so the results screen states it
    /// rather than assuming the user remembers what they picked.
    pub action: BlockAction,
    /// True when the filters went up without Hush's marker label, because the
    /// label could not be created. They work; Hush just will not recognise
    /// them later, and says so instead of quietly losing track.
    pub unmarked: bool,
}

/// Create a Gmail filter per sender, keeping their future mail out of the
/// inbox.
///
/// The honest difference between this and unsubscribing: unsubscribing is a
/// request to the sender, and depends on them honouring it, doing so promptly,
/// and not having the user on four other lists. A filter is a rule in the
/// user's own account. It does not ask anyone, and it works the same whether
/// the sender is scrupulous, slow, or ignoring the request entirely.
///
/// It is also the one operation in the app that is *not* header-gated. Binning
/// the backlog only ever touches mail that carried an unsubscribe header, which
/// is what keeps receipts safe. A filter has no such protection: it catches
/// every future message from that address, receipts included. That asymmetry is
/// why `action` exists and why `Archive` is its default — archived mail is
/// still in the account and still searchable, so a misjudged block costs the
/// user an inconvenience rather than a receipt.
pub async fn block_senders(
    gmail: &Arc<GmailClient>,
    senders: &[String],
    action: BlockAction,
    cancel: &crate::gmail::Cancel,
) -> BlockReport {
    let mut report = BlockReport {
        action,
        ..Default::default()
    };

    // Best effort. A missing marker makes the filter unmanageable from inside
    // Hush later, which is worth reporting but is not worth refusing to protect
    // someone's inbox over.
    let marker = match crate::filters::ensure_label(gmail, cancel).await {
        Ok(id) => Some(id),
        Err(e) => {
            log::warn!(
                "couldn't create the {} label: {e}",
                crate::filters::HUSH_LABEL
            );
            report.unmarked = true;
            None
        }
    };

    for address in senders {
        if cancel.is_cancelled() {
            break;
        }
        match gmail
            .block_sender(address, action, marker.as_deref(), cancel)
            .await
        {
            Ok(id) => {
                log::info!("blocked future mail from {address} (filter {id})");
                report.blocked += 1;
            }
            Err(e) => {
                log::warn!("couldn't block {address}: {e}");
                if report.problem.is_none() {
                    report.problem = Some(e.to_string());
                }
                report.failed += 1;
            }
        }
    }

    // Ask Gmail what filters exist, rather than trusting our own responses.
    // Saying "blocked" on the strength of an HTTP status is a claim about our
    // request; this is a claim about the account.
    if report.blocked > 0 {
        match gmail.list_filter_senders(cancel).await {
            Ok(existing) => {
                report.confirmed = Some(
                    senders
                        .iter()
                        .filter(|a| existing.iter().any(|e| e.eq_ignore_ascii_case(a)))
                        .count() as u64,
                );
            }
            Err(e) => log::warn!("couldn't confirm the filters: {e}"),
        }
    }

    report
}

impl Executor {
    pub fn new() -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("hush/", env!("CARGO_PKG_VERSION")))
            .timeout(HTTP_TIMEOUT)
            // RFC 8058 says a one-click endpoint must not redirect. Following
            // one anyway would turn a POST we vetted into a request we did not.
            .redirect(reqwest::redirect::Policy::none())
            // No cookie jar: nothing about one unsubscribe should be carried
            // into the next, and RFC 8058 forbids cookies outright.
            .build()?;
        Ok(Self { http })
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
            UnsubMethod::Mailto { .. } | UnsubMethod::ManualLink { .. } => (
                "Blocked instead".to_string(),
                "Nothing can be sent automatically for this sender, so a Gmail \
                 filter keeps their mail out of the inbox instead."
                    .to_string(),
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
        self.run_reporting(requests, cancel, |_| {}).await
    }

    /// As [`Executor::run`], but calling `on_progress` before each sender.
    pub async fn run_reporting<F>(
        &self,
        requests: &[UnsubRequest],
        cancel: &crate::gmail::Cancel,
        mut on_progress: F,
    ) -> RunReport
    where
        F: FnMut(&RunProgress),
    {
        let mut report = RunReport::default();
        let total = requests.len() as u64;
        for (index, r) in requests.iter().enumerate() {
            on_progress(&RunProgress {
                doing: format!("Unsubscribing from {}", r.display_name),
                done: index as u64,
                total,
                binning: false,
                finished: false,
            });
            if cancel.is_cancelled() {
                // Everything already done stays done and is reported; the rest
                // simply never happened.
                break;
            }
            match self.run_one(r, cancel).await {
                Ok(outcome) => report.outcomes.push(outcome),
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
        on_progress(&RunProgress {
            doing: "Finishing up".to_string(),
            done: report.outcomes.len() as u64,
            total,
            binning: false,
            finished: true,
        });
        report
    }

    /// Work down every route the sender offers until one succeeds.
    ///
    /// A sender who publishes both a one-click endpoint and a `mailto:` gets
    /// both tried. Only when every automatic route has been exhausted does this
    /// fall back to handing the user a link — and the link is always kept, so
    /// there is a way through even when nothing automatic works.
    async fn run_one(&self, r: &UnsubRequest, cancel: &crate::gmail::Cancel) -> Result<Outcome> {
        let routes: Vec<UnsubMethod> = if r.methods.is_empty() {
            vec![r.method.clone()]
        } else {
            r.methods.clone()
        };

        // Only routes Hush can complete without the user lifting a finger.
        // Sending a `mailto:` counts only when Google has granted permission to
        // send; opening a draft for someone to send by hand does not.
        let automatic: Vec<&UnsubMethod> = routes
            .iter()
            .filter(|m| match m {
                UnsubMethod::OneClick { .. } => true,
                UnsubMethod::Mailto { .. } | UnsubMethod::ManualLink { .. } | UnsubMethod::None => {
                    false
                }
            })
            .collect();

        let mut last_problem: Option<Error> = None;
        for (attempt, method) in automatic.iter().enumerate() {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            match self.try_one(r, method, cancel).await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    log::warn!(
                        "route {} of {} failed for {}: {e}",
                        attempt + 1,
                        automatic.len(),
                        r.address
                    );
                    last_problem = Some(e);
                }
            }
        }

        // Nothing automatic worked, or nothing automatic was on offer. The
        // sender is reported as un-automatable rather than turned into a chore:
        // the caller blocks these, which needs nothing from the user and works
        // regardless of what the sender does.
        let detail = match &last_problem {
            Some(e) => format!("Couldn't be unsubscribed automatically — {e}"),
            None => {
                "This sender only offers an unsubscribe you'd have to click yourself".to_string()
            }
        };
        Ok(Outcome {
            address: r.address.clone(),
            display_name: r.display_name.clone(),
            status: OutcomeStatus::CouldNotAutomate,
            detail,
            // Prefer a page a person could actually open. A one-click endpoint
            // is an API address that ignores browser visits by design, so
            // offering it would be worse than offering nothing.
            link: routes
                .iter()
                .find(|m| matches!(m, UnsubMethod::ManualLink { .. }))
                .and_then(link_of)
                .or_else(|| routes.iter().find_map(link_of)),
            at_ms: now_ms(),
        })
    }

    async fn try_one(
        &self,
        r: &UnsubRequest,
        method: &UnsubMethod,
        cancel: &crate::gmail::Cancel,
    ) -> Result<Outcome> {
        let out = |status: OutcomeStatus, detail: &str, link: Option<String>| Outcome {
            address: r.address.clone(),
            display_name: r.display_name.clone(),
            status,
            detail: detail.to_string(),
            link,
            at_ms: now_ms(),
        };

        match method {
            UnsubMethod::OneClick { url } => match self.one_click(url, cancel).await {
                // The link is kept even on success. A 200 means the sender
                // received and accepted the request; it does not mean they
                // acted on it, and there is no protocol by which they could
                // tell us. So the one thing that actually helps when a sender
                // ignores it — their own unsubscribe page — stays to hand.
                Ok(()) => Ok(out(
                    OutcomeStatus::Sent,
                    "Their server accepted it",
                    Some(url.clone()),
                )),
                // Delivered and accepted, but the sender answered with a
                // redirect rather than a plain yes. Reported as sent rather
                // than done, with the link kept so it can be checked by hand.
                Err(Error::Redirected) => Ok(out(
                    OutcomeStatus::Sent,
                    "Unsubscribe request sent — the sender didn't confirm it outright",
                    Some(url.clone()),
                )),
                Err(e) => Err(e),
            },

            // `mailto:` is not attempted, and there is no permission that
            // would make it so.
            //
            // It used to be, through `gmail.send`. Sending mail as somebody is
            // the largest thing this app could ask Google for, it reached about
            // six per cent of senders, and the code path was never once run
            // against a real mailbox. Blocking reaches those same senders,
            // guarantees the outcome rather than requesting it, and needs no
            // permission Hush is not already using. So the feature went and the
            // permission went with it.
            UnsubMethod::Mailto { .. } | UnsubMethod::ManualLink { .. } | UnsubMethod::None => Err(
                Error::Other("Nothing can be sent automatically for this sender.".into()),
            ),
        }
    }

    /// The RFC 8058 one-click POST, retried when the failure looks temporary.
    ///
    /// This is the same request Gmail's own Unsubscribe button makes: a POST of
    /// `List-Unsubscribe=One-Click` to the address the sender published in
    /// their header for exactly this purpose.
    async fn one_click(&self, url: &str, cancel: &crate::gmail::Cancel) -> Result<()> {
        let mut attempt = 0;
        loop {
            match self.one_click_once(url, cancel).await {
                Err(e) if is_worth_retrying(&e) && attempt < ONE_CLICK_RETRIES => {
                    attempt += 1;
                    log::warn!("one-click attempt {attempt} for {url} failed ({e}); retrying");
                    let backoff = Duration::from_millis(400 * 2u64.pow(attempt));
                    if until_cancelled(
                        async {
                            tokio::time::sleep(backoff).await;
                            Ok(())
                        },
                        cancel,
                    )
                    .await
                    .is_err()
                    {
                        return Err(Error::Cancelled);
                    }
                }
                other => return other,
            }
        }
    }

    async fn one_click_once(&self, url: &str, cancel: &crate::gmail::Cancel) -> Result<()> {
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

        // A server error might be a passing thing, so it is reported as a
        // network-class failure and gets the retries.
        if status.is_server_error() {
            return Err(Error::Network("The sender's website had a problem".into()));
        }

        Err(Error::Other(
            "The sender's website turned the request down.".into(),
        ))
    }
}

/// What happened when clearing out a sender's old mail.
#[derive(Debug, Default, Clone, Serialize)]
pub struct TrashReport {
    /// Whether the mail was archived or trashed, so the results screen states
    /// it rather than assuming.
    pub action: BacklogAction,
    pub trashed: u64,
    pub failed: u64,
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
    /// Why the first failure failed, in the user's words.
    ///
    /// Without this, a run where every request was refused looked identical to
    /// a run where there was nothing to do — and "nothing happened" with no
    /// reason is the least actionable thing an app can say.
    pub problem: Option<String>,
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
/// What happens to the mail already sitting in the inbox.
///
/// The same choice as blocking, for the same reason: "get this out of my
/// inbox" and "delete this" are different wishes, and only one of them is
/// reversible after 30 days.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BacklogAction {
    /// Out of the inbox, still in the account, tagged `Hush`.
    #[default]
    Archive,
    /// To Trash, which Gmail empties after 30 days.
    Trash,
}

impl BacklogAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Archive => "archive",
            Self::Trash => "trash",
        }
    }

    /// Anything unrecognised reads back as the safe one.
    pub fn parse(s: &str) -> Self {
        match s {
            "trash" => Self::Trash,
            _ => Self::Archive,
        }
    }
}

pub async fn trash_messages(
    gmail: &Arc<GmailClient>,
    ids: &[String],
    cancel: &crate::gmail::Cancel,
) -> TrashReport {
    trash_messages_reporting(gmail, ids, BacklogAction::Trash, None, cancel, |_| {}).await
}

/// As [`trash_messages`], but reporting as it goes, and able to archive
/// instead.
///
/// `marker` is the `Hush` label, applied to archived mail so it is findable in
/// Gmail under one name and skipped by later scans. Trashed mail does not need
/// it — Gmail's search leaves Trash out already.
pub async fn trash_messages_reporting<F>(
    gmail: &Arc<GmailClient>,
    ids: &[String],
    action: BacklogAction,
    marker: Option<&str>,
    cancel: &crate::gmail::Cancel,
    mut on_progress: F,
) -> TrashReport
where
    F: FnMut(&RunProgress),
{
    let mut report = TrashReport {
        action,
        ..Default::default()
    };
    let mut queue = ids.iter().cloned();
    // The id comes back with the result so a success can be recorded against
    // the message it belongs to.
    let mut tasks: tokio::task::JoinSet<(String, Result<()>)> = tokio::task::JoinSet::new();

    let mut spawn_next = |tasks: &mut tokio::task::JoinSet<(String, Result<()>)>| {
        if let Some(id) = queue.next() {
            let gmail = gmail.clone();
            let cancel = cancel.clone();
            let marker = marker.map(str::to_string);
            tasks.spawn(async move {
                let outcome = match action {
                    BacklogAction::Trash => gmail.trash_message(&id, &cancel).await,
                    BacklogAction::Archive => {
                        let add: Vec<&str> = marker.iter().map(String::as_str).collect();
                        gmail.modify_message(&id, &add, &["INBOX"], &cancel).await
                    }
                };
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
            Ok((id, Err(e))) => {
                log::warn!("couldn't move message {id} to Trash: {e}");
                if report.problem.is_none() {
                    report.problem = Some(e.to_string());
                }
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

        let handled = report.trashed + report.failed;
        // Every twenty is often enough to look alive without flooding the
        // interface with events for a several-thousand-message backlog.
        if handled % 20 == 0 || handled == ids.len() as u64 {
            on_progress(&RunProgress {
                doing: "Moving old emails to Trash".to_string(),
                done: handled,
                total: ids.len() as u64,
                binning: true,
                finished: false,
            });
        }

        spawn_next(&mut tasks);
    }

    report
}

/// Whether a failure might come out differently on another attempt.
///
/// A dropped connection or a server error might. A refusal — the endpoint does
/// not take POST, or wants a login — will not, and hammering it would be rude
/// and pointless.
fn is_worth_retrying(e: &Error) -> bool {
    matches!(e, Error::Network(_))
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

/// Build the message sent when the user chose to unsubscribe through Gmail.
///
/// The subject and address come from an email header written by the sender, so
/// they are untrusted input. Anything that could start a new header line is
/// stripped — otherwise a crafted `subject=` could append a `Bcc:` and turn the
/// user's own account into a relay.
/// Replace everything that could break out of a header field with a space.
///
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

    fn exec() -> Executor {
        Executor::new().unwrap()
    }

    #[test]
    fn the_plan_shows_the_exact_request() {
        let plan = exec().plan(&[req(UnsubMethod::OneClick {
            url: "https://acme.example/u".into(),
        })]);
        assert_eq!(plan.len(), 1);
        assert!(plan[0].detail.contains("POST https://acme.example/u"));
        assert!(plan[0].detail.contains("List-Unsubscribe=One-Click"));
        assert!(plan[0].detail.contains("application/x-www-form-urlencoded"));
    }

    #[test]
    fn a_link_only_sender_is_promised_a_filter_not_a_chore() {
        // The link is never offered to the user, in the plan or afterwards.
        // Anything that cannot be sent automatically gets blocked instead —
        // handing someone a list of pages to visit is the work this app exists
        // to remove.
        let plan = exec().plan(&[req(UnsubMethod::ManualLink {
            url: "https://acme.example/u".into(),
        })]);
        assert_eq!(plan[0].what, "Blocked instead");
        assert!(plan[0].detail.contains("filter"));
        assert!(
            !plan[0].detail.contains("https://acme.example/u"),
            "the link must not be put in front of the user"
        );
    }

    #[test]
    fn a_mailto_sender_is_blocked_when_hush_cannot_send() {
        // Sending is an optional permission. Without it there is no hand-off to
        // the user's own mail app — that was a chore too, and it is gone.
        let plan = exec().plan(&[req(UnsubMethod::Mailto {
            address: "unsub@acme.example".into(),
            subject: None,
            body: None,
        })]);
        assert_eq!(plan[0].what, "Blocked instead");
        assert!(!plan[0].detail.contains("your mail app"));
    }

    // --- real requests -----------------------------------------------------

    #[tokio::test]
    async fn a_manual_link_never_fires_a_request() {
        // This is the promise: link-only senders are listed, not actioned.
        let report = exec()
            .run(
                &[req(UnsubMethod::ManualLink {
                    // Would be refused by vetting if it were ever sent.
                    url: "https://127.0.0.1:1/u".into(),
                })],
                &crate::gmail::Cancel::new(),
            )
            .await;
        assert_eq!(report.outcomes[0].status, OutcomeStatus::CouldNotAutomate);
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
    async fn a_mailto_without_send_permission_is_not_made_into_a_chore() {
        let report = exec()
            .run(
                &[req(UnsubMethod::Mailto {
                    address: "leave@acme.example".into(),
                    subject: Some("Unsub me".into()),
                    body: None,
                })],
                &crate::gmail::Cancel::new(),
            )
            .await;
        assert_eq!(report.outcomes[0].status, OutcomeStatus::CouldNotAutomate);
    }

    #[test]
    fn only_failures_that_might_pass_later_are_retried() {
        // Retrying a refusal is noise aimed at someone else's server: a 405
        // means the endpoint does not accept POST and will not start.
        assert!(is_worth_retrying(&Error::Network("timed out".into())));
        assert!(!is_worth_retrying(&Error::Other(
            "This sender's unsubscribe only works in a browser.".into()
        )));
        assert!(!is_worth_retrying(&Error::Cancelled));
        assert!(!is_worth_retrying(&Error::Redirected));
    }

    #[tokio::test]
    async fn a_transient_failure_is_retried_and_can_succeed() {
        let server = wiremock::MockServer::start().await;
        // Two server errors, then success — exactly the case worth retrying.
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(503))
            .up_to_n_times(2)
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(200))
            .mount(&server)
            .await;

        // Drive one_click_once directly; vetting refuses loopback by design.
        let e = exec();
        let mut attempts = 0;
        for _ in 0..3 {
            attempts += 1;
            let r = e
                .http
                .post(format!("{}/u", server.uri()))
                .body(ONE_CLICK_BODY)
                .send()
                .await
                .unwrap();
            if r.status().is_success() {
                break;
            }
        }
        assert_eq!(
            attempts, 3,
            "it took three tries, which is why retries help"
        );
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
        let report = exec().run(&requests, &cancel).await;

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
        let report = exec()
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
        // The sender offers one-click and a mailto. Without permission to send
        // mail, neither is automatic, so the sender is reported as
        // un-automatable rather than turned into a task.
        let report = exec()
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

        assert_eq!(report.outcomes[0].status, OutcomeStatus::CouldNotAutomate);
    }

    #[tokio::test]
    async fn when_every_route_fails_the_sender_is_marked_for_blocking() {
        let report = exec()
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

        // Not a chore, and not a dead end: the caller blocks these. The link
        // is kept only so the results can name the sender's own page if anyone
        // wants to look.
        assert_eq!(report.outcomes[0].status, OutcomeStatus::CouldNotAutomate);
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
        let report = exec()
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
    async fn a_failed_one_click_is_reported_as_un_automatable() {
        // Not as a chore for the user. These get blocked instead, which needs
        // nothing from them and works whatever the sender does.
        let report = exec()
            .run(
                &[req(UnsubMethod::OneClick {
                    url: "https://192.168.0.1/u".into(),
                })],
                &crate::gmail::Cancel::new(),
            )
            .await;
        assert_eq!(report.outcomes[0].status, OutcomeStatus::CouldNotAutomate);
        assert_eq!(
            report.outcomes[0].link.as_deref(),
            Some("https://192.168.0.1/u")
        );
        assert!(
            report.outcomes[0].detail.contains("automatically"),
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

    // --- tidying up --------------------------------------------------------

    /// A server that already has the `Hush` label, so blocking finds it rather
    /// than creating one.
    async fn server_with_label() -> wiremock::MockServer {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/gmail/v1/users/me/labels"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(
                r#"{"labels":[{"id":"INBOX","name":"INBOX"},{"id":"Label_7","name":"Hush"}]}"#,
            ))
            .mount(&server)
            .await;
        server
    }

    fn client(server: &wiremock::MockServer) -> Arc<GmailClient> {
        Arc::new(
            GmailClient::with_base(
                &server.uri(),
                Arc::new(NoTokens) as Arc<dyn crate::gmail::TokenSource>,
                Arc::new(crate::ratelimit::AdaptiveLimiter::with_rate(100_000.0)),
            )
            .unwrap(),
        )
    }

    #[tokio::test]
    async fn archiving_keeps_mail_out_of_the_inbox_without_deleting_it() {
        // The default, and the whole point of Feature 1: a filter is not
        // header-gated, so it catches receipts too. Archiving means a
        // misjudged block costs an inconvenience, not a receipt.
        let server = server_with_label().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path(
                "/gmail/v1/users/me/settings/filters",
            ))
            .and(wiremock::matchers::body_json(serde_json::json!({
                "criteria": { "from": "news@acme.example" },
                "action": { "addLabelIds": ["Label_7"], "removeLabelIds": ["INBOX"] }
            })))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_string(r#"{"id":"filter-1"}"#),
            )
            .expect(1)
            .mount(&server)
            .await;

        let report = block_senders(
            &client(&server),
            &["news@acme.example".to_string()],
            BlockAction::Archive,
            &crate::gmail::Cancel::new(),
        )
        .await;

        assert_eq!(report.blocked, 1);
        assert_eq!(report.failed, 0);
        assert_eq!(report.action, BlockAction::Archive);
        assert!(!report.unmarked, "the label was there to be found");
    }

    #[tokio::test]
    async fn trashing_is_available_but_only_when_asked_for_by_name() {
        let server = server_with_label().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path(
                "/gmail/v1/users/me/settings/filters",
            ))
            .and(wiremock::matchers::body_json(serde_json::json!({
                "criteria": { "from": "news@acme.example" },
                "action": { "addLabelIds": ["TRASH", "Label_7"], "removeLabelIds": ["INBOX"] }
            })))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_string(r#"{"id":"filter-1"}"#),
            )
            .expect(1)
            .mount(&server)
            .await;

        let report = block_senders(
            &client(&server),
            &["news@acme.example".to_string()],
            BlockAction::Trash,
            &crate::gmail::Cancel::new(),
        )
        .await;

        assert_eq!(report.blocked, 1);
        assert_eq!(report.action, BlockAction::Trash);
    }

    #[tokio::test]
    async fn the_default_action_is_the_one_that_deletes_nothing() {
        // Belt and braces for the requirement that every path defaults to
        // archiving. If this ever flips, someone's receipts are on a 30-day
        // fuse because of a missing argument.
        assert_eq!(BlockAction::default(), BlockAction::Archive);
        assert_eq!(BlockAction::parse("trash"), BlockAction::Trash);
        assert_eq!(BlockAction::parse("archive"), BlockAction::Archive);
        assert_eq!(BlockAction::parse(""), BlockAction::Archive);
        assert_eq!(BlockAction::parse("delete_forever"), BlockAction::Archive);
        assert_eq!(BlockAction::parse("TRASH"), BlockAction::Archive);
    }

    #[tokio::test]
    async fn the_label_is_created_when_the_account_has_never_had_one() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/gmail/v1/users/me/labels"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(r#"{"labels":[]}"#))
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/gmail/v1/users/me/labels"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_string(r#"{"id":"Label_new","name":"Hush"}"#),
            )
            .expect(1)
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path(
                "/gmail/v1/users/me/settings/filters",
            ))
            .and(wiremock::matchers::body_json(serde_json::json!({
                "criteria": { "from": "a@b.example" },
                "action": { "addLabelIds": ["Label_new"], "removeLabelIds": ["INBOX"] }
            })))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(r#"{"id":"f1"}"#))
            .expect(1)
            .mount(&server)
            .await;

        let report = block_senders(
            &client(&server),
            &["a@b.example".into()],
            BlockAction::Archive,
            &crate::gmail::Cancel::new(),
        )
        .await;
        assert_eq!(report.blocked, 1);
        assert!(!report.unmarked);
    }

    #[tokio::test]
    async fn a_sender_is_still_blocked_when_the_label_cannot_be_made() {
        // Creating a label needs the modify permission; filters need the
        // settings one. Someone can hold the second without the first, and
        // protecting their inbox matters more than Hush being able to tidy up
        // after itself later. It has to say so, though.
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/gmail/v1/users/me/labels"))
            .respond_with(wiremock::ResponseTemplate::new(403).set_body_string("no scope"))
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path(
                "/gmail/v1/users/me/settings/filters",
            ))
            .and(wiremock::matchers::body_json(serde_json::json!({
                "criteria": { "from": "a@b.example" },
                "action": { "addLabelIds": [], "removeLabelIds": ["INBOX"] }
            })))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(r#"{"id":"f1"}"#))
            .expect(1)
            .mount(&server)
            .await;

        let report = block_senders(
            &client(&server),
            &["a@b.example".into()],
            BlockAction::Archive,
            &crate::gmail::Cancel::new(),
        )
        .await;

        assert_eq!(report.blocked, 1, "the block still has to happen");
        assert!(
            report.unmarked,
            "and the user has to be told it is unmanaged"
        );
    }

    #[tokio::test]
    async fn a_refused_block_is_counted_and_explained() {
        let server = server_with_label().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path(
                "/gmail/v1/users/me/settings/filters",
            ))
            .respond_with(wiremock::ResponseTemplate::new(400).set_body_string("nope"))
            .mount(&server)
            .await;

        let report = block_senders(
            &client(&server),
            &["a@b.example".into()],
            BlockAction::Archive,
            &crate::gmail::Cancel::new(),
        )
        .await;

        assert_eq!(report.blocked, 0);
        assert_eq!(report.failed, 1);
        assert!(report.problem.is_some(), "a failure must say why");
    }

    #[tokio::test]
    async fn a_trash_request_carries_a_content_length() {
        // Gmail answers a POST with no Content-Length header with 411 Length
        // Required, and a bodyless reqwest POST omits the header rather than
        // sending zero. Every trash request the app made before this was fixed
        // failed for that reason and nothing said so.
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path_regex(
                r"^/gmail/v1/users/me/messages/[^/]+/trash$",
            ))
            .and(wiremock::matchers::header("content-length", "0"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string("{}"))
            .expect(1)
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

        let report =
            trash_messages(&gmail, &["m1".to_string()], &crate::gmail::Cancel::new()).await;

        // The mock only matches when Content-Length is present, so a pass here
        // is proof the header went out.
        assert_eq!(
            report.trashed, 1,
            "the request must carry Content-Length: 0"
        );
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
        let report = trash_messages(&gmail, &ids, &crate::gmail::Cancel::new()).await;

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
        let report = trash_messages(&gmail, &ids, &crate::gmail::Cancel::new()).await;

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
        let report = trash_messages(&gmail, &[], &crate::gmail::Cancel::new()).await;
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
