# Issue 18: user-owned drive with a document and issues table

Assistant: Claude Code.

## Human request

Work on https://github.com/localthought/reflector-rs/issues/18, update docs
as the work progresses, and merge at will.

The issue asks for one discoverable Atomic Data drive per imported
repository/dataset (not one per record namespace, which today fragments a
repository's issue comments into a separate drive per issue), registered
through the normal saved-drive mechanism; a DocumentV2 under that drive with
a native AD table of the imported issues (number, title, state, body,
author, updated time, source URL); comments and nested data staying
accessible in that same drive; consistent `parent`/`drive` properties;
reuse/idempotence across reimports and restarts; and a safe transition for
drives created by the earlier per-namespace scheme.

## Investigation

Read `store.rs`'s `AtomicStorage` and `Storage::put`, `config.rs`, and
`main.rs` to see how drives are currently created (keyed off each record's
own `namespace`, which for a nested collection like issue comments is
`owner/repo/<issue_number>` — one drive per issue). Confirmed against
`syncables-rs`'s `sync::client::walk_all` that a nested collection's
namespace is always a deeper extension of its dataset's own root namespace,
and that root-level records are always synced (and `put`) before anything
nested under them.

Cloned `ontola/atomic-server` at the pinned revision to check the actual
resource shapes: `urls::TABLE`'s `classtype` names a row Class (confirmed
against `browser/data-browser/src/chunks/TablePage/createTableFromSpec.ts`
and its AI skill doc `creating-tables.md`), a Table's rows are queried by
`parent == table AND is-a == classtype` (`useTableData.ts`), permission
inheritance climbs the `parent` chain while `drive` is a separate direct
pointer used for fan-out (`lib/src/hierarchy.rs`, `resources.rs::get_drive`),
and the saved-drive list is the Agent's `drives` property (`lib/src/db.rs`'s
private `push_drive_to_list`, generalized here against `Storelike` since
`AtomicStorage` isn't always a `Db`). `DocumentV2`'s real collaborative body
is a loro-prosemirror CRDT tree with no Rust-side construction path in this
codebase or a vendored reference implementation; `documentContent` as an
ordinary Markdown property (the format `extract_document_plain_text`'s own
fallback reads) was used instead of attempting to hand-build that tree.

## Changes and results

- `AtomicStorage` now takes a `dataset` (`with_dataset`, e.g.
  `owner/repo`), computed by a new `Config::dataset_namespace` from
  `API_CONSTANTS`' values in key order — one Drive per dataset, independent
  of any individual record's own (possibly deeper) namespace.
- Added one DocumentV2 and one native AD Table per root-level resource
  (`ensure_document`/`ensure_table`), both idempotent and created lazily on
  the first matching `put`. A root-level record's `parent` is now its
  table (so it renders as a row); nested records (comments) keep `parent`
  pointing at the drive, same drive as the root records. `drive` always
  points at the drive itself either way.
- `ensure_drive` now also registers the drive on `DRIVE_OWNER`'s `drives`
  property when that agent already has a local resource, and removes any
  drive left over from the old per-namespace scheme under this dataset
  (`migrate_legacy_drives`) — the records that pointed at a removed drive
  are re-pointed to the real one automatically the next time they're
  `put`, since `persist` always rewrites `parent`/`drive` from scratch.
- Added `html_url` to the vendored `issue` schema (`spec/`) as the issue's
  source URL, so it flows through the existing ontology derivation as a
  table column with no `syncables-rs` changes.
- Updated/added `store.rs` unit tests for containment (record → table →
  document → drive), nested collections staying off the table, saved-drive
  registration idempotence, and legacy-drive migration; rewrote the
  restart test's "unrelated repository" case as two dataset-scoped
  `AtomicStorage` instances sharing one `Db`, matching how two real syncs
  would actually share an AtomicServer database. Updated `README.md` and
  `CLAUDE.md`.
- `cargo fmt --check`, `cargo test` (43 tests), `cargo clippy --all-targets
  -- -D warnings`, `cargo build`, and both `--target wasm32-unknown-unknown`
  checks all pass locally.

Not covered: this only verifies the resource shapes atomic-server's own
Rust code and TypeScript client documented, not an actual rendered document
open in the data browser — there is no browser or running atomic-server
frontend in this environment to click through. `documentContent` is set as
a Markdown summary rather than the browser's own collaborative-editor
representation, for the reason above; the Table's `classtype` reuses the
existing per-resource ontology Class as-is (so the table also gets
`id`/`state_reason`/`labels` columns beyond the seven the issue names)
rather than hand-building a second, narrower row Class duplicating those
same property subjects. Both are called out in the PR description.
