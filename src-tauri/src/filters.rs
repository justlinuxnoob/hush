//! Reading, classifying and undoing the filters that blocking creates.
//!
//! The organising decision here is that **Gmail is the database**. Hush stores
//! no list of what it has blocked. Filters live on the account, so they are
//! read back live: they survive a reinstall, they are already there on a second
//! machine, and a future phone build gets them for nothing. It also means the
//! app cannot drift out of step with reality, because it never holds a second
//! copy of reality to drift from.
//!
//! That leaves one problem. If we do not remember which filters we made, we
//! have to be able to *recognise* them, and we must never delete one the user
//! wrote by hand. The marker is a Gmail label, `Hush`, applied by every filter
//! this app creates:
//!
//! - It rides inside the filter's own action, so it round-trips through the
//!   API as plain data rather than living anywhere it could be lost.
//! - It is visible. Someone reading their Gmail filter list can see where the
//!   rule came from, which beats a hidden marker they would have to trust.
//! - It labels the *mail* as well as identifying the filter, so undoing a block
//!   can put back exactly what that block caught, and nothing else.
//!
//! It can be defeated — delete the label in Gmail and Hush stops recognising
//! its own filters. The failure mode is the safe one: they become foreign, and
//! foreign filters are read-only.
//!
//! **On what has actually been proven.** Creating the label has been run
//! against a real account. The create-list-classify-delete round trip has only
//! been run against a mock, because the connection on the machine this was
//! written on holds the modify permission but not the settings one. A mock
//! agrees with whatever it is sent — that is how a completely dead trash
//! feature once passed a green suite — so `tests/live_filters.rs` exists to ask
//! Google the same question, and should be run before this is trusted.

use std::sync::Arc;

use serde::Serialize;

use crate::error::{Error, Result};
use crate::gmail::{BlockAction, Cancel, Filter, GmailClient};

/// The label Hush applies to everything it filters. Also the marker that makes
/// a filter recognisably ours.
pub const HUSH_LABEL: &str = "Hush";

/// Gmail's own reserved label ids that we reason about.
const TRASH: &str = "TRASH";
const INBOX: &str = "INBOX";

/// One filter on the account, as Hush understands it.
#[derive(Debug, Clone, Serialize)]
pub struct ManagedFilter {
    pub id: String,
    /// The address it matches. Empty for filters that match on something else.
    pub address: String,
    /// What it does, in plain language, whoever created it.
    pub summary: String,
    /// `Some` only for filters Hush created — the others are not ours to
    /// describe in our own vocabulary.
    pub action: Option<BlockAction>,
    /// Whether Hush created it, and may therefore offer to remove it.
    pub mine: bool,
}

/// What removing a block would affect, worked out before anything is removed.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RemovalPreview {
    pub address: String,
    pub action: Option<BlockAction>,
    /// Messages sitting in Trash that this block put there.
    pub in_trash: u64,
    /// Messages archived out of the inbox by this block.
    pub archived: u64,
    /// True when the counts are lower bounds rather than exact.
    pub approximate: bool,
}

/// What actually happened.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RemovalReport {
    pub filter_removed: bool,
    pub restored: u64,
    pub restore_failed: u64,
    pub problem: Option<String>,
}

/// Find the `Hush` label's id, creating the label if this account has never
/// had one.
///
/// Creating a label needs the modify permission. Blocking needs the settings
/// permission. They are separate grants, so a user can hold one without the
/// other — hence `Result`, and hence callers that treat failure as "block
/// without a marker" rather than "do not block".
pub async fn ensure_label(gmail: &Arc<GmailClient>, cancel: &Cancel) -> Result<String> {
    if let Some(id) = find_label(gmail, cancel).await? {
        return Ok(id);
    }
    let id = gmail.create_label(HUSH_LABEL, cancel).await?;
    log::info!("created the {HUSH_LABEL} label ({id})");
    Ok(id)
}

/// The label's id if it exists, without creating it.
pub async fn find_label(gmail: &Arc<GmailClient>, cancel: &Cancel) -> Result<Option<String>> {
    Ok(gmail
        .list_labels(cancel)
        .await?
        .into_iter()
        .find(|l| l.name.eq_ignore_ascii_case(HUSH_LABEL))
        .map(|l| l.id))
}

/// Every filter on the account, ours marked as such.
pub async fn list(gmail: &Arc<GmailClient>, cancel: &Cancel) -> Result<Vec<ManagedFilter>> {
    // Deliberately does not create the label. Listing is a read; a read should
    // not leave anything behind on the account.
    let marker = find_label(gmail, cancel).await.unwrap_or(None);
    let filters = gmail.list_filters(cancel).await?;

    let mut out: Vec<ManagedFilter> = filters
        .into_iter()
        .map(|f| describe(&f, marker.as_deref()))
        .collect();

    // Ours first — they are the ones that can be acted on — then by address so
    // the list does not reshuffle between visits.
    out.sort_by(|a, b| {
        b.mine
            .cmp(&a.mine)
            .then_with(|| a.address.to_lowercase().cmp(&b.address.to_lowercase()))
    });
    Ok(out)
}

/// Classify one filter and put its effect into words.
fn describe(f: &Filter, marker: Option<&str>) -> ManagedFilter {
    let mine = marker.is_some_and(|m| f.action.add_label_ids.iter().any(|l| l == m));
    let trashes = f.action.add_label_ids.iter().any(|l| l == TRASH);
    let archives = f.action.remove_label_ids.iter().any(|l| l == INBOX);

    let action = mine.then_some(if trashes {
        BlockAction::Trash
    } else {
        BlockAction::Archive
    });

    let summary = if mine {
        if trashes {
            "Moves their mail to Trash, where Gmail deletes it after 30 days"
        } else {
            "Keeps their mail out of the inbox. Nothing is deleted"
        }
        .to_string()
    } else {
        foreign_summary(f, trashes, archives)
    };

    ManagedFilter {
        id: f.id.clone(),
        address: f.criteria.from.clone(),
        summary,
        action,
        mine,
    }
}

/// Describe a filter the user wrote themselves. Hush has no business
/// paraphrasing these as if they were its own, so this stays descriptive and
/// admits when it does not know.
fn foreign_summary(f: &Filter, trashes: bool, archives: bool) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if trashes {
        parts.push("moves it to Trash");
    }
    if archives && !trashes {
        parts.push("skips the inbox");
    }
    if !f.action.forward.is_empty() {
        parts.push("forwards it");
    }
    if f.action
        .add_label_ids
        .iter()
        .any(|l| l != TRASH && !l.starts_with("CATEGORY_"))
    {
        parts.push("adds a label");
    }

    if parts.is_empty() {
        "One of your own filters".to_string()
    } else {
        format!("One of your own filters — it {}", join_words(&parts))
    }
}

fn join_words(parts: &[&str]) -> String {
    match parts {
        [] => String::new(),
        [one] => one.to_string(),
        [rest @ .., last] => format!("{} and {}", rest.join(", "), last),
    }
}

/// Look up one of our filters by id, refusing anything that is not ours.
///
/// Every mutating path goes through this. The check is done against Gmail at
/// the moment of the change rather than against whatever the interface was
/// showing, which may be minutes stale and may predate the user editing the
/// filter by hand.
async fn mine_or_refuse(
    gmail: &Arc<GmailClient>,
    id: &str,
    cancel: &Cancel,
) -> Result<(Filter, BlockAction)> {
    let marker = find_label(gmail, cancel).await?.ok_or_else(|| {
        Error::Other(
            "Hush can't find its label on this account, so it can't tell which filters are \
             its own. Nothing was changed."
                .into(),
        )
    })?;

    let filter = gmail
        .list_filters(cancel)
        .await?
        .into_iter()
        .find(|f| f.id == id)
        .ok_or_else(|| Error::Other("That filter is no longer on the account.".into()))?;

    if !filter.action.add_label_ids.contains(&marker) {
        return Err(Error::Other(
            "That filter wasn't created by Hush, so Hush won't touch it. You can remove it \
             yourself in Gmail's settings."
                .into(),
        ));
    }

    let action = if filter.action.add_label_ids.iter().any(|l| l == TRASH) {
        BlockAction::Trash
    } else {
        BlockAction::Archive
    };
    Ok((filter, action))
}

/// How much mail removing this block could put back.
pub async fn preview_removal(
    gmail: &Arc<GmailClient>,
    id: &str,
    cancel: &Cancel,
) -> Result<RemovalPreview> {
    let (filter, action) = mine_or_refuse(gmail, id, cancel).await?;
    let address = filter.criteria.from.clone();

    let (in_trash, t_more) = count(gmail, &trash_query(&address), cancel).await?;
    let (archived, a_more) = count(gmail, &archived_query(&address), cancel).await?;

    Ok(RemovalPreview {
        address,
        action: Some(action),
        in_trash,
        archived,
        approximate: t_more || a_more,
    })
}

/// Remove one of Hush's filters, optionally putting back the mail it caught.
///
/// The filter goes first. If restoring then fails halfway, the user is left
/// with no filter and some mail still out of the inbox, which is recoverable by
/// hand. The other order would risk restoring mail into an inbox that the still
/// live filter immediately sweeps out again.
pub async fn remove(
    gmail: &Arc<GmailClient>,
    id: &str,
    restore: bool,
    cancel: &Cancel,
) -> Result<RemovalReport> {
    let (filter, _) = mine_or_refuse(gmail, id, cancel).await?;
    let address = filter.criteria.from.clone();
    let marker = find_label(gmail, cancel).await?.unwrap_or_default();

    let mut report = RemovalReport::default();

    gmail.delete_filter(id, cancel).await?;
    report.filter_removed = true;
    log::info!("removed the filter blocking {address}");

    if !restore {
        return Ok(report);
    }

    // Out of Trash first: a message has to exist before it can be put in the
    // inbox. Untrashing alone returns it to wherever it was, which for mail
    // this filter caught means archived, so both steps are needed.
    for id in ids_for(gmail, &trash_query(&address), cancel).await? {
        if cancel.is_cancelled() {
            break;
        }
        match gmail.untrash_message(&id, cancel).await {
            Ok(()) => match gmail
                .modify_message(&id, &[INBOX], &[marker.as_str()], cancel)
                .await
            {
                Ok(()) => report.restored += 1,
                Err(e) => {
                    log::warn!("untrashed {id} but couldn't return it to the inbox: {e}");
                    remember(&mut report, e);
                }
            },
            Err(e) => {
                log::warn!("couldn't untrash {id}: {e}");
                remember(&mut report, e);
            }
        }
    }

    for id in ids_for(gmail, &archived_query(&address), cancel).await? {
        if cancel.is_cancelled() {
            break;
        }
        match gmail
            .modify_message(&id, &[INBOX], &[marker.as_str()], cancel)
            .await
        {
            Ok(()) => report.restored += 1,
            Err(e) => {
                log::warn!("couldn't return {id} to the inbox: {e}");
                remember(&mut report, e);
            }
        }
    }

    Ok(report)
}

fn remember(report: &mut RemovalReport, e: Error) {
    if report.problem.is_none() {
        report.problem = Some(e.to_string());
    }
    report.restore_failed += 1;
}

/// Mail this block sent to Trash. Scoped by the marker label, so mail the user
/// binned themselves before ever blocking the sender is left where it is.
fn trash_query(address: &str) -> String {
    format!("in:trash label:{HUSH_LABEL} from:{}", quote(address))
}

/// Mail this block took out of the inbox and did not delete.
fn archived_query(address: &str) -> String {
    format!(
        "-in:trash -in:inbox label:{HUSH_LABEL} from:{}",
        quote(address)
    )
}

/// Gmail's search syntax treats spaces as term separators, so an address is
/// quoted. Any quote inside it is dropped rather than escaped — an address
/// containing one is malformed, and a broken query would silently match the
/// wrong mail.
fn quote(address: &str) -> String {
    format!("\"{}\"", address.replace(['"', '\\'], ""))
}

/// Every message id matching a query, following pagination.
async fn ids_for(gmail: &Arc<GmailClient>, query: &str, cancel: &Cancel) -> Result<Vec<String>> {
    let mut ids = Vec::new();
    let mut token: Option<String> = None;
    loop {
        cancel.check()?;
        let page = gmail
            .list_messages(query, token.as_deref(), 500, cancel)
            .await?;
        ids.extend(page.ids);
        match page.next_page_token {
            Some(t) => token = Some(t),
            None => break,
        }
    }
    Ok(ids)
}

/// A count for the preview, and whether there is more beyond one page.
///
/// `resultSizeEstimate` is not used. It has already been caught lying on this
/// account — it is what produced "75 of 501" during scanning — and a number
/// shown next to a button that moves someone's mail has to be a real one.
async fn count(gmail: &Arc<GmailClient>, query: &str, cancel: &Cancel) -> Result<(u64, bool)> {
    let page = gmail.list_messages(query, None, 500, cancel).await?;
    let more = page.next_page_token.is_some();
    Ok((page.ids.len() as u64, more))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gmail::{FilterAction, FilterCriteria};

    fn filter(from: &str, add: &[&str], remove: &[&str]) -> Filter {
        Filter {
            id: "f1".into(),
            criteria: FilterCriteria {
                from: from.into(),
                ..Default::default()
            },
            action: FilterAction {
                add_label_ids: add.iter().map(|s| s.to_string()).collect(),
                remove_label_ids: remove.iter().map(|s| s.to_string()).collect(),
                forward: String::new(),
            },
        }
    }

    #[test]
    fn an_archive_filter_of_ours_is_recognised() {
        let f = filter("news@shop.com", &["Label_9"], &["INBOX"]);
        let d = describe(&f, Some("Label_9"));
        assert!(d.mine);
        assert_eq!(d.action, Some(BlockAction::Archive));
        assert!(d.summary.contains("Nothing is deleted"));
    }

    #[test]
    fn a_trash_filter_of_ours_is_recognised_and_says_so() {
        let f = filter("news@shop.com", &["TRASH", "Label_9"], &["INBOX"]);
        let d = describe(&f, Some("Label_9"));
        assert!(d.mine);
        assert_eq!(d.action, Some(BlockAction::Trash));
        assert!(d.summary.contains("30 days"));
    }

    #[test]
    fn a_filter_without_the_marker_is_never_ours() {
        // Identical in every other respect to one of ours. Shape is not
        // evidence; the marker is.
        let f = filter("news@shop.com", &["TRASH"], &["INBOX"]);
        let d = describe(&f, Some("Label_9"));
        assert!(!d.mine);
        assert_eq!(d.action, None);
        assert!(d.summary.starts_with("One of your own"));
    }

    #[test]
    fn nothing_is_ours_when_the_label_is_missing() {
        // The user deleted the label. Everything becomes read-only rather than
        // Hush guessing from the shape of the rule.
        let f = filter("news@shop.com", &["TRASH", "Label_9"], &["INBOX"]);
        assert!(!describe(&f, None).mine);
    }

    #[test]
    fn a_foreign_filter_is_described_not_paraphrased() {
        let f = filter("boss@work.com", &["Label_3"], &[]);
        let d = describe(&f, Some("Label_9"));
        assert!(!d.mine);
        assert!(d.summary.contains("adds a label"));
    }

    #[test]
    fn a_foreign_forwarding_filter_says_it_forwards() {
        let mut f = filter("boss@work.com", &[], &["INBOX"]);
        f.action.forward = "someone@else.com".into();
        let d = describe(&f, Some("Label_9"));
        assert!(d.summary.contains("forwards it"), "{}", d.summary);
        assert!(d.summary.contains("skips the inbox"), "{}", d.summary);
    }

    #[test]
    fn gmails_own_category_labels_are_not_read_as_labelling() {
        // Gmail writes CATEGORY_* into filters it manages itself. Calling that
        // "adds a label" would be technically true and useless.
        let f = filter("news@shop.com", &["CATEGORY_PROMOTIONS"], &[]);
        assert_eq!(
            describe(&f, Some("Label_9")).summary,
            "One of your own filters"
        );
    }

    #[test]
    fn the_search_queries_are_scoped_to_our_own_label() {
        // Without the label these would sweep up mail the user trashed or
        // archived themselves long before they ever blocked the sender.
        assert!(trash_query("a@b.com").contains("label:Hush"));
        assert!(archived_query("a@b.com").contains("label:Hush"));
        assert!(archived_query("a@b.com").contains("-in:trash"));
        assert!(archived_query("a@b.com").contains("-in:inbox"));
    }

    #[test]
    fn an_address_with_a_space_cannot_break_out_of_the_query() {
        let q = trash_query("odd address@b.com");
        assert!(q.contains("from:\"odd address@b.com\""), "{q}");
    }

    #[test]
    fn quotes_in_an_address_are_dropped_rather_than_escaped() {
        assert_eq!(quote("a\"b@c.com"), "\"ab@c.com\"");
        assert_eq!(quote("a\\b@c.com"), "\"ab@c.com\"");
    }

    #[test]
    fn ours_are_listed_first() {
        let mine = describe(
            &filter("z@z.com", &["Label_9"], &["INBOX"]),
            Some("Label_9"),
        );
        let theirs = describe(&filter("a@a.com", &["TRASH"], &[]), Some("Label_9"));
        let mut v = [theirs, mine];
        v.sort_by(|a, b| {
            b.mine
                .cmp(&a.mine)
                .then_with(|| a.address.to_lowercase().cmp(&b.address.to_lowercase()))
        });
        assert_eq!(v[0].address, "z@z.com");
    }
}
