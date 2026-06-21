#!/usr/bin/env bash
# Stage 0: build core + compiler_builtins for x86_64-lythos.
#
# This is the minimum needed to compile any no_std crate for Lythos.
# Run this first before building lythos-libc or lythos-libstd.

set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
SPEC="$REPO/tools/lythos-toolchain/target-specs/x86_64-lythos-sysroot.json"

echo "Stage 0: core + compiler_builtins → x86_64-lythos"

cd "$REPO"
cargo +nightly build \
    --release \
    -Z build-std=core,compiler_builtins \
    -Z build-std-features=compiler-builtins-mem \
    --target "$SPEC" \
    --manifest-path tools/lythos-toolchain/lythos-libc/Cargo.toml

echo "Stage 0 done. Artefacts in target/x86_64-lythos/release/deps/"
