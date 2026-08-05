# Notes

Written for whoever picks this up next — including me. Everything here is
something I guessed at, left unfinished, or decided in a way that deserves a
second opinion.

## What was actually verified

| | |
|---|---|
| Rust tests | 158 passing (142 unit, 16 against a mocked Gmail API) |
| Clippy | clean with `-D warnings` |
| Frontend | type-checks and builds |
| App launches | **yes, on Linux** — built, launched, every screen exercised |
| Release workflow | **yes** — all four installers built in public CI, checksums verified |
| Against a real Gmail account | deletion and blocking **both confirmed**; see below |
| macOS / Windows | **built, never run** — see below |

### What has and has not met the real Gmail

Confirmed against a live account:

- **Deleting** — `POST .../trash` returning `200` with `"labelIds": ["TRASH"]`,
  after the `411` fix.
- **Blocking** — the filter appears under Gmail → Settings → Filters and
  Blocked Addresses. `addLabelIds: ["TRASH"]` with `removeLabelIds: ["INBOX"]`
  is accepted, despite Google's filter documentation not listing `TRASH` among
  valid action labels. Worth recording, because that undocumented gap looked
  exactly like the `411` and turned out to be fine.
- **Scanning, grouping, the safety gate** — a real mailbox, 61 senders found
  from 200 messages, split 57 one-click / 2 mailto / 2 link-only.

Still only exercised against mocks: sending unsubscribe mail through Gmail, and
the count-then-read scan rework.

**How the block was nearly written off**: the tester looked for Gmail's "Block
sender" button, saw it still there, and concluded the filter had not been
created. That button is present on every message regardless of any filter, and
Gmail's own Block sends to Spam while this sends to Trash — different mechanism
entirely. The same shape of mistake as "I still see the unsubscribe link".
Neither button is state; both are always there.

### The original position, kept for context

At first there were no Google credentials available, so **no code path that
talks to Google had run against Google.** The Gmail client, the OAuth flow and the
scanner are covered by tests against a mock server and a local loopback, which
catches shape and logic errors but not the things only reality produces:
Google's exact error bodies, real rate-limit behaviour, oddities in real
`From` headers, or whether the consent flow feels right end to end.

**Before recommending this to anyone, run it against a real account with dry run
on.** Dry run exercises everything except the final send.

Likewise, **no unsubscribe request has ever been sent to a real endpoint.** The
one-click POST is built and unit-tested but has never met a live marketing
platform.

### Only *run* on Linux

All four installers now build in CI — `.msi`, `.exe`, universal `.dmg`, `.deb`
and `.AppImage` — and a downloaded `.deb` verifies against the published
`SHA256SUMS`. But **the macOS and Windows binaries have never been launched by
anybody.** Building is not running: a missing runtime dependency, a broken
keychain call, or a window that opens blank would all sail through a green
build. Someone should open them before the draft release goes public.

The first release run also cost three attempts, which is worth recording since
it says something about the workflow rather than the app:

1. `macos-13` (Intel) runners never allocated — queued 20+ minutes while the
   other three finished in 4–6. Replaced with a single universal build.
2. The universal build compiled and then failed at the link with only one
   architecture installed. `rust-toolchain.toml` pins the toolchain, so the
   toolchain action's `targets:` input put them where the build could not see
   them; an explicit `rustup target add` fixed it.
3. Green.

The AppImage step also fails on a sandboxed local machine because `linuxdeploy`
needs FUSE. On a CI runner it is fine.

## Things I had to guess at

### Gmail's quota numbers disagree between sources

Google's own [usage limits page](https://developers.google.com/workspace/gmail/api/reference/quota)
says `messages.get` costs **20** quota units. Several third-party summaries say
**5**. The per-user ceiling is documented as 6,000 units/minute; other sources
describe 250 units/second.

I couldn't resolve this, so I stopped trying to: the limiter in
`src-tauri/src/ratelimit.rs` starts conservatively and adapts to whatever the
real limit turns out to be, halving on any pushback. The published costs are
still in the code as starting values, and if they're wrong the loop absorbs it.

**Worth revisiting**: if the real numbers become clear, the `START_RATE` and
`MAX_RATE` constants could be tuned. A first scan of a large mailbox may be slow
— at 20 units per fetch and 100 units/second, 38,000 messages is over two hours.
That's why scan depth defaults to a year and why the scan is resumable.

### I skipped the batch endpoint

The brief asked for batching. I used HTTP/2 concurrency instead, and this is a
judgement call worth challenging.

Gmail's batch endpoint still exists (the *global* batch endpoint was shut down in
2020; the per-API one at `/batch/gmail/v1` was not). But Google's own docs say
batching **doesn't reduce quota** — *n* batched calls count as *n* calls. Quota
is the binding constraint here, not connection overhead, so batching would buy
round-trips we aren't short of while adding multipart assembly and parsing, and
a failure mode where one malformed part is harder to attribute.

If a real-world scan turns out to be latency-bound rather than quota-bound, this
decision is wrong and should be revisited.

### Google Cloud console URLs

The setup wizard deep-links to `console.cloud.google.com/auth/branding`,
`/auth/audience` and `/auth/clients` — the newer Google Auth Platform pages.
Google reorganised these once already and may again. **These are unverified: I
could not open them.** If setup reports are confusing, check these first
(`src/screens/Setup.tsx`).

### The client-secret format check

`ClientCredentials::validate` rejects anything not starting with `GOCSPX-`. That
prefix has been stable for years, but it is a Google implementation detail, not
a documented guarantee. If Google changes it, setup breaks with a confidently
wrong error message. The check exists because pasting the Client ID into both
boxes is a very common slip that otherwise fails much later and cryptically.

### The transactional heuristics are a starting point

The lists in `src-tauri/src/heuristics.rs` are Anglophone and incomplete —
mostly US and Western European institutions. They will miss banks and delivery
firms in most of the world.

The weights and the caution threshold (60) were set by hand until the test
fixtures behaved. They are not tuned against real data and probably should be.
Erring towards flagging is deliberate: a false positive costs one click, a false
negative costs someone a receipt.

## Decisions where I picked one way

### Both `mailto:` methods are implemented

The brief said pick one and document the trade-off. I built both, because the
trade-off is the user's to make rather than mine:

- **Hand off to their mail app** — the default. Needs no extra permission. Costs
  a click per sender.
- **Send via Gmail** — fully automatic, but requires `gmail.send`, which lets the
  app send mail as them. Off unless explicitly granted at the consent screen,
  and the app trusts what Google actually granted rather than what it asked for.

### Clearing out the backlog moves mail to Trash, never deletes it

Added after the first version, at the maintainer's request. Three decisions
inside it that another pair of eyes should check:

- **Only mail carrying an unsubscribe header is binned**
  (`Store::bulk_message_ids`). This reuses the unsubscribe gate, so a shop's
  order confirmations survive while its marketing goes. It is the reason the
  feature is safe at all, and there are tests pinning it.
- **`gmail.modify`, not `mail.google.com`.** The narrower scope permits
  trashing but not permanent deletion, so the app is structurally incapable of
  destroying anything even if the code were wrong.
- **`messages.trash`, not `batchModify` with a `TRASH` label.** Batching would
  be ~20x cheaper in quota (50 units per 1,000 vs 20 units each), but whether
  the API accepts `TRASH` via `addLabelIds` is disputed between sources and I
  could not settle it. A disputed reading is not something to lean on for a
  destructive call. **Worth revisiting** if someone confirms it: binning a
  600-message backlog currently takes a couple of minutes.

None of this has run against a real account either — see the top of this file.

### Subjects are stored locally

Needed by the safety heuristics — "your order has shipped" is exactly the signal
that earns a warning. Documented in the README rather than hidden.

### An unsubscribe URL is checked before anything is sent

Not in the brief, but the one-click URL comes from an email header, which means
a sender chooses it. Without a check, crafted mail could make the app POST to a
router admin page or a cloud metadata endpoint *from inside the user's network*.
`vet_destination` in `src-tauri/src/unsub.rs` resolves the host and refuses
anything not publicly routable.

**Known gap:** this is check-then-connect, so a DNS rebinding attack could still
slip through the window between the check and the request. Closing it properly
means a custom DNS resolver pinned to the vetted address. Given the attacker has
to control both the mail and the DNS, and the payload is a fixed form body, I
judged the remaining risk small — but it is a real gap, not an absent one.

### Header injection is refused rather than sanitised

A sender controls the `subject=` and address in their own `List-Unsubscribe`.
Line breaks are replaced with spaces, and an address that isn't a single plain
mailbox is rejected outright rather than cleaned up and used. There are tests
for both.

## Unfinished

### Reproducible builds

Claimed as a goal, not achieved. Done: pinned Rust toolchain
(`rust-toolchain.toml`), committed `Cargo.lock` and `package-lock.json`,
`CARGO_INCREMENTAL=0`, `codegen-units = 1`, `--remap-path-prefix` in the release
workflow, and SHA-256 checksums on every artifact.

Not done, and needed for byte-identical rebuilds: pinned system libraries and
linker, a fixed `SOURCE_DATE_EPOCH`, deterministic archive ordering inside the
installers, and pinned runner images (currently floating tags like
`ubuntu-22.04`). The README says plainly that checksums probably won't match yet
and explains what *can* be verified today.

### The setup wizard has no screenshots

It was built with placeholder frames for them and they were removed: the written
steps carry it on their own, and six empty boxes read as an unfinished app
rather than a considered one. If anyone adds real ones later, one per step in
`src/screens/Setup.tsx` is the natural place.

### Smaller gaps

- **One account at a time.** The database schema is keyed by account throughout,
  so multiple accounts would mostly work, but nothing in the interface offers it.
- **English only.** No i18n scaffolding at all.
- **Accessibility is reasoned, not tested.** Focus order, visible focus rings,
  ARIA labels on the drawn checkboxes, `role="alert"` on problems, and
  `prefers-reduced-motion` are all in place. It has not been through a screen
  reader.
- **The app icon is mine, not a designer's.** `src-tauri/icons/source.png`, with
  a small script's worth of thought behind it. Regenerate with
  `npx tauri icon <file>`.
- **The CI "promises" job greps.** It catches embedded credentials, a `fetch` in
  the web layer, and a `format=full`. Greps are crude and a determined change
  would slip past; it's a tripwire, not a proof.
- **`resume_session` assumes keychain storage** when restoring on launch, so a
  memory-only session that somehow restored would be labelled wrongly. Harmless
  today because a memory-only token cannot survive a restart.

## What a click-through of every control found

Every Tauri command was checked as registered and reachable, every button in the
interface was checked for a handler (40 of them), and the main screens were
driven end to end against the mock. That turned up three real things:

- **Binned mail left the counts stale.** Trashing a sender's backlog moved the
  mail at Gmail but never dropped the rows from the local cache, so the sender
  kept showing its old count and a second tidy-up would re-attempt messages
  already in the bin. Fixed with `Store::forget_messages`, called with the ids
  that actually moved; a test pins the whole cycle. Verified in the interface:
  a sender went from 612 to 122 after binning 490.
- **`never_touch_list` was registered but never called.** Removed — protected
  senders are already listed and un-protectable on the main screen, and an
  unused command is surface for nothing.
- **The demo mock was more permissive than the real backend.** It did not
  enforce the never-touch guard and did not record outcomes, so someone working
  on the interface could have concluded those features did nothing. Now mirrors
  the real guards.

Several other apparent failures turned out to be the test script rather than the
app — React controlled inputs read stale immediately after a click, and the data
file path lives in an `input` value where `innerText` cannot see it. Worth
knowing before chasing them again.

**Still unclicked:** the setup wizard's six steps were walked but not with real
credentials pasted, and nothing in the connect flow has been exercised, because
that needs Google.

## What one person actually using it found

Every item below was found by a real first run on Windows, and none of them by
the mock suite — which clicks every button and drives every screen. Worth
sitting with, because the pattern is consistent: the mock tests *logic*, and
every one of these was about **defaults, wording, or what happens when
something does not answer**.

- **Practice mode was on by default, labelled "Dry run", and silently made the
  whole app a no-op.** The user unsubscribed, binned mail, checked Gmail, found
  nothing changed, and reasonably concluded the app was broken. It was doing
  exactly as told. The toggle sat in a toolbar corner with its explanation in a
  hover tooltip — in an app whose own contributing guide bans jargon from the
  interface. Now: no toggle, and the confirmation screen makes you choose
  between "Try it first — nothing is sent, nothing is moved" and "Do it for
  real — this actually happens", with the counts spelled out.
- **Granted permissions were forgotten on every restart.** `resume_session`
  hardcoded the narrowest scopes, so each launch sent the user back through
  Google's consent page for something already granted. Now persisted.
- **A redirect from an unsubscribe endpoint counted as a failure.** RFC 8058
  says they must not redirect; a great many answer a successful POST with a
  302 to a "you have been unsubscribed" page. Those were reported as failures.
- **Only one route per sender was ever attempted.** Senders commonly publish
  both a one-click endpoint and a `mailto:`; if the first failed, the second was
  discarded and the sender reported as a failure they had not caused. The
  executor now works down every route the sender offers.
- **"Unsubscribed automatically" overclaimed.** A 200 means the sender received
  and accepted the request. Nothing in the protocol reports whether they acted
  on it. It now reads "Unsubscribe sent and accepted", and the link is kept even
  on success so there is a way to finish by hand when a sender ignores it.
- **`security@` and `notification@` were flagged identically**, which would have
  turned the safety warning into wallpaper — and wallpaper is what you scroll
  past on your way to unsubscribing from your bank.

## Practice mode is gone entirely

The original brief asked for a dry-run toggle, on by default for the first
launch. It was built exactly as specified and turned out to be the single most
damaging thing in the app.

A first-time user unsubscribed, binned mail, checked Gmail, found everything
untouched, and concluded — reasonably — that nothing worked. It had done
precisely as instructed. Hours went into hunting bugs elsewhere because the
symptom of "practice mode on" is identical to the symptom of "completely
broken", and the only clue was the word "Dry run" on a toggle in a corner.

It has been removed outright — the setting, the execution mode, the
`Simulated` outcome, all of it. What survives is the "show me exactly what gets
sent" panel on the confirmation screen, which is a *preview*: it prints the
literal request without running anything. That was always the useful half. Dry
run was an execution mode that pretended to work, which is a different thing and
only ever caused harm.

The safety this project actually rests on never depended on it: the
unsubscribe-header gate, the never-touch list, the transactional warnings, the
confirmation screen and Trash-rather-than-permanent-delete are all unchanged.

**The lesson is not "do not build dry-run modes."** It is that a default which
makes an app silently do nothing is indistinguishable from a broken app, and
the person who chose the default is the last one who will notice.

## Double protection had to be the default, not a tick-box

Blocking shipped in 0.4.0 as a checkbox beneath the action choice. The first
question asked about it was "can we do both — POST the unsubscribe *and* block?
double protection?" — which is precisely what had shipped the day before.

A feature nobody can find is a feature that does not exist. The confirmation
screen now asks two plain questions instead of one muddled one:

- **Stopping future emails** — unsubscribe and block *(recommended, and the
  default)*, unsubscribe only, or block only.
- **Their old emails** — move the backlog to Trash, or leave it.

They are genuinely independent, and conflating them was the mistake. Wanting a
sender's eight hundred old issues gone says nothing about whether you want to
keep receiving them, and asking a sender to stop says nothing about whether you
trust them to.

The action button spells out the result rather than naming a mode:
*"Unsubscribe and block 1 sender, bin 490 emails."*

## Where to ask for permissions, twice reconsidered

First version asked for everything on the connect screen as bare tick-boxes.
Fair criticism: nobody has seen a single sender at that point, so there is no
way to judge whether you want mail deleted, and declining meant coming back to
reconnect.

So it moved to just-in-time — each permission requested at the moment it was
wanted. Which read well and played badly, because blocking then became the
*recommended* action, so the ordinary path collected two extra trips through
the browser in the middle of a task.

It is back on the connect screen, with the thing that was actually missing the
first time: an explanation of what each permission does and what it cannot do.
The fact that made this easy was under-weighted originally — **Google's consent
screen already presents them as separate checkboxes**. Asking for three does not
impose three; it surfaces three decisions in one place, and the user declines
whichever they like on Google's own page. The narrow read-only path is still
offered for anyone who wants it, and the just-in-time prompts still exist for
whoever takes it.

The lesson is not that either placement is right. It is that "ask for less" and
"ask fewer times" pull in opposite directions, and the resolution was never
about placement — it was about explaining what is being asked for.

## The user is never given homework

The instruction was blunt and correct: *"if it can't accept by automatically
unsubbing just filter it out. I don't want the user to work."*

The app used to produce a to-do list. Senders offering only a link got "open
their page and press unsubscribe"; senders offering only a `mailto:` had a draft
opened in the user's mail client for them to send. Both are work, and removing
work is the entire point of the thing.

Blocking made that list unnecessary. Anything that cannot be completed without
the user is now reported as un-automatable and blocked instead — a filter needs
nothing from anybody and does not care what the sender does. The results screen
says "Blocked instead" rather than handing over a chore.

Gone with it: the `mailto:` draft handoff, `MailtoMode::HandOff` as a
destination, the `Handoff` type, `mark_manual_done`, and the tick-off list. A
`mailto:` is attempted only when Google has granted permission to send it, which
makes it genuinely automatic; otherwise the sender is blocked like any other.

One detail worth keeping: when a link *is* shown — for the curious rather than
as an instruction — it now prefers the sender's own preferences page over their
one-click endpoint. Opening a one-click endpoint in a browser is a GET, which
compliant senders ignore by design, so offering it would be worse than offering
nothing.

## Unsubscribing alone was never going to be enough

The complaint that produced this feature: "I don't want unhappy users saying
I'm still receiving mail."

Looking at how the established tools handle it turned up a line worth quoting:
every mass-unsubscribe tool is a different interface over **three** controls —
unsubscribe, block, or filter. Hush had built one of the three and called it
finished.

Unsubscribing is a *request*. It depends on the sender honouring it, doing so
promptly, and not having the user on four other lists under a different address.
Even a perfect implementation cannot promise the mail stops, which is why the
wording had to keep hedging.

A Gmail filter is not a request. `users.settings.filters.create` with
`criteria.from` and `action.addLabelIds: ["TRASH"]` is a rule in the user's own
account, and it works identically whether the sender is scrupulous, slow, or
ignoring the unsubscribe outright. It needs `gmail.settings.basic`, it is
visible and removable under Settings → Filters in Gmail, and it trashes rather
than destroys.

So the answer to "I'm still receiving mail" is: unsubscribe *and* block. The
first is polite and stops it at source; the second is the guarantee. Blocking is
opt-in, per run, like everything else here.

**Worth keeping in mind**: this is the same lesson as the transactional-mail
gate. When something cannot be guaranteed, either find a mechanism that can, or
say plainly that it cannot — and never paper over the gap with confident
wording.

## Deletion never worked: 411 Length Required

Every trash request this app ever made was rejected by Google, and it took a
logger, a diagnostic against a real account, and most of a day to find out why.

Gmail's `users.messages.trash` takes no request body. A bodyless `reqwest` POST
omits the `Content-Length` header entirely rather than sending `0`, and Google
answers that with **411 Length Required**. Setting an explicit empty body
produces `Content-Length: 0` and the same request succeeds.

Proven side by side against a live account, same message and token:

| Request | Response |
|---|---|
| `POST .../trash` with no body | `411 Length Required` |
| `POST .../trash` with an explicit empty body | `200 OK`, `"labelIds": ["TRASH"]` |

Three things made this take far longer than it should have:

- **No logger.** The status code was captured and then discarded in favour of a
  friendly sentence, so the one fact that identified the fault never reached
  anyone.
- **411 is not a status anyone expects.** It fell into the catch-all
  "unexpected response" branch, alongside genuinely unknown failures.
- **The mocked tests passed.** `wiremock` accepted the bodyless POST that Google
  rejects, so a full green suite proved nothing about the thing that mattered.
  The test added with the fix asserts on the header itself, and was checked by
  removing the fix and watching it fail.

**The lesson**: mocks agree with you. The mock server was more permissive than
Google in exactly the way that hid a total feature failure, and no amount of
test coverage against it would ever have found this.

## The scan counts before it reads

Gmail's `resultSizeEstimate` was the only total available, because listing and
fetching were interleaved — a page of ids, then that page's metadata, then the
next page. So the number beside the progress bar was Gmail's guess, and Gmail's
guess is wrong often enough to be worse than no number: a scan would go past
"501 messages" and keep counting upward past it.

Listing ids costs 5 quota units per 500 messages. Counting an entire mailbox
first is a few seconds and a rounding error of quota, and every number shown
afterwards is then a real count of real ids. The scan is now two passes —
"Counting your emails, 12,431 found so far", then "Reading your emails, 3,120 of
12,431" — and the progress bar means something.

Removing the number would have been the easier fix and the wrong one. It was
never impossible to get a true total; it just needed the work.

## Blocking silently did nothing without the permission

Reported as "it just didn't block". Confirmed against the live account in a
minute: `403 — Request had insufficient authentication scopes`. The token had
`gmail.readonly` and `gmail.modify` and had never been through a consent asking
for `gmail.settings.basic`.

Worth stating for anyone who assumes otherwise: **the Google Cloud setup does
not need changing.** Scopes are requested at sign-in, and a test user on a
project in Testing mode can grant any of them without pre-registration. The
wizard is fine.

The bug was that the app let the user choose blocking, did nothing, and said
nothing. Three fixes:

- A missing permission no longer fails the whole run. It used to return an error
  *after* the unsubscribes had gone out, throwing away results for work that had
  already happened. It is now reported as a failed block alongside the successful
  unsubscribes.
- The results screen distinguishes "nothing was blocked because Google refused"
  from a successful block, and when the cause is the permission it says which
  button to press.
- Blocks are confirmed by listing the account's filters afterwards, the same way
  binning is confirmed by re-querying the inbox. Claiming success on the strength
  of an HTTP status is a statement about our request; asking Gmail what filters
  exist is a statement about the account.

## Stop did not stop

Starting a scan cancels any scan already running, so that a wedged one can be
replaced. But a finishing scan cleared the shared cancel handle
unconditionally — including when a *later* scan had already taken ownership of
it. The later scan then ran with a Stop button wired to nothing.

A cancel handle now knows which operation it belongs to, and only clears the
shared slot if it is still its own.

## There was no logger

The most consequential omission in the project. Every failure path called
`log::warn!` — a message that could not be moved to Trash, a mail app that never
opened, a fetch that was skipped — and no logger was ever installed, so all of
it went to nowhere.

The effect was that "it doesn't delete my emails" could not be answered by
anyone, including the person whose machine it was happening on. Google's actual
refusal was discarded at the point it arrived. Several hours went into guessing
at causes that a single log line would have settled.

There is now a file logger writing to `hush.log` beside the database, a button
in Settings to open that folder, and — more importantly — the reason for a
failure is carried back into the interface rather than only into the log. A
tidy-up that moved nothing now says whether that is because there was nothing to
move or because every request was refused, and quotes the refusal.

**The lesson**: an app that cannot explain its own failures cannot be debugged
by its users, and its users are the only people who will ever encounter most of
its failures.

## A rescan reconciles rather than only adding

Found by the obvious question: does "Scan again" actually rescan?

It did not. A scan skipped ids it already held and only ever inserted, so mail
deleted in Gmail — by the user, or by a previous tidy-up — stayed in the local
list forever and nothing corrected it. The list drifted from the mailbox and
kept drifting.

A completed scan now records every id Gmail returned and drops local rows in the
scanned window that were not among them.

The first attempt at this **deleted the entire local database on every scan** —
the list of seen ids was declared and never filled, so everything looked absent.
The mocked API tests caught it immediately, which is the clearest argument for
them this project has produced. Worse, the same edit landed in the incremental
path, where reconciling is categorically wrong: a history sweep reports only
what *changed*, so treating it as the full picture would delete every message it
did not mention. Both are now pinned by tests that assert survival rather than
deletion. Listing ids costs 5 quota units per
500, so reconciliation is nearly free. Two things make it safe: it is scoped by
date, because a six-month scan says nothing about last year, and it only runs on
a scan that *finished*, because a cancelled one has not seen everything and
would delete the remainder.

## Handled senders leave the list

They stayed put looking untouched after being unsubscribed, which reads as
nothing having happened. A list that never shrinks as you work through it makes
the work feel imaginary. They now drop out of every view except the "Already
done" filter, with a count of how many are hidden.

## Unsubscribing and binning are separate choices

They were welded together: you could unsubscribe, or unsubscribe *and* bin, but
there was no way to clear out a sender's backlog while staying subscribed. That
is a real thing to want — a newsletter worth keeping whose eight hundred old
issues are not. The confirmation screen now offers three: unsubscribe only, bin
only, or both.

## Senders that accept a one-click POST and then still want a click

Found by a user opening the "check it yourself" link after the app reported
success, and landing on a page saying "Yes, unsubscribe".

RFC 8058 forbids this outright — returning a confirmation page in response to a
one-click POST violates the spec, and Gmail treats it as a complaint. Senders do
it anyway. There is no way to tell from the response: a correct implementation
and a broken one both return 200 with an HTML body, and sniffing the body for a
form would misfire on the many correct implementations that return a "you have
been unsubscribed" page.

So the app cannot detect it, and says so instead. A successful one-click now
reports "Their server accepted it" rather than any claim of being finished, the
link survives, and the section explains that a few senders accept the request
and still want a button pressed. Overstating this was the actual bug: the
interface said "accepted" and then offered a link that contradicted it.

## `mailto:` handoff does nothing on a machine with no mail app

Also found in real use, on Windows. The default for `mailto:` senders is to open
a prefilled draft in the user's own mail client. Most Windows installs have no
`mailto:` handler at all, because people use webmail — so the draft opens
nowhere, silently, while the app claimed one was waiting.

Nothing in the app can fix the missing handler. What it can do is stop depending
on it: the outcome now carries the address and the draft link whether or not
anything opened, and the results screen shows who to write to. Sending it by
hand from webmail takes ten seconds once you know the address.

## Cancellation, and where it was missing

Long operations that cannot be abandoned are the recurring failure in this app,
and each one was found the same way — by someone waiting on a spinner.

| Operation | Worst case before | Now |
|---|---|---|
| Connecting to Google | 5 minutes, no button | Stop waiting, noticed within 250ms |
| Scanning | already cancellable | unchanged |
| Unsubscribing a batch | 20s timeout × senders, no button | Stop, noticed mid-request |
| Binning a backlog | thousands of calls, no button | shares the run's cancel |

The unsubscribe case was the worst: fifty senders against slow endpoints is
sixteen minutes with nothing to press. Cancellation is checked between senders
*and* inside a request in flight, so Stop is honoured within a quarter second
rather than at the next timeout boundary. Whatever completed stays completed and
is reported; the rest simply never happens. Two tests cover it, one asserting
that stopping beats the HTTP timeout.

## What the wider ecosystem gets wrong

Researched rather than assumed, and it changed the code:

- **405 Method Not Allowed is the commonest one-click failure by a distance.**
  Senders publish an endpoint in the header that only accepts GET, so it works
  when clicked in a browser and refuses a POST. 401/403 is the same story with
  a login in front. All of these are finishable by hand, so they now produce
  "This sender's unsubscribe only works in a browser" plus the link, instead of
  a status code and a dead end.
- **Redirects are common despite being forbidden.** RFC 8058 says a one-click
  endpoint must not redirect; many answer a successful POST with a 302 to a
  confirmation page. Treated as delivered, not failed.
- **A 2xx is not proof of anything.** Advice to senders is explicitly "return
  200 fast, process asynchronously" — so a success response can precede the
  actual unsubscribe by some margin, and can equally precede nothing at all.
  This is why the wording is "sent and accepted" rather than "unsubscribed",
  and why the link survives a success.

## What earlier testing found

The first person to actually run it hit two things no amount of mock testing
would have caught, both now fixed in 0.1.1:

- **The app froze on "Connect with Google".** `wait_for_code` blocked for the
  full five-minute consent timeout with no way out, and the interface offered
  no cancel — so anything that went wrong in the browser left a dead window.
  The wait now wakes every 250ms to check for cancellation, there is a
  `cancel_connect` command, and the screen has a "Stop waiting" button. A test
  covers it.
- **The extra permissions were asked for at the worst possible moment.** The
  connect screen offered "bin my old emails" and "send mail as me" as tick-boxes
  *before the user had seen a single sender*. There was no way to answer
  sensibly, and declining meant coming back to reconnect. Both are now asked for
  in context — the tidy-up on the confirm screen where the counts are visible,
  the send permission when that mode is chosen in Settings — each opening
  Google's consent page once, at the moment the user has asked for the thing.

The lesson worth keeping: the mock exercised every screen and every button and
found neither, because both are about *when* something is asked and *what
happens when it does not answer*. Those only show up against the real thing.

## Prior art

Worth reading before changing the approach. Several open-source Gmail
unsubscribers exist, and they made a different trade:

- [justjake/gmail-unsubscribe](https://github.com/justjake/gmail-unsubscribe) —
  Apps Script. Uses `List-Unsubscribe` including one-click, *and* falls back to
  scraping the HTML body for links containing "unsubscribe" and firing requests
  at them. No protection against transactional mail; the user controls it by
  labelling threads by hand.
- [labnol/unsubscribe-gmail](https://github.com/labnol/unsubscribe-gmail) —
  the most widely used Apps Script version.
- [zbowling/gmail-ai-unsub](https://github.com/zbowling/gmail-ai-unsub) — a CLI
  that identifies marketing mail with an LLM and drives a browser.

The body-scraping fallback is the interesting one: it finds senders Hush cannot,
which is a real advantage, at the cost of reading message bodies and firing
requests at links nobody vetted. That is precisely the trade this project
declines — see the deliberate gap in coverage described above. Anyone arguing
Hush should scrape bodies should read those projects first and be explicit about
giving up the "we never read your mail" claim.

## Working on the interface without a Google account

`VITE_HUSH_DEMO=1 npm run dev` swaps the Rust backend for a fake one
(`src/devmock.ts`) with sample senders covering the awkward cases: a flagged
bank, a link-only sender, a mailto-only sender, and one already protected.

It's behind a compile-time constant so production builds drop it entirely, and
CI fails if it ever appears in `dist/`. This is how the list, confirm and results
screens were verified.
