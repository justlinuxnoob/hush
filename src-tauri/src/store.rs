//! Local storage: a single SQLite file in the app's data directory.
//!
//! What is kept: for each scanned message, the sender, the subject, the date,
//! and the unsubscribe headers. Plus the user's own choices — the never-touch
//! list, settings, and what happened when they unsubscribed.
//!
//! What is never kept, because it is never fetched: message bodies,
//! attachments, recipients, or anything about mail the user sends.
//!
//! Subjects are stored because the safety heuristics read them; a sender whose
//! recent subjects are all "Your order has shipped" needs a warning, and that
//! judgement cannot be made without the words. "Forget everything" deletes this
//! file outright.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::{Error, Result};
use crate::heuristics::{assess, SenderSignals};
use crate::model::{describe_frequency, now_ms, MessageMeta, Outcome, OutcomeStatus, Sender};
use crate::parse::parse_unsubscribe;

/// How many recent subjects to keep per sender for the safety check and for
/// showing the user something recognisable.
const SAMPLE_SUBJECTS: usize = 8;

pub struct Store {
    conn: Mutex<Connection>,
    path: PathBuf,
}

/// Where a scan got to, so relaunching does not start from nothing.
#[derive(Debug, Clone, Default)]
pub struct ScanState {
    pub history_id: Option<String>,
    pub last_scan_ms: i64,
    pub complete: bool,
    pub depth: Option<String>,
    /// Set while a scan is partway through a page run.
    pub page_token: Option<String>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path)?;
        Self::init(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            path: path.to_path_buf(),
        })
    }

    /// A throwaway store that never touches disk. Used by the tests, and handy
    /// for anyone poking at the sender-grouping rules.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::init(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            path: PathBuf::from(":memory:"),
        })
    }

    fn init(conn: &Connection) -> Result<()> {
        // WAL keeps the UI responsive while a scan writes; the rest are the
        // usual desktop-SQLite settings.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;

        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS messages (
                account   TEXT NOT NULL,
                id        TEXT NOT NULL,
                sender    TEXT NOT NULL,
                name      TEXT NOT NULL DEFAULT '',
                subject   TEXT NOT NULL DEFAULT '',
                date_ms   INTEGER NOT NULL DEFAULT 0,
                lu        TEXT,
                lup       TEXT,
                PRIMARY KEY (account, id)
            );
            CREATE INDEX IF NOT EXISTS messages_by_sender
                ON messages (account, sender, date_ms DESC);

            CREATE TABLE IF NOT EXISTS never_touch (
                account TEXT NOT NULL,
                sender  TEXT NOT NULL,
                added_ms INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (account, sender)
            );

            CREATE TABLE IF NOT EXISTS outcomes (
                account TEXT NOT NULL,
                sender  TEXT NOT NULL,
                name    TEXT NOT NULL DEFAULT '',
                status  TEXT NOT NULL,
                detail  TEXT NOT NULL DEFAULT '',
                link    TEXT,
                at_ms   INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (account, sender)
            );

            CREATE TABLE IF NOT EXISTS scan_state (
                account    TEXT PRIMARY KEY,
                history_id TEXT,
                last_scan_ms INTEGER NOT NULL DEFAULT 0,
                complete   INTEGER NOT NULL DEFAULT 0,
                depth      TEXT,
                page_token TEXT
            );

            CREATE TABLE IF NOT EXISTS settings (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            "#,
        )?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| Error::Storage("local data is busy".into()))
    }

    // --- messages ----------------------------------------------------------

    /// Insert or update a batch of message metadata in one transaction.
    pub fn put_messages(&self, account: &str, messages: &[MessageMeta]) -> Result<()> {
        if messages.is_empty() {
            return Ok(());
        }
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO messages (account, id, sender, name, subject, date_ms, lu, lup)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(account, id) DO UPDATE SET
                    sender=excluded.sender, name=excluded.name, subject=excluded.subject,
                    date_ms=excluded.date_ms, lu=excluded.lu, lup=excluded.lup",
            )?;
            for m in messages {
                stmt.execute(params![
                    account,
                    m.id,
                    m.sender_address,
                    m.sender_name,
                    m.subject,
                    m.date_ms,
                    m.list_unsubscribe,
                    m.list_unsubscribe_post,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn message_count(&self, account: &str) -> Result<u64> {
        let conn = self.lock()?;
        // SQLite counts are signed; the cast is safe for any real mailbox.
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE account = ?1",
            params![account],
            |r| r.get(0),
        )?;
        Ok(n.max(0) as u64)
    }

    /// Ids we already hold, so a rescan does not refetch them.
    pub fn known_ids(&self, account: &str) -> Result<std::collections::HashSet<String>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare("SELECT id FROM messages WHERE account = ?1")?;
        let rows = stmt.query_map(params![account], |r| r.get::<_, String>(0))?;
        let mut out = std::collections::HashSet::new();
        for r in rows {
            out.insert(r?);
        }
        Ok(out)
    }

    /// The ids of a sender's *bulk* messages — the ones that actually carried
    /// an unsubscribe header.
    ///
    /// This is the whole safety story for the tidy-up feature. Plenty of shops
    /// send marketing and receipts from one address; the marketing carries
    /// `List-Unsubscribe` and the receipt does not. Selecting on that header
    /// means an order confirmation from a sender you just unsubscribed from is
    /// left exactly where it was.
    pub fn bulk_message_ids(&self, account: &str, sender: &str) -> Result<Vec<String>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id FROM messages
             WHERE account = ?1 AND sender = ?2 AND lu IS NOT NULL AND lu != ''",
        )?;
        let rows = stmt.query_map(params![account, sender], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Forget messages that are no longer in the inbox.
    ///
    /// Called after mail is moved to Trash. Gmail's `messages.list` excludes
    /// trashed mail, so a later scan would never re-add these — but the rows we
    /// already hold would linger, leaving the sender showing a count that is no
    /// longer true and letting a second tidy-up re-attempt mail already binned.
    pub fn forget_messages(&self, account: &str, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        {
            let mut stmt =
                tx.prepare_cached("DELETE FROM messages WHERE account = ?1 AND id = ?2")?;
            for id in ids {
                stmt.execute(params![account, id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Drop local rows for messages Gmail no longer returns.
    ///
    /// A scan only ever added, so anything deleted in Gmail — by the user, or
    /// by a previous tidy-up — lingered here forever and kept showing up. This
    /// makes a rescan a genuine reconciliation rather than a top-up.
    ///
    /// Scoped by date, because a six-month scan says nothing about what exists
    /// outside that window and must not delete rows it never looked for.
    pub fn reconcile(&self, account: &str, seen: &[String], since_ms: i64) -> Result<u64> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let removed;
        {
            // A temporary table beats a WHERE ... NOT IN (?, ?, ... × 40,000).
            tx.execute_batch(
                "CREATE TEMP TABLE IF NOT EXISTS seen_ids (id TEXT PRIMARY KEY);
                 DELETE FROM seen_ids;",
            )?;
            {
                let mut stmt =
                    tx.prepare_cached("INSERT OR IGNORE INTO seen_ids (id) VALUES (?1)")?;
                for id in seen {
                    stmt.execute(params![id])?;
                }
            }
            removed = tx.execute(
                "DELETE FROM messages
                 WHERE account = ?1
                   AND date_ms >= ?2
                   AND id NOT IN (SELECT id FROM seen_ids)",
                params![account, since_ms],
            )? as u64;
            tx.execute_batch("DROP TABLE IF EXISTS seen_ids;")?;
        }
        tx.commit()?;
        Ok(removed)
    }

    /// Every subject Hush holds for one sender, newest first.
    ///
    /// Loaded on demand rather than bundled into the sender list: a mailbox
    /// with sixty senders and hundreds of messages each would put tens of
    /// thousands of strings into a payload the list screen mostly never shows.
    pub fn subjects_for_sender(
        &self,
        account: &str,
        sender: &str,
        limit: u32,
    ) -> Result<Vec<(String, i64)>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT subject, date_ms FROM messages
             WHERE account = ?1 AND sender = ?2 AND subject != ''
             ORDER BY date_ms DESC
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![account, sender, limit], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // --- senders -----------------------------------------------------------

    /// Build the sender list the interface shows.
    ///
    /// Only senders with at least one `List-Unsubscribe` header appear. That is
    /// the safety gate, and it lives here so no caller can route around it.
    pub fn senders(&self, account: &str) -> Result<Vec<Sender>> {
        let conn = self.lock()?;

        let never: std::collections::HashSet<String> = {
            let mut stmt = conn.prepare("SELECT sender FROM never_touch WHERE account = ?1")?;
            let rows = stmt.query_map(params![account], |r| r.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };

        let outcomes: std::collections::HashMap<String, Outcome> = {
            let mut stmt = conn.prepare(
                "SELECT sender, name, status, detail, link, at_ms FROM outcomes WHERE account = ?1",
            )?;
            let rows = stmt.query_map(params![account], |r| {
                Ok(Outcome {
                    address: r.get(0)?,
                    display_name: r.get(1)?,
                    status: status_from_str(&r.get::<_, String>(2)?),
                    detail: r.get(3)?,
                    link: r.get(4)?,
                    at_ms: r.get(5)?,
                })
            })?;
            rows.map(|o| o.map(|o| (o.address.clone(), o)))
                .collect::<rusqlite::Result<_>>()?
        };

        // One ordered pass beats a query per sender: sorted by sender then
        // newest-first, so each group's first rows are the recent ones.
        let mut stmt = conn.prepare(
            "SELECT sender, name, subject, date_ms, lu, lup
             FROM messages WHERE account = ?1
             ORDER BY sender ASC, date_ms DESC",
        )?;
        let mut rows = stmt.query(params![account])?;

        let mut out: Vec<Sender> = Vec::new();
        let mut group: Option<Group> = None;

        while let Some(row) = rows.next()? {
            let sender: String = row.get(0)?;
            let name: String = row.get(1)?;
            let subject: String = row.get(2)?;
            let date_ms: i64 = row.get(3)?;
            let lu: Option<String> = row.get(4)?;
            let lup: Option<String> = row.get(5)?;

            if group.as_ref().is_some_and(|g| g.address != sender) {
                if let Some(g) = group.take() {
                    out.extend(g.finish(&never, &outcomes));
                }
            }
            let g = group.get_or_insert_with(|| Group::new(sender.clone()));
            g.add(name, subject, date_ms, lu, lup);
        }
        if let Some(g) = group.take() {
            out.extend(g.finish(&never, &outcomes));
        }

        // Busiest first: that is the order in which unsubscribing pays off most.
        out.sort_by(|a, b| {
            b.message_count
                .cmp(&a.message_count)
                .then_with(|| a.address.cmp(&b.address))
        });
        Ok(out)
    }

    // --- never touch -------------------------------------------------------

    pub fn set_never_touch(&self, account: &str, sender: &str, on: bool) -> Result<()> {
        let conn = self.lock()?;
        if on {
            conn.execute(
                "INSERT OR IGNORE INTO never_touch (account, sender, added_ms) VALUES (?1, ?2, ?3)",
                params![account, sender, now_ms()],
            )?;
        } else {
            conn.execute(
                "DELETE FROM never_touch WHERE account = ?1 AND sender = ?2",
                params![account, sender],
            )?;
        }
        Ok(())
    }

    pub fn never_touch(&self, account: &str) -> Result<Vec<String>> {
        let conn = self.lock()?;
        let mut stmt =
            conn.prepare("SELECT sender FROM never_touch WHERE account = ?1 ORDER BY sender")?;
        let rows = stmt.query_map(params![account], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn is_never_touch(&self, account: &str, sender: &str) -> Result<bool> {
        let conn = self.lock()?;
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM never_touch WHERE account = ?1 AND sender = ?2",
            params![account, sender],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    // --- outcomes ----------------------------------------------------------

    pub fn record_outcome(&self, account: &str, o: &Outcome) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO outcomes (account, sender, name, status, detail, link, at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(account, sender) DO UPDATE SET
                name=excluded.name, status=excluded.status, detail=excluded.detail,
                link=excluded.link, at_ms=excluded.at_ms",
            params![
                account,
                o.address,
                o.display_name,
                status_to_str(&o.status),
                o.detail,
                o.link,
                o.at_ms
            ],
        )?;
        Ok(())
    }

    pub fn outcomes(&self, account: &str) -> Result<Vec<Outcome>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT sender, name, status, detail, link, at_ms FROM outcomes
             WHERE account = ?1 ORDER BY at_ms DESC",
        )?;
        let rows = stmt.query_map(params![account], |r| {
            Ok(Outcome {
                address: r.get(0)?,
                display_name: r.get(1)?,
                status: status_from_str(&r.get::<_, String>(2)?),
                detail: r.get(3)?,
                link: r.get(4)?,
                at_ms: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Mark a manual link as handled, so the "finish these yourself" list shrinks.
    pub fn mark_manual_done(&self, account: &str, sender: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE outcomes SET status = 'done', detail = 'You marked this as done', at_ms = ?3
             WHERE account = ?1 AND sender = ?2",
            params![account, sender, now_ms()],
        )?;
        Ok(())
    }

    // --- scan state --------------------------------------------------------

    pub fn scan_state(&self, account: &str) -> Result<ScanState> {
        let conn = self.lock()?;
        let row = conn
            .query_row(
                "SELECT history_id, last_scan_ms, complete, depth, page_token
                 FROM scan_state WHERE account = ?1",
                params![account],
                |r| {
                    Ok(ScanState {
                        history_id: r.get(0)?,
                        last_scan_ms: r.get(1)?,
                        complete: r.get::<_, i64>(2)? != 0,
                        depth: r.get(3)?,
                        page_token: r.get(4)?,
                    })
                },
            )
            .optional()?;
        Ok(row.unwrap_or_default())
    }

    pub fn put_scan_state(&self, account: &str, s: &ScanState) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO scan_state (account, history_id, last_scan_ms, complete, depth, page_token)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(account) DO UPDATE SET
                history_id=excluded.history_id, last_scan_ms=excluded.last_scan_ms,
                complete=excluded.complete, depth=excluded.depth, page_token=excluded.page_token",
            params![
                account,
                s.history_id,
                s.last_scan_ms,
                s.complete as i64,
                s.depth,
                s.page_token
            ],
        )?;
        Ok(())
    }

    // --- settings ----------------------------------------------------------

    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        let conn = self.lock()?;
        Ok(conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// Delete everything Hush has stored about mail, keeping the user's own
    /// never-touch list only if they asked to.
    pub fn erase(&self, keep_never_touch: bool) -> Result<()> {
        let conn = self.lock()?;
        conn.execute_batch(
            "DELETE FROM messages; DELETE FROM outcomes; DELETE FROM scan_state; DELETE FROM settings;",
        )?;
        if !keep_never_touch {
            conn.execute_batch("DELETE FROM never_touch;")?;
        }
        // Hand the pages back to the filesystem so "erase" means erase.
        conn.execute_batch("VACUUM;")?;
        Ok(())
    }
}

/// Accumulator for one sender while walking the ordered message rows.
struct Group {
    address: String,
    name: String,
    count: u32,
    /// Of those, how many carried an unsubscribe header.
    bulk_count: u32,
    first_ms: i64,
    last_ms: i64,
    subjects: Vec<String>,
    /// Headers from this sender's most recent message that had any.
    lu: Option<String>,
    lup: Option<String>,
}

impl Group {
    fn new(address: String) -> Self {
        Self {
            address,
            name: String::new(),
            count: 0,
            bulk_count: 0,
            first_ms: i64::MAX,
            last_ms: 0,
            subjects: Vec::new(),
            lu: None,
            lup: None,
        }
    }

    fn add(
        &mut self,
        name: String,
        subject: String,
        date_ms: i64,
        lu: Option<String>,
        lup: Option<String>,
    ) {
        // Rows arrive newest-first, so the first non-empty name is the latest.
        if self.name.is_empty() && !name.is_empty() {
            self.name = name;
        }
        self.count += 1;
        if lu.as_deref().is_some_and(|v| !v.is_empty()) {
            self.bulk_count += 1;
        }
        self.first_ms = self.first_ms.min(date_ms);
        self.last_ms = self.last_ms.max(date_ms);
        if self.subjects.len() < SAMPLE_SUBJECTS && !subject.is_empty() {
            self.subjects.push(subject);
        }
        // Take the most recent message that actually carried a usable
        // unsubscribe header. A sender's newest mail may be a receipt with none
        // at all — or, just as common, one with the field present but empty.
        // Latching onto an empty one would hide a sender whose older mail is
        // perfectly unsubscribable.
        if self.lu.is_none() && lu.as_deref().is_some_and(|v| !v.trim().is_empty()) {
            self.lu = lu;
            self.lup = lup;
        }
    }

    fn finish(
        self,
        never: &std::collections::HashSet<String>,
        outcomes: &std::collections::HashMap<String, Outcome>,
    ) -> Option<Sender> {
        // The gate: no unsubscribe header, no entry in the list. Ever.
        let parsed = parse_unsubscribe(self.lu.as_deref(), self.lup.as_deref());
        if !parsed.method.is_actionable() {
            return None;
        }

        let assessment = assess(&SenderSignals {
            address: &self.address,
            display_name: &self.name,
            subjects: &self.subjects,
        });

        let display_name = if self.name.is_empty() {
            self.address
                .split('@')
                .next()
                .unwrap_or(&self.address)
                .to_string()
        } else {
            self.name
        };

        Some(Sender {
            frequency: describe_frequency(self.count, self.first_ms, self.last_ms),
            never_touch: never.contains(&self.address),
            outcome: outcomes.get(&self.address).cloned(),
            message_count: self.count,
            bulk_count: self.bulk_count,
            first_seen_ms: if self.first_ms == i64::MAX {
                0
            } else {
                self.first_ms
            },
            last_seen_ms: self.last_ms,
            fallbacks: parsed.methods(),
            method: parsed.method,
            assessment,
            sample_subjects: self.subjects,
            display_name,
            address: self.address,
        })
    }
}

fn status_to_str(s: &OutcomeStatus) -> &'static str {
    match s {
        OutcomeStatus::Done => "done",
        OutcomeStatus::Sent => "sent",
        OutcomeStatus::NeedsYou => "needs_you",
        OutcomeStatus::Failed => "failed",
    }
}

fn status_from_str(s: &str) -> OutcomeStatus {
    match s {
        "done" => OutcomeStatus::Done,
        "sent" => OutcomeStatus::Sent,
        "needs_you" => OutcomeStatus::NeedsYou,
        _ => OutcomeStatus::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::UnsubMethod;

    const ACC: &str = "me@example.com";
    const DAY: i64 = 86_400_000;

    fn msg(id: &str, sender: &str, subject: &str, day: i64, lu: Option<&str>) -> MessageMeta {
        MessageMeta {
            id: id.into(),
            sender_address: sender.into(),
            sender_name: "Acme".into(),
            subject: subject.into(),
            date_ms: day * DAY,
            list_unsubscribe: lu.map(str::to_string),
            list_unsubscribe_post: None,
        }
    }

    #[test]
    fn senders_without_an_unsubscribe_header_never_appear() {
        // The single most important behaviour in the app.
        let s = Store::open_in_memory().unwrap();
        s.put_messages(
            ACC,
            &[
                msg("1", "receipts@bank.example", "Your statement", 1, None),
                msg("2", "receipts@bank.example", "Your statement", 2, None),
                msg(
                    "3",
                    "news@shop.example",
                    "Sale",
                    3,
                    Some("<https://s.example/u>"),
                ),
            ],
        )
        .unwrap();

        let senders = s.senders(ACC).unwrap();
        assert_eq!(senders.len(), 1);
        assert_eq!(senders[0].address, "news@shop.example");
    }

    #[test]
    fn a_sender_qualifies_from_any_message_that_carried_the_header() {
        // Newest mail is a receipt with no header; an older campaign had one.
        let s = Store::open_in_memory().unwrap();
        s.put_messages(
            ACC,
            &[
                msg(
                    "1",
                    "hi@shop.example",
                    "Sale!",
                    1,
                    Some("<https://s.example/u>"),
                ),
                msg("2", "hi@shop.example", "Your receipt", 9, None),
            ],
        )
        .unwrap();
        let senders = s.senders(ACC).unwrap();
        assert_eq!(senders.len(), 1);
        assert_eq!(
            senders[0].method,
            UnsubMethod::ManualLink {
                url: "https://s.example/u".into()
            }
        );
    }

    #[test]
    fn senders_are_grouped_counted_and_sorted_by_volume() {
        let s = Store::open_in_memory().unwrap();
        let lu = Some("<https://x.example/u>");
        let mut batch = vec![];
        for i in 0..5 {
            batch.push(msg(&format!("a{i}"), "a@x.example", "Hi", i, lu));
        }
        for i in 0..12 {
            batch.push(msg(&format!("b{i}"), "b@x.example", "Hi", i, lu));
        }
        s.put_messages(ACC, &batch).unwrap();

        let senders = s.senders(ACC).unwrap();
        assert_eq!(senders.len(), 2);
        assert_eq!(senders[0].address, "b@x.example");
        assert_eq!(senders[0].message_count, 12);
        assert_eq!(senders[1].message_count, 5);
        assert_eq!(senders[0].first_seen_ms, 0);
        assert_eq!(senders[0].last_seen_ms, 11 * DAY);
    }

    #[test]
    fn rescanning_the_same_message_does_not_double_count() {
        let s = Store::open_in_memory().unwrap();
        let m = msg("dup", "a@x.example", "Hi", 1, Some("<https://x.example/u>"));
        let batch = [m];
        s.put_messages(ACC, &batch).unwrap();
        s.put_messages(ACC, &batch).unwrap();
        s.put_messages(ACC, &batch).unwrap();
        assert_eq!(s.message_count(ACC).unwrap(), 1);
        assert_eq!(s.senders(ACC).unwrap()[0].message_count, 1);
    }

    #[test]
    fn accounts_are_kept_apart() {
        let s = Store::open_in_memory().unwrap();
        let lu = Some("<https://x.example/u>");
        s.put_messages(ACC, &[msg("1", "a@x.example", "Hi", 1, lu)])
            .unwrap();
        s.put_messages("other@example.com", &[msg("1", "b@x.example", "Hi", 1, lu)])
            .unwrap();
        assert_eq!(s.senders(ACC).unwrap().len(), 1);
        assert_eq!(s.senders(ACC).unwrap()[0].address, "a@x.example");
        assert_eq!(s.senders("other@example.com").unwrap().len(), 1);
    }

    #[test]
    fn the_never_touch_list_survives_and_is_reflected_on_senders() {
        let s = Store::open_in_memory().unwrap();
        s.put_messages(
            ACC,
            &[msg(
                "1",
                "a@x.example",
                "Hi",
                1,
                Some("<https://x.example/u>"),
            )],
        )
        .unwrap();

        assert!(!s.senders(ACC).unwrap()[0].never_touch);
        s.set_never_touch(ACC, "a@x.example", true).unwrap();
        assert!(s.senders(ACC).unwrap()[0].never_touch);
        assert!(s.is_never_touch(ACC, "a@x.example").unwrap());
        assert_eq!(s.never_touch(ACC).unwrap(), vec!["a@x.example"]);

        s.set_never_touch(ACC, "a@x.example", false).unwrap();
        assert!(!s.senders(ACC).unwrap()[0].never_touch);
    }

    #[test]
    fn outcomes_round_trip_and_attach_to_senders() {
        let s = Store::open_in_memory().unwrap();
        s.put_messages(
            ACC,
            &[msg(
                "1",
                "a@x.example",
                "Hi",
                1,
                Some("<https://x.example/u>"),
            )],
        )
        .unwrap();

        let o = Outcome {
            address: "a@x.example".into(),
            display_name: "Acme".into(),
            status: OutcomeStatus::NeedsYou,
            detail: "Open the link to finish".into(),
            link: Some("https://x.example/u".into()),
            at_ms: 42,
        };
        s.record_outcome(ACC, &o).unwrap();
        assert_eq!(s.outcomes(ACC).unwrap(), vec![o.clone()]);
        assert_eq!(s.senders(ACC).unwrap()[0].outcome.as_ref(), Some(&o));

        s.mark_manual_done(ACC, "a@x.example").unwrap();
        assert_eq!(s.outcomes(ACC).unwrap()[0].status, OutcomeStatus::Done);
    }

    #[test]
    fn scan_state_round_trips() {
        let s = Store::open_in_memory().unwrap();
        assert_eq!(s.scan_state(ACC).unwrap().history_id, None);
        let state = ScanState {
            history_id: Some("12345".into()),
            last_scan_ms: 99,
            complete: true,
            depth: Some("one_year".into()),
            page_token: None,
        };
        s.put_scan_state(ACC, &state).unwrap();
        let back = s.scan_state(ACC).unwrap();
        assert_eq!(back.history_id.as_deref(), Some("12345"));
        assert!(back.complete);
        assert_eq!(back.depth.as_deref(), Some("one_year"));
    }

    #[test]
    fn erase_clears_mail_data_and_can_keep_the_never_touch_list() {
        let s = Store::open_in_memory().unwrap();
        s.put_messages(
            ACC,
            &[msg(
                "1",
                "a@x.example",
                "Hi",
                1,
                Some("<https://x.example/u>"),
            )],
        )
        .unwrap();
        s.set_never_touch(ACC, "keep@x.example", true).unwrap();
        s.set_setting("mailto_mode", "hand_off").unwrap();

        s.erase(true).unwrap();
        assert_eq!(s.message_count(ACC).unwrap(), 0);
        assert_eq!(s.get_setting("mailto_mode").unwrap(), None);
        assert_eq!(s.never_touch(ACC).unwrap(), vec!["keep@x.example"]);

        s.erase(false).unwrap();
        assert!(s.never_touch(ACC).unwrap().is_empty());
    }

    #[test]
    fn known_ids_lets_a_rescan_skip_what_we_have() {
        let s = Store::open_in_memory().unwrap();
        s.put_messages(
            ACC,
            &[
                msg("a", "x@y.example", "Hi", 1, None),
                msg("b", "x@y.example", "Hi", 2, None),
            ],
        )
        .unwrap();
        let ids = s.known_ids(ACC).unwrap();
        assert!(ids.contains("a") && ids.contains("b") && !ids.contains("c"));
    }

    #[test]
    fn the_latest_display_name_wins() {
        let s = Store::open_in_memory().unwrap();
        let lu = Some("<https://x.example/u>");
        let mut old = msg("1", "a@x.example", "Hi", 1, lu);
        old.sender_name = "Old Name".into();
        let mut new = msg("2", "a@x.example", "Hi", 5, lu);
        new.sender_name = "New Name".into();
        s.put_messages(ACC, &[old, new]).unwrap();
        assert_eq!(s.senders(ACC).unwrap()[0].display_name, "New Name");
    }

    #[test]
    fn every_subject_can_be_listed_for_one_sender() {
        let s = Store::open_in_memory().unwrap();
        let lu = Some("<https://x.example/u>");
        let batch: Vec<_> = (0..30)
            .map(|i| {
                msg(
                    &format!("m{i}"),
                    "a@x.example",
                    &format!("Subject {i}"),
                    i,
                    lu,
                )
            })
            .collect();
        s.put_messages(ACC, &batch).unwrap();
        s.put_messages(ACC, &[msg("other", "b@x.example", "Not theirs", 1, lu)])
            .unwrap();

        let all = s.subjects_for_sender(ACC, "a@x.example", 500).unwrap();
        assert_eq!(all.len(), 30, "not capped at the list screen's sample");
        assert_eq!(all[0].0, "Subject 29", "newest first");
        assert!(!all.iter().any(|(subject, _)| subject == "Not theirs"));

        let capped = s.subjects_for_sender(ACC, "a@x.example", 10).unwrap();
        assert_eq!(capped.len(), 10);
    }

    #[test]
    fn subject_samples_are_capped_and_recent() {
        let s = Store::open_in_memory().unwrap();
        let lu = Some("<https://x.example/u>");
        let batch: Vec<_> = (0..30)
            .map(|i| {
                msg(
                    &format!("m{i}"),
                    "a@x.example",
                    &format!("Subject {i}"),
                    i,
                    lu,
                )
            })
            .collect();
        s.put_messages(ACC, &batch).unwrap();
        let sender = &s.senders(ACC).unwrap()[0];
        assert_eq!(sender.sample_subjects.len(), SAMPLE_SUBJECTS);
        assert_eq!(sender.sample_subjects[0], "Subject 29");
    }

    #[test]
    fn tidying_up_leaves_a_senders_receipts_alone() {
        // The case this exists for: one shop, one address, two kinds of mail.
        // The marketing carries an unsubscribe header; the order confirmation
        // does not. Only the marketing may ever be binned.
        let s = Store::open_in_memory().unwrap();
        let lu = Some("<https://shop.example/u>");
        s.put_messages(
            ACC,
            &[
                msg("promo1", "hi@shop.example", "50% off everything", 1, lu),
                msg("promo2", "hi@shop.example", "New arrivals", 2, lu),
                msg("receipt", "hi@shop.example", "Your order #1234", 3, None),
                msg(
                    "shipped",
                    "hi@shop.example",
                    "Your order has shipped",
                    4,
                    None,
                ),
            ],
        )
        .unwrap();

        let mut ids = s.bulk_message_ids(ACC, "hi@shop.example").unwrap();
        ids.sort();
        assert_eq!(ids, vec!["promo1", "promo2"]);
        assert!(!ids.contains(&"receipt".to_string()));
        assert!(!ids.contains(&"shipped".to_string()));

        // And the sender's counts tell the user the same story.
        let sender = &s.senders(ACC).unwrap()[0];
        assert_eq!(sender.message_count, 4);
        assert_eq!(sender.bulk_count, 2, "only the bulk mail would be binned");
    }

    #[test]
    fn after_binning_the_sender_reflects_what_is_actually_left() {
        // The bug this guards: trash the backlog, and the sender kept showing
        // its old count because the local rows were never dropped — so a second
        // tidy-up would re-attempt mail already in the bin.
        let s = Store::open_in_memory().unwrap();
        let lu = Some("<https://shop.example/u>");
        s.put_messages(
            ACC,
            &[
                msg("promo1", "hi@shop.example", "Sale", 1, lu),
                msg("promo2", "hi@shop.example", "Sale again", 2, lu),
                msg("receipt", "hi@shop.example", "Your order #1", 3, None),
            ],
        )
        .unwrap();

        let before = &s.senders(ACC).unwrap()[0];
        assert_eq!(before.message_count, 3);
        assert_eq!(before.bulk_count, 2);

        // Bin the bulk mail, then forget it, as a real run does.
        let binned = s.bulk_message_ids(ACC, "hi@shop.example").unwrap();
        s.forget_messages(ACC, &binned).unwrap();

        // The receipt survives, so the sender is still listed — but with no
        // bulk mail left there is nothing further to bin.
        assert_eq!(s.message_count(ACC).unwrap(), 1);
        assert!(s
            .bulk_message_ids(ACC, "hi@shop.example")
            .unwrap()
            .is_empty());

        // And with no unsubscribe header left, the sender drops off the list
        // entirely rather than lingering with a stale count.
        assert!(
            s.senders(ACC).unwrap().is_empty(),
            "a sender with only receipts left is not unsubscribable"
        );
    }

    #[test]
    fn a_rescan_drops_mail_that_is_no_longer_in_gmail() {
        // The drift this fixes: delete something in Gmail yourself, and Hush
        // kept showing it forever because a scan only ever added.
        let s = Store::open_in_memory().unwrap();
        let lu = Some("<https://x.example/u>");
        s.put_messages(
            ACC,
            &[
                msg("still-there", "a@x.example", "Hi", 5, lu),
                msg("user-deleted", "a@x.example", "Hi", 6, lu),
            ],
        )
        .unwrap();
        assert_eq!(s.senders(ACC).unwrap()[0].message_count, 2);

        // Gmail now returns only the one that survives.
        let removed = s.reconcile(ACC, &["still-there".to_string()], 0).unwrap();

        assert_eq!(removed, 1);
        assert_eq!(s.senders(ACC).unwrap()[0].message_count, 1);
    }

    #[test]
    fn reconciling_never_touches_mail_outside_the_scanned_window() {
        // A six-month scan says nothing about what exists before it, so it must
        // not delete rows it never went looking for.
        let s = Store::open_in_memory().unwrap();
        let lu = Some("<https://x.example/u>");
        s.put_messages(
            ACC,
            &[
                msg("old", "a@x.example", "Hi", 1, lu),
                msg("recent", "a@x.example", "Hi", 100, lu),
            ],
        )
        .unwrap();

        // Only the recent window was scanned, and it came back empty.
        let removed = s.reconcile(ACC, &[], 50 * DAY).unwrap();

        assert_eq!(removed, 1, "only the in-window message went");
        assert_eq!(s.message_count(ACC).unwrap(), 1);
        assert_eq!(s.senders(ACC).unwrap()[0].message_count, 1);
    }

    #[test]
    fn forgetting_is_scoped_and_safe_to_repeat() {
        let s = Store::open_in_memory().unwrap();
        let lu = Some("<https://x.example/u>");
        s.put_messages(
            ACC,
            &[
                msg("mine", "a@x.example", "Hi", 1, lu),
                msg("keep", "b@x.example", "Hi", 1, lu),
            ],
        )
        .unwrap();
        s.put_messages(
            "other@example.com",
            &[msg("mine", "a@x.example", "Hi", 1, lu)],
        )
        .unwrap();

        s.forget_messages(ACC, &["mine".to_string()]).unwrap();
        assert_eq!(s.message_count(ACC).unwrap(), 1, "only the one row went");
        assert_eq!(
            s.message_count("other@example.com").unwrap(),
            1,
            "another account's identically-named message is untouched"
        );

        // Repeating it, or forgetting nothing, must not error.
        s.forget_messages(ACC, &["mine".to_string()]).unwrap();
        s.forget_messages(ACC, &[]).unwrap();
        assert_eq!(s.message_count(ACC).unwrap(), 1);
    }

    #[test]
    fn tidying_up_never_reaches_another_sender_or_another_account() {
        let s = Store::open_in_memory().unwrap();
        let lu = Some("<https://x.example/u>");
        s.put_messages(
            ACC,
            &[
                msg("mine", "a@x.example", "Hi", 1, lu),
                msg("theirs", "b@x.example", "Hi", 1, lu),
            ],
        )
        .unwrap();
        s.put_messages(
            "other@example.com",
            &[msg("elsewhere", "a@x.example", "Hi", 1, lu)],
        )
        .unwrap();

        assert_eq!(
            s.bulk_message_ids(ACC, "a@x.example").unwrap(),
            vec!["mine"]
        );
        assert!(s
            .bulk_message_ids(ACC, "nobody@x.example")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn an_empty_header_on_recent_mail_does_not_hide_the_sender() {
        // Seen in the wild: the field is present but blank on the latest
        // message. Taking that one and stopping would drop a sender who is
        // plainly unsubscribable from their earlier mail.
        let s = Store::open_in_memory().unwrap();
        s.put_messages(
            ACC,
            &[
                msg(
                    "older",
                    "news@shop.example",
                    "Sale",
                    1,
                    Some("<https://shop.example/u>"),
                ),
                msg("newest", "news@shop.example", "Sale again", 9, Some("   ")),
            ],
        )
        .unwrap();

        let senders = s.senders(ACC).unwrap();
        assert_eq!(senders.len(), 1, "the sender must still be offered");
        assert_eq!(
            senders[0].method,
            UnsubMethod::ManualLink {
                url: "https://shop.example/u".into()
            }
        );
    }

    #[test]
    fn an_empty_unsubscribe_header_does_not_count_as_bulk() {
        // A header present but blank parses to nothing, so the sender is not
        // offered — and their mail must not be binnable either.
        let s = Store::open_in_memory().unwrap();
        s.put_messages(
            ACC,
            &[
                msg("blank", "a@x.example", "Hi", 1, Some("")),
                msg(
                    "real",
                    "a@x.example",
                    "Hi",
                    2,
                    Some("<https://x.example/u>"),
                ),
            ],
        )
        .unwrap();
        assert_eq!(
            s.bulk_message_ids(ACC, "a@x.example").unwrap(),
            vec!["real"]
        );
    }

    #[test]
    fn transactional_senders_that_do_carry_the_header_are_flagged_not_hidden() {
        let s = Store::open_in_memory().unwrap();
        s.put_messages(
            ACC,
            &[msg(
                "1",
                "service@paypal.com",
                "Your monthly statement",
                1,
                Some("<https://paypal.com/u>"),
            )],
        )
        .unwrap();
        let senders = s.senders(ACC).unwrap();
        assert_eq!(senders.len(), 1, "must be shown, not hidden");
        assert!(senders[0].assessment.caution, "must be flagged");
        assert!(!senders[0].assessment.reasons.is_empty());
    }
}
