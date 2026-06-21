#!/usr/bin/env bash
# build-toolchain.sh — top-level driver for the Lythos Rust toolchain port.
#
# Runs all three sysroot stages, then prints a summary of what was built.
#
# Prerequisites:
#   - Rust nightly with rust-src component  (`rustup component add rust-src`)
#   - llvm-tools component                  (`rustup component add llvm-tools`)
#   - rust-lld available on PATH (shipped with llvm-tools)
#   - cargo, git

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TOOLCHAIN_DIR="$REPO_ROOT/lythos-toolchain"
TARGET_SPEC="$TOOLCHAIN_DIR/target-specs/x86_64-lythos.json"
SYSROOT_OUT="$REPO_ROOT/lythos-sysroot"

echo "=== Lythos Rust Toolchain Build ==="
echo "Repo root    : $REPO_ROOT"
echo "Target spec  : $TARGET_SPEC"
echo "Sysroot out  : $SYSROOT_OUT"
echo ""

# ── Verify prerequisites ──────────────────────────────────────────────────────

command -v rustup  >/dev/null 2>&1 || { echo "ERROR: rustup not found"; exit 1; }
command -v cargo   >/dev/null 2>&1 || { echo "ERROR: cargo not found";  exit 1; }
command -v rust-lld >/dev/null 2>&1 || {
    echo "INFO: rust-lld not in PATH — attempting via rustup's llvm-tools..."
    LLVM_BIN="$(rustup which --toolchain nightly rust-lld 2>/dev/null || true)"
    if [[ -z "$LLVM_BIN" ]]; then
        echo "ERROR: rust-lld not available. Install with:"
        echo "  rustup component add llvm-tools --toolchain nightly"
        exit 1
    fi
    export PATH="$(dirname "$LLVM_BIN"):$PATH"
}

# Ensure rust-src is present.
rustup component add rust-src    --toolchain nightly 2>/dev/null || true
rustup component add llvm-tools  --toolchain nightly 2>/dev/null || true

# ── Stage 0: core + compiler_builtins ────────────────────────────────────────

echo ""
echo "--- Stage 0: core + compiler_builtins ---"
cd "$REPO_ROOT"
cargo +nightly build \
    --release \
    -Z build-std=core,compiler_builtins \
    -Z build-std-features=compiler-builtins-mem \
    --target "$TARGET_SPEC" \
    --manifest-path lythos-toolchain/lythos-libc/Cargo.toml \
    2>&1

# ── Stage 1: lythos-libc + lythos-unwind ─────────────────────────────────────

echo ""
echo "--- Stage 1: lythos-libc + lythos-unwind ---"
cargo +nightly build \
    --release \
    -Z build-std=core,alloc,compiler_builtins \
    -Z build-std-features=compiler-builtins-mem \
    --target "$TARGET_SPEC" \
    --manifest-path lythos-toolchain/lythos-libc/Cargo.toml \
    2>&1

cargo +nightly build \
    --release \
    -Z build-std=core,compiler_builtins \
    --target "$TARGET_SPEC" \
    --manifest-path lythos-toolchain/lythos-unwind/Cargo.toml \
    2>&1

# ── Stage 2: lythos-std + lythos-libstd ──────────────────────────────────────

echo ""
echo "--- Stage 2: lythos-std + lythos-libstd ---"
cargo +nightly build \
    --release \
    -Z build-std=core,alloc,compiler_builtins \
    -Z build-std-features=compiler-builtins-mem \
    --target "$TARGET_SPEC" \
    --manifest-path lythos-std/Cargo.toml \
    2>&1

cargo +nightly build \
    --release \
    -Z build-std=core,alloc,compiler_builtins \
    -Z build-std-features=compiler-builtins-mem \
    --target "$TARGET_SPEC" \
    --manifest-path lythos-libstd/Cargo.toml \
    2>&1

# ── Assemble sysroot ──────────────────────────────────────────────────────────

echo ""
echo "--- Assembling sysroot at $SYSROOT_OUT ---"
cargo +nightly run \
    --manifest-path lythos-toolchain/sysroot-builder/Cargo.toml \
    -- \
    --toolchain-root "$(rustup show home)/toolchains/nightly-x86_64-unknown-linux-gnu" \
    --out-sysroot    "$SYSROOT_OUT" \
    --target-spec    "$TARGET_SPEC" \
    --verbose \
    2>&1

echo ""
echo "=== Build complete ==="
echo ""
echo "To use this sysroot, add to your project's .cargo/config.toml:"
echo ""
echo "  [build]"
echo "  target = \"$TARGET_SPEC\""
echo ""
echo "  [target.x86_64-lythos]"
echo "  rustflags = [\"--sysroot\", \"$SYSROOT_OUT\"]"
