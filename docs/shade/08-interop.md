# Shade — Integration with shade

Shade is the **sole** recipe frontend for shade ([`shade-pkg 03`](../shade-pkg/03-recipe-format.md)):
every recipe is a `.shade` file, evaluated by shadec to CDF
([`shade-pkg 02 §3.2`](../shade-pkg/02-store.md#32-canonical-derivation-form-cdf)), which
shade's store/build layer consumes unchanged. There is no TOML recipe format
and no second frontend; this doc defines how shadec plugs into shade's existing
pipeline (§2), the tooling seam for verifying CDF (§3), how a recipe selects a
package from a set (§4), and the two open cross-cutting items — the unified
lockfile (§5) and the prism registry (§6). It **redefines nothing** in shade;
CDF, store paths, resolution, and channels stay
[`shade`](../shade-pkg/01-overview.md)'s.

Governing principle, stated once: **Shade is a CDF frontend and only a CDF
frontend** ([`01 §1`](01-overview.md#1-goals)). It adds no store concept, no
hash input, no `.drv` key. Every guarantee below follows from that single
constraint.

---

## 1. Single frontend, one canonicalizer {#1-single-frontend}

The layer boundary is [`shade-pkg 01 §5`](../shade-pkg/01-overview.md#5-os-general-vs-shade-local)'s
**OS-general vs shade-local** line:

```
  [ frontend — shade-local ]        [ derivation layer — OS-general ]

  foo.shade ─shadec eval─▶ derivation value ─▶ CDF bytes ─▶ digest ─▶ /shade/store/…
                                                  (shade 02 §3)   (shade 06 build)
```

shadec owns everything left of "CDF bytes"; shade owns everything right. The
interface is exactly one artifact: CDF text.

**One canonicalizer.** CDF canonicalization — key sorting, list indexing,
percent-escaping ([`shade-pkg 02 §3.3`](../shade-pkg/02-store.md#33-hash-inputs)) — is a
**single implementation**, referenced by both shadec's emitter
([`05 §3`](05-derivation.md#3-cdf-emission) steps 4–6) and shade's store
layer, never reimplemented. Even with one frontend this matters: the digest
*is* the store path ([`shade-pkg 02 §3`](../shade-pkg/02-store.md#3-input-addressing)),
so any byte-divergence in how CDF is produced silently shifts store paths and
breaks input-addressing. It also underwrites the bootstrap: the seed shadec
and the store-built shadec must emit **byte-identical** CDF for the same
recipe, or store paths move under the system during the seed→store handoff
([`shade-pkg 09 §6`](../shade-pkg/09-bootstrap.md#6-evaluator-selection)).

`TODO(open):` whether the canonicalizer is a standalone crate both shadec and
shade's store services link, or lives in one and is called by the other.
Strongly prefer a **factored crate** — single source of truth for the byte
format, and the natural place for the bootstrap acceptance test
([`shade-pkg 09 §6`](../shade-pkg/09-bootstrap.md#6-evaluator-selection)). Flagged for
the implementation phase.

Consequences that hold by construction:

- **No new store concepts.** Shade cannot introduce a store path shape, a
  generation field, or a GC root shade lacks. If a Shade feature would need
  one, it is out of scope until shade's store grows it first (same rule as
  multi-output derivations, [`05 §6`](05-derivation.md#6-multiple-outputs)).
- **CDF is a pure function of the recipe + pinned inputs.** Evaluation is pure
  and lazy ([`03 §5`](03-semantics.md#5-purity)); the only eval-time IO is
  fixed-output fetches ([`05 §5`](05-derivation.md#5-fetch-builtins)) and
  tracked reads. Same recipe + same pins ⇒ same CDF bytes ⇒ same store path,
  on any machine.

## 2. Pipeline integration {#2-pipeline-integration}

shadec is invoked **by shade**, not standalone ([`01 §3`](01-overview.md#3-pipeline)).
It occupies the recipe→derivation slot in shade's compile pipeline
([`shade-pkg 03 §8`](../shade-pkg/03-recipe-format.md#8-recipe--derivation-compilation-summary)).
It **never writes the store directly**; it hands CDF to the store services.

Because Shade is the only frontend, there is **no frontend dispatch** — a
prism reference that is a path resolves as a `prism.shade`
([`shade-pkg 04 §1`](../shade-pkg/04-sources.md#1-the-prism); a `.pspkg` bundle's
manifest is likewise `prism.shade`, but a bundle is reached as an *input*, not
a top-level install, [`shade-pkg 04 §3.4`](../shade-pkg/04-sources.md#34-pspackage));
there is no extension-based selection between competing recipe languages to
make.

The sequence when shade processes a `.shade` argument
([`shade-pkg 07 §1`](../shade-pkg/07-cli.md)):

1. **shade selects the evaluator** — the profile shadec, else the seed shadec
   ([`shade-pkg 09 §6`](../shade-pkg/09-bootstrap.md#6-evaluator-selection)) — and
   invokes it on the top-level expression, passing the evaluation roots (the
   file's directory) and the lockfile location for channel pins
   ([`06 §4`](06-imports.md#4-shade-lock)).
2. **shadec evaluates** ([`03`](03-semantics.md)) under the purity rules
   ([`03 §5`](03-semantics.md#5-purity)). Fixed-output fetches
   ([`05 §5`](05-derivation.md#5-fetch-builtins)) are the only IO; channels
   must already be pinned (`shadec lock` ran earlier,
   [`06 §4.2`](06-imports.md#42-shadec-lock)).
3. The top-level value is one derivation, an attrset of many (a package set —
   the selected member(s) are taken, §4), or plain data
   ([`01 §3`](01-overview.md#3-pipeline)).
4. **shadec serializes** each selected derivation to CDF
   ([`05 §3`](05-derivation.md#3-cdf-emission)) via the one canonicalizer
   (§1), and reports the derivations, their `drvPath`/`outPath`, source
   derivations, and the recorded **eval inputs**
   ([`03 §5.3`](03-semantics.md#53-eval-inputs)) back to shade.
5. **shade proceeds** ([`shade-pkg 03 §8`](../shade-pkg/03-recipe-format.md#8-recipe--derivation-compilation-summary)
   steps 2–5): resolve sources/deps, and if `/shade/db/valid/<digest>` exists,
   done; else build ([`shade-pkg 06`](../shade-pkg/06-build.md)). shade walks the dep DAG
   ([`shade-pkg 05 §1`](../shade-pkg/05-dependencies.md#1-the-derivation-graph)) over the
   CDF shadec produced — the graph is CDF nodes, evaluator-agnostic.

Once CDF exists, **every downstream shade command behaves identically** —
`install`, `build`, `bundle`, `gc`, `generations`, `verify`
([`shade-pkg 07 §2`](../shade-pkg/07-cli.md)) operate on the resulting store paths with
no knowledge that Shade produced them.

## 3. `shadec cdf` — raw CDF dump {#3-shadec-cdf}

shadec has no standalone UX in v1 ([`01 §`](01-overview.md) — it is invoked by
shade), with **one** exception: a debug subcommand that emits the canonical CDF
bytes of an expression to stdout.

```
shadec cdf <expr>        # → the exact CDF text (shade 02 §3.2) on stdout
```

Purpose: **byte-diff verification**. The store path is `BLAKE3(CDF)`
truncated ([`shade-pkg 02 §3.1`](../shade-pkg/02-store.md#31-hash-function)); when a
change to shadec, the canonicalizer (§1), or a recipe must be checked for
store-path impact, dumping CDF and `diff`-ing is the ground-truth test —
analogous to `shade-pkg --dry-run` printing its plan
([`shade-pkg 07 §2`](../shade-pkg/07-cli.md)). It is also the mechanism of the bootstrap
acceptance test: seed shadec and store shadec must produce identical
`shadec cdf` output over a recipe corpus before the store shadec is activated
([`shade-pkg 09 §6`](../shade-pkg/09-bootstrap.md#6-evaluator-selection)).

Normative: `shadec cdf` runs the full emission procedure
([`05 §3`](05-derivation.md#3-cdf-emission)) including deep-forcing and source
realization, and writes **exactly** the bytes that would become the `.drv` —
no pretty-printing, no trailing newline beyond CDF rule 1's
([`shade-pkg 02 §3.2`](../shade-pkg/02-store.md#32-canonical-derivation-form-cdf)). It is
the only shadec output whose format is byte-normative.

`TODO(open):` flags — `--eval-inputs` to also dump the recorded eval-input set
([`03 §5.3`](03-semantics.md#53-eval-inputs)), `--json` for the derivation
metadata (deferred until a consumer exists, same rule as
[`shade-pkg 07 §2`](../shade-pkg/07-cli.md)). The raw-CDF dump itself is MVP-adjacent —
it needs only the emitter, which the MVP has. Flagged.

## 4. Package-set selection {#4-package-set-selection}

A prism's `outputs.packages` ([`shade-pkg 04 §1`](../shade-pkg/04-sources.md#1-the-prism))
is an **attrset of derivations** (a package set — `{ rkilo = …; rutils = …; }`).
A prism reference selects one **output** with a flake-style fragment:

```
shade install ./myprism#rkilo
```

The `#rkilo` is an **output selector**, whose grammar is defined in
[`02 §6`](02-grammar.md#6-package-set-selectors) (`#a.b.c` for nested sets).
It is **not** part of the `.shade` expression language — it is CLI/argument
syntax applied to the *result* of evaluating the prism's `outputs`
([`shade-pkg 07 §1`](../shade-pkg/07-cli.md#1-prism-reference-forms)).

Selection respects laziness ([`03 §2`](03-semantics.md#2-laziness)): only the
selected output is forced, so a broken sibling does not block a working
one. When `#<output>` is omitted, `TODO(open):` the default — an output named
`default` if present, else the whole `packages` set installs (each member a
package), else error. Leaning: `default` if present, else install-all, error
for anything else. Deferred until multi-package prisms are common; flagged
(also [`02 §6`](02-grammar.md#6-package-set-selectors)).

A prism's outputs also populate the prism registry (§6): each package output is
a registry entry by its `name`
([`shade-pkg 05 §2`](../shade-pkg/05-dependencies.md#2-shade-level-resolution)).

## 5. Unified lockfile {#5-unified-lockfile}

The **prism model resolves the long-standing two-lockfile split**: a prism has
a single `prism.lock` — the flake.lock analog — pinning its whole **input
closure** ([`shade-pkg 04 §1`](../shade-pkg/04-sources.md#1-the-prism),
[`shade-pkg 04 §5`](../shade-pkg/04-sources.md#5-lockfile)). A prism's `inputs` cover
*both* the evaluation inputs older docs pinned in `shade.lock`
([`06 §4`](06-imports.md#4-shade-lock)) *and* the build-source pins
(crate/git/local/pspackage identities). One prism author, one lockfile — the
asymmetry that motivated unification is gone by construction.

What remains open is only the **frozen schema** of that one file (below), not
*whether* to unify.

**Fixed target (decision):** one lockfile format that both the evaluator and
the resolver emit and read, covering (a) channel pins — name → source
identity + resolved commit/tree; (b) build-source pins — per-source
crate/git/local/pspackage identities; (c) shade-level dep version pins. The two
current lockfiles already share shade's source-identity vocabulary by design
([`06 §4`](06-imports.md#4-shade-lock)), so the merge is a **format** question,
not a semantics one.

**Blocked on the channel design.** This is one of **three dependent TODOs that
land together**:

1. **Shade channels** — the resolution/pin model
   ([`06 §3`](06-imports.md#3-channels), [`06 §3.1`](06-imports.md#31-channel-roots-and-precedence)).
2. **shade channels** — the versioned prism registry
   ([`shade-pkg 05 §2`](../shade-pkg/05-dependencies.md#2-shade-level-resolution) `TODO`).
3. **The unified lockfile** — this section.

`TODO(open):` the **frozen unified schema** — field names, table layout,
whether it stays TOML (a machine-written state serialization, as lockfiles are
today, [`shade-pkg 04 §5`](../shade-pkg/04-sources.md#5-lockfile)) or another format;
whether it also absorbs the bootstrap pin
([`shade-pkg 09 §3`](../shade-pkg/09-bootstrap.md#3-trust-and-pinning)). Do not freeze
before channels resolve; until then the two pin surfaces stay separate as
specified. This is the top open cross-cutting item.

## 6. Prism registry {#6-recipe-universe}

shade's **prism registry** ([`shade-pkg 05 §2`](../shade-pkg/05-dependencies.md#2-shade-level-resolution))
is the ordered lookup resolving a dep name to a recipe. With Shade as sole
frontend, a registry member is a `.shade` recipe, a bundle (`.shade` manifest),
or — once channels land ([`06 §3`](06-imports.md#3-channels)) — a channel's
package set. The authority for membership and resolution is
[`shade-pkg 05 §2`](../shade-pkg/05-dependencies.md#2-shade-level-resolution); the
Shade-specific facts:

- **A package-set `.shade` file contributes multiple members** — one per
  attribute, keyed by each derivation's `name` (§4). Forcing is lazy, so
  resolving one name does not evaluate siblings.
- **Name collision is first-wins by search order**, not an error
  ([`shade-pkg 05 §2`](../shade-pkg/05-dependencies.md#2-shade-level-resolution)): a
  higher-precedence registry location or channel shadows a lower one
  deliberately. This is the search-path resolution rule, distinct from the
  *profile* file-collision rule (two installed packages providing the same
  `bin/foo` is still a hard error, [`shade-pkg 02 §5`](../shade-pkg/02-store.md#5-generations)).

Deps flow into CDF the same regardless of a dep's origin: a resolved dep
becomes a `dep.<i>` store path ([`05 §2`](05-derivation.md#2-arguments),
[`shade-pkg 05 §1`](../shade-pkg/05-dependencies.md#1-the-derivation-graph)); below the
derivation layer, which `.shade` produced it is invisible. A mixed DAG (many
recipes, channels, bundles) is one DAG of CDF nodes, built by shade's existing
scheduler.

## 7. Worked example — deferred {#7-worked-example}

A canonical worked recipe — one real package written idiomatically in Shade,
shown alongside the CDF it emits and the store path that results — belongs
here as the concrete anchor for "what a Shade recipe looks like end to end."

`TODO(open):` **blocked on `lib.rustPackage`** ([`07 §4`](07-stdlib.md#4-deferred-lib)).
The idiomatic form of a Rust package recipe is a `lib.rustPackage` call, and
that constructor's interface is itself blocked on shade's
per-crate-vs-per-package decision
([`shade-pkg 05 §4`](../shade-pkg/05-dependencies.md#4-crate-derivations)). Writing the
example against a not-yet-frozen constructor would bake in an interface that
may change. The low-level form (a direct `derivation` call) is already shown
in [`05 §7`](05-derivation.md#7-worked-mapping-illustrative) — that stands as the
interim worked example; this section fills in with the `lib.rustPackage`
version once it lands. (There is no TOML comparison — Shade is the only
frontend; the example is just the canonical Shade recipe.)

## 8. Summary of integration TODO(open)

- **§1** — canonicalizer as a factored crate vs. linked. Prefer factored
  crate (single byte-format source; bootstrap acceptance-test home).
- **§3** — `shadec cdf` flags (`--eval-inputs`, `--json`). Raw dump is
  MVP-adjacent; flags deferred.
- **§4** — default-selection behavior when `#<attr>` omitted. Lean `default`
  then install-all.
- **§5** — **unified lockfile schema.** Top item. Blocked on the channel
  design; the three channel/lock TODOs land together.
- **§7** — `lib.rustPackage` worked example. Blocked on
  [`shade-pkg 05 §4`](../shade-pkg/05-dependencies.md#4-crate-derivations).

None blocks the MVP ([`01 §5`](01-overview.md#5-tiering): file imports +
`derivation` + core `lib`). MVP integration is §1 (one canonicalizer), §2
(shade invokes shadec), and §3 (raw CDF dump) — all fully specified here. §4's
default rule, §5, and §6's channel-dependent parts are tier-2+ and
design-flagged.
