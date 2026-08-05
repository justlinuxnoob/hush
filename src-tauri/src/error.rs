//! Error types.
//!
//! Every error that can reach the user interface carries a plain-language
//! message. The user of this app is not a developer, so "Bad credentials"
//! beats "401 Unauthorized" and neither the word "token" nor "API" belongs in
//! anything shown outside the setup wizard.

use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Google didn't accept the connection. Try connecting again.")]
    Unauthorized,

    #[error("Google is asking us to slow down. Hush will retry automatically.")]
    RateLimited,

    #[error("Couldn't reach Google. Check your internet connection and try again.")]
    Network(String),

    #[error("Google sent back something unexpected. Please try again.")]
    UnexpectedResponse(String),

    #[error("{0}")]
    Setup(String),

    #[error("Couldn't save to your computer's secure storage: {0}")]
    Keychain(String),

    #[error("Couldn't read Hush's local data: {0}")]
    Storage(String),

    #[error("The connection was cancelled.")]
    Cancelled,

    /// A one-click endpoint accepted the request and answered with a redirect.
    /// Delivered, but its outcome is the sender's to confirm, not ours.
    #[error("Sent, though the website didn't confirm it outright.")]
    Redirected,

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// The shape errors take when they cross into the interface.
///
/// `code` lets the UI branch (for example, to offer a Reconnect button) without
/// matching on human-readable text.
#[derive(Debug, Serialize)]
pub struct UiError {
    pub code: &'static str,
    pub message: String,
}

impl Serialize for Error {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        UiError {
            code: self.code(),
            message: self.to_string(),
        }
        .serialize(s)
    }
}

impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Error::Unauthorized => "unauthorized",
            Error::RateLimited => "rate_limited",
            Error::Network(_) => "network",
            Error::UnexpectedResponse(_) => "unexpected_response",
            Error::Setup(_) => "setup",
            Error::Keychain(_) => "keychain",
            Error::Storage(_) => "storage",
            Error::Cancelled => "cancelled",
            Error::Redirected => "redirected",
            Error::Other(_) => "other",
        }
    }
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Error::Network(e.to_string())
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::Storage(e.to_string())
    }
}

impl From<keyring::Error> for Error {
    fn from(e: keyring::Error) -> Self {
        Error::Keychain(e.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::UnexpectedResponse(e.to_string())
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Other(e.to_string())
    }
}
