# shade — Recipes

A recipe is what a human writes to describe how to build one package. In
PureshadeOS recipes are **Shade** ([`shade`](../shade/01-overview.md)) — a
`.shade` file, evaluated by **shadec** to a derivation in CDF
([`02 §3.2`](02-store.md#32-canonical-derivation-form-cdf)). Shade is the
**sole** recipe language; there is no other frontend. The language itself
(grammar, evaluation, the `derivation` builtin) is specified in the
[`shade`](../shade/01-overview.md) doc set and **not restated here**.

This doc is **[shade-local]** ([`01 §5`](01-overview.md#5-os-general-vs-shade-local))
and defines the **frontend-independent field policy** every recipe's CDF must
obey: the constraints on `name`/`version` (§2), sources (§3), deps (§4),
build phases and the substitution variables (§5), outputs (§6), the
default-builder policy for a recipe-less input (§7, no longer an install
path), and the compile pipeline (§8). Shade's
`derivation` builtin maps its arguments onto exactly these fields
([`shade 05 §2`](../shade/05-derivation.md#2-arguments)); this doc is the
authority for the *rules*, shade 05 for the *mapping*.

File name: `<name>.shade` standalone, or `prism.shade` at the root of a git
repository (the *in-repo recipe*, [`04 §3.2`](04-sources.md#32-git)).

A recipe evaluates to a derivation value (or an attrset of them — a package
set, [`shade 08 §4`](../shade/08-interop.md#4-package-set-selection)).
Because evaluation is a real language, everything TOML deliberately lacked —
interpolation, conditionals, functions, imports — is available; the field
*policy* below is what constrains the result regardless of how expressively
it was computed.

---

## 1. From recipe to CDF {#1-recipe-to-cdf}

```
foo.shade ──shadec eval──▶ derivation value ──serialize──▶ CDF ([02 §3])
```

shadec owns evaluation and CDF serialization
([`shade 08 §2`](../shade/08-interop.md#2-pipeline-integration)); shade owns
everything below CDF ([`01 §5`](01-overview.md#5-os-general-vs-shade-local)).
The single interface is CDF text. The field policy in this doc constrains the
CDF, so it binds *any* recipe regardless of how its Shade computed the values
— the same way it would bind a second frontend if one ever existed (none is
planned; Shade is sole).

Evaluation is pure and lazy ([`shade 03`](../shade/03-semantics.md)); the only
eval-time IO is fixed-output fetches ([`shade 05 §5`](../shade/05-derivation.md#5-fetch-builtins))
and tracked path reads. So compiling a recipe to CDF is reproducible: same
recipe + same pinned inputs ⇒ same CDF bytes ⇒ same store path (§8).

## 2. `name` and `version` {#2-package}

Every derivation carries these two identity fields
([`02 §3.3`](02-store.md#33-hash-inputs)); they gate the store path grammar
([`02 §2`](02-store.md#2-store-path-format)). In Shade they are the `name`
and `version` arguments to `derivation`
([`shade 05 §2`](../shade/05-derivation.md#2-arguments)).

| Field | Type | Required | Constraints |
|---|---|---|---|
| `name` | string | yes | after normalization must match `[a-z0-9][a-z0-9_-]*`, ≤ 64 bytes ([`02 §2`](02-store.md#2-store-path-format)). Normalization: ASCII-lowercase; any other character is an error (no lossy mapping — the recipe author fixes the name, shadec does not guess) |
| `version` | string | yes | must match `[0-9a-z.+-]+`, ≤ 32 bytes. Semver strongly recommended; required when the package is depended on with a version requirement ([`05 §2`](05-dependencies.md#2-shade-level-resolution)) |

CDF mapping: `name`, `version` copied verbatim after normalization.

Display-only metadata — `description`, `license`, `bootCritical` — is carried
on the derivation value but **not hashed** and **not in CDF**
([`shade 05 §2`](../shade/05-derivation.md#2-arguments)): it does not affect
build output, and excluding it lets docs-only edits avoid rebuilds.
`bootCritical` is carried to the generation manifest instead
([`02 §5`](02-store.md#5-generations)); `TODO(open):` whether it belongs in
the hash — current decision **not hashed**, revisit if manifest-only carriage
proves fragile.

## 3. Sources {#3-source--array-of-tables}

A recipe declares zero or more sources. Source *order is significant*: it
fixes the `source.<i>` CDF indices and the `$src0`, `$src1`, … substitution
variables (§5.2). In Shade, sources are the `sources` list argument
([`shade 05 §4`](../shade/05-derivation.md#4-sources)); each element is a
path (ingested), a fetch-builtin result, or an explicit pinned source-spec
attrset. The four source types, their pinned identity keys, and their
lockfile resolution are specified in
[`04 §3`](04-sources.md#3-resolution-per-source-type) — not restated here.

The identity that reaches `source.<i>.*` is always the **pinned object**
(content hash, commit, tree hash), never a URL or symbolic ref
([`04 §2`](04-sources.md#2-source-derivations)). How a Shade recipe pins —
inline fetch-builtin hashes or a channel pin in the unified lockfile — is
[`shade 06 §4`](../shade/06-imports.md#4-shade-lock) and §8 here.

## 4. Deps {#4-deps}

shade-level dependencies name **other shade packages** whose outputs are needed:

| Kind | Meaning |
|---|---|
| build | packages whose outputs are available during the build (compilers, code generators, libraries linked at build time) |
| runtime | packages that must be in the output's closure but aren't needed to build (e.g. a binary this package `exec`s) |

In Shade these arrive as the `deps` list argument (derivation values) and,
implicitly, via string context — any derivation interpolated into a phase or
env string is a dependency ([`shade 05 §2.2`](../shade/05-derivation.md#22-dependencies-via-string-context-vs-explicit-deps)).
Both routes land as `dep.<i>` store paths in the CDF, deduplicated and sorted
bytewise ([`02 §3.3`](02-store.md#33-hash-inputs)).

Dependency *names* resolve against the **prism registry**
([`05 §2`](05-dependencies.md#2-shade-level-resolution)); a registry member is
now a `.shade` recipe (or a channel, or a bundle). The build/runtime
distinction is **sandbox policy** ([`06 §3`](06-build.md#3-sandbox)), not a
hash input: both kinds are `dep.<i>` identically. CDF does not distinguish
them; `TODO(open):` a `runtimeDeps` marker if the sandbox ever needs the
distinction at the derivation level
([`shade 05 §2.1`](../shade/05-derivation.md#21-deps--build-vs-runtime)).

**Cargo crate dependencies do not appear here.** They come from the source's
own Cargo metadata ([`05 §3`](05-dependencies.md#3-cargo-integration)); deps
here are dependencies *between shade packages*.

## 5. Build {#5-build}

### 5.1 Phases {#51-phases}

An ordered list of command strings, executed in the sandbox
([`06 §2`](06-build.md#2-phases) defines the fixed skeleton around them). In
Shade this is the `phases` list argument
([`shade 05 §2`](../shade/05-derivation.md#2-arguments)). Each string is one
command line: `argv` split on unquoted whitespace, single- and double-quote
grouping, **no** shell — no pipes, no redirection, no `&&`, no globbing, no
variable expansion beyond §5.2. A step needing shell logic ships a script in
its source and invokes it.

If phases are omitted, the default for the first source's type applies
([`§7.1`](#71-default-phase-table) default phase table). `lib.rustPackage`
([`shade 07 §4`](../shade/07-stdlib.md#4-deferred-lib))
is the ergonomic way to get standard cargo phases without writing them.

CDF mapping: each string verbatim as `phase.<i>` in list (execution) order,
after §5.2 variables are left **unexpanded**.

### 5.2 Substitution variables {#52-substitution-variables}

Recognized in phase strings and build-env values, expanded **at build time**
by shade — never at eval time by shadec:

| Variable | Expands to |
|---|---|
| `$out` | the output store path being built |
| `$src0`, `$src1`, … | store path of source *i* ([`04 §2`](04-sources.md#2-source-derivations)) |
| `$TARGET` | the `system` value ([`02 §3.3`](02-store.md#33-hash-inputs)) |
| `$JOBS` | build parallelism (not hashed; determinism requirement [`06 §6`](06-build.md#6-determinism)) |

`$` followed by anything else is literal (no escaping mechanism: `$$` is not
special).

**Critical for Shade.** These tokens hash as their **literal bytes**
(`$out` hashes as `$out`) — the concrete store path can't be an input to its
own hash. A Shade recipe must therefore emit the literal token, **not** a
Shade interpolation of the eventual store path. Shade's `${…}` runs first (at
eval time) and must resolve to a string *containing* the literal `$out`; a
bare `$out` uses the `$` Shade leaves alone
([`shade 02 §2.6`](../shade/02-grammar.md#26-strings)). `lib.placeholder
"out"` yields the literal `"$out"` for authors who prefer not to hand-write
the sigil ([`shade 05 §3.1`](../shade/05-derivation.md#31-the-outsrci-substitution-seam)).
This is normative and the single most likely way a recipe accidentally
produces a wrong store path.

### 5.3 Build env {#53-buildenv}

A string→string map of extra environment variables, set after the sandbox's
scrubbed base environment ([`06 §4`](06-build.md#4-environment)). In Shade,
the `env` attrset argument ([`shade 05 §2`](../shade/05-derivation.md#2-arguments)).
Values may use §5.2 variables. CDF mapping: `env.<key>=<value>` per entry,
unexpanded, sorted by key, where `<key>` is the variable name folded to
lowercase — CDF keys are lowercase-only
([`02 §3.2`](02-store.md#32-canonical-derivation-form-cdf) rule 2); the fold
is lossless (names contain no lowercase) and the builder restores the
uppercase name. Recipe-side, keys must match `[A-Z_][A-Z0-9_]*`. Setting a
variable the sandbox defines as fixed (`PATH`, `HOME`, `SOURCE_DATE_EPOCH`,
… — the fixed list is [`06 §4`](06-build.md#4-environment)) is an error.

## 6. Outputs {#6-outputs}

Declares what the build must produce, relative to `$out`. In Shade, the
`outputs` attrset argument `{ bin = […]; lib = […]; share = […]; }`
([`shade 05 §2`](../shade/05-derivation.md#2-arguments)):

| Key | Meaning |
|---|---|
| `bin` | files that must exist and be executable under `$out/bin/` |
| `lib` | files under `$out/lib/`; entries may end in `*` for a prefix match (rlib names embed metadata hashes, [`05 §4`](05-dependencies.md#4-crate-derivations)) |
| `share` | files or directories under `$out/share/` |

At least one entry across the three is required. Registration fails the build
if a declared output is missing, and fails it if anything exists in `$out`
outside `bin/`, `lib/`, `share/` ([`06 §5`](06-build.md#5-registration)). CDF
mapping: `output.<i>` in `bin`,`lib`,`share` order then list order, each entry
as `bin/rkilo`, `lib/libfoo*`, etc.

v1 is **single-output** — one `outPath` per derivation; the `output.<i>`
entries are FHS sub-paths *within* that one output, not Nix-style separate
`$out`/`$dev`/`$doc` store paths ([`shade 05 §6`](../shade/05-derivation.md#6-multiple-outputs)).

## 7. Default phases; no builder for a recipe-less input {#7-unsafe-default-recipes}

**No install path here, and no derivation synthesis.** Under the prism-only
model ([`04 §1`](04-sources.md#1-the-prism),
[`07 §1`](07-cli.md#1-prism-reference-forms)) there is no
`shade install --unsafe <url>` — a git URL is either a **prism location** (its
root holds `prism.shade`, whose `outputs` govern the build) or a **git input**
of some prism (source bytes only).

**Decision (resolved): a recipe-less input gets no default builder — a builder
must be explicit.** A raw source input (a tree with no `prism.shade` and no
builder declared by the enclosing prism) is **not** buildable: shade does
**not** synthesize a derivation for it. Resolution **fails** with an error
naming the input and demanding an explicit builder (the enclosing prism must
provide `phases`/`outputs` for it, [`04 §3.2`](04-sources.md#32-git)). There is
no `builder = default`, no probe-and-guess, no `unsafe=1` synthesized build.
This closes the former "default-builder for a recipe-less input" open item:
the answer is **explicit-required**, chosen so that nothing ever builds from a
source without an author-written, reviewable build spec. (Consequently the CDF
`unsafe` key and the git-URL synthesis rules of earlier drafts are **retired**;
[`08 §3`](08-security.md#3-unsafe) records the security rationale.)

### 7.1 Default phase table {#71-default-phase-table}

This is a distinct mechanism: the default `phases` for a **real recipe/prism
that omits `phases`** (§5.1) — *not* a builder for recipe-less inputs. A prism
still authors the derivation (name, version, outputs, inputs); it may simply
leave `phases` unspecified and take the ecosystem default below.

| Probe (source root) | Default phases |
|---|---|
| `Cargo.toml` present | `cargo build --release --offline --target $TARGET` then, per bin target `<t>`: `install -m755 target/$TARGET/release/<t> $out/bin/<t>` |
| anything else | error — the default table covers only Cargo; the recipe must specify `phases` explicitly (`TODO(open):` probe table grows with supported ecosystems) |

(`Cargo.toml` is upstream Rust project metadata — TOML by Cargo's design, read
here as *data*, not as a shade recipe. shade authors no TOML.) Note the
"anything else → error" row is the same explicit-required stance as above,
applied to omitted phases: shade never guesses a build beyond the one ecosystem
whose conventions it encodes.

## 8. Recipe → derivation compilation {#8-recipe--derivation-compilation-summary}

Pipeline (each step's own doc is authoritative):

1. **Evaluate** the `.shade` recipe with shadec ([`shade 03`](../shade/03-semantics.md),
   [`shade 05 §3`](../shade/05-derivation.md#3-cdf-emission)) — pure, lazy;
   the evaluator is chosen per the bootstrap rule ([`09 §6`](09-bootstrap.md#6-evaluator-selection)).
   Channel pins resolve from the unified lockfile
   ([`shade 06 §4`](../shade/06-imports.md#4-shade-lock)); eval itself does no
   network.
2. **Resolve sources** against their pins; realize fixed-output fetches
   ([`04`](04-sources.md), [`shade 05 §5`](../shade/05-derivation.md#5-fetch-builtins)).
3. **Resolve deps** and the Cargo crate graph to concrete store paths,
   building dep derivations first as needed ([`05`](05-dependencies.md)).
4. **Serialize CDF** ([`02 §3`](02-store.md#3-input-addressing)) via the one
   shared canonicalizer ([`shade 08 §1`](../shade/08-interop.md#1-single-frontend));
   the digest is the store path. If `/shade/db/valid/<digest>` exists, done — no
   build.
5. Otherwise **build** ([`06`](06-build.md)).

Steps 4–5 are frontend-independent store machinery; step 1 is the language.
The boundary is CDF text, and this doc's field policy is what step 1 must
produce for steps 2–5 to accept it.
