//! GitHub OAuth (device flow) — the same mechanism the other Fond apps use (ported from
//! Zerkalo). Blocking HTTP on the calling thread; run the polling on a worker thread.
//!
//! Unlike Zerkalo (which shells out to `git` with the token as a header), Kartoteka pushes
//! **in-process via libgit2** with the token as HTTPS credentials — see
//! `fond_vault::Vault::push_github`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::Deserialize;
use thiserror::Error;

/// Client ID for Kartoteka's GitHub OAuth App (Device Flow enabled). Client IDs are not
/// secret. **This is a placeholder** — register a "Kartoteka" GitHub OAuth App with device
/// flow enabled and paste its client id here. (You may temporarily reuse another Fond app's
/// client id for testing, but the GitHub approval screen will then show that app's name.)
pub const CLIENT_ID: &str = "REPLACE_WITH_KARTOTEKA_OAUTH_APP_CLIENT_ID";

const USER_AGENT: &str = "Kartoteka (https://github.com/calstfrancis/kartoteka)";

pub fn is_configured() -> bool {
    !CLIENT_ID.starts_with("REPLACE_WITH")
}

#[derive(Debug, Error)]
pub enum GithubAuthError {
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("sign-in was cancelled or denied")]
    AccessDenied,
    #[error("sign-in was cancelled")]
    Cancelled,
    #[error("the sign-in code expired before it was approved; try again")]
    ExpiredToken,
    #[error("GitHub error: {0}")]
    Api(String),
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Debug, Deserialize)]
struct AccessTokenResponse {
    access_token: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GithubUser {
    login: String,
}

#[derive(Debug, Deserialize)]
struct CreatedRepo {
    clone_url: String,
}

fn client() -> Result<reqwest::blocking::Client, GithubAuthError> {
    Ok(reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .build()?)
}

/// Start the device flow: request a user code + verification URL to display, and a device
/// code used to poll for approval.
pub fn request_device_code(client_id: &str) -> Result<DeviceCodeResponse, GithubAuthError> {
    let resp = client()?
        .post("https://github.com/login/device/code")
        .header("Accept", "application/json")
        .form(&[("client_id", client_id), ("scope", "repo")])
        .send()?
        .error_for_status()?;
    Ok(resp.json()?)
}

/// Poll GitHub until the user approves (or denies/expires) the device code, or `cancelled`
/// is set. Run on a background thread; checks `cancelled` every second.
pub fn poll_for_access_token(
    client_id: &str,
    device: &DeviceCodeResponse,
    cancelled: &AtomicBool,
) -> Result<String, GithubAuthError> {
    let http = client()?;
    let mut interval = Duration::from_secs(device.interval.max(1));
    let deadline = Instant::now() + Duration::from_secs(device.expires_in);

    loop {
        let mut slept = Duration::ZERO;
        while slept < interval {
            if cancelled.load(Ordering::Relaxed) {
                return Err(GithubAuthError::Cancelled);
            }
            let step = Duration::from_secs(1).min(interval - slept);
            std::thread::sleep(step);
            slept += step;
        }
        if cancelled.load(Ordering::Relaxed) {
            return Err(GithubAuthError::Cancelled);
        }
        if Instant::now() > deadline {
            return Err(GithubAuthError::ExpiredToken);
        }

        let resp: AccessTokenResponse = http
            .post("https://github.com/login/oauth/access_token")
            .header("Accept", "application/json")
            .form(&[
                ("client_id", client_id),
                ("device_code", &device.device_code),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .send()?
            .error_for_status()?
            .json()?;

        if let Some(token) = resp.access_token {
            return Ok(token);
        }
        match resp.error.as_deref() {
            Some("authorization_pending") => continue,
            Some("slow_down") => {
                interval += Duration::from_secs(5);
                continue;
            }
            Some("expired_token") => return Err(GithubAuthError::ExpiredToken),
            Some("access_denied") => return Err(GithubAuthError::AccessDenied),
            Some(other) => return Err(GithubAuthError::Api(other.to_string())),
            None => return Err(GithubAuthError::Api("unknown response".to_string())),
        }
    }
}

/// The login name of the authenticated user.
pub fn fetch_username(token: &str) -> Result<String, GithubAuthError> {
    let user: GithubUser = client()?
        .get("https://api.github.com/user")
        .bearer_auth(token)
        .send()?
        .error_for_status()?
        .json()?;
    Ok(user.login)
}

/// Create a repo under the authenticated user and return its HTTPS clone URL.
pub fn create_repo(token: &str, name: &str, private: bool) -> Result<String, GithubAuthError> {
    let resp = client()?
        .post("https://api.github.com/user/repos")
        .bearer_auth(token)
        .json(&serde_json::json!({ "name": name, "private": private }))
        .send()?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        return Err(GithubAuthError::Api(format!("{status}: {body}")));
    }
    let repo: CreatedRepo = resp.json()?;
    Ok(repo.clone_url)
}
