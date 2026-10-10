#!/bin/sh
# clang-cl wrapper that adds -msse4.1 (task 1839).
#
# Why: the x86_64-pc-windows-msvc build is cross-compiled from macOS via
# cargo-xwin. libwebp-sys compiles C with `clang-cl`, and its SSE4.1 sources need
# the target feature enabled on the command line -- the `cc` crate does not pass
# it for clang-cl, so the build fails (or silently drops the SSE4.1 paths)
# without this. Cut v0.13.2 with exactly this wrapper first on PATH.
#
# Not meant to be run by hand: scripts/release-local.sh copies this file to a
# temp dir as `clang-cl` and prepends that dir to PATH for the Windows target
# only. It forwards to Homebrew's llvm@21 (LLVM_BIN overrides the bin dir).
LLVM_BIN="${LLVM_BIN:-/opt/homebrew/opt/llvm@21/bin}"
exec "$LLVM_BIN/clang-cl" -msse4.1 "$@"
