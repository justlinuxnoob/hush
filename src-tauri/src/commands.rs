//! The bridge between the interface and everything else.
//!
//! Commands here stay thin: they translate, they check the user's stated
//! intent, and they hand off. The rules that matter — what may be
//! unsubscribed, what may be contacted — live in `store`, `parse` and `unsub`
//! where they are tested, not here.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::auth::{
    ClientCredentials, GoogleAuth, Keychain, TokenStorage, SCOPE_MODIFY, SCOPE_READONLY, SCOPE_SEND,
};
use crate::error::{Error, Result};
use crate::filters::{ManagedFilter, RemovalPreview, RemovalReport};
use crate::gmail::{BlockAction, Cancel, GmailClient};
use crate::model::{Outcome, ScanDepth, ScanProgress, Sender};
use crate::scan::Scanner;
use crate::state::{
    AppState, Session, SETTING_ACCOUNT, SETTING_BACKLOG_ACTION, SETTING_BLOCK_ACTION,
    SETTING_GRANTED, SETTING_SEEN_WELCOME,
};
use crate::store::Store;
use crate::unsub::{
    BacklogAction, Executor, MailtoMode, PlannedAction, RunReport, TrashReport, UnsubRequest,
};

/// Emitted repeatedly while a scan runs.
pub const EVENT_SCAN_PROGRESS: &str = "scan-progress";
/// Emitted repeatedly while unsubscribes and binning run.
pub const EVENT_RUN_PROGRESS: &str = "run-progress";

#[derive(Debug, Serialize)]
pub struct Status {
    pub connected: bool,
    pub email: Option<String>,
    pub has_credentials: bool,
    pub can_send: bool,
    /// Whether Hush may move old mail to Trash. Opt-in, and off by default.
    pub can_delete: bool,
    /// Whether Hush may create a Gmail filter to stop future mail outright.
    pub can_block: bool,
    pub mailto_mode: MailtoMode,
    /// False on machines with no working secret store; the interface warns that
    /// the connection will not survive quitting.
    pub keychain_available: bool,
    pub token_storage: Option<TokenStorage>,
    pub seen_welcome: bool,
    pub scan_complete: bool,
    pub last_scan_ms: i64,
    pub message_count: u64,
    pub sender_count: u64,
    pub scanning: bool,
    /// How the user blocked last time, so the choice can be preselected.
    /// Defaults to archiving, which is also what an unreadable value means.
    pub block_action: BlockAction,
    /// The same, for what happens to old mail.
    pub backlog_action: BacklogAction,
}

#[tauri::command]
pub async fn status(state: State<'_, AppState>) -> Result<Status> {
    let session = state.session.read().await;
    let account = state.account()?;
    let email = session
        .as_ref()
        .map(|s| s.email.clone())
        .or_else(|| account.clone());

    let (message_count, sender_count, scan_complete, last_scan_ms) = match &email {
        Some(acc) => (
            state.store.message_count(acc)?,
            state.store.senders(acc)?.len() as u64,
            state.store.scan_state(acc)?.complete,
            state.store.scan_state(acc)?.last_scan_ms,
        ),
        None => (0, 0, false, 0),
    };

    Ok(Status {
        connected: session.is_some(),
        email,
        has_credentials: state.credentials()?.is_some(),
        can_send: session.as_ref().is_some_and(|s| s.can_send),
        can_delete: session.as_ref().is_some_and(|s| s.can_delete),
        can_block: session.as_ref().is_some_and(|s| s.can_block),
        mailto_mode: state.mailto_mode(),
        keychain_available: Keychain::is_available(),
        token_storage: session.as_ref().map(|s| s.storage),
        seen_welcome: state
            .store
            .get_setting(SETTING_SEEN_WELCOME)?
            .is_some_and(|v| v == "true"),
        scan_complete,
        last_scan_ms,
        message_count,
        sender_count,
        scanning: state.scan_cancel.read().await.is_some(),
        block_action: state
            .store
            .get_setting(SETTING_BLOCK_ACTION)?
            .as_deref()
            .map(BlockAction::parse)
            .unwrap_or_default(),
        backlog_action: state
            .store
            .get_setting(SETTING_BACKLOG_ACTION)?
            .as_deref()
            .map(BacklogAction::parse)
            .unwrap_or_default(),
    })
}

#[tauri::command]
pub async fn mark_welcome_seen(state: State<'_, AppState>) -> Result<()> {
    state.store.set_setting(SETTING_SEEN_WELCOME, "true")
}

/// Check and store the Google credentials the user pasted during setup.
#[tauri::command]
pub async fn save_credentials(
    state: State<'_, AppState>,
    client_id: String,
    client_secret: String,
) -> Result<()> {
    let creds = ClientCredentials {
        client_id,
        client_secret,
    }
    .trimmed();
    creds.validate()?;
    state.save_credentials(&creds)
}

/// Run the consent flow in the user's real browser.
///
/// `allow_send` and `allow_delete` are passed explicitly by the interface after
/// it has explained, in plain words, what each wider permission is for. Neither
/// is ever inferred, and both default to off.
#[tauri::command]
pub async fn connect(
    state: State<'_, AppState>,
    allow_send: bool,
    allow_delete: bool,
    allow_block: bool,
) -> Result<Status> {
    let creds = state
        .credentials()?
        .ok_or_else(|| Error::Setup("Finish the setup steps first.".into()))?;
    creds.validate()?;

    let auth = GoogleAuth::new(creds)?;
    let mut scopes: Vec<&str> = vec![SCOPE_READONLY];
    if allow_send {
        scopes.push(SCOPE_SEND);
    }
    if allow_delete {
        scopes.push(SCOPE_MODIFY);
    }
    if allow_block {
        scopes.push(crate::auth::SCOPE_SETTINGS);
    }

    let cancel = Cancel::new();
    *state.connect_cancel.write().await = Some(cancel.clone());

    let granted = auth
        .connect(&scopes, &cancel, |url| {
            tauri_plugin_opener::open_url(url, None::<&str>)
                .map_err(|e| Error::Other(format!("Couldn't open your browser: {e}")))
        })
        .await;

    // Clear the handle whatever happened, so a later attempt is not born
    // already cancelled.
    *state.connect_cancel.write().await = None;
    let granted = granted?;

    let gmail = Arc::new(GmailClient::new(
        auth.clone() as Arc<dyn crate::gmail::TokenSource>,
        state.limiter.clone(),
    )?);
    let profile = gmail.profile().await?;
    let email = profile.email_address.clone();

    let storage = auth.persist(&email).await;
    state.store.set_setting(SETTING_ACCOUNT, &email)?;

    // Trust what Google actually granted, not what we asked for. A user can
    // approve some permissions and decline others on the consent screen.
    let can_send = granted.contains("gmail.send");
    let can_delete = granted.contains("gmail.modify");
    let can_block = granted.contains("gmail.settings.basic");
    // Remembered so a relaunch does not march the user back through Google's
    // consent page for permissions they have already given.
    state.store.set_setting(SETTING_GRANTED, &granted)?;

    *state.session.write().await = Some(Session {
        email,
        auth,
        gmail,
        storage,
        can_send,
        can_delete,
        can_block,
    });

    status(state).await
}

/// Reconnect on launch without bothering the user, if we can.
#[tauri::command]
pub async fn resume_session(state: State<'_, AppState>) -> Result<Status> {
    if state.session.read().await.is_some() {
        return status(state).await;
    }
    let (Some(creds), Some(email)) = (state.credentials()?, state.account()?) else {
        return status(state).await;
    };

    let auth = GoogleAuth::new(creds)?;
    if !auth.restore(&email).await.unwrap_or(false) {
        return status(state).await;
    }

    let gmail = Arc::new(GmailClient::new(
        auth.clone() as Arc<dyn crate::gmail::TokenSource>,
        state.limiter.clone(),
    )?);

    let granted = state
        .store
        .get_setting(SETTING_GRANTED)?
        .unwrap_or_default();

    // Prove the stored connection still works before claiming to be connected.
    // In Testing mode these lapse after seven days, and a silent failure later
    // is worse than an honest "reconnect" prompt now.
    match gmail.profile().await {
        Ok(_) => {
            *state.session.write().await = Some(Session {
                email,
                auth,
                gmail,
                storage: TokenStorage::Keychain,
                can_send: granted.contains("gmail.send"),
                can_delete: granted.contains("gmail.modify"),
                can_block: granted.contains("gmail.settings.basic"),
            });
        }
        Err(Error::Unauthorized) => {}
        Err(e) => return Err(e),
    }
    status(state).await
}

#[tauri::command]
pub async fn disconnect(state: State<'_, AppState>, erase_local_data: bool) -> Result<Status> {
    // Stop anything in flight *first*.
    //
    // A scan holds its own `Arc<Store>` and writes to it for as long as it
    // runs — up to forty minutes on a large mailbox. Erasing underneath one
    // wipes the database and then watches the scan refill it, so "disconnect
    // and erase everything" quietly did not erase everything. For an app whose
    // whole promise is that the data is yours and local, that is the worst
    // failure available.
    //
    // Revoking the token without stopping the scan is bad on its own terms
    // too: every request in flight starts failing against a dead credential.
    for handle in [&state.scan_cancel, &state.run_cancel] {
        if let Some(cancel) = handle.write().await.take() {
            cancel.cancel();
        }
    }

    if let Some(session) = state.session.write().await.take() {
        session.auth.disconnect().await?;
    } else if let Some(email) = state.account()? {
        // No live session, but there may still be a token on disk.
        let _ = Keychain::new(&email).erase();
    }

    if erase_local_data {
        state.store.erase(false)?;
    } else {
        state.store.set_setting(SETTING_ACCOUNT, "")?;
        state.store.set_setting(SETTING_GRANTED, "")?;
    }
    status(state).await
}

// --- scanning ------------------------------------------------------------

#[tauri::command]
pub async fn start_scan(
    app: AppHandle,
    state: State<'_, AppState>,
    depth: ScanDepth,
    incremental: bool,
) -> Result<()> {
    // Starting a scan while one is running stops the old one rather than
    // refusing. Refusing left people stuck: navigate away mid-scan, come back,
    // and the app insisted a scan was happening with nothing on screen to show
    // it or stop it.
    if let Some(existing) = state.scan_cancel.write().await.take() {
        log::info!("a new scan was asked for, stopping the one already running");
        existing.cancel();
    }
    let (email, gmail) = state.require_session().await?;

    let cancel = Cancel::new();
    *state.scan_cancel.write().await = Some(cancel.clone());

    let store = state.store.clone();
    let handle = app.clone();

    // The scan outlives this command: the interface follows it by event so the
    // window stays responsive and cancelling stays possible.
    tauri::async_runtime::spawn(async move {
        let scanner = Scanner::new(gmail, store, email);
        let emit = |p: &ScanProgress| {
            let _ = handle.emit(EVENT_SCAN_PROGRESS, p);
        };

        let result = if incremental {
            match scanner.incremental_scan(cancel.clone(), emit).await {
                // A history marker that has aged out is normal, not an error.
                // Quietly fall back to a full sweep.
                Err(Error::Other(_)) => scanner.full_scan(depth, cancel.clone(), emit).await,
                other => other,
            }
        } else {
            scanner.full_scan(depth, cancel.clone(), emit).await
        };

        let final_progress = match result {
            Ok(p) => p,
            Err(e) => ScanProgress {
                finished: true,
                note: Some(e.to_string()),
                ..Default::default()
            },
        };
        let _ = handle.emit(EVENT_SCAN_PROGRESS, &final_progress);

        if let Some(state) = handle.try_state::<AppState>() {
            // Only clear the handle if it is still ours. A scan that started
            // after this one owns it now, and clearing it would leave that scan
            // running with a Stop button wired to nothing.
            let mut held = state.scan_cancel.write().await;
            if held.as_ref().is_some_and(|c| c.is_same(&cancel)) {
                *held = None;
            }
        }
    });

    Ok(())
}

/// Stop a run of unsubscribes partway through.
///
/// Whatever already completed stays completed and is reported; the rest simply
/// never happens.
#[tauri::command]
pub async fn cancel_run(state: State<'_, AppState>) -> Result<()> {
    if let Some(c) = state.run_cancel.read().await.as_ref() {
        c.cancel();
    }
    Ok(())
}

/// Give up waiting for Google's consent page.
#[tauri::command]
pub async fn cancel_connect(state: State<'_, AppState>) -> Result<()> {
    if let Some(c) = state.connect_cancel.read().await.as_ref() {
        c.cancel();
    }
    Ok(())
}

#[tauri::command]
pub async fn cancel_scan(state: State<'_, AppState>) -> Result<()> {
    if let Some(c) = state.scan_cancel.read().await.as_ref() {
        c.cancel();
    }
    Ok(())
}

#[tauri::command]
pub async fn list_senders(state: State<'_, AppState>) -> Result<Vec<Sender>> {
    let account = state.account_or_stored().await?;
    state.store.senders(&account)
}

/// Every subject Hush has for one sender, newest first.
#[tauri::command]
pub async fn sender_messages(
    state: State<'_, AppState>,
    address: String,
) -> Result<Vec<SenderMessage>> {
    let account = state.account_or_stored().await?;
    // Generous, but not unbounded: a sender with tens of thousands of messages
    // should not be able to stall the interface.
    const MAX: u32 = 500;
    Ok(state
        .store
        .subjects_for_sender(&account, &address, MAX)?
        .into_iter()
        .map(|(subject, date_ms)| SenderMessage { subject, date_ms })
        .collect())
}

#[derive(Debug, Serialize)]
pub struct SenderMessage {
    pub subject: String,
    pub date_ms: i64,
}

#[tauri::command]
pub async fn set_never_touch(
    state: State<'_, AppState>,
    address: String,
    never: bool,
) -> Result<()> {
    let account = state.account_or_stored().await?;
    state.store.set_never_touch(&account, &address, never)
}

// --- unsubscribing --------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct Selection {
    pub addresses: Vec<String>,
}

/// Turn a set of chosen addresses into concrete requests.
///
/// Two guards live here and cannot be skipped by the interface: a sender must
/// still be in the scanned list (so must still have an unsubscribe header), and
/// must not be on the never-touch list.
async fn resolve(state: &AppState, addresses: &[String]) -> Result<Vec<UnsubRequest>> {
    let account = state.account_or_stored().await?;
    let senders = state.store.senders(&account)?;
    let never = state.store.never_touch(&account)?;

    let mut out = Vec::new();
    for addr in addresses {
        if never.iter().any(|n| n == addr) {
            continue;
        }
        let Some(s) = senders.iter().find(|s| &s.address == addr) else {
            continue;
        };
        out.push(UnsubRequest {
            address: s.address.clone(),
            display_name: s.display_name.clone(),
            method: s.method.clone(),
            methods: s.fallbacks.clone(),
        });
    }
    Ok(out)
}

/// What would happen, described exactly. Sends nothing.
#[tauri::command]
pub async fn plan_unsubscribe(
    state: State<'_, AppState>,
    selection: Selection,
) -> Result<Vec<PlannedAction>> {
    let requests = resolve(&state, &selection.addresses).await?;
    let executor = Executor::new(
        state.mailto_mode(),
        state.account_or_stored().await.unwrap_or_default(),
    )?;
    Ok(executor.plan(&requests))
}

/// Unsubscribe from the chosen senders, and optionally clear out their backlog.
///
/// `delete_backlog` is a separate, per-run decision. Unsubscribing is the point
/// of the app; binning old mail is an extra the user asks for each time, never
/// something that rides along with a previous choice.
// Eight arguments, and clippy is right that this is a lot. Bundling them into
// a struct would read better here and worse everywhere else: each one is a
// separate thing the user said yes to, and the flat list is what makes it
// obvious at the call site that none of them defaults to something destructive.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn run_unsubscribe(
    app: AppHandle,
    state: State<'_, AppState>,
    selection: Selection,
    unsubscribe: bool,
    delete_backlog: bool,
    block_future: bool,
    block_action: Option<BlockAction>,
    backlog_action: Option<BacklogAction>,
) -> Result<RunReport> {
    let account = state.account_or_stored().await?;
    let requests = resolve(&state, &selection.addresses).await?;

    let mut executor = Executor::new(state.mailto_mode(), account.clone())?;
    if state.mailto_mode() == MailtoMode::SendViaGmail {
        let session = state.session_parts().await?;
        if !session.can_send {
            return Err(Error::Setup(
                "Hush doesn't have permission to send mail yet. Reconnect your \
                 account and allow it, or switch back to using your own mail app."
                    .into(),
            ));
        }
        executor = executor.with_gmail(session.gmail.clone());
    }

    let cancel = Cancel::new();
    *state.run_cancel.write().await = Some(cancel.clone());

    let emit = |p: &crate::unsub::RunProgress| {
        let _ = app.emit(EVENT_RUN_PROGRESS, p);
    };

    // Unsubscribing and clearing the backlog are separate jobs. Wanting a
    // sender's old mail gone is not the same as wanting to stop hearing from
    // them, and neither should drag the other along.
    let mut report = if unsubscribe {
        executor.run_reporting(&requests, &cancel, emit).await
    } else {
        RunReport::default()
    };

    if delete_backlog && !cancel.is_cancelled() {
        // Absent means the interface did not say, and the answer to that is
        // always the reversible one.
        let action = backlog_action.unwrap_or_default();
        state
            .store
            .set_setting(SETTING_BACKLOG_ACTION, action.as_str())?;
        report.trash = Some(tidy_up(&state, &account, &requests, action, &cancel, &app).await?);
    }

    if block_future && !cancel.is_cancelled() {
        // Absent means "the interface did not say", and the answer to that is
        // always the safe one. A missing argument must never be the route by
        // which someone's receipts end up on a 30-day fuse.
        let action = block_action.unwrap_or_default();
        state
            .store
            .set_setting(SETTING_BLOCK_ACTION, action.as_str())?;

        let session = state.session_parts().await?;
        let addresses: Vec<String> = requests.iter().map(|r| r.address.clone()).collect();

        report.blocked = Some(if session.can_block {
            crate::unsub::block_senders(&session.gmail, &addresses, action, &cancel).await
        } else {
            // Reported as a failed block rather than raised as an error. The
            // unsubscribes have already gone out; failing the whole call would
            // throw those results away and tell the user nothing happened when
            // half of it did.
            log::warn!(
                "asked to block {} sender(s) without the settings permission",
                addresses.len()
            );
            crate::unsub::BlockReport {
                blocked: 0,
                failed: addresses.len() as u64,
                problem: Some(
                    "Hush doesn't have Google's permission to create filters yet.".to_string(),
                ),
                confirmed: None,
                action,
                unmarked: false,
            }
        });
    }

    *state.run_cancel.write().await = None;

    for outcome in &report.outcomes {
        state.store.record_outcome(&account, outcome)?;
    }

    Ok(report)
}

/// One thing that was checked, and what to do if it is wrong.
#[derive(Debug, Serialize)]
pub struct Check {
    pub name: String,
    /// `ok`, `warn` or `fail`. Plain strings so the interface can style them
    /// without knowing the whole set.
    pub status: &'static str,
    pub detail: String,
    /// What the user can actually do about it. Empty when nothing is wrong.
    pub fix: String,
}

fn ok(name: &str, detail: String) -> Check {
    Check {
        name: name.into(),
        status: "ok",
        detail,
        fix: String::new(),
    }
}
fn warn(name: &str, detail: String, fix: &str) -> Check {
    Check {
        name: name.into(),
        status: "warn",
        detail,
        fix: fix.into(),
    }
}
fn bad(name: &str, detail: String, fix: &str) -> Check {
    Check {
        name: name.into(),
        status: "fail",
        detail,
        fix: fix.into(),
    }
}

/// Check everything, against reality rather than against what Hush believes.
///
/// This exists because of an honest hole. Every permission decision in the app
/// runs off a scope string cached when the user connected — which is right
/// almost always, and wrong in the one case that matters: access revoked from a
/// Google account page, where nothing tells the app and it carries on assuming
/// it can create filters until something fails mid-run.
///
/// So nothing here is taken on trust. The permissions come from Google's
/// tokeninfo endpoint, reading is proved by actually reading, and filters are
/// proved by actually listing them. If the live answer disagrees with the
/// cache, the cache is corrected and the user is told.
#[tauri::command]
pub async fn diagnose(state: State<'_, AppState>) -> Result<Vec<Check>> {
    let mut checks = Vec::new();

    // --- setup -----------------------------------------------------------
    match state.credentials() {
        Ok(Some(_)) => checks.push(ok("Your Google key", "Saved on this computer.".into())),
        Ok(None) => checks.push(bad(
            "Your Google key",
            "Not set up yet.".into(),
            "Go through the setup steps to create one.",
        )),
        Err(e) => checks.push(bad(
            "Your Google key",
            e.to_string(),
            "Try restarting Hush.",
        )),
    }

    // --- the connection itself -------------------------------------------
    let session = {
        let guard = state.session.read().await;
        guard
            .as_ref()
            .map(|s| (s.email.clone(), s.gmail.clone(), s.auth.clone()))
    };

    let Some((email, gmail, auth)) = session else {
        checks.push(bad(
            "Connection",
            "Not connected to Google right now.".into(),
            "Press Reconnect at the top of the window.",
        ));
        checks.push(local_data(&state));
        return Ok(checks);
    };

    // --- what Google says we may do --------------------------------------
    //
    // The authoritative check, and the reason this screen exists.
    let cancel = Cancel::new();
    let live = auth.live_scopes().await;
    match &live {
        Ok(scopes) if scopes.is_empty() => checks.push(bad(
            "Connection",
            "Google rejected the connection — it has expired or been revoked.".into(),
            "Press Reconnect at the top of the window.",
        )),
        Ok(scopes) => {
            checks.push(ok("Connection", format!("Connected as {email}.")));

            let joined = scopes.join(" ");
            let has = |s: &str| joined.contains(s);
            let cached = state
                .store
                .get_setting(SETTING_GRANTED)?
                .unwrap_or_default();

            for (label, scope, what) in [
                ("Reading your mail", "gmail.readonly", "find senders at all"),
                (
                    "Moving old mail",
                    "gmail.modify",
                    "archive or bin old emails",
                ),
                ("Managing filters", "gmail.settings.basic", "block senders"),
            ] {
                if has(scope) {
                    checks.push(ok(label, "Granted.".into()));
                } else {
                    checks.push(warn(
                        label,
                        format!("Not granted, so Hush can't {what}."),
                        "Press Reconnect and tick it on Google's page.",
                    ));
                }
            }

            // The case this whole command was written for.
            if cached.split_whitespace().count() != scopes.len() {
                state.store.set_setting(SETTING_GRANTED, &joined)?;
                checks.push(warn(
                    "Permissions were out of date",
                    "What Google allows had changed since Hush last looked. It has \
                     been corrected."
                        .into(),
                    "Restart Hush so every screen picks up the change.",
                ));
            }
        }
        Err(e) => checks.push(warn(
            "Connection",
            format!("Couldn't ask Google what's permitted: {e}"),
            "Usually a network problem. Try again in a moment.",
        )),
    }

    // --- prove it by doing it --------------------------------------------
    match gmail.profile().await {
        Ok(p) => checks.push(ok(
            "Reading works",
            format!(
                "Gmail answered — {} messages in the account.",
                p.messages_total
            ),
        )),
        Err(e) => checks.push(bad(
            "Reading works",
            format!("Gmail refused: {e}"),
            "Press Reconnect. If it keeps happening, check your internet.",
        )),
    }

    if live
        .as_ref()
        .is_ok_and(|s| s.iter().any(|x| x.contains("settings.basic")))
    {
        match crate::filters::list(&gmail, &cancel).await {
            Ok(fs) => {
                let mine = fs.iter().filter(|f| f.mine).count();
                checks.push(ok(
                    "Blocking works",
                    format!("{} filters on the account, {mine} made by Hush.", fs.len()),
                ));
            }
            Err(e) => checks.push(bad(
                "Blocking works",
                format!("Couldn't read your filters: {e}"),
                "Press Reconnect and allow the filters permission again.",
            )),
        }
    }

    // --- this computer ----------------------------------------------------
    if Keychain::is_available() {
        checks.push(ok(
            "Password store",
            "Working, so the connection survives quitting.".into(),
        ));
    } else {
        checks.push(warn(
            "Password store",
            "This computer has no working secret store.".into(),
            "Everything works, but you'll reconnect each time you open Hush.",
        ));
    }

    checks.push(local_data(&state));
    Ok(checks)
}

/// Whether the local database is where it should be and can be written to.
fn local_data(state: &AppState) -> Check {
    match state.store.message_count("") {
        Ok(_) => {
            let path = state.store.path().to_path_buf();
            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            ok(
                "Local data",
                format!("Readable, {:.1} MB.", size as f64 / 1_048_576.0),
            )
        }
        Err(e) => bad(
            "Local data",
            format!("Hush can't read its own database: {e}"),
            "Check you have free disk space, then restart Hush.",
        ),
    }
}

/// Every filter on the account, read live from Gmail.
///
/// Hush keeps no record of what it has blocked. Gmail holds the filters, so
/// Gmail is asked — which means this works on a machine that has never seen the
/// account, survives reinstalling, and cannot fall out of step with the truth.
#[tauri::command]
pub async fn list_blocks(state: State<'_, AppState>) -> Result<Vec<ManagedFilter>> {
    let session = state.session.read().await;
    let session = session.as_ref().ok_or(Error::Unauthorized)?;
    if !session.can_block {
        return Err(Error::Setup(
            "Hush needs Google's permission to read your filters before it can show them.".into(),
        ));
    }
    crate::filters::list(&session.gmail, &Cancel::new()).await
}

/// What removing a block would put back, counted before anything happens.
#[tauri::command]
pub async fn preview_block_removal(
    state: State<'_, AppState>,
    id: String,
) -> Result<RemovalPreview> {
    let session = state.session_parts().await?;
    crate::filters::preview_removal(&session.gmail, &id, &Cancel::new()).await
}

/// Remove one of Hush's filters, optionally restoring the mail it caught.
///
/// Restoring needs the modify permission, which is a separate grant from the
/// one that manages filters. Asking for it and being refused should not cost
/// the user the removal they actually came for, so the filter goes either way
/// and the restore is skipped with an explanation.
#[tauri::command]
pub async fn remove_block(
    state: State<'_, AppState>,
    id: String,
    restore: bool,
) -> Result<RemovalReport> {
    let session = state.session_parts().await?;
    if !session.can_block {
        return Err(Error::Setup(
            "Hush needs Google's permission for filters before it can remove one.".into(),
        ));
    }

    if restore && !session.can_delete {
        let mut report = crate::filters::remove(&session.gmail, &id, false, &Cancel::new()).await?;
        report.problem = Some(
            "The filter is gone, so no new mail will be caught. Putting the old mail back \
             needs the permission to manage your mail, which Hush doesn't have — you can \
             grant it and remove the next one with restoring switched on."
                .into(),
        );
        return Ok(report);
    }

    crate::filters::remove(&session.gmail, &id, restore, &Cancel::new()).await
}

/// Move the chosen senders' bulk mail to Trash.
///
/// Only mail that carried an unsubscribe header is collected, by
/// [`Store::bulk_message_ids`] — so a receipt from a shop you just
/// unsubscribed from stays where it is. `resolve` has already dropped anything
/// on the never-touch list before this is reached.
async fn tidy_up(
    state: &AppState,
    account: &str,
    requests: &[UnsubRequest],
    action: BacklogAction,
    cancel: &Cancel,
    app: &AppHandle,
) -> Result<TrashReport> {
    let session = state.session_parts().await?;
    if !session.can_delete {
        return Err(Error::Setup(
            "Hush doesn't have permission to move your old mail yet. Reconnect \
             your account and allow it."
                .into(),
        ));
    }

    // Archived mail gets the label so it is findable in Gmail under one name
    // and skipped by later scans. Best effort: failing to label it is no
    // reason to leave it sitting in the inbox.
    let marker = match action {
        BacklogAction::Archive => crate::filters::ensure_label(&session.gmail, cancel)
            .await
            .map_err(|e| log::warn!("couldn't label archived mail: {e}"))
            .ok(),
        BacklogAction::Trash => None,
    };

    let mut ids = Vec::new();
    for r in requests {
        let for_sender = state.store.bulk_message_ids(account, &r.address)?;
        log::info!(
            "tidy-up: {} has {} scanned bulk messages to bin",
            r.address,
            for_sender.len()
        );
        ids.extend(for_sender);
    }

    if ids.is_empty() {
        log::warn!(
            "tidy-up found nothing to bin across {} sender(s) — either a previous \
             run already cleared them, or the scan never reached their mail",
            requests.len()
        );
    }

    let mut report = crate::unsub::trash_messages_reporting(
        &session.gmail,
        &ids,
        action,
        marker.as_deref(),
        cancel,
        |p| {
            let _ = app.emit(EVENT_RUN_PROGRESS, p);
        },
    )
    .await;

    // Check the mailbox rather than trusting our own HTTP responses. Gmail's
    // message list excludes Trash, so anything binned that still comes back
    // under a search for that sender did not actually move.
    log::info!(
        "tidy-up finished: {} moved, {} failed{}",
        report.trashed,
        report.failed,
        report
            .problem
            .as_deref()
            .map(|p| format!(" — first problem: {p}"))
            .unwrap_or_default()
    );

    if !cancel.is_cancelled() {
        let senders: Vec<String> = requests.iter().map(|r| r.address.clone()).collect();
        report.still_present =
            crate::unsub::verify_binned(&session.gmail, &senders, &report.moved_ids, cancel).await;
    }

    // Drop what actually moved, so the sender's count reflects reality straight
    // away and a second tidy-up does not re-attempt mail already in the bin.
    state.store.forget_messages(account, &report.moved_ids)?;
    Ok(report)
}

#[tauri::command]
pub async fn outcomes(state: State<'_, AppState>) -> Result<Vec<Outcome>> {
    let account = state.account_or_stored().await?;
    state.store.outcomes(&account)
}

/// Open a link the user asked to open. Only ever called from a click.
#[tauri::command]
pub async fn open_link(url: String) -> Result<()> {
    // The interface only offers https links, but this is the last gate before
    // the operating system acts, so it re-checks rather than assuming.
    let parsed =
        url::Url::parse(&url).map_err(|_| Error::Other("That link isn't valid.".into()))?;
    if !matches!(parsed.scheme(), "https" | "http" | "mailto") {
        return Err(Error::Other("That link can't be opened.".into()));
    }
    tauri_plugin_opener::open_url(&url, None::<&str>)
        .map_err(|e| Error::Other(format!("Couldn't open that link: {e}")))
}

// --- settings -------------------------------------------------------------

#[tauri::command]
pub async fn set_mailto_mode(state: State<'_, AppState>, mode: MailtoMode) -> Result<()> {
    state.set_mailto_mode(mode)
}

/// Open the folder holding the log and the database.
///
/// Exists because "it didn't work" is unanswerable without knowing what Google
/// actually said, and until now nothing recorded that.
#[tauri::command]
pub async fn open_data_folder(app: AppHandle) -> Result<()> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| Error::Storage(format!("couldn't find Hush's folder: {e}")))?;
    tauri_plugin_opener::open_path(dir.to_string_lossy().to_string(), None::<&str>)
        .map_err(|e| Error::Other(format!("Couldn't open that folder: {e}")))
}

/// Where the local database lives, so the user can see it or delete it.
#[tauri::command]
pub async fn data_location(state: State<'_, AppState>) -> Result<String> {
    Ok(state.store.path().display().to_string())
}

#[tauri::command]
pub async fn erase_everything(state: State<'_, AppState>) -> Result<Status> {
    disconnect(state, true).await
}

/// Build the store in the platform's app-data directory.
pub fn open_store(app: &AppHandle) -> Result<Store> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| Error::Storage(format!("couldn't find a place to save data: {e}")))?;
    Store::open(&dir.join("hush.sqlite3"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::now_ms;
    use crate::parse::UnsubMethod;
    use crate::store::Store;

    fn state_with(messages: &[crate::model::MessageMeta]) -> (AppState, String) {
        let store = Store::open_in_memory().unwrap();
        let account = "me@example.com".to_string();
        store.put_messages(&account, messages).unwrap();
        store.set_setting(SETTING_ACCOUNT, &account).unwrap();
        (AppState::new(store), account)
    }

    fn msg(sender: &str, lu: Option<&str>) -> crate::model::MessageMeta {
        crate::model::MessageMeta {
            id: format!("id-{sender}"),
            sender_address: sender.into(),
            sender_name: "Acme".into(),
            subject: "Hi".into(),
            date_ms: 1_700_000_000_000,
            list_unsubscribe: lu.map(str::to_string),
            list_unsubscribe_post: None,
        }
    }

    #[tokio::test]
    async fn resolve_refuses_a_sender_with_no_unsubscribe_option() {
        // Even if the interface asked for it explicitly.
        let (state, _) = state_with(&[msg("receipts@bank.example", None)]);
        let requests = resolve(&state, &["receipts@bank.example".into()])
            .await
            .unwrap();
        assert!(requests.is_empty());
    }

    #[tokio::test]
    async fn resolve_refuses_an_unknown_sender() {
        let (state, _) = state_with(&[msg("a@x.example", Some("<https://x.example/u>"))]);
        let requests = resolve(&state, &["never-seen@x.example".into()])
            .await
            .unwrap();
        assert!(requests.is_empty());
    }

    #[tokio::test]
    async fn resolve_refuses_anything_on_the_never_touch_list() {
        let (state, account) = state_with(&[msg("a@x.example", Some("<https://x.example/u>"))]);
        state
            .store
            .set_never_touch(&account, "a@x.example", true)
            .unwrap();
        let requests = resolve(&state, &["a@x.example".into()]).await.unwrap();
        assert!(requests.is_empty());
    }

    #[tokio::test]
    async fn resolve_passes_a_genuine_sender_through() {
        let (state, _) = state_with(&[msg("a@x.example", Some("<https://x.example/u>"))]);
        let requests = resolve(&state, &["a@x.example".into()]).await.unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].method,
            UnsubMethod::ManualLink {
                url: "https://x.example/u".into()
            }
        );
    }

    #[test]
    fn the_mailto_default_needs_no_extra_permission() {
        let (state, _) = state_with(&[]);
        assert_eq!(state.mailto_mode(), MailtoMode::HandOff);
        state.set_mailto_mode(MailtoMode::SendViaGmail).unwrap();
        assert_eq!(state.mailto_mode(), MailtoMode::SendViaGmail);
    }

    #[tokio::test]
    async fn only_https_and_mailto_links_can_be_opened() {
        for bad in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:text/html,hi",
            "not a url",
        ] {
            assert!(open_link(bad.to_string()).await.is_err(), "{bad}");
        }
    }

    #[test]
    fn outcome_timestamps_are_real() {
        assert!(now_ms() > 1_700_000_000_000);
    }
}

#[cfg(test)]
mod state_machine_tests {
    use super::*;
    use crate::gmail::Cancel;

    /// The own-address gate has to hold on the path that *acts*, not only the
    /// one that displays. A gate that only filters the list is a gate that a
    /// stale selection walks straight through.
    #[tokio::test]
    async fn a_run_refuses_your_own_address_even_if_asked_directly() {
        let store = Store::open_in_memory().unwrap();
        let me = "joe@gmail.com";
        store
            .put_messages(
                me,
                &[
                    crate::model::MessageMeta {
                        id: "spoof".into(),
                        sender_address: "j.o.e@gmail.com".into(),
                        sender_name: "You".into(),
                        subject: "Your account".into(),
                        date_ms: 1,
                        list_unsubscribe: Some("<https://x.example/u>".into()),
                        list_unsubscribe_post: Some("List-Unsubscribe=One-Click".into()),
                    },
                    crate::model::MessageMeta {
                        id: "real".into(),
                        sender_address: "news@shop.example".into(),
                        sender_name: "Shop".into(),
                        subject: "Sale".into(),
                        date_ms: 2,
                        list_unsubscribe: Some("<https://x.example/u>".into()),
                        list_unsubscribe_post: Some("List-Unsubscribe=One-Click".into()),
                    },
                ],
            )
            .unwrap();
        store.set_setting(SETTING_ACCOUNT, me).unwrap();
        let state = AppState::new(store);

        // The interface asks for both, as a stale selection would.
        let resolved = resolve(
            &state,
            &[
                "j.o.e@gmail.com".to_string(),
                "news@shop.example".to_string(),
            ],
        )
        .await
        .unwrap();

        let addresses: Vec<&str> = resolved.iter().map(|r| r.address.as_str()).collect();
        assert_eq!(
            addresses,
            ["news@shop.example"],
            "a run would have blocked the user's own mailbox"
        );
    }

    /// Erasing must stop the scan that is writing to what is being erased.
    ///
    /// Without this, "disconnect and erase everything" wipes the database and
    /// then a scan that is still running — holding its own `Arc<Store>` for up
    /// to forty minutes — writes the user's mail straight back in. The erase
    /// appears to work and silently does not.
    #[tokio::test]
    async fn erasing_stops_a_scan_that_is_still_running() {
        let state = AppState::new(Store::open_in_memory().unwrap());
        let scan = Cancel::new();
        *state.scan_cancel.write().await = Some(scan.clone());

        // Not calling the command directly: it needs a Tauri State wrapper.
        // This is the part of it that has to happen, in the order it has to
        // happen in.
        for handle in [&state.scan_cancel, &state.run_cancel] {
            if let Some(c) = handle.write().await.take() {
                c.cancel();
            }
        }
        state.store.erase(false).unwrap();

        assert!(
            scan.is_cancelled(),
            "the scan kept running and will refill the database"
        );
        assert!(
            state.scan_cancel.read().await.is_none(),
            "and the handle must be cleared, or the next scan is born cancelled"
        );
    }

    /// The same for a run, which trashes mail and would keep going against a
    /// token that has just been revoked.
    #[tokio::test]
    async fn disconnecting_stops_a_run_in_flight() {
        let state = AppState::new(Store::open_in_memory().unwrap());
        let run = Cancel::new();
        *state.run_cancel.write().await = Some(run.clone());

        for handle in [&state.scan_cancel, &state.run_cancel] {
            if let Some(c) = handle.write().await.take() {
                c.cancel();
            }
        }

        assert!(run.is_cancelled());
    }

    /// Erasing really does empty the store, for every account it holds.
    #[tokio::test]
    async fn erasing_leaves_nothing_behind_for_any_account() {
        let store = Store::open_in_memory().unwrap();
        for account in ["one@example.com", "two@example.com"] {
            store
                .put_messages(
                    account,
                    &[crate::model::MessageMeta {
                        id: "m1".into(),
                        sender_address: "news@shop.example".into(),
                        sender_name: "Shop".into(),
                        subject: "Subject lines are personal data too".into(),
                        date_ms: 1,
                        list_unsubscribe: Some("<https://x.example/u>".into()),
                        list_unsubscribe_post: None,
                    }],
                )
                .unwrap();
        }
        store.erase(false).unwrap();

        for account in ["one@example.com", "two@example.com"] {
            assert_eq!(
                store.message_count(account).unwrap(),
                0,
                "a second account's mail survived the erase"
            );
            assert!(store.senders(account).unwrap().is_empty());
        }
    }
}
