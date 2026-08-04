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
        self.first_ms = self.first_ms.min(date_ms);
        self.last_ms = self.last_ms.max(date_ms);
        if self.subjects.len() < SAMPLE_SUBJECTS && !subject.is_empty() {
            self.subjects.push(subject);
        }
        // Take the most recent message that actually carried an unsubscribe
        // header; a sender's newest mail may be a receipt that has none.
        if self.lu.is_none() && lu.is_some() {
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
            first_seen_ms: if self.first_ms == i64::MAX {
                0
            } else {
                self.first_ms
            },
            last_seen_ms: self.last_ms,
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
        OutcomeStatus::Simulated => "simulated",
    }
}

fn status_from_str(s: &str) -> OutcomeStatus {
    match s {
        "done" => OutcomeStatus::Done,
        "sent" => OutcomeStatus::Sent,
        "needs_you" => OutcomeStatus::NeedsYou,
        "simulated" => OutcomeStatus::Simulated,
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
        s.set_setting("dry_run", "false").unwrap();

        s.erase(true).unwrap();
        assert_eq!(s.message_count(ACC).unwrap(), 0);
        assert_eq!(s.get_setting("dry_run").unwrap(), None);
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
