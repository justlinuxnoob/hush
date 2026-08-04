//! OAuth 2.0 for installed applications: system browser, loopback redirect, PKCE.
//!
//! Design notes worth knowing before changing anything here:
//!
//! * **The consent screen opens in the real browser**, never an embedded
//!   webview. An embedded webview can read what the user types into Google's
//!   sign-in page. Ours would not, but the user cannot verify that, and Google
//!   blocks the pattern for exactly this reason.
//! * **PKCE is used even though desktop clients get a "secret".** That secret
//!   ships inside every copy of the app, so it is not a secret; PKCE is what
//!   actually binds the response to this session.
//! * **The refresh token goes to the OS keychain.** If the keychain is
//!   unavailable we hold it in memory for the session and say so, rather than
//!   quietly writing it to a file.

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use rand::Rng;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::RwLock;

use crate::error::{Error, Result};
use crate::gmail::TokenSource;

const AUTH_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
const REVOKE_ENDPOINT: &str = "https://oauth2.googleapis.com/revoke";

pub const SCOPE_READONLY: &str = "https://www.googleapis.com/auth/gmail.readonly";
pub const SCOPE_SEND: &str = "https://www.googleapis.com/auth/gmail.send";

/// How long we keep the loopback listener open waiting for the user to finish.
const CONSENT_TIMEOUT: Duration = Duration::from_secs(300);
/// Renew a little before expiry so a scan never trips over the boundary.
const EXPIRY_MARGIN: Duration = Duration::from_secs(120);

const KEYCHAIN_SERVICE: &str = "dev.hush.desktop";

/// The user's own Google Cloud OAuth client. Hush ships with none of these.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClientCredentials {
    pub client_id: String,
    /// Google issues one for desktop clients. It is embedded in every install
    /// of any such app, so it is not treated as a secret here — PKCE is what
    /// provides the real protection.
    pub client_secret: String,
}

impl ClientCredentials {
    /// Check the shape before we send the user to Google, so a typo produces a
    /// specific complaint here rather than a blank Google error page.
    pub fn validate(&self) -> Result<()> {
        let id = self.client_id.trim();
        let secret = self.client_secret.trim();

        if id.is_empty() {
            return Err(Error::Setup("The Client ID box is empty.".into()));
        }
        // Checked before anything about shape or length: an embedded space is a
        // copy-paste slip, and "that looks too short" would send the user
        // hunting for the wrong problem.
        if id.contains(char::is_whitespace) || secret.contains(char::is_whitespace) {
            return Err(Error::Setup(
                "There's a space in what you pasted. Try copying it again.".into(),
            ));
        }
        if !id.ends_with(".apps.googleusercontent.com") {
            return Err(Error::Setup(
                "That doesn't look like a Client ID. It should end with \
                 \".apps.googleusercontent.com\" — check you copied the whole thing."
                    .into(),
            ));
        }
        if id.len() < 40 {
            return Err(Error::Setup(
                "That Client ID looks too short. Copy the whole line from Google.".into(),
            ));
        }
        if secret.is_empty() {
            return Err(Error::Setup("The Client secret box is empty.".into()));
        }
        // Google's desktop client secrets have looked like this for years. A
        // warning rather than a hard rule would be missed, and a wrong secret
        // fails much later with a confusing message.
        if !secret.starts_with("GOCSPX-") {
            return Err(Error::Setup(
                "That doesn't look like a Client secret. Google's start with \
                 \"GOCSPX-\" — you may have pasted the Client ID twice."
                    .into(),
            ));
        }
        Ok(())
    }

    pub fn trimmed(&self) -> Self {
        Self {
            client_id: self.client_id.trim().to_string(),
            client_secret: self.client_secret.trim().to_string(),
        }
    }
}

/// Where the refresh token lives.
///
/// `Keychain` is the norm. `Memory` is the honest fallback for machines with no
/// working secret store — the user stays connected until they quit, and the UI
/// tells them why.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenStorage {
    Keychain,
    Memory,
}

pub struct Keychain {
    account: String,
}

impl Keychain {
    pub fn new(account: &str) -> Self {
        Self {
            account: account.to_string(),
        }
    }

    fn entry(&self) -> Result<keyring::Entry> {
        Ok(keyring::Entry::new(KEYCHAIN_SERVICE, &self.account)?)
    }

    pub fn store(&self, refresh_token: &str) -> Result<()> {
        self.entry()?.set_password(refresh_token)?;
        Ok(())
    }

    pub fn load(&self) -> Result<Option<String>> {
        match self.entry()?.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn erase(&self) -> Result<()> {
        match self.entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Whether a usable secret store exists on this machine.
    pub fn is_available() -> bool {
        keyring::Entry::new(KEYCHAIN_SERVICE, "__hush_probe__")
            .and_then(|e| match e.get_password() {
                Ok(_) => Ok(()),
                Err(keyring::Error::NoEntry) => Ok(()),
                Err(e) => Err(e),
            })
            .is_ok()
    }
}

/// Holds the live credentials for one connected account.
pub struct GoogleAuth {
    http: reqwest::Client,
    creds: ClientCredentials,
    token_endpoint: String,
    refresh_token: RwLock<Option<String>>,
    access: RwLock<Option<(String, Instant)>>,
    keychain: RwLock<Option<Keychain>>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: u64,
    #[serde(default)]
    scope: String,
}

#[derive(Debug, Deserialize)]
struct TokenError {
    #[serde(default)]
    error: String,
    #[serde(default)]
    error_description: String,
}

impl GoogleAuth {
    pub fn new(creds: ClientCredentials) -> Result<Arc<Self>> {
        Self::with_token_endpoint(creds, TOKEN_ENDPOINT)
    }

    /// Injectable endpoint so tests can exercise refresh and expiry offline.
    pub fn with_token_endpoint(creds: ClientCredentials, endpoint: &str) -> Result<Arc<Self>> {
        Ok(Arc::new(Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()?,
            creds: creds.trimmed(),
            token_endpoint: endpoint.to_string(),
            refresh_token: RwLock::new(None),
            access: RwLock::new(None),
            keychain: RwLock::new(None),
        }))
    }

    /// Restore a previously stored connection without any user interaction.
    pub async fn restore(&self, email: &str) -> Result<bool> {
        let kc = Keychain::new(email);
        let Some(token) = kc.load()? else {
            return Ok(false);
        };
        *self.refresh_token.write().await = Some(token);
        *self.keychain.write().await = Some(kc);
        Ok(true)
    }

    pub async fn has_refresh_token(&self) -> bool {
        self.refresh_token.read().await.is_some()
    }

    /// Run the full consent flow and return the connected account.
    ///
    /// `open_browser` is passed in rather than called directly so this can be
    /// driven from a test, and so the Tauri dependency stays out of this module.
    pub async fn connect<F>(&self, scopes: &[&str], open_browser: F) -> Result<String>
    where
        F: FnOnce(&str) -> Result<()>,
    {
        // Bind first: the port has to appear in the redirect URI we send.
        let listener = TcpListener::bind("127.0.0.1:0").await.map_err(|e| {
            Error::Other(format!(
                "Couldn't open a local connection to receive Google's reply: {e}"
            ))
        })?;
        let port = listener.local_addr()?.port();
        let redirect_uri = format!("http://127.0.0.1:{port}");

        let verifier = random_urlsafe(64);
        let challenge = s256_challenge(&verifier);
        let state = random_urlsafe(32);

        let auth_url = build_auth_url(
            &self.creds.client_id,
            &redirect_uri,
            scopes,
            &challenge,
            &state,
        );
        open_browser(&auth_url)?;

        let code = wait_for_code(listener, &state).await?;
        let tokens = self.exchange_code(&code, &verifier, &redirect_uri).await?;

        let refresh = tokens.refresh_token.ok_or_else(|| {
            Error::Setup(
                "Google didn't send a lasting connection. In your Google account's \
                 security settings, remove Hush's access and connect again."
                    .into(),
            )
        })?;
        *self.refresh_token.write().await = Some(refresh);
        self.set_access(tokens.access_token, tokens.expires_in)
            .await;
        Ok(tokens.scope)
    }

    /// Persist the refresh token now that we know which account it belongs to.
    pub async fn persist(&self, email: &str) -> TokenStorage {
        let Some(token) = self.refresh_token.read().await.clone() else {
            return TokenStorage::Memory;
        };
        let kc = Keychain::new(email);
        match kc.store(&token) {
            Ok(()) => {
                *self.keychain.write().await = Some(kc);
                TokenStorage::Keychain
            }
            // Not fatal: the session continues, and the caller tells the user
            // they will need to reconnect after quitting.
            Err(e) => {
                log::warn!("secure storage unavailable, keeping token in memory only: {e}");
                TokenStorage::Memory
            }
        }
    }

    /// Forget everything: revoke with Google, clear the keychain, clear memory.
    pub async fn disconnect(&self) -> Result<()> {
        let token = self.refresh_token.write().await.take();
        *self.access.write().await = None;

        if let Some(kc) = self.keychain.write().await.take() {
            // Erase locally even if the network revoke fails; a token we cannot
            // reach is still a token we should not keep.
            let _ = kc.erase();
        }

        if let Some(token) = token {
            let _ = self
                .http
                .post(REVOKE_ENDPOINT)
                .form(&[("token", token.as_str())])
                .send()
                .await;
        }
        Ok(())
    }

    async fn exchange_code(
        &self,
        code: &str,
        verifier: &str,
        redirect_uri: &str,
    ) -> Result<TokenResponse> {
        let form = [
            ("code", code),
            ("client_id", self.creds.client_id.as_str()),
            ("client_secret", self.creds.client_secret.as_str()),
            ("code_verifier", verifier),
            ("grant_type", "authorization_code"),
            ("redirect_uri", redirect_uri),
        ];
        self.post_token(&form).await
    }

    async fn post_token(&self, form: &[(&str, &str)]) -> Result<TokenResponse> {
        let response = self
            .http
            .post(&self.token_endpoint)
            .form(form)
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;

        if status.is_success() {
            return Ok(serde_json::from_str(&body)?);
        }

        let err: TokenError = serde_json::from_str(&body).unwrap_or(TokenError {
            error: String::new(),
            error_description: String::new(),
        });
        Err(match err.error.as_str() {
            "invalid_grant" => Error::Unauthorized,
            "invalid_client" => Error::Setup(
                "Google didn't recognise those credentials. Check the Client ID and \
                 secret in setup — it's easy to paste one into the other's box."
                    .into(),
            ),
            "access_denied" => Error::Setup(
                "Google turned the request down. If your project is in Testing mode, \
                 make sure your own address is listed as a test user."
                    .into(),
            ),
            _ => Error::Setup(format!(
                "Google couldn't complete the connection{}.",
                if err.error_description.is_empty() {
                    String::new()
                } else {
                    format!(": {}", err.error_description)
                }
            )),
        })
    }

    async fn set_access(&self, token: String, expires_in: u64) {
        let ttl = Duration::from_secs(expires_in.max(60));
        let expires_at = Instant::now() + ttl.saturating_sub(EXPIRY_MARGIN);
        *self.access.write().await = Some((token, expires_at));
    }

    async fn refresh(&self) -> Result<String> {
        let refresh = self
            .refresh_token
            .read()
            .await
            .clone()
            .ok_or(Error::Unauthorized)?;

        let form = [
            ("refresh_token", refresh.as_str()),
            ("client_id", self.creds.client_id.as_str()),
            ("client_secret", self.creds.client_secret.as_str()),
            ("grant_type", "refresh_token"),
        ];
        let tokens = match self.post_token(&form).await {
            Ok(t) => t,
            Err(Error::Unauthorized) => {
                // Testing-mode connections lapse after seven days. Drop the dead
                // token so the UI offers a clean reconnect instead of looping.
                *self.refresh_token.write().await = None;
                if let Some(kc) = self.keychain.read().await.as_ref() {
                    let _ = kc.erase();
                }
                return Err(Error::Unauthorized);
            }
            Err(e) => return Err(e),
        };

        // A refresh may hand back a replacement refresh token; keep it if so.
        if let Some(new_refresh) = tokens.refresh_token.clone() {
            *self.refresh_token.write().await = Some(new_refresh.clone());
            if let Some(kc) = self.keychain.read().await.as_ref() {
                let _ = kc.store(&new_refresh);
            }
        }

        self.set_access(tokens.access_token.clone(), tokens.expires_in)
            .await;
        Ok(tokens.access_token)
    }
}

impl TokenSource for GoogleAuth {
    fn access_token(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + '_>> {
        Box::pin(async move {
            if let Some((token, expires_at)) = self.access.read().await.as_ref() {
                if Instant::now() < *expires_at {
                    return Ok(token.clone());
                }
            }
            self.refresh().await
        })
    }

    fn force_refresh(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + '_>> {
        Box::pin(async move {
            *self.access.write().await = None;
            self.refresh().await
        })
    }
}

fn build_auth_url(
    client_id: &str,
    redirect_uri: &str,
    scopes: &[&str],
    challenge: &str,
    state: &str,
) -> String {
    let scope = scopes.join(" ");
    let params = [
        ("client_id", client_id),
        ("redirect_uri", redirect_uri),
        ("response_type", "code"),
        ("scope", scope.as_str()),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
        // Without offline access we would get no refresh token and the user
        // would have to sign in on every launch.
        ("access_type", "offline"),
        // Force the consent screen so a re-connect reliably yields a refresh
        // token; Google omits it on silent re-authorisation.
        ("prompt", "consent"),
        ("state", state),
    ];
    let query = params
        .iter()
        .map(|(k, v)| format!("{}={}", k, urlencode(v)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{AUTH_ENDPOINT}?{query}")
}

fn urlencode(s: &str) -> String {
    use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
    utf8_percent_encode(s, NON_ALPHANUMERIC).to_string()
}

fn random_urlsafe(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill(&mut buf[..]);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

fn s256_challenge(verifier: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

/// Accept connections until Google's redirect arrives, then answer with a page
/// telling the user to go back to the app.
///
/// Browsers open speculative connections and ask for `/favicon.ico`, so this
/// keeps listening rather than treating the first connection as the answer.
async fn wait_for_code(listener: TcpListener, expected_state: &str) -> Result<String> {
    let deadline = tokio::time::Instant::now() + CONSENT_TIMEOUT;

    loop {
        let accept = tokio::time::timeout_at(deadline, listener.accept()).await;
        let Ok(accepted) = accept else {
            return Err(Error::Setup(
                "The connection timed out waiting for Google. Try connecting again.".into(),
            ));
        };
        let (mut stream, _) = accepted?;

        let mut buf = vec![0u8; 8192];
        let n = stream.read(&mut buf).await.unwrap_or(0);
        if n == 0 {
            continue;
        }
        let request = String::from_utf8_lossy(&buf[..n]);
        let Some(target) = request
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
        else {
            continue;
        };

        let url = match url::Url::parse(&format!("http://127.0.0.1{target}")) {
            Ok(u) => u,
            Err(_) => continue,
        };

        let mut code = None;
        let mut state = None;
        let mut error = None;
        for (k, v) in url.query_pairs() {
            match k.as_ref() {
                "code" => code = Some(v.to_string()),
                "state" => state = Some(v.to_string()),
                "error" => error = Some(v.to_string()),
                _ => {}
            }
        }

        if let Some(error) = error {
            let _ = respond(&mut stream, &closing_page(false)).await;
            return Err(match error.as_str() {
                "access_denied" => Error::Setup(
                    "You turned down the request on Google's page. Nothing was connected.".into(),
                ),
                other => Error::Setup(format!("Google reported a problem: {other}")),
            });
        }

        let Some(code) = code else {
            // Almost certainly /favicon.ico. Keep waiting for the real thing.
            let _ = respond(&mut stream, "").await;
            continue;
        };

        // The state check is what stops another local process from feeding us
        // a code of its choosing.
        if state.as_deref() != Some(expected_state) {
            let _ = respond(&mut stream, &closing_page(false)).await;
            return Err(Error::Setup(
                "Google's reply didn't match this request, so it was ignored. \
                 Please try connecting again."
                    .into(),
            ));
        }

        let _ = respond(&mut stream, &closing_page(true)).await;
        return Ok(code);
    }
}

async fn respond(stream: &mut tokio::net::TcpStream, body: &str) -> std::io::Result<()> {
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(response.as_bytes()).await?;
    stream.flush().await
}

/// The page the user lands on after consenting. It is the last thing they see
/// in the browser, so it should look like it belongs to the app.
fn closing_page(ok: bool) -> String {
    let (title, message) = if ok {
        (
            "You're connected",
            "You can close this tab and go back to Hush.",
        )
    } else {
        (
            "Not connected",
            "Nothing was changed. You can close this tab and try again in Hush.",
        )
    };
    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8">
<title>{title}</title><style>
:root{{color-scheme:light dark}}
body{{margin:0;min-height:100vh;display:grid;place-items:center;
font:16px/1.6 ui-sans-serif,-apple-system,"Segoe UI",Roboto,sans-serif;
background:#faf9f7;color:#1c1a17}}
@media (prefers-color-scheme:dark){{body{{background:#131211;color:#f0ede8}}}}
.card{{max-width:26rem;padding:2.5rem;text-align:center}}
h1{{font-size:1.5rem;font-weight:600;margin:0 0 .5rem}}
p{{margin:0;opacity:.72}}
</style></head><body><div class="card"><h1>{title}</h1><p>{message}</p></div></body></html>"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shaped like the real thing so `validate` accepts it, but obviously fake.
    /// The "not-a-real" marker is what keeps CI's embedded-credential check from
    /// tripping over these lines — see .github/workflows/ci.yml.
    fn creds() -> ClientCredentials {
        ClientCredentials {
            client_id: "not-a-real-client-id-used-only-by-tests.apps.googleusercontent.com"
                .into(),
            client_secret: "GOCSPX-not-a-real-secret-for-tests".into(),
        }
    }

    #[test]
    fn good_credentials_validate() {
        creds().validate().unwrap();
    }

    #[test]
    fn credential_mistakes_get_specific_advice() {
        let cases: Vec<(ClientCredentials, &str)> = vec![
            (
                ClientCredentials {
                    client_id: "".into(),
                    ..creds()
                },
                "empty",
            ),
            (
                ClientCredentials {
                    client_id: "not-a-client-id".into(),
                    ..creds()
                },
                "apps.googleusercontent.com",
            ),
            (
                ClientCredentials {
                    client_secret: "".into(),
                    ..creds()
                },
                "empty",
            ),
            (
                // The classic slip: the Client ID pasted into both boxes.
                ClientCredentials {
                    client_secret: creds().client_id,
                    ..creds()
                },
                "GOCSPX-",
            ),
            (
                ClientCredentials {
                    client_id: "not-a-real id-with-a-space.apps.googleusercontent.com".into(),
                    ..creds()
                },
                "space",
            ),
        ];
        for (c, expected) in cases {
            let err = c.validate().unwrap_err().to_string();
            assert!(err.contains(expected), "got {err:?}, wanted {expected:?}");
        }
    }

    #[test]
    fn surrounding_whitespace_is_forgiven() {
        let c = ClientCredentials {
            client_id: format!("  {}\n", creds().client_id),
            client_secret: format!("\t{} ", creds().client_secret),
        };
        c.trimmed().validate().unwrap();
    }

    #[test]
    fn pkce_challenge_matches_the_rfc7636_example() {
        // RFC 7636 Appendix B.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            s256_challenge(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn verifiers_are_long_random_and_urlsafe() {
        let a = random_urlsafe(64);
        let b = random_urlsafe(64);
        assert_ne!(a, b);
        // RFC 7636 requires 43..=128 characters.
        assert!((43..=128).contains(&a.len()), "length {}", a.len());
        assert!(a
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn the_auth_url_carries_pkce_and_offline_access() {
        let url = build_auth_url(
            "cid",
            "http://127.0.0.1:5000",
            &[SCOPE_READONLY],
            "chal",
            "st",
        );
        assert!(url.starts_with(AUTH_ENDPOINT));
        for expected in [
            "code_challenge=chal",
            "code_challenge_method=S256",
            "access_type=offline",
            "response_type=code",
            "state=st",
            "prompt=consent",
        ] {
            assert!(url.contains(expected), "missing {expected} in {url}");
        }
        assert!(url.contains("redirect_uri=http%3A%2F%2F127%2E0%2E0%2E1%3A5000"));
        assert!(url.contains("gmail%2Ereadonly"));
        // The send permission must never be requested implicitly.
        assert!(!url.contains("gmail%2Esend"));
    }

    #[test]
    fn scopes_are_space_separated_when_send_is_requested() {
        let url = build_auth_url(
            "cid",
            "http://127.0.0.1:1",
            &[SCOPE_READONLY, SCOPE_SEND],
            "c",
            "s",
        );
        assert!(url.contains("gmail%2Ereadonly%20https"));
        assert!(url.contains("gmail%2Esend"));
    }

    #[tokio::test]
    async fn the_loopback_listener_accepts_the_redirect() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let task = tokio::spawn(async move { wait_for_code(listener, "the-state").await });

        // A browser preflight for /favicon.ico must not be mistaken for the
        // redirect; the listener has to keep waiting.
        let _ = reqwest::get(format!("http://127.0.0.1:{port}/favicon.ico")).await;
        let landing = reqwest::get(format!(
            "http://127.0.0.1:{port}/?code=the-code&state=the-state"
        ))
        .await
        .unwrap();
        let page = landing.text().await.unwrap();
        assert!(page.contains("You're connected"));

        assert_eq!(task.await.unwrap().unwrap(), "the-code");
    }

    #[tokio::test]
    async fn a_mismatched_state_is_rejected() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move { wait_for_code(listener, "expected").await });

        let _ = reqwest::get(format!(
            "http://127.0.0.1:{port}/?code=injected&state=attacker"
        ))
        .await;

        let err = task.await.unwrap().unwrap_err();
        assert!(err.to_string().contains("didn't match"));
    }

    #[tokio::test]
    async fn a_refusal_on_googles_page_is_reported_plainly() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move { wait_for_code(listener, "s").await });

        let _ = reqwest::get(format!(
            "http://127.0.0.1:{port}/?error=access_denied&state=s"
        ))
        .await;

        let err = task.await.unwrap().unwrap_err().to_string();
        assert!(err.contains("turned down"), "{err}");
        assert!(!err.contains("access_denied"), "{err}");
    }

    #[test]
    fn the_closing_page_says_what_to_do_next() {
        assert!(closing_page(true).contains("close this tab"));
        assert!(closing_page(false).contains("Nothing was changed"));
    }
}
