//! Wires the configuration, the Atomic Data store and the sync engine
//! together, and runs one sync.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use atomic_lib::Db;
use reflector_rs::config::Config;
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
        document = %config.openapi_document.display(),
        overlays = config.openapi_overlays.len(),
        public_url = %config.public_url,
        constants = ?config.constants,
        credentials = ?config.credentials,
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

    let client = SyncClient::new(
        ClientConfig {
            document: config.openapi_document.clone(),
            overlays: config.openapi_overlays.clone(),
            credentials: config.credentials.clone(),
            constants: config.constants.clone(),
            // The ontology derived from the document is minted under the same
            // origin this store is published on, so a class URL a consumer
            // reads out of the data actually resolves.
            ontology_base_url: config.public_url.clone(),
        },
        Arc::new(ReqwestFetch::new()),
    )
    .map_err(|error| anyhow::anyhow!("{error}"))?;

    match client.sync(&storage).await {
        Ok(report) => {
            info!(?report, "sync finished");

            Ok(())
        }
        Err(SyncError::NotImplemented(what)) => {
            warn!(
                "{what}\n\
                 Local-first writes (create/update/remove) are the only \
                 part of the sync engine not implemented yet — see \
                 https://github.com/localthought/syncables-rs/issues/9."
            );
            std::process::exit(1);
        }
        Err(error) => Err(anyhow::anyhow!("{error}")).context("sync failed"),
    }
}
