#!/usr/bin/env bash
# Release guard for task 2275 (owner items under collusion), plan T4.5.
#
# T4 is the CLI half of 2275: the CLI must stop resolving owner keys itself and
# take the class, the resolver and the wrap comparison from beebeeb-core. Until
# that lands, a CLI release would ship the old routing, where an owner-class (O)
# item could open through a served envelope - the competing owner key 2275
# closes. This refuses a release from main while that is still true.
#
# The PR that lands T4 removes this file and its release.yml step.
#
# Usage: scripts/check-2275-owner-items.sh [src/owned_keys.rs]
set -euo pipefail
cd "$(dirname "$0")/.."
resolver="${1:-src/owned_keys.rs}"

if [ ! -f "$resolver" ]; then
  echo "check-2275-owner-items: $resolver is missing" >&2
  exit 1
fi

# T4.1: owner keys are resolved by core, by id class.
if ! grep -q 'owned_keys::resolve_owned_key(' "$resolver"; then
  echo "check-2275-owner-items: REFUSING to release: the CLI does not resolve owned keys through beebeeb_core::owned_keys::resolve_owned_key (task 2275 T4 has not landed)." >&2
  exit 1
fi

# The revert of 0b39b6a: no local HKDF/AES owner envelope may come back.
if grep -nE 'Aes256Gcm|Hkdf|hkdf::|aes_gcm' "$resolver" | grep -v 'CipherSuite::V1Aes256Gcm'; then
  echo "check-2275-owner-items: REFUSING to release: the CLI re-implements owner-key crypto locally (task 2275 T4 forbids it)." >&2
  exit 1
fi

echo "check-2275-owner-items: ok (task 2275 T4 landed)"
