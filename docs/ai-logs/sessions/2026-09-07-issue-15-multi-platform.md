# Issue 15: reflecting more than one platform

Assistant: Claude Code (Claude Sonnet).

## Human request

Work on https://github.com/localthought/reflector-rs/issues/15, update docs
as you go, and merge at will.

The issue asks for reflector-rs to sync many read-only platforms rather than
just GitHub: find each one's OpenAPI document, write the completing overlays
(server/auth/pagination/crud-causality), populate `spec/` with one subfolder
per platform, and support importing several platforms in one run as long as
credentials are configured. It names Google Calendar (already worked on in
`localthought/reflector`) and Clockify as candidates.

## Changes and results

- Restructured `spec/` into one subfolder per platform (`spec/github/`,
  `spec/google-calendar/`), each with its own OpenAPI document and
  `overlays/auth`, `overlays/pagination`, `overlays/crud-causality` files.
- Reworked `src/config.rs` around a `PlatformConfig` per platform and a new
  `PLATFORMS` environment variable: unset, behavior is byte-for-byte what it
  was before (one `github` platform from the historical unprefixed
  variables); set, every named platform reads its own
  `<PLATFORM>_OPENAPI_DOCUMENT`/`_OPENAPI_OVERLAYS`/`_API_TOKEN`/
  `_API_CONSTANTS` variables, falling back to built-in defaults for `github`
  and `google-calendar` and requiring explicit configuration for anything
  else. Added unit tests for both modes and for an unrecognized platform
  name.
- Updated `src/main.rs` to sync every configured platform into the same
  store in one run, applying the GitHub-specific OAuth fallback only to a
  platform actually named `github`, and continuing to the next platform (with
  a non-zero exit at the end) if one fails rather than aborting the run.
- Added a real Google Calendar platform: a narrowed, read-only OpenAPI
  document (calendar-list entries and events, following the "still keep them
  read-only" scope in the issue) plus overlays, derived from the fuller
  document `localthought/reflector` already vendors for the TypeScript
  engine but adapted to what `syncables-rs` actually supports — notably its
  pagination extension only recognizes `pageNumber`/`pageToken`/`nextLink`
  scheme types, not the TypeScript engine's `incrementalSync`, so this
  overlay declares a plain `pageToken` scheme and does full listings.
  Verified against the real API end-to-end (anonymous credentials, so the
  requests themselves were rejected, but the document parsed, the resource
  model derived correctly, and the calendar-list/events collections were
  walked as expected).
- Investigated Clockify per the issue and found the API it publishes
  authenticates via an `x-api-key` header (OpenAPI `apiKey` security scheme),
  which `syncables::Credentials` (in the separate `syncables-rs` repository)
  has no representation for — it only models `Authorization: Bearer`.
  Documented this as a concrete blocker in the README rather than adding
  Clockify without working auth.
- Updated README (a new "Reflecting more than one platform" section, the
  configuration table, the vendored-documents section) and `.env.example` to
  match.
- `cargo fmt`, `cargo test` (46 tests), `cargo clippy --all-targets -D
  warnings`, `cargo build`, and both `wasm32-unknown-unknown` checks all
  pass.

No credentials were used; the Google Calendar smoke test ran with anonymous
credentials against the real API and only exercised document parsing and
request construction, not authenticated reads. Changes received automated
review and tests; no separate human code review is claimed before merge.
