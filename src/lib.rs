//! Reflector (Rust): reflects a REST API described by an OpenAPI document and
//! a set of OpenAPI Overlays into an Atomic Data store.
//!
//! Nothing here is GitHub-specific. The API being reflected, the credential
//! used to reach it, and the constants that narrow *which* of its data to
//! fetch all arrive as environment variables ([`config`]); the sync flow
//! itself is derived from the document by [`syncables-rs`](syncables); and
//! the result — records plus the ontology derived from the document — is
//! written into an Atomic Data [`Storelike`](atomic_lib::Storelike) by
//! [`store::AtomicStorage`].
//!
//! The default configuration points at the vendored GitHub Issues document
//! and syncs the issues and comments of one repository,
//! `localthought/test-repo-1`.

pub mod config;
pub mod http;
pub mod ontology;
pub mod store;

pub use config::Config;
pub use http::ReqwestFetch;
pub use ontology::SubjectMapper;
pub use store::AtomicStorage;
