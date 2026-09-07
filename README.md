# reflector-rs

Reflects a REST API — described by an OpenAPI document and a set of OpenAPI
Overlays — into an [Atomic Data](https://atomicdata.dev) store.

This is a **scaffold**. The host side is here and tested: configuration, the
`Storelike` the data lands in, the subject mapping that makes the minted
ontology's URLs resolve, and the wiring that hands all of it to
`client.sync()`. The sync engine it calls,
[`localthought/syncables-rs`](https://github.com/localthought/syncables-rs),
is depended on by git revision (it isn't published to crates.io yet) and its
full read — resource discovery, pagination, ontology derivation — is
implemented and tested; only local-first writes
([issue #9](https://github.com/localthought/syncables-rs/issues/9)) are
still to come. Running the binary today performs a full sync.

Nothing in `src/` is GitHub-specific. The default configuration happens to
point at a vendored GitHub Issues document and syncs the issues and comments
of [`localthought/test-repo-1`](https://github.com/localthought/test-repo-1);
pointing it at a different API is a change of environment variables and
overlay files.

It is the Rust sibling of [`localthought/reflector`](https://github.com/localthought/reflector),
which does the same job in TypeScript against the npm `syncables` package.

## Running it

```sh
cp .env.example .env      # then set PUBLIC_URL and API_TOKEN
cargo test
PUBLIC_URL=https://my-ontologies.com API_TOKEN=ghp_… cargo run
```

Requires a Rust toolchain (2021 edition, 1.89+). The first build fetches
`atomic_lib` from the [ontola/atomic-server](https://github.com/ontola/atomic-server)
repository, pinned to a revision — the crates.io release does not yet have the
async `Storelike` and typed `Subject` this crate is written against.

## Configuration

Everything that varies between deployments is an environment variable. Only
`PUBLIC_URL` has no default.

| Variable | Default | What it is |
| --- | --- | --- |
| `PUBLIC_URL` | *(required)* | The origin this store's data is public under, e.g. `https://my-ontologies.com`. |
| `OPENAPI_DOCUMENT` | `spec/github-issues.openapi.yaml` | The OpenAPI document the sync flow is derived from. |
| `OPENAPI_OVERLAYS` | the three files in `spec/overlays/github/` | Comma-separated OpenAPI Overlay files, applied **in order**. |
| `API_TOKEN` (or `GITHUB_TOKEN`) | *(none — anonymous)* | Bearer token for the API. |
| `API_CONSTANTS` | `owner=localthought,repo=test-repo-1` | `key=value` pairs bound into the document's path/query parameters. |
| `STORE_DIR` | `data/store` | Directory containing the persistent `atomic.redb` database (not the file itself). |
| `DRIVE_OWNER` | *(none)* | Agent subject to grant read/write access when a repository drive is first created. Existing drive permissions are retained. |
| `REFLECTOR_ROOT` | the working directory | What the paths above are resolved against. |
| `OAUTH_CLIENT_ID` (or `GITHUB_CLIENT_ID`) | *(none)* | GitHub OAuth App client id, used to authenticate when `API_TOKEN` is missing or rejected. |
| `OAUTH_CLIENT_SECRET` (or `GITHUB_CLIENT_SECRET`) | *(none)* | GitHub OAuth App client secret, paired with `OAUTH_CLIENT_ID`. |
| `OAUTH_REDIRECT_ADDR` | `127.0.0.1:8901` | `host:port` the local OAuth callback web server binds to. |
| `OAUTH_SCOPE` | `repo` | OAuth scope requested from GitHub during the authorization-code flow. |

### Authenticating with GitHub

By default a request carries whatever `API_TOKEN`/`GITHUB_TOKEN` names — a
personal access or installation token — or no `Authorization` header at all.

Setting `OAUTH_CLIENT_ID` and `OAUTH_CLIENT_SECRET` (both are required
together) — the client id and secret of a
[GitHub OAuth App](https://docs.github.com/en/apps/oauth-apps) whose
"Authorization callback URL" is `http://<OAUTH_REDIRECT_ADDR>/callback` —
turns on an interactive fallback:

1. If `API_TOKEN` is set, it is checked against `GET /user`. If GitHub still
   accepts it, the sync proceeds with that token and nothing else happens.
2. Otherwise (the token is missing, expired, or revoked), reflector-rs starts
   a small local web server and prints its URL. Opening it in a browser and
   clicking through GitHub's consent screen redirects back to `/callback`,
   which exchanges the authorization code for an access token and lets the
   sync proceed with it.

No token is ever written to disk by this crate; a fresh run with no valid
`API_TOKEN` repeats the browser step. This is implemented in
[`src/oauth.rs`](src/oauth.rs) and never runs at all — not even the PAT
validity check — unless both OAuth variables are set, so a deployment that
only ever sets `API_TOKEN` is unaffected.

### Why `PUBLIC_URL` is required

A GitHub token can read every repository its owner can reach, so `API_CONSTANTS`
is what narrows a sync to one tracker. `PUBLIC_URL` answers a different
question: **how the data in the store is public on the web.**

Alongside the issues and comments, `syncables-rs` derives an ontology from the
OpenAPI document — a Class per resource, a Property per field. An ontology is
only useful if the class and property URLs embedded in the stored data resolve,
so the terms cannot be minted under a guessed origin. `PUBLIC_URL` is passed to
the engine as the base its terms are minted under, and the same value is set as
the store's base URL:

```
term path       github-issues/property/title
stored as       internal:/github-issues/property/title
served as       https://my-ontologies.com/github-issues/property/title
```

`internal:` is how Atomic Data addresses a locally-hosted resource that a
server rewrites to `<base>/path` on the way out. Keeping the store free of
absolute URLs means moving the deployment to another origin is a config change
rather than a data migration.

## How it fits together

```
environment variables
      │
      ▼
Config (src/config.rs) ──────────────┐
      │                              │
      │  document, overlays,         │  PUBLIC_URL
      │  credentials, constants      │
      ▼                              ▼
SyncClient (syncables crate) ── ontology_base_url
      │                                    │
      │ client.sync(&storage)              │
      ▼                                    │
AtomicStorage (src/store.rs) ◄─────────────┘
      │   renders JSON records and neutral ontology terms
      │   into Atomic Data resources
      ▼
Storelike (atomic_lib) ── internal:/… subjects, served as <PUBLIC_URL>/…
```

`syncables-rs` never sees Atomic Data: it speaks plain JSON records and hands
the ontology over as a neutral description. Rendering both into an Atomic Data
`Storelike` is this crate's job, which is what keeps `atomic_lib` out of the
engine's dependency tree.

### Where things are stored

| | Subject | Public URL |
| --- | --- | --- |
| Ontology | `internal:/github-issues` | `<PUBLIC_URL>/github-issues` |
| Class | `internal:/github-issues/class/issue` | `<PUBLIC_URL>/github-issues/class/issue` |
| Property | `internal:/github-issues/property/title` | `<PUBLIC_URL>/github-issues/property/title` |
| Record | `internal:/localthought%2Ftest-repo-1/issue/1` | `<PUBLIC_URL>/localthought%2Ftest-repo-1/issue/1` |

Records are `internal:/<namespace>/<resource>/<id>`, each segment escaped so a
namespace containing `/` stays a single segment. Fields are typed with the
ontology's own properties — a field named `title` is stored under the property
the ontology declared with shortname `title` — which is why the engine's
contract has it store the ontology before any record.

The binary opens AtomicServer's `Db` with its **redb** backend. Writes persist
as they happen, including partial progress if a sync fails; there is no final
JSON-AD dump. `STORE_DIR` is resolved against `REFLECTOR_ROOT` when relative,
and defaults to `data/store` (creating `data/store/atomic.redb`). Old
`data/store.json-ad` exports are not imported automatically: run a full sync
to populate the database.

Each namespace gets a separate Drive at
`internal:/reflector-drives/<escaped-namespace>`, named from the ontology and
namespace: for example, **github issues localthought test-repo-1**. Issues,
comments, and their nested resources have `parent` and `drive` pointing to
that drive. The user's main drive is not changed. Ontology terms keep their
shared canonical paths shown above. Repeated syncs reuse the repository drive
and retain its permissions; updating resources builds on their stored Loro
state.

### Sharing AtomicServer's database on macOS

AtomicServer defaults to
`~/Library/Application Support/atomic-data/store/atomic.redb` on macOS.
**Stop AtomicServer before running Reflector against that store**, then restart
it after the sync. redb takes an exclusive file lock: two processes cannot open
the same database simultaneously. This is direct database access, not a live
HTTP sync. Reflector updates the database's atom indexes, but not AtomicServer's
separate full-text search index. Restart AtomicServer with
`--rebuild-indexes search` to make imported content searchable.

```sh
# Stop your local AtomicServer first.
export STORE_DIR="$HOME/Library/Application Support/atomic-data/store"
export PUBLIC_URL="http://localhost:9883"  # match your server's public URL
export API_CONSTANTS="owner=localthought,repo=test-repo-1"
export DRIVE_OWNER="did:ad:agent:YOUR_PUBLIC_KEY"  # your AtomicServer agent subject
# Export API_TOKEN if needed for the source repository.
cargo run
# Restart AtomicServer with --rebuild-indexes search after Reflector exits.
```

Use the **directory**, not the `atomic.redb` filename. Shell expansion of
`$HOME` handles the home directory; a literal `~` inside an environment value
is not expanded by Reflector. The binary reads process environment variables;
it does not load `.env` automatically.

Set `DRIVE_OWNER` before the first sync so your AtomicServer agent can read and
edit the new drive. Without it, no user read/write grants are added (server
administration can still access it). Reflector never makes imported private
issues public by default. To change permissions on an existing drive, use
AtomicServer's sharing controls. Open the drive directly at
`<PUBLIC_URL>/reflector-drives/localthought%2Ftest-repo-1`.

## The vendored document

`spec/github-issues.openapi.yaml` is a narrowed subset of the GitHub REST API
covering issues and issue comments, with three overlays in
`spec/overlays/github/`:

- **auth** — the `http`/`bearer` security scheme.
- **pagination** — GitHub's RFC 8288 `Link` header, declared per list operation
  (auto-detection only inspects query parameters and cannot see a header).
- **crud-causality** — `crudResources`: which collections exist, how an item is
  addressed, and what each write does. Two GitHub-specific wrinkles it records:
  an issue is addressed by its per-repository `number` rather than the global
  `id` in the same payload, and a comment is listed under its parent issue but
  addressed at a non-nested URL.

All four files are copied from `localthought/reflector`, which uses them
against the TypeScript engine.

## WASM compatibility

The **library** (`src/lib.rs`'s `config`, `http`, `ontology`, `store`
modules, and everything `syncables` itself needs) builds for
`wasm32-unknown-unknown` (`cargo build --target wasm32-unknown-unknown
--lib`) — see [issue #19](https://github.com/localthought/reflector-rs/issues/19),
which waited on [`syncables-rs` issue #25](https://github.com/localthought/syncables-rs/issues/25)
landing first. The **binary** (`src/main.rs`) is native-only and always
will be: it opens a real `redb` file, binds a `#[tokio::main]`
multi-thread runtime, and (via `src/oauth.rs`) a local TCP listener for
the interactive OAuth callback server — none of which has a wasm32
equivalent inside a browser sandbox. A browser-hosted client is expected
to use the library pieces with its own transport and storage, the way
`atomic_lib`'s own [`wasm/`](https://github.com/ontola/atomic-server/tree/main/wasm)
crate wraps its core for the same target.

What that took, on top of what `syncables-rs`'s own README documents for
itself:

- **`src/oauth.rs` is `#[cfg(not(target_arch = "wasm32"))]`** (see the
  `#[cfg]` on `pub mod oauth` in `src/lib.rs`), and its `axum`/`rand`/
  `tokio` dependencies move to a `target.'cfg(not(target_arch =
  "wasm32"))'.dependencies` section in `Cargo.toml` accordingly — binding
  a TCP listener has no wasm32 equivalent, and nothing else in the
  library needs any of the three.
- **`atomic_lib`'s `wasm` feature** is added alongside `db-redb` for the
  wasm32 target only (`db-redb`'s own doc comment in that fork's
  Cargo.toml already notes it "works in WASM with InMemoryBackend"; `wasm`
  supplies the JS-backed randomness/time that needs).
- **`getrandom` 0.3, with its `wasm_js` feature**, is added as a direct
  wasm32-only dependency: `ulid` (via `atomic_lib`) pulls in `rand` 0.9 →
  `getrandom` 0.3, which — unlike the `getrandom` 0.4 `uuid` uses via
  `syncables-rs` — needs the feature *and* a `--cfg
  getrandom_backend="wasm_js"` rustc flag, set in `.cargo/config.toml`.
- **`Fetch`/`Storage` impls go `?Send` on wasm32**: `http.rs`'s
  `impl Fetch for ReqwestFetch` and `store.rs`'s `impl Storage for
  AtomicStorage<S>` both use
  `#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]` /
  `#[cfg_attr(not(target_arch = "wasm32"), async_trait)]`, matching the
  same relaxation `syncables::{Fetch, Storage}` and `atomic_lib::Storelike`
  make on their trait definitions — wasm32 is single-threaded, and a
  `reqwest`-over-`fetch()` or `Storelike`-backed future generally isn't
  `Send` there.

CI checks `cargo build`/`clippy --target wasm32-unknown-unknown --lib`
separately from the native `cargo test`/`clippy --all-targets`; the
binary and its tests (`oauth.rs`'s local-server test, `store.rs`'s `redb`
tests) are native-only and aren't expected to build for wasm32.

## Generative AI disclosure

This project follows [NLnet's Generative AI policy](https://nlnet.nl/foundation/policies/generativeAI/),
as its TypeScript sibling does. Commits made with AI assistance carry a
`Claude-Session:` trailer, and each such session has a log under
[`docs/ai-logs/sessions/`](docs/ai-logs/sessions).

## Licence

Apache-2.0, matching [`localthought/reflector`](https://github.com/localthought/reflector).
