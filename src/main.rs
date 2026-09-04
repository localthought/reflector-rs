//! Wires the configuration, the Atomic Data store and the sync engine
//! together, and runs one sync.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use atomic_lib::{Store, Storelike};
use reflector_rs::config::{env_var, Config};
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

    // The Storelike the reflection lands in. In-memory for now: swapping in a
    // persistent `Db` is a change of this one line, because everything
    // downstream is generic over `Storelike`.
    let store = Store::init().await.context("initialising the store")?;
    // How this store's data is public on the web. Every `internal:/path`
    // subject — the minted ontology's classes and properties included — is
    // served as `<public_url>/path`.
    store.set_base_url(&config.public_url);
    let storage = AtomicStorage::new(
        Arc::new(store),
        SubjectMapper::new(config.public_url.clone()),
    );

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
            export(storage.store(), &config).await?;
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

/// Writes the whole store out as JSON-AD, so a run leaves something
/// inspectable behind even before a server is put in front of the store.
async fn export(store: &Arc<Store>, config: &Config) -> Result<()> {
    std::fs::create_dir_all(&config.data_dir).with_context(|| {
        format!(
            "creating {} (set {} to change it)",
            config.data_dir.display(),
            env_var::DATA_DIR
        )
    })?;
    let path = config.data_dir.join("store.json-ad");
    let exported = store
        .export(false)
        .map_err(|error| anyhow::anyhow!("{error}"))
        .context("exporting the store")?;
    let formatted = pretty_json_ad(&exported)?;
    std::fs::write(&path, formatted).with_context(|| format!("writing {}", path.display()))?;
    info!(path = %path.display(), "exported the store");
    Ok(())
}

/// Formats the valid JSON-AD export for people inspecting the generated file.
fn pretty_json_ad(exported: &str) -> Result<String> {
    let value: serde_json::Value =
        serde_json::from_str(exported).context("parsing exported JSON-AD")?;
    serde_json::to_string_pretty(&value).context("formatting exported JSON-AD")
}

#[cfg(test)]
mod tests {
    use super::pretty_json_ad;

    #[test]
    fn pretty_json_ad_indents_an_export() {
        let formatted = pretty_json_ad(r#"[{"@id":"internal:/issue/1","title":"Readable"}]"#)
            .expect("a JSON-AD export formats");

        assert_eq!(
            formatted,
            "[\n  {\n    \"@id\": \"internal:/issue/1\",\n    \"title\": \"Readable\"\n  }\n]"
        );
    }
}
