# Shade — Imports and Channels

How a Shade evaluation pulls in other Shade code: file imports (MVP) and
channel-aware imports (tier 2), the `shade.lock` pin file, and how both
stay within the purity rules ([`03 §5`](03-semantics.md#5-purity)). Channel
pinning cross-references shade's channel `TODO`
([`shade-pkg 05 §2`](../shade-pkg/05-dependencies.md#2-shade-level-resolution)) — the
two must land as one design.

---

## 1. `import` — the primitive {#1-import}

`import` is a builtin (re-exported to global scope,
[`03 §4.1`](03-semantics.md#4-scoping)):

```
import :: path | derivation | string-with-context -> value
```

`import e`:

1. Force `e`. It must coerce to a filesystem path
   ([`04 §4.1`](04-values.md#41-string-coercion)):
   - a **path value** — used directly (resolved, normalized,
     [`04 §2.4`](04-values.md#24-paths));
   - a **derivation** or **context-bearing string** — its `outPath` (the
     realized store path) is the import target; realizing it is the one
     case where `import` triggers a build (see IFD, §5 — deferred, so in
     v1 the target must already be realized or be a source derivation).
2. If the path is a **directory**, append `/default.shade` (the fixed
   entry-file name; `TODO(open):` confirm `default.shade` vs
   `default.nix`-style — chosen for brand consistency).
3. Read the file (a tracked read → an eval input,
   [`03 §5.3`](03-semantics.md#53-eval-inputs)), parse it
   ([`02`](02-grammar.md)), evaluate it **in a fresh scope** (only the
   initial scope, [`03 §4.1`](03-semantics.md#4-scoping); *not* the
   importer's lexical scope — imports are hermetic, exactly like Nix), and
   return the resulting value.
4. Memoize by resolved absolute path: importing the same file twice returns
   the same (already-evaluated) value.

A file's value is whatever its single top-level expression evaluates to —
almost always a function (`{ pkgs, ... }: …`) or an attrset. `import` does
no auto-application; `import ./f.shade { x = 1; }` is `(import ./f.shade)`
applied to `{ x = 1; }` by ordinary juxtaposition
([`02 §3.1`](02-grammar.md#31-operator-expressions)).

## 2. File imports {#2-file-imports}

**MVP.** The path passed to `import` is a relative or absolute path literal
([`02 §2.5`](02-grammar.md#25-paths)); relative paths resolve against the
**importing file's directory** ([`04 §2.4`](04-values.md#24-paths)), so a
tree of `.shade` files imports by relative navigation and is
location-independent:

```
# lib/default.shade
{ strings = import ./strings.shade; lists = import ./lists.shade; }

# recipe.shade
let lib = import ./lib; in lib.strings.toUpper "hi"
```

Confinement: file imports may read any path the purity rules permit
([`03 §5.2`](03-semantics.md#5-purity)); v1 tracks-not-blocks reads outside
the evaluation roots, flagged there. Imports do not cross into the store
except via realized derivations (§1 step 1).

Cyclic imports (`a.shade` imports `b.shade` imports `a.shade`) are detected
by the memo table's in-progress mark and reported as an import cycle
([`03 §8`](03-semantics.md#8-errors)), not diverging.

## 3. Channels {#3-channels}

**Tier 2.** A **channel** is a named, versioned, pinned tree of Shade
expressions — the Nixpkgs / flake-registry analog, and the resolution of
shade's channel `TODO`
([`shade-pkg 05 §2`](../shade-pkg/05-dependencies.md#2-shade-level-resolution)). A
channel is reached by name, never by ambient search path (Nix's `<nixpkgs>`
/ `NIX_PATH` is **removed**, [`01 §4`](01-overview.md#4-relation-to-nix-the-language)):

```
builtins.channel :: string -> value      # returns the channel's root value
```

`builtins.channel "shadepkgs"`:

1. Look up `"shadepkgs"` in `shade.lock` (§4). **Unpinned channel = eval
   error** — there is no live resolution at eval time (that would be impure
   network). Pinning happens in a separate `shadec lock` step (§4.2),
   analogous to `shade lock` ([`shade-pkg 07`](../shade-pkg/07-cli.md)) touching the
   network only there.
2. The pin names a source identity (a git commit + tree hash, or a
   PsPackage bundle tree hash — the same identities shade sources use,
   [`shade-pkg 04 §3`](../shade-pkg/04-sources.md#3-resolution-per-source-type)).
   shadec realizes that source derivation (fetch verified against the pin,
   fails closed) and imports its root `default.shade` (§1).
3. The realized channel root + pin is recorded as an eval input
   ([`03 §5.3`](03-semantics.md#53-eval-inputs)).

Channels compose with file imports: a channel root is just a value
(usually a package set / a `lib`), consumed the same way an imported file
is. `builtins.channel "shadepkgs"` returning `{ lib, packages, … }` is the
expected shape.

### 3.1 Channel roots and precedence

The set of known channel *names* comes from `shade.lock` only — there is no
implicit `shadepkgs`. A recipe that wants the system package set writes
`builtins.channel "shadepkgs"` and the caller must have pinned `shadepkgs`. This is
deliberately stricter than Nix (no ambient default channel) so every
evaluation's inputs are explicit and reproducible.

`TODO(open):` channel *aliases* / a project-level default channel
(flake-`inputs`-style) — whether a recipe can declare its channel
requirements inline (a declaration block — a header in the `.shade` file, or
a sidecar) so `shadec lock` knows what to pin without scanning for
`builtins.channel` calls. Leaning toward an explicit declaration block;
deferred until the first multi-channel recipe exists, and settled together
with the unified lockfile ([`08 §5`](08-interop.md#5-unified-lockfile)).
Flagged.

## 4. `shade.lock` {#4-shade-lock}

**Tier 2.** The pin file for channels — TOML as a machine-written state
serialization (not a recipe/config language, [`shade-pkg 01 §1`](../shade-pkg/01-overview.md#1-goals)),
sitting beside the top-level `.shade` file it locks. It is to channels what
`prism.lock`
([`shade-pkg 04 §5`](../shade-pkg/04-sources.md#5-lockfile)) is to sources — and
deliberately shares its identity vocabulary so the two lockfiles can
eventually merge or cross-reference.

```toml
schema = 1

[[channel]]
name = "shadepkgs"
type = "git"                    # git | pspackage
url = "https://…/shadepkgs"         # transport only, not hashed
commit = "adf2135f…40hex"       # pinned resolution
tree = "77c3…64hex"             # BLAKE3 tree hash of the checkout (04 §3.3)

[[channel]]
name = "mylib"
type = "pspackage"
path = "./channels/mylib.pspkg" # informational
tree = "a91d…64hex"             # bundle tree hash (shade 04 §3.4)
```

Rules (mirroring [`shade-pkg 04 §5`](../shade-pkg/04-sources.md#5-lockfile)):

- Every `builtins.channel <name>` used by an evaluation **must** have a
  matching `[[channel]]` entry; missing pin = eval error naming
  `shadec lock`.
- shadec never re-resolves a channel silently during evaluation; the pin is
  the sole authority. Same-source-same-lock ⇒ same channel tree ⇒
  reproducible evaluation, network up or down.
- The `tree` hash is the verification backstop — a git `commit` alone has
  the SHA-1 concern shade flags
  ([`shade-pkg 08 §4`](../shade-pkg/08-security.md#4-source-authenticity)); recording
  `tree` alongside closes it, and is what a fetched checkout is actually
  re-verified against.

### 4.1 Relationship to `prism.lock`

A Shade recipe that both pins channels (`shade.lock`) and produces a
derivation whose sources need pinning has **two** pin surfaces today:
`shade.lock` pins *evaluation* inputs (channels, the code that computes
derivations); `prism.lock` ([`shade-pkg 04 §5`](../shade-pkg/04-sources.md#5-lockfile))
pins *build* inputs (crate/git/local sources of a specific derivation). For
Shade-produced derivations, source pins live inline in the Shade expression
(explicit fetch-builtin hashes, [`05 §5`](05-derivation.md#5-fetch-builtins))
or in `shade.lock` when they come from a channel.

**Fixed target:** these converge into **one** lockfile both the evaluator and
the resolver read/write — a Shade recipe should have a single lockfile, like a
source tree does. This is **blocked on the channel design** and lands with it
as one design ([`08 §5`](08-interop.md#5-unified-lockfile),
[`shade-pkg 05 §2`](../shade-pkg/05-dependencies.md#2-shade-level-resolution)); the frozen
unified schema is `TODO(open):`. Until then the two files stay as specified.
This is the top open cross-cutting item.

### 4.2 `shadec lock`

The only shadec operation that touches the network, and only for channel
**resolution** (turning a channel ref → a pinned commit+tree). Mirrors
`shade lock` precisely ([`shade-pkg 07`](../shade-pkg/07-cli.md)): evaluate enough to
discover `builtins.channel` names (or read the declared requirements once
the §3.1 `TODO` resolves), resolve each against its remote, write/refresh
`shade.lock` whole (stable sort, clean diffs). `TODO(open):` discovering
channel names by partial evaluation is fragile (a `channel` call behind a
condition may be missed) — this is exactly why the §3.1 explicit-declaration
block is preferred. Until it lands, `shadec lock` resolves the names it can
reach and errors on an unpinned channel encountered later, prompting a
re-lock. Flagged.

## 5. Import-from-derivation (IFD) {#5-ifd}

**Deferred (tier 3).** IFD = `import`ing a path that must be *built* first
(a derivation's `outPath` whose content is itself Shade/JSON to import).
Nix supports it; it forces a build in the middle of evaluation, coupling
the evaluator to the builder.

v1 decision: **not supported.** `import` of a derivation whose output is
not already realized is an eval error. Rationale: shadec is a pure frontend
that hands finished CDF to shade ([`05 §3`](05-derivation.md#3-cdf-emission),
[`08 §2`](08-interop.md#2-pipeline-integration)); making eval trigger
builds inverts that and pulls the whole build sandbox
([`shade-pkg 06`](../shade-pkg/06-build.md)) into the evaluator. Reading a source
derivation's tree (a fetched, non-built path) is **allowed** (§1) — that is
fetch, not build. The line is exactly build-vs-fetch, same line
[`03 §5`](03-semantics.md#5-purity) draws.

`TODO(open):` if generated code (e.g. a Cargo-metadata→Shade bridge)
proves necessary, IFD or an out-of-band codegen step must be designed —
prefer the latter (shade's Cargo integration,
[`shade-pkg 05 §3`](../shade-pkg/05-dependencies.md#3-cargo-integration), already runs
`cargo metadata` at lock time outside eval; a Shade recipe can consume its
output as pinned data rather than importing-from-derivation). Flagged.
