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
//!   which is what narrows the sync to *one* issue tracker (or calendar, or
//!   workspace) rather than every one the credentials can reach.
//! * **The public URL** — the origin under which this store's data is
//!   published on the web. Needed because the ontology `syncables-rs` mints
//!   has to carry canonical, resolvable subjects.
//!
//! A deployment reflects one or more **platforms** in a single run — see
//! [`Config::platforms`]. `PLATFORMS` unset means "just `github`, configured
//! with the same unprefixed variables this crate has always read"; setting
//! it opts into one [`PlatformConfig`] per named platform, each configured
//! by its own `<PLATFORM>_`-prefixed variables (see [`platform_env_var`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

use syncables::Credentials;

/// The variable naming each setting, kept in one place so the README, the
/// `.env.example` and the error messages cannot drift apart.
pub mod env_var {
    /// Comma-separated platforms to sync in one run, e.g.
    /// `github,google-calendar`. Defaults to just [`super::DEFAULT_PLATFORM`],
    /// configured from the unprefixed variables below — the single-platform
    /// behavior this crate has always had. Setting this switches every
    /// listed platform (including `github`, if named explicitly) over to
    /// `<PLATFORM>_`-prefixed variables instead; see [`super::platform_env_var`].
    pub const PLATFORMS: &str = "PLATFORMS";
    /// Path to the OpenAPI document describing the API to reflect. Read
    /// unprefixed only when [`PLATFORMS`] is unset.
    pub const OPENAPI_DOCUMENT: &str = "OPENAPI_DOCUMENT";
    /// Comma-separated list of OpenAPI Overlay files, applied in order. Read
    /// unprefixed only when [`PLATFORMS`] is unset.
    pub const OPENAPI_OVERLAYS: &str = "OPENAPI_OVERLAYS";
    /// Bearer token for the API (a GitHub PAT or installation token here).
    /// Read unprefixed only when [`PLATFORMS`] is unset.
    pub const API_TOKEN: &str = "API_TOKEN";
    /// Legacy/convenience alias for [`API_TOKEN`], also only read when
    /// [`PLATFORMS`] is unset.
    pub const GITHUB_TOKEN: &str = "GITHUB_TOKEN";
    /// `key=value` pairs bound into the document's parameters, comma-separated.
    /// Read unprefixed only when [`PLATFORMS`] is unset.
    pub const API_CONSTANTS: &str = "API_CONSTANTS";
    /// Public origin (optionally with a path) this store is served under.
    pub const PUBLIC_URL: &str = "PUBLIC_URL";
    /// Directory containing AtomicServer's atomic.redb database.
    pub const STORE_DIR: &str = "STORE_DIR";
    /// Optional agent granted read/write access to newly created drives.
    pub const DRIVE_OWNER: &str = "DRIVE_OWNER";
    /// OAuth client id for the interactive GitHub authorization-code flow,
    /// used when the `github` platform's token is missing or no longer
    /// accepted. Applies only to the `github` platform — every other
    /// platform is expected to supply an already-obtained token.
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

    /// Suffix combined with a platform's name into `<PLATFORM>_<SUFFIX>` by
    /// [`super::platform_env_var`] — the multi-platform equivalent of
    /// [`OPENAPI_DOCUMENT`].
    pub const PLATFORM_OPENAPI_DOCUMENT_SUFFIX: &str = "OPENAPI_DOCUMENT";
    /// The multi-platform equivalent of [`OPENAPI_OVERLAYS`].
    pub const PLATFORM_OPENAPI_OVERLAYS_SUFFIX: &str = "OPENAPI_OVERLAYS";
    /// The multi-platform equivalent of [`API_TOKEN`]. There is no
    /// multi-platform `GITHUB_TOKEN`-style alias — a platform named `github`
    /// in [`PLATFORMS`] uses `GITHUB_API_TOKEN` like every other platform.
    pub const PLATFORM_API_TOKEN_SUFFIX: &str = "API_TOKEN";
    /// The multi-platform equivalent of [`API_CONSTANTS`].
    pub const PLATFORM_API_CONSTANTS_SUFFIX: &str = "API_CONSTANTS";
}

/// Default `host:port` the local OAuth callback server binds to.
pub const DEFAULT_OAUTH_REDIRECT_ADDR: &str = "127.0.0.1:8901";

/// Default OAuth scope: read access to a user's repositories, matching what
/// the vendored GitHub Issues document needs.
pub const DEFAULT_OAUTH_SCOPE: &str = "repo";

/// Which repository this scaffold points at by default: the issue tracker the
/// first milestone syncs, rather than every tracker the token can read.
pub const DEFAULT_CONSTANTS: &str = "owner=localthought,repo=test-repo-1";

/// The platform synced when [`env_var::PLATFORMS`] is unset.
pub const DEFAULT_PLATFORM: &str = "github";

/// Built-in defaults for a platform named in [`env_var::PLATFORMS`], so a
/// deployment that just wants the vendored document doesn't have to spell
/// out every path. A platform not listed here (a deployment's own API) has
/// no defaults — its document, overlays and constants must all be
/// configured explicitly.
struct PlatformDefaults {
    document: &'static str,
    overlays: &'static [&'static str],
    constants: &'static str,
}

const GITHUB_DEFAULTS: PlatformDefaults = PlatformDefaults {
    document: "spec/github/github-issues.openapi.yaml",
    overlays: &[
        "spec/github/overlays/auth-overlay.yaml",
        "spec/github/overlays/pagination-overlay.yaml",
        "spec/github/overlays/crud-causality-overlay.yaml",
    ],
    constants: DEFAULT_CONSTANTS,
};

/// The calendar synced by default: `primary` resolves to whichever calendar
/// belongs to the authenticated account, so no calendar id needs discovering
/// up front.
const GOOGLE_CALENDAR_DEFAULTS: PlatformDefaults = PlatformDefaults {
    document: "spec/google-calendar/google-calendar.openapi.yaml",
    overlays: &[
        "spec/google-calendar/overlays/auth-overlay.yaml",
        "spec/google-calendar/overlays/pagination-overlay.yaml",
        "spec/google-calendar/overlays/crud-causality-overlay.yaml",
    ],
    constants: "calendarId=primary",
};

fn platform_defaults(name: &str) -> Option<&'static PlatformDefaults> {
    match name {
        "github" => Some(&GITHUB_DEFAULTS),
        "google-calendar" => Some(&GOOGLE_CALENDAR_DEFAULTS),
        _ => None,
    }
}

/// Builds the multi-platform variable name for `suffix` on platform `name`:
/// `google-calendar` + `API_TOKEN` → `GOOGLE_CALENDAR_API_TOKEN`. Used only
/// when [`env_var::PLATFORMS`] is set — see the module docs.
pub fn platform_env_var(name: &str, suffix: &str) -> String {
    format!("{}_{suffix}", name.to_ascii_uppercase().replace('-', "_"))
}

#[derive(Clone, Debug)]
pub struct Config {
    /// One entry per platform this run reflects, in the order
    /// [`env_var::PLATFORMS`] names them (or just `github` if unset).
    pub platforms: Vec<PlatformConfig>,
    /// The origin (and optional base path) this store's data is public under,
    /// e.g. `https://my-ontologies.com`. No trailing slash.
    pub public_url: String,
    /// Directory containing atomic.redb.
    pub store_dir: PathBuf,
    /// Optional agent granted read/write access to newly created drives.
    pub drive_owner: Option<String>,
    /// The interactive OAuth fallback, present only when both
    /// [`env_var::OAUTH_CLIENT_ID`] and [`env_var::OAUTH_CLIENT_SECRET`] (or
    /// their `GITHUB_*` aliases) are set. Applies only to the `github`
    /// platform's credentials (see [`crate::oauth`]).
    pub oauth: Option<OAuthSettings>,
}

/// One platform this run reflects: an API description, the credential sent
/// to it, and the constants that narrow the sync to one tracker/calendar/
/// workspace rather than every one the credential can reach.
#[derive(Clone, Debug)]
pub struct PlatformConfig {
    /// The platform's name, e.g. `github` or `google-calendar` — also the
    /// prefix its own environment variables carry in multi-platform mode.
    pub name: String,
    /// The OpenAPI document the sync flow is derived from.
    pub openapi_document: PathBuf,
    /// Overlays applied to that document, in the order given.
    pub openapi_overlays: Vec<PathBuf>,
    /// The credential sent to the API.
    pub credentials: Credentials,
    /// Constants bound into the document's parameters, e.g.
    /// `owner=localthought`, `repo=test-repo-1`. Sorted for stable logging.
    pub constants: BTreeMap<String, String>,
}

/// An OAuth App's credentials, used to run the interactive
/// authorization-code flow (see [`crate::oauth`]) when the `github`
/// platform's credentials are missing or no longer accepted by GitHub.
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

/// Resolves one platform's configuration.
///
/// `prefixed` selects the variable names: `false` reads the historical
/// unprefixed variables (`OPENAPI_DOCUMENT`, `API_TOKEN`/`GITHUB_TOKEN`,
/// `API_CONSTANTS`) — only correct for the sole default platform — `true`
/// reads `<PLATFORM>_`-prefixed ones for `name`, via [`platform_env_var`].
/// Falls back to [`platform_defaults`] for anything left unconfigured,
/// erroring only if a value has neither a configured nor a built-in default.
fn resolve_platform(
    name: &str,
    prefixed: bool,
    root: &Path,
    get: &impl Fn(&str) -> Option<String>,
) -> Result<PlatformConfig> {
    let defaults = platform_defaults(name);
    let lookup = |suffix: &str, legacy: &str| -> Option<String> {
        if prefixed {
            get(&platform_env_var(name, suffix))
        } else {
            get(legacy)
        }
    };
    let var_name = |suffix: &str, legacy: &str| -> String {
        if prefixed {
            platform_env_var(name, suffix)
        } else {
            legacy.to_owned()
        }
    };

    let document = lookup(
        env_var::PLATFORM_OPENAPI_DOCUMENT_SUFFIX,
        env_var::OPENAPI_DOCUMENT,
    )
    .or_else(|| defaults.map(|d| d.document.to_owned()))
    .ok_or_else(|| {
        anyhow!(
            "no OpenAPI document configured for platform `{name}` — set {}",
            var_name(
                env_var::PLATFORM_OPENAPI_DOCUMENT_SUFFIX,
                env_var::OPENAPI_DOCUMENT
            )
        )
    })?;
    let openapi_document = root.join(document);

    let overlays_raw = lookup(
        env_var::PLATFORM_OPENAPI_OVERLAYS_SUFFIX,
        env_var::OPENAPI_OVERLAYS,
    )
    .or_else(|| defaults.map(|d| d.overlays.join(",")))
    .unwrap_or_default();
    let openapi_overlays = split_list(&overlays_raw)
        .into_iter()
        .map(|path| root.join(path))
        .collect();

    let token = if prefixed {
        get(&platform_env_var(name, env_var::PLATFORM_API_TOKEN_SUFFIX))
    } else {
        get(env_var::API_TOKEN).or_else(|| get(env_var::GITHUB_TOKEN))
    };
    let credentials = match token {
        Some(token) => Credentials::Bearer(token),
        None => Credentials::Anonymous,
    };

    let constants_raw = lookup(
        env_var::PLATFORM_API_CONSTANTS_SUFFIX,
        env_var::API_CONSTANTS,
    )
    .or_else(|| defaults.map(|d| d.constants.to_owned()))
    .unwrap_or_default();
    let constants = parse_constants(&constants_raw)
        .with_context(|| format!("constants for platform `{name}` are malformed"))?;

    Ok(PlatformConfig {
        name: name.to_owned(),
        openapi_document,
        openapi_overlays,
        credentials,
        constants,
    })
}

impl Config {
    /// Reads the configuration from the process environment, resolving paths
    /// relative to `root` (the crate directory, so the vendored `spec/` works
    /// out of the box).
    pub fn from_env(root: &Path) -> Result<Self> {
        Self::from_lookup(root, var)
    }

    fn from_lookup(root: &Path, get: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let platforms_setting = get(env_var::PLATFORMS);
        let multi_platform = platforms_setting.is_some();
        let platform_names = platforms_setting
            .map(|raw| split_list(&raw))
            .filter(|names| !names.is_empty())
            .unwrap_or_else(|| vec![DEFAULT_PLATFORM.to_owned()]);

        let platforms = platform_names
            .iter()
            .map(|name| resolve_platform(name, multi_platform, root, &get))
            .collect::<Result<Vec<_>>>()?;

        // The one setting with no sensible default: an ontology minted under
        // the wrong origin would carry subjects that resolve to somebody
        // else's server, so guessing `localhost` here would be worse than
        // refusing to start.
        let public_url = get(env_var::PUBLIC_URL).ok_or_else(|| {
            anyhow!(
                "{} is required — it is the origin this store's data (and the \
                 minted ontology's class/property URLs) is public under, e.g. \
                 https://my-ontologies.com",
                env_var::PUBLIC_URL
            )
        })?;
        let public_url = normalize_public_url(&public_url)?;

        let store_dir =
            root.join(get(env_var::STORE_DIR).unwrap_or_else(|| "data/store".to_owned()));
        let drive_owner = get(env_var::DRIVE_OWNER);

        let oauth = resolve_oauth_settings(
            get(env_var::OAUTH_CLIENT_ID).or_else(|| get(env_var::GITHUB_CLIENT_ID)),
            get(env_var::OAUTH_CLIENT_SECRET).or_else(|| get(env_var::GITHUB_CLIENT_SECRET)),
            get(env_var::OAUTH_REDIRECT_ADDR),
            get(env_var::OAUTH_SCOPE),
        )?;

        Ok(Config {
            platforms,
            public_url,
            store_dir,
            drive_owner,
            oauth,
        })
    }

    /// Identifies the complete dataset this run imports — e.g.
    /// `localthought/test-repo-1` — derived from [`Config::constants`]'
    /// values in key order (`owner` then `repo`, for the default document).
    /// Every record the sync writes, root or nested, is grouped under the
    /// *one* Drive/Document/Table this names — see
    /// [`crate::store::AtomicStorage::with_dataset`] — rather than each
    /// record's own (possibly deeper, per-parent) namespace.
    pub fn dataset_namespace(&self) -> String {
        self.constants
            .values()
            .cloned()
            .collect::<Vec<_>>()
            .join("/")
    }

    /// Fails early on anything the sync would only discover mid-flight: a
    /// missing document or overlay is a deployment mistake, not a sync error.
    pub fn validate(&self) -> Result<()> {
        for platform in &self.platforms {
            if !platform.openapi_document.is_file() {
                return Err(anyhow!(
                    "OpenAPI document not found for platform `{}`: {}",
                    platform.name,
                    platform.openapi_document.display()
                ));
            }
            for overlay in &platform.openapi_overlays {
                if !overlay.is_file() {
                    return Err(anyhow!(
                        "overlay not found for platform `{}`: {}",
                        platform.name,
                        overlay.display()
                    ));
                }
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

    fn lookup(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |key| {
            pairs
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| value.to_string())
        }
    }

    #[test]
    fn store_directory_defaults_and_resolves_relative_to_root() {
        for (configured, expected) in [
            (None, "/reflector/data/store"),
            (Some("custom"), "/reflector/custom"),
            (Some("/shared/store"), "/shared/store"),
        ] {
            let config = Config::from_lookup(Path::new("/reflector"), |key| match key {
                env_var::PUBLIC_URL => Some("http://localhost:9883".into()),
                env_var::STORE_DIR => configured.map(str::to_owned),
                env_var::DRIVE_OWNER => Some("did:ad:agent:owner".into()),
                _ => None,
            })
            .unwrap();
            assert_eq!(config.store_dir, PathBuf::from(expected));
            assert_eq!(config.drive_owner.as_deref(), Some("did:ad:agent:owner"));
        }
    }

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
    fn dataset_namespace_joins_constant_values_in_key_order() {
        let config = Config::from_lookup(Path::new("/reflector"), |key| match key {
            env_var::PUBLIC_URL => Some("http://localhost:9883".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(config.dataset_namespace(), "localthought/test-repo-1");
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

    #[test]
    fn platform_env_var_uppercases_and_joins_with_underscores() {
        assert_eq!(
            platform_env_var("google-calendar", "API_TOKEN"),
            "GOOGLE_CALENDAR_API_TOKEN"
        );
        assert_eq!(platform_env_var("github", "API_TOKEN"), "GITHUB_API_TOKEN");
    }

    #[test]
    fn unset_platforms_yields_one_github_platform_from_unprefixed_vars() {
        let config = Config::from_lookup(
            Path::new("/reflector"),
            lookup(&[
                (env_var::PUBLIC_URL, "http://localhost:9883"),
                (env_var::API_TOKEN, "ghp_secret"),
            ]),
        )
        .unwrap();
        assert_eq!(config.platforms.len(), 1);
        let github = &config.platforms[0];
        assert_eq!(github.name, "github");
        assert_eq!(
            github.openapi_document,
            PathBuf::from("/reflector/spec/github/github-issues.openapi.yaml")
        );
        assert_eq!(github.openapi_overlays.len(), 3);
        assert_eq!(github.constants["owner"], "localthought");
        assert!(matches!(github.credentials, Credentials::Bearer(ref t) if t == "ghp_secret"));
    }

    #[test]
    fn github_token_alias_is_only_honored_when_platforms_is_unset() {
        let config = Config::from_lookup(
            Path::new("/reflector"),
            lookup(&[
                (env_var::PUBLIC_URL, "http://localhost:9883"),
                (env_var::GITHUB_TOKEN, "ghp_legacy"),
            ]),
        )
        .unwrap();
        assert!(
            matches!(config.platforms[0].credentials, Credentials::Bearer(ref t) if t == "ghp_legacy")
        );
    }

    #[test]
    fn platforms_setting_switches_every_named_platform_to_prefixed_vars() {
        let config = Config::from_lookup(
            Path::new("/reflector"),
            lookup(&[
                (env_var::PUBLIC_URL, "http://localhost:9883"),
                (env_var::PLATFORMS, "github, google-calendar"),
                (env_var::API_TOKEN, "unused-legacy-token"),
                ("GITHUB_API_TOKEN", "ghp_prefixed"),
                ("GOOGLE_CALENDAR_API_TOKEN", "ya29_prefixed"),
                (
                    "GOOGLE_CALENDAR_API_CONSTANTS",
                    "calendarId=team@example.com",
                ),
            ]),
        )
        .unwrap();

        assert_eq!(config.platforms.len(), 2);
        let github = &config.platforms[0];
        assert_eq!(github.name, "github");
        assert!(matches!(github.credentials, Credentials::Bearer(ref t) if t == "ghp_prefixed"));

        let calendar = &config.platforms[1];
        assert_eq!(calendar.name, "google-calendar");
        assert_eq!(
            calendar.openapi_document,
            PathBuf::from("/reflector/spec/google-calendar/google-calendar.openapi.yaml")
        );
        assert_eq!(calendar.openapi_overlays.len(), 3);
        assert_eq!(calendar.constants["calendarId"], "team@example.com");
        assert!(matches!(calendar.credentials, Credentials::Bearer(ref t) if t == "ya29_prefixed"));
    }

    #[test]
    fn an_unlisted_platform_defaults_calendarid_to_primary() {
        let config = Config::from_lookup(
            Path::new("/reflector"),
            lookup(&[
                (env_var::PUBLIC_URL, "http://localhost:9883"),
                (env_var::PLATFORMS, "google-calendar"),
            ]),
        )
        .unwrap();
        assert_eq!(config.platforms[0].constants["calendarId"], "primary");
        assert!(matches!(
            config.platforms[0].credentials,
            Credentials::Anonymous
        ));
    }

    #[test]
    fn an_unknown_platform_needs_an_explicit_document() {
        let error = Config::from_lookup(
            Path::new("/reflector"),
            lookup(&[
                (env_var::PUBLIC_URL, "http://localhost:9883"),
                (env_var::PLATFORMS, "clockify"),
            ]),
        )
        .unwrap_err();
        assert!(format!("{error}").contains("CLOCKIFY_OPENAPI_DOCUMENT"));
    }

    #[test]
    fn an_unknown_platform_works_once_fully_configured() {
        let config = Config::from_lookup(
            Path::new("/reflector"),
            lookup(&[
                (env_var::PUBLIC_URL, "http://localhost:9883"),
                (env_var::PLATFORMS, "clockify"),
                (
                    "CLOCKIFY_OPENAPI_DOCUMENT",
                    "spec/clockify/clockify.openapi.yaml",
                ),
                (
                    "CLOCKIFY_OPENAPI_OVERLAYS",
                    "spec/clockify/overlays/auth-overlay.yaml",
                ),
                ("CLOCKIFY_API_CONSTANTS", "workspaceId=abc123"),
            ]),
        )
        .unwrap();
        let clockify = &config.platforms[0];
        assert_eq!(
            clockify.openapi_document,
            PathBuf::from("/reflector/spec/clockify/clockify.openapi.yaml")
        );
        assert_eq!(clockify.openapi_overlays.len(), 1);
        assert_eq!(clockify.constants["workspaceId"], "abc123");
    }
}
