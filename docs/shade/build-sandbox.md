# shade — Build Sandbox (`SeatbeltSandbox`)

The **host macOS-Seatbelt** implementation of the executor's `BuildSandbox`
seam ([build-executor.md §2.1](build-executor.md#21-seam-a--buildsandbox)):
`pkg/shade-build/src/sandbox.rs`. It enforces the sandbox profile `1`
contract ([shade-pkg 06 §3.1](../shade-pkg/06-build.md#31-contract-sandbox-profile-1))
that `PermissiveSandbox` only names — but via macOS Seatbelt, **not** Lythos
capabilities. The real `SYS_MOUNT` + capability-enforced native OROS sandbox
that will consume the same `SandboxPlan` is a separate, **as-yet-unwritten**
impl; `SeatbeltSandbox` does not stand in for it. The executor is unchanged —
this is one impl of the same trait, selected by the caller.

**Verified gate** (`pkg/shade-build/src/tests.rs`):
`seatbelt_sandbox_rebuild_is_byte_identical` — the same derivation built twice
yields byte-identical output trees; `seatbelt_sandbox_denies_network` and
`seatbelt_sandbox_denies_out_of_tree_read` — a builder attempting network
access or an undeclared read fails, with a `PermissiveSandbox` control build
proving the denial came from the sandbox, not the recipe.

---

## 1. Isolation model: plan vs. vehicle

Two layers, split so the contract outlives the enforcement mechanism:

- **`SandboxPlan` — the pure model.** From a `SandboxSpec` it computes the
  builder's entire world: the mount list (filesystem view in `SYS_MOUNT`
  terms), the minimal capability grant set (`lythos-abi` `CapKind` + rights
  bits), and the complete environment. Its `check_read` / `check_write` /
  `check_network` answer in **ABI errnos**, exactly as the Lythos syscall
  boundary will. Same spec ⇒ same plan, byte for byte (mount order, env
  order, profile text). This layer is host-independent and is what the
  native OROS builder task gets constructed from when the kernel grows a
  per-task fs namespace (06 §3.2's open kernel design).
- **The host vehicle** enforces the plan while builds still run host-assisted
  (06 intro): the plan compiles to a macOS Seatbelt (SBPL) profile and every
  phase runs as `/usr/bin/sandbox-exec -p <profile> /bin/sh -c "umask 0022;
  <phase>"` with a **cleared** environment (`env_clear` + the plan's table,
  nothing inherited). Denials surface inside the builder as host `EPERM` —
  the host spelling of the plan's `ENOPERM`.

Construction is **fail-closed**: `SeatbeltSandbox::new()` errors on a host
with no enforcement facility (non-macOS, or `sandbox-exec` missing). There
is no silent fallback — the permissive impl already exists and is honestly
named.

## 2. Filesystem view

Modeled as mounts (the plan's `MountPlan` list), stable order — declared
inputs in `dep.*` order, the writable build dir last:

| Target (builder namespace) | Source | Rights |
|---|---|---|
| `/shade/store/<input-store-name>` (one per declared input) | the realized input path | `RIGHT_READ` |
| `/shade/build/<drv-store-name>` | the scratch dir (contains `tmp/` and the `$out` staging `out/`) | `RIGHT_READ \| RIGHT_WRITE` |

Nothing else from the store — or the host — is in the view. Undeclared-dep
hiding is the point (06 §3.1): a sibling store path is exactly as invisible
as `/Users`. The plan answers:

| Access | Errno |
|---|---|
| read under a declared input or the build dir | ok |
| write under the build dir (incl. `$out` staging, `tmp/`) | ok |
| write to a declared input (read-only mount) | `EROFS` (-14) |
| read or write anywhere else | `ENOPERM` (-3) |
| any network operation | `ENOPERM` (-3) |

**Output confinement** has two halves: the profile confines all writes to
the build dir, and `collect_outputs` additionally rejects a staging tree
containing an undeclared top-level entry (06 §5 step 1 — the anti-smuggling
check), so nothing undeclared can ride a realize into the store. It surfaces
through the executor as `MissingOutput`/`ENOENT`.

## 3. Capability set — exact and complete

The builder task's grants, verbatim from `SandboxPlan::caps`
(bits from `abi/lythos-abi/src/cap.rs`):

| Kind | Rights | Role |
|---|---|---|
| `CapKind::Filesystem` | `RIGHT_READ` (1) | one per declared input store path (read-only mount) |
| `CapKind::Filesystem` | `RIGHT_READ \| RIGHT_WRITE` (3) | the build dir: scratch + `$out` staging + `tmp/` |
| `CapKind::Ipc` | `RIGHT_WRITE` (2) | the supervisor-granted log endpoint (06 §3.1) — on the host, the inherited log fd |

That is the whole set. No `Device`, no `Rollback`, no `Memory` grant beyond
the task default, no `Ipc` reach to `lythmsg` or any daemon — and **no grant
carries `RIGHT_GRANT` or `RIGHT_REVOKE`**, so the builder can neither
delegate nor tear down what it was given. Asserted by
`sandbox::plan_tests::cap_set_is_minimal_and_named`.

## 4. Determinism knobs — every one

| Knob | Pinned value |
|---|---|
| environment base | **cleared** (`env_clear`); the builder sees the table below and the recipe `env`, nothing else |
| `PATH` | input `bin/` dirs in `dep.*` order + the fixed host tool tail `/usr/bin:/bin` (escape hatch §6.1; the native plan drops the tail) |
| `HOME` | `/homeless` (nonexistent — anything reading it fails loudly, 06 §4) |
| `TMPDIR` | `<scratch>/tmp` |
| `SOURCE_DATE_EPOCH` | `0` |
| `TZ` | `UTC` |
| `LANG` / `LC_ALL` | `C.UTF-8` (per 06 §4; supersedes the looser `LC_ALL=C` phrasing elsewhere) |
| `TARGET` | the CDF `system` value |
| `JOBS` | supervisor-chosen, never hashed (06 §4) |
| `OUT` / `out` | the `$out` staging dir |
| umask | `0022`, set by the phase wrapper before the recipe command |
| cwd | the scratch dir, every phase |
| build identity | fixed `BUILD_UID`/`BUILD_GID` = 30001/30001 in the model (host gap: §6.4) |
| mount ordering | inputs in `dep.*` order, writable build dir last — part of the plan's byte-determinism |
| recipe `env` collisions | the fixed table **wins** (03 §5.3): recipe vars are placed first, fixed vars last, and the later duplicate is what the process receives |

## 5. Threat surface

What the sandbox is defending, and against whom: phase 3 runs
derivation-controlled commands — arbitrary code (06 §2). The assets are
(a) the CDF byte-identity guarantee (a build must not read undeclared
inputs or nondeterministic state, or input-addressing is fiction), (b) the
store's integrity (nothing writes it except realize), and (c) the host/user
environment (secrets, other builds, the network).

| Vector | Standing |
|---|---|
| read undeclared store path / host file | **denied** — profile deny-default; test-verified |
| write outside build dir (store, host, other builds) | **denied** — single writable subpath; test-verified |
| network (sockets, `/dev/tcp`) | **denied** — `(deny network*)`; test-verified |
| env leakage (host `PATH`, `USER`, agent sockets…) | **closed** — `env_clear`; test-verified |
| smuggle undeclared tree into `$out` | **rejected** at `collect_outputs`; test-verified |
| embed an undeclared store path in output bytes | backstopped by the registrar's reference scan (06 §5 step 3), not the sandbox |
| IPC to system daemons | closed on the host (deny-default covers mach lookup); on OROS: no `Ipc` grant beyond the log endpoint |
| resource exhaustion (memory, task bombs) | **open** — 06 §3.1 resource rows still `TODO` (blocked on range-restricted Memory caps) |

## 6. Non-hermetic escape hatches — explicit list

1. **Host shell runtime reads.** The profile allows reading `/System`,
   `/usr/lib`, `/usr/share`, `/usr/bin`, `/bin`, `/private/var/select`,
   `/private/etc`, `/Library/Preferences`, `/dev` — dyld, libSystem,
   `/bin/sh`, coreutils. Plus the `PATH` tail `/usr/bin:/bin`. Builds may
   therefore vary across *host OS versions* (same machine ⇒ identical; the
   gate is per-machine byte-identity, matching 06 §6's stance). The native
   OROS plan has no such tail — tools come from declared inputs only.
2. **Metadata oracle.** `(allow file-read-metadata)` is global: the builder
   can stat/probe existence of any host path (contents stay denied). Needed
   by dyld and `getcwd`; an information leak, not an integrity leak.
3. **`/dev` reads** include `/dev/urandom` — a recipe that consumes
   randomness builds nondeterministically. Same class as `$RANDOM`; a recipe
   bug, not prevented in v1 (06 §6).
4. **Build identity.** uid/gid cannot change without privilege on the host;
   phases run as the invoking user. `BUILD_UID`/`BUILD_GID` are model
   values, enforced only when the OROS builder task lands.
5. **Staging-path embedding.** `$out` is the staging dir; a binary that
   embeds it embeds a non-store path (pre-existing, build-executor.md §1
   `TODO`, unchanged here).
6. **mtimes.** The fixup phase (06 §2 phase 4, mtime→epoch-0 normalization)
   is still deferred; the byte-identity gate compares file contents, not
   timestamps.
7. **Vehicle longevity.** `sandbox-exec` is deprecated-but-functional Apple
   tooling (the same facility Nix uses on darwin). Non-macOS hosts have no
   vehicle at all — `SeatbeltSandbox::new()` fails closed there.

## 7. Errno map

| Event | Model (ABI) | Host vehicle spelling |
|---|---|---|
| out-of-tree read | `ENOPERM` (-3) | builder sees `EPERM`; phase exits nonzero → `PhaseFailed`/`EINVAL` at the executor |
| write to read-only input mount | `EROFS` (-14) | `EPERM` in-builder, same executor fold |
| network attempt | `ENOPERM` (-3) | `EPERM` in-builder, same executor fold |
| undeclared staged output | — | `MissingOutput` → `ENOENT` (-5) |

The model errnos are the contract the OROS syscall boundary will return
directly (`SYS_OPEN`/`SYS_WRITE` cap checks, `SYS_MOUNT` fold in
`docs/plans/mount-syscall-shade-store.md` §3.5); the host vehicle can only
approximate them through the builder's own libc.
