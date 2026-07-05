# Shade — Builtins and Library

The full `builtins` primitive surface and the `lib` library surface, each
entry with a signature and semantics. **Tier markers** ([`01 §5`](01-overview.md#5-tiering)):

- **[MVP]** — tier 1, lands with the first shadec.
- **[T2]** — tier 2, incremental.
- **[T3]** — tier 3, deferred / design-flagged.

`builtins` are language primitives (implemented in shadec).
`lib` is Shade code shipped as a channel/import
([`06`](06-imports.md)) — every `lib` function is expressible in terms of
`builtins`, and its spec here is its contract, not its implementation.
Signatures use `::`; `a`/`b` are type variables; `attrs` an attrset;
`->` is function type. A `?` marks an optional attrset field.

---

## 1. Conventions

- Currying: multi-arg builtins are curried (`builtins.map f list`) unless
  the signature shows a single attrset argument (`{ … } -> …`).
- Forcing: every builtin forces its arguments exactly as far as it needs
  ([`03 §2`](03-semantics.md#2-laziness)); list builtins force the spine,
  element-wise builtins force elements they touch. Where forcing is deeper
  than obvious it is noted.
- Errors: type mismatches raise type errors
  ([`03 §8`](03-semantics.md#8-errors)); domain errors (`head []`) are
  noted per entry.
- String context ([`04 §5`](04-values.md#5-string-contexts)) propagates
  through string builtins unless the entry says it is discarded.

## 2. `builtins`

### 2.1 Core / evaluation {#21-core}

| Entry | Sig | Semantics | Tier |
|---|---|---|---|
| `builtins.derivation` | `attrs -> drv` | the derivation primitive ([`05`](05-derivation.md)) | MVP |
| `builtins.import` | `path -> value` | [`06 §1`](06-imports.md#1-import) | MVP |
| `builtins.throw` | `string -> ⊥` | raise catchable error ([`03 §8`](03-semantics.md#8-errors)) | MVP |
| `builtins.abort` | `string -> ⊥` | raise uncatchable error | MVP |
| `builtins.assert` | (syntax, not a builtin) | `assert c; e` ([`02 §3`](02-grammar.md#3-syntactic-grammar)) | MVP |
| `builtins.tryEval` | `a -> { success :: bool; value :: a }` | force arg to WHNF; catch throw/assert/type errors ([`03 §8`](03-semantics.md#8-errors)); not abort/recursion | MVP |
| `builtins.seq` | `a -> b -> b` | force `a` to WHNF, return `b` | MVP |
| `builtins.deepSeq` | `a -> b -> b` | deep-force `a`, return `b` | MVP |
| `builtins.trace` | `a -> b -> b` | emit `a` to shadec's trace stream, return `b`; **eval-pure** (trace is diagnostic, not a value) | MVP |
| `builtins.typeOf` | `a -> string` | the tag ([`04 §1`](04-values.md#1-value-types)) | MVP |

### 2.2 Numbers / booleans

| Entry | Sig | Semantics | Tier |
|---|---|---|---|
| `builtins.add` `sub` `mul` `div` | `int -> int -> int` | operator functions; `div` by zero errors | MVP |
| `builtins.lessThan` | `int -> int -> bool` (also string) | `<` as a function | MVP |
| `builtins.bitAnd` `bitOr` `bitXor` | `int -> int -> int` | bitwise | T2 |
| `builtins.ceil` `floor` | `int -> int` | identity in v1 (no floats); reserved for parity | T2 |

### 2.3 Lists {#23-lists}

| Entry | Sig | Semantics | Tier |
|---|---|---|---|
| `builtins.length` | `list -> int` | forces spine only | MVP |
| `builtins.elemAt` | `list -> int -> a` | 0-based; out of range errors | MVP |
| `builtins.head` | `list -> a` | `elemAt l 0`; `head []` errors | MVP |
| `builtins.tail` | `list -> list` | all but head; `tail []` errors | MVP |
| `builtins.map` | `(a -> b) -> list -> list` | lazy in elements | MVP |
| `builtins.filter` | `(a -> bool) -> list -> list` | forces predicate per element | MVP |
| `builtins.elem` | `a -> list -> bool` | membership by `==` | MVP |
| `builtins.concatLists` | `[list] -> list` | flatten one level | MVP |
| `builtins.foldl'` | `(b -> a -> b) -> b -> list -> b` | **strict** left fold (accumulator forced each step) | MVP |
| `builtins.genList` | `(int -> a) -> int -> list` | `[ f 0 … f (n-1) ]` | MVP |
| `builtins.sort` | `(a -> a -> bool) -> list -> list` | stable sort by strict-weak `lessThan` | MVP |
| `builtins.all` `any` | `(a -> bool) -> list -> bool` | short-circuit | T2 |
| `builtins.partition` | `(a -> bool) -> list -> { right; wrong }` | | T2 |
| `builtins.concatMap` | `(a -> list) -> list -> list` | map then flatten | T2 |
| `builtins.groupBy` | `(a -> string) -> list -> attrs` | | T2 |

### 2.4 Attribute sets {#24-attrsets}

| Entry | Sig | Semantics | Tier |
|---|---|---|---|
| `builtins.attrNames` | `attrs -> [string]` | keys, **sorted bytewise** | MVP |
| `builtins.attrValues` | `attrs -> list` | values in `attrNames` order | MVP |
| `builtins.getAttr` | `string -> attrs -> a` | `s.${name}`; missing errors | MVP |
| `builtins.hasAttr` | `string -> attrs -> bool` | `s ? name` | MVP |
| `builtins.removeAttrs` | `attrs -> [string] -> attrs` | drop named keys | MVP |
| `builtins.intersectAttrs` | `attrs -> attrs -> attrs` | keys in both, values from 2nd | T2 |
| `builtins.mapAttrs` | `(string -> a -> b) -> attrs -> attrs` | | MVP |
| `builtins.listToAttrs` | `[{ name; value }] -> attrs` | later dup name wins | MVP |
| `builtins.catAttrs` | `string -> [attrs] -> list` | collect `.name` from each, skipping absent | T2 |
| `builtins.zipAttrsWith` | `(string -> list -> a) -> [attrs] -> attrs` | | T2 |
| `builtins.functionArgs` | `lambda -> attrs` | formal → hasDefault bool ([`03 §3.3`](03-semantics.md#3-application)) | T2 |

### 2.5 Paths and filtering {#25-paths-and-filtering}

| Entry | Sig | Semantics | Tier |
|---|---|---|---|
| `builtins.path` | `{ path; name?; filter?; sha256? } -> drv` | explicit ingestion / fixed-output ([`05 §5`](05-derivation.md#5-fetch-builtins)) | MVP |
| `builtins.filterSource` | `(path -> type -> bool) -> path -> drv` | ingest with per-entry filter, hashed post-filter ([`04 §4.2`](04-values.md#42-path-coercion)) | MVP |
| `builtins.readFile` | `path -> string` | file content as string (tracked read, [`03 §5.2`](03-semantics.md#5-purity)); context empty | MVP |
| `builtins.readDir` | `path -> attrs` | name → `"regular"`/`"directory"`/`"symlink"`; tracked | MVP |
| `builtins.pathExists` | `path -> bool` | tracked existence check | MVP |
| `builtins.baseNameOf` | `path\|string -> string` | last `/`-segment | MVP |
| `builtins.dirOf` | `path\|string -> path\|string` | parent | MVP |
| `builtins.hashFile` | `string -> path -> string` | `hashFile "blake3" p`; hex digest | T2 |
| `builtins.hashString` | `string -> string -> string` | `hashString "blake3" s` | T2 |

Hash algorithm names: `"blake3"` (native, [`shade-pkg 02 §3.1`](../shade-pkg/02-store.md#31-hash-function)),
`"sha256"` (for crates.io parity, [`shade-pkg 04 §3.1`](../shade-pkg/04-sources.md#31-crates-io)).
`TODO(open):` whether to expose `sha1` (git object ids) — omitted until a
recipe needs it; git commits arrive pre-pinned
([`05 §5`](05-derivation.md#5-fetch-builtins)).

### 2.6 Strings {#26-strings}

| Entry | Sig | Semantics | Tier |
|---|---|---|---|
| `builtins.toString` | `a -> string` | coercion ([`04 §4.1`](04-values.md#41-string-coercion)) | MVP |
| `builtins.substring` | `int -> int -> string -> string` | `substring start len s`; clamps; context propagated | MVP |
| `builtins.stringLength` | `string -> int` | byte length | MVP |
| `builtins.split` | `string -> string -> list` | regex split, Nix semantics (matches interleaved as sublists); `TODO(open):` regex dialect — pin to a documented subset (RE2-style, no backrefs) before freeze | T2 |
| `builtins.replaceStrings` | `[string] -> [string] -> string -> string` | simultaneous replace | MVP |
| `builtins.concatStringsSep` | `string -> [string] -> string` | join; unions contexts | MVP |
| `builtins.toLower` `toUpper` | `string -> string` | ASCII only | T2 |
| `builtins.match` | `string -> string -> list\|null` | anchored regex groups or null | T2 |
| `builtins.toJSON` | `a -> string` | canonical JSON of int/bool/null/string/list/attrs; function/path/derivation handling per §2.9 | T2 |
| `builtins.fromJSON` | `string -> a` | parse JSON → Shade value | T2 |
| `builtins.fromTOML` | `string -> attrs` | parse a TOML **data** string → attrs (e.g. an upstream `Cargo.toml`, external config); **not** a recipe format — recipes are Shade ([`shade-pkg 03`](../shade-pkg/03-recipe-format.md)) | T2 |
| `builtins.toTOML` | `attrs -> string` | `TODO(open):` emit TOML data; defer until a consumer exists | T3 |

### 2.7 Introspection {#27-introspection}

| Entry | Sig | Semantics | Tier |
|---|---|---|---|
| `builtins.isString` `isInt` `isBool` `isNull` `isList` `isAttrs` `isFunction` `isPath` | `a -> bool` | type predicates | MVP |
| `builtins.functionArgs` | (see §2.4) | | T2 |
| `builtins.builtins` / `builtins.langVersion` | `-> attrs` / `-> int` | shadec self-description; `TODO(open):` version scheme | T2 |

### 2.8 String-context ops {#28-string-context-ops}

| Entry | Sig | Semantics | Tier |
|---|---|---|---|
| `builtins.getContext` | `string -> attrs` | inspect context ([`04 §5`](04-values.md#5-string-contexts)) | T2 |
| `builtins.appendContext` | `string -> attrs -> string` | rebuild context | T2 |
| `builtins.unsafeDiscardStringContext` | `string -> string` | strip context — **unsafe**, drops a real build dep ([`04 §5`](04-values.md#5-string-contexts)) | T2 |
| `builtins.hasContext` | `string -> bool` | | T2 |

### 2.9 Fetchers

All in [`05 §5`](05-derivation.md#5-fetch-builtins): `fetchCratesIo` [MVP],
`fetchGit` [MVP], `path` [MVP], `fetchTree` [T3]. Each fixed-output,
hash-required.

## 3. `lib`

Shade-level library, shipped as an importable tree / the `shadepkgs` channel's
`lib` ([`06 §3`](06-imports.md#3-channels)). Organized Nix-lib-style. Only
the MVP subset is required for tier 1; the rest is spec-complete but lands
incrementally.

### 3.1 `lib.strings` {#31-strings}

`concatStrings`, `concatMapStrings`, `concatStringsSep` (re-export),
`hasPrefix`, `hasSuffix`, `removePrefix`, `removeSuffix`, `splitString`,
`optionalString :: bool -> string -> string`, `escapeShellArg`,
`fixedWidthString`, `toInt :: string -> int` (parse, errors on non-numeric),
`boolToString :: bool -> string`. **[MVP]** subset:
`concatStringsSep`, `hasPrefix`, `hasSuffix`, `optionalString`,
`splitString`, `boolToString`. Rest **[T2]**.

### 3.2 `lib.lists` {#32-lists}

`fold` (alias `foldr`), `foldl'` (re-export), `flatten`, `remove`,
`unique`, `range :: int -> int -> list`, `optional :: bool -> a -> list`
(`[]` or `[a]`), `optionals :: bool -> list -> list`, `imap0`/`imap1`,
`zipListsWith`, `last`, `init`, `findFirst`, `count`. **[MVP]** subset:
`optional`, `optionals`, `range`, `flatten`, `unique`, `last`. Rest **[T2]**.

### 3.3 `lib.attrsets` {#33-attrsets}

`mapAttrs` (re-export), `filterAttrs :: (string -> a -> bool) -> attrs -> attrs`,
`recursiveUpdate :: attrs -> attrs -> attrs` (deep `//`,
[`04 §4`](04-values.md#4-attribute-sets-and-coercion)),
`optionalAttrs :: bool -> attrs -> attrs`, `getAttrFromPath`,
`setAttrFromPath`, `nameValuePair`, `attrByPath :: [string] -> a -> attrs -> a`,
`mapAttrsToList`, `foldlAttrs`, `genAttrs :: [string] -> (string -> a) -> attrs`.
**[MVP]** subset: `filterAttrs`, `optionalAttrs`, `recursiveUpdate`,
`attrByPath`, `nameValuePair`, `mapAttrsToList`. Rest **[T2]**.

### 3.4 `lib.fix` and composition {#34-fixpoints}

`fix :: (a -> a) -> a` (least fixpoint via self-application,
[`03 §6`](03-semantics.md#6-recursion)), `fix'`, `extends`, `makeExtensible`,
`composeExtensions`, `makeOverridable`. These power overlay-style package
sets (a `self`/`super` fixpoint over an attrset of packages). **[T3]** —
they are the substrate of the shade-aware package-set constructors (§4) and
land with them. `fix` alone is **[T2]** (small, broadly useful).

### 3.5 Derivation helpers {#35-derivation-helpers}

`lib.isDerivation :: a -> bool` (`x.type or null == "derivation"`,
[`04 §6`](04-values.md#6-the-derivation-value)) **[MVP]**;
`lib.placeholder :: string -> string` (→ `"$out"` etc., the literal build
sigil, [`05 §3.1`](05-derivation.md#31-the-outsrci-substitution-seam))
**[MVP]**; `lib.getBin`/`getLib` (select an output sub-path) **[T2]**;
`lib.makeBinPath :: [drv] -> string` (join `bin/` dirs for a `PATH`-like
env value) **[T2]**.

### 3.6 `lib.cleanSource` and source helpers {#36-source-helpers}

`lib.cleanSource :: path -> drv` (ingest a path with a default filter
dropping `.git/`, `target/`, editor droppings — thin wrapper over
`builtins.filterSource`, [`04 §4.2`](04-values.md#42-path-coercion))
**[MVP]**; `lib.cleanSourceWith`, `lib.sourceByRegex`,
`lib.sourceFilesBySuffices` **[T2]**.

### 3.7 `lib.trivial` {#37-trivial}

`id`, `const`, `pipe :: a -> [f] -> b`, `flip`, `mapNullable`,
`importJSON :: path -> a` (`fromJSON (readFile p)`),
`importTOML :: path -> attrs` (`fromTOML (readFile p)` — read an external
TOML **data** file, e.g. a vendored `Cargo.toml`; not a recipe format,
[`shade-pkg 03`](../shade-pkg/03-recipe-format.md)). **[MVP]** subset:
`id`, `const`, `flip`, `importTOML`, `importJSON`. `pipe` **[T2]**.

## 4. Deferred `lib` — shade-aware constructors {#4-deferred-lib}

**[T3]**, the highest layer, spec-flagged not spec-complete because it
depends on decisions still open in shade (Cargo integration granularity,
[`shade-pkg 05 §4`](../shade-pkg/05-dependencies.md#4-crate-derivations); channel
format, [`06 §3`](06-imports.md#3-channels)):

- `lib.rustPackage :: attrs -> drv` — the ergonomic Rust-package
  constructor: takes `{ name; version; src; cargoLock?; deps?; … }`,
  produces a `derivation` with the standard cargo phases
  ([`shade-pkg 03 §7`](../shade-pkg/03-recipe-format.md#7-unsafe-default-recipes)
  default phase table) and the resolved crate graph as `deps`. **This is
  the Shade analog of nixpkgs' `buildRustCrate`/`rustPlatform`** and the
  ergonomic default for Rust recipes — the idiomatic form the worked example
  ([`08 §7`](08-interop.md#7-worked-example)) is blocked on. `TODO(open):`
  its exact interface is blocked on shade's per-crate-vs-per-package
  derivation decision ([`shade-pkg 05 §4`](../shade-pkg/05-dependencies.md#4-crate-derivations)
  `TODO`); do not stabilize `lib.rustPackage` until that lands.
- `lib.mkPackageSet` / overlay plumbing (over §3.4 `fix`) — a `self`-recursive
  package set with `override`/`overrideAttrs`, the nixpkgs fixpoint shape.
- `lib.modules` — a NixOS-module-style option/merge system.
  **[T3]**, explicitly a non-goal for v1 ([`01 §2`](01-overview.md#2-non-goals)),
  speced here only to reserve the namespace.

`TODO(open):` the entire §4 surface is a **design placeholder** — signatures
above are indicative, not frozen. Each constructor gets its own spec pass
when its shade dependency settles. What is frozen: they are `lib` (Shade
code over `builtins`), never new `builtins`, and never new CDF keys
([`05 §1`](05-derivation.md#1-design-stance)).
