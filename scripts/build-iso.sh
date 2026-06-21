#!/usr/bin/env bash
# build-iso.sh — build lythos.iso (El Torito bootable ISO 9660 via GRUB 2)
#
# Usage:
#   ./scripts/build-iso.sh          # build debug kernel
#   RELEASE=1 ./scripts/build-iso.sh  # build release kernel
#
# Requirements (choose one):
#   Option A — native (Intel Mac or Rosetta):
#     brew install grub xorriso mtools
#   Option B — Docker (Apple Silicon or if brew fails):
#     Start Docker Desktop, then run this script.
#     The script detects a missing grub-mkrescue and falls back automatically.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
ISO_OUT="$REPO_DIR/lythos.iso"
ISO_DIR="$REPO_DIR/isodir"

# ── Build kernel ──────────────────────────────────────────────────────────────

cd "$REPO_DIR"

if [[ "${RELEASE:-0}" == "1" ]]; then
    echo "[build-iso] building release kernel..."
    cargo build --release
    KERNEL_ELF="$REPO_DIR/target/x86_64-lythos/release/lythos"
else
    echo "[build-iso] building debug kernel..."
    cargo build
    KERNEL_ELF="$REPO_DIR/target/x86_64-lythos/debug/lythos"
fi

if [[ ! -f "$KERNEL_ELF" ]]; then
    echo "error: kernel ELF not found at $KERNEL_ELF" >&2
    exit 1
fi

# ── Assemble ISO directory tree ───────────────────────────────────────────────

rm -rf "$ISO_DIR"
mkdir -p "$ISO_DIR/boot/grub"

cp "$KERNEL_ELF"              "$ISO_DIR/boot/lythos"
cp "$SCRIPT_DIR/grub.cfg"     "$ISO_DIR/boot/grub/grub.cfg"

echo "[build-iso] ISO tree ready:"
find "$ISO_DIR" -type f | sort | sed 's/^/  /'

# ── Run grub-mkrescue (native or Docker) ─────────────────────────────────────

if command -v grub-mkrescue &>/dev/null; then
    echo "[build-iso] using native grub-mkrescue..."
    grub-mkrescue -o "$ISO_OUT" "$ISO_DIR"

elif command -v grub2-mkrescue &>/dev/null; then
    echo "[build-iso] using native grub2-mkrescue..."
    grub2-mkrescue -o "$ISO_OUT" "$ISO_DIR"

elif command -v docker &>/dev/null && docker info &>/dev/null 2>&1; then
    echo "[build-iso] grub-mkrescue not found — falling back to Docker (linux/amd64)..."
    docker run --rm --platform linux/amd64 \
        -v "$ISO_DIR:/isodir:ro" \
        -v "$REPO_DIR:/out" \
        debian:bookworm-slim \
        bash -c "
            apt-get update -qq &&
            apt-get install -y -qq grub-pc-bin grub-common xorriso mtools &&
            grub-mkrescue -o /out/lythos.iso /isodir
        "
else
    echo "" >&2
    echo "error: grub-mkrescue not found and Docker is not running." >&2
    echo "" >&2
    echo "  Option A (native, Intel Mac / Rosetta):" >&2
    echo "    brew install grub xorriso mtools" >&2
    echo "    then re-run this script." >&2
    echo "" >&2
    echo "  Option B (Docker, any architecture):" >&2
    echo "    Start Docker Desktop, then re-run this script." >&2
    echo "" >&2
    exit 1
fi

# ── Done ──────────────────────────────────────────────────────────────────────

echo ""
echo "[build-iso] done: $ISO_OUT ($(du -sh "$ISO_OUT" | cut -f1))"
echo ""
echo "Run with QEMU:"
echo "  qemu-system-x86_64 -cdrom lythos.iso -serial stdio -display none -m 128M"
echo ""
echo "Or attach lythos.iso as an optical drive in VirtualBox / VMware Fusion."
