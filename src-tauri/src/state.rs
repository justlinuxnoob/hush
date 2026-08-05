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
