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

use crate::syncables::Credentials;

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
}

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

        Ok(Config {
            openapi_document,
            openapi_overlays,
            credentials,
            constants,
            public_url,
            data_dir,
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
}
