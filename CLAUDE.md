# CLAUDE.md

Guidance for Claude Code (or another AI assistant) working in this repository.

## What this is

`reflector-rs` reflects a REST API — described by an OpenAPI document plus a
set of OpenAPI Overlays — into an [Atomic Data](https://atomicdata.dev)
store. It is a **host**: the sync engine
([`localthought/syncables-rs`](https://github.com/localthought/syncables-rs),
depended on by git revision) derives the whole sync flow from the document;
this crate supplies configuration, an HTTP `Fetch` implementation, and the
`Storelike` the reflected data lands in. See [README.md](README.md) for the
full picture, configuration reference, and how the pieces fit together —
read it before making non-trivial changes.

The default configuration points at a vendored GitHub Issues document and
syncs one repository (`localthought/test-repo-1`), but nothing in `src/` is
supposed to be GitHub-specific *by design* — in practice `src/oauth.rs` and
part of `src/config.rs` do call GitHub's API directly, which is a deliberate,
narrow exception (see below), not a precedent for adding more.

## Commands

```sh
cargo build              # first build fetches atomic_lib and syncables from git — slow
cargo test                # unit + integration tests; no network required except see below
cargo fmt -- --check      # this repo is rustfmt-clean; run `cargo fmt` before committing
cargo clippy --all-targets -- -D warnings   # must be warning-free
cargo run                 # needs PUBLIC_URL at minimum; see .env.example
cargo build --target wasm32-unknown-unknown --lib   # the library only — see below
cargo clippy --target wasm32-unknown-unknown --lib -- -D warnings
```

Run all four native commands (`fmt`, `test`, `clippy`, and a `build`),
plus the two wasm32 checks, before considering a change done. `src/oauth.rs`'s
end-to-end test binds a real local TCP listener (`127.0.0.1:18901`) but
never reaches `github.com`; every other test is pure.

**The library builds for `wasm32-unknown-unknown`; the binary does not and
never will** — see README.md's [WASM compatibility](README.md#wasm-compatibility)
section for the full list of what that took (`src/oauth.rs` gated
`#[cfg(not(target_arch = "wasm32"))]`, `atomic_lib`'s `wasm` feature, a
direct `getrandom` 0.3 dependency plus `.cargo/config.toml`, and `?Send`
on the `Fetch`/`Storage` impls). Keep it that way: a new dependency, a new
`tokio`/`axum` feature, or a new `#[async_trait]` impl in `src/` needs
checking against that target before it lands, and anything that only the
*binary* needs (a real socket, a real file, an interactive prompt) belongs
in `main.rs`, not in a `pub mod` under `src/lib.rs`.

## Module map

| Module | Responsibility |
| --- | --- |
| `src/config.rs` | Reads all deployment configuration from environment variables (see the `env_var` module — the single source of truth for variable names, referenced from README, `.env.example`, and error messages). |
| `src/oauth.rs` | Interactive GitHub OAuth fallback: validates a configured PAT against `GET /user`, and if that fails or no PAT is configured, runs a local web server through the authorization-code flow. Only active when `OAUTH_CLIENT_ID`/`OAUTH_CLIENT_SECRET` are set. `#[cfg(not(target_arch = "wasm32"))]` — binds a real TCP listener, which has no wasm32 equivalent. |
| `src/http.rs` | `ReqwestFetch`, the sync engine's one HTTP extension point (`syncables::client::client::Fetch`). Its impl is `?Send` on wasm32 (see [WASM compatibility](README.md#wasm-compatibility)). |
| `src/ontology.rs` | `SubjectMapper` — the `internal:/…` ⇄ `<PUBLIC_URL>/…` subject mapping that keeps minted ontology terms resolvable. |
| `src/store.rs` | `AtomicStorage` — renders `syncables-rs`'s plain JSON records and neutral ontology description into Atomic Data resources in a `Storelike`. Its `Storage` impl is `?Send` on wasm32, same reason as `http.rs`. |
| `src/main.rs` | Wires configuration, credential resolution, the store, and the sync engine together, then runs one sync and exports it as JSON-AD. |

## Conventions to preserve

- **Env var names live in one place.** Every setting is declared as a
  `pub const` in `config::env_var`, with a doc comment. Add new settings
  there, keep README's configuration table and `.env.example` in sync with
  it, and reference the constant (not a string literal) in error messages.
- **Secrets never render.** Anything holding a credential (`Credentials`,
  `OAuthSettings`) has a hand-written `Debug` impl that redacts it, so
  logging a config or credential at startup is always safe. Preserve this
  for any new field that holds a secret.
- **Pure functions are extracted for testability.** Code that both touches
  the environment/network *and* has interesting logic is split so the logic
  is a pure function tested directly (e.g. `config::resolve_oauth_settings`,
  `oauth::validate_callback`), with the environment- or network-touching
  wrapper thin and largely untested. Follow this pattern rather than adding
  hard-to-test logic inline.
- **Minimal dependency footprint.** This crate deliberately uses few direct
  dependencies; before adding one, check whether it is already resolved
  transitively (`grep '^name = "<crate>"' Cargo.lock`) and prefer that
  version.
- **Doc comments explain *why*, not *what*.** Match the existing style: `//!`
  module docs describe the module's role in the larger flow; `///` doc
  comments on non-obvious fields/functions explain constraints and
  rationale, not restate the signature.
- **GitHub-specific code is the exception, not the rule.** The crate's
  stated design is to reflect *any* REST API; GitHub-specific behavior
  (`src/oauth.rs`'s hardcoded `github.com`/`api.github.com` URLs, the
  default document and constants) is scoped narrowly and gated so it never
  runs — not even an extra network call — for a deployment that doesn't
  opt into it (e.g. `oauth::resolve_credentials` is a no-op unless OAuth
  settings are configured).

## Generative AI disclosure

This project logs AI-assisted sessions under
[`docs/ai-logs/sessions/`](docs/ai-logs/sessions) per
[NLnet's Generative AI policy](https://nlnet.nl/foundation/policies/generativeAI/) —
see [`docs/ai-logs/README.md`](docs/ai-logs/README.md) for the exact
conventions (naming, redaction, human review). Any commit made with AI
assistance must carry a `Claude-Session:` (or equivalent) trailer and be
accompanied by a session log entry in that folder, added in the same PR.
