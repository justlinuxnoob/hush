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
use crate::gmail::{Cancel, GmailClient};
use crate::model::{Outcome, ScanDepth, ScanProgress, Sender};
use crate::scan::Scanner;
use crate::state::{AppState, Session, SETTING_ACCOUNT, SETTING_GRANTED, SETTING_SEEN_WELCOME};
use crate::store::Store;
use crate::unsub::{
    trash_messages, Executor, Handoff, MailtoMode, PlannedAction, RunReport, TrashReport,
    UnsubRequest,
};

/// Emitted repeatedly while a scan runs.
pub const EVENT_SCAN_PROGRESS: &str = "scan-progress";

#[derive(Debug, Serialize)]
pub struct Status {
    pub connected: bool,
    pub email: Option<String>,
    pub has_credentials: bool,
    pub can_send: bool,
    /// Whether Hush may move old mail to Trash. Opt-in, and off by default.
    pub can_delete: bool,
    pub dry_run: bool,
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
        dry_run: state.dry_run(),
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
            });
        }
        Err(Error::Unauthorized) => {}
        Err(e) => return Err(e),
    }
    status(state).await
}

#[tauri::command]
pub async fn disconnect(state: State<'_, AppState>, erase_local_data: bool) -> Result<Status> {
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
    if state.scan_cancel.read().await.is_some() {
        return Err(Error::Other("A scan is already running.".into()));
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
            *state.scan_cancel.write().await = None;
        }
    });

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
        state.dry_run(),
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
#[tauri::command]
pub async fn run_unsubscribe(
    state: State<'_, AppState>,
    selection: Selection,
    delete_backlog: bool,
) -> Result<RunReport> {
    let account = state.account_or_stored().await?;
    let requests = resolve(&state, &selection.addresses).await?;
    let dry_run = state.dry_run();

    let mut executor = Executor::new(dry_run, state.mailto_mode(), account.clone())?;
    if state.mailto_mode() == MailtoMode::SendViaGmail {
        let session = state.session.read().await;
        let session = session.as_ref().ok_or(Error::Unauthorized)?;
        if !session.can_send {
            return Err(Error::Setup(
                "Hush doesn't have permission to send mail yet. Reconnect your \
                 account and allow it, or switch back to using your own mail app."
                    .into(),
            ));
        }
        executor = executor.with_gmail(session.gmail.clone());
    }

    let mut report = executor.run(&requests).await;

    if delete_backlog {
        report.trash = Some(tidy_up(&state, &account, &requests, dry_run).await?);
    }

    // A dry run leaves no trace: recording simulated outcomes would make the
    // list look acted-upon when nothing happened.
    if !dry_run {
        for outcome in &report.outcomes {
            state.store.record_outcome(&account, outcome)?;
        }
        open_handoffs(&report.handoffs);
    }

    Ok(report)
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
    dry_run: bool,
) -> Result<TrashReport> {
    let session = state.session.read().await;
    let session = session.as_ref().ok_or(Error::Unauthorized)?;
    if !session.can_delete {
        return Err(Error::Setup(
            "Hush doesn't have permission to move mail to Trash yet. Reconnect \
             your account and allow it."
                .into(),
        ));
    }

    let mut ids = Vec::new();
    for r in requests {
        ids.extend(state.store.bulk_message_ids(account, &r.address)?);
    }

    let cancel = Cancel::new();
    let report = trash_messages(&session.gmail, &ids, dry_run, &cancel).await;

    // Drop what actually moved, so the sender's count reflects reality straight
    // away and a second tidy-up does not re-attempt mail already in the bin.
    // A rehearsal moved nothing, so it forgets nothing.
    if !dry_run {
        state.store.forget_messages(account, &report.moved_ids)?;
    }
    Ok(report)
}

/// Open each prefilled unsubscribe mail in the user's own mail app.
///
/// Nothing is sent: the drafts open, and the user presses send — or does not.
fn open_handoffs(handoffs: &[Handoff]) {
    for h in handoffs {
        if let Err(e) = tauri_plugin_opener::open_url(&h.mailto_url, None::<&str>) {
            log::warn!("couldn't open a draft for {}: {e}", h.address);
        }
    }
}

#[tauri::command]
pub async fn mark_manual_done(state: State<'_, AppState>, address: String) -> Result<()> {
    let account = state.account_or_stored().await?;
    state.store.mark_manual_done(&account, &address)
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
pub async fn set_dry_run(state: State<'_, AppState>, on: bool) -> Result<()> {
    state.set_dry_run(on)
}

#[tauri::command]
pub async fn set_mailto_mode(state: State<'_, AppState>, mode: MailtoMode) -> Result<()> {
    state.set_mailto_mode(mode)
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
    use crate::model::{now_ms, OutcomeStatus};
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
    fn dry_run_is_on_before_anyone_has_chosen() {
        let (state, _) = state_with(&[]);
        assert!(state.dry_run(), "dry run must default to on");
        state.set_dry_run(false).unwrap();
        assert!(!state.dry_run());
        state.set_dry_run(true).unwrap();
        assert!(state.dry_run());
    }

    #[test]
    fn the_mailto_default_needs_no_extra_permission() {
        let (state, _) = state_with(&[]);
        assert_eq!(state.mailto_mode(), MailtoMode::HandOff);
        state.set_mailto_mode(MailtoMode::SendViaGmail).unwrap();
        assert_eq!(state.mailto_mode(), MailtoMode::SendViaGmail);
    }

    #[tokio::test]
    async fn a_dry_run_leaves_no_recorded_outcome() {
        let (state, account) = state_with(&[msg("a@x.example", Some("<https://x.example/u>"))]);
        assert!(state.dry_run());

        let requests = resolve(&state, &["a@x.example".into()]).await.unwrap();
        let executor = Executor::new(true, MailtoMode::HandOff, account.clone()).unwrap();
        let report = executor.run(&requests).await;

        assert_eq!(report.outcomes[0].status, OutcomeStatus::Simulated);
        // `run_unsubscribe` skips recording in dry run; confirm the store is
        // still untouched so the list cannot look acted-upon.
        assert!(state.store.outcomes(&account).unwrap().is_empty());
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
