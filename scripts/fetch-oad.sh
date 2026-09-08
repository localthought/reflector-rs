#!/usr/bin/env bash
# Fetches the two OAD documents and their overlays this crate's built-in
# platform defaults point at (src/config.rs's GITHUB_DEFAULTS and
# GOOGLE_CALENDAR_DEFAULTS) into spec/, at the same paths they used to be
# vendored at. They now live in their own repos —
# https://github.com/localthought/openapi-directory (PR #1) and
# https://github.com/localthought/overlays (PR #145) — so spec/ is
# gitignored and populated by this script instead of being committed here.
#
# Run this once before `cargo run`/`cargo run -- import-oad` if you want the
# zero-config default (just `github`, localthought/test-repo-1) to work.
# Pointing OPENAPI_DOCUMENT/OPENAPI_OVERLAYS (or their <PLATFORM>_-prefixed
# equivalents) at a document of your own does not need this script at all.
#
# Pinned by commit so a run is reproducible offline afterwards; bump these
# revisions deliberately when the source documents change.
set -euo pipefail

OPENAPI_DIRECTORY_REV=9c5cfb87b3f8b64e11069373a73e3fc85de0de5e
OVERLAYS_REV=d603d5453e9dc5ec26a1d7a6b9cfaeec934d5f4a

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

fetch() {
  local url="$1" dest="$2"
  mkdir -p "$(dirname "$dest")"
  echo "fetching $dest"
  curl -fsSL "$url" -o "$dest"
}

fetch "https://raw.githubusercontent.com/localthought/openapi-directory/$OPENAPI_DIRECTORY_REV/APIs/github.com/github-issues/1.1.4/openapi.yaml" \
  "$root/spec/github/github-issues.openapi.yaml"
fetch "https://raw.githubusercontent.com/localthought/openapi-directory/$OPENAPI_DIRECTORY_REV/APIs/googleapis.com/google-calendar/v3/openapi.yaml" \
  "$root/spec/google-calendar/google-calendar.openapi.yaml"

for overlay in auth pagination crud-causality; do
  fetch "https://raw.githubusercontent.com/localthought/overlays/$OVERLAYS_REV/github.com/github-issues/1.1.4/$overlay-overlay.yaml" \
    "$root/spec/github/overlays/$overlay-overlay.yaml"
  fetch "https://raw.githubusercontent.com/localthought/overlays/$OVERLAYS_REV/googleapis.com/google-calendar/v3/$overlay-overlay.yaml" \
    "$root/spec/google-calendar/overlays/$overlay-overlay.yaml"
done

echo "spec/ populated from openapi-directory@${OPENAPI_DIRECTORY_REV:0:7} and overlays@${OVERLAYS_REV:0:7}"
