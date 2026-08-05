<div align="center">

# Hush

**Quietly unsubscribe from bulk email.**

A desktop app that finds everyone who mails you in bulk and unsubscribes from
the ones you pick — without ever risking your receipts, order confirmations, or
password resets.

No server. No telemetry. Your own Google credentials. Nothing is ever
permanently deleted.

</div>

---

## What it does

1. Connects to your Gmail using **credentials you create yourself** — Hush ships
   with none.
2. Reads the **sender, subject and date** of your messages. Never the contents.
3. Groups them by sender, showing how much each one sends and how often.
4. Unsubscribes from the ones you tick.
5. Optionally clears out their old newsletters, if you ask it to.

It's a one-shot tool. Run it, tick the senders you're done with, close it. There
is no background service, nothing is filtered or blocked, and you can uninstall
it straight afterwards — the unsubscribes stay done.

By default the permission it asks Google for is **read-only**, so it *cannot*
delete, archive, label or move anything. The tidy-up below is opt-in, and even
then only ever moves mail to Trash.

## The safety mechanism

This is the part worth understanding before you trust it with your inbox.

Bulk and marketing mail carries a `List-Unsubscribe` header (RFC 2369).
Transactional mail — receipts, shipping notices, password resets, two-factor
codes, invoices — generally does not, because there is nothing to unsubscribe
from.

**So: a sender is only ever offered to you if their mail carries that header.**
Senders without it are not shown, not selectable, and not reachable through any
path in the app. That single rule prevents the worst thing this kind of tool can
do.

That gate lives in one function — [`Group::finish`][gate] in
`src-tauri/src/store.rs` — and everything downstream reads from it, so no part of
the interface can route around it.

On top of that:

- **A never-touch list.** Add anyone to it and they can't be selected, in this
  session or any future one.
- **Warnings on likely-transactional senders.** Banks, payment processors,
  airlines, delivery services, government, healthcare — plus subject-line
  signals like *receipt*, *order*, *invoice*, *verification code*, *reset your
  password*. These are **not blocked**, because plenty of shops send both
  marketing and receipts from the same address. They're flagged in a warning
  colour, with a plain-language reason, and need a second, separate confirmation.
- **Nothing is ever pre-selected.** Every checkbox starts empty. The bulk
  selection helpers deliberately skip flagged senders.
- **A confirmation screen** that says exactly who, how many, and what will be
  sent — with a panel showing the literal request.
- **Dry run, on by default.** The first launch sends nothing at all until you
  turn it off.

## How unsubscribing actually works

There are three kinds of unsubscribe in the wild, and Hush treats them
differently on purpose.

| | What Hush does | Why |
|---|---|---|
| **One-click** (RFC 8058) | Sends a `POST` with body `List-Unsubscribe=One-Click`. Fully automatic. | The sender has explicitly promised that this exact request means "unsubscribe" and nothing else. |
| **`mailto:`** | Either opens a ready-written message in your own mail app *(default)*, or sends it through Gmail *(only if you grant the send permission)*. | Unambiguous, but sending mail as you is a big permission — so it's opt-in. |
| **A plain link** | **Nothing.** It's listed under "you'll open these yourself", and you tick them off as you go. | A bare link might be a one-tap unsubscribe, a preference centre, a login wall, or a confirmation page. We can't tell, so a human decides. |

Hush will not follow redirects on a one-click endpoint (RFC 8058 forbids them),
sends no cookies, and refuses any unsubscribe URL that resolves to your own
network rather than the public internet.

## Clearing out the backlog

Unsubscribing stops the next newsletter. It does nothing about the 600 already
sitting in your inbox, so Hush can bin those too — but only if you ask, twice:
once when connecting (it needs a wider Google permission, and it's a tick-box
that starts off), and again on the confirmation screen before each run.

Two things make it safe rather than alarming:

**It only bins mail that carried an unsubscribe header.** This is the same gate
that decides who's unsubscribable, reused. A shop that sends you marketing *and*
order confirmations from one address has the header on the marketing and not on
the receipts — so the marketing goes and the receipts stay. The confirmation
screen tells you the split before you commit: *"Also bin their old emails (658)
… 164 other emails from these senders — receipts, confirmations and the like —
will be left alone."*

**It moves mail to Trash, never deletes it.** Gmail keeps trashed mail for 30
days, so a mistake is yours to undo without needing us. Hush has no code path
that permanently deletes anything, and never requests a permission that would
let it — `gmail.modify` grants trashing but *not* permanent deletion, which is
exactly why it's the one used.

If you skip the permission, everything else works exactly the same. The tick-box
is simply absent.

## What touches what

**Leaves your computer:** requests to `googleapis.com` and `accounts.google.com`,
and — only for senders you tick, only when dry run is off — one request to that
sender's own unsubscribe endpoint.

**Never leaves your computer:** everything else. There is no Hush server. There
is no analytics, no crash reporting, no update ping, no usage counter. You can
verify this: the app's web layer runs under a Content Security Policy that
forbids network requests entirely (`src-tauri/tauri.conf.json`), and the only
HTTP client in the Rust code is used for Google and for unsubscribe endpoints.

**Stored on your computer**, in a single SQLite file:

- Per message: sender address, sender name, subject, date, and the unsubscribe
  headers.
- Your never-touch list, your settings, and what happened when you unsubscribed.

Subjects are stored because the safety heuristics read them — a sender whose
recent subjects are all "Your order has shipped" needs a warning, and that
judgement can't be made without the words.

**Stored in your operating system's keychain:** the Google refresh token, and
nothing else. If your machine has no working secret store, Hush keeps it in
memory for the session and tells you so, rather than quietly writing it to a
file.

Settings → *Erase everything* revokes access with Google, clears the keychain,
and deletes the database. Your mail is untouched by it — that button only clears
what's on this computer.

### The permissions Hush asks for

| Permission | When | What it allows |
|---|---|---|
| `gmail.readonly` | Always | Reading message metadata. Cannot change anything. |
| `gmail.modify` | Only if you tick "bin the old emails" | Moving mail to Trash. **Not** permanent deletion. |
| `gmail.send` | Only if you tick "send mail as me" | Sending the handful of unsubscribes that only work by email. |

Hush trusts what Google actually granted rather than what it asked for, so
declining an extra on the consent screen leaves that feature switched off rather
than failing later.

## Getting your Google credentials

The app walks you through this one screen at a time, with buttons that open the
exact pages. The short version:

1. Create a project at [console.cloud.google.com/projectcreate](https://console.cloud.google.com/projectcreate).
2. Enable the [Gmail API](https://console.cloud.google.com/apis/library/gmail.googleapis.com).
3. Fill in [app details](https://console.cloud.google.com/auth/branding) — pick
   **External** as the audience.
4. On the [audience page](https://console.cloud.google.com/auth/audience), leave
   publishing status as **Testing** and add your own address as a test user.
5. Create an OAuth client of type **Desktop app** on the
   [clients page](https://console.cloud.google.com/auth/clients).
6. Paste the Client ID and secret into Hush.

### Why "Testing" mode

Keeping the project in Testing means Google never reviews it — because nobody
except your listed test users can use it. No verification, no security
assessment, no waiting.

The cost: **Google expires the connection every seven days.** Hush will notice
and ask you to reconnect, which is one click. This is Google's rule and there is
nothing the app can do about it short of asking you to publish the project and
submit to review, which defeats the point.

### About that "client secret"

Google issues desktop OAuth clients a secret, and then
[documents](https://developers.google.com/identity/protocols/oauth2/native-app)
that it is not treated as confidential — it ships inside every copy of any app
that uses it. Hush stores yours in the local database rather than the keychain
for that reason, and relies on PKCE, which is what actually binds an
authorisation response to the session that asked for it.

## Install

Download from [Releases](https://github.com/justlinuxnoob/hush/releases). Nothing else
is required — no Node, no Rust, no Python.

| Platform | File |
|---|---|
| Windows | `.msi` |
| macOS | `.dmg` — universal, runs on both Apple silicon and Intel |
| Linux | `.deb` (Debian, Ubuntu), `.rpm` (Fedora, RHEL, openSUSE), or `.AppImage` (anything) |

### macOS: the app is unsigned

Unless the maintainer has an Apple Developer account, macOS builds are not
signed or notarised, and macOS will refuse to open them by double-click.

To open it anyway:

1. Drag Hush to Applications.
2. **Right-click** (or Control-click) the app → **Open** → **Open** in the dialog.

If macOS says the app "is damaged and can't be opened", that's Gatekeeper's
message for an unsigned download rather than actual damage. Clear the quarantine
flag:

```sh
xattr -dr com.apple.quarantine /Applications/Hush.app
```

Only run that on a build whose checksum you've verified (see below).

## Verifying the build

Releases are built by [GitHub Actions](.github/workflows/release.yml) on
GitHub's own runners — not on anyone's laptop. The workflow file is in this
repository, and every run's log is public.

Each release publishes `SHA256SUMS`. To check what you downloaded:

```sh
# macOS / Linux
shasum -a 256 -c SHA256SUMS --ignore-missing

# Windows (PowerShell)
Get-FileHash .\Hush_0.1.0_x64_en-US.msi -Algorithm SHA256
```

### Rebuilding it yourself

```sh
git clone https://github.com/justlinuxnoob/hush
cd hush
git checkout v0.1.0          # the tag you downloaded
npm ci                       # exact versions from package-lock.json
npm run tauri build
```

Then compare your `SHA256` against the published one.

**Honest caveat:** the checksums will probably not match yet. Fully reproducible
builds need every input pinned — compiler, linker, system libraries, embedded
timestamps and paths — and this project is not there. What is pinned today: the
Rust toolchain (`rust-toolchain.toml`), the exact dependency versions
(`Cargo.lock`, `package-lock.json`), and `CARGO_INCREMENTAL=0` with
`codegen-units = 1`. See [NOTES.md](NOTES.md) for what remains.

What you *can* verify today, and what actually matters: that the published
binary came from this source, built by a public CI run you can read, from a
commit you can inspect.

### Building for development

Requires [Rust](https://rustup.rs) and [Node 20+](https://nodejs.org). On Debian
or Ubuntu you also need:

```sh
sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev \
                 librsvg2-dev build-essential curl wget file libssl-dev
```

Then:

```sh
npm install
npm run tauri dev                                  # run it
cargo test --manifest-path src-tauri/Cargo.toml    # run the tests

VITE_HUSH_DEMO=1 npm run dev   # the interface, with a fake backend and no Google
```

See [NOTES.md](NOTES.md) for what's unfinished and what was guessed at.

## How it stays inside Gmail's limits

Gmail bills in "quota units" rather than requests, and the published per-user
ceiling has changed more than once. Rather than hard-code a number that may
already be wrong, Hush uses an adaptive limiter: it starts conservatively,
speeds up while requests succeed, and halves its rate the moment Google pushes
back — additive increase, multiplicative decrease, the same control loop TCP
uses. Retries use exponential backoff with jitter and honour `Retry-After`.

Scans are cached in SQLite, resumable, cancellable at any point, and a second
scan asks Gmail only what changed since the last one using its history marker.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). The short version: the safety gate is
not negotiable, and changes near it need tests.

## Licence

[MIT](LICENSE).

[gate]: src-tauri/src/store.rs
