# PureshadeOS

**A capability-based operating system where authority is a thing you hold, not a thing you are.**

PureshadeOS is built from two halves: **Lythos**, a small bare-metal microkernel written in `no_std` Rust, and **OROS**, a userspace that runs natively on the Lythos ABI. Together they answer a simple question. What if nothing in the system had ambient power, and every single privileged action had to be backed by an unforgeable token?

This is not a Unix clone with capabilities bolted on. The capability model, the IPC design, and the package system were all designed from scratch for this OS.

---

## What makes it tick

### Authority is unforgeable

There is no root that can do anything. There are no ambient permissions waiting to be abused. A task can only touch memory, talk to a device, open an IPC channel, or roll back the system if it physically holds a capability handle that grants that exact right.

Handles are opaque 64-bit tokens. Userspace can pass them and compare them, and that is all. The kernel owns the truth. Every syscall re-checks the rights on the handle you present, so a compromised program is boxed in to the authority it was already given. Rights can only ever shrink when a capability is passed along. There is no path that amplifies privilege, by design.

### The kernel can rewind

`Rollback` is a capability like any other, and it points at a kernel checkpoint. Hold the right handle and you can ask the kernel to restore a known-good state. The package system leans on the same idea from the other direction: a bad system update is just a pointer that gets flipped back.

### IPC with no shared memory

Every IPC endpoint is a single 4 KiB page owned by the kernel. Messages are a fixed 64 bytes and they are copied, sender to ring to receiver, with no memory ever shared between tasks. Capabilities travel across these channels too, so handing another task a slice of your authority is a first-class operation, not a hack. Simple, bounded, and easy to reason about.

### Content-addressed, rollback-first packaging

`shade` is the package store, and it is input-addressed. Every build lands in the store under its own digest, nothing overwrites anything, and the live system is a single symlink into the currently active generation. Updates are atomic. Rollbacks are instant. If a rebuild breaks your machine, the previous generation is still sitting right there.

### Your system, described not assembled

You do not install PureshadeOS by clicking through steps and hoping. You describe it. A **prism** is a declarative profile that says what your system is, and activating it makes reality match the description. The system has its own prism, and every user gets their own independent prism line on top. Rebuild, activate, and if you do not like it, roll back.

### A real userspace, not a shim

OROS speaks the Lythos ABI natively. It ships PID-1 (`lythd`) as a proper root server and service supervisor, a capability distributor, an IPC bus, a shell, editors, utilities, and **WebWM**, a window manager. POSIX-style paths like `/bin` exist only as compatibility symlinks. The real system lives under `/lth`.

---

## The shape of it

```
Lythos  (microkernel)   capabilities, IPC, memory, rollback, no_std bare metal
   │
   │  lythos-abi         one verified kernel <-> userspace contract
   │
OROS    (userspace)      lythd (PID-1) + daemons + shell + apps + WebWM
   │
shade   (packaging)      content-addressed store, generations, prisms
```

Target is x86_64. aarch64 is stubbed and not yet complete.

---

## Build and run

```bash
make            # full build: userspace, kernel, and disk image
make run        # boot it under QEMU
make run-gui    # boot with a graphical display
```

The kernel and userspace each build against their own custom target, so a bare `cargo build` only touches host tools. See the docs for the details.

---

## Go deeper

The promo stops here. The ground truth lives in `docs/spec/`:

- `docs/spec/capabilities.md` — kinds, rights, grant and revoke semantics
- `docs/spec/ipc.md` — ring layout, blocking model, capability transfer
- `docs/spec/syscalls.md` — the full syscall and boundary-struct contract
- `docs/spec/fhs.md` — where everything lives on disk

Brand: **PureshadeOS**. Kernel: **Lythos**. Userspace: **OROS**.
