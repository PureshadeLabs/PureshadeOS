# shade — Build Executor

How `shade build` turns an evaluated derivation closure into realized store
paths: the CDF → run → realize pipeline, the two replaceable seams (sandbox,
registrar), build ordering, and failure semantics. This documents the
implementation in `pkg/shade-build` (module `executor`); the *policy* it
implements is [`shade-pkg 06`](../shade-pkg/06-build.md) (isolated build
model) and [`shade-pkg 02 §2–3`](../shade-pkg/02-store.md) (store paths,
input-addressing).

Current vehicle: the **host** `shade-build` binary
(`pkg/shade-build/src/bin/shade-build.rs`), per the seed model in
[`shade-pkg 09 §2`](../shade-pkg/09-bootstrap.md#2-seed-shadec). The OROS
`shade` binary stays a stub until argv is plumbed through the ABI and an
OROS `EvalIo` exists (see `pkg/shade/src/main.rs`).

---

## 1. Pipeline

```
recipe (.shade | expr)
  │  shadec eval — forcing drvPath emits every derivation in the
  │  closure as canonical CDF bytes (shade 05 §3)
  ▼
PlanGraph { root plan, canonical drvPath → CDF bytes }
  │  topological order over dep.* refs (§3)
  ▼
per derivation, in order:            LOOKUP-THEN-BUILD (never build-first)
  ├─ resolver hit ────────────────▶ done (no build; local store is
  │                                  "exists ⇒ complete")
  └─ miss:
       1. scratch  <build_root>/<store-name>/  (+ tmp/, out/)
       2. sandbox.prepare → BuildEnv (env table, cwd, $out staging)
       3. run each phase.<i> via sandbox.spawn;
          stdout+stderr → <log_root>/<store-name>.log
       4. verify every declared output.<i> exists under the staging tree
       5. shade_store::realize_cdf — atomic, idempotent, input-addressed
          install into <store_root>/<digest>-<name>-<version>
       6. registrar.register(out_path, cdf_hash, refs)
       7. remove scratch (unless keep_failed)
```

The store path is fixed **before** anything runs: digest =
BLAKE3-256 truncated to 160 bits over the elided CDF (no resolved output
path ever enters the hash), encoded in the pinned Nix base32 alphabet
([`02 §2–3`](../shade-pkg/02-store.md#2-store-paths), enforced in
`pkg/shade-cdf`). Same recipe + same resolved inputs ⇒ same CDF ⇒ same
path — which is why step "resolver hit" is sound and why a second
`shade build` of an unchanged recipe is a pure lookup.

Phases see the `$out` **staging** directory (`<scratch>/out`), not the final
store path: the shell resolves the literal `$out` token
([shade 05 §3.1](05-derivation.md)) through the `out`/`OUT` variables the
sandbox exports. The store is written exactly once, at realize, after the
outputs are verified — a failed build can never leave a partial store entry.
`TODO(open):` a binary that *embeds* `$out` at build time embeds the staging
path, not the store path; store-path rewriting is explicitly not in v1
([`06 §2`](../shade-pkg/06-build.md#2-phases) phase 4) and this is the same
gap, revisit together.

CDF bytes are read back through `shade_cdf::parse` — the strict inverse of
the emitter, in the same crate, so the byte format still lives in exactly
one place ([shade 08 §1](08-interop.md#1-single-frontend)).

## 2. The two seams

### 2.1 Seam A — `BuildSandbox`

Owns *how* builder commands run. Three methods:

| Method | Contract |
|---|---|
| `prepare(SandboxSpec) → BuildEnv` | turn identity + scratch/staging dirs + declared env + resolved input paths into a runnable environment |
| `spawn(BuildEnv, command, log) → exit code` | run one phase; stdout/stderr to the log; nonzero fails the build at that phase |
| `collect_outputs(BuildEnv, declared) → paths` | verify each `output.<i>` rel path exists under staging; error names the missing one |

**`PermissiveSandbox`** is the bringup impl: phases run as `sh -c` with the
**full host environment** underneath the fixed build vars — no isolation of
any kind. This is host-assisted mode
([`06` intro](../shade-pkg/06-build.md), [`01 §6.1`](../shade-pkg/01-overview.md#6-known-system-gaps-design-time-flags)):
the derivation-visible contract (cwd = scratch, `$out`, the
[`06 §4`](../shade-pkg/06-build.md#4-environment) variables `OUT`, `TARGET`,
`TMPDIR`, `SOURCE_DATE_EPOCH=0`, `TZ=UTC`, `LANG`/`LC_ALL`, `JOBS`, dep
`bin/` dirs heading `PATH`) is exact, but none of the
[`06 §3.1`](../shade-pkg/06-build.md#3-sandbox) *enforcement* rows hold —
`sandbox=1` in the CDF names the contract, and
[`08 §5`](../shade-pkg/08-security.md#5-sandbox-guarantees) records that this
impl overstates it. Recipe `env` keys are restored from the CDF's lowercase
fold to uppercase (invertible: emission validates `[A-Z_][A-Z0-9_]*`).

Real isolation is **`LythosSandbox`**, a second impl of this trait
([build-sandbox.md](build-sandbox.md)): a pure sandbox plan (mounts,
minimal capability set, deterministic env) enforced on the host via a
Seatbelt profile. The executor did not change.

### 2.2 Seam B — `StoreRegistrar`

Owns *what happens after* each realization. One method:
`register(Registration)` with the realized `out_path`, the `store_name`, the
32-char store digest, the full BLAKE3-256 of the CDF bytes, and `refs` — the
derivation's `dep.*` store paths, i.e. the seed of the
[`06 §5`](../shade-pkg/06-build.md#5-registration) reference record.

**`NoopRegistrar`** is the default: correctness during bringup does not need
db records, because the store's atomic realize already gives
"exists ⇒ complete". The real registration procedure (reference scan,
`/shade/db/refs/<digest>` + `/shade/db/valid/<digest>` under the db lock)
replaces this impl; the executor's call site is already in place — it fires
once per realization, never on a lookup hit.

## 3. Build ordering

The evaluator emits every derivation whose `drvPath`/`outPath` was forced,
keyed by canonical drvPath (`/shade/store/<digest>-<name>-<version>.drv`).
`plan_graph` keeps that whole map; the executor walks it:

- **Edges** are the root's transitive `dep.*` entries. A dep value is a
  canonical out path; its producing derivation is keyed at `<out>.drv` in
  the closure map.
- **Order** is DFS postorder from the root: dependencies realize strictly
  before dependents, so when a dependent builds, its inputs already sit in
  the store (and their `bin/` dirs are on `PATH` in `dep.*` order).
- A `dep.*` ref with **no producing derivation** in the evaluation is an
  error (`UnknownDep`) unless the path already exists in the store —
  the evaluator flags store-path literals at eval time
  ([shade 04 §2.4](04-values.md)), so this only admits pre-realized inputs.
- **Cycles** are unconstructible under input-addressing (a dep's path is a
  function of its own hash, which would have to include the dependent's
  path); detecting one means corrupt input and errors out.
- **Source derivations** (`builder=fetch`,
  [`04 §2`](../shade-pkg/04-sources.md#2-source-derivations)) are
  lookup-only: on a miss the executor fails with `FetchUnrealized`
  (errno `ENOSYS`) — the fetcher
  ([`06 §2`](../shade-pkg/06-build.md#2-phases) phase 1) is not implemented.

Per derivation the resolver stack runs first ([`Resolver`] impls in order —
local store today, remote substituter later as a new impl), and only a full
miss builds. Scheduling is serial; the [`05 §6`](../shade-pkg/05-dependencies.md#6-build-order-and-scheduling)
build lock and parallel scheduling are deferred with the OROS port.

## 4. Failure semantics

Any of these aborts the failing derivation and stops the run (its
dependents could only rebuild the same missing input):

| Failure | Error | ABI errno |
|---|---|---|
| phase exits nonzero (or signal-killed) | `PhaseFailed { phase, code, log }` | `EINVAL` |
| declared `output.<i>` not produced | `MissingOutput` | `ENOENT` |
| `dep.*` ref with no producer and not in store | `UnknownDep` | `ENOENT` |
| source derivation miss (fetch unimplemented) | `FetchUnrealized` | `ENOSYS` |
| existing `.drv` byte-mismatch at the target path | `Store(DrvMismatch)` | `EEXIST` |
| unreadable/cyclic closure CDF | `CdfParse` / `BadDrv` / `Cycle` | `EINVAL` |

Guarantees on every failure path:

- **Store untouched.** Realization is the only store write and it happens
  after output verification; it is itself atomic (temp dir + rename,
  `shade-store`), so no partial `<out_path>` or `.drv` can surface.
- **Scratch cleaned.** `<build_root>/<store-name>/` — including any partial
  staged outputs — is removed, unless `keep_failed` (CLI `--keep-failed`,
  [`07 §shade build`](../shade-pkg/07-cli.md)) keeps it for autopsy.
- **Log kept.** `<log_root>/<store-name>.log` survives with the full phase
  trace and the failing exit code appended.
- `BuildError::errno()` maps every variant to the Lythos errno table
  (`abi/lythos-abi/src/errno.rs`) for the eventual OROS `shade` binary; host
  CLIs exit 1 and print the message + errno
  ([`07 §1`](../shade-pkg/07-cli.md#1-conventions)).

Derivations already satisfied earlier in the same run stay satisfied — they
are in the store, immutable; a rerun after a fix resolves them as hits and
resumes at the failure point.

## 5. Filesystem layout

| Path | Role |
|---|---|
| `<store_root>` (`/shade/store`) | immutable realized outputs + `.drv`s |
| `<build_root>/<store-name>/` (`/shade/build/…`) | per-derivation scratch: phase cwd, `tmp/` (`TMPDIR`), `out/` ($out staging) |
| `<log_root>/<store-name>.log` (`/shade/log/…`) | phase-by-phase build log, one per derivation per attempt (truncated on retry) |

All three are executor parameters so host tests and bringup tooling target
throwaway roots; the canonical values are constants in `shade-store` /
`shade-build::executor`.

## 6. Verified gate

`pkg/shade-build/src/tests.rs::executor_gate_build_then_pure_lookup`: a
trivial one-file derivation builds end-to-end into a real store path, and a
second run of the same recipe is a **pure lookup** — zero sandbox spawns,
zero new registrations, same path. Companion tests cover dep-before-dependent
ordering (a dependent that *executes* its dep's binary off `PATH`), env
restore, phase failure, missing output, `keep_failed`, and the fetch-miss
errno. The same gate holds at the CLI:

```
$ shade-build --store-root … --toolchain … ./default.shade
shade-build: 9wg74a0m…-smoke-0.1 — built
/…/store/9wg74a0m…-smoke-0.1
$ shade-build --store-root … --toolchain … ./default.shade
shade-build: 9wg74a0m…-smoke-0.1 — hit (local)
/…/store/9wg74a0m…-smoke-0.1
```

## 7. Deferred

- **Isolation, native half**: `LythosSandbox`
  ([build-sandbox.md](build-sandbox.md)) enforces the 06 §3.1 contract
  host-assisted (Seatbelt); the capability-restricted builder *task* on OROS
  per [`06 §3.2`](../shade-pkg/06-build.md#3-sandbox) remains blocked on a
  kernel per-task fs namespace — the sandbox plan (mounts + cap set) is the
  prepared input for it.
- **Registration db** (seam B replacement): [`06 §5`](../shade-pkg/06-build.md#5-registration)
  reference scan + `/shade/db` records under the db lock.
- **Fetching**: source-derivation realization
  ([`04 §3`](../shade-pkg/04-sources.md#3-source-types)); until then only
  already-realized sources resolve.
- **Fixup phase** (mtime/permission normalization, 06 §2 phase 4) and output
  validation depth (OROS ELF check, 06 §5 step 1).
- **OROS `shade build`**: blocked on argv through the ABI and an OROS
  `EvalIo`; `BuildError::errno()` is the prepared surface.
