//! Deployment configuration, read from the environment.
//!
//! Everything that varies between deployments arrives as an environment
//! variable; nothing about the API being reflected is compiled in. The four
//! groups are:
//!
//! * **The API description** — an OpenAPI document plus an ordered list of
//!   OpenAPI Overlays that complete it (auth profile, pagination schemes,
//!   CRUD causality). `syncables-rs` derives the whole sync flow from these.
//! * **Credentials** — the token sent to that API.
//! * **Constants** — values bound into the document's path/query parameters,
//!   which is what narrows the sync to *one* issue tracker rather than every
//!   tracker the credentials can reach.
//! * **The public URL** — the origin under which this store's data is
//!   published on the web. Needed because the ontology `syncables-rs` mints
//!   has to carry canonical, resolvable subjects.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

use syncables::Credentials;

/// The variable naming each setting, kept in one place so the README, the
/// `.env.example` and the error messages cannot drift apart.
pub mod env_var {
    /// Path to the OpenAPI document describing the API to reflect.
    pub const OPENAPI_DOCUMENT: &str = "OPENAPI_DOCUMENT";
    /// Comma-separated list of OpenAPI Overlay files, applied in order.
    pub const OPENAPI_OVERLAYS: &str = "OPENAPI_OVERLAYS";
    /// Bearer token for the API (a GitHub PAT or installation token here).
    pub const API_TOKEN: &str = "API_TOKEN";
    /// Legacy/convenience alias for [`API_TOKEN`].
    pub const GITHUB_TOKEN: &str = "GITHUB_TOKEN";
    /// `key=value` pairs bound into the document's parameters, comma-separated.
    pub const API_CONSTANTS: &str = "API_CONSTANTS";
    /// Public origin (optionally with a path) this store is served under.
    pub const PUBLIC_URL: &str = "PUBLIC_URL";
    /// Directory the store is exported to as JSON-AD after a sync.
    pub const DATA_DIR: &str = "DATA_DIR";
    /// OAuth client id for the interactive GitHub authorization-code flow,
    /// used when [`API_TOKEN`] is missing or no longer accepted.
    pub const OAUTH_CLIENT_ID: &str = "OAUTH_CLIENT_ID";
    /// Legacy/convenience alias for [`OAUTH_CLIENT_ID`].
    pub const GITHUB_CLIENT_ID: &str = "GITHUB_CLIENT_ID";
    /// OAuth client secret paired with [`OAUTH_CLIENT_ID`].
    pub const OAUTH_CLIENT_SECRET: &str = "OAUTH_CLIENT_SECRET";
    /// Legacy/convenience alias for [`OAUTH_CLIENT_SECRET`].
    pub const GITHUB_CLIENT_SECRET: &str = "GITHUB_CLIENT_SECRET";
    /// `host:port` the local OAuth callback web server binds to.
    pub const OAUTH_REDIRECT_ADDR: &str = "OAUTH_REDIRECT_ADDR";
    /// OAuth scope requested during the authorization-code flow.
    pub const OAUTH_SCOPE: &str = "OAUTH_SCOPE";
}

/// Default `host:port` the local OAuth callback server binds to.
pub const DEFAULT_OAUTH_REDIRECT_ADDR: &str = "127.0.0.1:8901";

/// Default OAuth scope: read access to a user's repositories, matching what
/// the vendored GitHub Issues document needs.
pub const DEFAULT_OAUTH_SCOPE: &str = "repo";

/// Which repository this scaffold points at by default: the issue tracker the
/// first milestone syncs, rather than every tracker the token can read.
pub const DEFAULT_CONSTANTS: &str = "owner=localthought,repo=test-repo-1";

#[derive(Clone, Debug)]
pub struct Config {
    /// The OpenAPI document the sync flow is derived from.
    pub openapi_document: PathBuf,
    /// Overlays applied to that document, in the order given.
    pub openapi_overlays: Vec<PathBuf>,
    /// The credential sent to the API.
    pub credentials: Credentials,
    /// Constants bound into the document's parameters, e.g.
    /// `owner=localthought`, `repo=test-repo-1`. Sorted for stable logging.
    pub constants: BTreeMap<String, String>,
    /// The origin (and optional base path) this store's data is public under,
    /// e.g. `https://my-ontologies.com`. No trailing slash.
    pub public_url: String,
    /// Where a JSON-AD export of the store is written after a sync.
    pub data_dir: PathBuf,
    /// The interactive OAuth fallback, present only when both
    /// [`env_var::OAUTH_CLIENT_ID`] and [`env_var::OAUTH_CLIENT_SECRET`] (or
    /// their `GITHUB_*` aliases) are set.
    pub oauth: Option<OAuthSettings>,
}

/// An OAuth App's credentials, used to run the interactive
/// authorization-code flow (see [`crate::oauth`]) when [`Config::credentials`]
/// is missing or no longer accepted by GitHub.
#[derive(Clone)]
pub struct OAuthSettings {
    /// The OAuth App's client id. Not secret, but grouped with the secret
    /// since the two are only ever configured together.
    pub client_id: String,
    /// The OAuth App's client secret.
    pub client_secret: String,
    /// `host:port` the local callback web server binds to; also the host of
    /// the `redirect_uri` registered with the OAuth App.
    pub redirect_addr: String,
    /// The scope requested from GitHub during the authorization-code flow.
    pub scope: String,
}

impl std::fmt::Debug for OAuthSettings {
    /// Never renders the secret, matching [`Credentials`]'s `Debug` impl —
    /// a host logging its resolved configuration at startup is exactly the
    /// scenario this guards against.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthSettings")
            .field("client_id", &self.client_id)
            .field("client_secret", &"<redacted>")
            .field("redirect_addr", &self.redirect_addr)
            .field("scope", &self.scope)
            .finish()
    }
}

/// Builds the OAuth fallback settings from its four raw parts, or `None` if
/// neither client id nor secret was configured. Kept separate from
/// [`Config::from_env`] so it can be unit-tested without touching the
/// process environment.
fn resolve_oauth_settings(
    client_id: Option<String>,
    client_secret: Option<String>,
    redirect_addr: Option<String>,
    scope: Option<String>,
) -> Result<Option<OAuthSettings>> {
    match (client_id, client_secret) {
        (None, None) => Ok(None),
        (Some(client_id), Some(client_secret)) => Ok(Some(OAuthSettings {
            client_id,
            client_secret,
            redirect_addr: redirect_addr.unwrap_or_else(|| DEFAULT_OAUTH_REDIRECT_ADDR.to_owned()),
            scope: scope.unwrap_or_else(|| DEFAULT_OAUTH_SCOPE.to_owned()),
        })),
        (Some(_), None) => Err(anyhow!(
            "{} is set but {} is not — both are required to enable the OAuth fallback",
            env_var::OAUTH_CLIENT_ID,
            env_var::OAUTH_CLIENT_SECRET
        )),
        (None, Some(_)) => Err(anyhow!(
            "{} is set but {} is not — both are required to enable the OAuth fallback",
            env_var::OAUTH_CLIENT_SECRET,
            env_var::OAUTH_CLIENT_ID
        )),
    }
}

fn var(name: &str) -> Option<String> {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => Some(value),
        _ => None,
    }
}

/// Splits a comma-separated list, trimming and dropping empty entries.
fn split_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Parses `key=value,key=value` into a map. A value may itself contain `=`
/// (only the first one splits), so tokens like `since=2020-01-01T00:00:00Z`
/// survive intact.
fn parse_constants(raw: &str) -> Result<BTreeMap<String, String>> {
    let mut constants = BTreeMap::new();
    for pair in split_list(raw) {
        let (key, value) = pair
            .split_once('=')
            .ok_or_else(|| anyhow!("`{pair}` is not a `key=value` pair"))?;
        let key = key.trim();
        if key.is_empty() {
            return Err(anyhow!("`{pair}` has an empty key"));
        }
        constants.insert(key.to_owned(), value.trim().to_owned());
    }
    Ok(constants)
}

impl Config {
    /// Reads the configuration from the process environment, resolving paths
    /// relative to `root` (the crate directory, so the vendored `spec/` works
    /// out of the box).
    pub fn from_env(root: &Path) -> Result<Self> {
        let openapi_document = root.join(
            var(env_var::OPENAPI_DOCUMENT)
                .unwrap_or_else(|| "spec/github-issues.openapi.yaml".to_owned()),
        );

        let overlays = var(env_var::OPENAPI_OVERLAYS).unwrap_or_else(|| {
            [
                "spec/overlays/github/auth-overlay.yaml",
                "spec/overlays/github/pagination-overlay.yaml",
                "spec/overlays/github/crud-causality-overlay.yaml",
            ]
            .join(",")
        });
        let openapi_overlays = split_list(&overlays)
            .into_iter()
            .map(|path| root.join(path))
            .collect();

        let credentials = match var(env_var::API_TOKEN).or_else(|| var(env_var::GITHUB_TOKEN)) {
            Some(token) => Credentials::Bearer(token),
            None => Credentials::Anonymous,
        };

        let constants = parse_constants(
            &var(env_var::API_CONSTANTS).unwrap_or_else(|| DEFAULT_CONSTANTS.to_owned()),
        )
        .with_context(|| format!("{} is malformed", env_var::API_CONSTANTS))?;

        // The one setting with no sensible default: an ontology minted under
        // the wrong origin would carry subjects that resolve to somebody
        // else's server, so guessing `localhost` here would be worse than
        // refusing to start.
        let public_url = var(env_var::PUBLIC_URL).ok_or_else(|| {
            anyhow!(
                "{} is required — it is the origin this store's data (and the \
                 minted ontology's class/property URLs) is public under, e.g. \
                 https://my-ontologies.com",
                env_var::PUBLIC_URL
            )
        })?;
        let public_url = normalize_public_url(&public_url)?;

        let data_dir = root.join(var(env_var::DATA_DIR).unwrap_or_else(|| "data".to_owned()));

        let oauth = resolve_oauth_settings(
            var(env_var::OAUTH_CLIENT_ID).or_else(|| var(env_var::GITHUB_CLIENT_ID)),
            var(env_var::OAUTH_CLIENT_SECRET).or_else(|| var(env_var::GITHUB_CLIENT_SECRET)),
            var(env_var::OAUTH_REDIRECT_ADDR),
            var(env_var::OAUTH_SCOPE),
        )?;

        Ok(Config {
            openapi_document,
            openapi_overlays,
            credentials,
            constants,
            public_url,
            data_dir,
            oauth,
        })
    }

    /// Fails early on anything the sync would only discover mid-flight: a
    /// missing document or overlay is a deployment mistake, not a sync error.
    pub fn validate(&self) -> Result<()> {
        if !self.openapi_document.is_file() {
            return Err(anyhow!(
                "OpenAPI document not found: {}",
                self.openapi_document.display()
            ));
        }
        for overlay in &self.openapi_overlays {
            if !overlay.is_file() {
                return Err(anyhow!("overlay not found: {}", overlay.display()));
            }
        }
        if let Some(oauth) = &self.oauth {
            use std::net::ToSocketAddrs;
            oauth.redirect_addr.to_socket_addrs().with_context(|| {
                format!(
                    "{} is not a valid `host:port`: `{}`",
                    env_var::OAUTH_REDIRECT_ADDR,
                    oauth.redirect_addr
                )
            })?;
        }
        Ok(())
    }
}

/// Strips trailing slashes and rejects anything that is not an absolute
/// `http(s)` URL, so `public_url` can be joined with a `/`-prefixed path
/// without producing `https://host//path` or a relative subject.
fn normalize_public_url(raw: &str) -> Result<String> {
    let trimmed = raw.trim().trim_end_matches('/');
    if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
        return Err(anyhow!(
            "{} must be an absolute http(s) URL, got `{raw}`",
            env_var::PUBLIC_URL
        ));
    }
    if trimmed.split("://").nth(1).is_none_or(str::is_empty) {
        return Err(anyhow!("{} has no host: `{raw}`", env_var::PUBLIC_URL));
    }
    Ok(trimmed.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_key_value_constants() {
        let constants = parse_constants("owner=localthought, repo=test-repo-1").unwrap();
        assert_eq!(constants["owner"], "localthought");
        assert_eq!(constants["repo"], "test-repo-1");
    }

    #[test]
    fn constant_values_may_contain_equals_signs() {
        let constants = parse_constants("filter=a=b").unwrap();
        assert_eq!(constants["filter"], "a=b");
    }

    #[test]
    fn rejects_constants_that_are_not_pairs() {
        assert!(parse_constants("owner").is_err());
        assert!(parse_constants("=localthought").is_err());
    }

    #[test]
    fn default_constants_name_the_target_repository() {
        let constants = parse_constants(DEFAULT_CONSTANTS).unwrap();
        assert_eq!(constants["owner"], "localthought");
        assert_eq!(constants["repo"], "test-repo-1");
    }

    #[test]
    fn public_url_loses_its_trailing_slash() {
        assert_eq!(
            normalize_public_url("https://my-ontologies.com/").unwrap(),
            "https://my-ontologies.com"
        );
    }

    #[test]
    fn public_url_must_be_absolute() {
        assert!(normalize_public_url("my-ontologies.com").is_err());
        assert!(normalize_public_url("https://").is_err());
    }

    #[test]
    fn overlay_list_is_ordered_and_ignores_blanks() {
        assert_eq!(split_list("a.yaml, ,b.yaml,"), vec!["a.yaml", "b.yaml"]);
    }

    #[test]
    fn credentials_never_render_the_secret() {
        let rendered = format!("{:?}", Credentials::Bearer("ghp_secret".into()));
        assert!(!rendered.contains("ghp_secret"), "{rendered}");
    }

    #[test]
    fn oauth_settings_absent_when_neither_var_is_set() {
        let oauth = resolve_oauth_settings(None, None, None, None).unwrap();
        assert!(oauth.is_none());
    }

    #[test]
    fn oauth_settings_fill_in_defaults() {
        let oauth = resolve_oauth_settings(
            Some("client-id".into()),
            Some("client-secret".into()),
            None,
            None,
        )
        .unwrap()
        .expect("both id and secret were given");
        assert_eq!(oauth.client_id, "client-id");
        assert_eq!(oauth.client_secret, "client-secret");
        assert_eq!(oauth.redirect_addr, DEFAULT_OAUTH_REDIRECT_ADDR);
        assert_eq!(oauth.scope, DEFAULT_OAUTH_SCOPE);
    }

    #[test]
    fn oauth_settings_honor_overrides() {
        let oauth = resolve_oauth_settings(
            Some("client-id".into()),
            Some("client-secret".into()),
            Some("0.0.0.0:9000".into()),
            Some("repo,read:user".into()),
        )
        .unwrap()
        .unwrap();
        assert_eq!(oauth.redirect_addr, "0.0.0.0:9000");
        assert_eq!(oauth.scope, "repo,read:user");
    }

    #[test]
    fn oauth_settings_reject_a_client_id_without_a_secret() {
        assert!(resolve_oauth_settings(Some("client-id".into()), None, None, None).is_err());
    }

    #[test]
    fn oauth_settings_reject_a_client_secret_without_an_id() {
        assert!(resolve_oauth_settings(None, Some("client-secret".into()), None, None).is_err());
    }

    #[test]
    fn oauth_settings_never_render_the_secret() {
        let oauth = resolve_oauth_settings(
            Some("client-id".into()),
            Some("shh-secret".into()),
            None,
            None,
        )
        .unwrap()
        .unwrap();
        let rendered = format!("{oauth:?}");
        assert!(!rendered.contains("shh-secret"), "{rendered}");
    }
}
