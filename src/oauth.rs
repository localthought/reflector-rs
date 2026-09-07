//! Interactive GitHub OAuth fallback, used when [`Config::credentials`]
//! (`API_TOKEN`/`GITHUB_TOKEN`) is missing or no longer accepted.
//!
//! [`resolve_credentials`] is the entry point: it keeps the configured PAT
//! if GitHub still accepts it, otherwise — if an OAuth App's client
//! id/secret is configured — walks the user through the web-based
//! authorization-code flow and returns the token that produces. Nothing
//! here runs, and no extra network call is made, unless
//! [`OAuthSettings`] is present: a deployment that never configures the
//! OAuth fallback behaves exactly as it did before this module existed.
//!
//! [`Config::credentials`]: crate::config::Config::credentials

use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};
use axum::extract::{Query, State};
use axum::response::Html;
use axum::routing::get;
use axum::Router;
use rand::distr::Alphanumeric;
use rand::Rng;
use serde::Deserialize;
use syncables::Credentials;
use tokio::sync::oneshot;
use tracing::info;

use crate::config::OAuthSettings;
use crate::http::USER_AGENT;

const AUTHORIZE_URL: &str = "https://github.com/login/oauth/authorize";
const TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
/// Used only to check whether a configured PAT still works — the sync
/// engine itself never calls this.
const USER_URL: &str = "https://api.github.com/user";

/// Resolves the credential a sync should run with.
///
/// * `credentials` is whatever `API_TOKEN`/`GITHUB_TOKEN` produced (a
///   [`Credentials::Bearer`] or [`Credentials::Anonymous`]).
/// * `oauth`, if present, is a configured OAuth App to fall back to.
///
/// If no OAuth fallback is configured, `credentials` is returned unchanged
/// and unvalidated — the pre-existing behavior. Otherwise a configured PAT
/// is checked against GitHub first, and the OAuth flow only runs if that
/// check fails or no PAT was configured at all.
pub async fn resolve_credentials(
    credentials: &Credentials,
    oauth: Option<&OAuthSettings>,
) -> Result<Credentials> {
    let Some(oauth) = oauth else {
        return Ok(credentials.clone());
    };

    if let Credentials::Bearer(token) = credentials {
        if pat_is_valid(token).await? {
            return Ok(credentials.clone());
        }
        info!("configured API token was rejected by GitHub; falling back to OAuth");
    }

    Ok(Credentials::Bearer(authorize(oauth).await?))
}

/// `true` if GitHub still accepts `token`, `false` if it was rejected
/// (expired or revoked). A network failure or an unexpected status is
/// propagated instead of being treated as either outcome.
async fn pat_is_valid(token: &str) -> Result<bool> {
    let response = reqwest::Client::new()
        .get(USER_URL)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .bearer_auth(token)
        .send()
        .await
        .context("checking the configured API token with GitHub")?;

    match response.status() {
        status if status.is_success() => Ok(true),
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => Ok(false),
        status => Err(anyhow!(
            "GitHub returned {status} while checking the configured API token"
        )),
    }
}

/// The query parameters GitHub redirects back with on `/callback`.
#[derive(Debug, Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

/// The state shared between the two routes of the callback server: the
/// OAuth App's settings, the CSRF token minted for this run, and the
/// channel the callback handler reports its outcome through.
#[derive(Clone)]
struct AppState {
    settings: OAuthSettings,
    redirect_uri: String,
    csrf_state: String,
    result: Arc<Mutex<Option<oneshot::Sender<Result<String>>>>>,
}

impl AppState {
    fn authorize_url(&self) -> String {
        format!(
            "{AUTHORIZE_URL}?client_id={}&redirect_uri={}&scope={}&state={}",
            urlencoding::encode(&self.settings.client_id),
            urlencoding::encode(&self.redirect_uri),
            urlencoding::encode(&self.settings.scope),
            urlencoding::encode(&self.csrf_state),
        )
    }
}

/// Runs the local web server, prints the authorize URL, and blocks until the
/// browser is walked through GitHub's consent screen and redirected back —
/// then returns the access token that callback exchanged for.
async fn authorize(settings: &OAuthSettings) -> Result<String> {
    let (sender, receiver) = oneshot::channel();
    let state = AppState {
        settings: settings.clone(),
        redirect_uri: format!("http://{}/callback", settings.redirect_addr),
        csrf_state: random_state(),
        result: Arc::new(Mutex::new(Some(sender))),
    };

    let listener = tokio::net::TcpListener::bind(&settings.redirect_addr)
        .await
        .with_context(|| {
            format!(
                "binding the OAuth callback server to {}",
                settings.redirect_addr
            )
        })?;
    let app = Router::new()
        .route("/", get(landing_page))
        .route("/callback", get(callback))
        .with_state(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await });

    let landing_url = format!("http://{}/", settings.redirect_addr);
    println!("No valid API token configured — authorize reflector-rs with GitHub:\n\n    {landing_url}\n");
    info!(url = %landing_url, "waiting for GitHub OAuth authorization");

    let token = match receiver.await {
        Ok(result) => result,
        Err(_) => Err(anyhow!("OAuth callback server stopped without a result")),
    };
    server.abort();
    token
}

async fn landing_page(State(state): State<AppState>) -> Html<String> {
    let url = state.authorize_url();
    Html(format!(
        "<!doctype html><html><head><title>reflector-rs</title></head><body>\
         <h1>reflector-rs</h1>\
         <p><a href=\"{url}\">Sign in with GitHub</a> to authorize reflector-rs \
         to read on your behalf.</p>\
         </body></html>"
    ))
}

async fn callback(
    State(state): State<AppState>,
    Query(query): Query<CallbackQuery>,
) -> Html<String> {
    let result = handle_callback(&state, query).await;
    let page = match &result {
        Ok(_) => "<!doctype html><html><body><h1>Authorized</h1>\
             <p>You can close this tab and return to reflector-rs.</p></body></html>"
            .to_owned(),
        Err(error) => format!(
            "<!doctype html><html><body><h1>Authorization failed</h1><p>{error}</p></body></html>"
        ),
    };
    if let Some(sender) = state.result.lock().unwrap().take() {
        let _ = sender.send(result);
    }
    Html(page)
}

async fn handle_callback(state: &AppState, query: CallbackQuery) -> Result<String> {
    let code = validate_callback(&state.csrf_state, &query)?;
    exchange_code(&state.settings, &state.redirect_uri, code).await
}

/// The part of handling `/callback` that needs no network access: reads
/// GitHub's own denial, checks the CSRF state, and extracts `code`. Split
/// out from [`handle_callback`] so it can be unit-tested without a server.
fn validate_callback<'a>(csrf_state: &str, query: &'a CallbackQuery) -> Result<&'a str> {
    if let Some(error) = &query.error {
        return Err(anyhow!(
            "GitHub denied authorization: {}",
            query.error_description.as_deref().unwrap_or(error)
        ));
    }
    let received_state = query
        .state
        .as_deref()
        .ok_or_else(|| anyhow!("OAuth callback had no `state` parameter"))?;
    if received_state != csrf_state {
        return Err(anyhow!(
            "OAuth callback `state` did not match what was sent — possible CSRF, refusing it"
        ));
    }
    query
        .code
        .as_deref()
        .ok_or_else(|| anyhow!("OAuth callback had no `code` parameter"))
}

/// The response body of GitHub's `POST /login/oauth/access_token`, requested
/// as JSON via `Accept: application/json`.
#[derive(Debug, Deserialize)]
struct AccessTokenResponse {
    access_token: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

async fn exchange_code(settings: &OAuthSettings, redirect_uri: &str, code: &str) -> Result<String> {
    let response: AccessTokenResponse = reqwest::Client::new()
        .post(TOKEN_URL)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .header(reqwest::header::ACCEPT, "application/json")
        .form(&[
            ("client_id", settings.client_id.as_str()),
            ("client_secret", settings.client_secret.as_str()),
            ("code", code),
            ("redirect_uri", redirect_uri),
        ])
        .send()
        .await
        .context("exchanging the OAuth code with GitHub")?
        .json()
        .await
        .context("parsing GitHub's OAuth token response")?;

    if let Some(error) = response.error {
        return Err(anyhow!(
            "GitHub rejected the OAuth code exchange: {}",
            response.error_description.unwrap_or(error)
        ));
    }
    response
        .access_token
        .ok_or_else(|| anyhow!("GitHub's OAuth token response had no `access_token`"))
}

/// A 32-character random string used as the `state` parameter, so the
/// callback can tell a genuine GitHub redirect from a forged request.
fn random_state() -> String {
    rand::rng()
        .sample_iter(&Alphanumeric)
        .take(32)
        .map(char::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(code: Option<&str>, state: Option<&str>, error: Option<&str>) -> CallbackQuery {
        CallbackQuery {
            code: code.map(str::to_owned),
            state: state.map(str::to_owned),
            error: error.map(str::to_owned),
            error_description: None,
        }
    }

    #[test]
    fn accepts_a_matching_callback() {
        let q = query(Some("the-code"), Some("csrf-token"), None);
        let code = validate_callback("csrf-token", &q);
        assert_eq!(code.unwrap(), "the-code");
    }

    #[test]
    fn rejects_a_mismatched_state() {
        assert!(
            validate_callback("csrf-token", &query(Some("the-code"), Some("wrong"), None)).is_err()
        );
    }

    #[test]
    fn rejects_a_missing_state() {
        assert!(validate_callback("csrf-token", &query(Some("the-code"), None, None)).is_err());
    }

    #[test]
    fn rejects_a_missing_code() {
        assert!(validate_callback("csrf-token", &query(None, Some("csrf-token"), None)).is_err());
    }

    #[test]
    fn surfaces_githubs_denial() {
        let error = validate_callback(
            "csrf-token",
            &query(None, Some("csrf-token"), Some("access_denied")),
        )
        .unwrap_err();
        assert!(format!("{error}").contains("access_denied"));
    }

    #[test]
    fn random_state_is_long_and_varies() {
        let a = random_state();
        let b = random_state();
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
    }

    /// Exercises the real local web server end-to-end — binding, serving the
    /// landing page, and rejecting a forged callback — without reaching
    /// github.com: a forged `state` is caught before `exchange_code` (the
    /// only part of this module that calls out to GitHub) ever runs.
    #[tokio::test]
    async fn server_serves_a_landing_page_and_rejects_a_forged_callback() {
        let settings = OAuthSettings {
            client_id: "test-client".into(),
            client_secret: "test-secret".into(),
            redirect_addr: "127.0.0.1:18901".into(),
            scope: "repo".into(),
        };

        let server = tokio::spawn(async move { authorize(&settings).await });

        let client = reqwest::Client::new();
        let landing = get_with_retries(&client, "http://127.0.0.1:18901/").await;
        assert!(landing.status().is_success());
        let body = landing.text().await.unwrap();
        assert!(body.contains("https://github.com/login/oauth/authorize"));
        assert!(body.contains("client_id=test-client"));

        let callback = client
            .get("http://127.0.0.1:18901/callback?code=abc&state=forged")
            .send()
            .await
            .unwrap();
        assert!(callback.status().is_success());
        let body = callback.text().await.unwrap();
        assert!(body.contains("Authorization failed"));

        let result = server.await.unwrap();
        assert!(result.is_err(), "a forged state must not yield a token");
    }

    /// Retries the landing page a few times: the server task needs a moment
    /// to bind after being spawned.
    async fn get_with_retries(client: &reqwest::Client, url: &str) -> reqwest::Response {
        for attempt in 0..50 {
            if let Ok(response) = client.get(url).send().await {
                return response;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20 * (attempt as u64 + 1))).await;
        }
        panic!("server at {url} never came up");
    }
}
