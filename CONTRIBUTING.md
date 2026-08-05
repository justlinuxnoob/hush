# Contributing to Hush

Thanks for looking. This is a small, deliberately boring app, and the goal is to
keep it that way.

## Before you start

Please open an issue before writing anything substantial. It's much easier to
agree on an approach in a paragraph than in a diff.

## The rules that aren't up for negotiation

These aren't style preferences. A change that breaks one of them won't be
merged, however good the code is.

1. **No backend.** Hush talks to Google and to unsubscribe endpoints. Nothing
   else, ever. No analytics, no crash reporting, no update check, no "anonymous"
   telemetry.
2. **No embedded credentials.** The app ships with no client ID and no secret.
   Every user brings their own.
3. **`List-Unsubscribe` is the gate.** A sender with no unsubscribe header is
   never shown and never actionable. If you find yourself adding a way around
   this, stop.
4. **Never permanently delete anything, and never touch mail the user didn't
   ask about.** Hush unsubscribes; clearing out a sender's old newsletters is an
   opt-in extra that moves them to Trash and nothing more. Don't add archiving,
   labelling, filtering, or permanent deletion, and don't request a permission
   that would allow them.
5. **Only bin what carried an unsubscribe header.** The tidy-up reuses the same
   gate as unsubscribing, via `Store::bulk_message_ids`. A shop's receipts have
   no header and must survive. There are tests; keep them passing.
6. **No AI, no models, no inference.** Header parsing and grouping, nothing more.
   This is a deterministic tool and its predictability is the point.
7. **Bodies are never fetched.** `format=metadata` with a fixed header list.
   There is a test that fails if someone changes this.
8. **Nothing is pre-selected.** Every checkbox starts empty.

## Language in the interface

The person using this is not a developer. Outside the setup wizard, the words
"header", "API", "token", "OAuth", "scope" and "endpoint" should not appear
anywhere on screen. Neither should status codes, stack traces, or anything that
looks like a log.

"Google didn't accept the connection. Try connecting again." beats "401
Unauthorized". If you can't say it plainly, the design is probably wrong rather
than the wording.

## Safety changes need tests

Anything touching these needs test coverage in the same change:

- `src-tauri/src/parse.rs` — `List-Unsubscribe` parsing. Add your real-world
  malformed header to the fixtures; that's what the file is for.
- `src-tauri/src/heuristics.rs` — the transactional flags. A new rule needs both
  a case it catches and a case it must *not* catch. False positives cost one
  click; false negatives cost someone a receipt.
- `src-tauri/src/store.rs` — sender grouping, and the gate itself.
- `src-tauri/src/unsub.rs` — anything that sends a request, and anything that
  moves mail to Trash.

### Adding to the heuristics lists

The bank/airline/delivery lists are incomplete and always will be — they can't
be otherwise. Additions are welcome, with two asks:

- Use `Match::Label` for short tokens. `ups` as a substring matches
  `startups.com`; there's a test that catches this exact mistake.
- Add the case you're fixing *and* a plausible false positive to
  `plain_newsletters_are_not_flagged`.

## Running things

```sh
npm install
npm run tauri dev                                  # run the app
cargo test --manifest-path src-tauri/Cargo.toml    # Rust tests
npx tsc --noEmit                                   # frontend types
cargo clippy --manifest-path src-tauri/Cargo.toml -- -D warnings
cargo fmt --manifest-path src-tauri/Cargo.toml
```

The Gmail tests use a mock server ([`src-tauri/tests/gmail_api.rs`](src-tauri/tests/gmail_api.rs))
and never touch a real account. You don't need Google credentials to work on
most of the app.

### Working on the interface without a Google account

```sh
VITE_HUSH_DEMO=1 npm run dev
```

This swaps the Rust backend for a fake one ([`src/devmock.ts`](src/devmock.ts))
with sample senders covering the cases that are awkward to reproduce on demand:
a flagged bank, a link-only sender, a mailto-only sender, and one that's already
protected. Every screen is reachable.

It sits behind a compile-time constant, so a normal build drops it — and CI
fails if it ever turns up in `dist/`.

## Testing against a real inbox

Set up your own Google project — the app walks you through it — and use an
account you do not mind changing. There is no practice mode, deliberately: one
shipped, defaulted to on, and silently turned the entire app into a no-op for
every user who never found the toggle. See NOTES.md.

Two things make that safe enough to work with. Nothing is ever deleted
permanently — Hush does not hold a permission that would allow it, so the worst
case is mail in Trash for 30 days. And blocking defaults to archiving, which
deletes nothing at all.

`src-tauri/tests/live_filters.rs` runs a real create-list-delete round trip
against your own account and cleans up after itself. Run it after touching
anything in `filters.rs`, because a mock will agree with whatever you send it.

## Commit style

Plain sentences. Say what changed and why the change is correct. If it touches a
safety rule, say which one and how the tests cover it.
