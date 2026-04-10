//! Google OAuth 2.0 (installed app / loopback) authentication and token persistence.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};
use oauth2::basic::BasicClient;
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointNotSet, EndpointSet,
    PkceCodeChallenge, RedirectUrl, RefreshToken, Scope, TokenUrl,
};
use oauth2::TokenResponse as _;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tracing::{debug, info, warn};

use crate::error::OxidriveError;

/// Google OAuth client with authorization + token endpoints configured (redirect URI is applied per flow).
type GoogleOAuthClient = BasicClient<
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointSet,
>;

/// Serializable OAuth token bundle stored at the configured `token_path` (typically `token.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenResponse {
    /// Bearer access token for Google APIs.
    pub access_token: String,
    /// Token type (usually `Bearer`).
    #[serde(default)]
    pub token_type: Option<String>,
    /// Refresh token, when granted by the authorization server.
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// `expires_in` seconds from the token response, if provided.
    #[serde(default)]
    pub expires_in: Option<u64>,
    /// Granted scopes, if returned by the server.
    #[serde(default)]
    pub scope: Option<String>,
    /// Absolute expiry instant for the access token (UTC), if known.
    #[serde(default, with = "chrono::serde::ts_seconds_option")]
    pub expires_at: Option<chrono::DateTime<Utc>>,
}

/// Errors specific to the OAuth / token lifecycle.
#[derive(Debug, Error)]
pub enum AuthError {
    /// Misconfigured OAuth endpoints or redirect URL.
    #[error("OAuth configuration error: {0}")]
    Configuration(String),
    /// Failure opening the system browser or local HTTP server.
    #[error("local OAuth loopback error: {0}")]
    Loopback(String),
    /// Could not parse the OAuth callback request.
    #[error("invalid OAuth callback request")]
    OAuthCallbackParse,
    /// Authorization server returned an explicit OAuth error (for example, user denied consent).
    #[error("OAuth authorization denied: {0}")]
    OAuthDenied(String),
    /// CSRF `state` did not match the value issued at authorization time.
    #[error("OAuth state mismatch (possible CSRF)")]
    StateMismatch,
    /// Token exchange or refresh failed.
    #[error("token request failed: {0}")]
    TokenRequest(String),
    /// Token file missing or unusable.
    #[error("not authorized (missing or invalid token)")]
    NotAuthorized,
    /// I/O while reading/writing the token file.
    #[error("token storage I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// JSON (de)serialization of the token file.
    #[error("token JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

impl From<AuthError> for OxidriveError {
    fn from(value: AuthError) -> Self {
        OxidriveError::Auth(value.to_string())
    }
}

impl From<oauth2::url::ParseError> for AuthError {
    fn from(value: oauth2::url::ParseError) -> Self {
        AuthError::Configuration(value.to_string())
    }
}

/// Manages Google OAuth credentials, loopback authorization, and JSON token persistence.
pub struct AuthManager {
    token_path: PathBuf,
    client_id: String,
    client_secret: String,
    http: reqwest::Client,
}

impl AuthManager {
    /// Creates a manager for the given OAuth client credentials and token storage path.
    ///
    /// Google authorization and token endpoints are applied when starting a flow or refreshing tokens.
    pub fn new(
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
        token_path: impl Into<PathBuf>,
    ) -> Self {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            token_path: token_path.into(),
            client_id: client_id.into(),
            client_secret: client_secret.into(),
            http,
        }
    }

    /// Builds a [`BasicClient`] with Google’s OAuth endpoints and this manager’s credentials.
    fn base_oauth_client(&self) -> Result<GoogleOAuthClient, AuthError> {
        let auth_url = AuthUrl::new("https://accounts.google.com/o/oauth2/v2/auth".to_string())?;
        let token_url = TokenUrl::new("https://oauth2.googleapis.com/token".to_string())?;
        Ok(
            BasicClient::new(ClientId::new(self.client_id.clone()))
                .set_client_secret(ClientSecret::new(self.client_secret.clone()))
                .set_auth_uri(auth_url)
                .set_token_uri(token_url),
        )
    }

    /// Runs the interactive browser + loopback flow, exchanges the authorization code, and saves [`TokenResponse`] JSON to disk.
    pub async fn setup(&self) -> Result<(), OxidriveError> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.map_err(|e| {
            AuthError::Loopback(format!("failed to bind loopback listener: {e}"))
        })?;
        let port = listener.local_addr().map_err(|e| {
            AuthError::Loopback(format!("failed to read local addr: {e}"))
        })?;
        let redirect = RedirectUrl::new(format!("http://127.0.0.1:{}/", port.port()))
            .map_err(AuthError::from)?;

        let oauth_client = self.base_oauth_client()?.set_redirect_uri(redirect);
        let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();
        let (auth_url, csrf) = oauth_client
            .authorize_url(CsrfToken::new_random)
            .add_scope(Scope::new(
                "https://www.googleapis.com/auth/drive".to_string(),
            ))
            .set_pkce_challenge(pkce_challenge)
            .url();

        info!(url = %auth_url, "opening browser for Google OAuth consent");
        open_browser(auth_url.as_str()).map_err(|e| {
            AuthError::Loopback(format!("failed to open browser: {e}"))
        })?;

        let code = match tokio::time::timeout(Duration::from_secs(10 * 60), async {
            accept_oauth_callback(listener, csrf.secret().as_str()).await
        })
        .await
        {
            Ok(res) => res?,
            Err(_) => {
                return Err(AuthError::Loopback("timed out waiting for OAuth callback".into()).into());
            }
        };

        let token = oauth_client
            .exchange_code(AuthorizationCode::new(code))
            .set_pkce_verifier(pkce_verifier)
            .request_async(&self.http)
            .await
            .map_err(|e| AuthError::TokenRequest(e.to_string()))?;

        let stored = token_response_from_oauth(&token);
        self.save_token(&stored)?;
        info!(path = %self.token_path.display(), "saved OAuth token");
        Ok(())
    }

    /// Loads a valid access token, refreshing with the refresh token when expired or near expiry.
    pub async fn get_access_token(&self) -> Result<String, OxidriveError> {
        let token = self.load_token()?;
        if access_token_usable(&token) {
            return Ok(token.access_token);
        }
        let Some(ref refresh) = token.refresh_token else {
            return Err(AuthError::NotAuthorized.into());
        };
        debug!("access token expired or near expiry; refreshing");
        let refreshed = self
            .base_oauth_client()?
            .exchange_refresh_token(&RefreshToken::new(refresh.clone()))
            .request_async(&self.http)
            .await
            .map_err(|e| AuthError::TokenRequest(e.to_string()))?;
        let mut updated = token_response_from_oauth(&refreshed);
        if updated.refresh_token.is_none() {
            updated.refresh_token = token.refresh_token.clone();
        }
        self.save_token(&updated)?;
        Ok(updated.access_token)
    }

    /// Reads the token file from disk.
    pub fn load_token(&self) -> Result<TokenResponse, OxidriveError> {
        let bytes = match std::fs::read(&self.token_path) {
            Ok(b) => b,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                return Err(AuthError::NotAuthorized.into());
            }
            Err(e) => return Err(AuthError::from(e).into()),
        };
        let t: TokenResponse = serde_json::from_slice(&bytes).map_err(AuthError::from)?;
        Ok(t)
    }

    /// Persists `token` as JSON at the configured path (staging file then rename).
    pub fn save_token(&self, token: &TokenResponse) -> Result<(), OxidriveError> {
        if let Some(parent) = self.token_path.parent() {
            std::fs::create_dir_all(parent).map_err(AuthError::from)?;
        }
        let data = serde_json::to_vec_pretty(token).map_err(AuthError::from)?;
        let tmp = self.token_path.with_extension("json.part");
        std::fs::write(&tmp, &data).map_err(AuthError::from)?;
        std::fs::rename(&tmp, &self.token_path).map_err(AuthError::from)?;
        Ok(())
    }
}

fn access_token_usable(token: &TokenResponse) -> bool {
    match token.expires_at {
        Some(exp) => Utc::now() < exp - ChronoDuration::seconds(60),
        None => false,
    }
}

fn token_response_from_oauth(token: &oauth2::basic::BasicTokenResponse) -> TokenResponse {
    let access_token = token.access_token().secret().to_string();
    let refresh_token = token
        .refresh_token()
        .map(|t| t.secret().to_string());
    let expires_at = token.expires_in().map(|d| {
        let secs = i64::try_from(d.as_secs()).unwrap_or(i64::MAX);
        Utc::now() + ChronoDuration::seconds(secs)
    });
    let scope = token.scopes().and_then(|scopes| {
        let joined = scopes
            .iter()
            .map(|s| s.as_ref())
            .collect::<Vec<_>>()
            .join(" ");
        if joined.is_empty() {
            None
        } else {
            Some(joined)
        }
    });
    TokenResponse {
        access_token,
        token_type: Some("Bearer".to_string()),
        refresh_token,
        expires_in: token.expires_in().map(|d| d.as_secs()),
        scope,
        expires_at,
    }
}

fn open_browser(url: &str) -> Result<(), std::io::Error> {
    #[cfg(target_os = "macos")]
    {
        let status = Command::new("open").arg(url).status()?;
        if !status.success() {
            return Err(std::io::Error::new(
                ErrorKind::Other,
                "`open` exited with non-zero status",
            ));
        }
        return Ok(());
    }
    #[cfg(target_os = "windows")]
    {
        let status = Command::new("cmd")
            .args(["/C", "start", ""])
            .arg(url)
            .status()?;
        if !status.success() {
            return Err(std::io::Error::new(
                ErrorKind::Other,
                "`cmd /c start` exited with non-zero status",
            ));
        }
        return Ok(());
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let status = Command::new("xdg-open")
            .arg(url)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        if !status.success() {
            return Err(std::io::Error::other(
                "`xdg-open` exited with non-zero status",
            ));
        }
        Ok(())
    }
}

async fn accept_oauth_callback(listener: TcpListener, expected_state: &str) -> Result<String, AuthError> {
    let (mut stream, peer) = listener.accept().await.map_err(|e| {
        AuthError::Loopback(format!("accept failed: {e}"))
    })?;
    debug!(%peer, "accepted OAuth callback connection");

    const MAX_REQUEST_BYTES: usize = 64 * 1024;
    let mut request = Vec::with_capacity(4096);
    let mut read_buf = [0u8; 4096];

    loop {
        let n = stream
            .read(&mut read_buf)
            .await
            .map_err(|e| AuthError::Loopback(format!("read failed: {e}")))?;
        if n == 0 {
            break;
        }
        request.extend_from_slice(&read_buf[..n]);
        if request.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if request.len() >= MAX_REQUEST_BYTES {
            return Err(AuthError::OAuthCallbackParse);
        }
    }
    let req = std::str::from_utf8(&request).map_err(|_| AuthError::OAuthCallbackParse)?;

    let first = req.lines().next().ok_or(AuthError::OAuthCallbackParse)?;
    let path = first
        .split_whitespace()
        .nth(1)
        .ok_or(AuthError::OAuthCallbackParse)?;
    let query = path
        .split_once('?')
        .map(|(_, q)| q)
        .ok_or(AuthError::OAuthCallbackParse)?;
    let params = parse_query(query);
    let state = params
        .get("state")
        .ok_or(AuthError::OAuthCallbackParse)?
        .as_str();
    if state != expected_state {
        warn!("OAuth callback state mismatch");
        return Err(AuthError::StateMismatch);
    }
    if let Some(error) = params.get("error") {
        let message = params
            .get("error_description")
            .cloned()
            .unwrap_or_else(|| error.clone());
        return Err(AuthError::OAuthDenied(message));
    }
    let code = params
        .get("code")
        .ok_or(AuthError::OAuthCallbackParse)?
        .clone();

    let body = b"<!doctype html><meta charset=\"utf-8\"><title>oxidrive</title><p>Authorization complete. You can close this window.</p>";
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .await
        .map_err(|e| AuthError::Loopback(format!("write failed: {e}")))?;
    stream
        .write_all(body)
        .await
        .map_err(|e| AuthError::Loopback(format!("write failed: {e}")))?;

    Ok(code)
}

fn parse_query(query: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for pair in query.split('&') {
        let Some((k, v)) = pair.split_once('=') else {
            continue;
        };
        out.insert(
            url_decode(k),
            url_decode(v),
        );
    }
    out
}

fn url_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '+' {
            out.push(' ');
        } else if c == '%' {
            let a = chars.next();
            let b = chars.next();
            if let (Some(a), Some(b)) = (a, b) {
                let h = format!("{a}{b}");
                if let Ok(v) = u8::from_str_radix(&h, 16) {
                    out.push(char::from(v));
                    continue;
                }
            }
            out.push(c);
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_query_round_trip_simple() {
        let q = "code=abc&state=xyz";
        let m = parse_query(q);
        assert_eq!(m.get("code").map(String::as_str), Some("abc"));
        assert_eq!(m.get("state").map(String::as_str), Some("xyz"));
    }

    #[test]
    fn token_response_serde_round_trip() {
        let t = TokenResponse {
            access_token: "a".into(),
            token_type: Some("Bearer".into()),
            refresh_token: Some("r".into()),
            expires_in: Some(3600),
            scope: Some("drive".into()),
            expires_at: Some(Utc::now()),
        };
        let v = serde_json::to_vec(&t).expect("ser");
        let back: TokenResponse = serde_json::from_slice(&v).expect("de");
        assert_eq!(back.access_token, t.access_token);
        assert_eq!(back.refresh_token, t.refresh_token);
    }

    #[test]
    fn auth_manager_new_returns_self() {
        let _m = AuthManager::new("id", "secret", PathBuf::from("/tmp/tok.json"));
    }
}
