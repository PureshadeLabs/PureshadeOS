# Shade — Values and Coercions

The nine value types, their operations' type obligations, the coercion
rules, string contexts, and the derivation value. Grammar is
[`02`](02-grammar.md); evaluation timing (when forcing happens) is
[`03`](03-semantics.md).

---

## 1. Value types {#1-value-types}

Every Shade value is exactly one of:

| Type | `builtins.typeOf` | Literal / origin |
|---|---|---|
| integer | `"int"` | `INT`, arithmetic |
| boolean | `"bool"` | `true` / `false` |
| null | `"null"` | `null` |
| string | `"string"` | `STRING` / `IND-STRING` / interpolation / coercion |
| path | `"path"` | `PATH`, path operations |
| list | `"list"` | `[ … ]` |
| attribute set | `"set"` | `{ … }` / `rec { … }` |
| function | `"lambda"` | `x: …`, patterns, partial application, builtins |
| derivation | `"set"` | `derivation`/fetch builtins ([§6](#6-the-derivation-value)) |

Derivations are attrsets with a marker attribute (§6), so `typeOf` reports
`"set"`; `lib.isDerivation` distinguishes them. There is **no float type**
([`01 §4`](01-overview.md#4-relation-to-nix-the-language)).

## 2. Primitives

### 2.1 Integers

64-bit signed two's-complement. Range −2⁶³ … 2⁶³−1. Overflow behavior is
**wrapping** (`TODO(open):` wrap vs. eval-error on overflow — Nix wraps;
wrapping is chosen for parity, but configuration arithmetic that overflows
is almost always a bug, so erroring is defensible. Decide before freeze;
flagged). Operators:

- `+ - *` : int×int → int. `/` : integer division, truncating toward zero;
  division by zero is a type-class eval error ([`03 §8`](03-semantics.md#8-errors)).
- unary `-` : negation.
- `< <= > >=` : int×int → bool.
- `+` is overloaded for strings and paths (§4); on mixed int/string it is a
  type error (no numeric-to-string coercion in `+`).

### 2.2 Booleans

`true`/`false` are initial-scope identifiers ([`03 §4.1`](03-semantics.md#4-scoping)).
`&& || ->` short-circuit (right operand a thunk forced only if needed);
`!` negates. Operands must force to bool or type error. `if`'s condition
must be bool.

### 2.3 Null

`null` is a distinct unit value. Its main uses: absent-optional sentinel,
and CDF/derivation attributes that are dropped when null
([`05 §2`](05-derivation.md#2-arguments)).

### 2.4 Paths {#24-paths}

A path value is an **absolute, normalized** filesystem path plus a flag
recording whether it originated inside the store.

- **Resolution:** a relative path literal (`./x`, `../y`) is resolved
  against the **directory of the file it appears in**, at parse time, to an
  absolute path. This is lexical and pure — it does not depend on process
  cwd (which Shade cannot observe, [`03 §5.1`](03-semantics.md#5-purity)).
  An absolute path literal (`/x`) is taken as-is.
- **Normalization:** `.` segments dropped, `..` segments resolved
  syntactically (not by following symlinks at eval time), trailing slash
  removed. No symlink resolution at eval time.
- **Operations:** `path + path` and `path + string` and `string + path`
  produce a path (concatenate then normalize) — the left operand's type
  decides the result type: `path + x` → path, `str + path` → string
  (§4.3). `path.baseNameOf`/`dirOf` via builtins. Paths are **not**
  indexable or attrset-selectable.
- **Coercion to string** triggers **ingestion** (§4.2) — a path becomes a
  store path. This is the single most important path behavior and the one
  place path values differ sharply from strings.

Store-origin paths (already under `/r/store/`) coerce to their own string
without re-ingesting (§4.2). `TODO(open):` whether eval-time path literals
pointing *into* `/r/store` are permitted at all, or must arrive only via
derivation `outPath` — allowing raw store-path literals is an integrity
seam ([`rpkg 08 §2`](../rpkg/08-security.md#2-trust-model)). v1: permitted
but flagged.

## 3. Lists

Ordered, heterogeneous, immutable. `[ a b c ]` — elements are select-level
expressions ([`02 §3.2`](02-grammar.md#32-simple-expressions)), each a
thunk. Operations:

- `l1 ++ l2` — concatenation (spine forced, elements not).
- indexing/length/mapping via builtins (`elemAt`, `length`, `map`,
  `filter`, `foldl'`, …, [`07 §2.3`](07-stdlib.md#23-lists)); there is no
  index operator syntax.
- `==` : elementwise ([`03 §7`](03-semantics.md#7-equality)); ordering
  lexicographic.

## 4. Attribute sets and coercion

Unordered string-keyed maps; keys are strings (from `ID`, `STRING`, or
dynamic `${}`), values are thunks. Operations:

- `s.a` select; `s.a or d` defaulted select; `s ? a` membership
  ([`02 §3.1`](02-grammar.md#31-operator-expressions)).
- `s1 // s2` update: result has the union of keys, `s2` winning on
  collision. **Shallow** — nested attrsets are replaced, not merged.
  `lib.recursiveUpdate` ([`07 §3.3`](07-stdlib.md#33-attrsets)) deep-merges.
- `removeAttrs s [ "a" "b" ]`, `builtins.attrNames`,
  `builtins.mapAttrs`, etc. ([`07 §2.4`](07-stdlib.md#24-attrsets)).
- `==` : same keys, pairwise-equal values ([`03 §7`](03-semantics.md#7-equality));
  no ordering.

### 4.1 String coercion (`toString` and interpolation) {#41-string-coercion}

The values coercible to a string, and how — this is the exhaustive rule for
`${…}` interpolation and `builtins.toString`:

| Value | Coerces to |
|---|---|
| string | itself (context preserved) |
| path | ingested store path string (§4.2), with a string context referencing the ingested source |
| derivation | its `outPath` string, with a context referencing the derivation ([§5](#5-string-contexts)) |
| attrset with `__toString` | result of `s.__toString s` (must be a string) — the override hook |
| attrset with `outPath` (no `__toString`) | that `outPath`, coerced — lets non-derivation "path-like" sets interpolate |
| int | decimal digits (no separators; negative gets `-`) |
| bool, null, list, function | **type error** — not coercible |

`bool`/`null`/`list` are deliberately *not* coercible (Nix coerces `null`
and bool to `""`/`"1"` in some paths; Shade refuses — silent coercion of
`null` to `""` is a notorious Nix footgun). Convert explicitly:
`lib.boolToString`, `builtins.toJSON` for structured values.

### 4.2 Path coercion and ingestion {#42-path-coercion}

Coercing a **path** to a string (interpolation, `toString`, `+` against a
string) **ingests** it:

1. If the path is already under `/r/store/` (store-origin flag set), the
   result is its own path string with an empty (or preserving) context —
   no copy.
2. Otherwise shadec computes the path's tree hash
   ([`rpkg 04 §3.3`](../rpkg/04-sources.md#33-local)) and realizes a
   `local` **source derivation** ([`rpkg 04 §2`](../rpkg/04-sources.md#2-source-derivations))
   whose store path is the result string. The ingested path + hash is
   recorded as an eval input ([`03 §5.3`](03-semantics.md#53-eval-inputs))
   and the resulting string carries a context referencing that source
   derivation (§5).

Ingestion is how `src = ./.;` in a recipe becomes a pinned source: the
path coerces, the tree is hashed and ingested exactly as rpkg's `local`
source type, and the derivation gets a `source.*` entry
([`05 §2`](05-derivation.md#2-arguments)). A single file path ingests as a
single-file tree. `builtins.filterSource` / `lib.cleanSource`
([`07 §2.5`](07-stdlib.md#25-paths-and-filtering)) prune the tree *before*
hashing, so `target/` and `.git/` can be excluded — the ingested hash
covers only what survives the filter.

`TODO(open):` `toString ./path` that does **not** ingest — Nix's
`toString` on a path yields the plain path string with *no* copy and *no*
context, which recipes use to get a build-time-relative path without a
store dependency. Shade's §4.1 table makes `toString path` ingest, which
is safer (no dangling non-store paths in builds) but loses that idiom.
Decision: **`toString` ingests; provide `builtins.unsafeDiscardStringContext`
+ an explicit `builtins.pathStr` for the rare non-ingesting case**, both
tier-2 and both flagged unsafe. Confirm before freeze.

### 4.3 `+` coercion by left operand {#43-plus-coercion}

`+` is overloaded; the **left** operand's type selects the result and the
coercion applied to the right:

- `int + int` → int.
- `string + x` → string: `x` is string-coerced per §4.1 (so
  `"a" + ./b` ingests `./b` and appends its store path; `"a" + 1` is a
  type error — int is coercible by `toString` but **not** by `+`'s
  right-side rule, which permits only string/path/derivation, matching the
  interpolation-context intent). `TODO(open):` reconcile — either `+`
  allows exactly what `${}` allows (then `"a" + 1` works) or a stricter
  set; current decision: `+`'s right side allows string, path, derivation
  only (the context-bearing types), *not* bare int — because `+` is the
  path/string-building operator, and numeric-looking concatenation should
  be explicit `"a" + toString 1`. Flagged.
- `path + x` → path: `x` must be path or string (a relative-ish suffix);
  derivation/int → type error.

## 5. String contexts {#5-string-contexts}

A string value carries, besides its bytes, a **context**: a set of
references to derivations (and ingested sources) that must be *built/realized*
before the string's bytes are meaningful as a build input. Context is the
mechanism by which dependencies flow from a recipe's interpolations into
the CDF `dep.*`/`source.*` sets — **without it Shade could not know a build
depends on another** ([`05 §3`](05-derivation.md#3-cdf-emission)).

Rules:

- A plain literal string has empty context.
- Coercing a **derivation** to a string (§4.1) adds a context element
  referencing that derivation's output.
- Coercing a **path** (ingestion, §4.2) adds a context element referencing
  the ingested source derivation.
- String concatenation (`+`) and interpolation **union** the contexts of
  all parts. `${a}${b}` carries `context(a) ∪ context(b)`.
- Most string builtins (`substring`, `replaceStrings`, `toLower`, …)
  **propagate** context (the result "still depends on" whatever the input
  did). Splitting/among-parts builtins propagate the whole input context to
  each part (conservative — matches Nix).
- `builtins.unsafeDiscardStringContext s` returns `s`'s bytes with empty
  context; `builtins.getContext` / `appendContext` inspect and rebuild it
  ([`07 §2.8`](07-stdlib.md#28-string-context-ops)). "unsafe" because
  discarding context drops a real build dependency — a string that names a
  store path but no longer *depends* on it, which registration's reference
  scan ([`rpkg 06 §5`](../rpkg/06-build.md#5-registration)) may then flag
  as an undeclared reference. Use only when the dependency is provably
  provided another way.

Context is **ignored by `==`** ([`03 §7`](03-semantics.md#7-equality)) and
is **not** part of the string's printed form; it exists only to be
harvested at `derivation`-call time (§6, [`05 §3`](05-derivation.md#3-cdf-emission)).

## 6. The derivation value {#6-the-derivation-value}

The value returned by `derivation` ([`05`](05-derivation.md)) and, at one
remove, by the fetch builtins. It is an **attrset** carrying:

| Attribute | Value |
|---|---|
| `type` | the marker: the string `"derivation"` (distinguishes from a plain set; `lib.isDerivation x = x.type or null == "derivation"`) |
| `name` | the derivation name (normalized, [`rpkg 02 §2`](../rpkg/02-store.md#2-store-path-format)) |
| `version` | the version string |
| `system` | target triple |
| `drvPath` | string: the `.drv` store path, `/r/store/<digest>-<name>-<version>.drv` — computed by serializing to CDF and hashing ([`05 §3`](05-derivation.md#3-cdf-emission)) |
| `outPath` | string: the output store path (the `.drv` path minus `.drv`), **carrying a context referencing this derivation** — so interpolating a derivation into another recipe threads the dependency |
| `outputName` | `"out"` (single-output only in v1; `TODO(open):` multi-output, [`05 §6`](05-derivation.md#6-multiple-outputs)) |
| the original argument attrs | passed through, so `d.buildInputs` etc. remain readable |

`drvPath`/`outPath` are computed lazily on first demand and memoized: the
derivation attrset can be built and passed around without forcing
serialization until a store path is actually needed. Forcing `outPath`
forces the full CDF emission ([`05 §3`](05-derivation.md#3-cdf-emission)),
which deep-forces every argument attribute (the CDF is a total function of
them).

Two derivation values are `==` iff their `drvPath`s are equal
([`03 §7`](03-semantics.md#7-equality)); equal `drvPath` ⇒ identical CDF ⇒
identical build ⇒ the store dedups them
([`rpkg 02 §3`](../rpkg/02-store.md#3-input-addressing)).
