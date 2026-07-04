#!/usr/bin/env bash
# run.sh — launch Lythos under QEMU with socket-based serial.
#
# Uses a Unix-domain socket for the serial port so QEMU delivers every
# keystroke reliably (stdio/nographic can silently drop RX on macOS).
# Build first with `make all` or `make kernel`; this script only runs QEMU.
set -e

LYTHOS="$(cd "$(dirname "$0")" && pwd)"
RELEASE="${RELEASE:-}"

if [[ -n "$RELEASE" ]]; then
    KERNEL_BIN="$LYTHOS/target/x86_64-lythos/release/lythos"
else
    KERNEL_BIN="$LYTHOS/target/x86_64-lythos/debug/lythos"
fi

DISK_IMG="$LYTHOS/disk.img"
SOCK="/tmp/lythos-serial-$$.sock"

cleanup() {
    kill "$QPID" 2>/dev/null || true
    rm -f "$SOCK" "$BRIDGE"
}
trap cleanup EXIT

qemu-system-x86_64 \
    -kernel "$KERNEL_BIN" \
    -drive  file="$DISK_IMG",format=raw,if=none,id=hd0 \
    -device virtio-blk-pci,drive=hd0 \
    -netdev user,id=net0 \
    -device virtio-net-pci,netdev=net0 \
    -chardev socket,id=s0,path="$SOCK",server=on,wait=on \
    -serial chardev:s0 \
    -display none \
    "$@" &
QPID=$!

for i in $(seq 1 50); do
    [ -S "$SOCK" ] && break
    sleep 0.1
done

if [ ! -S "$SOCK" ]; then
    echo "[run.sh] error: QEMU socket not created" >&2
    exit 1
fi

BRIDGE="$(mktemp)"
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
