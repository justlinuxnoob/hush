//! Walking the mailbox and turning it into a sender list.
//!
//! Scanning is the slow part, so the design is about staying honest and
//! interruptible rather than being clever:
//!
//! * Progress is a real count of real messages, never a made-up percentage.
//! * Cancelling stops within a second and keeps everything found so far.
//! * Everything is written to the local database as it arrives, so quitting
//!   mid-scan loses nothing and a relaunch picks up where it stopped.
//! * A second scan asks Gmail only what changed, using the history marker.

use std::sync::Arc;

use tokio::task::JoinSet;

use crate::error::{Error, Result};
use crate::gmail::{Cancel, GmailClient};
use crate::model::{now_ms, MessageMeta, ScanDepth, ScanProgress};
use crate::store::{ScanState, Store};

/// Gmail's maximum page size for `messages.list`. Bigger pages mean fewer
/// list calls, and list calls are pure overhead next to the metadata fetches.
const LIST_PAGE_SIZE: u32 = 500;

/// How many metadata fetches are in flight at once.
///
/// The rate limiter decides the actual pace; this only decides how many
/// requests may be waiting on the network together. Beyond about this many,
/// HTTP/2 multiplexing stops helping and failures get harder to attribute.
const CONCURRENCY: usize = 12;

/// Messages held in memory before being written to the local database.
const COMMIT_EVERY: usize = 200;

/// How often the progress count is reported to the interface.
const REPORT_EVERY: u64 = 25;

/// Mail the user sent, drafts, and chats are not bulk mail from anyone.
const BASE_QUERY: &str = "-in:sent -in:drafts -in:chats";

pub struct Scanner {
    gmail: Arc<GmailClient>,
    store: Arc<Store>,
    account: String,
}

impl Scanner {
    pub fn new(gmail: Arc<GmailClient>, store: Arc<Store>, account: String) -> Self {
        Self {
            gmail,
            store,
            account,
        }
    }

    /// Scan from scratch to the given depth.
    ///
    /// Two passes. The first counts — it pages through message ids and nothing
    /// else, which costs 5 quota units per 500 messages and takes seconds even
    /// for a large mailbox. The second reads metadata, against a total that is
    /// now an exact count rather than a guess.
    ///
    /// The one-pass version that interleaved them could only ever quote Gmail's
    /// `resultSizeEstimate`, which is wrong often enough to be worse than no
    /// number at all: a scan would pass "501 messages" and keep going.
    pub async fn full_scan<F>(
        &self,
        depth: ScanDepth,
        cancel: Cancel,
        mut on_progress: F,
    ) -> Result<ScanProgress>
    where
        F: FnMut(&ScanProgress) + Send,
    {
        let query = build_query(depth);
        let known = self.store.known_ids(&self.account)?;

        let mut progress = ScanProgress {
            counting: true,
            ..Default::default()
        };
        on_progress(&progress);

        // --- pass one: count -------------------------------------------------
        let mut seen: Vec<String> = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            if cancel.is_cancelled() {
                return Ok(self.stop_early(progress, depth, true, None));
            }
            let page = match self
                .gmail
                .list_messages(&query, page_token.as_deref(), LIST_PAGE_SIZE, &cancel)
                .await
            {
                Ok(p) => p,
                Err(Error::Cancelled) => return Ok(self.stop_early(progress, depth, true, None)),
                Err(e) => return Err(e),
            };

            seen.extend(page.ids);
            progress.found = seen.len() as u64;
            on_progress(&progress);

            page_token = page.next_page_token;
            if page_token.is_none() {
                break;
            }
        }

        // --- pass two: read --------------------------------------------------
        let wanted: Vec<String> = seen
            .iter()
            .filter(|id| !known.contains(*id))
            .cloned()
            .collect();

        progress.counting = false;
        progress.total = seen.len() as u64;
        // Anything already held counts as read, so the bar starts where the
        // last run left off rather than at zero.
        progress.scanned = (seen.len() - wanted.len()) as u64;
        on_progress(&progress);

        match self
            .fetch_batch(&wanted, &cancel, &mut progress, &mut on_progress)
            .await
        {
            Ok(()) => {}
            Err(Error::Cancelled) => return Ok(self.stop_early(progress, depth, true, None)),
            Err(e) => {
                // Keep what we have and say so plainly. A partial list is far
                // more useful than an error screen.
                return Ok(self.stop_early(progress, depth, false, Some(e.to_string())));
            }
        }

        // A completed sweep saw everything in scope, so anything held locally
        // and not seen is gone from Gmail — deleted by the user, binned by a
        // previous run, or moved out of the window. Only a *completed* scan can
        // say that, which is why this is not done on a cancelled one.
        let removed = self
            .store
            .reconcile(&self.account, &seen, depth_cutoff_ms(depth))?;
        if removed > 0 {
            log::info!("rescan dropped {removed} messages no longer in Gmail");
        }

        // Record where the mailbox stands now, so the next scan can ask only
        // for what changed.
        let history_id = self.gmail.profile().await.ok().map(|p| p.history_id);
        self.store.put_scan_state(
            &self.account,
            &ScanState {
                history_id,
                last_scan_ms: now_ms(),
                complete: true,
                depth: Some(depth_key(depth).to_string()),
                page_token: None,
            },
        )?;

        progress.finished = true;
        progress.senders_found = self.store.senders(&self.account)?.len() as u64;
        on_progress(&progress);
        Ok(progress)
    }

    /// Fetch only what arrived since the last scan.
    ///
    /// Gmail keeps history for a limited window; once a marker is too old it
    /// returns 404 and the only correct answer is a full scan. That is reported
    /// rather than hidden, because it changes how long the user waits.
    pub async fn incremental_scan<F>(
        &self,
        cancel: Cancel,
        mut on_progress: F,
    ) -> Result<ScanProgress>
    where
        F: FnMut(&ScanProgress) + Send,
    {
        let state = self.store.scan_state(&self.account)?;
        let Some(start) = state.history_id.clone().filter(|h| !h.is_empty()) else {
            return Err(Error::Other("needs a full scan first".into()));
        };

        let mut progress = ScanProgress {
            scanned: self.store.message_count(&self.account)?,
            ..Default::default()
        };
        let known = self.store.known_ids(&self.account)?;
        // Deliberately no reconciliation here. A history sweep reports only what
        // *changed*, so treating it as the full picture would delete every
        // message it did not mention — which is nearly all of them.
        let mut page_token = None;
        let mut latest_history = state.history_id.clone();

        loop {
            if cancel.is_cancelled() {
                progress.cancelled = true;
                return Ok(progress);
            }

            let page = match self
                .gmail
                .list_history(&start, page_token.as_deref(), &cancel)
                .await
            {
                Ok(p) => p,
                Err(Error::Cancelled) => {
                    progress.cancelled = true;
                    return Ok(progress);
                }
                // The marker has aged out of Gmail's history window.
                Err(Error::UnexpectedResponse(ref m)) if m.starts_with("404") => {
                    return Err(Error::Other("history expired".into()));
                }
                Err(e) => return Err(e),
            };

            if let Some(h) = page.history_id.clone() {
                latest_history = Some(h);
            }

            let wanted: Vec<String> = page
                .added_ids
                .into_iter()
                .filter(|id| !known.contains(id))
                .collect();
            self.fetch_batch(&wanted, &cancel, &mut progress, &mut on_progress)
                .await?;

            page_token = page.next_page_token;
            if page_token.is_none() {
                break;
            }
        }

        self.store.put_scan_state(
            &self.account,
            &ScanState {
                history_id: latest_history,
                last_scan_ms: now_ms(),
                complete: true,
                depth: state.depth,
                page_token: None,
            },
        )?;

        progress.finished = true;
        progress.senders_found = self.store.senders(&self.account)?.len() as u64;
        on_progress(&progress);
        Ok(progress)
    }

    /// Fetch metadata for a set of ids, several at a time, saving as we go.
    async fn fetch_batch<F>(
        &self,
        ids: &[String],
        cancel: &Cancel,
        progress: &mut ScanProgress,
        on_progress: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&ScanProgress) + Send,
    {
        let mut queue = ids.iter().cloned();
        let mut tasks: JoinSet<Result<MessageMeta>> = JoinSet::new();
        let mut buffer: Vec<MessageMeta> = Vec::with_capacity(ids.len().min(256));

        let mut spawn_next = |tasks: &mut JoinSet<Result<MessageMeta>>| {
            if let Some(id) = queue.next() {
                let gmail = self.gmail.clone();
                let cancel = cancel.clone();
                tasks.spawn(async move { gmail.get_metadata(&id, &cancel).await });
                true
            } else {
                false
            }
        };

        for _ in 0..CONCURRENCY {
            if !spawn_next(&mut tasks) {
                break;
            }
        }

        while let Some(joined) = tasks.join_next().await {
            match joined {
                Ok(Ok(meta)) => {
                    buffer.push(meta);
                    progress.scanned += 1;
                }
                Ok(Err(Error::Cancelled)) => {
                    tasks.abort_all();
                    self.store.put_messages(&self.account, &buffer)?;
                    return Err(Error::Cancelled);
                }
                // One unreadable message must not sink an entire scan; skip it
                // and carry on. The count reflects what we actually read.
                Ok(Err(e)) => log::warn!("skipping a message: {e}"),
                Err(e) => log::warn!("a fetch task ended unexpectedly: {e}"),
            }

            if cancel.is_cancelled() {
                tasks.abort_all();
                self.store.put_messages(&self.account, &buffer)?;
                return Err(Error::Cancelled);
            }

            // Commit in chunks so a crash or a quit keeps most of the work, but
            // report progress more often than that — a counter that jumps in
            // steps of two hundred reads as a stall, not as progress.
            if buffer.len() >= COMMIT_EVERY {
                self.store.put_messages(&self.account, &buffer)?;
                buffer.clear();
            }
            if progress.scanned % REPORT_EVERY == 0 {
                on_progress(progress);
            }

            spawn_next(&mut tasks);
        }

        self.store.put_messages(&self.account, &buffer)?;
        on_progress(progress);
        Ok(())
    }

    fn stop_early(
        &self,
        mut progress: ScanProgress,
        depth: ScanDepth,
        cancelled: bool,
        note: Option<String>,
    ) -> ScanProgress {
        let _ = self.save_state(depth, false);
        progress.cancelled = cancelled;
        progress.finished = true;
        progress.note = note;
        progress.senders_found = self
            .store
            .senders(&self.account)
            .map(|s| s.len() as u64)
            .unwrap_or(0);
        progress
    }

    fn save_state(&self, depth: ScanDepth, complete: bool) -> Result<()> {
        let previous = self.store.scan_state(&self.account)?;
        self.store.put_scan_state(
            &self.account,
            &ScanState {
                history_id: previous.history_id,
                last_scan_ms: now_ms(),
                complete,
                depth: Some(depth_key(depth).to_string()),
                page_token: None,
            },
        )
    }
}

fn build_query(depth: ScanDepth) -> String {
    match depth.query_fragment() {
        Some(f) => format!("{BASE_QUERY} {f}"),
        None => BASE_QUERY.to_string(),
    }
}

/// The earliest moment a given depth looked at, as epoch milliseconds.
///
/// Reconciliation must not delete rows outside the window that was actually
/// searched — a six-month scan says nothing about last year's mail.
fn depth_cutoff_ms(depth: ScanDepth) -> i64 {
    const DAY: i64 = 86_400_000;
    let days = match depth {
        ScanDepth::SixMonths => 183,
        ScanDepth::OneYear => 365,
        ScanDepth::TwoYears => 730,
        ScanDepth::Everything => return 0,
    };
    // A day of slack, since Gmail's `newer_than` and our clock need not agree
    // to the minute and over-deleting is the worse mistake.
    (now_ms() - (days + 1) * DAY).max(0)
}

fn depth_key(depth: ScanDepth) -> &'static str {
    match depth {
        ScanDepth::SixMonths => "six_months",
        ScanDepth::OneYear => "one_year",
        ScanDepth::TwoYears => "two_years",
        ScanDepth::Everything => "everything",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_query_excludes_the_users_own_mail() {
        let q = build_query(ScanDepth::SixMonths);
        assert!(q.contains("-in:sent"));
        assert!(q.contains("-in:drafts"));
        assert!(q.contains("-in:chats"));
        assert!(q.contains("newer_than:6m"));
    }

    #[test]
    fn everything_has_no_date_limit() {
        let q = build_query(ScanDepth::Everything);
        assert!(!q.contains("newer_than"));
        assert!(q.contains("-in:sent"));
    }

    #[test]
    fn a_depth_cutoff_never_reaches_past_what_was_searched() {
        // Everything means everything; the rest leave older mail alone.
        assert_eq!(depth_cutoff_ms(ScanDepth::Everything), 0);
        let now = now_ms();
        for (depth, days) in [
            (ScanDepth::SixMonths, 183),
            (ScanDepth::OneYear, 365),
            (ScanDepth::TwoYears, 730),
        ] {
            let cutoff = depth_cutoff_ms(depth);
            let age_days = (now - cutoff) / 86_400_000;
            assert!(
                age_days >= days && age_days <= days + 2,
                "{depth:?} cutoff was {age_days} days back"
            );
        }
    }

    #[test]
    fn depth_keys_are_stable() {
        // These are written into the database, so changing one silently would
        // strand every existing resume point.
        assert_eq!(depth_key(ScanDepth::SixMonths), "six_months");
        assert_eq!(depth_key(ScanDepth::OneYear), "one_year");
        assert_eq!(depth_key(ScanDepth::TwoYears), "two_years");
        assert_eq!(depth_key(ScanDepth::Everything), "everything");
    }
}
