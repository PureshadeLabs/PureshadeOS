# rpkg — Isolated Build Model

How a derivation ([`02 §3`](02-store.md#3-input-addressing)) becomes a valid
store path: the phase skeleton, the sandbox contract, the build environment,
registration, and determinism goals. The sandbox contract is **[OS-general]**
(a capability profile any supervisor can grant); the phase skeleton and
registration procedure are **[rpkg-local]** policy on top of it
([`01 §5`](01-overview.md#5-os-general-vs-rpkg-local)).

Two execution modes share this spec: **native** (build task on OROS) and
**host-assisted** (cross-build on the dev host during bringup,
[`01 §6.1`](01-overview.md#6-known-system-gaps-design-time-flags)). The
derivation, store path, and registration are identical in both; only §3's
enforcement mechanism differs (host-assisted mode *approximates* the sandbox
and is documented as weaker, [`08 §5`](08-security.md#5-sandbox-guarantees)).

---

## 1. Inputs and outputs of one build

A build consumes exactly:

- the `.drv` (CDF) being built,
- the store paths named by its `dep.*` and `source.*`-derived entries — all
  already valid,
- the fixed environment (§4) and toolchain named by `toolchain`.

and produces exactly:

- the output directory at the predetermined store path,
- a build log at `/r/log/<store-name>.log`,
- db records at registration (§5).

Nothing else. No network, no clock-dependent behavior (§6), no reads outside
the input set, no writes outside `$out` and the build dir. That's the whole
contract; §3 is about enforcing it, §6 about how close reality gets.

## 2. Phases {#2-phases}

Fixed skeleton around the recipe's `phases` array
([`03 §5`](03-recipe-format.md#5-build)):

| # | Phase | Actor | What happens |
|---|---|---|---|
| 1 | fetch | store services, **outside** sandbox | source derivations realized ([`04 §2`](04-sources.md#2-source-derivations)); network allowed here and only here |
| 2 | setup | store services | create `/r/build/<digest>/` (§3 fs layout), copy source trees in (writable working copy; store paths stay immutable), stage env (§4) |
| 3 | build | **sandboxed builder task** | run each `phases[i]` command in order, cwd = build dir, argv semantics per [`03 §5.1`](03-recipe-format.md#51-phases); non-zero exit fails the build at that phase |
| 4 | fixup | store services | normalize: mtimes → epoch 0, strip write bits; `TODO(open):` debug-info stripping and store-path rewriting are not in v1 (no dynamic linker on OROS yet to motivate rpath-style fixup) |
| 5 | register | store services | §5 |

The split matters for the trust story: phases 1–2 and 4–5 are store-services
code operating on verified data; only phase 3 runs derivation-controlled
commands, and only phase 3 is sandboxed-as-untrusted
([`08 §5`](08-security.md#5-sandbox-guarantees)).

## 3. Sandbox {#3-sandbox}

### 3.1 Contract (sandbox profile `1`)

The `sandbox` CDF key ([`02 §3.3`](02-store.md#33-hash-inputs)) names this
contract version. Profile `1`:

**Filesystem** — the builder task may:

- read: its build dir, the store paths in the derivation's input set
  (deps + sources), the toolchain closure, and nothing else — notably *not*
  all of `/r/store/` (undeclared-dep hiding is the point; a build that reads
  a store path not in its inputs must fail, or input-addressing is fiction);
- write: its build dir and `$out` only;
- see no `/user`, no `/cfg`, no `/var`, no other builds' dirs.

**Process/IPC** — spawn children (compilers do); no IPC endpoints except the
supervisor-granted log/control endpoint; no capability to reach `lythmsg` or
any system daemon.

**Network** — none. Fetch already happened (§2 phase 1).

**Resources** — memory cap and task-count cap set by the supervisor;
`TODO(open):` values and enforcement mechanism (kernel `Memory` capability
today grants the whole PMM pool — the planned range-restricted Memory caps
in `docs/spec/capabilities.md` are a prerequisite for a real memory cap).

**Environment** — exactly §4, nothing inherited.

### 3.2 Mechanism on OROS

Target mechanism: the builder is a task spawned by the store services with a
minimal capability set — restricted `Memory` cap, one `Ipc` cap to its
supervisor, no `Device`, no `Rollback` — per `docs/spec/capabilities.md`
no-ambient-authority model.

**Known gap, restated from [`01 §6.2`](01-overview.md#6-known-system-gaps-design-time-flags):**
the kernel has no filesystem capability kind and no per-task fs namespace,
so the *filesystem* rows of the contract are currently unenforceable on
OROS. `TODO(open):` kernel design work — candidate shapes: (a) a `File`/
`Path` capability kind checked in VFS syscalls, (b) per-task root + bind-ish
mounts, (c) VFS-daemon-mediated fs access where the daemon holds policy.
The sandbox *contract* above is written to be satisfiable by any of the
three; rpkg must not depend on which one lands. Until it lands, native
builds are honor-system on fs isolation and `sandbox=1` overstates reality —
acceptable for bringup, tracked in [`08 §5`](08-security.md#5-sandbox-guarantees).

Host-assisted mode: best-effort approximation (dedicated build user or
host sandbox facility; out of scope to specify — the host is trusted
infrastructure during bringup by definition).

## 4. Environment {#4-environment}

Fixed base environment, in full — the builder sees these and the recipe's
`[build.env]` ([`03 §5.3`](03-recipe-format.md#53-buildenv)) and **nothing
else**:

| Var | Value |
|---|---|
| `PATH` | `bin/` dirs of the derivation's build deps + toolchain, in `dep.*` order |
| `HOME` | `/homeless` (nonexistent path; anything reading `$HOME` should fail loudly) |
| `TMPDIR` | the build dir's `tmp/` subdirectory |
| `SOURCE_DATE_EPOCH` | `0` |
| `TZ` | `UTC` |
| `LANG` / `LC_ALL` | `C.UTF-8` |
| `TARGET` | the `system` value |
| `JOBS` | supervisor-chosen parallelism (not hashed) |
| `OUT` / `$out` | the output store path |
| `SRC0…` / `$src<i>` | source store paths |

Recipe env vars may not override this table
([`03 §5.3`](03-recipe-format.md#53-buildenv)). The toolchain identity string
hashed as `toolchain` ([`02 §3.3`](02-store.md#33-hash-inputs)) is defined
as: `rustc-<semver>-<first 9 hex of rustc commit hash>`, taken from
`rustc --version --verbose` of the toolchain in `PATH`. `TODO(open):` once
the toolchain is itself a store package, `toolchain` should be replaced by
the toolchain's store path in `dep.*` and dropped as a separate key —
CDF v2 candidate, don't freeze v1 without deciding.

## 5. Registration {#5-registration}

On phase-3 success, under the db lock ([`02 §7.2`](02-store.md#72-references)):

1. Verify declared outputs exist (and only declared top-level dirs exist)
   per [`03 §6`](03-recipe-format.md#6-outputs); executables in `bin/` must
   be valid OROS ELF for `system` (`TODO(open):` exact validation depth —
   magic + machine type at minimum).
2. Fixup normalization already applied (§2 phase 4).
3. Reference-scan the outputs ([`02 §7.2`](02-store.md#72-references)); every
   found reference must be in the derivation's input closure ∪ `$out`
   itself — a reference to anything else means an undeclared input leaked in
   and **fails the build** (this backstops §3's fs gap: even an unsandboxed
   build can't silently *embed* undeclared store paths).
4. Move the output from the build staging location to the final store path
   (same-FS rename), fsync, write `/r/db/refs/<digest>` and
   `/r/db/valid/<digest>`.
5. Release the build lock, delete the build dir.

A crash before step 4's records leaves an unregistered directory that the
next GC removes ([`02 §7.3`](02-store.md#7-garbage-collection)); a crash
after leaves a valid path. There is no in-between because `valid` is a
single file creation on RFS (atomic at commit granularity,
[`02 §6.3`](02-store.md#63-rfs-interaction)).

## 6. Determinism {#6-determinism}

Goal, stated precisely: **bit-identical outputs for identical derivations**
is aspirational, not load-bearing. Input-addressing means correctness never
depends on it — the store path is fixed by inputs, and a path is built at
most once per machine (build lock, [`05 §6`](05-dependencies.md#6-build-order-and-scheduling)),
so two differing rebuilds never coexist locally. Nondeterminism only starts
to *matter* when substitution arrives ([`08 §6`](08-security.md#6-future-binary-substitution)),
where it becomes a trust problem.

What v1 does enforce (cheap, catches the common leaks):

- fixed env (§4): epoch, TZ, locale;
- offline builds: no time-of-fetch variance;
- mtime + permission normalization (§2 phase 4);
- `$JOBS` excluded from the hash but parallelism-dependent output is a
  recipe bug — `TODO(open):` a `rpkg build --check` mode (rebuild + diff)
  to detect it, deferred.

What v1 explicitly tolerates: rustc nondeterminism across *machines*
(codegen-unit scheduling etc.) — irrelevant until substitution, revisit then.
