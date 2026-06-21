#!/usr/bin/env bash
# Build the complete Lythos Rust sysroot (stages 1 + 2) and install it.
#
# Assumes stage-0 (core + compiler_builtins) has already been built by
# build-stage0.sh or build-toolchain.sh.

set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
SPEC="$REPO/tools/lythos-toolchain/target-specs/x86_64-lythos-sysroot.json"
SYSROOT="${1:-$REPO/lythos-sysroot}"

cd "$REPO"

echo "Building lythos-libc..."
cargo +nightly build --release \
    -Z build-std=core,alloc,compiler_builtins \
    -Z build-std-features=compiler-builtins-mem \
    --target "$SPEC" \
    --manifest-path tools/lythos-toolchain/lythos-libc/Cargo.toml

echo "Building lythos-unwind..."
cargo +nightly build --release \
    -Z build-std=core,compiler_builtins \
    --target "$SPEC" \
    --manifest-path tools/lythos-toolchain/lythos-unwind/Cargo.toml

echo "Building lythos-rt..."
cargo +nightly build --release \
    -Z build-std=core,alloc,compiler_builtins \
    -Z build-std-features=compiler-builtins-mem \
    --target "$SPEC" \
    --manifest-path userspace/lib/lythos-rt/Cargo.toml

echo "Building lythos-libstd..."
cargo +nightly build --release \
    -Z build-std=core,alloc,compiler_builtins \
    -Z build-std-features=compiler-builtins-mem \
    --target "$SPEC" \
    --manifest-path userspace/lib/lythos-libstd/Cargo.toml

echo "Installing sysroot to $SYSROOT..."
cargo +nightly run \
    --manifest-path tools/lythos-toolchain/sysroot-builder/Cargo.toml \
    -- --toolchain-root "$(rustup show home)/toolchains/nightly-x86_64-unknown-linux-gnu" \
       --out-sysroot "$SYSROOT" \
       --target-spec "$SPEC" \
       --verbose

echo ""
echo "Sysroot installed at: $SYSROOT"
