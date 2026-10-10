#!/usr/bin/env bash
# Local release build + assertions (task 1839).
#
# GitHub Actions minutes can run out; 0.13.0, 0.13.1 and 0.13.2 were cut from a
# Mac. Each time the procedure was reconstructed from evidence files, and 0.13.1
# shipped a dist-manifest.json with NO `checksums.sha256` per artifact, so
# src/update.rs failed closed ("refusing to update") and OTA silently died for
# every install. This script is the committed procedure, and its assertions are
# what stops that class of mistake: a release that fails them is not publishable.
#
# Usage:
#   scripts/release-local.sh <version>                    build everything, finalize, assert
#   scripts/release-local.sh <version> --skip-build       finalize + assert what is already in target/distrib
#   scripts/release-local.sh <version> --assert-only DIR  assertions only, against a directory of assets
#                                                         (e.g. `gh release download vX -D DIR`)
#
# <version> is bare semver (no leading 'v'). Never publishes, tags, or pushes:
# it prints the upload list and stops. Publishing is a separate, deliberate step
# (see RELEASING.md "Local release").
#
# Env:
#   DIST_DIR      where dist writes archives (default: target/distrib)
#   LOG_DIR       per-step dist logs (default: target/release-local-logs)
#   SCOOP_JSON    scoop manifest to update/check (default: scoop/bb.json)
#   LLVM_BIN      llvm@21 bin dir for the Windows build (default: /opt/homebrew/opt/llvm@21/bin)
#   TARGETS       space-separated local targets (default: the 5 in dist-workspace.toml)
#
# Bash 3.2 compatible (stock macOS).

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

VERSION="${1:-}"
[[ -n "$VERSION" ]] || { sed -n '2,/^set -euo/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//' >&2; exit 2; }
VERSION="${VERSION#v}"
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "FAIL: version must be bare semver, got '$VERSION'" >&2; exit 2; }
shift

MODE=full
ASSERT_DIR=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --skip-build) MODE=skip-build ;;
    --assert-only) MODE=assert-only; ASSERT_DIR="${2:?--assert-only needs a directory}"; shift ;;
    *) echo "FAIL: unknown argument '$1'" >&2; exit 2 ;;
  esac
  shift
done

TAG="v$VERSION"
DIST_DIR="${DIST_DIR:-$ROOT/target/distrib}"
LOG_DIR="${LOG_DIR:-$ROOT/target/release-local-logs}"
SCOOP_JSON="${SCOOP_JSON:-$ROOT/scoop/bb.json}"
LLVM_BIN="${LLVM_BIN:-/opt/homebrew/opt/llvm@21/bin}"
TARGETS="${TARGETS:-aarch64-apple-darwin x86_64-apple-darwin x86_64-unknown-linux-musl aarch64-unknown-linux-musl x86_64-pc-windows-msvc}"
export LLVM_BIN

ARCHIVE_TARGETS="aarch64-apple-darwin x86_64-apple-darwin x86_64-unknown-linux-musl aarch64-unknown-linux-musl x86_64-pc-windows-msvc"
archive_name() { # target -> file name
  case "$1" in
    *windows*) echo "beebeeb-cli-$1.zip" ;;
    *) echo "beebeeb-cli-$1.tar.xz" ;;
  esac
}

sha_of() { shasum -a 256 "$1" | awk '{print $1}'; }

# ---------------------------------------------------------------------------
# Assertions
# ---------------------------------------------------------------------------

FAILS=0
pass() { echo "PASS  $*"; }
fail() { echo "FAIL  $*" >&2; FAILS=$((FAILS + 1)); }

# The 16 assets a release carries (dist 0.31 with shell + homebrew installers).
expected_assets() {
  echo "bb.rb"
  echo "beebeeb-cli-installer.sh"
  echo "dist-manifest.json"
  echo "sha256.sum"
  echo "source.tar.gz"
  echo "source.tar.gz.sha256"
  local t a
  for t in $ARCHIVE_TARGETS; do
    a="$(archive_name "$t")"
    echo "$a"
    echo "$a.sha256"
  done
}

assert_release() { # dir
  local dir="$1" a n

  # 1. exactly the 16 expected assets, nothing unexpected.
  local present=0 missing=0 expected_list
  expected_list="$(expected_assets)"
  while IFS= read -r a; do
    if [[ -s "$dir/$a" ]]; then present=$((present + 1)); else missing=$((missing + 1)); fail "asset missing or empty: $a"; fi
  done < <(expected_assets)
  local extra=0
  while IFS= read -r a; do
    case "$a" in
      *-dist-manifest.json) continue ;;  # per-target build manifests stay local, never uploaded
    esac
    # (no `expected_assets | grep -q`: under pipefail grep's early exit SIGPIPEs the producer)
    if ! grep -qxF "$a" <<<"$expected_list"; then extra=$((extra + 1)); fail "unexpected asset: $a"; fi
  done < <(cd "$dir" && find . -maxdepth 1 -type f ! -name '.*' | sed 's|^\./||' | sort)
  if [[ $present -eq 16 && $extra -eq 0 ]]; then pass "assets: 16 of 16 expected present, 0 unexpected"; fi

  local manifest="$dir/dist-manifest.json"
  [[ -s "$manifest" ]] || { fail "dist-manifest.json absent: cannot run manifest assertions"; return; }
  jq -e . "$manifest" >/dev/null 2>&1 || { fail "dist-manifest.json is not valid JSON"; return; }

  # 2. manifest is for this tag and carries no build-host paths.
  local tag
  tag="$(jq -r '.announcement_tag // empty' "$manifest")"
  if [[ "$tag" == "$TAG" ]]; then pass "manifest announcement_tag = $TAG"; else fail "manifest announcement_tag '$tag' != $TAG"; fi
  n="$(grep -c -E '"path": *"(/Users/|/home/|/private/)' "$manifest" || true)"
  if [[ "$n" -eq 0 ]]; then pass "manifest has 0 local build paths"; else fail "manifest leaks $n local build paths (regenerate with --no-local-paths)"; fi

  # 3. THE assertion that 0.13.1 failed: every archive (and the source tarball)
  #    carries checksums.sha256 in the manifest, equal to the file's real hash
  #    AND to what its own .sha256 file says. src/update.rs refuses to update
  #    without it.
  local checked=0 total=0 art file_hash man_hash sidecar_hash sidecar_name
  for art in $(for t in $ARCHIVE_TARGETS; do archive_name "$t"; done) source.tar.gz; do
    total=$((total + 1))
    [[ -s "$dir/$art" ]] || { fail "manifest check: $art missing"; continue; }
    file_hash="$(sha_of "$dir/$art")"
    man_hash="$(jq -r --arg a "$art" '.artifacts[$a].checksums.sha256 // empty' "$manifest")"
    if [[ -z "$man_hash" ]]; then fail "manifest: $art has NO checksums.sha256 (OTA would refuse to update)"; continue; fi
    if [[ "$man_hash" != "$file_hash" ]]; then fail "manifest: $art checksums.sha256 $man_hash != file $file_hash"; continue; fi
    [[ -s "$dir/$art.sha256" ]] || { fail "sidecar: $art.sha256 missing"; continue; }
    sidecar_hash="$(head -n 1 "$dir/$art.sha256" | awk '{print $1}')"
    sidecar_name="$(head -n 1 "$dir/$art.sha256" | awk '{print $2}')"
    sidecar_name="${sidecar_name#\*}"
    if [[ "$sidecar_hash" != "$file_hash" || "$sidecar_name" != "$art" ]]; then
      fail "sidecar: $art.sha256 says '$sidecar_hash $sidecar_name', file is $file_hash"; continue
    fi
    checked=$((checked + 1))
  done
  if [[ $checked -eq $total ]]; then pass "manifest checksums.sha256 == file == .sha256 sidecar: $checked of $total artifacts"; fi

  # sha256.sum (unified checksum) lists source.tar.gz.
  local sum_hash
  sum_hash=""
  [[ -s "$dir/sha256.sum" ]] && sum_hash="$(awk '/source\.tar\.gz$/ {print $1}' "$dir/sha256.sum" | head -n 1)"
  if [[ -n "$sum_hash" && "$sum_hash" == "$(sha_of "$dir/source.tar.gz")" ]]; then pass "sha256.sum source.tar.gz hash == file"; else fail "sha256.sum does not match source.tar.gz"; fi

  # 4. bb.rb: version, and a sha256 line under every url, equal to the file.
  local rb="$dir/bb.rb" rb_ok=0 rb_total=0 name rbhash
  if [[ ! -s "$rb" ]]; then
    fail "bb.rb missing"
  else
    if grep -q "version \"$VERSION\"" "$rb"; then pass "bb.rb version = $VERSION"; else fail "bb.rb version is not $VERSION"; fi
    while read -r name rbhash; do
      rb_total=$((rb_total + 1))
      if [[ "$rbhash" == MISSING ]]; then fail "bb.rb: no sha256 line under url for $name"; continue; fi
      if [[ ! -s "$dir/$name" ]]; then fail "bb.rb references $name which is not in the asset set"; continue; fi
      if [[ "$rbhash" == "$(sha_of "$dir/$name")" ]]; then rb_ok=$((rb_ok + 1)); else fail "bb.rb: $name sha256 $rbhash != file"; fi
    done < <(bb_rb_pairs "$rb")
    if [[ $rb_total -eq 4 && $rb_ok -eq 4 ]]; then pass "bb.rb: 4 of 4 url+sha256 pairs match files"; else [[ $rb_total -eq 4 ]] || fail "bb.rb has $rb_total url lines, expected 4 (mac+linux x arm+intel)"; fi
  fi

  # 5. installer is for this version, and (local builds only) embeds the archive
  #    hashes so `curl | sh` verifies downloads instead of printing "no checksums
  #    to verify". Releases cut before 1839 (0.13.2) lack this: a warning there.
  if grep -q "$VERSION" "$dir/beebeeb-cli-installer.sh"; then pass "installer references $VERSION"; else fail "installer does not mention $VERSION"; fi
  local emb=0 t2
  for t2 in $ARCHIVE_TARGETS; do
    if [[ -s "$dir/$(archive_name "$t2").sha256" ]] && grep -q "$(awk '{print $1}' "$dir/$(archive_name "$t2").sha256")" "$dir/beebeeb-cli-installer.sh"; then emb=$((emb + 1)); fi
  done
  if [[ $emb -ge 4 ]]; then
    pass "installer embeds $emb of 5 archive hashes"
  elif [[ "${INSTALLER_STRICT:-0}" == 1 ]]; then
    fail "installer embeds only $emb of 5 archive hashes (global build ran without the per-target manifests)"
  else
    echo "WARN  installer embeds only $emb of 5 archive hashes (installer prints 'no checksums to verify'; known gap in releases cut before task 1839)"
  fi

  # 6. scoop manifest: version, URL and hash == the Windows zip.
  local zip
  zip="$dir/$(archive_name x86_64-pc-windows-msvc)"
  if [[ -s "$SCOOP_JSON" && -s "$zip" ]]; then
    local sv sh su
    sv="$(jq -r .version "$SCOOP_JSON")"
    sh="$(jq -r '.architecture["64bit"].hash' "$SCOOP_JSON")"
    su="$(jq -r '.architecture["64bit"].url' "$SCOOP_JSON")"
    if [[ "$sv" == "$VERSION" && "$sh" == "sha256:$(sha_of "$zip")" && "$su" == *"/$TAG/"* ]]; then
      pass "scoop manifest: version, url and hash == windows zip"
    else
      fail "scoop manifest ($SCOOP_JSON): version '$sv' hash '$sh' url '$su' vs $VERSION / sha256:$(sha_of "$zip")"
    fi
  else
    fail "scoop manifest or windows zip absent ($SCOOP_JSON)"
  fi
}

# "<archive> <sha256|MISSING>" per `url` line in a dist-generated bb.rb.
bb_rb_pairs() {
  awk '
    /^[[:space:]]*url "/ { n = $0; sub(/^[^"]*"/, "", n); sub(/".*$/, "", n); sub(/.*\//, "", n); pending = n; next }
    pending != "" {
      if ($1 == "sha256") { h = $2; gsub(/"/, "", h); print pending, h } else { print pending, "MISSING" }
      pending = ""
    }
  ' "$1"
}

# ---------------------------------------------------------------------------
# Finalization (what plain `dist` leaves out locally)
# ---------------------------------------------------------------------------

# dist run locally does not emit checksums.sha256 into dist-manifest.json (CI's
# does). Inject it for every artifact that names a `checksum` sidecar, hashing
# the real file; same shape as the CI-built 0.13.0 manifest.
inject_checksums() { # dir
  local dir="$1" art map
  map="$(mktemp)"
  echo '{}' > "$map"
  for art in $(jq -r '.artifacts | to_entries[] | select(.value.checksum != null) | .key' "$dir/dist-manifest.json"); do
    [[ -s "$dir/$art" ]] || { echo "FAIL: manifest lists $art but it is not in $dir" >&2; rm -f "$map"; exit 1; }
    jq --arg a "$art" --arg h "$(sha_of "$dir/$art")" '.[$a] = $h' "$map" > "$map.new" && mv "$map.new" "$map"
  done
  jq --slurpfile m "$map" '
    .artifacts |= with_entries(
      if $m[0][.key] != null then .value.checksums = {sha256: $m[0][.key]} else . end)' \
    "$dir/dist-manifest.json" > "$dir/dist-manifest.json.new"
  mv "$dir/dist-manifest.json.new" "$dir/dist-manifest.json"
  rm -f "$map"
  echo "injected checksums.sha256 for $(jq '[.artifacts[] | select(.checksums.sha256 != null)] | length' "$dir/dist-manifest.json") artifacts"
}

# dist's local bb.rb has `url` lines but no `sha256` lines; Homebrew then
# installs unverified. Insert one under every url, from the real file hash.
patch_bb_rb() { # dir
  local dir="$1" map name
  map="$(mktemp)"
  for name in $(bb_rb_pairs "$dir/bb.rb" | awk '{print $1}'); do
    [[ -s "$dir/$name" ]] || { echo "FAIL: bb.rb references $name, not in $dir" >&2; rm -f "$map"; exit 1; }
    echo "$name $(sha_of "$dir/$name")" >> "$map"
  done
  awk -v mapfile="$map" '
    BEGIN { while ((getline l < mapfile) > 0) { split(l, a, " "); m[a[1]] = a[2] } }
    { line[NR] = $0 }
    END {
      for (i = 1; i <= NR; i++) {
        print line[i]
        if (line[i] ~ /^[[:space:]]*url "/) {
          n = line[i]; sub(/^[^"]*"/, "", n); sub(/".*$/, "", n); sub(/.*\//, "", n)
          if (line[i + 1] !~ /^[[:space:]]*sha256 /) {
            ind = line[i]; sub(/[^[:space:]].*$/, "", ind)
            print ind "sha256 \"" m[n] "\""
          }
        }
      }
    }' "$dir/bb.rb" > "$dir/bb.rb.new"
  mv "$dir/bb.rb.new" "$dir/bb.rb"
  rm -f "$map"
}

# ---------------------------------------------------------------------------
# Build
# ---------------------------------------------------------------------------

preflight() {
  local t
  for t in dist jq shasum cargo cargo-xwin cargo-zigbuild zig; do
    command -v "$t" >/dev/null 2>&1 || { echo "FAIL: '$t' not on PATH" >&2; exit 1; }
  done
  [[ "$(dist --version)" == "cargo-dist 0.31.0" ]] || { echo "FAIL: need cargo-dist 0.31.0, have $(dist --version)" >&2; exit 1; }
  [[ -x "$LLVM_BIN/clang-cl" ]] || { echo "FAIL: $LLVM_BIN/clang-cl not found (brew install llvm@21)" >&2; exit 1; }
  local cargo_ver
  cargo_ver="$(awk -F'"' '/^version *=/ {print $2; exit}' Cargo.toml)"
  [[ "$cargo_ver" == "$VERSION" ]] || { echo "FAIL: Cargo.toml version $cargo_ver != $VERSION" >&2; exit 1; }
  bash scripts/check-release-notes.sh "$VERSION"
  if [[ -n "$(git status --porcelain)" ]]; then
    echo "FAIL: working tree is dirty; release from a clean checkout of the release commit:" >&2
    git status --short >&2
    exit 1
  fi
  echo "release commit: $(git rev-parse HEAD)"
  git fetch --quiet origin main
  [[ "$(git rev-parse HEAD)" == "$(git rev-parse origin/main)" ]] || echo "WARN: HEAD is not origin/main; is this the commit the tag will point at?" >&2
}

build_all() {
  [[ "$DIST_DIR" == "$ROOT/target/distrib" ]] || { echo "FAIL: refusing to clean non-default DIST_DIR $DIST_DIR" >&2; exit 1; }
  rm -rf -- "$DIST_DIR" "$LOG_DIR"
  mkdir -p "$DIST_DIR" "$LOG_DIR"

  local t wrap
  wrap="$(mktemp -d)"
  cp scripts/clang-cl-sse4.sh "$wrap/clang-cl"
  chmod +x "$wrap/clang-cl"

  for t in $TARGETS; do
    echo "== dist build (local) $t"
    if [[ "$t" == *windows* ]]; then
      # llvm@21 + the -msse4.1 clang-cl wrapper, first on PATH (see the wrapper's comment).
      PATH="$wrap:$LLVM_BIN:$PATH" dist build --tag "$TAG" --artifacts=local --target "$t" --output-format=json \
        > "$LOG_DIR/dist-$t.json" 2> "$LOG_DIR/dist-$t.err" || { echo "FAIL: dist build $t (see $LOG_DIR/dist-$t.err)" >&2; exit 1; }
    else
      dist build --tag "$TAG" --artifacts=local --target "$t" --output-format=json \
        > "$LOG_DIR/dist-$t.json" 2> "$LOG_DIR/dist-$t.err" || { echo "FAIL: dist build $t (see $LOG_DIR/dist-$t.err)" >&2; exit 1; }
    fi
    # CI saves each local build's manifest as target/distrib/<target>-dist-manifest.json
    # so the global build can embed archive checksums; do the same.
    cp "$LOG_DIR/dist-$t.json" "$DIST_DIR/$t-dist-manifest.json"
  done
  rm -rf -- "$wrap"

  echo "== dist build (global)"
  dist build --tag "$TAG" --artifacts=global --output-format=json \
    > "$LOG_DIR/dist-global.json" 2> "$LOG_DIR/dist-global.err" || { echo "FAIL: dist build global (see $LOG_DIR/dist-global.err)" >&2; exit 1; }
  cp "$LOG_DIR/dist-global.json" "$DIST_DIR/global-dist-manifest.json"
}

finalize() { # dir
  local dir="$1"
  echo "== dist manifest (all, no local paths)"
  # Write outside $dir first: dist merges every *dist-manifest.json it finds in
  # target/distrib, so a shell redirect straight into $dir/dist-manifest.json
  # hands it an empty file ("failed to parse JSON") -- found by the first real run.
  dist manifest --tag "$TAG" --artifacts=all --no-local-paths --output-format=json > "$LOG_DIR/dist-manifest.json" 2> "$LOG_DIR/dist-manifest.err" \
    || { echo "FAIL: dist manifest (see $LOG_DIR/dist-manifest.err)" >&2; exit 1; }
  cp "$LOG_DIR/dist-manifest.json" "$dir/dist-manifest.json"
  inject_checksums "$dir"
  patch_bb_rb "$dir"
  echo "== scoop manifest"
  (cd "$ROOT" && SCOOP_MANIFEST="$SCOOP_JSON" bash scripts/update-scoop-manifest.sh "$VERSION" "$dir/$(archive_name x86_64-pc-windows-msvc).sha256")
}

# ---------------------------------------------------------------------------

case "$MODE" in
  assert-only)
    [[ -d "$ASSERT_DIR" ]] || { echo "FAIL: $ASSERT_DIR is not a directory" >&2; exit 2; }
    assert_release "$(cd "$ASSERT_DIR" && pwd)"
    ;;
  skip-build)
    mkdir -p "$LOG_DIR"
    finalize "$DIST_DIR"
    assert_release "$DIST_DIR"
    ;;
  full)
    preflight
    build_all
    finalize "$DIST_DIR"
    INSTALLER_STRICT=1 assert_release "$DIST_DIR"
    ;;
esac

if [[ $FAILS -gt 0 ]]; then
  echo "RESULT: $FAILS assertion(s) FAILED -- do NOT publish" >&2
  exit 1
fi
echo "RESULT: all assertions passed"
if [[ "$MODE" != assert-only ]]; then
  echo
  echo "Upload set (draft release, next step is manual):"
  expected_assets | sed "s|^|  $DIST_DIR/|"
fi
