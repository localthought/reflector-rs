//! [`syncables::client::client::Fetch`] over a real network connection.
//!
//! The sync engine has no HTTP client of its own — [`SyncClient::new`]
//! (`syncables::sync::client`) takes a `Fetch` implementation as its only
//! extension point for reaching the network. [`ReqwestFetch`] is this host's
//! one implementation, built on the `reqwest` already pulled in by
//! `atomic_lib`.
//!
//! [`SyncClient::new`]: syncables::SyncClient::new

use indexmap::IndexMap;
use syncables::client::client::{Fetch, HttpRequest, HttpResponse};
use syncables::{Error, Result};

/// Identifies reflector to API hosts which reject requests without a user agent.
const USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));

/// Sends requests with a shared [`reqwest::Client`].
#[derive(Clone, Debug, Default)]
pub struct ReqwestFetch {
    client: reqwest::Client,
}

impl ReqwestFetch {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait::async_trait]
impl Fetch for ReqwestFetch {
    async fn fetch(&self, request: HttpRequest) -> Result<HttpResponse> {
        let method = request
            .method
            .parse::<reqwest::Method>()
            .map_err(|error| Error::Http(error.to_string()))?;

        // GitHub's REST API rejects requests without a User-Agent header. Set a
        // host-level default, then allow the declarative request to override it.
        let mut builder = self
            .client
            .request(method, &request.url)
            .header(reqwest::header::USER_AGENT, USER_AGENT);
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        if let Some(body) = request.body {
            builder = builder.body(body);
        }

        let response = builder
            .send()
            .await
            .map_err(|error| Error::Http(error.to_string()))?;

        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.to_string(),
                    value.to_str().unwrap_or_default().to_owned(),
                )
            })
            .collect::<IndexMap<_, _>>();
        let body = response
            .bytes()
            .await
            .map_err(|error| Error::Http(error.to_string()))?
            .to_vec();

        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }
}
