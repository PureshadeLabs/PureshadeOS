# shade — Store Database, GC Roots, and Garbage Collection

How a realized store path becomes *tracked* (`/shade/db/`), *kept alive*
(`/shade/roots/` + build locks), and *reclaimed when dead* (`shade gc`). This
documents the implementation in `pkg/shade-store-db` (the `StoreDb` engine and
the `shade-gc` host binary) and the `DbRegistrar` seam in `pkg/shade-build`; the
*policy* it implements is [`shade-pkg 02 §7`](../shade-pkg/02-store.md#7-garbage-collection)
(references, roots, sweep) and [`06 §5`](../shade-pkg/06-build.md#5-registration)
(registration). It sits directly on top of
[`store-db-gc`'s track-1 layer](../shade-pkg/02-store.md#2-store-path-format),
input-addressed realization ([`build-executor.md §2.2`](build-executor.md)).

Current vehicle: the **host** `shade-gc` binary
(`pkg/shade-store-db/src/bin/shade-gc.rs`) and the `DbRegistrar` wired into the
host `shade-build` binary — the seed model of
[`shade-pkg 09 §2`](../shade-pkg/09-bootstrap.md#2-seed-shadec). The OROS `shade`
binary stays a stub until argv is plumbed through the ABI (see
`pkg/shade/src/main.rs`).

---

## 1. The database — `/shade/db/`

A plain directory-of-files database ([`02 §7.2`](../shade-pkg/02-store.md#72-references)):
no binary format, **no TOML**, keyed on the 32-char store digest. Per realized
path:

```
/shade/db/valid/<digest>    registration record (existence = "valid")
/shade/db/refs/<digest>     referenced store digests, LF-separated, one per line
/shade/db/locks/<id>        an in-flight build's kept-alive digests (§3)
/shade/db/lock              the mutation lock (exclusive-create; §4)
```

### 1.1 `valid/<digest>` — the registration record

Line-based `key=value`, the same discipline as CDF
([`02 §3.2`](../shade-pkg/02-store.md#32-canonical-derivation-form-cdf)): a
header line first, then lowercase keys in bytewise-sorted order, one LF per
line, trailing LF. The header `shade-db=1` is bumped on any record-shape change
(exactly as `shade-drv` is for CDF).

```
shade-db=1
cdf-hash=<64-hex full BLAKE3-256 of the .drv/CDF bytes, untruncated>
deriver=<digest>-<name>-<version>     # the producing derivation (deriver link)
name=<digest>-<name>-<version>        # the output store-name
registered=<unix seconds>             # realization time; informational, never hashed
```

The **existence** of `valid/<digest>` is the "registered valid" marker
([`02 §2`](../shade-pkg/02-store.md#2-store-path-format) — immutable once
registered valid). The full BLAKE3 is kept untruncated (the path digest is the
160-bit prefix; the record keeps all 256 bits,
[`02 §3.1`](../shade-pkg/02-store.md#31-hash-function)).

### 1.2 `refs/<digest>` — the reference set

The referenced store digests, one per line, sorted, trailing LF. Empty file =
no references. The **closure** of a path is the transitive union over
`refs/`; that graph is what GC marks over (§4). References come from two
sources, unioned at registration:

- **Declared** — the derivation's `dep.*` store paths (from the `.drv`). These
  are canonical `/shade/store/...` and always present.
- **Scanned** — see §2.

Splitting identity (`valid/`) from references (`refs/`) is the
[`02 §7.2`](../shade-pkg/02-store.md#72-references) two-file layout: a sweep
deletes both together, and the closure walk reads only `refs/`.

---

## 2. Reference scanning

When registering an output, the store services **scan its bytes** for embedded
store-path references (Nix-style scanning,
[`02 §7.2`](../shade-pkg/02-store.md#72-references)) rather than trusting
declarations. This catches paths the compiler baked into binaries, panic
strings, or `env!`-captured values that no `dep.*` names.

**Method.** Recursively over the output tree: read each regular file's bytes
and read each symlink's target string, and search for the byte pattern

```
<store_root>/  followed by exactly 32 base32 characters
```

where `<store_root>` is the configured store root (`/shade/store` in
production) and the 32 characters are all in the pinned base32 alphabet
`shade_cdf::BASE32_ALPHABET`. Each match yields one referenced digest. Because
a digest is the hash of a path's whole input closure and cannot be guessed, any
occurrence of `<store_root>/<32 base32>` is a genuine reference — the scan can
only *over*-approximate (a coincidental 32-char run keeps an extra path alive),
never miss one, which is exactly the direction GC safety needs.

The recorded `refs/<digest>` is `scanned ∪ declared`, minus the path itself
(no self-references). At build time the executor additionally enforces the
[`06 §5`](../shade-pkg/06-build.md#5-registration) rule that every *scanned*
reference lies in the input closure ∪ `$out` — that check is a build-integrity
backstop; for GC we only rely on the recorded set being a superset of the real
runtime references.

`DbRegistrar` (in `pkg/shade-build`, the real
[`StoreRegistrar`](build-executor.md#22-seam-b--storeregistrar)) performs this
on every realization; the executor call site is unchanged from the `NoopRegistrar`
bring-up.

---

## 3. Roots — the live set

GC keeps the transitive closure (§4) of the roots. Three kinds
([`02 §7.1`](../shade-pkg/02-store.md#71-roots)):

### 3.1 Direct roots — `/shade/roots/<name>`

Symlinks into the store. Anyone may root a path by symlinking it here; the name
convention is `<owner>-<label>`. GC reads each symlink's target, extracts its
digest, and marks it. A symlink whose target no longer exists is **dangling**
and is pruned during the mark phase (reported as `pruned_roots`).

`StoreDb::add_root` / `remove_root` / `list_roots`, and the `shade-gc add-root
/ del-root / list-roots` subcommands, manage these.

### 3.2 Indirect roots — build locks `/shade/db/locks/<id>`

A build that has not yet registered its output must not have its **inputs**
collected mid-build. Before building, the executor takes a build lock naming
every digest it needs kept alive — its input closure plus the in-progress
output digest. The lock is a file under `/shade/db/locks/` listing those
digests; GC treats each as a root. The lock is held for the build's duration
(`BuildLock`, released on drop) — so it is an *indirect* root: it keeps a set
of store paths live without any of them being individually rooted.

The in-progress output itself lives under `/shade/build/`, not `/shade/store/`,
until realization, so it is never a sweep candidate; the lock protects only the
*inputs* it references.

### 3.3 Generations — `/shade/gen/`

Every store digest embedded anywhere under `/shade/gen/` is a root: the
generation `manifest` records' `package.<i>.path` entries and the `profile/`
symlink forests. GC finds them with the same byte scan as §2 over the
generation tree, so **no installed generation is ever collected** — deleting old
generations (`shade os clean`, [`02 §5`](../shade-pkg/02-store.md#5-generations))
is how their exclusive store paths become reclaimable. (Generations are
produced by `pkg/shade-gen` — see [`generations-profiles.md`](generations-profiles.md);
they additionally register **direct** roots per package (§3.1, names
`gen-<line>-<N>-<i>`), so a live generation's closure is doubly rooted. This
layer only *reads* roots either way.)

---

## 4. `shade gc` — mark and sweep

`StoreDb::gc` ([`02 §7.3`](../shade-pkg/02-store.md#73-sweep)):

1. **Lock.** Take `/shade/db/lock` (exclusive-create — the flock-equivalent
   [`02 §7.2`](../shade-pkg/02-store.md#72-references) calls for; on target
   backed by `SYS_CREATE`'s atomic create-if-absent guarantee,
   `docs/spec/syscalls.md`). Refuse if any build lock is present (a build is
   in flight) unless `--force`.
2. **Mark.** Collect the root digests (§3), then BFS their closure over
   `refs/`. The result is the live digest set.
3. **Sweep.** For every entry under `/shade/store/`: if its name is a valid
   store name (§2 grammar) *and* its digest is marked, keep it; otherwise
   delete it — the output dir or `.drv`, its `refs/<digest>` and
   `valid/<digest>` records, and its `/shade/log/<store-name>.log`. Entries that
   violate the grammar (e.g. a crashed realize's `.tmp-*` sibling) are deleted
   too. The output dir and its `.drv` share one digest, so marking keeps both
   and sweeping drops both.
4. **Reclaim.** Deletion frees host blocks immediately. On the OROS RFS the
   unlink reclaims blocks at the next mount-time mark-and-sweep — RFS keeps
   **no persistent free structure**; a block is free iff reachable from no valid
   superblock (RFS SPACE-1, `fs/rfs2/src/space.rs`). GC therefore never touches
   a free bitmap: unlinking the store entry is the whole reclaim, and RFS's own
   reachability sweep does the rest. This is the deliberate reuse the design
   calls for — the same mark-and-sweep shape at two layers (store references,
   then FS block reachability), neither reinventing the other.

`--dry-run` runs steps 1–2 and reports what step 3 *would* delete, touching
nothing.

### Safety argument

GC must never collect a rooted or reference-reachable path. It cannot, because:

- **The mark set is computed under the lock over an immutable store.** No path
  is realized or mutated between mark and sweep — realization only ever *adds*
  entries, and it holds a build lock while doing so, which either blocks GC
  (step 1) or, under `--force`, is itself a root (§3.2). Store paths are
  immutable ([`02 §2`](../shade-pkg/02-store.md#2-store-path-format)), so a
  marked path cannot change references underneath the walk.
- **References are a superset.** `refs/<digest>` is `declared ∪ scanned`
  (§1.2, §2); the scan over-approximates and never misses (§2). So the closure
  computed in step 2 is a *superset* of the true reachable set — reachability is
  never under-approximated, and a reference-reachable path is always marked.
- **Every liveness source is a root.** Direct roots, in-flight build inputs
  (locks), and all generations are enumerated in step 2 (§3). A path that is
  live for any of these reasons is marked and survives.
- **Indirect roots protect live builds.** A build's inputs are kept by its
  lock even though nothing else roots them; releasing the lock is the only way
  those inputs become collectable, and that happens only after the build has
  registered (so its output, which references them, is now itself valid and
  reachable from whatever roots it).

A crash mid-sweep leaves only already-dead paths behind, which the next GC
re-collects; the sweep order is irrelevant because the mark set is fixed and the
store is immutable ([`02 §7.3`](../shade-pkg/02-store.md#73-sweep) step 4).

---

## 5. Placement / what is deferred

- The `DbRegistrar` replaces `NoopRegistrar` as the `shade-build` binary's
  default registrar; the executor is unchanged (the seam did its job).
- The executor does not yet take a build lock around each derivation — the lock
  API (`StoreDb::lock_build`) exists and GC honors it, but wiring it into the
  build loop is a small follow-up (it matters only once GC and builds can run
  concurrently on OROS). Until then GC's step-1 refusal is the protection.
- Generations (prompt 4) write `/shade/roots/` and `/shade/gen/`; this layer
  reads them as roots and is agnostic to how they are produced.
- On OROS, the whole engine runs against the real `/shade` tree once the
  `shade` binary has argv and a VFS `EvalIo`; the byte format and algorithm are
  host/target-identical (std `fs` today, the same VFS calls later).
