//! What happens at the size of a real, old mailbox.
//!
//! The app was reported laggy after a 26,000-message scan. Everything measured
//! before that point used a few hundred. These tests build a mailbox that size
//! and time the operations the interface performs, so "laggy" becomes a number
//! attached to a named function.

use std::time::Instant;

use hush_lib::model::MessageMeta;
use hush_lib::store::Store;

const ACCOUNT: &str = "me@example.com";

fn build(messages: usize, senders: usize) -> Store {
    let store = Store::open_in_memory().unwrap();
    let batch: Vec<MessageMeta> = (0..messages)
        .map(|i| {
            let s = i % senders;
            MessageMeta {
                id: format!("m{i}"),
                sender_address: format!("sender{s}@example.com"),
                sender_name: format!("Sender Number {s}"),
                subject: format!("A subject line of roughly typical length, number {i}"),
                date_ms: 1_600_000_000_000 + i as i64 * 60_000,
                // Two thirds carry the header, as in a real mailbox.
                list_unsubscribe: (s % 3 != 0)
                    .then(|| format!("<https://example.com/u/{s}>, <mailto:u{s}@example.com>")),
                list_unsubscribe_post: (s % 3 != 0).then(|| "List-Unsubscribe=One-Click".into()),
            }
        })
        .collect();

    let t = Instant::now();
    for chunk in batch.chunks(500) {
        store.put_messages(ACCOUNT, chunk).unwrap();
    }
    println!("  writing {messages} messages: {:?}", t.elapsed());
    store
}

#[test]
fn a_twenty_six_thousand_message_mailbox() {
    let store = build(26_000, 1_500);

    let t = Instant::now();
    let senders = store.senders(ACCOUNT).unwrap();
    let listing = t.elapsed();
    println!("  senders(): {:?} for {} senders", listing, senders.len());

    let t = Instant::now();
    let _ = store.known_ids(ACCOUNT).unwrap();
    println!("  known_ids(): {:?}", t.elapsed());

    let t = Instant::now();
    let _ = store.message_count(ACCOUNT).unwrap();
    println!("  message_count(): {:?}", t.elapsed());

    let busiest = &senders[0].address;
    let t = Instant::now();
    let msgs = store.subjects_for_sender(ACCOUNT, busiest, 5_000).unwrap();
    println!(
        "  subjects_for_sender(): {:?} for {} rows",
        t.elapsed(),
        msgs.len()
    );

    let t = Instant::now();
    let ids = store.bulk_message_ids(ACCOUNT, busiest).unwrap();
    println!(
        "  bulk_message_ids(): {:?} for {} ids",
        t.elapsed(),
        ids.len()
    );

    // The interface calls senders() on mount and after every run. Anything
    // over a tenth of a second here is felt.
    assert!(
        listing.as_millis() < 1_000,
        "senders() took {listing:?}, which the interface waits on"
    );
}

#[test]
fn a_hundred_thousand_message_mailbox() {
    // The next order of magnitude, because someone has one.
    let store = build(100_000, 4_000);
    let t = Instant::now();
    let senders = store.senders(ACCOUNT).unwrap();
    println!(
        "  senders(): {:?} for {} senders",
        t.elapsed(),
        senders.len()
    );
}
