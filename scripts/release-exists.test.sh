#!/usr/bin/env bash
# Tests scripts/release-exists.sh against a stub `gh` (task 1839): the no-op
# guard in release.yml must skip ONLY when a release with assets exists, and must
# fail closed (not proceed) on an unknown error.
set -euo pipefail
cd "$(dirname "$0")/.."
SCRIPT="${SCRIPT:-$PWD/scripts/release-exists.sh}"
SCRATCH="$(mktemp -d)"
trap 'rm -rf "$SCRATCH"' EXIT
mkdir -p "$SCRATCH/bin"
cat > "$SCRATCH/bin/gh" <<'STUB'
#!/usr/bin/env bash
# stub: behaviour chosen by $GH_STUB_CASE
case "$GH_STUB_CASE" in
  assets)  echo '{"tag_name":"v1.2.3","assets":[{"name":"a"},{"name":"b"}]}' ;;
  empty)   echo '{"tag_name":"v1.2.3","assets":[]}' ;;
  notfound) echo "gh: Not Found (HTTP 404)" >&2; exit 1 ;;
  server)  echo "gh: Internal Server Error (HTTP 500)" >&2; exit 1 ;;
  network) echo "error connecting to api.github.com" >&2; exit 1 ;;
esac
STUB
chmod +x "$SCRATCH/bin/gh"

pass=0
check() { # case expected-stdout expected-rc
  local c="$1" want="$2" want_rc="$3" got rc=0
  got="$(PATH="$SCRATCH/bin:$PATH" GH_STUB_CASE="$c" bash "$SCRIPT" o/r v1.2.3 2>/dev/null)" || rc=$?
  if [[ "$rc" -ne "$want_rc" || "$got" != "$want" ]]; then
    echo "FAIL [$c]: stdout='$got' rc=$rc, wanted stdout='$want' rc=$want_rc"; exit 1
  fi
  pass=$((pass + 1)); echo "ok   [$c] -> '${got:-<none>}' rc=$rc"
}
check assets   "skip=true"  0
check empty    "skip=false" 0
check notfound "skip=false" 0
check server   ""           1
check network  ""           1
echo "release-exists.test.sh: $pass passed, 0 failed"
