#!/usr/bin/env bash
set -e

OROS="${OROS:-$HOME/Documents/GitHub/RaptorOS}"
LYTHOS="$(cd "$(dirname "$0")" && pwd)"
RELEASE="${RELEASE:-}"

# ── build flags ──────────────────────────────────────────────────────────────
if [[ -n "$RELEASE" ]]; then
    CARGO_FLAGS="--release"
    KERNEL_BIN="$LYTHOS/target/x86_64-lythos/release/lythos"
else
    CARGO_FLAGS=""
    KERNEL_BIN="$LYTHOS/target/x86_64-lythos/debug/lythos"
fi

OROS_TARGET="$OROS/target/x86_64-oros/release"
ROOTFS_BIN="$LYTHOS/rootfs/lth/bin"
DISK_IMG="$LYTHOS/disk.img"

# ── build OROS userspace ──────────────────────────────────────────────────────
echo "[run.sh] building OROS userspace..."
(cd "$OROS" && cargo build --release -q)

# Copy binaries into rootfs/lth/bin/ so mkrfs (run by build.rs) picks them up.
mkdir -p "$ROOTFS_BIN"
cp "$OROS_TARGET/lythd"    "$ROOTFS_BIN/lythd"
cp "$OROS_TARGET/lythdist" "$ROOTFS_BIN/lythdist"
cp "$OROS_TARGET/lysh"     "$ROOTFS_BIN/lysh"
cp "$OROS_TARGET/rutils"   "$ROOTFS_BIN/rutils"
cp "$OROS_TARGET/rkilo"    "$ROOTFS_BIN/rkilo"
cp "$OROS_TARGET/rpkg"     "$ROOTFS_BIN/rpkg"
cp "$OROS_TARGET/lythd"    "$LYTHOS/rootfs/lth/system/init"

# ── build kernel + disk image ─────────────────────────────────────────────────
# build.rs compiles mkrfs and runs it against rootfs/ to produce disk.img.
echo "[run.sh] building lythos kernel + disk image..."
(cd "$LYTHOS" && cargo build $CARGO_FLAGS -q)

# ── run ───────────────────────────────────────────────────────────────────────
# Use a Unix-domain socket for the serial port so QEMU delivers every
# keystroke reliably (stdio/nographic can silently drop RX on macOS).
# nc(1) is built into macOS and handles the socket connection.
SOCK="/tmp/lythos-serial-$$.sock"

cleanup() {
    kill "$QPID" 2>/dev/null || true
    rm -f "$SOCK"
}
trap cleanup EXIT

echo "[run.sh] launching QEMU..."
qemu-system-x86_64 \
    -kernel "$KERNEL_BIN" \
    -drive  file="$DISK_IMG",format=raw,if=none,id=hd0 \
    -device virtio-blk-pci,drive=hd0 \
    -chardev socket,id=s0,path="$SOCK",server=on,wait=on \
    -serial chardev:s0 \
    -display none \
    "$@" &
QPID=$!

# Wait for QEMU to create the listening socket before connecting.
for i in $(seq 1 40); do
    [ -S "$SOCK" ] && break
    sleep 0.1
done

if [ ! -S "$SOCK" ]; then
    echo "[run.sh] error: QEMU socket not created" >&2
    exit 1
fi

echo "[run.sh] connected — Ctrl+C to quit"

# Python bridge: raw terminal ↔ Unix socket.
# Keeps ISIG so Ctrl+C still exits.
BRIDGE="$(mktemp /tmp/lythos-bridge-XXXX)"
cat > "$BRIDGE" <<'PYEOF'
import socket, sys, os, select, termios

path = sys.argv[1]
sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
sock.connect(path)

old_attrs = termios.tcgetattr(0)
new_attrs = termios.tcgetattr(0)
new_attrs[0] &= ~(termios.IGNBRK | termios.BRKINT | termios.PARMRK |
                  termios.ISTRIP | termios.INLCR | termios.IGNCR |
                  termios.ICRNL | termios.IXON)
new_attrs[1] |=  termios.OPOST | termios.ONLCR
new_attrs[2] &= ~termios.CSIZE
new_attrs[2] |=  termios.CS8
new_attrs[3] &= ~(termios.ECHO | termios.ECHONL | termios.ICANON | termios.IEXTEN)
new_attrs[3] |=  termios.ISIG
new_attrs[6][termios.VMIN]  = 1
new_attrs[6][termios.VTIME] = 0
termios.tcsetattr(0, termios.TCSADRAIN, new_attrs)

try:
    while True:
        r, _, _ = select.select([0, sock], [], [])
        if 0 in r:
            data = os.read(0, 256)
            if not data:
                break
            sock.sendall(data)
        if sock in r:
            data = sock.recv(4096)
            if not data:
                break
            os.write(1, data)
except (KeyboardInterrupt, BrokenPipeError, OSError):
    pass
finally:
    termios.tcsetattr(0, termios.TCSADRAIN, old_attrs)
PYEOF

python3 "$BRIDGE" "$SOCK"
rm -f "$BRIDGE"
