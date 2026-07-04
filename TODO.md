# PureshadeOS — work queue

## User management / TTY login  ✓ DONE

- [x] `/etc/shadow` — shadow file with per-user password hashes
- [x] SHA-256 password hashing (inline, no_std, no external crate)
- [x] `passwd` command — set/change password
- [x] `su <user>` command — authenticate + kernel uid/gid switch
- [x] Login prompt in lysh — username + silent password, loops on failure, `restart=always`
- [x] `whoami` / `id` take session user from lysh state
- [x] `user_exists` / `verify_password` / `lookup_uid_gid` helpers in rutils
- [x] Kernel `Task` uid/gid fields — inherited from parent on spawn
- [x] `SYS_GETUID` (45) / `SYS_GETGID` (46) — kernel + ABI + lythos-rt
- [x] `SYS_SETUID` (47) / `SYS_SETGID` (48) — kernel + ABI + lythos-rt; security: root-only escalation
- [x] DAC enforcement — SYS_OPEN checks read bit; SYS_CREATE/MKDIR/UNLINK/RENAME check parent dir write bit
- [x] rfs::create / rfs::mkdir assign creator's uid/gid to new inodes
- [x] `docs/spec/syscalls.md` — documented 45–48; gap 49 noted

## Shell

- [ ] I/O redirection (`>`, `<`)

## Networking

- [ ] TCP stack — connection state machine, SYS_CONNECT / SYS_SEND / SYS_RECV

## Kernel reliability

- [ ] ELF ASLR — randomise PT_LOAD base per exec; depends on per-process PML4
- [ ] Reclaim lythd module frames — PMM reserves 512 KiB at `0x400000` forever; free after ELF copy
- [ ] VirtIO interrupt-driven completion — replace polled spin on `used_ring.idx`; IRQ line read at init, unused

## Scheduler / memory

- [ ] SMP task affinity — APs idle after SIPI; bind tasks to specific cores, per-AP scheduler
- [ ] Shared memory (`MAP_SHARED`) — two tasks map same physical frame; kernel ref-counts via cap system
- [ ] Futex — `SYS_FUTEX_WAIT` / `SYS_FUTEX_WAKE`; needed for userspace mutexes / condvars

## IPC

- [ ] Capability-native async events — async notifications via IPC endpoint instead of signals

## Display / GUI

- [ ] VGA text-mode fallback (80×25)
- [ ] Basic window manager (webwm is in OROS, needs framebuffer wired up)
