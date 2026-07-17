# shade — Generations, Profiles, and Activation

How installed sets become **generations** (`/shade/gen/`), how per-user
**profiles** relate to system generations, how switch/rollback stay **atomic**
(one symlink flip), how a **system generation** is built from a prism and
activated at **boot** without ever building, and how every generation is a
**GC root**. This documents the implementation in `pkg/shade-gen` (the
`GenLine` engine, the prism→package-set driver, and the host `shade-gen`
binary); the *policy* it implements is
[`shade-pkg 02 §5–6`](../shade-pkg/02-store.md#5-generations) (generations,
activation, rollback) and [`10`](../shade-pkg/10-system-prism.md) (system
prism, pointer, boot dependency).

Current vehicle: the **host** `shade-gen` binary
(`pkg/shade-gen/src/bin/shade-gen.rs`), alongside `shade-build` and `shade-gc`
— the seed model of [`shade-pkg 09 §2`](../shade-pkg/09-bootstrap.md). The
engine itself is `no_std + alloc` over the injected `StoreFs` seam (B1):
every generation/profile/symlink/pointer operation goes through the backend
(`HostFs` on the host, `OrosFs` over the raw Lythos syscalls), and the crate
builds for the OROS target (`--features oros`). The OROS `shade` binary stays
a stub until an OROS `EvalIo` exists (`pkg/shade/src/main.rs`); on OROS these
verbs become `shade os rebuild`, `shade home rebuild`, `shade generations`,
`shade rollback` ([`07 §2`](../shade-pkg/07-cli.md#2-commands)).

---

## 1. Profiles vs generations — the model

A **generation** is one immutable snapshot of an installed set: a numbered
directory that is never edited after creation. A **profile** is a *line* of
generations plus the `current` symlink selecting one of them — the thing a
user (or the system) actually runs out of. Concretely (02 §5):

```
/shade/gen/
├── system/                     the system line (privileged; `shade os rebuild`)
│   ├── 1/
│   ├── 2/
│   │   ├── manifest            what is installed and why (record format below)
│   │   ├── prism.lock          the lock snapshot that produced it
│   │   └── profile/            symlink forest into /shade/store/*
│   │       ├── bin/ …
│   └── current -> 2            the activation symlink (§3)
└── users/<user>/               one independent line per user (unprivileged;
    └── … same layout …          `shade home rebuild`), own counter, own current
```

**The manifest record.** No TOML anywhere under `/shade/` — the manifest uses
the **same canonical record format as every sibling subsystem** (the CDF /
`db/valid` line discipline, [store-db-gc §1.1](store-db-gc.md#11-validdigest--the-registration-record)):
a header line first, then lowercase `key=value` lines in **bytewise-sorted**
key order, one LF per line, trailing LF. List fields are indexed keys, exactly
as CDF's `dep.<i>`/`phase.<i>`. The header `shade-gen=1` is bumped on any
record-shape change (as `shade-db` and `shade-drv` are).

```
shade-gen=1
created=1783814400                    unix seconds (like db `registered`); informational, never hashed
package.0.name=alpha
package.0.path=/shade/store/<digest>-alpha-1.0
package.0.requested=1                 1 = explicitly asked for, 0 = pulled in as a dep
package.0.version=1.0
parent=1                              generation this was derived from; 0 = none
reason=os rebuild /user/lyon/.prism   human-readable, set by the CLI
```

Serialization is **byte-stable**: the writer is canonical (deterministic key
order, single-line values), so parse → re-serialize reproduces the on-disk
bytes exactly — the same byte-identity discipline CDF and the db records
follow. The 02 §5 schema fields are unchanged; only the encoding is (`created`
is unix seconds rather than a timestamp string, `requested` is `0`/`1` —
normalizations of the former format's type-isms to the canonical form's
line vocabulary).

`GenLine` (`pkg/shade-gen/src/lib.rs`) is one such line — `GenLine::system`
or `GenLine::user`. Each line has its **own** monotonic counter (allocated at
creation, never reused) and its own append-only history; flipping one line
never touches another (10 §5 — a user rebuild is not folded into the system
generation).

**The profile tree.** `N/profile/` is a symlink forest: every file of every
package's output tree becomes a symlink to its absolute store path;
directories merge. Two packages providing the same file is an **error at
generation-build time** (02 §5 — no priority v1); the failed generation never
appears in the line. Cross-line shadowing (user over system) is PATH order at
use time, never a tree merge (10 §1.1).

**Creation ≠ activation.** `GenLine::create` builds the numbered directory in
a sibling temp dir (manifest, lock snapshot, forest), fsyncs it, and renames
it into place — so a numbered directory either exists complete or not at all,
and nothing is live until the separate flip (§3). Concurrent creators race on
the number; the rename loser retries with the next one.

## 2. System generations from the prism

A **system generation** is built from the system prism — entry file always
`prism.shade` (10 intro). `os_rebuild` (`pkg/shade-gen/src/prism.rs`) drives
the 07 §2.1 pipeline:

1. **Resolve the source** (10 §4): an explicit `<prism>[#<selector>]` argument
   wins; else the pointer (`/cfg/shade/current.pointer`, lines 1–2) is
   authoritative; else `.bak`, else the live bootstrap default
   `/cfg/shade/prism.shade`. A pointer whose target is missing **fails loud**
   — never a silent `.bak` fallback.
2. **Evaluate + select**: import the entry file once, apply the
   [`shade 08 §4`](08-interop.md#4-package-set-selection) selection — the
   `packages` attrset (or the value itself; a bare derivation is a singleton),
   `#a.b.c` navigating nested sets, no selector meaning `default` if present
   else every member.
3. **Build** each selected package LOOKUP-THEN-BUILD through the shade-build
   executor (one shared evaluation; `shade_build::plan_value` +
   `Executor::run_graph`), registering realizations in `/shade/db/`
   ([store-db-gc §2](store-db-gc.md)).
4. **Create + activate** the new generation in `/shade/gen/system/` (§1, §3).
5. **Retire the default** on the first explicit rebuild:
   `/cfg/shade/prism.shade → prism.shade.bak`, one-way (10 §3).
6. **Rewrite the pointer last** — prism path, selector, and the **pinned
   generation number** (10 §2) — so any failure above leaves the previous
   pointer and its still-live generation intact.

`home_rebuild` is the per-user analog: same build, `GenLine::user(<user>)`,
flip only that user's `current`. No pointer, no `/lth/bin`, no system line
(10 §5).

## 3. Activation and rollback — the atomic flip

Activation of generation *N* is exactly the 02 §6.1 procedure, and **defines
what "activate" touches**:

1. *N* must be complete (manifest + profile present — `create` guarantees
   this; `activate` refuses otherwise).
2. Symlink `.current.new -> N`.
3. `rename(".current.new", "current")` — the flip. Atomic at the VFS level:
   any reader sees the old generation or the new one, never neither, never a
   partial.
4. Fsync the line directory (the RFS commit point, 02 §6.3).

That is the whole switch — nothing else in the line moves. The **live system
view** is one additional, separately-idempotent symlink outside the line:
`/lth/bin -> /shade/gen/system/current/profile/bin` (`GenLine::wire_view`;
`docs/spec/fhs.md`). Because it dereferences *through* `current`, wiring it
once suffices; subsequent flips retarget it for free. Per-user lines have no
system view — their profile composes via PATH at session start (10 §1.1).
Re-activating the current generation re-runs the same flip: idempotent.

**Rollback** (`GenLine::rollback`) creates a **new** generation whose manifest
and lock copy the target's (default: the generation before `current`), then
activates it. History stays linear and append-only — like `git revert`, never
a detached state; rollback twice returns to where you started. The rollback
generation registers its roots like any other (§5).

**Re-pin on system rollback.** The pointer's line 3 pins what boot activates
(§4). A system-line rollback re-pins it to the rollback generation
(`repin_generation`, lines 1–2 untouched) — otherwise the next boot would
silently return to the pre-rollback configuration. This is spec'd in
[`10 §2`](../shade-pkg/10-system-prism.md#2-the-pointer-file): `shade rollback`
rewrites pointer line 3, the single documented exception to "only `shade os
rebuild` rewrites the pointer." Because the rollback generation is pre-built,
boot's no-build invariant (§4, 10 §6) is preserved.

## 4. Boot activation — pre-built only

**Boot always activates a pre-built generation and never builds** (10 §6).
`boot_activate(shade_root, cfg_root, lth_bin)` enforces this **by type**: it
takes no evaluator, no builder, no recipe — a boot-time build is
unrepresentable, not merely forbidden. Order:

1. **Pointer present** → activate its pinned generation (line 3) if complete.
2. **Pinned generation missing/corrupt** → activate the newest *complete*
   generation — the last-good recovery of 02 §6.2 / 10 §6. Never `.bak`,
   never re-reading a prism.
3. **Pointer absent** → whatever `current` already points at, else the newest
   complete generation.
4. **Nothing built** → error. Even with a source prism sitting in
   `/cfg/shade/`, boot fails rather than builds.

The flip itself is §3's — idempotent, so re-running boot activation is safe —
plus wiring `/lth/bin` when a link path is given. The lythd stability-window
integration (rollback flag at `/cfg/lythos/rollback`, 02 §6.2) is deferred
with lythd's adoption of the mechanism; the last-good fallback above is the
engine-level half it will drive.

## 5. Generations are GC roots

Two independent mechanisms keep a generation's closure alive
([store-db-gc §3](store-db-gc.md#3-roots--the-live-set)):

- **The roots API** (this layer): `GenLine::create` registers one direct root
  per package — `/shade/roots/gen-<line>-<N>-<i> -> <store path>` via
  `StoreDb::add_root` (the `<owner>-<label>` convention). Deleting a
  generation's records is what will remove them (generation deletion —
  `shade generations delete` / `os clean` — is deferred, §6).
- **The gen-tree byte scan** (the GC's side): every store digest embedded
  under `/shade/gen/` — manifest `package.<i>.path` entries and the profile forest's
  symlink targets — is a root (store-db-gc §3.3).

Both over-approximate, which is the direction GC safety needs; either alone
suffices, and the redundancy means neither layer's bug can collect a live
generation. Store paths referenced only by *deleted* generations become
unreachable and are reclaimed by the next `shade gc`.

## 6. Command surface / what is deferred

Host `shade-gen` verbs (07 §2 flattened): `os-rebuild [<prism>[#<sel>]]`,
`home-rebuild <prism>[#<sel>] --user U`, `list [--user U]`, `rollback [N]
[--user U]`, `boot`. `--shade-root/--cfg-root/--build-root/--log-root`
repoint everything for tests and bringup; production paths are the canonical
`/shade`, `/cfg/shade`, `/lth/bin`.

Deferred:

- **Generation deletion** (`shade generations delete`, `os clean`,
  `home clean` — 07 §2): record removal + root de-registration + gc trigger.
  The append-only history model makes this purely subtractive.
- **`generations diff`** (07 §2) — package-level diff; the manifests carry
  everything it needs.
- **Prism `inputs` resolution** — the seed accepts prisms whose packages need
  no input fetching (the fetcher is deferred with shade-pkg 06 §2 phase 1);
  `outputs`-as-function evaluation lands with it.
- **Owner HM coupling** — `shade os rebuild` activating the owner's user line
  alongside the system line (10 §1); today the two rebuilds are separate
  invocations of the same engine.
- **lythd boot integration** — PID-1 calling the boot activation path and the
  stability-window flag (02 §6.2); on OROS, `boot_activate` is the function it
  calls.
- **On-device activation** — **verified** on the B-series end-to-end gate
  (the `shade e2e` bringup probe): generation create, `activate`, live-view
  resolution through `current`, rollback flip, and `boot_activate`'s no-build
  path all run under QEMU. Symlink create/read landed
  (`SYS_SYMLINK`/`SYS_READLINK`), so the profile forest, `current`, and
  `/lth/bin` wiring work on target; `OrosFs::symlink`/`read_link` are live.
  The rebuild drivers (`os_rebuild`/`home_rebuild`) still stay behind the
  `std` feature — they drive the executor's host-only run loop; the on-target
  `shade` binary drives the seam pieces directly (eval → address → realize →
  generation) with a bringup phase interpreter instead. One seam nuance: on a
  backend whose `rename` is no-replace (OROS RFS), the `current` flip degrades
  to unlink+rename — same fallback the store db uses for its records; the host
  backend renames over the destination atomically.
- **Cross-power-cycle persistence** — the `/shade/store` mount is RAM-backed
  and volatile by design (content-addressed), so a real reboot loses the store
  bits while `/shade/gen` (root volume) persists. `boot_activate`'s no-build
  property is structural (it takes no evaluator/builder — a boot-time build is
  unrepresentable) and is verified in-session; a persistent on-disk store is a
  separate future step.
