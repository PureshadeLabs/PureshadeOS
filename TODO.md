# Lythos / OROS — next steps

## Shell (lysh)

- [x] Command history (up-arrow scrolls previous commands)
- [x] Tab completion (builtins only for now)
- [x] `uptime` — print milliseconds since boot via SYS_TIME
- [x] `free` — print free physical frames via a new SYS_MEM_STAT syscall
- [x] `kill <tid>` — terminate a task by ID (new SYS_TASK_KILL syscall)
- [x] Pipe support (`cmd1 | cmd2`) — lysh captures stdout via `sys_log` intercept; `cat` accepts piped stdin; chains of `|` supported
- [ ] I/O redirection (`>`, `<`)

## Filesystem

- [x] VirtIO block device driver (virtio-blk, MMIO or PCI)
- [x] Raw block read/write syscalls (SYS_BLK_READ / SYS_BLK_WRITE)
- [x] RFS kernel driver (`src/rfs.rs`) — read/write/lookup/stat; extent allocator, dir entry management
- [x] mkrfs integration — `build.rs` runs `mkrfs` to produce `disk.img` automatically
- [x] VFS layer: SYS_OPEN, SYS_READ, SYS_WRITE, SYS_CLOSE, SYS_STAT, SYS_READDIR, SYS_CREATE, SYS_UNLINK (SYS 22–29)
- [x] lysh `exec <path>` — load and run an ELF off the filesystem
- [x] lysh `ls`, `cat`, `cp`, `rm`

## Networking

- [x] VirtIO network device driver (virtio-net)
- [x] Ethernet + ARP
- [x] IP + ICMP (ping)
- [x] UDP sockets — SYS_SOCKET (50) / SYS_BIND (51) / SYS_SENDTO (52) / SYS_RECVFROM (53) / SYS_NET_CLOSE (54)
- [ ] TCP stack — connection state machine, SYS_CONNECT / SYS_SEND / SYS_RECV

## Quality of Life

### Debugging / observability

- [x] `SYS_DEBUG_LOG` — implemented as SYS_LOG (11); already existed
- [x] Panic screen with register dump + task name — panic shows task id/name/rsp; exceptions dump all regs + task name
- [x] `SYS_PS` (37) — 48-byte entries: id, state, kind, priority, name[16]

### Scheduler

- [x] `SYS_NANOSLEEP` — sleep without busy-waiting; APIC timer wakeup
- [x] Task priority hints — 3-level (low/normal/high), round-robin within tier
- [ ] SMP task affinity — APs idle after SIPI; bind tasks to specific cores and run the scheduler per-AP

### Memory

- [x] Anonymous mmap — `SYS_MMAP` without requiring a physical address; kernel picks frame
- [x] `SYS_BRK` (38) — heap growth for userspace allocators; HEAP_BASE=`0x0000_0004_0000_0000`
- [ ] Shared memory (`MAP_SHARED`) — two tasks map the same physical frame; kernel ref-counts the backing object via the cap system
- [ ] Futex — `SYS_FUTEX_WAIT` / `SYS_FUTEX_WAKE`; needed for userspace mutexes and condvars

### Filesystem

- [x] `SYS_RENAME` — rename/move files (regular files only; directory rename not supported)
- [x] `SYS_MKDIR` — create directories
- [x] `SYS_SEEK` — seek within open file descriptors (SEEK_SET/CUR/END; read offset only, writes still append)

### IPC ergonomics

- [x] Named endpoints — string → cap handle lookup, no hard-coded handle protocol
- [x] `SYS_IPC_POLL` — non-blocking recv; returns EAGAIN instead of blocking
- [ ] Capability-native async events — deliver async notifications via IPC endpoint instead of POSIX-style signals; sender posts a fixed-layout event message, receiver polls or blocks

---

## Kernel reliability

- [x] IOAPIC driver (replace 8259 PIC — needed for VirtIO PCI interrupts)
- [x] Multi-processor support — AP startup via INIT–SIPI–SIPI; each AP loads IDT, enables its local APIC timer, idles with `sti;hlt` (`src/smp.rs`, `src/arch/x86_64/ap_trampoline.s`)
- [x] Larger default kernel stack — bumped to 64 KiB (`KERNEL_STACK_SIZE` in `src/task.rs`)
- [x] Kernel ASLR — heap, IPC window, and APIC MMIO VAs shifted by a random [0,128 MiB) page-aligned RDRAND offset at boot (`src/kaslr.rs`)
- [x] SYS_MMAP range enforcement — page-aligned check + 0→1 GiB + kernel-space rejection already in `syscall_dispatch`
- [x] Per-process PML4 — `elf::exec` calls `vmm::create_user_page_table`; `task::switch_cr3` loads it on every context switch
- [ ] ELF ASLR — randomise PT_LOAD base address per exec; depends on per-process PML4
- [ ] Reclaim lythd module frames — PMM reserves 512 KiB at `0x400000` forever; free after the ELF is copied to heap
- [ ] VirtIO interrupt-driven completion — replace polled spin on `used_ring.idx`; IRQ line already read at init, just unused
- [x] IPC timeout / cancellation — SYS_IPC_RECV_TIMEOUT (42) / SYS_IPC_SEND_TIMEOUT (43); returns EAGAIN on expiry
- [x] ELF user-facing error reporting — `exec()` panics on malformed ELF; surface a proper error code instead

## Executable format

- [x] OROX — native OROS executable format (replaces bare ELF64 for userspace)
  - 264-byte prefix (`OROX` magic + version + 256-byte body) prepended to ELF; kernel receives only the ELF slice via `exec()`
  - Body encodes: restart policy, cap list (memory/rollback/ipc/registry), service name, up to 4 dep names
  - Parser in `lythos-std/src/orox.rs`; lythd scans `/bin/` for OROX binaries in addition to `/etc/svc/*.svc`; OROX manifest wins on name collision
  - `orox-pack` host tool in `RaptorOS/orox-pack/`: `orox-pack --name <n> [--cap <k>]... [--dep <d>]... [--svc <manifest>] <input.elf> <output>`

## lythd / userspace

- [x] lythdist service manifest format — line-based text, stored as `/etc/svc/<name>.svc` on RFS
  - Fields: `name=`, `path=`, `restart=` (never|on-failure[:N]|always), `cap=` (memory|rollback|ipc:<rights>), `dep=`
  - lythd reads `/etc/svc/` at boot, parses manifests, toposorts by deps, spawns in order
  - `cap=ipc` → lythd creates fresh endpoint and passes handle; `cap=memory:<rights>` → sys_cap_grant; `cap=rollback` → grant rollback cap
  - Replaces hardcoded `managed` array in lythd with manifest-driven `Vec<ManagedSvc>`
- [x] lythd: spawn lythdist and lysh automatically after BootInfo recv (currently manual in test ELFs)

## Text editor (rkilo)

- [x] `rkilo [path]` — kilo-style screen editor ported to OROS (no termios, no POSIX)
  - Input via `SYS_SERIAL_READ` (14) — already raw, no `tcsetattr` needed
  - Output via `SYS_LOG` / `SYS_WRITE`; ANSI VT100 escapes work through QEMU `-serial stdio`
  - Terminal size: ANSI CPR trick (`\x1b[999C\x1b[999B\x1b[6n`) or fallback hardcode 80×24
  - File I/O via VFS: `SYS_OPEN`/`SYS_READ`/`SYS_WRITE`/`SYS_CREATE`/`SYS_CLOSE`
  - Key bindings: `Ctrl-S` save, `Ctrl-Q` quit, `Ctrl-F` find, arrow keys / PgUp / PgDn
  - No syntax highlighting required for v1
  - Primary use case: editing `/etc/svc/*.svc` manifests on a live system

## Input / Power

- [x] PS/2 keyboard driver — IRQ1 via IOAPIC (already masked, GSI wired); scan-code set 2 → ASCII; `SYS_KEY_READ` or feed into serial RX buffer
- [x] ACPI shutdown — write `0x2000` to QEMU PM1a control port `0x604`; needed for clean `make run` exit and `poweroff` command

## Display / GUI

- [ ] VGA text-mode fallback (80×25)
- [x] Framebuffer driver (VESA / Multiboot framebuffer tag)
- [ ] Basic window manager (webwm is already in OROS, needs a framebuffer)
