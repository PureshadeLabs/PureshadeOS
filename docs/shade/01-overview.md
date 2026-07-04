# Shade — Overview

Shade is the PureshadeOS configuration language: **pure**, **lazy**,
**untyped**, functional — the Nix-language analog for the rpkg store. A
`.shade` expression evaluates to derivation values; each derivation value
serializes to the Canonical Derivation Form (CDF) that rpkg already defines
([`rpkg 02 §3.2`](../rpkg/02-store.md#32-canonical-derivation-form-cdf)).
Everything below the CDF boundary — store paths, hashing, generations,
GC, the build sandbox — is rpkg's existing machinery, consumed unchanged.

The evaluator is **shadec**. Recipes written in Shade are `.shade` files.

This document set is the design specification. It precedes and governs the
implementation (no shadec code exists yet). Specs only — no implementation
code lives here.

Doc set:

| Doc | Contents |
|-----|----------|
| [`01-overview.md`](01-overview.md) | goals, non-goals, relation to Nix and to rpkg TOML recipes, pipeline, tiering, glossary |
| [`02-grammar.md`](02-grammar.md) | exact lexical + syntactic grammar (EBNF), operator precedence, string interpolation, paths, comments |
| [`03-semantics.md`](03-semantics.md) | laziness (thunks, WHNF), purity restrictions, scoping, application, recursion, equality, error model |
| [`04-values.md`](04-values.md) | the nine value types, coercions, string contexts, the derivation value |
| [`05-derivation.md`](05-derivation.md) | the `derivation` builtin: argument → CDF mapping, fixed-output fetch builtins |
| [`06-imports.md`](06-imports.md) | file imports, channel-aware imports, `shade.lock`, purity interaction |
| [`07-stdlib.md`](07-stdlib.md) | full builtins + `lib` surface with signatures, MVP tier marked |
| [`08-interop.md`](08-interop.md) | Shade and TOML recipes coexisting; CDF interchangeability; when to use which |

The split follows the seed proposal unchanged, for the same reason the rpkg
set did ([`rpkg 01`](../rpkg/01-overview.md)): each doc owns one layer, and
the layering is real — 02–03 define the language 04 gives values to, 05
consumes 04 and targets rpkg's CDF, 06 sits on 03's purity rules, 07
enumerates what 02–06 make expressible, 08 closes the loop back to rpkg.
One candidate ninth doc — the shadec CLI/tool surface — is deliberately
*not* split out: shadec has no standalone UX in v1; it is invoked by rpkg
([`08 §4`](08-interop.md#4-pipeline-integration)), and speccing a CLI before
a consumer exists violates the house rule already applied in
[`rpkg 07`](../rpkg/07-cli.md) (`--json` schemas deferred for the same
reason). Definitions live in exactly one doc and are cross-referenced,
never restated (rfs-v2 house style). rpkg concepts — CDF, store paths,
source derivations, channels — are **referenced, never redefined**.

---

## 1. Goals

- **A real language over the same store.** TOML recipes deliberately have no
  expression power ([`rpkg 03`](../rpkg/03-recipe-format.md): no
  interpolation, no conditionals, no includes). Shade supplies abstraction —
  functions, composition, a stdlib — for the cases where a package set,
  not a single package, is being described.
- **CDF frontend, nothing more.** Evaluating Shade produces exactly the CDF
  rpkg defines. Shade adds no store concepts, no new hash inputs, no new
  `.drv` keys. If a Shade build and a TOML build emit the same CDF bytes,
  they are the same build ([`08 §2`](08-interop.md#2-interchangeability)).
- **Nix's evaluation model, exactly.** Pure, lazy, call-by-need, untyped.
  Purity restrictions mirror Nix's pure-eval mode precisely
  ([`03 §5`](03-semantics.md#5-purity)): no arbitrary IO, fixed-output
  fetches only, path reads tracked as eval inputs, no environment access.
- **Minimal language core.** One `derivation` builtin plus a small set of
  fixed-output fetch builtins ([`05`](05-derivation.md)). Higher-level
  constructors (Rust package builders, option systems) live in `lib`
  ([`07`](07-stdlib.md)), not in the language.
- **Coexistence with TOML.** Shade does not replace TOML recipes. Both
  frontends compile to CDF; the store cannot tell them apart
  ([`08`](08-interop.md)).

## 2. Non-goals

- **Nix compatibility.** Shade's syntax and semantics are closely modeled on
  the Nix expression language, and divergences are individually flagged —
  but no `.nix` file is expected to evaluate under shadec, and no Nix
  behavior may be assumed where these docs are silent. (Same stance as
  [`rpkg 01 §4`](../rpkg/01-overview.md#4-relation-to-nix) toward Nix's
  store.)
- **Static types.** Untyped by decision. Values are data; errors surface at
  eval time ([`03 §8`](03-semantics.md#8-errors)).
- **General-purpose programming.** No floats, no mutable state, no
  unrestricted IO, no FFI. Shade describes derivations.
- **A module/option system in v1.** NixOS-module-style config merging is a
  `lib`-level future ([`07 §4`](07-stdlib.md#4-deferred-lib)), not a
  language feature.
- **Replacing rpkg's recipe universe.** `[deps]` resolution, lockfiles for
  TOML recipes, and the CLI stay as specced in rpkg docs. Shade plugs in
  beside them ([`08 §5`](08-interop.md#5-recipe-universe)).

Relation to [`rpkg 01 §2`](../rpkg/01-overview.md#2-non-goals-v1), which
lists "a Nix-like language" as an rpkg non-goal: that statement governs
rpkg-core — recipes-as-TOML stay evaluation-free, and nothing below the
derivation layer grows language awareness. Shade is a **separate frontend**
above that boundary and does not amend it. `TODO(open):` add a forward
pointer from rpkg 01 §2 to this doc set when Shade lands, so the two
statements read as one decision.

## 3. Pipeline — Shade's place {#3-pipeline}

```
foo.shade ──shadec eval──▶ derivation values ──serialize──▶ CDF bytes
                                                              │
TOML recipe + rpkg.lock ──rpkg compile──────────────────────▶ CDF bytes
                                                              │
                                              BLAKE3 digest ──▶ /r/store/<digest>-… 
                                              (rpkg 02 §3)      build if missing (rpkg 06)
```

shadec owns everything left of "CDF bytes": parsing, evaluation, the
purity boundary, serialization. rpkg owns everything right of it. The
interface between them is exactly one artifact: CDF text
([`rpkg 02 §3.2`](../rpkg/02-store.md#32-canonical-derivation-form-cdf)).
shadec never writes the store directly; it hands derivations to the store
services like the TOML compiler does ([`08 §4`](08-interop.md#4-pipeline-integration)).

Evaluation of one top-level expression may yield one derivation, an attrset
of many (a package set), or any plain value (Shade is also usable as a
pure config-data language — `shadec eval` of an expression producing an
attrset is meaningful without any derivation involved).

## 4. Relation to Nix (the language)

Borrowed load-bearing and near-verbatim:

- Lazy call-by-need evaluation with memoized thunks; WHNF forcing rules
  ([`03 §2`](03-semantics.md#2-laziness)).
- Pure-eval restrictions: Shade's purity section is written to match Nix's
  `pure-eval = true` behavior rule for rule ([`03 §5`](03-semantics.md#5-purity)).
- Syntax family: attrsets, `rec`, `let/in`, `with`, `inherit`, lambdas with
  attrset patterns, `//`, string interpolation, indented strings, path
  literals ([`02`](02-grammar.md)).
- String contexts as the dependency-tracking mechanism
  ([`04 §5`](04-values.md#5-string-contexts)).
- Fixed-output fetches as the only network hatch
  ([`05 §5`](05-derivation.md#5-fetch-builtins)).

Deliberately different:

| | Nix | Shade |
|---|-----|------|
| Floats | yes | **no** — value set is int, string, bool, null, list, attrset, function, path, derivation; CDF has no float carrier and configuration needs none |
| `derivation` builtin | open-ended (`builder` + arbitrary env attrs) | closed argument set mapping onto CDF's exhaustive key table ([`05 §2`](05-derivation.md#2-arguments)); unknown attrs are an error, mirroring TOML's unknown-key rule |
| `version` | not a derivation field | required — CDF and store path grammar require it ([`rpkg 02 §2`](../rpkg/02-store.md#2-store-path-format)) |
| Search paths `<foo>` | `NIX_PATH`, impure | **removed**; channels are lock-pinned and reached via `builtins.channel` ([`06 §3`](06-imports.md#3-channels)) |
| `~/` paths, URI literals | present | **removed** (impure / deprecated respectively) |
| Derivation format | ATerm `.drv` | CDF ([`rpkg 02 §3.2`](../rpkg/02-store.md#32-canonical-derivation-form-cdf)) |
| Fixed-output derivations | output-hash-addressed | rpkg source derivations — declared identity is an *input*, addressing stays uniform ([`rpkg 04 §2`](../rpkg/04-sources.md#2-source-derivations)) |
| `import` of derivations (IFD) | supported | **not in v1** — `TODO(open):` import-from-derivation requires eval-time builds; see [`06 §5`](06-imports.md#5-ifd) |

## 5. Tiering — MVP vs incremental {#5-tiering}

The whole surface is specified now; implementation lands in tiers. Tier
markers appear throughout [`07`](07-stdlib.md) and are summarized here:

- **MVP (tier 1):** the language core (02–04 in full — grammar and
  semantics do not tier), `derivation` + the fetch builtins (05), file
  `import` (06 §2), core builtins + minimal `lib` (list/attr/string
  basics, `import` helpers) as marked in [`07`](07-stdlib.md).
- **Tier 2:** channel imports + `shade.lock` (06 §3–4), the remaining
  builtins, `lib.strings`/`lib.lists`/`lib.attrsets` in full.
- **Tier 3 (deferred, design flagged):** rpkg-aware constructors
  (`lib.rustPackage` and friends), fixed-point/overlay composition,
  module/option system, import-from-derivation.

A tier boundary never changes semantics of what is already shipped — later
tiers only add names.

## 6. Glossary

Terms used across all eight docs. rpkg terms (recipe, derivation, CDF,
store path, closure, generation, lockfile, source derivation, PsPackage)
keep their [`rpkg 01 §3`](../rpkg/01-overview.md#3-glossary) meanings and
are not redefined.

- **Expression** — a Shade syntactic term; evaluates to a value.
- **Value** — one of the nine runtime data forms ([`04 §1`](04-values.md#1-value-types)).
- **Thunk** — a suspended, memoized computation; the unit of laziness
  ([`03 §2`](03-semantics.md#2-laziness)).
- **WHNF** — weak head normal form; the result of forcing a thunk once
  ([`03 §2`](03-semantics.md#2-laziness)).
- **Derivation value** — the value returned by `derivation` and the fetch
  builtins; carries `drvPath`/`outPath` and serializes to CDF
  ([`04 §6`](04-values.md#6-the-derivation-value)).
- **String context** — the hidden set of derivation references a string
  carries; how dependencies flow into CDF `dep.*`
  ([`04 §5`](04-values.md#5-string-contexts)).
- **Eval inputs** — the recorded set of everything an evaluation read:
  files, ingested paths, channels. Makes an evaluation a pure function
  ([`03 §5.3`](03-semantics.md#53-eval-inputs)).
- **Channel** — a named, lock-pinned tree of Shade expressions resolved by
  `builtins.channel` ([`06 §3`](06-imports.md#3-channels)).
- **shade.lock** — the pin file for channels used by an evaluation
  ([`06 §4`](06-imports.md#4-shade-lock)).
- **shadec** — the evaluator; also the CDF serializer.
- **Ingestion** — copying a path value's tree into the store as a `local`
  source derivation when it is coerced to a string
  ([`04 §4.2`](04-values.md#42-path-coercion)).
