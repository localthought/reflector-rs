//! The API this binary needs from [`syncables-rs`], mirrored locally.
//!
//! [`syncables-rs`]: https://github.com/localthought/syncables-rs
//!
//! `syncables-rs` is the sync engine: give it an OpenAPI document, the
//! Overlays that complete it, a credential and a set of constants, and it
//! derives the whole flow — resource discovery, pagination, local-first CRUD —
//! and drives it against a [`Storage`] the host provides. It does not exist
//! yet, so this module states the contract reflector-rs is written against.
//! Each item below is tracked as an issue on that repository:
//!
//! * [#1] the crate skeleton and this API surface
//! * [#2] loading the document and applying the overlays
//! * [#3] deriving the resource model from `crudResources`
//! * [#4] pagination, including GitHub's RFC 8288 `Link` header
//! * [#5] credentials
//! * [#6] constants — fetching one issue tracker, not all of them
//! * [#7] the [`Storage`] trait
//! * [#8] the derived ontology and the public URL its terms are minted under
//! * [#9] `client.sync()` itself
//!
//! [#1]: https://github.com/localthought/syncables-rs/issues/1
//! [#2]: https://github.com/localthought/syncables-rs/issues/2
//! [#3]: https://github.com/localthought/syncables-rs/issues/3
//! [#4]: https://github.com/localthought/syncables-rs/issues/4
//! [#5]: https://github.com/localthought/syncables-rs/issues/5
//! [#6]: https://github.com/localthought/syncables-rs/issues/6
//! [#7]: https://github.com/localthought/syncables-rs/issues/7
//! [#8]: https://github.com/localthought/syncables-rs/issues/8
//! [#9]: https://github.com/localthought/syncables-rs/issues/9
//!
//! Two deliberate properties of this boundary:
//!
//! * **`syncables-rs` does not know about Atomic Data.** [`Storage`] speaks
//!   plain JSON records, and the ontology it derives from the document is
//!   handed over as neutral [`OntologyTerm`]s. Rendering those into Atomic
//!   Data `Resource`s is the host's job (see [`crate::store`]), which keeps
//!   `atomic_lib` out of the engine's dependency tree.
//! * **Subjects are minted from a public URL the host supplies.** The engine
//!   never invents an origin; it is told the canonical base its terms will be
//!   served under, so the ontology's class and property URLs resolve.
//!
//! When the crate lands, delete this module and point the `use`s at it — the
//! types are named to match, so nothing else in the crate should change.

use std::collections::BTreeMap;
use std::path::PathBuf;

use async_trait::async_trait;

/// Credentials presented to the API being reflected.
///
/// Only a static bearer token is modelled so far: that is what the GitHub
/// auth overlay's `http`/`bearer` security scheme asks for. An interactive
/// OAuth profile — derived from the document the way the TypeScript Reflector
/// derives Google Calendar's — is tracked separately.
#[derive(Clone, PartialEq, Eq)]
pub enum Credentials {
    /// Sent as `Authorization: Bearer <token>`.
    Bearer(String),
    /// No credential — only useful against a public, unauthenticated API.
    Anonymous,
}

impl std::fmt::Debug for Credentials {
    /// Never renders the secret, so `{:?}` on a config that holds one is safe
    /// to log.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Credentials::Bearer(_) => f.write_str("Bearer(<redacted>)"),
            Credentials::Anonymous => f.write_str("Anonymous"),
        }
    }
}

/// Everything the engine needs to derive and run a sync.
#[derive(Clone, Debug)]
pub struct ClientConfig {
    /// The OpenAPI document describing the API.
    pub document: PathBuf,
    /// Overlays applied to that document, in order.
    pub overlays: Vec<PathBuf>,
    /// The credential sent to the API.
    pub credentials: Credentials,
    /// Values bound into the document's path and query parameters. This is
    /// what narrows a sync to one issue tracker (`owner`/`repo`) instead of
    /// every tracker the credential can reach.
    pub constants: BTreeMap<String, String>,
    /// Canonical base URL the derived ontology's terms are minted under, e.g.
    /// `https://my-ontologies.com`. No trailing slash.
    pub ontology_base_url: String,
}

/// One record read from (or written to) the API.
///
/// `namespace` separates otherwise-identical `resource`/`id` pairs that belong
/// to different parents — every issue's comments share the resource name
/// `issueComment`, so the parent issue is the namespace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub namespace: String,
    pub resource: String,
    pub id: String,
    pub value: serde_json::Map<String, serde_json::Value>,
}

/// Whether a term describes a Class or a Property.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TermKind {
    Class,
    Property,
}

/// One term of the ontology the engine derives from the OpenAPI document.
///
/// Paths are relative to [`ClientConfig::ontology_base_url`] and are the
/// term's identity: `github-issues/property/title` is published as
/// `https://my-ontologies.com/github-issues/property/title`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OntologyTerm {
    /// Path under the ontology base URL, without a leading slash.
    pub path: String,
    pub kind: TermKind,
    /// Atomic Data shortname — lowercase, `-` separated.
    pub shortname: String,
    pub description: String,
    /// For a Property: the Atomic Data datatype URL its values carry.
    /// `None` for a Class.
    pub datatype: Option<String>,
    /// For a Class: the paths (or absolute URLs) of its required properties.
    pub requires: Vec<String>,
    /// For a Class: the paths (or absolute URLs) of its recommended properties.
    pub recommends: Vec<String>,
}

/// The ontology derived from one OpenAPI document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ontology {
    /// Path of the ontology resource itself, e.g. `github-issues`.
    pub path: String,
    pub shortname: String,
    pub description: String,
    pub terms: Vec<OntologyTerm>,
}

/// Where the engine keeps the local-first copy.
///
/// The engine only ever talks to this trait, so the host decides what
/// "storing a record" means. reflector-rs implements it over an Atomic Data
/// [`Storelike`](atomic_lib::Storelike).
#[async_trait]
pub trait Storage: Send + Sync {
    async fn put(&self, record: &Record) -> Result<(), StorageError>;
    async fn get(
        &self,
        namespace: &str,
        resource: &str,
        id: &str,
    ) -> Result<Option<Record>, StorageError>;
    async fn list(&self, namespace: &str, resource: &str) -> Result<Vec<Record>, StorageError>;
    async fn delete(&self, namespace: &str, resource: &str, id: &str) -> Result<(), StorageError>;
    /// Store the ontology derived from the document. Called once per sync,
    /// before any records, so a consumer reading the store never sees an
    /// instance of a class it cannot resolve.
    async fn put_ontology(&self, ontology: &Ontology) -> Result<(), StorageError>;
}

/// Anything a [`Storage`] implementation can fail with. Deliberately opaque:
/// the engine only distinguishes "the host could not store this" from its own
/// transport errors.
#[derive(Debug)]
pub struct StorageError(pub String);

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for StorageError {}

impl From<anyhow::Error> for StorageError {
    fn from(error: anyhow::Error) -> Self {
        StorageError(format!("{error:#}"))
    }
}

/// What one `sync()` did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncReport {
    /// Records read from the API and written to storage, per resource.
    pub read: BTreeMap<String, usize>,
    /// Ontology terms stored.
    pub ontology_terms: usize,
    /// Non-fatal problems: one collection failing does not abandon the rest.
    pub errors: Vec<String>,
}

/// Errors the engine itself raises.
#[derive(Debug)]
pub enum SyncError {
    /// The document or its overlays could not be loaded or reconciled.
    Document(String),
    /// The API rejected or failed a request.
    Transport(String),
    /// The [`Storage`] the host supplied failed.
    Storage(StorageError),
    /// This build of the contract has no engine behind it yet.
    NotImplemented(&'static str),
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncError::Document(message) => write!(f, "OpenAPI document error: {message}"),
            SyncError::Transport(message) => write!(f, "transport error: {message}"),
            SyncError::Storage(error) => write!(f, "storage error: {error}"),
            SyncError::NotImplemented(what) => write!(f, "not implemented yet: {what}"),
        }
    }
}

impl std::error::Error for SyncError {}

/// The sync engine.
///
/// This is the stand-in: it validates and holds the configuration so the host
/// wiring can be written and tested end to end, and [`SyncClient::sync`]
/// reports that the engine behind it is still to be built.
pub struct SyncClient {
    config: ClientConfig,
}

impl SyncClient {
    pub fn new(config: ClientConfig) -> Result<Self, SyncError> {
        if config.ontology_base_url.is_empty() {
            return Err(SyncError::Document(
                "ontology_base_url is required: ontology terms need a canonical, \
                 resolvable base URL"
                    .to_owned(),
            ));
        }
        Ok(SyncClient { config })
    }

    pub fn config(&self) -> &ClientConfig {
        &self.config
    }

    /// Read everything the document describes — narrowed by
    /// [`ClientConfig::constants`] — into `storage`, and store the ontology
    /// derived from the document alongside it.
    pub async fn sync(&self, _storage: &dyn Storage) -> Result<SyncReport, SyncError> {
        Err(SyncError::NotImplemented(
            "syncables-rs has no engine yet — see the tracking issues on \
             https://github.com/localthought/syncables-rs",
        ))
    }
}
