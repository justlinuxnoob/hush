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

## Nothing you need is ever below the fold

"i hate how you have to scroll down in this app in random menus." Measured at
the smallest window the app allows — 720x560 — and the connect screen's own
Connect button was 74 pixels past the bottom edge. The setup wizard's Next and
the results screen's way back were the same.

Every screen with a way forward now pins that row to the bottom of the window,
and Settings pins its Close to the top, so the way out of the longest page in
the app is reachable from the end of it. Content scrolls; the decision does
not move.

The general rule: whether a screen fits is a question about someone else's
window size, font size and display scaling, none of which we get to know. So
never answer it by looking at your own screen.

## The gate had a door in it

Binning a backlog is header-gated: only mail that carried `List-Unsubscribe`
can be moved, which is the mechanism that keeps receipts out of it. The block
filter was not gated at all. It matched on the address, so a shop that sends its
newsletter and its order confirmations from one address would have had every
future receipt trashed, thirty days from permanent deletion.

The exact harm the app exists to prevent, walking in through the one door the
gate did not guard — and it shipped in 0.5.0 as the *recommended* action.

The fix is not a better heuristic. It is that blocking now archives by default:
mail leaves the inbox and stays in the account, searchable, forever. Trashing is
still there, behind an explicit second tick that names what could be lost, and
it is visually de-emphasised when any selected sender looks transactional.

Worth naming the general shape, because it will recur: **two features that are
each safe can be unsafe as a pair.** The gate was real and the filter was real.
Nobody wrote "and future receipts get deleted" — it fell out of combining them.
Whenever a new capability lands next to an old protection, the question to ask
is not "is this safe" but "does this reach around that".

## Gmail is the database

The Blocked senders screen reads filters live from the account instead of
keeping a local list of what Hush blocked. That was the cheap option and it is
also the right one: no second copy to drift, correct on a machine that has never
seen the account, correct after a reinstall, free on a phone build if there ever
is one, and nothing about what you blocked stored on your disk.

The cost is that Hush has to recognise its own work rather than remember it. The
marker is a Gmail label, `Hush`, applied by every filter it creates. Considered
and rejected: a token in `negatedQuery` (invisible, and it quietly changes what
the filter matches), and inferring from the filter's shape (a user's own
`from:x → TRASH` rule is byte-identical to ours — shape is not evidence).

The label won because it does a second job. It tags the *mail* as well as the
filter, so unblocking can restore exactly what that block caught and leave alone
what the user filed themselves. Matching on `from:` alone would have swept up
mail they archived by hand months earlier.

It can be defeated: delete the label and Hush stops recognising its filters. The
failure mode is the safe one — everything becomes foreign, and foreign filters
are read-only.

## What Gmail will not tell you

The filters API returns `id`, `criteria` and `action`. **No creation date.** The
brief asked for "when it was created — where derivable", and the honest answer
is that it is not derivable: the id is opaque, and there is no timestamp
anywhere in the response. So the screen does not show one. Inventing a
plausible-looking date would have been worse than the blank.

## A live test, because mocks agree with you

`tests/live_filters.rs` does a create-list-classify-delete round trip against a
real account, ignored by default, cleaning up after itself.

It exists because of the 411: a fully green suite while the trash endpoint
rejected every real request, since wiremock accepted the bodyless POST that
Google does not. "Does a user label survive being written into a filter action
and read back" is exactly that shape of question, and no mock can answer it.

Status as of writing: the label creation half has been run against a real
account and works. The filter round trip has not — the connection on the
development machine holds `gmail.modify` but not `gmail.settings.basic`, so
Google refuses the create. The mocked round trip passes, which is worth
precisely what the 411 taught us it is worth. This is written down rather than
glossed because a comment claiming "verified against a real account" was in the
source for about ten minutes before it was true, and that is how the last one
happened.

## The dry run that was asked for again

The brief for this pair of features asked to "update the dry-run path to cover
both". There is no dry-run path. It was removed in 0.4.0 after it shipped on by
default and silently turned the whole app into a no-op for anyone who never
found the toggle — the single worst bug in this project's history, and the user
who hit it was the one who asked for the feature in the first place.

Not rebuilt. Recorded here so the next person to ask knows it was considered.
What replaced it: nothing is preselected, the confirm screen states exactly who
and how many, the request itself is inspectable behind a disclosure, blocking
archives by default, and every block is reversible from inside the app. That is
a better answer to "let me check before I commit" than a mode that changes what
the buttons do.

## 559ms to tick a checkbox

"app feels laggy idk why like unoptimized or something." It was, and it was one
line.

`toLocaleDateString` and `toLocaleString` build a fresh `Intl` formatter on
every call, and building one costs about 1.5ms in WebKit. Each sender row
rendered two dates and a count, and ticking any checkbox re-rendered the whole
list — so at 157 senders that was roughly 470 `Intl` constructions per
interaction. Measured: **559ms per click.**

Hoisting the two formatters to module scope, plus a `memo` on the row so one
tick stops re-rendering the other 156, took it to **21–35ms**. A search
keystroke went to 31ms.

Both fixes are ordinary. The part worth remembering is that the report was
"feels laggy", which is unfalsifiable, and the fix needed a number. Ten minutes
with `performance.now()` in the real list turned a vibe into a one-line cause.
Guessing would have produced a virtualised list — a week of work, aimed at the
wrong thing.

## A blocked sender kept coming back

Gmail's search excludes Trash by default, so a sender blocked with the Trash
action vanishes from later scans for free. A sender blocked with the *archive*
action does not: their mail is still in All Mail. Every rescan picked it up and
offered them again as fresh work, as though the block had never happened.

Fixed by adding `-label:Hush` to the scan query, which excludes exactly the mail
our own filters caught. Their older mail, from before the block, is unlabelled
and still appears — correctly, because that backlog is still sitting there.

Noting the shape, because it is the same one as the block-filter asymmetry above:
**the archive path was added later and inherited none of the assumptions the
trash path had been getting away with.** Trash was special-cased by Gmail's own
defaults, so nobody had to think about it. Archive had no such luck.

## "no option to archive old emails wtf?"

Correct, and inconsistent. Blocking had just gained an archive-or-trash choice
on the grounds that "get this out of my inbox" and "delete this" are different
wishes. The backlog — the *other* place mail gets moved — still only offered
Trash. Same fix, same default, same explicit tick for the destructive option.

Archived backlog gets the `Hush` label, so it is findable in Gmail under one
name and skipped by later scans, matching what blocking does.

## The list never showed anything as done

`isHandled` required `bulk_count === 0` as well as a successful outcome, so a
sender whose backlog had not been cleared stayed in the main list. The reasoning
was that a *failed* bin leaves work outstanding — but it conflated that with
never having been asked to bin, and binning is opt-in and off by default. So the
ordinary path, unsubscribing and nothing else, marked nothing as done, ever. The
list never shrank and "Already done" stayed empty.

## Five messages a second, and nothing can change that

`messages.get` costs 20 quota units and the per-user ceiling is 100 units a
second. That is five messages a second, full stop — no amount of concurrency,
batching or cleverness moves it. Measured against a real account: **12,823
messages, about 43 minutes.**

Batching was the obvious idea and it is worthless here. Gmail's `/batch`
endpoint bundles up to 100 sub-requests into one round trip, but each inner
request still costs its own quota, and we are quota-bound rather than
latency-bound. It would save nothing.

What *is* ours to choose is the order those 43 minutes happen in.
`label:^unsub` is an undocumented internal Gmail label for mail with an
unsubscribe option, and on that same account it covered 5,527 of the 12,823.
Reading that portion first means most senders surface early, and "stop and use
what's found" stops costing much.

It is an ordering hint and nothing more, because the probe said so. Sampling
the *excluded* side found one message in forty that carried the header anyway —
so excluding on it would silently hide senders, which is the one failure this
app cannot have. Everything still gets read; the header parse is still the only
thing that decides. If Google drops the operator the first pass errors, gets
logged, and the second pass covers the mailbox exactly as before. There is a
test for that.

The general lesson: **an undocumented shortcut is fine as an optimisation and
never as a gate.** The question to ask is not "does it work" but "what happens
to the user when it is wrong", and here the answer had to be "nothing".

## Guessing would have cost a week

`tests/live_probe.rs` is read-only and asks Google things no mock can answer.
It was written because the alternative to twenty minutes of probing was
implementing a batch endpoint that would have saved zero seconds, or trusting a
blog post about `label:^unsub` and shipping a scanner that hides senders.

Same shape as the 559ms checkbox: the fix took one line, finding it took ten
minutes of measurement, and every plausible guess pointed somewhere else.

## What 26,000 emails actually costs

"the app seemed laggy when i fetched 26k emails." Everything measured before
that point used a few hundred senders. So: build a mailbox that size and time
the things the interface waits on.

Store, at 26,000 messages / 1,000 senders (debug build, so release is several
times quicker):

| | |
|---|---|
| `senders()` | 263ms |
| `known_ids()` | 34ms |
| `message_count()` | 7ms |
| `subjects_for_sender()` | 0.16ms |
| `bulk_message_ids()` | 0.07ms |

At 100,000 messages `senders()` is 811ms. Worth watching, not the problem.

Interface, at 1,507 senders — **31,709 DOM nodes**, 552ms to tick a checkbox,
89ms per keystroke in the search box. That is the lag, and it is not memory:
the JS heap was 55MB and the database is 16MB for the user's real 26,000
messages, about 615 bytes each. Nothing is downloaded but headers; there was
never anything to trim there.

Rendering is now windowed at sixty rows, extended as the list is scrolled.
31,709 nodes becomes 1,324, ticking goes 552ms to 11ms, and the heap drops to
18MB. Everything that must be *correct* — the tally, the search, "pick the
automatic ones" — still runs over every matching sender. Only the rendering is
windowed, which is the distinction that keeps it honest.

## Two bugs found only by scrolling to the bottom

The first window implementation used an `IntersectionObserver` on a sentinel
row. It never fired — not with the default root, and not with the scroll
container passed explicitly. Constructing one by hand in the page confirmed
zero callbacks with the sentinel 30 pixels below the fold. Replaced with
arithmetic on `scrollTop`, which cannot be ambiguous. WebKitGTK is the runtime
on Linux and it is not the engine to be clever in.

The second was self-inflicted: calling the check once on mount, so a list that
happened to start near its bottom revealed a window, which changed the
callback's identity, which re-ran the effect, which checked again. A loop that
renders every row at once — precisely the thing being avoided. There is no
mount-time check now, and the "Show more" button is the guaranteed path.

## The freeze

Asked to look for ways to hang the app, and there was one.

`tidy_up` and `block_senders` both took a read guard on the session and held it
across the whole operation — minutes, for a few thousand messages. Tokio's
`RwLock` is write-preferring, so:

1. A run starts and holds the read guard.
2. The user clicks Reconnect. `connect` queues for the write lock.
3. Every reader after that queues behind the writer — including `status`,
   which the interface polls.
4. The window stops repainting until the run finishes.

Fixed with `session_parts()`, which copies out the client handle and the three
permission flags and drops the guard. There is a test that reproduces the
starvation and one that asserts `session_parts` leaves no guard alive.

The rule, written down because it will come up again: **nothing that awaits the
network may hold a lock the interface needs.** Take what you need, drop the
guard, then go and do the slow thing.

## The app did not know what it was allowed to do

"how does the app know? what if its mistaken?" — a fair question with an
uncomfortable answer: it did not know. Every permission decision ran off a
scope string cached when the user connected. Right almost always, and wrong in
the one case that matters — access revoked from a Google account page, where
nothing tells the app. It would keep believing it could create filters until
something failed halfway through a run.

Google's `tokeninfo` endpoint reports what a live token may actually do, and
Settings now has a **Check everything** button that uses it. Nothing on that
screen is taken on trust: permissions come from tokeninfo, reading is proved by
reading, filters are proved by listing them. Where the live answer disagrees
with the cache, the cache is corrected and the user is told to restart.

Every line carries a fix, not just a verdict, and the results copy to the
clipboard as plain text for a bug report — there is no Hush server to send
them to, which is rather the point.

## The last unverified thing is verified

`tests/live_filters.rs` had been written but never run, because the machine it
was written on lacked the settings permission. It has now run against a real
account and passes: the marker label survives create, list, classify, delete.
The doc comment in `filters.rs` claiming verification is finally true, having
spent a while being corrected to say it was not.

## Erase everything did not erase everything

Asked to go hunting, and the first thing found was the worst.

`disconnect` and `erase_everything` wiped the database without stopping a scan.
A scan holds its own `Arc<Store>` and writes to it for as long as it runs —
forty minutes on a large mailbox. So: start a scan, go to Settings, press
"Disconnect and erase everything", watch it succeed, and watch the scan quietly
refill the database it just emptied. For an app whose entire promise is that
the data is yours and local, an erase that does not erase is the worst
available failure.

It also revoked the token out from under a live scan, so every request in
flight started failing against a dead credential.

Both cancels are now taken and fired before anything is touched. Three tests.

## Never your own mailbox

There was no protection against unsubscribing from — or blocking — your own
address, and it is more reachable than it sounds. Mail arrives from your own
address routinely: aliases, notes to self, calendar invites. Spoofed spam
forging the recipient's own address is ordinary, and it carries unsubscribe
headers like everything else.

So your address appears in the list, and blocking it writes a Gmail filter that
archives or trashes *everything you send yourself*. On the Trash setting that
is silent deletion of your own mail after thirty days.

The gate sits next to the header gate in `Group::finish`, because that is where
"never offer this" belongs, and `resolve` inherits it — which is the bit that
matters, since a gate that only filters the displayed list is one a stale
selection walks straight through. There is a test for the acting path
specifically.

Matching had to go further than `normalise_address`, which handles case and
`+tags`: Gmail ignores dots in the local part, so `j.o.e@gmail.com` is the same
mailbox as `joe@gmail.com`, and spoofing exploits exactly that. Dots are
stripped for Google's domains only — everywhere else a dot is a real character
and merging on it would fuse two different senders.

There is no setting for either of these. Neither is a decision worth offering.

## Designing an app you have never seen

Every design decision in this project up to 0.9.2 was made without looking at
it. The browser pane's screenshot call timed out every time, so what got
measured instead was `getBoundingClientRect`, DOM node counts and computed
styles — real numbers, and completely blind to how any of it looked.

Installing a headless Chromium and taking actual screenshots found, in about
five minutes, three things that months of reading the CSS had not:

**The sticky bars were see-through.** Both used a gradient fading to
`--paper`. That reads beautifully in a mock-up and fails the moment anything
scrolls past: the transparent end lets content through, so Settings rendered a
paragraph *behind its own title* and the confirm screen rendered its next
option underneath its own buttons. Solid backgrounds now.

Fixing that exposed a second layer of the same bug. `top: 0` sticks an element
below the scroll container's *padding*, so there is a band above the bar where
content still scrolls past in the open. A solid background on the element does
not cover it; a tall `::before` does.

**Hidden buttons were reserving space.** Each row's actions are revealed on
hover, at `opacity: 0` — but still in the layout, holding about forty pixels
on every single row whether shown or not. That, more than padding, is why an
800-pixel window showed three senders. Absolutely positioned now, and rows
went from ~150px to ~100px.

**The disabled primary button was invisible in dark mode.** 42% opacity on a
dark accent against a dark surface, on the one control people look at to find
out what to do next.

Not one of these is subtle. All three were invisible from the code, and all
three were obvious in a screenshot. The lesson is not "the CSS was wrong" — it
is that a measurement can only answer the question you thought to ask, and
looking answers questions you did not.

## Gaps found by being asked "is all that thought through?"

Three, and the honest scoring is one out of three.

**Checking the connection still works on launch** — already there. `resume_session`
calls `getProfile` before claiming to be connected, because a silent failure
later is worse than an honest reconnect prompt now.

**A countdown on the seven-day expiry** — not there, and a real gap. Google
expires Testing-mode refresh tokens after seven days. That is unavoidable
without publishing the app for verification, which is the thing this whole
design exists to avoid. But unavoidable is not the same as surprising. The
connection date is recorded now, the last two days show a warning in the top
bar where people already look, and Settings always states it. Rounded down, so
"1 day left" never quietly means twenty minutes, and floored at zero, because a
countdown reading "-4 days" is nonsense.

**Two copies at once** — nothing stopped it. Two windows on one database, both
scanning, doubling an API quota that already caps a large mailbox at forty
minutes. Clicking a launcher twice is not a request for a second app. The
existing window comes forward instead.

## Setup should not read like a warning

"One catch, up front… about five minutes of clicking through Google's website."
Every word true, and the framing is an apology. Someone reading it decides the
app is hard before seeing it.

The bigger fix was making the claim untrue. Google's clients page has a
**Download JSON** button holding both credentials, and Hush was asking people
to open that file, find two fields among eight, and hand-copy each into the
right box — three chances to get it wrong, one of which the app's own error
message already anticipated ("it's easy to paste one into the other's box").
Either box now accepts the whole file and fills in both.

## Google picks the wrong account for you

The best gap report of the project, and it came from someone doing the setup:
Google's console silently signs you into whichever account it saw first. Anyone
with more than one — most people — can complete the entire setup against the
wrong account, and the failure surfaces much later as "Google turned the
request down" with nothing pointing at why.

The warning is attached to the button that opens the console rather than
written into each step, so a step added later cannot forget it. It repeats on
all five pages, deliberately: repetition is the right call when a mistake is
invisible, easy and expensive.

## Pictures of somebody else's website

The setup wizard can show a screenshot of each Google page beside the
instruction. They are loaded by filename from `src/assets/setup/`, so adding
one is dropping in a file, and a missing file shows no picture rather than
breaking the build.

That last part is the design, not a convenience. These are screenshots of
Google's console, which is redesigned without notice, so the words have to
carry the step on their own and the picture can only ever be a help. A wizard
that depends on an image matching a page Google controls is a wizard that
breaks on someone else's schedule.

One rule in the folder's README, which matters more than the rest: the last
screenshot must not include the Client ID or secret. Google shows both on that
dialog. A real-looking credential in a public repository is alarming whether or
not it still works — people report it, and anyone copying the repo inherits the
confusion.

## Finishing the job that "no homework" started

The rule was stated in 0.5.0 and then not carried all the way. Three paths
survived that ended with the user doing something:

- `mailto:` senders, without the send permission, opened a pre-written message
  in the user's own mail app for them to send by hand.
- Link-only senders were listed on the results screen with an **Open link**
  button each.
- Successful ones offered **Check it yourself**.

Each seemed reasonable in isolation and each is a chore. Removed: the whole
`MailtoMode` enum, the hand-off branch, the Settings section that existed only
to choose between them, the three link buttons, and the confirm screen's
"you'll open these yourself" group. Anything that cannot be done automatically
is now reported as un-automatable and blocked, which needs nothing from anyone.

Sending stays optional and always was. Declining it does not produce work — it
produces a filter.

The lesson is about how a rule decays. Nobody decided to keep homework in the
app; the rule was applied to the loudest case and the quiet ones were never
revisited. "We fixed that" is a claim with a shelf life, and the only way to
check it is to go back and look for the same shape somewhere else.

## Deleting the permission, not just the chore

0.10.0 removed the `mailto:` hand-off — the bit where the user's own mail app
opened and they pressed send. What it kept was `mailto:` sent automatically
through `gmail.send`, on the reasoning that automatic is not homework, which is
true and was the wrong question.

The right question is what it costs. Measured on a real account: **160 of 2,627
messages are `mailto:`-only, about 6%** — and those senders get blocked instead,
which is the stronger outcome anyway, since a filter does not depend on the
sender cooperating.

Against that: `gmail.send` lets an app send email as you. It is the largest
thing Hush could ask Google for, larger than reading, trashing and filtering put
together in how it reads on a consent screen. And the code path behind it had
never once run against a real mailbox — the only untested path in the app, kept
alive for 6% of a weaker action.

So the feature went and the permission went with it. Gone with them:
`send_raw`, the RFC 5322 builder, the header-injection guards that existed only
to make that builder safe, `can_send` through four files, and a tick-box on the
consent screen. The app now asks for three scopes instead of four and can state
plainly that it cannot send mail as you.

**A near miss worth recording.** Removing the tests for the deleted builder, I
matched on a regex instead of on names, and it ate 22 test functions — including
`requests_to_the_local_network_are_refused` and `plain_http_is_refused`, the SSRF
guards, which have nothing to do with sending email. Caught only because the
test count dropped from 169 to 146 and three helper functions went unused.

The suite is not a thing to tidy with a pattern. Restored from git and redone by
name, and the count now reads 163 — six genuinely dead tests, one renamed.

## The screen nobody looked at

Twelve releases in a day, and the first screen — the one that decides whether
anyone reaches the second — was never touched. Prompted by the only feedback
that could have caught it: "the start page looks entirely the same lmfao."

It was. Every change that day went into the list, blocked senders, the
troubleshooter, confirm. Welcome got one heading reworded and one phrase
shortened, which is indistinguishable from nothing.

Screenshotting it showed why that mattered. Four dense paragraphs under the
heading **"What it will never do"** — opening on a list of negatives before the
app had said what it was for, every claim true and none of it readable. The
exact "like a word document" complaint, sitting on the highest-traffic screen
in the app.

The promises are the reason to trust it, so they stay. Their shape changed: one
line each in a two-by-two grid instead of four paragraphs. Content fits without
scrolling, and it reads as designed rather than written.

Two lessons, and the second is the real one. Attention follows whoever is
complaining, and nobody had complained about the welcome screen because nobody
gets far enough to complain about it — the people it fails are gone. And: it
took a screenshot again. That is three separate times now that looking found
something reading could not.

## Twice wrong about a file that was there

Asked to add the setup screenshots, and reported twice — confidently — that it
was impossible because they had been pasted from the clipboard and never
written to disk.

They were in `~/Pictures/Screenshots`, nine of them, timestamped to the minute
they were sent.

Both searches were broken. The first used `-newermt '-40 minutes'` when they
were already older than that. The second used `-newermt 'today 00:00'`, run the
following day, which excludes everything from the day they were taken. Each
returned nothing, and instead of doubting the search I built an explanation for
the absence and stated it as fact.

The tell was available and ignored: "no files exist" is a much stranger claim
than "my search is wrong", and the second search's own output — zero images
anywhere on a desktop machine in active use — should have been read as an
implausible result rather than a finding.

## Nearly committing a client secret

The last screenshot shows the OAuth dialog, which displays the Client ID and
the Client secret alongside the Download JSON link. It had to be cropped to the
link.

The first crop, by eye from coordinates, landed square on the Client secret.
Caught only by rendering the cropped file and looking at it. The second attempt
was right, and was also checked.

CI greps the tree for anything matching a Google credential, which would have
caught this in a text file and cannot see inside a PNG. Where the guard cannot
reach, look at the artefact.

## The first issue anyone opened

Someone asked for a portable version. The AppImage was already portable in the
sense of needing no installation — and still wrote a database, a log and a
keychain entry into the user's home folder, so running it on a borrowed machine
left a list of who mails them behind on it. For an app whose whole argument is
that the data stays yours, that is the wrong half of the promise to keep.

Portable mode puts everything in a `hush-data` folder beside the executable.
Opt-in and explicit — a `hush-portable.txt` marker or `HUSH_PORTABLE=1` —
because guessing is worse than either answer: silently writing to a read-only
mount fails, and silently *not* writing to the home folder loses someone's scan.

Two details worth keeping:

**`current_exe` is wrong for an AppImage.** It unpacks into `/tmp` and runs from
there, so "beside the executable" resolves to a temporary directory that
vanishes. The `APPIMAGE` variable holds the path the user actually
double-clicked, which is the one that means anything.

**It deliberately does not save the connection.** A keychain entry belongs to
the machine, which is exactly the trace being avoided, and a refresh token in a
plain file on a memory stick is worse than signing in again. Portable mode
reports the keychain as unavailable, which reuses the path already built for
machines without a secret store — token in memory, connect screen says so.

Verified by running it from a fake USB directory and fingerprinting the home
folder before and after. Byte-identical.

## A condition on a name that had already drifted

The step that adds the standalone Windows binary tested for `windows-latest`.
The matrix says `windows-2022`. So it never ran, the release shipped without
the file, and nothing failed — a skipped conditional is not an error.

Caught only by listing the release assets afterwards instead of trusting that
green means done. It keys on the file existing now, which cannot drift.

The general shape: **a condition that silently does nothing is worse than one
that breaks.** Anything guarded by a name someone else controls needs a check
that the guarded thing actually happened.

## The Windows .exe was never portable

Worth noticing while answering: the `.exe` in every release is an NSIS
*installer*, the opposite of what was asked for. Tauri had already built the
standalone binary it wraps, and nobody was shipping it. Releases now carry
`Hush-portable-*.exe` as well.

## Reassurance on the wrong screen, a clock nobody saw

Two things reported as missing that had both been built, in places where they
did no work.

**"About two minutes" was only on the welcome screen** — one grey line beside
Get started. Click through and you are in six pages of Google's cloud console
with no reminder that it is short. The reassurance was sitting where nobody is
nervous, and absent from the part that scares people. It is in the step header
now, on every page of the wizard.

**The seven-day countdown only appeared at two days or fewer.** Which sounds
restrained and means that for five days out of seven there is no sign the clock
exists — so the honest report is "there's no timer anywhere". It is always on
screen once connected now: quiet grey while there is time, amber for the last
two days.

Both are the same mistake. A feature placed where it is *technically* correct
rather than where the person is, is indistinguishable from a feature that was
never built — and the bug report you get is "you didn't do it", which is fair.

## Counting in the app's unit instead of the user's

Asked whether the design was "psychologically nice", which is a better question
than any I had asked myself. Walking the flow and looking at it answered it:
calm, and unrewarding.

**The action bar counted senders.** "2 senders selected." Nobody wants fewer
senders — they want fewer emails, and the list already knew that number for
every row. It reads "2 senders · 1,000 emails" now, so ticking a box adds up to
something instead of incrementing a tally in a unit the user never chose.

**The results screen opened on three caveats.** Headline, then "old emails were
left alone", then "which is as far as anything can be confirmed", then "nothing
in email reports back". Every one true and every one still there — but a screen
that leads with qualifications reads as an apology for the thing you just did.
The size of the win goes first now: *that's 1,000 emails they've sent you, and
the next one won't arrive.* Then the caveats.

The count comes from the store, for the addresses that actually succeeded, so
it can never claim more than happened. Honesty was never the problem — order
was.

## Shortening sentences was the wrong lever

"it just feels bloated… it's too much text." Said three times, in different
words, after two rounds of tightening copy. So: measure the screen instead of
editing it.

The confirm screen carried **649 words, seven choice buttons and four
checkboxes at once.** No sentence-level edit fixes eleven controls. The problem
was never the prose, it was that every decision the app can make was on screen
simultaneously, competing, for a user who almost always wants the default.

The defaults were already the safe ones. So they are now a single line —
*"Unsubscribe and block them / Future mail skips your inbox · Old emails left
alone"* — with one **Change** button beside it. **190 words visible, two
buttons.** Nothing was deleted; everything is one click away, and every safety
gate is untouched: the flagged-sender warning still shows unconditionally, and
trashing still needs its explicit tick inside the expanded panel.

The welcome screen got the same treatment for the same reason. Four promises
with a sentence each became four titles with a tick, because the titles *are*
the promise and on a phone the two-column grid collapsed into the wall of text
it had been built to replace.

The lesson is about how the complaint was phrased. "Too much text" sounds like a
copy problem and was a structure problem — and I spent two releases on the
reading before measuring the thing.

## A twenty-pixel target

"clicking the check to select it is weird, it sometimes doesn't click, feels
like u need an angle."

The hit area was **20x20 pixels** — under half the 44x44 a finger needs, and
small for a mouse. Clicks landed a few pixels outside and did nothing, which
does not read as a miss. It reads as the app ignoring you.

Fixed with a negative inset on the invisible input, so the target is 44x44
while nothing on screen moves. Verified by clicking four pixels from the corner
and asserting the box ticked — a click that would have missed entirely before.

Worth noting how it was reported: not "the target is too small" but "it feels
like you need an angle". Someone describing a physical sensation of aiming is
describing a hit-area problem, and it took measuring the element to see it.

## Protecting a sender looked like it did nothing

"if u click never touch this one it should disappear into another section no?
why does it stay."

Right, and it was inconsistent with itself. A finished sender drops out of the
list; a protected one went grey and stayed exactly where it was. Both mean "I
have dealt with this, stop showing it to me", and only one behaved that way.

Protected senders now move to their own tab, where they can be found and
unprotected.

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

## Android: builds, runs, cannot sign in

Real progress and a real wall, recorded so the next attempt does not start over.

**Working, verified on a Pixel 7a:** compiles for arm64, installs, launches,
renders every screen, dark mode, the setup screenshots, and opens links. Two
bugs were fixed getting there — `tauri-plugin-single-instance` has no `init`
off desktop, and the free `tauri_plugin_opener::open_url` always takes the
desktop path and shells out to `xdg-open`, so every link died with `os error 2`
until it went through the app handle's `cfg(mobile)`-aware method.

**Not working:** connecting to Google. The browser opens and sign-in happens;
the redirect back never completes.

The cause is not yet known, and this is written down *without* a diagnosis on
purpose. Three candidates, all plausible: Android freezing the process while
the browser is in front, so the loopback listener never accepts; the browser
declining a plain-HTTP localhost URL; or Google refusing a Desktop-type client
used from a phone.

What is clear is the shape of the proper fix, whichever it is. Loopback is the
*desktop* OAuth pattern. Android's equivalent is a custom-scheme redirect
handled by an intent filter — and Google only permits custom schemes on an
**Android** client type, which is registered against the package name and the
signing certificate's SHA-1, and carries no client secret.

So Android needs its own setup flow, not a patched desktop one: a different
client type, a different thing to paste, an intent filter, and a deep-link
handler. That is a feature, not a fix, and guessing at it while the actual
error is unread is how the last three mistakes happened.
