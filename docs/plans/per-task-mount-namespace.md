# Per-task mount namespaces — design (no code yet)

**Status:** design proposal. Follows the design-doc-first rule that governed the
original mount/VFS/capability work. **No kernel edits accompany this document.**
`kernel/src/vfs.rs` is untouched.

**Author's note to the reviewer:** several decisions here are security-relevant
(they change which task can see and write which subtree). Open ones are flagged
inline as **[DECISION]** and collected in [§7 Open questions](#7-open-questions);
I am proposing, not deciding those. As of 2026-07-21 the reviewer has **decided
four** — the inheritance ABI (§1.1), who may create/change a namespace (§3.1),
the `Filesystem`-cap subtree (§3.2, deferred), and the backend-sharing model
(§5.1, which also subsumes the old `MountId`-uniqueness question). Those now read
as **[DECIDED]** / **[DEFERRED]** in the body, with the reasoning recorded there;
§7 lists only what is still open. **Update 2026-07-21:** the backend-sharing
model of §5.1 has since **landed** — in both the kernel VFS and the userspace
build executor — ahead of the namespace itself, because everything below assumes
it. §5.1 now *describes* the shipped `BackendId` indirection rather than proposing
it; no other part of this design is built yet.

---

## 0. Problem

`SYS_MOUNT = 56` is implemented, but the mount table is a single global static:

- `kernel/src/vfs.rs:465` — `static STATE: VfsState` holds one `Option<Vfs>`.
- `kernel/src/vfs.rs:451` — `struct Vfs { table: MountTable, … }`, one
  `MountTable` for the whole kernel.
- Every FS syscall resolves against that one table via `resolve()`
  (`vfs.rs:848`) → `MountTable::resolve` (`fs/vfs-core/src/mount.rs:149`,
  longest-prefix routing).

So there is exactly **one namespace for the whole machine**: a mount made by any
task is visible to every task. Meanwhile `SandboxPlan` (`pkg/shade-build/src/
sandbox.rs`) already computes a *per-build* mount list — read-only input mounts
plus one read-write build dir, visible **only in the builder's own view**
(`from_spec`, `check_read`/`check_write`). The kernel cannot honor that: it has
nowhere to put a mount list that only one task sees. This is precisely the gap
between "the sandbox *describes* rights" and "the kernel *enforces* them."

This document designs the missing primitive: a mount namespace that is a
property of a task (or a group of tasks), not of the machine.

---

## 1. Ownership model

**Proposal: per-task namespace handle, shared by reference, copy-on-mount at
`SYS_EXEC` by default.**

- A **namespace** is a `MountTable` plus its realize guards (the `guards` map
  currently inside `Vfs`). Give it an id (`NsId(u64)`) and refcount it.
- Each `Task` gains a field `ns: NsId` (a handle, not an inline table), so
  several tasks can point at the same namespace. The global table becomes the
  **root namespace** `NsId(0)` (see [§6 Migration](#6-migration)).
- **[DECISION] Inheritance at `SYS_EXEC`.** Three options; I recommend (b):
  - (a) **Inherit/share** — child points at the parent's `NsId`; later mounts by
    either are seen by both. Cheapest; but a builder could mutate its parent's
    view. Wrong default for a sandbox.
  - (b) **Copy-on-exec (recommended default)** — the child gets a *new* `NsId`
    whose `MountTable` is cloned from the parent's at exec time. Subsequent
    mounts diverge. This is what a build needs: the executor (parent) mounts the
    inputs + build dir into the child's fresh namespace, and nothing the child
    mounts leaks back out.
  - (c) **Fresh/empty** — child starts with an empty namespace (not even root).
    Too strong as a default (the child cannot find `/lth/bin/...` to run); useful
    only for a fully sealed builder that is handed every mount explicitly.
- **Who decides.** The inheritance mode is chosen by the *spawner*, not the
  child — a builder task is (b) or (c), a normal `lysh` child is (a) or (b).
  **[DECIDED]** The mode is *not* a `SYS_EXEC` argument: see
  [§1.1](#11-namespace-syscalls-sys_ns_create--sys_ns_enter). The spawner
  arranges the child's view with a separate `SYS_NS_CREATE` / `SYS_NS_ENTER`
  pair *before* `SYS_EXEC`, and the child simply inherits the spawner's current
  namespace.

### 1.1 Namespace syscalls (`SYS_NS_CREATE` / `SYS_NS_ENTER`)

**[DECIDED — was [§7 Q1].] Two dedicated syscalls, not a `SYS_EXEC` flag.**

*Reasoning.* `SYS_EXEC = 10` already consumes all six argument registers
(`a1`=elf_ptr, `a2`=elf_len, `a3`=caps_ptr, `a4`=caps_len, `a5`=argv_ptr,
`a6`=argv_bytes — see `docs/spec/syscalls.md` §SYS_EXEC). There is **no free
slot** for a namespace-mode flag, so the "one syscall" option is not actually
available without repurposing an existing argument (e.g. overloading the caps
array), which would be a silent, error-prone ABI change. A separate pair is also
strictly more composable: the spawner can create a namespace, mount into it, and
inspect it before committing a child to it. The child inherits the spawner's
**current** namespace at exec (there is no way to pass an `NsId` to `SYS_EXEC`,
by the same no-free-slot argument), so the spawner sets its current namespace to
the child's intended view immediately before `SYS_EXEC` and restores its own
afterward.

Proposed numbers: the next two free syscall numbers after the current
`SYSCALL_MAX = 66`, i.e. **`SYS_NS_CREATE = 67`** and **`SYS_NS_ENTER = 68`**
(the gaps 49, 50–54 and 59 stay retired and are never reused).

| Nr | Name | Args | Returns |
|----|------|------|---------|
| 67 | `SYS_NS_CREATE` | `a1` = mode (`NS_CLONE = 0`: copy the caller's current view; `NS_EMPTY = 1`: no mounts) | new `NsId` (`u64`, ≥ 1, always `< ERR_MIN`) on success; `EINVAL` (bad mode), `EAGAIN` (namespace table exhausted) on error |
| 68 | `SYS_NS_ENTER` | `a1` = target `NsId` | the caller's *previous* `NsId` on success (so it can be restored); `ENOENT` if the caller did not create that `NsId` (ownership is not revealed — a task cannot probe for namespaces it does not own); `EINVAL` on a malformed id |

- **`SYS_NS_CREATE`** makes a new namespace derived from the caller's *current*
  namespace (`NS_CLONE` copies the routing table and shares its backends by
  reference per [§5](#5-cost); `NS_EMPTY` starts with no mounts) and records the
  caller as its owner. It does **not** change the caller's current namespace —
  the caller keeps running in its own view. Refcount starts at 1.
- **`SYS_NS_ENTER`** sets the caller's current namespace to a namespace **it
  created**, returning the previous `NsId`. It affects only the caller. It
  cannot target a namespace the caller does not own (→ `ENOENT`), which is the
  ABI-level expression of the [§3](#3-capability-interaction) invariant.

**Exact spawner sequence to give a child a restricted view** (e.g. the build
executor handing a builder its inputs + one RW build dir):

```
old   = SYS_NS_ENTER(SYS_NS_CREATE(NS_EMPTY))  // create child view, switch to it,
                                               //   remember own view `old`
SYS_MOUNT(input_a, …, RO)                       // populate the child view — these
SYS_MOUNT(input_b, …, RO)                       //   mounts land in the caller's
SYS_MOUNT(build_dir, …, RW)                     //   (now the child's) namespace
child = SYS_EXEC(builder_elf, …, caps, …)       // child inherits the current ns
SYS_NS_ENTER(old)                               // executor restores its own view
```

After `SYS_EXEC` the child holds the only live reference to the new namespace
(the executor dropped it by re-entering `old`), so when the child exits,
[§4](#4-lifetime) teardown drops the namespace and every mount it held —
including the RW build dir — with nothing leaking into the executor's view or a
sibling's. The whole sequence runs inside one cooperatively-scheduled task with
no intervening FS syscall from another task, so the transient "executor is in
the child's namespace" window between the first and last `SYS_NS_ENTER` is not
observable elsewhere.

Cloning a `MountTable` requires cloning its `Box<dyn FsBackend>` entries **by
reference, not by value** — a backend is a live filesystem, not copyable. So the
namespace must hold `Rc`/`Arc`-like shared backends (or an indirection table of
backend ids), and a mount becomes "namespace N maps prefix P → backend-ref B."
This is the main structural change and is called out in [§5 Cost](#5-cost).

---

## 2. Resolution

Today every FS syscall resolves against `state()` — the single global `Vfs`.
With namespaces, **every FS syscall must resolve against the caller's
namespace**, i.e. `namespace_of(current_task_id())` instead of `state()`.

Affected syscalls (all currently route through the global table):

| Syscall | Kernel entry (`vfs.rs`) | Resolves via |
|---|---|---|
| `SYS_OPEN` | `open` (914) | `resolve` → `table.resolve` |
| `SYS_READ` / `SYS_WRITE` | `read` (961) / `write` (986) | fd → mount id (already bound at open) |
| `SYS_CREATE` | `create` (1053) | `resolve_parent` |
| `SYS_MKDIR` | `mkdir` (1101) | `resolve_parent` |
| `SYS_UNLINK` | `unlink` (1145) | `resolve_parent` |
| `SYS_RENAME` | `rename` (1378) | `resolve_parent` ×2 |
| `SYS_SYMLINK` / `SYS_READLINK` | `readlink` (1347) | `resolve` / `table.resolve` |
| `SYS_STAT` | `stat_path` (1478) | `resolve` |
| `SYS_READDIR` | `readdir_path` (1496) | `resolve` |
| `SYS_SEEK` | `seek` (1455) | fd-local (no path) |
| `SYS_MOUNT` | `mount` (729) | mutates the table |
| `SYS_STORE_REMOVE` | (syscall.rs 1734) | Filesystem-cap gated table op |

**Where resolution happens today vs. where it must move.** Today the resolvers
take `v: &mut Vfs` from the single `state()`. The change is mechanical but
pervasive: each entry point must first look up the caller's namespace
(`state_for(current_task_id())`) and thread *that* `Vfs`/`MountTable` through
`resolve`/`resolve_parent`. `resolve` and `resolve_parent` already take the
`Vfs` by parameter, so their bodies are unchanged — only the callers' source of
the `Vfs` changes.

**Open fds are already namespace-safe.** `OpenFile` stores a `MountId`
(`vfs.rs:438`) and read/write re-address through it, so a fd opened in one
namespace keeps working even if the path would resolve differently later —
**provided** `MountId`/backend identity stays valid across namespaces. This
constrains the migration: mount ids must be unique per-backend, not per-table —
resolved by the global `BackendId` scheme in
[§5.1](#51-backend-ownership-across-namespaces--landed-for-both-layers).

---

## 3. Capability interaction

Two mechanisms would now both bound a task's filesystem authority, and they must
be composed explicitly:

- **A `Filesystem` capability** today is a *boolean gate with rights bits*
  (`SYS_MOUNT`: `has_kind_with_rights(Filesystem, WRITE)`, syscall.rs:1666). A
  restricted cap (e.g. `READ`-only) limits *which operations* a holder may
  perform (it cannot mount), **but not which subtree** it may touch. There is no
  subtree field on the cap.
- **A namespace** limits *which subtree is visible at all*: a path with no
  covering mount fails to resolve (`MountError` → `errno_mount`), regardless of
  cap rights.

**[DECISION] Authority when they disagree — the namespace is the outer bound;
the cap is the inner bound. Both must permit.** Proposed rule:

1. Resolution runs first, in the caller's namespace. If no mount covers the
   path → deny (`ENOENT`/`ENOMNT`). A namespace can therefore *hide* a subtree
   the cap's rights would otherwise allow.
2. If resolution succeeds, the cap rights are checked as today (e.g. write
   needs `WRITE`; the realize guard still seals `/shade/store`). A cap can
   therefore *narrow* rights within a visible subtree, but never *widen*
   visibility.

This makes the namespace authoritative for **visibility** and the cap
authoritative for **operation rights** — they intersect, neither overrides. It
matches how `SandboxPlan` already reasons: `check_read`/`check_write` answer in
terms of the *mount list* (visibility) and then the mount's rights bits
(`RIGHT_READ`/`RIGHT_WRITE`). The builder holds a `Filesystem` cap with only the
rights its mounts carry, in a namespace that contains only its inputs + build
dir.

### 3.1 Who may create or change a namespace — ambient, self-and-children only

**[DECIDED — was [§7 Q2].] Creating a namespace requires no capability; it is
ambient but affects only yourself and your children. Re-pointing or mutating
*another* task's namespace does not exist as an ABI capability at all.**

*Reasoning.* Creating a *more restricted* view of what you can already see is a
**voluntary reduction of your own authority** — you can only ever `NS_CLONE`
your current view or start `NS_EMPTY` and mount back a subset of what you could
already reach. Handing that restricted view to a child you are about to spawn is
likewise strictly de-escalating. Neither operation lets a task see or touch
anything it could not already, so gating it behind a capability would add
ceremony without adding safety — the same reasoning by which the cap model lets
any task *drop* rights freely but never *gain* them.

The dangerous operation is the opposite one: **re-pointing or mutating a task's
namespace out from under it** (making some other task suddenly see a different
`/`, or injecting a mount into a namespace it is already running in). That is an
authority *amplification* against the victim. The decision is that this is not a
cap-gated operation — **it is simply absent from the ABI.** There is deliberately
no `SYS_NS_ENTER(other_task)`, no `SYS_NS_MOUNT_INTO(nsid, …)` for a namespace
the caller does not own, and no "set task T's namespace" syscall. `SYS_NS_ENTER`
changes only the *caller's* current namespace and only to a namespace the caller
created (→ `ENOENT` otherwise); `SYS_MOUNT` only ever mutates the caller's own
current namespace. A namespace becomes a child's by the child *inheriting the
spawner's current view at exec* (§1.1), never by a third party writing into it.

**What the ABI therefore does *not* offer** (by construction, not by omission):

- No way to enter, read, or enumerate a namespace you did not create.
- No way to add, remove, or replace a mount in a namespace you are not currently
  running in.
- No way to force another task into a different namespace, or to change the view
  of a task that is already running.

Because a task can only ever narrow its own reachable set and its children's,
the namespace subsystem needs no new `CapKind` — the absence of the amplifying
operations is the enforcement.

### 3.2 Subtree-scoped `Filesystem` caps — deferred

**[DEFERRED — was [§7 Q3].] The `Filesystem` cap stays rights-only. Subtree
scoping lives entirely in the namespace; the cap model is not grown in this
design.**

A `Filesystem` cap remains a rights gate (`READ`/`WRITE`/`GRANT`), with **no
path/subtree field**. Which subtree a task can touch is decided solely by what
its namespace makes visible (§3, rule 1). This keeps the cap model unchanged and
avoids two overlapping mechanisms for "which subtree."

*What would have to change if subtree-scoped mount authority is adopted later.*
If a future requirement needs mount *authority itself* scoped — e.g. a builder
that may `SYS_MOUNT` only under `/shade/build/<its-id>` and nowhere else — then:
(1) `CapKind::Filesystem` (or a new `CapKind`) grows a subtree/prefix field;
(2) `cap_grant` must be able to *narrow* that prefix on derivation (a child cap's
subtree ⊆ the parent's), mirroring how rights already narrow, and never widen
it; (3) the `SYS_MOUNT` gate (`has_kind_with_rights(Filesystem, WRITE)`) must
additionally check the requested mount point lies within the cap's subtree; and
(4) the boundary-struct/asserts for the cap representation change, so kernel and
userspace cap encoders must move together. None of that is done here — it is
recorded as future work so the present design stays inside the existing cap
model.

---

## 4. Lifetime

- **Creation.** A namespace is created at `SYS_EXEC` under mode (b)/(c) (clone
  or empty), or explicitly via a future `SYS_NS_CREATE`. Refcount starts at 1
  (the owning task).
- **Sharing.** Mode (a) increments the parent namespace refcount instead of
  cloning.
- **Teardown on task exit.** `task_exit`/`kill_task`/`sweep_dead` must
  decrement the namespace refcount. At refcount 0 the namespace's `MountTable`
  is dropped, which drops its backend-refs; a backend whose last namespace
  reference is gone is unmounted and dropped.
- **What stops a dead builder's mounts from leaking.** Because the builder's
  input/build mounts live in *its* namespace (not the global one), reaping the
  builder drops that namespace and with it every mount it held — including the
  RW build dir. Nothing survives into the root namespace or any sibling. This is
  the concrete payoff over the global table, where a crashed task's mounts
  persist forever.
- **Interaction with the exit-code reaper.** Namespace teardown is a Task
  teardown step (`sweep_dead`), *not* tied to the exit-status record — the
  status record (see `docs/spec/syscalls.md` §SYS_TASK_EXIT) outlives the Task,
  the namespace does not.

**[DECISION] Root-namespace pinning.** `NsId(0)` (root) must never reach
refcount 0 even if PID-1 semantics change — it holds the `/` and `/shade/store`
mounts the whole system needs. Proposal: root is pinned (refcount floor 1).

---

## 5. Cost

- **Memory per namespace.** One `MountTable` = `Vec<Mount>` + `next_id`; each
  `Mount` = `MountId` + `Vec<String>` prefix + a backend **reference**. With
  shared backends, a cloned namespace costs only the per-mount routing metadata
  (a handful of small `Vec`s), not a copy of any filesystem. Estimate: tens of
  bytes per mount, a few hundred bytes for a builder's 3–10 mounts. Plus a
  refcount word and the `NsId` field on each `Task`.
- **Resolution hot path.** Unchanged in complexity: still longest-prefix over
  `Vec<Mount>` (`covering_index`, `mount.rs:97`) — O(mounts × path-depth). A
  builder namespace has *fewer* mounts than the global table, so per-call cost
  is equal or lower. The one added cost is a namespace lookup
  (`current_task_id()` → `NsId` → table) per FS syscall — a single indexed
  fetch, negligible next to the backend I/O.
- **Does `MountTable`'s longest-prefix routing survive unchanged?** **Yes for
  the routing algorithm.** `covering_index`/`is_prefix`/`rel_path` are
  table-local and need no change. What changes is *ownership of backends*:
  `Mount.backend: Box<dyn FsBackend>` must become a shared reference so a table
  can be cloned without copying a filesystem. That is the one invasive edit to
  `fs/vfs-core/src/mount.rs`, and it is **[DECISION]**-worthy because it changes
  the `MountTable` public type (`mount()` takes a backend-ref, not a `Box`).

### 5.1 Backend ownership across namespaces — landed, for both layers

**[LANDED 2026-07-21 — was [§7 Q5], and folds in [§7 Q4] (`MountId`
uniqueness): they were the same question one layer apart — "what is a backend's
stable identity, and who holds the `&mut`".] The backend lives in a single
global owning table keyed by a `Copy` `BackendId`; routing tables store
`BackendId`s, not backend pointers.** This section now *describes* the shipped
code (option (B) below), not a proposal. It landed **ahead of** the namespace
work because the rest of this design assumes it: both the kernel VFS and the
userspace build executor converged onto it in one change.

*Options considered, against the real constraints (single-threaded kernel; the
realize guard's and the backend-by-id accessor's assumption of unique `&mut`
access; `MountId`/backend identity must stay valid when an fd opened in one
namespace is used after the path would resolve elsewhere — [§2](#2-resolution)):*

- **(A) `Rc<RefCell<dyn FsBackend>>`.** Cloning a table bumps the `Rc`; mutable
  access is `borrow_mut()` at the point of use. Works single-threaded, but: the
  "unique `&mut`" assumption is enforced only at *runtime* by `RefCell`, so a
  latent re-entrant borrow **panics** instead of failing to compile; and the
  `Rc` pointer is **not** a stable, serializable identity, so open fds still need
  a separately-assigned id for re-addressing — meaning (A) does not actually
  answer Q4, it only adds interior mutability on top of the id scheme you still
  need. **Rejected.**
- **(B) backend-id indirection table — *this is what shipped*.** A single global
  owner — `BackendStore`, a `BTreeMap<BackendId, Box<dyn FsBackend>>` with a
  monotonic `next_id`, held by the VFS state (`vfs_core::mount::BackendStore`,
  `kernel/src/vfs.rs` `Vfs::backends`) — assigns each backend a `BackendId` at
  mount time. A `Mount` stores `{ at: Vec<String>, backend: BackendId }`; the
  backend itself lives only in the store. Cloning a `MountTable` copies
  `BackendId`s (they are `Copy`), sharing the backend with zero refcount traffic
  on the future hot clone-at-`SYS_NS_CREATE` path.
- **(C) something else** (e.g. per-namespace `Box` copies): rejected — a backend
  is a live filesystem, not copyable.

*Borrow-checker story as implemented (not "single-threaded so it's fine"):* the
`BackendStore` is a field of `Vfs` **disjoint** from the routing `MountTable`
(separate fields, so their borrows never alias). `MountTable::resolve` takes
`&self` and returns `(BackendId, String)` — routing holds **no** backend borrow
at all. Every FS handler in `kernel/src/vfs.rs` runs the same three lexical
steps: (1) `v.table.resolve(path)` → copy out the `BackendId`, routing borrow
ends immediately; (2) `v.backends.get(id)` → one `&mut dyn FsBackend`; (3) the
handler's ops on that one backend, then the `&mut` is dropped before return.
**What holds the borrow, and for how long:** one stack frame — the handler —
holds one `&mut` into the store, for one backend's run-to-completion work, and
never across a `yield`, a block, or a nested call back into resolution or another
backend. FS syscalls are non-reentrant, so at any instant at most one `&mut` into
the store is live; the borrow checker is satisfied by ordinary lexical scoping,
with **no `RefCell` and therefore no possible borrow panic.** A cloned
`MountTable` holds only `Copy` `BackendId`s, so it cannot alias a backend —
cloning is a memcpy of routing metadata.

> **Invariant the namespace work relies on (new, record it):** a `BackendId` is
> assigned **once**, by `BackendStore::insert`, and is **never reused** (`next_id`
> only increments). So a stale id — an fd whose backend was freed — resolves to
> `None` in `BackendStore::get`, never to a *different* later backend. The
> namespace teardown ([§4](#4-lifetime)) may therefore free a backend the instant
> its last route is gone without first walking every fd: fds fail closed
> (`EBADF`/no-op), they cannot be silently re-pointed. Teardown order is fixed:
> drop the route (`MountTable::unmount` → the `BackendId`), and free the backend
> from the store **only once `MountTable::routes_to(id)` is false**, so a backend
> shared by several tables (namespaces) outlives the removal of any one route and
> no live route is ever left dangling to a freed id.

*Q4 (`MountId` uniqueness across namespaces), resolved by the above:* the fd's
stored identity **is** the `BackendId` (`OpenFile::mount`, typed `MountId` —
which is now a `pub type MountId = BackendId` alias: one value, two roles). It is
global and namespace-independent by construction, so an fd opened in one
namespace re-addresses to the one shared backend regardless of where it is used.
Backend lifetime is the explicit route check above (how many tables route to the
id), the same bookkeeping (A)'s `Rc` would do, made explicit; the kernel
`vfs::unmount` composes it (`MountTable::unmount` + `routes_to` +
`BackendStore::remove` + guard drop), with root (`NsId(0)`'s `/`) pinned against
teardown per [§4](#4-lifetime).

*Same model for the userspace executor's backend handle — also landed.* The
build executor's `fs` field was `RefCell<Box<dyn StoreFs>>` — option (A) one
layer up, with the same runtime-borrow-panic risk. It converged in this same
change: `Executor.fs` is now a plain `Box<dyn StoreFs + 'a>` (single owner), and
its consumers (scratch/log setup, the dep-existence check, realization) take
`&mut` from it per operation via `&mut self` run methods
(`pkg/shade-build/src/executor.rs`). The `RefCell` and its `borrow_mut()` sites
are gone, so the userspace layer now matches the kernel's story exactly: one
owner, per-operation `&mut`, no interior mutability, no borrow panic. The
existing `Executor::with_backends` / `Executor::new` constructors and every call
site kept their shape (the two production call sites already bound `mut exec`;
only test bindings gained `mut`). This closes the convergence question rather
than deferring it.

---

## 6. Migration

- The current global `Vfs` becomes **the root namespace `NsId(0)`**. `init()`
  (`vfs.rs:477`) still mounts `/` and (later) `/shade/store` into it exactly as
  today — no boot-path behavior change.
- The bootstrap task (id 0 / lythd) is assigned `ns = NsId(0)`. Because every
  task spawned before the namespace work inherited the global table implicitly,
  the default `SYS_EXEC` mode during migration is **(a) share `NsId(0)`** — so
  the *entire existing system boots identically* (every task sees the one table
  it sees today). Per-task isolation is opt-in: only the build executor requests
  mode (b)/(c).
- The `/shade/store` mount (created with `MOUNT_STORE`, guarded by
  `RealizeGuard`, `vfs.rs:457`) lives in root and is **shared into** builder
  namespaces read-only rather than re-created — the realize guard and its seal
  semantics are preserved because the backend is the same shared instance.
- Staging: land the refactor in two commits — (1) introduce `NsId` with a single
  root namespace and route all resolution through `namespace_of()` (pure
  refactor, no behavior change, boot stays green); (2) add copy/empty modes and
  wire the executor. This keeps each step independently boot-verifiable per the
  project's verification discipline.

---

## 7. Open questions

> **Resolved and folded in (2026-07-21).** Four earlier questions are now
> decided and their reasoning has moved into the body of the doc:
> - *Inheritance ABI* → **[§1.1](#11-namespace-syscalls-sys_ns_create--sys_ns_enter)**
>   (separate `SYS_NS_CREATE`/`SYS_NS_ENTER`; `SYS_EXEC` has no free arg slot).
> - *Who may create/isolate* → **[§3.1](#31-who-may-create-or-change-a-namespace--ambient-self-and-children-only)**
>   (ambient, self-and-children only; re-pointing another task's namespace is
>   absent from the ABI, not cap-gated).
> - *`Filesystem` cap subtree* → **[§3.2](#32-subtree-scoped-filesystem-caps--deferred)**
>   (deferred; cap stays rights-only, subtree scoping lives in the namespace).
> - *Backend sharing model* → **[§5.1](#51-backend-ownership-across-namespaces--landed-for-both-layers)**
>   (global backend-id indirection table; this also **subsumes the former
>   `MountId`-uniqueness question**, which was the same identity/ownership
>   question one layer apart). **Now landed** in both layers (2026-07-21),
>   including the executor `RefCell` → single-owner convergence that was
>   previously tracked as follow-up.

The following remain open:

1. **Symlink resolution across mount points inside a namespace.** `resolve`
   (`vfs.rs:848`) follows symlinks by re-resolving absolute targets against the
   *same* table. In a restricted namespace an absolute symlink target may point
   outside every mount → it simply fails to resolve. Is "dangling because hidden"
   the desired behavior, or should it be a distinct errno?
2. **`SYS_TASK_LIST` / `SYS_PS` visibility.** Should a task be able to observe
   the *existence* of mounts/namespaces it is not in? Proposed: no — namespace
   membership is not enumerable across the boundary, matching the cap model's
   "no ambient authority, learn nothing without the handle."
3. **RAM disk / test mounts.** `MOUNT_SRC_RFS2_RAM` builds a fresh backend per
   mount; in a per-namespace world, is a RAM mount private to the namespace that
   made it (yes, by construction) — and is that the intended isolation for
   builds that need scratch beyond the one RW build dir?

---

## Non-goals (this document)

- No *namespace* kernel code — `NsId`, `SYS_NS_CREATE`/`SYS_NS_ENTER`, per-task
  namespace tables, copy-on-exec: none of that is built. (The §5.1 `BackendId`
  indirection it depends on *has* landed in `kernel/src/vfs.rs` and
  `fs/vfs-core`, and the executor's matching single-owner conversion in
  `pkg/shade-build`; those are the only code this design has shipped.)
- Not designing `OrosBuilderSandbox` (the native lowering of `SandboxPlan` to
  `SYS_MOUNT` + grants) — that is the *consumer* of this primitive and is
  tracked separately.
- Not touching the exit-code ABI, the store realize-guard semantics, or the
  capability grant/revoke algorithm.
