# rpkg — Dependency Resolution and Build Ordering

Two dependency layers exist and stay separate:

1. **rpkg-level deps** — `[deps]` in a recipe, naming other rpkg packages
   (§2).
2. **Cargo-level deps** — the crate graph implied by a Rust source's Cargo
   metadata (§3, §4).

Both resolve to store paths that appear as `dep.<i>` CDF entries
([`02 §3.3`](02-store.md#33-hash-inputs)); below the derivation layer the
distinction disappears. **[rpkg-local]** throughout — this doc is policy for
*producing* derivations ([`01 §5`](01-overview.md#5-os-general-vs-rpkg-local)).

---

## 1. The derivation graph

Resolution turns one requested package into a DAG of derivations:

- the package's own build derivation,
- source derivations for each of its `[[source]]` entries
  ([`04 §2`](04-sources.md#2-source-derivations)),
- build derivations for each rpkg dep (recursively, §2),
- source + build derivations for each crate in its Cargo graph (§4).

A dep satisfied by a PsPackage bundle produces the *same* graph shape: the
bundle's recipe + lockfile define its derivations, and its vendored entries
feed the source derivations ([`04 §3.4`](04-sources.md#34-pspackage)). Below
resolution, nothing distinguishes a bundle-sourced node from a
network-sourced one — same identities, same CDFs, same store paths.

Edges are "needs the store path of". The graph must be acyclic; a cycle is
reported with the full cycle path and aborts resolution (no heuristic
breaking — a real build cycle needs a bootstrap package, which is a recipe
author decision).

Input-addressing makes the graph *cheap to skip through*: every node's digest
is computable from its children's digests without building anything, so rpkg
computes the full graph's store paths first, then builds only the nodes whose
`/r/db/valid/` record is missing ([`03 §8`](03-recipe-format.md#8-recipe--derivation-compilation-summary)).

## 2. rpkg-level resolution {#2-rpkg-level-resolution}

`[deps].build` / `[deps].runtime` entries (`"name"` or `"name@<semver-req>"`,
[`03 §4`](03-recipe-format.md#4-deps)) resolve against the **recipe
universe**: the ordered list of places rpkg looks for a recipe by name.

v1 universe, in precedence order:

1. Recipes in the same directory as the requesting recipe (a repo can carry
   its own lib recipes).
2. The system recipe collection: `/cfg/rpkg/recipes/` — a directory of
   `<name>.rpkg.toml` files, versioned however the user manages `/cfg`.
3. Bundle repositories: `/cfg/rpkg/bundles/` — a directory of PsPackage
   bundles ([`04 §3.4`](04-sources.md#34-pspackage)), each self-describing
   (name + version from its own recipe). A dep resolving to a bundle needs
   **no network at any stage** — resolution reads the bundle's recipe,
   fetch reads its vendor tree — so a populated bundle repo makes entire
   dependency graphs installable offline and immune to upstream
   disappearance. This is the intended air-gap and archival deployment
   shape.

`TODO(open):` channels. A real distribution needs a versioned, updatable,
signed recipe collection (the Nixpkgs analog) with a pinned revision recorded
in the lockfile. v1's directory-based universe is a placeholder; the
lockfile's `[[dep]]` pins (name → version, [`04 §5`](04-sources.md#5-lockfile))
plus the resolved recipes' own lockfiles keep builds reproducible meanwhile,
but *discovery* is unversioned. Design a channel format before any
multi-machine deployment story.

Version selection: among recipes found for a name, choose the highest
`package.version` satisfying the requirement; error on none, error listing
candidates on ambiguity (two universes providing the same name+version with
different content). Exactly **one version of an rpkg package per closure**:
if two recipes in one resolution require disjoint ranges of the same dep,
resolution fails (no Nix-style coexistence in v1 — coexistence needs profile
collision policy first, [`02 §5`](02-store.md#5-generations) `TODO`).

## 3. Cargo integration {#3-cargo-integration}

A source with `Cargo.toml` at its root brings a crate graph. rpkg resolves
it via **Cargo's own resolver** — `cargo metadata` run at lock time
([`04 §1`](04-sources.md#1-two-step-model-resolve-then-fetch), host-assisted
until OROS hosts cargo) — never by reimplementing semver/feature unification.

Inputs to `cargo metadata`:

- The source's `Cargo.toml`(+ workspace).
- The source's `Cargo.lock` **if it ships one** (binaries usually do): pins
  are honored as-is. If absent, resolution generates one (highest compatible
  versions at lock time) — either way the result is snapshotted into
  `rpkg.lock` `[[crate]]` entries and the upstream file is never consulted
  again ([`04 §5`](04-sources.md#5-lockfile) is the single authority).
- Feature selection: default features of the top-level targets;
  `TODO(open):` per-recipe feature overrides (`[source].features` key) —
  omitted from schema v1 until needed, flagged so the schema slot is
  reserved.

Output: the resolved crate set — for each crate instance: name, exact
version, crates.io sha256 (or git/path identity for non-registry deps),
unified feature set, resolved dep list. Written to `[[crate]]` in the
lockfile. Non-registry crate deps (git/path deps in upstream Cargo.tomls)
map to git/local source identities per [`04 §3`](04-sources.md#3-resolution-per-source-type).

## 4. Crate derivations {#4-crate-derivations}

Fixed decision, restated: *a crate dependency resolves via its Cargo
metadata and builds into the store like any other derivation.* Concretely:

- Every `[[crate]]` lockfile entry becomes **one source derivation** (the
  `.crate` fetch, [`04 §3.1`](04-sources.md#31-crates-io)) and **one build
  derivation** whose output is the compiled artifact: `lib/lib<name>-<meta>.rlib`
  (or `.so` for proc-macros, `bin/` for build scripts' host artifacts) plus
  the `--emit=metadata` output needed by dependents.
- A crate build derivation's CDF deps are: its source derivation, the build
  derivations of its resolved crate deps, and the toolchain identity
  ([`02 §3.3`](02-store.md#33-hash-inputs)). Its `env.*` carries the unified
  feature set (as `env.CARGO_FEATURES=<sorted,comma-joined>`) so features
  participate in the hash.
- The top-level package's build derivation then compiles **only its own
  crate**, with `--extern` flags pointing at dep rlib store paths, offline.

Why per-crate (vs. one big derivation that runs `cargo build` over the whole
vendored graph): sharing. Two packages using `serde 1.0.219` with the same
features and toolchain hit the *same* crate derivation digest and share one
build. This is the payoff of "crates.io is just another source" — the store
dedups the ecosystem instead of each package rebuilding its world.

Cost, stated honestly: rpkg must drive `rustc` directly (crate-by-crate,
like Nix's `buildRustCrate` / Buck2, not like `cargo build`), including
build-script execution (`build.rs` compiled + run as a host-target
derivation step) and proc-macro host builds. This is the most
implementation-heavy part of the whole design.
`TODO(open):` build-script and proc-macro handling under cross-compilation
(host-assisted mode builds them for the *host*; self-hosted OROS builds them
for OROS — the CDF `system` key covers the hash correctness, but the
execution model for `build.rs` inside the OROS sandbox needs its own design
pass before self-hosted builds of crates with non-trivial build scripts).

`TODO(open):` escape hatch. If per-crate compilation proves premature, the
fallback is a per-package `cargo build --offline` over vendored source
derivations (sources still shared and pinned, compilation not shared). The
recipe schema and lockfile are identical under both; only the derivation
*granularity* changes, so the fallback is implementable without format
changes. Decide at implementation time; the spec's normative model is
per-crate.

## 5. Version selection summary

| Layer | Resolver | Policy |
|---|---|---|
| rpkg deps | rpkg (§2) | highest satisfying version; one version per name per closure; ambiguity = error |
| Cargo crates | Cargo (§3) | Cargo's resolver v2 semantics, upstream lock honored; multiple semver-major versions of one crate may coexist (Cargo's normal behavior) |

Divergence is deliberate: rpkg names are few and curated (flat namespace,
coexistence banned until profiles can express it); crate graphs are large and
Cargo-shaped (fighting Cargo's resolution rules would desync us from the
ecosystem).

## 6. Build order and scheduling

- Order: topological over the derivation graph (§1). Ready set = nodes whose
  deps are all valid in `/r/db/valid/`.
- Parallelism: up to `$JOBS` independent derivations build concurrently;
  within a crate derivation, `rustc` parallelism is its own affair.
  `$JOBS` is not hashed ([`03 §5.2`](03-recipe-format.md#52-substitution-variables)).
- Locking: one in-flight build per digest, machine-wide, via
  `/r/db/locks/<digest>` ([`02 §7.1`](02-store.md#7-garbage-collection));
  a second builder of the same digest blocks and then reuses the result.
- Failure: a failed derivation aborts scheduling of its dependents; already
  running independent builds finish; already-registered results stay valid
  (they're correct regardless). The failed build dir and log persist
  ([`02 §8`](02-store.md#8-non-durable-areas)).
