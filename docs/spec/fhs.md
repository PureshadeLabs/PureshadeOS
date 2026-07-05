# OROS Filesystem Hierarchy

## Overview

OROS uses a RFS subvolume-based structure split into three domains:

- **`/lth/`** — system-owned, managed by `shade` and the kernel, immutable at runtime
- **`/shade/`** — reserved OS-wide for the shade store services (store, generations, GC roots, cache); canonical spec is `docs/shade-pkg/02-store.md`
- **Root-level** — standard POSIX paths, user-adjacent and mutable

Subvolumes are snapshots atomically together during updates. Config always rolls back with the system.

---

## Complete Hierarchy

```
/                                   (root, RFS filesystem)
├── /lth/                           (system namespace — kernel and core daemons)
│   ├── /lth/system/                (@core subvolume, read-only)
│   │   ├── /lth/system/boot/       (Lythos kernel binary, UEFI stub)
│   │   ├── /lth/system/lib/        (core system libraries — musl, Lythos stdlib)
│   │   └── /lth/system/init        (lythd binary — PID 1)
│   └── /lth/bin → /shade/gen/current/profile/bin
│                                   (single symlink into the active shade
│                                    generation — docs/shade-pkg/02-store.md §6)
│
├── /shade/                              (reserved OS-wide for shade store services —
│   │                                canonical layout: docs/shade-pkg/02-store.md §1)
│   ├── /shade/store/                   (input-addressed store: <digest>-<name>-<version>)
│   ├── /shade/db/                      (store metadata: valid set, references)
│   ├── /shade/gen/                     (generations + `current` activation symlink)
│   ├── /shade/roots/                   (GC roots)
│   ├── /shade/cache/                   (fetch cache — supersedes /var/cache/shade)
│   ├── /shade/build/                   (transient build directories)
│   └── /shade/log/                     (build logs)
│
├── /cfg/                            (@cfg subvolume, read-write)
│   ├── /cfg/lythos/                (CASK kernel configuration)
│   │   ├── /cfg/lythos/rollback    (rollback flag file — set by shade, cleared by lythd)
│   │   └── /cfg/lythos/boot        (kernel command-line and boot parameters)
│   ├── /cfg/services/              (service definitions — TOML; OS-init config, see note)
│   │   ├── lythd.toml
│   │   ├── lythdist.toml
│   │   ├── lythmsg.toml
│   │   ├── lynet.toml
│   │   ├── lygpu.toml
│   │   └── (other service defs)
│   ├── /cfg/webwm/                 (webWM frontend configuration)
│   │   ├── config.toml             (keybinds, gaps, layout rules, app assignments)
│   │   └── theme.css               (visual theming via CSS custom properties)
│   └── /cfg/shell/                 (shell configuration)
│       └── .shellrc                (lysh shell initialization)
│
├── /user/                           (@home subvolume, read-write)
│   ├── /user/home/                 (user home directories)
│   │   ├── /user/home/alice/       (per-user home)
│   │   │   ├── .local/
│   │   │   │   ├── /user/home/alice/.local/share/   (user data)
│   │   │   │   └── /user/home/alice/.local/state/   (user state)
│   │   │   ├── Documents/
│   │   │   ├── Downloads/
│   │   │   ├── Desktop/
│   │   │   └── .config/            (user per-app configuration)
│   │   └── /user/home/bob/         (additional users)
│   └── /user/root/                 (root home)
│
├── /bin/                            (symlinks to /lth/bin — for POSIX compatibility)
│   └── (populated at boot by lythd, points to active tools)
│
├── /sbin/                           (symlinks to /lth/bin — system binaries)
│   └── (populated at boot by lythd)
│
├── /lib/                            (symlinks to /lth/system/lib — for POSIX compat)
│   └── (populated at boot, core libraries accessible at standard path)
│
├── /var/                            (runtime and volatile state — tmpfs or small RFS)
│   ├── /var/run/                   (PID files, sockets)
│   │   ├── /var/run/lythmsg.sock   (lythmsg IPC socket)
│   │   ├── /var/run/lythd.pid
│   │   └── (other daemon sockets)
│   ├── /var/log/                   (system logs)
│   │   ├── /var/log/lythd.log
│   │   ├── /var/log/lythmsg.log
│   │   ├── /var/log/kernel.log
│   │   └── (service logs)
│   ├── /var/cache/                 (transient caches — shade caches live under
│   │                                /shade/cache/, not here)
│   └── /var/tmp/                   (temporary files)
│
├── /tmp/                            (user temporary files — tmpfs, world-writable)
│   └── (ephemeral, cleared on reboot)
│
├── /etc/                            (legacy POSIX config — minimal, mostly empty)
│   ├── /etc/passwd                 (generated from /user/home, read-only at runtime)
│   ├── /etc/group
│   ├── /etc/hostname
│   └── /etc/fstab                  (RFS subvolume mount configuration)
│
├── /root/                           (symlink to /user/root for POSIX compat)
│
├── /home/                           (symlink to /user/home for POSIX compat)
│
├── /proc/                           (kernel proc filesystem — optional, minimal)
│   ├── /proc/cmdline               (kernel command line)
│   ├── /proc/cpuinfo               (CPU info)
│   ├── /proc/meminfo               (memory info)
│   └── (minimal — CASK kernel exposes this)
│
├── /sys/                            (kernel sysfs — optional, minimal)
│   ├── /sys/class/                 (device classes)
│   └── /sys/devices/               (device tree)
│
└── /dev/                            (device nodes — devtmpfs)
    ├── /dev/null, /dev/zero, /dev/full
    ├── /dev/urandom, /dev/random
    ├── /dev/tty, /dev/pts/         (terminal devices)
    ├── /dev/sd*, /dev/nvme*        (block devices)
    └── (managed by devtmpfs or udev equivalent)
```

---

## Subvolume Mapping

| Path          | Subvolume                      | Writable | Snapshots | Purpose                                 |
| ------------- | ------------------------------ | -------- | --------- | --------------------------------------- |
| `/lth/system` | `@core`                        | No       | Yes       | Kernel and `lythd` binary               |
| `/r`          | (directory on root RFS, v1)\*  | No\*\*   | No        | shade store, generations, GC roots       |
| `/cfg`        | `@cfg`                         | Yes      | Yes       | System config, snapshotted with `@core` |
| `/user`       | `@home`                        | Yes      | No        | User data, persistent across updates    |
| `/var`        | (separate, tmpfs or small vol) | Yes      | No        | Logs, runtime state, transient          |
| `/tmp`        | tmpfs                          | Yes      | No        | Ephemeral, world-writable               |

\*Whether `/shade/` becomes its own subvolume once RFS v2 subvolumes are
specified is an open decision — `docs/shade-pkg/02-store.md` §1.

\*\*`/shade/store`, `/shade/db`, `/shade/gen` are writable only by the store services;
`/shade/cache`, `/shade/build`, `/shade/log` are working areas. See
`docs/shade-pkg/02-store.md` §1.

**Config-format scope.** The TOML under `/cfg/services/*.toml` and
`/cfg/webwm/config.toml` is **OS-init and desktop configuration**, read
directly by `lythd` and webWM at boot — it is *not* an shade recipe and is not
evaluated by shadec. The package system uses **Shade** as its sole recipe
language (`docs/shade-pkg/03-recipe-format.md`, `docs/shade/`); that change does
**not** reach these files. `TODO(open):` whether OS-init/desktop config
eventually migrates to Shade (a unified declarative config story) is a
separate design — it would require a Shade evaluator available at PID-1 boot,
which the bootstrap (`docs/shade-pkg/09-bootstrap.md`) does not currently provide.
Flagged, out of scope for the package-system frontend change.

---

## Boot-Time Initialization

1. **Lythos kernel** mounts `/lth/system` read-only from `@core` snapshot
2. **`lythd` (PID 1)** reads rollback flag from `/cfg/lythos/rollback`
3. **If rollback flag present**: starts 30-second stability timer
4. **`lythd` mounts remaining subvolumes**:
   - `/cfg` from `@cfg` (read-write)
   - `/user` from `@home` (read-write)
   - `/var` (tmpfs or small persistent vol)
5. **`lythd` populates symlinks**:
   - `/bin/` → `/lth/bin/` entries
   - `/sbin/` → `/lth/bin/` system tools
   - `/lib/` → `/lth/system/lib/`
   - `/root/` → `/user/root/`
   - `/home/` → `/user/home/`
6. **`lythd` generates `/etc/passwd`, `/etc/group`** from `/user/home/` and `/user/root/`
7. **`lythd` reads service definitions** from `/cfg/services/*.toml`
8. **`lythd` spawns services in dependency order**
9. **On boot cleanup**: if stability timer expires, clears `/cfg/lythos/rollback`

---

## Snapshot Atomicity

Package-level atomicity is the shade generation mechanism
(`docs/shade-pkg/02-store.md` §5–6): every install/remove/rollback creates a new
generation under `/shade/gen/`, and activation is one atomic symlink flip of
`/shade/gen/current`. Rollback is flipping back to a prior generation's manifest.

Boot-critical updates additionally arm the boot rollback protocol:

```
shade writes previous generation number → /cfg/lythos/rollback
```

On next boot, if a critical daemon fails within 30 seconds:

```
lythd re-points /shade/gen/current → recorded generation (same atomic flip)
lythd reboot
```

If 30 seconds pass cleanly:

```
lythd rm /cfg/lythos/rollback
```

Kernel/config snapshot atomicity (`@core` + `@cfg` snapshotted together) is
deferred until RFS v2 specifies subvolume snapshots; the generation manifest
is the intended integration point (`docs/shade-pkg/02-store.md` §6.3).

---

## Key Invariants

- **`/lth/` is immutable at runtime** — no mutations except by `shade` on update
- **`/shade/` is reserved OS-wide for the shade store services** — store paths are
  immutable once registered; only the store services write `/shade/store`, `/shade/db`,
  `/shade/gen` (`docs/shade-pkg/02-store.md`)
- **`/cfg` rolls back with `/lth/system`** — atomically snapshotted together
- **`/user` never rolls back** — user data is persistent across updates
- **`/var` is ephemeral** — cleared or reset on reboot
- **Symlinks in `/bin`, `/sbin`, `/lib`** allow POSIX-compliant tools to find binaries and libraries
- **All user-facing tools are reached via `/lth/bin`** — a single symlink to
  `/shade/gen/current/profile/bin`, flipped atomically per generation

---

## POSIX Compatibility

OROS provides standard POSIX paths for compatibility:

| POSIX Path              | OROS Reality                                          |
| ----------------------- | ----------------------------------------------------- |
| `/bin`                  | Symlinks to `/lth/bin/`                               |
| `/sbin`                 | Symlinks to `/lth/bin/` (system tools)                |
| `/lib`                  | Symlinks to `/lth/system/lib/`                        |
| `/etc`                  | Minimal: `/etc/passwd`, `/etc/hostname`, `/etc/fstab` |
| `/root`                 | Symlink to `/user/root/`                              |
| `/home`                 | Symlink to `/user/home/`                              |
| `/tmp`                  | tmpfs, world-writable, ephemeral                      |
| `/var`                  | Logs, runtime state, transient                        |
| `/proc`, `/sys`, `/dev` | Kernel-provided, minimal                              |

Tools ported from Linux expect these paths to exist and work; OROS satisfies that expectation via symlinks and compatibility stubs, with no bloat in the actual namespace.
