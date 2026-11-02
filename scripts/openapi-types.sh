#!/usr/bin/env bash
# openapi-types.sh — generate the CLI's wire types from the vendored OpenAPI
# document, offline and reproducibly.
#
# Generator: typify 0.8.0, pinned (version chosen 2026-11-02).
#
# Why typify and not progenitor: the server document is OpenAPI 3.1.0
# (`"type": ["string", "null"]` unions). progenitor 0.15.1's
# `validate_openapi_spec_version` accepts only `3.0.x`, and the `openapiv3`
# crate it converts through cannot model 3.1 type arrays, so progenitor
# cannot read this document at all. typify is progenitor's own type
# generator, reads 3.1 schemas directly, and is the piece the CLI needs
# (types only, no client code).
#
# Source: contracts/openapi.json, vendored verbatim from the server's
# `refactor/modular-split` at the commit in contracts/SERVER_REV. The server
# tree is private; this repo is public, so the document is committed and CI
# checks the committed types against it.
#
# No generator crate is added to the CLI's Cargo.toml or Cargo.lock: the
# generator runs from a throw-away cargo project in a temp directory, with
# its own manifest and its own target dir.
#
# Usage:
#   scripts/openapi-types.sh --print   # write the generated Rust to stdout
#   scripts/openapi-types.sh --write   # write src/api/generated.rs
#   scripts/openapi-types.sh --check   # regenerate into a temp path and diff
#
# `--check` prints `openapi-types: N types, 0 differences` when the committed
# file matches the document, and exits non-zero otherwise.
set -euo pipefail

TYPIFY_VERSION="0.8.0"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
document="$repo_root/contracts/openapi.json"
server_rev_file="$repo_root/contracts/SERVER_REV"
out_file="$repo_root/src/api/generated.rs"

# The types the CLI consumes. Each seed is a schema in the server document;
# the generator also pulls in every schema those reference, transitively.
# Chosen from the response shapes src/ parses and branches on (file listings,
# shares, sessions, billing, account, uploads, trash, tokens). Shapes the
# document does not describe (the v2 upload init and the passkey/2FA/session
# device routes) are not here and stay hand-written in src/.
seeds=(
  BulkTrashResponse
  ChunkUploadResponse
  CompleteUploadResponse
  ConflictCheckResponse
  ConfirmationResponse
  ErrorResponse
  FileCountResponse
  FileDetailResponse
  FileListResponse
  FilesListResponse
  InvoicesResponse
  MeResponse
  MessageResponse
  OkResponse
  PlansResponse
  RegionPreferenceWithChoices
  RestoreFileResponse
  SessionListResponse
  ShareListResponse
  ShareResponse
  StorageUsageResponse
  SubscriptionResponse
  ThumbnailUploadResponse
  TokenListResponse
  TrashFileResponse
  UpdateFileResponse
  UsageResponse
)

mode="${1:---check}"
case "$mode" in
  --print | --write | --check) ;;
  *)
    echo "usage: $0 --print|--write|--check" >&2
    exit 2
    ;;
esac

for required in "$document" "$server_rev_file"; do
  if [[ ! -f "$required" ]]; then
    echo "openapi-types: missing $(basename "$required")" >&2
    exit 1
  fi
done

server_rev="$(tr -d '[:space:]' <"$server_rev_file")"

workdir="$(mktemp -d)"
trap 'rm -rf "$workdir"' EXIT

crate="$workdir/generator"
mkdir -p "$crate/src"

cat >"$crate/Cargo.toml" <<EOF
[package]
name = "openapi-types-gen"
version = "0.0.0"
edition = "2021"
publish = false

[dependencies]
schemars = "=0.8.22"
serde_json = "1"
typify = { version = "=$TYPIFY_VERSION", default-features = false }

[workspace]
EOF

cat >"$crate/src/main.rs" <<'EOF'
use std::collections::BTreeSet;

fn main() {
    let mut args = std::env::args().skip(1);
    let document = args.next().expect("usage: openapi-types-gen <document> <Schema>...");
    let seeds: Vec<String> = args.collect();

    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&document).expect("read document"))
            .expect("parse document");
    let all = doc["components"]["schemas"]
        .as_object()
        .expect("document has components.schemas");

    // Each seed plus every schema it references, transitively. Ref names are
    // local pointers into `#/components/schemas/`, which is how typify
    // resolves references too (it keys definitions by the last path segment).
    let mut selected: BTreeSet<String> = BTreeSet::new();
    let mut queue = seeds;
    while let Some(name) = queue.pop() {
        if !selected.insert(name.clone()) {
            continue;
        }
        let schema = all
            .get(&name)
            .unwrap_or_else(|| panic!("no schema named {name} in the document"));
        collect_refs(schema, &mut queue);
    }

    let defs = selected
        .iter()
        .map(|name| {
            let schema: schemars::schema::Schema =
                serde_json::from_value(all[name].clone()).expect("convert schema");
            (name.clone(), schema)
        })
        .collect::<Vec<_>>();

    let settings = typify::TypeSpaceSettings::default();
    let mut space = typify::TypeSpace::new(&settings);
    space.add_ref_types(defs).expect("convert schemas");
    print!("{}", space.to_stream());
}

fn collect_refs(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                if key == "$ref" {
                    if let Some(reference) = value.as_str() {
                        if let Some(name) = reference.strip_prefix("#/components/schemas/") {
                            out.push(name.replace("~1", "/").replace("~0", "~"));
                        }
                    }
                } else {
                    collect_refs(value, out);
                }
            }
        }
        serde_json::Value::Array(items) => items.iter().for_each(|item| collect_refs(item, out)),
        _ => {}
    }
}
EOF

raw="$workdir/generated.raw"

CARGO_TARGET_DIR="$workdir/target" env -u RUSTFLAGS cargo run \
  --quiet \
  --manifest-path "$crate/Cargo.toml" \
  -- "$document" "${seeds[@]}" >"$raw"

# The provenance header is added here, outside the generator, so the pin, the
# date and the source revision are visible in the committed file itself.
header="$workdir/header"
{
  echo "//! Wire types generated from \`contracts/openapi.json\` — do not edit by hand."
  echo "//!"
  echo "//! Generator: typify $TYPIFY_VERSION (pinned; chosen 2026-11-02)."
  echo "//! Source: the server's \`refactor/modular-split\` at $server_rev"
  echo "//! (\`contracts/SERVER_REV\`), vendored verbatim as \`contracts/openapi.json\`."
  echo "//!"
  echo "//! Regenerate with \`scripts/openapi-types.sh --write\`; CI runs"
  echo "//! \`scripts/openapi-types.sh --check\` and fails on any drift."
  echo
} >"$header"

generated="$workdir/generated.rs"
{
  cat "$header"
  rustfmt --edition 2021 --emit stdout --quiet <"$raw" 2>/dev/null || cat "$raw"
} >"$generated"

type_count="$(grep -cE '^pub (struct|enum|type) ' "$generated" || true)"

case "$mode" in
  --print)
    cat "$generated"
    ;;
  --write)
    cp "$generated" "$out_file"
    echo "openapi-types: $type_count types, written src/api/generated.rs"
    ;;
  --check)
    if [[ ! -f "$out_file" ]]; then
      echo "openapi-types: $type_count types, src/api/generated.rs is missing" >&2
      exit 1
    fi
    if diff -u "$out_file" "$generated" >"$workdir/diff"; then
      echo "openapi-types: $type_count types, 0 differences"
    else
      differences="$(grep -cE '^[+-][^+-]' "$workdir/diff" || true)"
      sed -n '3,$p' "$workdir/diff" >&2
      echo "openapi-types: $type_count types, $differences differences" >&2
      exit 1
    fi
    ;;
esac
