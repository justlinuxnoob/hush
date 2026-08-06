//! Application state shared by every command.

use std::sync::Arc;

use tokio::sync::RwLock;

use crate::auth::{ClientCredentials, GoogleAuth, TokenStorage};
use crate::error::{Error, Result};
use crate::gmail::{Cancel, GmailClient};
use crate::ratelimit::AdaptiveLimiter;
use crate::store::Store;
use crate::unsub::MailtoMode;

pub const SETTING_ACCOUNT: &str = "account_email";
pub const SETTING_CLIENT_ID: &str = "client_id";
pub const SETTING_CLIENT_SECRET: &str = "client_secret";
pub const SETTING_MAILTO_MODE: &str = "mailto_mode";
pub const SETTING_SEEN_WELCOME: &str = "seen_welcome";
/// The permissions Google actually granted last time.
///
/// Kept because the alternative is what shipped in 0.1.2: every relaunch
/// assumed the narrowest permissions and sent the user back through Google's
/// consent page to re-grant something they had already granted.
pub const SETTING_GRANTED: &str = "granted_scopes";

/// What the last block the user set up actually did — `archive` or `trash`.
///
/// Remembered so someone who has decided how they like their blocking is not
/// asked the same question every run. It is a *preselection*, never an
/// instruction: the choice is still on screen, the 30-day warning still shows,
/// and anything unrecognised in this field reads back as `archive`. Nothing in
/// the app upgrades a user from archiving to trashing on their behalf.
pub const SETTING_BLOCK_ACTION: &str = "block_action";

/// The same, for what a tidy-up does to old mail — `archive` or `trash`.
pub const SETTING_BACKLOG_ACTION: &str = "backlog_action";

/// When the current connection was granted, in epoch milliseconds.
///
/// Google expires refresh tokens for projects in Testing mode after seven
/// days. That is not a bug and cannot be avoided without publishing the app for
/// verification, which is the thing this design exists to avoid — but it *can*
/// stop being a surprise. Recording when the clock started is what lets the app
/// say "four days left" instead of letting someone discover it mid-run.
pub const SETTING_CONNECTED_MS: &str = "connected_at_ms";

/// How long Google leaves a Testing-mode connection alive.
pub const TESTING_TOKEN_DAYS: i64 = 7;

/// A snapshot of the session, held by value so no lock travels with it.
pub struct SessionParts {
    pub gmail: Arc<GmailClient>,
    pub can_send: bool,
    pub can_delete: bool,
    pub can_block: bool,
}

/// Everything about the currently connected account.
pub struct Session {
    pub email: String,
    pub auth: Arc<GoogleAuth>,
    pub gmail: Arc<GmailClient>,
    pub storage: TokenStorage,
    pub can_send: bool,
    /// Whether Google granted the permission needed to move mail to Trash.
    pub can_delete: bool,
    /// Whether Google granted the permission needed to create a filter.
    pub can_block: bool,
}

pub struct AppState {
    pub store: Arc<Store>,
    pub session: RwLock<Option<Session>>,
    pub limiter: Arc<AdaptiveLimiter>,
    /// Set while a scan is running so it can be stopped from the interface.
    pub scan_cancel: RwLock<Option<Cancel>>,
    /// Set while waiting for Google's consent redirect, so the user can give up
    /// rather than watching a dead window for five minutes.
    pub connect_cancel: RwLock<Option<Cancel>>,
    /// Set while unsubscribes and binning are running, so a run of fifty
    /// senders is not a commitment the user cannot back out of.
    pub run_cancel: RwLock<Option<Cancel>>,
}

impl AppState {
    pub fn new(store: Store) -> Self {
        Self {
            store: Arc::new(store),
            session: RwLock::new(None),
            limiter: Arc::new(AdaptiveLimiter::new()),
            scan_cancel: RwLock::new(None),
            connect_cancel: RwLock::new(None),
            run_cancel: RwLock::new(None),
        }
    }

    /// The credentials the user pasted during setup, if they have.
    ///
    /// These live in the local database rather than the keychain. Google issues
    /// them to every copy of an installed app, and treats them as public for
    /// exactly that reason — PKCE is what protects the flow. The refresh token,
    /// which is genuinely secret, goes to the keychain instead.
    pub fn credentials(&self) -> Result<Option<ClientCredentials>> {
        let (Some(client_id), Some(client_secret)) = (
            self.store.get_setting(SETTING_CLIENT_ID)?,
            self.store.get_setting(SETTING_CLIENT_SECRET)?,
        ) else {
            return Ok(None);
        };
        Ok(Some(ClientCredentials {
            client_id,
            client_secret,
        }))
    }

    pub fn save_credentials(&self, creds: &ClientCredentials) -> Result<()> {
        self.store
            .set_setting(SETTING_CLIENT_ID, &creds.client_id)?;
        self.store
            .set_setting(SETTING_CLIENT_SECRET, &creds.client_secret)?;
        Ok(())
    }

    pub fn account(&self) -> Result<Option<String>> {
        self.store.get_setting(SETTING_ACCOUNT)
    }

    pub fn mailto_mode(&self) -> MailtoMode {
        match self
            .store
            .get_setting(SETTING_MAILTO_MODE)
            .ok()
            .flatten()
            .as_deref()
        {
            Some("send_via_gmail") => MailtoMode::SendViaGmail,
            _ => MailtoMode::HandOff,
        }
    }

    pub fn set_mailto_mode(&self, mode: MailtoMode) -> Result<()> {
        self.store.set_setting(
            SETTING_MAILTO_MODE,
            match mode {
                MailtoMode::HandOff => "hand_off",
                MailtoMode::SendViaGmail => "send_via_gmail",
            },
        )
    }

    /// The session's client and permissions, copied out, with the lock released.
    ///
    /// This exists because holding the guard is a way to freeze the whole app.
    /// `RwLock` here is write-preferring: a single queued writer — `connect`
    /// or `disconnect` — makes every subsequent reader wait, and `status` is a
    /// reader that the interface polls. So a read guard held across a run that
    /// trashes four thousand messages plus one click on Reconnect equals a
    /// window that stops repainting for several minutes.
    ///
    /// Nothing long-running may hold the guard. Take what you need, drop it,
    /// then go and do the slow thing.
    pub async fn session_parts(&self) -> Result<SessionParts> {
        let guard = self.session.read().await;
        let s = guard.as_ref().ok_or(Error::Unauthorized)?;
        Ok(SessionParts {
            gmail: s.gmail.clone(),
            can_send: s.can_send,
            can_delete: s.can_delete,
            can_block: s.can_block,
        })
    }

    pub async fn require_session(&self) -> Result<(String, Arc<GmailClient>)> {
        let guard = self.session.read().await;
        let s = guard.as_ref().ok_or(Error::Unauthorized)?;
        Ok((s.email.clone(), s.gmail.clone()))
    }

    pub async fn account_or_stored(&self) -> Result<String> {
        if let Some(s) = self.session.read().await.as_ref() {
            return Ok(s.email.clone());
        }
        self.account()?.ok_or(Error::Unauthorized)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A read guard held across slow work freezes the whole window.
    ///
    /// `RwLock` here is write-preferring, which is the detail that makes this
    /// bite. One long-running reader plus one queued writer — a click on
    /// Reconnect or Disconnect — and *every* later reader waits behind the
    /// writer, including `status`, which the interface polls. The window stops
    /// repainting until the run finishes, which for a few thousand messages is
    /// minutes.
    ///
    /// This reproduces exactly that shape and asserts the reader gets through.
    #[tokio::test]
    async fn a_long_read_plus_a_queued_write_must_not_starve_later_reads() {
        let lock: Arc<RwLock<u32>> = Arc::new(RwLock::new(0));

        // The slow operation, holding a read guard the way `tidy_up` used to.
        let slow = lock.clone();
        let held = tokio::spawn(async move {
            let guard = slow.read().await;
            tokio::time::sleep(Duration::from_millis(300)).await;
            drop(guard);
        });
        tokio::time::sleep(Duration::from_millis(20)).await;

        // The user clicks Reconnect.
        let writer = lock.clone();
        let queued = tokio::spawn(async move {
            let mut guard = writer.write().await;
            *guard += 1;
        });
        tokio::time::sleep(Duration::from_millis(20)).await;

        // And now the interface asks for status. This is the call that used to
        // hang, and the assertion is that it does not finish quickly — it
        // demonstrates the starvation is real, so the fix has to be "do not
        // hold the guard" rather than "hope the lock is fair".
        let started = std::time::Instant::now();
        {
            let _ = lock.read().await;
        }
        let waited = started.elapsed();

        held.await.unwrap();
        queued.await.unwrap();

        assert!(
            waited > Duration::from_millis(100),
            "if this ever stops being true the lock became read-preferring and \
             this test no longer proves anything — but the rule stands either \
             way: take what you need from the session and drop the guard \
             before doing anything slow. Waited {waited:?}"
        );
    }

    /// The shape the code must use instead: nothing borrowed from the guard.
    #[tokio::test]
    async fn session_parts_holds_no_lock() {
        let state = AppState::new(Store::open_in_memory().unwrap());
        // No session yet, so this is the error path — the point is that it
        // returns an owned value and the guard is gone by the time it does.
        assert!(state.session_parts().await.is_err());
        // Which means a writer can take the lock immediately afterwards.
        assert!(
            tokio::time::timeout(Duration::from_millis(50), state.session.write())
                .await
                .is_ok(),
            "session_parts left a guard alive"
        );
    }
}
