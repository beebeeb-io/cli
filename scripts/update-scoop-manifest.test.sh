#!/usr/bin/env bash
# Red-first test for scripts/update-scoop-manifest.sh hash parsing (task 1839).
# Feeds the script every .sha256 shape it must accept and asserts the manifest
# hash equals the real hash; a malformed file must be refused.
set -euo pipefail
cd "$(dirname "$0")/.."
# SCRIPT may be overridden to red-prove the test against an older revision.
SCRIPT="${SCRIPT:-$PWD/scripts/update-scoop-manifest.sh}"
REAL_MANIFEST="$PWD/scoop/bb.json"

HASH="c6fcce39f99164792b5b5916e7f9dc58003d0111de9ac55ef143ad9c48b49aaa"
UPPER="$(printf '%s' "$HASH" | tr a-f A-F)"
SCRATCH="$(mktemp -d)"
trap 'rm -rf "$SCRATCH"' EXIT
pass=0

run_case() { # name, file-content, expect: ok|fail
  local name="$1" content="$2" expect="$3"
  # Run in a scratch tree so the script's relative scoop/bb.json is a copy.
  mkdir -p "$SCRATCH/tree/scoop"
  cp "$REAL_MANIFEST" "$SCRATCH/tree/scoop/bb.json"
  printf '%b' "$content" > "$SCRATCH/in.sha256"
  local rc=0
  (cd "$SCRATCH/tree" && bash "$SCRIPT" 9.9.9 "$SCRATCH/in.sha256") >"$SCRATCH/out.log" 2>&1 || rc=$?
  if [[ "$expect" == ok ]]; then
    [[ $rc -eq 0 ]] || { echo "FAIL [$name]: exit $rc"; cat "$SCRATCH/out.log"; exit 1; }
    got="$(jq -r '.architecture["64bit"].hash' "$SCRATCH/tree/scoop/bb.json")"
    [[ "$got" == "sha256:$HASH" ]] || { echo "FAIL [$name]: manifest hash '$got'"; exit 1; }
    [[ "$(jq -r .version "$SCRATCH/tree/scoop/bb.json")" == 9.9.9 ]] || { echo "FAIL [$name]: version not bumped"; exit 1; }
  else
    [[ $rc -ne 0 ]] || { echo "FAIL [$name]: expected refusal, got success"; exit 1; }
    cmp -s "$REAL_MANIFEST" "$SCRATCH/tree/scoop/bb.json" || { echo "FAIL [$name]: manifest modified despite refusal"; exit 1; }
  fi
  pass=$((pass + 1))
  echo "ok   [$name]"
}

run_case "bare hash"               "$HASH\n"                                          ok
run_case "dist '<hash> *<file>'"   "$HASH *beebeeb-cli-x86_64-pc-windows-msvc.zip\n"  ok
run_case "shasum '<hash>  <file>'" "$HASH  beebeeb-cli-x86_64-pc-windows-msvc.zip\n"  ok
run_case "uppercase hash"          "$UPPER *x.zip\n"                                  ok
run_case "garbage refused"         "not-a-hash x.zip\n"                               fail
run_case "short hash refused"      "abc123 *x.zip\n"                                  fail

echo "update-scoop-manifest.test.sh: $pass passed, 0 failed"
