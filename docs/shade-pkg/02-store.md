# shade — Store, Generations, Activation

This document defines the `/shade/` hierarchy, the store path format, the
input-addressing hash (exact inputs, hash function, encoding), references and
garbage collection, generations, and the atomic activation + rollback
mechanism. Everything here is **[OS-general]** unless marked otherwise
([`01 §5`](01-overview.md#5-os-general-vs-shade-local)).

Prerequisites: [`01`](01-overview.md) for the glossary. Derivations are
produced from recipes ([`03`](03-recipe-format.md)) and sources
([`04`](04-sources.md)); this doc defines only their canonical form and
addressing.

---

## 1. The `/shade/` hierarchy {#1-the-shade-hierarchy}

The `/shade/` prefix is **reserved OS-wide** for the store services. No other
subsystem may create entries under `/shade/`. Layout:

```
/shade/
├── store/      immutable: build outputs and .drv files (§2)
├── db/         store metadata: valid set, references, deriver links (§7.2)
├── gen/        generations + `current` symlink (§5, §6)
├── roots/      explicit GC roots (§7.1)
├── cache/      fetch cache: downloaded artifacts pre-ingestion (§8)   [shade-local]
├── build/      transient build directories (per-build, §8)
└── log/        build logs, one file per store path (§8)
```

Permissions model: `/shade/store`, `/shade/db`, and the system generation line
`/shade/gen/system` are writable only by the store services
([`01 §5`](01-overview.md#5-os-general-vs-shade-local) — in v1, the `shade`
binary running with the store-write authority, privileged). Each per-user line
`/shade/gen/users/<user>` is writable by its owning user **unprivileged**
(`shade home rebuild`, [`10 §5`](10-system-prism.md#5-per-user-prisms)); building
into the shared `/shade/store` still goes through the store services. All of
`/shade/` is world-readable. `TODO(open):` the enforcement mechanism is the kernel fs
isolation gap ([`01 §6.2`](01-overview.md#6-known-system-gaps-design-time-flags));
until it exists, immutability of `/shade/store` is a convention backed only by
RFS mount options.

On-media placement: `/shade/` is a directory on the root RFS filesystem in v1
(so recorded in `docs/spec/fhs.md`'s subvolume table). `TODO(open):` whether
`/shade/` becomes its own RFS subvolume (mounted read-only with transient rw
remount for installs) once RFS v2 subvolumes are specified — see §6.3.

## 2. Store path format {#2-store-path-format}

A store path is:

```
/shade/store/<digest>-<name>-<version>          (output directory)
/shade/store/<digest>-<name>-<version>.drv      (its derivation, CDF text, §3.2)
```

- `<digest>` — 32 characters: the first 160 bits (20 bytes) of
  `BLAKE3-256(CDF bytes)` (§3.1), base32-encoded MSB-first with the **pinned
  alphabet** `0123456789abcdfghijklmnpqrsvwxyz` (Nix's alphabet — `0-9a-z`
  minus `e o t u`, so a digest never forms a word in a path), no padding.
  This is **not** RFC 4648 and not any stdlib base32; it is a frozen
  constant (`shade_cdf::BASE32_ALPHABET`) — changing it moves every store
  path. 20 bytes × 8 / 5 = exactly 32 characters, no partial group.
- `<name>` — matches `[a-z0-9][a-z0-9_-]*`, max 64 bytes. Recipe names are
  normalized to this set before hashing ([`03 §2`](03-recipe-format.md#2-package)).
- `<version>` — matches `[0-9a-z.+-]+`, max 32 bytes.
- Whole final path component ≤ 160 bytes.

The output directory and its `.drv` share one digest: the digest is computed
from the `.drv` content, and the output path is derived from it by dropping
the extension. One hash, two entries.

Store path component grammar (final component after `/shade/store/`):

```
store-name   = digest "-" name "-" version [".drv"]
digest       = 32 * base32-char
base32-char  = %x30-39 / %x61-7A         ; 0-9 a-z, minus e o t u (pinned alphabet)
```

Anything under `/shade/store/` not matching this grammar is invalid and is
deleted by `shade gc` (§7.3).

Store paths are **immutable once registered valid** (§7.2). No file under a
valid store path is ever modified; mtimes are normalized to epoch 0 and write
permission is dropped at registration ([`06 §5`](06-build.md#5-registration)).

## 3. Input-addressing {#3-input-addressing}

### 3.1 Hash function

`BLAKE3`, 256-bit output, keyed mode **not** used, no salt. Truncation to 160
bits happens only for the path digest; where a full hash is stored (lockfile,
db), all 32 bytes are kept, lowercase hex.

Rationale: BLAKE3 is fast enough to hash source trees on every resolution,
has a tree mode we can later exploit for large-file ingestion, and a single
well-defined output. `TODO(open):` no `no_std` BLAKE3 audit has been done for
the OROS target; if it fails to port, the fallback is SHA-256 — decide before
first store format freeze, because the choice is baked into every path.

### 3.2 Canonical Derivation Form (CDF) {#32-canonical-derivation-form-cdf}

A derivation is serialized to CDF — a line-based UTF-8 text format — and the
store digest is `BLAKE3(CDF bytes)` truncated per §3.1. The `.drv` file
content is exactly the CDF bytes.

Format rules:

1. Lines are `key=value`, terminated by a single LF (`0x0A`). No CR. The file
   ends with a trailing LF. Key is everything up to the **first** `=`.
2. Keys match `[a-z0-9._-]+` — **lowercase-only, no exceptions**: keys are
   hash inputs, and case-fold collisions or case-insensitive-platform
   inconsistency are unacceptable in a hashed key. Keys appear in strict
   bytewise-ascending sorted order, except line 1. Where a source value is
   uppercase by nature (env var names, §3.3), the key records its lowercase
   fold; the uppercase form is restored outside CDF.
3. Line 1 is always the format header: `shade-drv=1`. The format version is
   bumped on any change to this section; a version bump changes every hash,
   deliberately.
4. Values are percent-escaped: bytes `0x0A` (LF), `0x0D` (CR), and `0x25`
   (`%`) are encoded `%0A`, `%0D`, `%25`. All other bytes are literal. Keys
   are never escaped (their charset needs none).
5. Repeated logical lists use zero-based indexed keys (`dep.0`, `dep.1`, …)
   with a defined per-list ordering (§3.3). Indexes are decimal, no leading
   zeros.
6. No comments, no blank lines, no whitespace around `=`.

Canonicalization is total: two derivations are the same iff their CDF bytes
are identical iff their digests are equal (modulo truncation).

### 3.3 Hash inputs — the exact key set {#33-hash-inputs}

Every key that may appear in a CDF, and where its value comes from. This list
is exhaustive; adding a key requires bumping `shade-drv`.

| Key | Value | Source |
|---|---|---|
| `shade-drv` | `1` | format version (line 1) |
| `name` | normalized package name | recipe [`03 §2`](03-recipe-format.md#2-package) |
| `version` | version string | recipe |
| `system` | target triple, e.g. `x86_64-oros` | build env; identical in host-assisted mode ([`01 §6.1`](01-overview.md#6-known-system-gaps-design-time-flags)) |
| `toolchain` | toolchain identity string: `rustc-<version>-<commit-short>` | build env ([`06 §4`](06-build.md#4-environment)) |
| `sandbox` | sandbox profile version, e.g. `1` | [`06 §3`](06-build.md#3-sandbox) |
| `source.<i>.type` | `crates-io` \| `git` \| `local` \| `pspackage` | resolved source [`04 §3`](04-sources.md#3-resolution-per-source-type) |
| `source.<i>.*` | type-specific identity keys (crate+version+sha256; commit; tree hash) — always the pinned object identity, never a URL or symbolic ref; exact keys in [`04 §3`](04-sources.md#3-resolution-per-source-type) | lockfile |
| `dep.<i>` | full store path of a build-time dependency (its digest embeds *its* whole input closure — recursion does the rest) | resolution [`05`](05-dependencies.md) |
| `env.<key>` | extra build env var, literal value; `<key>` is the variable name **folded to lowercase** (§3.2 rule 2 charset is lowercase-only) — lossless, since variable names match `[A-Z_][A-Z0-9_]*` and contain no lowercase; the builder restores the uppercase name ([`06 §4`](06-build.md#4-environment)) | recipe build env ([`03 §5.3`](03-recipe-format.md#53-buildenv)) |
| `phase.<i>` | build phase command line | recipe [`03 §5`](03-recipe-format.md#5-build) |
| `output.<i>` | declared output entry | recipe [`03 §6`](03-recipe-format.md#6-outputs) |

(The former `unsafe` key is **retired** — shade no longer synthesizes builds
for recipe-less inputs, so it is never emitted and is not part of the CDF key
set, [`03 §7`](03-recipe-format.md#7-unsafe-default-recipes).)

Ordering of indexed lists: `source.*` in recipe order; `dep.*` sorted
bytewise by store path; `env.*` sorted by key (rule 2 does this
automatically); `phase.*` in execution order; `output.*` in recipe order.

Deliberately **excluded** from the hash: recipe comments and formatting
(recipes are *compiled* to CDF, not hashed raw), fetch URLs and symbolic
refs — source identity is the pinned object (content hash, commit, tree
hash), never the transport or name used to reach it
([`04 §3`](04-sources.md#3-resolution-per-source-type)) —
build parallelism, timestamps, and the building machine's identity.

Consequence of input-addressing (restated from
[`01 §4`](01-overview.md#4-relation-to-nix) because it is load-bearing):
a store path's content cannot be verified from its name. Local builds are
trusted because we ran them; any future substitution needs signatures
([`08 §6`](08-security.md#6-future-binary-substitution)).

Example CDF (illustrative values):

```
shade-drv=1
dep.0=/shade/store/c4fq3m2z7xj5kx2apwrn6uu3drhtbz3i-lythos-libstd-0.3.0
env.rustflags=-C opt-level=3
name=rkilo
output.0=bin/rkilo
phase.0=cargo build --release --offline --target x86_64-oros
phase.1=install -m755 target/x86_64-oros/release/rkilo $out/bin/rkilo
sandbox=1
source.0.sha256=9f1c2ab34c1d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e0f
source.0.type=crates-io
source.0.crate=rkilo
source.0.version=1.2.0
system=x86_64-oros
toolchain=rustc-1.86.0-adf2135f0
version=1.2.0
```

(Real files are fully sorted; `source.0.crate` sorts before
`source.0.sha256` — the example above shows the key kinds, the sort rule in
§3.2 rule 2 is normative.)

## 4. Store contents

An output directory follows the FHS-inside-the-path convention:

```
/shade/store/<digest>-<name>-<version>/
├── bin/        executables
├── lib/        libraries, rlibs ([`05 §4`](05-dependencies.md#4-crate-derivations))
└── share/      data, docs, man pages
```

Only declared outputs exist ([`03 §6`](03-recipe-format.md#6-outputs));
the builder writes to `$out` and registration verifies the declaration
([`06 §5`](06-build.md#5-registration)).

## 5. Generations {#5-generations}

A generation is one immutable snapshot of an installed set. Generations are
partitioned into **one system line** and **one line per user**
([`10 §1`](10-system-prism.md#1-the-system-prism),
[`10 §5`](10-system-prism.md#5-per-user-prisms)):

```
/shade/gen/
├── system/                the system generation line (built by `shade os rebuild`)
│   ├── 1/
│   ├── 2/
│   │   ├── manifest       what is installed and why (schema below)
│   │   ├── prism.lock      the lockfile snapshot that produced this generation
│   │   └── profile/
│   │       ├── bin/       symlink forest into /shade/store/*/bin/
│   │       ├── lib/
│   │       └── share/
│   └── current -> 2       the activation symlink (§6)
└── users/                 per-user generation lines (built by `shade home rebuild`)
    └── <user>/            one independent line per user, same layout as system/
        ├── 1/
        ├── 2/  (manifest, prism.lock, profile/)
        └── current -> 2   the user's own activation symlink
```

Each line — `system/` and every `users/<user>/` — has its **own** monotonic
generation counter, its own `current` symlink, and its own append-only history.
The lines are **independent**: a user rebuild ([`10 §5`](10-system-prism.md#5-per-user-prisms))
flips only that user's `current`, never `system/current`, and vice versa. There
is no single atomic super-generation combining them.

Generation numbers are a monotonically increasing decimal counter, never
reused, allocated at generation creation. Every operation that changes the
installed set (`install`, `remove`, `rollback` — [`07`](07-cli.md)) creates a
**new** generation; nothing edits an existing one. `rollback` to generation
*N* creates generation *M+1* whose manifest is a copy of *N*'s — history is
linear and append-only, like `git revert`, so "which generation am I on"
never requires interpreting a detached state.

`manifest` schema **[OS-general]** — the canonical line record every
`/shade/` subsystem uses (the CDF / `db/valid` discipline, §3.2 / §7.2:
header line, then lowercase `key=value` in bytewise-sorted key order, one LF
per line, trailing LF; list fields as indexed keys). No TOML anywhere under
`/shade/`. The `shade-gen=1` header is bumped on any record-shape change,
exactly as `shade-db` and `shade-drv` are. Serialization is byte-stable
(canonical writer ⇒ parse → re-serialize is the identity):

```
shade-gen=1
created=1783814400                    unix seconds; informational, NOT hashed anywhere
package.0.name=rkilo
package.0.path=/shade/store/<digest>-rkilo-1.2.0
package.0.requested=1                 1 = explicitly asked for (GC/remove semantics), 0 = dep
package.0.version=1.2.0
package.1.name=lythos-libstd
package.1.path=/shade/store/<digest>-lythos-libstd-0.3.0
package.1.requested=0
package.1.version=0.3.0
parent=1                              generation this was derived from; 0 = none
reason=install rkilo                  human-readable, set by the CLI
```

The profile is built by symlinking every file of every package's declared
outputs into the merged `profile/` tree. Collisions (two packages providing
`bin/foo`) are an **error at generation-build time**; there is no priority
system in v1. `TODO(open):` collision policy if/when two packages must
coexist (Nix's priority mechanism is the known prior art).

**Per-user profiles.** Each user has an independent profile under
`/shade/gen/users/<user>/`, built and activated by that user's own prism
([`10 §1`](10-system-prism.md#1-the-system-prism),
[`10 §5`](10-system-prism.md#5-per-user-prisms)) exactly as the system profile
above — symlink forest of the user prism's declared outputs into a `profile/`
tree, same collision rule, flipped by the same procedure (§6) scoped to
`/shade/gen/users/<user>/current`. A user's profile composes with the system
profile at use time via `PATH`, not by merging trees: the user's
`profile/bin` precedes the system `profile/bin`
([`10 §1`](10-system-prism.md#1-the-system-prism)), so a user override shadows
the system tool without a build-time collision. `TODO(open):` cross-line
collision *reporting* — a user shadowing a system binary is legal by PATH
order; whether `shade home rebuild` warns is unspecified.

**Temporary environments do not touch profiles.** `shade -t`
([`07 §2`](07-cli.md#shade-t)) — the ephemeral nix-shell-style env — builds its
packages into the store like anything else, but it **creates no generation and
mutates no profile**: it neither symlinks into any `profile/` tree nor flips
`current`. A temp env is a transient `PATH` in a subshell (§7.1 holds its store
paths only for the session); on exit the profile and generation history are
exactly as before. Nothing about the persistent profile mechanism above is
reachable from `shade -t`.

## 6. Activation and rollback {#6-activation}

### 6.1 The flip

Activation of generation *N*:

1. Build `/shade/gen/<line>/N/` completely (manifest, lock, profile), where
   `<line>` is `system` or `users/<user>`. Fsync it. Until step 3 it is
   unreferenced by that line's `current` and invisible.
2. Create symlink `/shade/gen/<line>/.current.new -> N`.
3. `rename("/shade/gen/<line>/.current.new", "/shade/gen/<line>/current")` — the flip.
4. Fsync the containing directory (forces an RFS commit; see §6.3).

`rename` over an existing symlink is atomic at the VFS level: any reader sees
the old target or the new target, never neither. Rollback is the same
procedure with a manifest copied from an older generation (§5).

Every path the rest of the system uses goes through the flip point:

- `/lth/bin` is a single symlink to `/shade/gen/system/current/profile/bin`, as
  specified in `docs/spec/fhs.md`; lythd's boot-time `/bin`, `/sbin` POSIX
  links are unchanged (they point at `/lth/bin`, which dereferences through
  `current`).
- Anything else that must be generation-consistent (service definitions,
  eventually kernel + config — the fhs.md snapshot-atomicity story) is
  reached via `/shade/gen/system/current/…` when the OS adopts the mechanism
  ([`01 §5`](01-overview.md#5-os-general-vs-shade-local)).

Processes already running keep their open files and mapped binaries (store
paths are immutable and stay alive — §7.1 roots include all generations);
only *new* lookups see the new generation. Nothing is restarted by
activation itself; service restart policy is the supervisor's business, not
the store's. **[OS-general]**

### 6.2 Boot integration

`docs/spec/fhs.md` defines a boot-time rollback protocol (rollback flag at
`/cfg/lythos/rollback`, lythd 30-second stability window). shade plugs into
it rather than replacing it:

- Before activating a generation that changes any package marked
  `boot-critical` in its manifest entry (`TODO(open):` marker definition —
  likely a recipe field, [`03 §2`](03-recipe-format.md#2-package)), shade
  writes the previous generation number into `/cfg/lythos/rollback`.
- If lythd's stability window fails, lythd re-points `/shade/gen/system/current`
  to the recorded system generation using the same flip (§6.1) and reboots.
  Boot integration is **system-line only**: per-user lines
  (`/shade/gen/users/<user>/`) are not boot-critical and never arm the flag
  ([`10 §6`](10-system-prism.md#6-boot-dependency)).
- On a clean window, lythd clears the flag.

### 6.3 RFS interaction — decision {#63-rfs-interaction}

**Decision: activation is self-contained (symlink flip), not
FS-snapshot-based.** RFS v2's COW commit machinery
(`docs/rfs-v2/04-cow-and-commit.md`) gives us exactly what the flip needs:
a commit is atomic and totally ordered, so after the step-4 fsync the flip
is durable, and a crash at any earlier point mounts to a state where
`current` still points at the old generation (COW-1/COW-3 guarantee the
rename is visible only via a committed superblock). We *lean on* RFS for
crash-atomicity of a single rename — which any correct FS must provide — and
on nothing else.

Why not subvolume snapshots (the `docs/spec/fhs.md` mechanism): RFS v2
explicitly leaves subvolume/snapshot on-disk representation unspecified
(`docs/rfs-v2/01-overview.md`, `TODO(open)` there). Building activation on an
unspecified primitive inverts the dependency order. The symlink flip is also
strictly more portable (works on any POSIX-ish FS, matters for host-assisted
mode) and makes generations first-class *data* rather than FS objects, which
the OS-general adoption path needs (a generation can reference kernel,
config, and packages in one manifest; a subvolume snapshot can only capture
one subvolume).

`TODO(open):` revisit once RFS v2 subvolumes are specified — a future
optimization may snapshot `@cfg` alongside a generation flip for the
config-rolls-back-with-system invariant, with the generation manifest
recording the snapshot ID. The flip stays the commit point either way.

## 7. Garbage collection {#7-garbage-collection}

### 7.1 Roots

The live set is the union of closures (§7.2) of:

1. Every generation's manifest store paths — all of
   `/shade/gen/system/*/manifest` **and** `/shade/gen/users/*/*/manifest`.
   (Deleting old generations — `shade os clean` / `shade home clean`, or
   explicit `generations delete` — is how store space is actually reclaimed;
   [`07 §2.1`](07-cli.md#21-shade-os).)
2. Every symlink in `/shade/roots/`. Anyone may root a path by symlinking it
   here; the symlink name is `<owner>-<label>` by convention, content is the
   store path. Dangling symlinks are pruned by GC. **[OS-general]**
3. In-flight builds: every store path referenced by a derivation currently
   being built (the build lock registry under `/shade/db/locks/`, held for the
   duration of a build — [`06 §5`](06-build.md#5-registration)).
4. **Temporary environments:** every store path a live `shade -t` session
   ([`07 §2`](07-cli.md#shade-t)) depends on, held **only** for the session's
   lifetime. The mechanism (resolved): when a temp env starts, `shade -t`
   writes a GC root symlink **`/shade/roots/tmp-<pid>`** (`<pid>` = the temp
   env's subshell/process-tree root PID) pointing at each selected output; the
   root is a §2 `/shade/roots/` entry like any other, so `gc`'s existing
   mark phase (rule 2) keeps the closure live with no special-casing. On exit
   the session **removes its own `tmp-<pid>` root** — so no persistent root and
   **no generation** ([`02 §5`](02-store.md#5-generations)) ever result from a
   temp env. A stale `tmp-<pid>` left by a crashed session is a dangling entry
   pruned like any other under `/shade/roots/` (rule 2): `TODO(open):` whether
   `gc` additionally cross-checks `<pid>` liveness to reclaim a crashed
   session's paths before the next natural prune — deferred, low-stakes since a
   dangling symlink is already handled.

`TODO(open):` roots for *running* processes. OROS has no enumeration of
which store paths live processes have open/mapped. Because rule 1 keeps all
generations live until explicitly deleted, this only bites for a process
started from a generation that is deleted while it runs. v1 accepts that
window; document in [`07`](07-cli.md) that `generations delete` +
`gc` can pull binaries out from under long-running processes' future
`exec`s (already-mapped pages are unaffected).

### 7.2 References {#72-references}

At registration ([`06 §5`](06-build.md#5-registration)) the store services
scan every file in the new output for the byte pattern `/shade/store/` followed
by a valid digest (§2 grammar) and record the found set in the db:

```
/shade/db/refs/<digest>        LF-separated list of referenced store digests
/shade/db/valid/<digest>       registration record: full BLAKE3 (untruncated),
                           registration time, deriver digest
```

The closure of a path = transitive union over `/shade/db/refs/`. The `.drv` of a
path references all its `dep.*` and source paths; output references are
whatever the scan found. Scanning (rather than trusting declarations)
catches paths embedded by the compiler, e.g. in panic messages or
`env!`-captured values. **[OS-general]**

`/shade/db/` is a plain directory-of-files database in v1 — no binary format, no
locking beyond an exclusive `flock`-equivalent on `/shade/db/lock` for mutations.
The OROS VFS exclusive-create primitive backing this is `SYS_CREATE`: atomic
create-if-absent, exactly one winner among concurrent creators, losers get
`EEXIST`, release via `SYS_UNLINK` (`docs/spec/syscalls.md`, SYS_CREATE
exclusive-create guarantee — resolved 2026-07-13, was `TODO(open)`).

### 7.3 Sweep

`shade gc`:

1. Take the db lock; refuse if builds are in flight unless `--force`.
2. Mark: union of closures of §7.1 roots.
3. Sweep: every `/shade/store/` entry not marked — plus every entry violating
   the §2 grammar — is deleted; its `/shade/db/refs/` and `/shade/db/valid/` records
   go with it. `/shade/cache/` entries are deleted by age/size policy
   ([`07`](07-cli.md) `gc` flags); cache entries are never roots.
4. Deletion order within the sweep is unordered — references among dead
   paths don't matter, and a crash mid-sweep leaves only dead paths behind,
   which the next GC re-collects. (Store immutability + the mark set being
   computed under the lock make this safe.)

## 8. Non-durable areas

`/shade/build/<digest>/` — one transient directory per build, deleted on success
after registration, kept on failure for inspection (deleted by next `gc`).
`/shade/cache/` — fetched artifacts keyed by content hash, see
[`04 §4`](04-sources.md#4-fetch-cache). `/shade/log/<store-name>.log` — build
log per store path, kept until its store path is GC'd. All three are
**[shade-local]**; nothing else may depend on their layout.
