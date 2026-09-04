# reflector-rs

Reflects a REST API — described by an OpenAPI document and a set of OpenAPI
Overlays — into an [Atomic Data](https://atomicdata.dev) store.

This is a **scaffold**. The host side is here and tested: configuration, the
`Storelike` the data lands in, the subject mapping that makes the minted
ontology's URLs resolve, and the wiring that hands all of it to
`client.sync()`. The sync engine it calls,
[`localthought/syncables-rs`](https://github.com/localthought/syncables-rs),
does not exist yet — the API this crate needs from it is written out as a
contract in [`src/syncables.rs`](src/syncables.rs) and filed as
[issues #1–#9](https://github.com/localthought/syncables-rs/issues) on that
repository. Running the binary today gets as far as `client.sync()` and then
says so.

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

Requires a Rust toolchain (2021 edition, 1.85+). The first build fetches
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
| `DATA_DIR` | `data` | Where the store is exported as JSON-AD after a sync. |
| `REFLECTOR_ROOT` | the working directory | What the paths above are resolved against. |

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
SyncClient (src/syncables.rs) ── ontology_base_url
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

The store is in-memory today; because everything downstream is generic over
`Storelike`, swapping in a persistent `Db` is a one-line change in
`src/main.rs`.

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

## Generative AI disclosure

This project follows [NLnet's Generative AI policy](https://nlnet.nl/foundation/policies/generativeAI/),
as its TypeScript sibling does. Commits made with AI assistance carry a
`Claude-Session:` trailer, and each such session has a log under
[`docs/ai-logs/sessions/`](docs/ai-logs/sessions).

## Licence

Apache-2.0, matching [`localthought/reflector`](https://github.com/localthought/reflector).
