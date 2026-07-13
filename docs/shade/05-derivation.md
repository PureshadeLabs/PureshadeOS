# Shade — The `derivation` Builtin and CDF Emission

The single language primitive that produces a derivation value
([`04 §6`](04-values.md#6-the-derivation-value)) and its serialization to
the Canonical Derivation Form shade already defines. **CDF is not redefined
here** — it is [`shade-pkg 02 §3.2`](../shade-pkg/02-store.md#32-canonical-derivation-form-cdf);
its exhaustive key table is [`shade-pkg 02 §3.3`](../shade-pkg/02-store.md#33-hash-inputs).
This doc defines the *mapping* from `derivation` arguments onto those keys,
nothing more. If a mapping here would require a CDF key not in that table,
it is a spec bug, not a CDF extension.

---

## 1. Design stance

`derivation` is a **closed** primitive: a fixed argument schema mapping
one-to-one onto CDF's fixed key set. Nix's `derivation` is open (any extra
attr becomes an environment variable); Shade's is closed because CDF's key
set is closed ([`shade-pkg 02 §3.3`](../shade-pkg/02-store.md#33-hash-inputs): "adding
a key requires bumping `shade-drv`"). An unknown argument attribute is an
**eval error** — the argument schema is exactly the CDF field policy
([`shade-pkg 03`](../shade-pkg/03-recipe-format.md)), so nothing can be passed that has
no CDF home. This keeps every recipe emitting the same closed CDF surface
([`08 §1`](08-interop.md#1-single-frontend)).

Everything expressive — computing phase strings, assembling env, selecting
sources by platform — happens in Shade *before* the `derivation` call, in
ordinary evaluation. `derivation` itself is a pure structural map.

## 2. Arguments {#2-arguments}

`derivation` takes exactly one attrset. Schema (every recognized
attribute; anything else → error):

| Attr | Type | Req | CDF target ([`shade-pkg 02 §3.3`](../shade-pkg/02-store.md#33-hash-inputs)) |
|---|---|---|---|
| `name` | string | yes | `name` (normalized per [`shade-pkg 02 §2`](../shade-pkg/02-store.md#2-store-path-format); non-normalizable = error, no guessing) |
| `version` | string | yes | `version` |
| `system` | string | yes | `system` |
| `toolchain` | string | yes* | `toolchain` (*may be omitted → defaults to the ambient toolchain identity string, [`shade-pkg 06 §4`](../shade-pkg/06-build.md#4-environment); `TODO(open):` once toolchain is a store dep, this becomes a `dep`, not a scalar — CDF v2, [`shade-pkg 06 §4`](../shade-pkg/06-build.md#4-environment)) |
| `sandbox` | int | no | `sandbox` (default `1`, the only profile, [`shade-pkg 06 §3.1`](../shade-pkg/06-build.md#31-contract-sandbox-profile-1)) |
| `sources` | list of source-specs | no | `source.<i>.*` (§4) |
| `deps` | list of derivations | no | `dep.<i>` — each element's `outPath` store path; sorted bytewise ([`shade-pkg 02 §3.3`](../shade-pkg/02-store.md#33-hash-inputs) ordering rule) |
| `env` | attrset (string→string) | no | `env.<key>` — argument keys must match `[A-Z_][A-Z0-9_]*` and are recorded folded to lowercase (CDF keys are lowercase-only, [`shade-pkg 02 §3.2`](../shade-pkg/02-store.md#32-canonical-derivation-form-cdf) rule 2; lossless fold, builder restores uppercase); values string-coerced; setting a sandbox-fixed var ([`shade-pkg 06 §4`](../shade-pkg/06-build.md#4-environment)) is an error |
| `phases` | list of strings | no | `phase.<i>` in list order (execution order) |
| `outputs` | attrset `{ bin=[…]; lib=[…]; share=[…]; }` | yes | `output.<i>` in `bin`,`lib`,`share` order then list order ([`shade-pkg 03 §6`](../shade-pkg/03-recipe-format.md#6-outputs)) |
| ~~`unsafe`~~ | — | — | **retired** — no longer an accepted argument or CDF key; shade synthesizes no recipe-less builds ([`shade-pkg 03 §7`](../shade-pkg/03-recipe-format.md#7-unsafe-default-recipes)). Passing it is an unknown-argument error |
| `description` | string | no | **not hashed** — carried on the value, dropped from CDF ([`shade-pkg 03 §2`](../shade-pkg/03-recipe-format.md#2-package)) |
| `license` | string | no | **not hashed** — same |
| `bootCritical` | bool | no | **not hashed** — carried to the generation manifest, not CDF ([`shade-pkg 03 §2`](../shade-pkg/03-recipe-format.md#2-package)) |

Null-valued optional attrs are treated as absent (drop the key) — this lets
`env = { FOO = if cond then x else null; }` conditionally omit a var
without attrset surgery.

`shade-drv` (the format header line) is emitted by shadec unconditionally as
`1`, not an argument ([`shade-pkg 02 §3.2`](../shade-pkg/02-store.md#32-canonical-derivation-form-cdf)
rule 3).

### 2.1 `deps` — build vs runtime

CDF does not distinguish build from runtime deps — both are `dep.<i>`
([`shade-pkg 03 §4`](../shade-pkg/03-recipe-format.md#4-deps): "both kinds are hash
inputs identically"). So `derivation` takes a single flat `deps` list. The
build/runtime distinction is **sandbox policy**, carried out-of-CDF: v1
`derivation` has no way to mark it, matching that CDF itself doesn't.
`TODO(open):` if the sandbox ever needs the distinction at the derivation
level (to grant runtime-dep read access differently), add a
`runtimeDeps` argument that still emits `dep.<i>` but tags the manifest —
mirrors shade's `[deps].runtime`. Deferred with the shade sandbox fs work
([`shade-pkg 06 §3.2`](../shade-pkg/06-build.md#32-mechanism-on-oros)).

### 2.2 Dependencies via string context vs explicit `deps`

Two paths put a `dep.<i>` in the CDF:

1. **Explicit** — a derivation in the `deps` list.
2. **Implicit** — a derivation interpolated into any string argument
   (`phases`, `env` values), which carries a string context
   ([`04 §5`](04-values.md#5-string-contexts)) referencing it.

shadec **unions** both sources at emission (§3): the final `dep.*` set is
`deps` ∪ (every derivation appearing in the context of any string argument)
∪ (every source derivation from ingested paths). This is exactly Nix's
model — `buildInputs` is a convenience; the true dependency set is whatever
the strings reference. A dep that appears only via context is as real as an
explicit one. Deduplicated by store path, then sorted bytewise.

## 3. CDF emission {#3-cdf-emission}

Forcing `drvPath`/`outPath` ([`04 §6`](04-values.md#6-the-derivation-value))
runs this total procedure:

1. **Deep-force** every argument attribute (CDF is a function of literal
   values; no thunks may survive into bytes).
2. **Realize sources.** Each `sources` spec and each ingested path
   ([§4](#4-sources)) is resolved to a source derivation
   ([`shade-pkg 04 §2`](../shade-pkg/04-sources.md#2-source-derivations)); its store
   path and identity keys are collected. Fixed-output fetch specs
   ([§5](#5-fetch-builtins)) are realized here — the one eval-time
   network/IO point, gated by the declared hash.
3. **Collect deps** — union of explicit `deps`, string-context derivations
   from all string args, and source derivations (§2.2). Map each to its
   store path.
4. **Build the key set** — populate every CDF key from the argument
   mapping (§2, §4), applying null-drop, normalization, and the not-hashed
   exclusions.
5. **Canonicalize** — sort keys bytewise ascending except the `shade-drv`
   header first; index the repeated lists (`source.<i>`, `dep.<i>`,
   `env.<key>`, `phase.<i>`, `output.<i>`) per the ordering rules
   ([`shade-pkg 02 §3.3`](../shade-pkg/02-store.md#33-hash-inputs)); percent-escape
   values ([`shade-pkg 02 §3.2`](../shade-pkg/02-store.md#32-canonical-derivation-form-cdf)
   rule 4); emit `key=value\n` lines with a trailing LF.
6. **Hash** — `BLAKE3(CDF bytes)`, truncate to 160 bits, base32-encode →
   the digest ([`shade-pkg 02 §3.1`](../shade-pkg/02-store.md#31-hash-function)).
   `drvPath = /shade/store/<digest>-<name>-<version>.drv`;
   `outPath` = same without `.drv`.
7. **Hand off** — shadec writes the `.drv` (CDF bytes) into the store via
   the store services (not directly, [`08 §2`](08-interop.md#2-pipeline-integration)),
   per shade's compile pipeline ([`shade-pkg 03 §8`](../shade-pkg/03-recipe-format.md#8-recipe--derivation-compilation-summary)
   step 4). If `/shade/db/valid/<digest>` exists, no rebuild.

Steps 4–6 use the *one* shared canonicalizer
([`08 §1`](08-interop.md#1-single-frontend)), so **any two recipes that reduce
to the same key set produce byte-identical CDF and therefore the identical
store path** — the property input-addressing depends on. This is also what
lets the seed and store-built shadec agree on every store path during the
bootstrap ([`shade-pkg 09 §6`](../shade-pkg/09-bootstrap.md#6-evaluator-selection)).

### 3.1 The `$out`/`$src<i>` substitution seam

CDF stores phase strings with `$out`/`$src<i>`/`$TARGET`/`$JOBS`
**unexpanded** — they hash as literal bytes, expanded only at build time by
shade ([`shade-pkg 03 §5.2`](../shade-pkg/03-recipe-format.md#52-substitution-variables)).
Shade must therefore emit those literal tokens, **not** a Shade
interpolation of the eventual store path (which would both differ per-build
and create a self-referential hash). Normative: inside `phases` and `env`
values destined for CDF, the strings `$out`, `$src0`, `$src1`, …,
`$TARGET`, `$JOBS` are passed through verbatim. Recipe authors write them
literally: `phases = [ "install -m755 foo $out/bin/foo" ]`. Shade
interpolation `${…}` in those strings is still evaluated by Shade first
(for computing the surrounding command), so a literal `$out` uses the `$`
that Shade leaves alone ([`02 §2.6`](02-grammar.md#26-strings): bare `$`
not before `{` is literal). `lib` provides `lib.placeholder "out"` →
the literal `"$out"` for authors who prefer not to hand-write the sigil
([`07 §3.5`](07-stdlib.md#35-derivation-helpers)).

## 4. Sources {#4-sources}

A `sources` element is one of:

- **an ingested path** — a Shade path value; realized as a `local` source
  ([`04 §4.2`](04-values.md#42-path-coercion)), emitting
  `source.<i>.type=local` + `source.<i>.tree`
  ([`shade-pkg 04 §3.3`](../shade-pkg/04-sources.md#33-local)).
- **a fetch-builtin result** ([§5](#5-fetch-builtins)) — carries its pinned
  identity; emits the type-appropriate `source.<i>.*` keys.
- **an explicit source attrset** — a literal
  `{ type = "crates-io"; crate = …; version = …; sha256 = …; }` (or `git`
  / `local` / `pspackage`), whose keys map directly to the
  `source.<i>.*` identity keys defined per type in
  [`shade-pkg 04 §3`](../shade-pkg/04-sources.md#3-resolution-per-source-type). shadec
  validates the key set per type (unknown/missing key = error) but does
  **not** re-resolve — an explicit attrset is a *pinned* identity, the same
  values a lockfile would hold ([`shade-pkg 04 §5`](../shade-pkg/04-sources.md#5-lockfile)).

Source order in the list fixes `source.<i>` indices and the `$src<i>`
variables ([`shade-pkg 03 §3`](../shade-pkg/03-recipe-format.md#3-source--array-of-tables)).
Ordering rule: `source.*` in list order (recipe order), matching shade
([`shade-pkg 02 §3.3`](../shade-pkg/02-store.md#33-hash-inputs)).

### 4.1 Source derivations are trimmed CDFs

A source derivation omits `system`/`toolchain` and carries `builder=fetch`
([`shade-pkg 04 §2`](../shade-pkg/04-sources.md#2-source-derivations)). shadec emits
that trimmed form for source derivations it creates (path ingestion, fetch
builtins) — it does **not** invent a full build CDF for a fetch. The
`builder`-key `TODO(open)` in shade 04 §2 applies unchanged; shadec follows
whatever shade freezes.

## 5. Fetch builtins (fixed-output) {#5-fetch-builtins}

The **only** eval-time network/IO hatch, and only because the output is
pinned by a declared hash ([`03 §5.1`](03-semantics.md#5-purity)). Each
returns a derivation value whose `outPath` is the ingested source and whose
CDF is a source derivation (§4.1). All require a hash argument; a
missing/empty/placeholder hash is an eval error (no "fetch then tell me the
hash" impurity — that is a Nix impure-mode feature Shade omits).

| Builtin | Signature | Emits (`source.*`) | shade source type |
|---|---|---|---|
| `builtins.fetchCratesIo` | `{ crate, version, sha256 }` → drv | `type=crates-io`, `crate`, `version`, `sha256` | [`shade-pkg 04 §3.1`](../shade-pkg/04-sources.md#31-crates-io) |
| `builtins.fetchGit` | `{ url, commit, submodules ? false }` → drv | `type=git`, `commit`, `submodules?` | [`shade-pkg 04 §3.2`](../shade-pkg/04-sources.md#32-git) |
| `builtins.fetchTree` | `{ url, type, ... , narHash }` → drv | generalized; `TODO(open)` below | — |
| `builtins.path` | `{ path, name ?, filter ? , sha256 ? }` → drv | `type=local`, `tree` | [`shade-pkg 04 §3.3`](../shade-pkg/04-sources.md#33-local) |

- `commit`/`sha256`/`tree` values are the **pinned** identities — shadec
  verifies fetched bytes against them at realization ([§3](#3-cdf-emission)
  step 2) and fails closed on mismatch, exactly as shade fetch does
  ([`shade-pkg 04 §2`](../shade-pkg/04-sources.md#2-source-derivations)). The `url`
  is **not** hashed ([`shade-pkg 04 §3.2`](../shade-pkg/04-sources.md#32-git)); it is
  transport only.
- `fetchGit` takes a resolved **commit**, never a branch/tag — resolving a
  symbolic ref is impure (network at eval, unpinned). Pinning a branch is a
  lockfile/channel concern ([`06 §4`](06-imports.md#4-shade-lock)), not a
  builtin.
- `builtins.path` is the explicit form of path ingestion
  ([`04 §4.2`](04-values.md#42-path-coercion)) with an optional `filter`
  and an optional `name`; a given `sha256` turns it into a verified
  fixed-output ingestion (fails closed on drift).

`TODO(open):` `fetchTree` scope. Nix's `fetchTree`/flake-ref surface is
broad (tarball, github:, mercurial, …). Shade needs *only* what maps to an
shade source type; the generic form is speced as tier-3 and its exact
attribute set is deferred until channel/flake resolution
([`06 §3`](06-imports.md#3-channels)) is designed — do not implement ad-hoc
fetchers that emit non-shade source identities.

## 6. Multiple outputs {#6-multiple-outputs}

v1 is **single-output**: every derivation produces one `outPath`,
`outputName = "out"`. CDF's `output.<i>` entries are the FHS sub-paths
(`bin/foo`, `lib/bar*`) *within* that one output
([`shade-pkg 03 §6`](../shade-pkg/03-recipe-format.md#6-outputs)), **not** Nix-style
separate `$out`/`$dev`/`$doc` store paths. This matches shade's store model
([`shade-pkg 02 §4`](../shade-pkg/02-store.md#4-store-contents): one output directory
with `bin/ lib/ share/`).

`TODO(open):` multi-output derivations (separate `dev`/`doc` store paths)
are a Nix feature with no shade store equivalent yet — adding them requires
shade 02/03 to grow multi-output store paths first. Deferred; Shade will not
lead shade here. `outputName`/`outputs`-as-list attributes are reserved on
the value ([`04 §6`](04-values.md#6-the-derivation-value)) so the schema
slot exists.

## 7. Worked mapping (illustrative)

The `rkilo` CDF example from
[`shade-pkg 02 §3.3`](../shade-pkg/02-store.md#33-hash-inputs) as a Shade expression:

```
derivation {
  name = "rkilo";
  version = "1.2.0";
  system = "x86_64-oros";
  toolchain = "rustc-1.86.0-adf2135f0";
  sources = [
    (builtins.fetchCratesIo {
      crate = "rkilo"; version = "1.2.0";
      sha256 = "9f1c2ab3…e0f";
    })
  ];
  deps = [ lythos-libstd ];        # a derivation value; → dep.0
  env = { RUSTFLAGS = "-C opt-level=3"; };
  phases = [
    "cargo build --release --offline --target x86_64-oros"
    "install -m755 target/x86_64-oros/release/rkilo $out/bin/rkilo"
  ];
  outputs = { bin = [ "rkilo" ]; lib = []; share = []; };
}
```

Emitting this yields byte-for-byte the CDF in
[`shade-pkg 02 §3.3`](../shade-pkg/02-store.md#33-hash-inputs) (given the same
`lythos-libstd` store path), hence the identical digest and store path. This
is the interim worked example ([`08 §7`](08-interop.md#7-worked-example)) —
the low-level `derivation` form; the ergonomic `lib.rustPackage` form lands
with that constructor ([`07 §4`](07-stdlib.md#4-deferred-lib)).
