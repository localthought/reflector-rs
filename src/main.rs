//! Wires the configuration, the Atomic Data store and the sync engine
//! together, and runs one sync.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use atomic_lib::Db;
use reflector_rs::config::{Config, DEFAULT_PLATFORM};
use reflector_rs::store::AtomicStorage;
use reflector_rs::{ReqwestFetch, SubjectMapper};
use syncables::{ClientConfig, SyncClient, SyncError};
use tracing::{info, warn};

/// Paths in the configuration are resolved against this directory, so the
/// vendored `spec/` works from a `cargo run` with no configuration beyond the
/// required `PUBLIC_URL`.
fn root() -> PathBuf {
    std::env::var("REFLECTOR_ROOT")
        .map(PathBuf::from)
        .or_else(|_| std::env::current_dir())
        .unwrap_or_else(|_| PathBuf::from("."))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            // Loro (the CRDT backing every Atomic Data resource) logs each
            // block it encodes at INFO, which buries this crate's own output.
            // Quiet it by default; `RUST_LOG` still overrides.
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,loro=warn,loro_internal=warn".into()),
        )
        .init();

    let config = Config::from_env(&root())?;
    config.validate()?;
    info!(
        platforms = config.platforms.len(),
        public_url = %config.public_url,
        oauth_configured = config.oauth.is_some(),
        "reflector-rs starting"
    );

    let store = Db::init_redb_file(
        &config.store_dir,
        Some(config.public_url.clone()),
        &config.store_dir.join("uploads"),
    )
    .await
    .with_context(|| format!(
        "opening {}/atomic.redb; stop AtomicServer before sharing its store (redb requires exclusive access)",
        config.store_dir.display()
    ))?;
    let storage = AtomicStorage::new(
        Arc::new(store),
        SubjectMapper::new(config.public_url.clone()),
    )
    .with_drive_owner(config.drive_owner.clone());

    // Each platform gets its own SyncClient (document, overlays, credentials,
    // constants) but writes into the same store: syncing several platforms
    // in one run is a matter of listing them, not of any per-platform code.
    let mut any_platform_failed = false;
    for platform in &config.platforms {
        info!(
            platform = %platform.name,
            document = %platform.openapi_document.display(),
            overlays = platform.openapi_overlays.len(),
            constants = ?platform.constants,
            "syncing platform"
        );

        // Falls back to the interactive GitHub OAuth flow (src/oauth.rs) when
        // the github platform's token is missing or no longer accepted and an
        // OAuth App's client id/secret is configured; otherwise a no-op. Only
        // the github platform gets this treatment — everything else is
        // expected to supply an already-obtained token (see CLAUDE.md on
        // GitHub-specific code being the exception, not the rule).
        let credentials = if platform.name == DEFAULT_PLATFORM {
            reflector_rs::oauth::resolve_credentials(&platform.credentials, config.oauth.as_ref())
                .await
                .with_context(|| {
                    format!("resolving credentials for platform `{}`", platform.name)
                })?
        } else {
            platform.credentials.clone()
        };
        info!(platform = %platform.name, credentials = ?credentials, "credentials resolved");

        let client = match SyncClient::new(
            ClientConfig {
                document: platform.openapi_document.clone(),
                overlays: platform.openapi_overlays.clone(),
                credentials,
                constants: platform.constants.clone(),
                // The ontology derived from each document is minted under the
                // same origin this store is published on, so a class URL a
                // consumer reads out of the data actually resolves.
                ontology_base_url: config.public_url.clone(),
            },
            Arc::new(ReqwestFetch::new()),
        ) {
            Ok(client) => client,
            Err(error) => {
                warn!(platform = %platform.name, %error, "failed to build sync client");
                any_platform_failed = true;
                continue;
            }
        };

        match client.sync(&storage).await {
            Ok(report) => info!(platform = %platform.name, ?report, "sync finished"),
            Err(SyncError::NotImplemented(what)) => {
                warn!(
                    platform = %platform.name,
                    "{what}\n\
                     Local-first writes (create/update/remove) are the only \
                     part of the sync engine not implemented yet — see \
                     https://github.com/localthought/syncables-rs/issues/9."
                );
                any_platform_failed = true;
            }
            Err(error) => {
                warn!(platform = %platform.name, %error, "sync failed");
                any_platform_failed = true;
            }
        }
    }

    if any_platform_failed {
        std::process::exit(1);
    }
    Ok(())
}
