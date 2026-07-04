# Shade — Evaluation Semantics

The evaluation model: laziness, scoping, application, recursion, equality,
purity, and the error model. Grammar is [`02`](02-grammar.md); value types
and coercions are [`04`](04-values.md). This doc says *when* and *whether*
things evaluate and *what* they mean; it defers the shape of results to 04.

---

## 1. Evaluation overview

shadec evaluates one top-level `expr` to a value. Evaluation is:

- **Pure** — the result is a function of the source plus the recorded eval
  inputs (§5.3) and nothing else. No wall-clock, no ambient environment, no
  network except fixed-output fetches ([`05 §5`](05-derivation.md#5-fetch-builtins)).
- **Lazy** — call-by-need. An expression is evaluated at most once, only
  when its value is demanded, and its result is memoized (§2).
- **Untyped** — no static checking; type errors are runtime eval errors
  (§8). Every value carries a runtime type tag ([`04 §1`](04-values.md#1-value-types)).
- **Terminating is not guaranteed** — Shade is Turing-complete via
  recursion (§6); nonterminating expressions diverge. shadec MAY impose a
  configurable stack-depth / step limit as a resource guard, reported as an
  eval error (§8), never as a value.

## 2. Laziness — thunks and WHNF {#2-laziness}

The evaluation unit is the **thunk**: a suspended expression paired with
the environment (§4) it closes over. Demand forces a thunk to **weak head
normal form (WHNF)** and memoizes the result; a second demand returns the
memoized value without re-evaluating.

WHNF for each value form:

| Value | WHNF means |
|---|---|
| int, bool, null, string, path | fully evaluated (these have no lazy interior) |
| list | the spine exists; **elements stay thunks** until individually forced |
| attrset | the key set and each key's binding-thunk exist; **values stay thunks** |
| function | the closure exists (unapplied) |
| derivation | its defining attrset is in WHNF; `drvPath`/`outPath` computed on demand ([`04 §6`](04-values.md#6-the-derivation-value)) |

Forcing is *shallow*: forcing a list yields the list without forcing
elements; forcing an attrset does not force its values. **Deep forcing**
(recursively forcing everything) happens only where explicitly required:
CDF serialization ([`05 §3`](05-derivation.md#3-cdf-emission)), the
`shadec eval --strict` mode, and `builtins.deepSeq`
([`07 §2.6`](07-stdlib.md#26-evaluation-control)).

Demand is created by: the top-level result being serialized/printed;
operators forcing their operands (arithmetic forces to int, `.`-select
forces the attrset, `if` forces the condition to bool, etc. — the forcing
obligation of each operator is listed with its value rule in [`04`](04-values.md));
`builtins.seq`/`deepSeq`; and pattern matching of an attrset argument
(forces the argument to an attrset in WHNF, §3.2).

Memoization is per-thunk-instance. Two syntactically identical expressions
in different scopes are different thunks. A thunk caught forcing itself is
an **infinite-recursion** error (§8), detected by a per-thunk
"blackhole" mark set on entry and cleared on completion — this catches
`let x = x; in x` and mutual cycles deterministically rather than
diverging.

## 3. Function application and currying {#3-application}

### 3.1 Simple-parameter lambdas

`x: body` is a one-argument function. Application `(x: body) arg` binds `x`
to a **thunk** of `arg` (not its forced value — arguments are lazy) in the
environment of the lambda, then evaluates `body`. Currying is ordinary
nesting: `x: y: body` is a function returning a function; `f a b` parses as
`(f a) b` (§[`02 3.1`](02-grammar.md#31-operator-expressions)).

### 3.2 Attrset-pattern lambdas

`{ a, b ? d, ... }: body`:

1. The argument is forced to WHNF and must be an attrset (else a type
   error, §8).
2. Each formal `a` binds to the argument's `a` thunk. A formal `b ? d`
   with `b` absent binds to a thunk of the default `d`, evaluated in the
   **body's** environment (so defaults may reference other formals and the
   `@`-binding — mutually, laziness permitting).
3. Without `...`, an argument attribute not named by any formal is a
   **type error** naming the unexpected attribute (matches Nix). With
   `...`, extra attributes are ignored (but still reachable only via an
   `@`-binding, next).
4. `args@{ … }` / `{ … }@args` also binds `args` to the whole argument
   attrset (the original, including extras even without capturing them by
   name).

A formal `a` with no default, absent from the argument, is a **missing
required argument** error, raised when the formal is demanded — not
eagerly at application (Nix forces required formals eagerly; Shade defers,
so `({a}: 1) {}` still errors because binding checks presence at call
time). **Normative:** presence of all non-defaulted formals IS checked at
application time (WHNF force of step 1 includes the membership check);
their *values* are not forced. This makes arity errors eager while keeping
values lazy.

### 3.3 No partial application of attrset patterns

Attrset-pattern lambdas take exactly one attrset argument; there is no
positional currying across an attrset pattern. `builtins.functionArgs`
([`07 §2.7`](07-stdlib.md#27-introspection)) reports a pattern lambda's
formals and which have defaults.

## 4. Scoping and recursion {#4-scoping}

Shade is **lexically scoped**. The environment is a chain of frames; name
lookup walks inner to outer; the innermost binding wins.

### 4.1 Initial scope

The outermost frame binds: `true`, `false`, `null`, `builtins`, and the
names re-exported from `builtins` into the global scope for convenience —
exactly `import`, `map`, `throw`, `abort`, `toString`, `derivation`,
`removeAttrs`, `baseNameOf`, `dirOf`, and `isNull` (`isNull` deprecated,
present for parity). The full builtin surface is always reachable through
`builtins.*` ([`07`](07-stdlib.md)); the global re-exports are the only
short names. Shadowing any of them in an inner scope is legal.

### 4.2 Binding forms

- `let b1; b2; in e` — introduces a frame where all bindings are mutually
  recursive: each binding's RHS sees every binding in the same `let`
  (and outer scope). Order is irrelevant. Lazy evaluation makes forward and
  cyclic *references* fine; a cyclic *value dependency* (a binding whose
  WHNF needs its own WHNF) is an infinite-recursion error (§2, §8).
- `rec { … }` — like `let … in`, but the frame's names are also the
  attrset's attributes: inside a `rec` attrset, binds see each other; the
  resulting value is the attrset. Non-`rec` `{ … }` binds do **not** see
  each other (each RHS is evaluated in the enclosing scope only).
- `inherit` — copies a binding from an outer scope (or `inherit (e)`) into
  the current bind set by name ([`02 §3.3`](02-grammar.md#33-attribute-sets));
  the copied binding is a reference, subject to the same laziness.

### 4.3 `with`

`with e; body` — `e` must force to an attrset; its attributes become an
extra scope for `body`. Precedence rule (normative, matches Nix):
a `with`-introduced name is **weaker than any lexical binding**. A bare
identifier resolves to a `with` attribute only if no `let`/lambda/`rec`
binding in scope defines it. Nested `with`s: the innermost `with` wins
among `with`s, but all `with`s lose to lexical bindings. Because `with`
scope is weaker than lexical scope, `with e; x` cannot be resolved
statically in general; a name reachable *only* through `with` and absent
from `e` is an "undefined variable" error at the point of use.

## 5. Purity {#5-purity}

Shade evaluation runs in Nix's `pure-eval = true` mode, always — there is
no impure mode. The rules, exhaustively:

### 5.1 Forbidden

- **No environment access.** There is no `builtins.getEnv`; reading process
  environment is impossible. (Nix keeps `getEnv` but returns `""` in pure
  mode; Shade omits it entirely — a missing name is a clearer failure than
  a silent empty string.)
- **No wall-clock / entropy.** No `builtins.currentTime`,
  `currentSystem` as an ambient value (the target system is an explicit
  argument to `derivation`, [`05 §2`](05-derivation.md#2-arguments)), no
  random. `builtins.currentSystem` is **absent**; recipes take `system`
  as a parameter.
- **No arbitrary filesystem reads.** Path reads are allowed but *tracked*
  and restricted (§5.2). There is no directory listing outside tracked
  ingestion, no stat of arbitrary absolute paths for control flow beyond
  what §5.2 permits.
- **No network** except fixed-output fetch builtins, whose output is
  pinned by a declared hash ([`05 §5`](05-derivation.md#5-fetch-builtins)).
  A fetch with no/placeholder hash is an eval error (Nix allows it in
  impure mode; Shade has none).
- **No mutable state, no ordering-dependent effects.** Evaluation order is
  unobservable except through termination and errors.

### 5.2 Permitted path reads (tracked)

Reading a path is pure because the read content becomes part of the eval
inputs and thus of the result's identity:

- **`import ./f.shade`** — reads and evaluates a Shade file
  ([`06 §2`](06-imports.md#2-file-imports)).
- **`builtins.readFile ./f`**, **`builtins.readDir ./d`**,
  **`builtins.pathExists ./p`**, **`builtins.hashFile`** — read file
  content / directory entries / existence, at an evaluation-time path.
- **Path coercion / ingestion** — using a path in a string context copies
  its tree into the store ([`04 §4.2`](04-values.md#42-path-coercion)).

Every such read is recorded (§5.3). Reads are confined to paths reachable
from the evaluation roots: the top-level file's directory and its imports'
directories, plus channel roots ([`06 §3`](06-imports.md#3-channels)).
`TODO(open):` the exact confinement boundary — whether an absolute path
outside every root is a hard error or merely a tracked read — is tied to
the rpkg build-sandbox fs gap ([`rpkg 06 §3.2`](../rpkg/06-build.md#32-mechanism-on-oros)).
v1 decision: **tracked, not blocked** — reads succeed and are recorded;
the confinement guarantee is documented as honor-system until the kernel
fs-capability work lands, exactly as rpkg's own sandbox row is
([`rpkg 08 §5`](../rpkg/08-security.md#5-sandbox-guarantees)). Revisit
when that closes.

### 5.3 Eval inputs {#53-eval-inputs}

shadec accumulates an **eval-input set** during evaluation: every file
imported, every path read or ingested (by store path + hash), and every
channel pin used ([`06 §4`](06-imports.md#4-shade-lock)). Two evaluations
with the same source and the same eval-input set produce identical results.
The set is reported by `shadec eval --inputs` and is what makes an
evaluation reproducible and cacheable. It is **not** part of any CDF — CDF
identity comes from the derivation's own inputs
([`rpkg 02 §3.3`](../rpkg/02-store.md#33-hash-inputs)); the eval-input set
governs *evaluation* reproducibility, a separate concern from *build*
reproducibility.

## 6. Recursion

Recursion is via `let`/`rec` self-reference and via `builtins.functionArgs`-free
ordinary fixpoints. There is no named-function syntax; recursion is
`let f = x: … f …; in f`. Mutual recursion falls out of `let`'s mutual
scope. A `lib.fix` fixpoint combinator is provided
([`07 §3.4`](07-stdlib.md#34-fixpoints)) for overlay-style composition;
it is library code, not a language primitive.

## 7. Equality {#7-equality}

`==` and `!=` compare by value, deeply, forcing both sides as needed:

- **int** ~ int: numeric equality. **bool**, **null**: by constructor.
- **string**: byte-equal content. **String context is ignored for
  equality** (two strings with equal bytes but different contexts are
  `==`) — matches Nix; contexts affect building, not value identity.
- **path**: equal iff they denote the same absolute normalized path. A
  path and a string are **never** `==` (no cross-type coercion in
  comparison), even if the path would coerce to that string.
- **list**: same length and pairwise `==` (forces spine and elements).
- **attrset**: same key set and pairwise-`==` values. **Exception:** if
  both sides are derivations (carry the derivation marker,
  [`04 §6`](04-values.md#6-the-derivation-value)), they are compared by
  `drvPath` alone — structural comparison of derivations is both expensive
  and wrong (equal `drvPath` ⇒ identical build). This matches Nix.
- **function**: functions are **never equal to anything**, including
  themselves — `f == f` is `false` (Nix raises; Shade returns `false` to
  keep `==` total). `TODO(open):` confirm `false`-vs-error choice against
  real recipe patterns; erroring is the safer default if `false` masks
  bugs. Flagged, not frozen.
- **Different types** compare `false` (never an error), except the
  function case above is still `false`.

Ordering (`< <= > >=`) is defined only for int~int and string~string
(bytewise); lists compare lexicographically element-by-element (Nix
extension, included). Any other operand pairing is a type error (§8).

## 8. Error model {#8-errors}

Errors are **not values** — Shade has no exceptions, no `try` that catches
arbitrary failures. An error aborts evaluation with a message and a source
trace. The only catchable failure is via `builtins.tryEval`
([`07 §2.6`](07-stdlib.md#26-evaluation-control)), which catches
`throw`/`abort`/assertion/type errors reached while forcing its argument
to WHNF and reports success/failure — it does **not** catch
infinite-recursion or resource-limit aborts (those are non-recoverable).

Error kinds (all carry a source position and a forcing trace):

| Kind | Raised by |
|---|---|
| type error | operator/builtin applied to wrong value type; unexpected/missing attrset formal (§3.2) |
| undefined variable | unbound identifier, incl. `with`-only miss (§4.3) |
| missing attribute | `.`-select of an absent attr with no `or` default |
| assertion failure | `assert e; …` with `e` forcing to `false` |
| `throw` | `builtins.throw msg` — user-raised, catchable |
| `abort` | `builtins.abort msg` — user-raised, **not** catchable by `tryEval` |
| infinite recursion | blackhole detection (§2) — not catchable |
| resource limit | step/stack guard (§1) — not catchable |
| purity violation | a forbidden operation (§5.1) reached — not catchable |
| import/eval-input error | unreadable import, channel resolution failure, hash mismatch ([`06`](06-imports.md)) |

`throw` vs `abort`: `throw` is for expected, catchable failures
(a `lib` function rejecting bad input); `abort` is for
"this should be impossible" and bypasses `tryEval` so it cannot be
swallowed. Assertions are `throw`-class (catchable). This is the Nix
convention, adopted deliberately.
