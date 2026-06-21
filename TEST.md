# Running Lythos + OROS under QEMU

## Prerequisites

| Tool                       | Version tested            | Install (macOS)         |
| -------------------------- | ------------------------- | ----------------------- |
| `qemu-system-x86_64`       | 8.x+                      | `brew install qemu`     |
| Rust nightly + `build-std` | see `rust-toolchain.toml` | `rustup update nightly` |
| Python 3                   | 3.8+                      | ships with macOS        |
| GNU Make                   | any                       | `brew install make`     |

Both repos must be siblings on disk:

```
~/Documents/GitHub/
    lythos/       ← kernel
    RaptorOS/     ← userspace (lythd, lythdist, lysh)
```

If yours live elsewhere, set `OROS=/path/to/RaptorOS` before running.

---

## Quick start

```bash
cd lythos
./run.sh
```

What `run.sh` does in order:

1. Builds `lythdist`, `lysh`, `lythd` (OROS, release profile).
2. Copies the three binaries into `rootfs/bin/`.
3. Builds the kernel (`cargo build`), which triggers `build.rs`:
   - compiles `tools/mkrfs`
   - runs `mkrfs disk.img 64M rootfs/` to create the VirtIO block image.
4. Launches QEMU with a Unix-socket serial bridge (raw terminal, Ctrl+C exits).

---

## Expected boot output

```
[pmm] ...
[vmm] ...
[heap] ...
[gdt/idt/apic] ...
[virtio-blk] device found ...
[rfs] mounted ...
[smoke] ...
[integration] all checks passed
[boot] lythd launched — entering scheduler

lysh 0.3 — OROS interactive shell
Type 'help' for available commands.

lysh>
```

The `lysh>` prompt means lythd started, spawned lythdist, which spawned lysh. The system is fully up.

---

## lysh commands

```
help             list all commands
ls [path]        list directory (default: /)
cat <path>       print file contents
cp <src> <dst>   copy a file
rm <path>        delete a file
exec <path>      load and run an ELF from disk
ps               list running tasks
uptime           time since boot
free             free physical memory
kill <tid>       terminate task by ID
echo [args]      print to terminal
clear            clear screen
exit             exit shell
```

Tab completion works on command names. Up/down arrow scrolls history.

---

## Makefile targets

```bash
make            # build OROS + kernel (no QEMU)
make run        # build debug kernel + run
make run-release# build release kernel + run
make debug      # run with QEMU -d int,cpu_reset (interrupt trace)
make oros       # build OROS only, copy to rootfs/bin/
make kernel     # build kernel only (also rebuilds disk.img)
make clean      # wipe all build artefacts and disk.img
```

---

## Release build

```bash
RELEASE=1 ./run.sh
```

Or via make:

```bash
make run-release
```

---

## Debug / tracing

Interrupt and triple-fault trace (very verbose):

```bash
make debug
```

Pass extra QEMU flags through `run.sh` with `--`:

```bash
./run.sh -d int,cpu_reset       # interrupt trace
./run.sh -s -S                  # wait for GDB on :1234
```

GDB attach (in another terminal):

```bash
gdb target/x86_64-lythos/debug/lythos \
    -ex "target remote :1234" \
    -ex "set architecture i386:x86-64" \
    -ex "continue"
```

---

## Disk image

`disk.img` is a 64 MiB RFS_V1 image rebuilt automatically by `build.rs` whenever `rootfs/` or `tools/mkrfs/src/main.rs` changes. Rebuild manually:

```bash
make kernel           # fastest — cargo decides if rebuild needed
# or force:
touch rootfs/bin/lysh && make kernel
```

To inspect the image from the host:

```bash
tools/mkrfs/mkrfs disk.img 64M rootfs/   # recreate
# there is no mount tool; use cat/ls in lysh inside QEMU
```

---

## Filesystem layout on disk

```
/
└── bin/
    ├── lythd       init daemon
    ├── lythdist    service manager
    └── lysh        interactive shell
```

Files placed in `rootfs/` before `make kernel` appear at the same relative path under `/` inside QEMU.
