# shade — Prisms, Inputs, and the Lock

The **prism** is shade's flake: the sole unit you install
([`07 §1`](07-cli.md#1-prism-reference-forms)). This doc defines the prism
(§1), the four **input types** a prism may declare — crates.io, git, local,
PsPackage — and how each resolves to a pinned identity (§3), how that identity
becomes a *source derivation* with a store path (§2), the fetch cache (§4), and
the lock that makes the whole thing reproducible (§5). **[shade-local]** except
where noted; the source derivations a prism's inputs emit are ordinary store
objects ([`02`](02-store.md)).

Fixed decision restated: all inputs resolve **uniformly** to store-path builds.
crates.io is just another input type. A crate dependency's build is a
derivation like any other; nothing downstream of resolution knows or cares
where bytes came from.

---

## 1. The prism {#1-the-prism}

A **prism** is a directory (or a single `prism.shade` file) that declares its
pinned **inputs** and computes its **outputs** from them — the exact
Nix-flake shape, renamed for the brand. It is the **only** thing `shade
install` accepts ([`07 §1`](07-cli.md#1-prism-reference-forms)); a bare crate,
git repo, local recipe, or `.pspkg` is **never** a standalone install target —
each exists only as an *input* to some prism.

```
# prism.shade
{
  inputs = {
    lythos-libstd = { type = "git"; url = "https://…"; rev = "v0.3.0"; };
    serde         = { type = "crates-io"; crate = "serde"; version = "^1"; };
    mylib         = { type = "local"; path = ./vendor/mylib; };
  };
  outputs = { self, lythos-libstd, serde, mylib, ... }: {
    packages.rkilo = derivation { … };   # 08 §4 selects packages.<name>
  };
}
```

- **`inputs`** — a named set of input specs. Each names one of the four input
  types (§3) and its requirement; resolution pins each to an exact identity
  and records it in the prism lock (§5). The named inputs are the prism's
  entire outside world — evaluation sees nothing else
  ([`shade 03 §5`](../shade/03-semantics.md#5-purity)).
- **`outputs`** — a function of the resolved inputs (plus `self`, the prism's
  own realized tree) returning an attrset; `outputs.packages.<name>` are the
  installable packages, selected by `#<output>`
  ([`shade 08 §4`](../shade/08-interop.md#4-package-set-selection)).
- **The prism lock** (`prism.lock`, §5) pins the whole input closure — the
  flake.lock analog. It subsumes the old split between build-source pins and
  channel pins: one prism, one lock ([`05 §2`](05-dependencies.md#2-shade-level-resolution),
  [`shade 08 §5`](../shade/08-interop.md#5-unified-lockfile)).

The **input closure** is the transitive set of inputs reached from a prism's
`inputs` (an input may itself be a prism, contributing its inputs). `shade`
resolves the closure at lock time and never re-resolves silently at install
([§5](#5-lockfile)).

### 1.1 Two-step model: resolve, then fetch {#1-two-step-model-resolve-then-fetch}

Each input goes through two steps:

- **Resolve** (needs network, or a pre-populated cache): turn a *requirement*
  (semver range, git branch/tag, local path) into a pinned *identity* (exact
  version + content hash; commit hash; tree hash). Results are recorded in
  the prism lock (§5). Resolution runs only for `shade lock` /
  first-install-without-lock ([`07`](07-cli.md)).
- **Fetch** (needs network unless cached): obtain the bytes for a pinned
  identity, verify against it, ingest into the store as the source
  derivation's output (§2). Fetching a pinned identity is repeatable and
  fails closed on any mismatch.

Everything after fetch is offline by construction — the build sandbox has no
network ([`06 §3`](06-build.md#3-sandbox)). Until OROS has a network stack
capable of TLS/git, both steps run host-assisted
([`01 §6.3`](01-overview.md#6-known-system-gaps-design-time-flags)).
PsPackage inputs (§3.4) need no network in *either* step: resolve and fetch
both read the bundle.

## 2. Source derivations {#2-source-derivations}

A pinned source becomes a derivation whose CDF has:

- `name` = source name, `version` = pinned version (or commit/tree shorthand
  as defined per type in §3), suffix `-src` appended to `name`.
- No `dep.*`, no `phase.*` (the "build" is the fetch, performed by the store
  services, not by a sandboxed builder).
- The type-specific identity keys from §3 as `source.0.*`.
- `system` and `toolchain` **omitted** — source bytes are
  platform-independent. (This is the one place the
  [`02 §3.3`](02-store.md#33-hash-inputs) key table is trimmed; the keys are
  *permitted* to be absent only for source derivations, marked by
  `builder=fetch` in the CDF. `TODO(open):` fold `builder` into the 02 key
  table when CDF v1 freezes — currently source derivations are the only
  non-sandbox builder.)

Output layout: the unpacked source tree, directly under the store path (no
`bin/ lib/ share/` convention — source derivations are exempt from the
[`03 §6`](03-recipe-format.md#6-outputs) layout rule; their store path is
consumed only via `$src<i>`).

Because the identity (content hash / commit / tree hash) is an *input*, the
same pinned source always lands at the same store path, and two recipes
depending on the same crate version share one source store path. This is how
shade gets Nix fixed-output-derivation behavior without an output-addressed
mechanism ([`01 §4`](01-overview.md#4-relation-to-nix)).

Verification at fetch time is per-type (§3); a hash/commit mismatch fails
the fetch and nothing is ingested ([`08 §4`](08-security.md#4-source-authenticity)).

## 3. Input types (resolution per type) {#3-resolution-per-source-type}

The four input types a prism's `inputs` (§1) may declare. Each is **only**
reachable as a prism input — none is a standalone install target
([`07 §1`](07-cli.md#1-prism-reference-forms)). "source" below means "the
resolved bytes of an input."

### 3.1 `crates-io`

Recipe keys ([`03 §3`](03-recipe-format.md#3-source--array-of-tables)):

| Key | Required | Meaning |
|---|---|---|
| `crate` | no (defaults to `package.name`) | crates.io crate name |
| `version` | yes | semver requirement (`"1.2.0"`, `"^1.2"`, `"=1.2.3"`) |

Resolve: query the crates.io index (the sparse HTTP index protocol) for the
highest version satisfying the requirement that is not yanked; record
`version` (exact) and `sha256` (the registry index's checksum for the `.crate`
file) in the lockfile.

Fetch: download `https://crates.io/api/v1/crates/<crate>/<version>/download`
(or a configured mirror — the URL is *not* part of the identity),
SHA-256 the bytes, compare to the locked `sha256`, unpack the `.crate`
(a gzipped tar with a single `<crate>-<version>/` root, stripped on unpack).

CDF identity keys: `source.<i>.type=crates-io`, `source.<i>.crate`,
`source.<i>.version` (exact), `source.<i>.sha256` (64 lowercase hex).

Note the hash is SHA-256 here, not BLAKE3: it's crates.io's checksum and we
verify what the registry attests. Store digests remain BLAKE3-of-CDF; the
SHA-256 is just an input value.

The same mechanism resolves *crate dependencies* of a source's Cargo graph —
each locked crate in [`05 §3`](05-dependencies.md#3-cargo-integration) gets a
source derivation with exactly these identity keys.

### 3.2 `git` {#32-git}

| Key | Required | Meaning |
|---|---|---|
| `url` | yes | clone URL (https or ssh) |
| `rev` | yes | branch, tag, or full/short commit hash |
| `submodules` | no (default `false`) | recursively fetch submodules, pinned to their recorded commits |

Resolve: `ls-remote`-equivalent; a branch or tag resolves to its current
commit; a commit passes through. Record the full 40-hex commit in the
lockfile. (SHA-1 as identity: see [`08 §4`](08-security.md#4-source-authenticity).)

Fetch: clone/fetch the commit (shallow permitted), check out, strip the
`.git` directory, ingest the tree.

CDF identity keys: `source.<i>.type=git`, `source.<i>.commit` (40 hex),
`source.<i>.submodules` (present as `1` only when true).

**The URL and the symbolic ref are not hash inputs.** A commit hash is a
fixed object id: it fully determines the fetched tree, so the same commit
obtained from any mirror, fork, or transport is the same derivation — the
Nix determinism rule ([`02 §3.3`](02-store.md#33-hash-inputs), excluded-list).
The `url` and `requested` ref live only in the lockfile (§5), as fetch
transport and audit trail; changing them without changing the commit
changes nothing in the store.

As a **git input**, a git entry contributes only its pinned source tree to the
enclosing prism (via `$src<i>` / the input binding); the enclosing prism's
`outputs` govern the build. A git repo that is *itself* a prism (its root holds
`prism.shade`) is reached as a **remote prism reference**
([`07 §1`](07-cli.md#1-prism-reference-forms)), not as a git input — the two
uses are distinct: a git *input* is source bytes, a git *prism* is an install
unit. A raw git input with **no builder** declared by the enclosing prism is
**not buildable** — the enclosing prism must give it explicit `phases`/`outputs`;
shade does not synthesize a default builder for a recipe-less source tree, and
resolution fails if none is provided ([`03 §7`](03-recipe-format.md#7-unsafe-default-recipes),
decided **explicit-required**). A git input is thus always *just source bytes*;
turning bytes into a build is always the enclosing prism's explicit job.

### 3.3 `local`

| Key | Required | Meaning |
|---|---|---|
| `path` | yes | directory, relative to the recipe file's location |

Resolve = fetch: compute the **tree hash** of the directory and ingest it.
The lockfile records the tree hash; a later build verifies the directory
still matches or re-resolves (local sources are expected to change — the
lockfile entry pins *what was built*, and `shade build` refreshes it,
[`07`](07-cli.md)).

**Tree hash algorithm** (normative): a BLAKE3 hash over a canonical
manifest of the tree.

1. Walk the tree. Exclude: `.git/`, and nothing else (`TODO(open):` ignore
   file support, e.g. honoring `.gitignore` — v1 hashes everything, which
   makes `target/` in a dirty checkout a footgun; likely resolution is
   excluding paths matching the recipe's VCS ignores).
2. For each entry, emit one line: `<type> <path> <hash>\n` where `type` ∈
   `f` (file) / `x` (executable file) / `l` (symlink) / `d` (directory);
   `path` is the /-separated relative path, percent-escaped per CDF rule 4
   ([`02 §3.2`](02-store.md#32-canonical-derivation-form-cdf)); `hash` is
   lowercase-hex BLAKE3-256 of file content, of the symlink target string,
   or empty for `d`.
3. Sort lines bytewise, concatenate, BLAKE3-256 the result. Mode bits beyond
   the executable bit, mtimes, and ownership are excluded — they don't
   survive store ingestion anyway ([`02 §2`](02-store.md#2-store-path-format)).

CDF identity keys: `source.<i>.type=local`, `source.<i>.tree` (64 lowercase
hex). The recipe-relative `path` is **not** hashed — identity is content,
so the same tree at a different path is the same source.

### 3.4 `pspackage` {#34-pspackage}

A **PsPackage** is a self-contained bundle: a prism (`prism.shade`), its lock,
and vendored sources shipped together. It is the fourth input type and the
offline/archival form of a prism: as a `pspackage` input, a bundle builds with
**no network fetch** and **no dependence on upstream availability** — crates.io
outages, deleted git repos, and yanked versions cannot break a build whose
bundle you hold. A bundle built today builds identically in ten years. (A
`.pspkg` is produced from a prism by `shade bundle`,
[`07`](07-cli.md); it is consumed only as an input, never installed directly.)

**Bundle layout** (normative). A directory, conventionally named
`<name>-<version>.pspkg` (`TODO(open):` a single-file archive form —
uncompressed tar of this layout — for transport; directory form is
authoritative either way):

```
<name>-<version>.pspkg/
├── prism.shade           the recipe (03, Shade), governs the build
├── prism.lock            required — pins every source and crate (§5)
└── vendor/
    ├── crate/           byte-identical registry .crate files, named
    │                    <name>-<version>.crate, one per [[crate]] lock entry
    └── src/<i>/         one unpacked tree per non-crates-io source of the
                         recipe, by source index
```

The bundle's `prism.shade` must evaluate **offline** — no channel fetch, no
network fixed-output fetch at bundle-build time. `shade bundle`
([`07`](07-cli.md)) pins every channel and source the recipe references into
`prism.lock` before vendoring, so bundle evaluation reads only pinned data.
`TODO(open):` a bundle whose recipe imports a channel must vendor the
channel tree too (as a `vendor/channel/<name>/` entry) so evaluation is fully
self-contained — specify the channel-vendor layout when channels land
([`shade 06 §3`](../shade/06-imports.md#3-channels)); flagged.

**Resolve = fetch = read the bundle**, offline:

1. Evaluate `prism.shade` against `prism.lock`; recipe/lock mismatch is an
   error, as ever (§5 rules).
2. Every lockfile pin must have a vendor entry; each entry is verified
   against its pin exactly as a network fetch would be — `.crate` files by
   `sha256`, trees by tree hash (§3.3 algorithm). Missing entry or mismatch
   fails closed; there is **no network fallback** inside a bundle.
3. Verified entries are ingested as ordinary source derivations (§2).

Vendored *git* sources are stored and verified as trees: bundle creation
(`shade bundle`, [`07`](07-cli.md)) records `tree` alongside `commit` in the
bundled lockfile, since a stripped checkout cannot re-verify a commit hash.
This is the same tree-hash backstop [`08 §4`](08-security.md#4-source-authenticity)
wants for git sources generally.

**Input-addressing.** The hash covers the vendored source *and* the recipe,
by construction: the recipe evaluates to the CDF being hashed, and every
vendored entry contributes its pinned identity (`sha256` / `tree`) as a
`source.*`/crate input ([`02 §3.3`](02-store.md#33-hash-inputs)). Because
those identities are the same ones a network resolution would have pinned,
**a bundle build lands at the identical store path as an online build of the
same recipe + lockfile** — vendoring changes the transport, never the
derivation. Offline reproducibility falls out: same bundle ⇒ same CDF bytes
⇒ same digest, on any machine.

**Bundle identity.** The whole bundle has one identity: the §3.3 tree hash
over the bundle directory. It is used to pin a bundle as an **input** from
*outside* — never as a direct install target
([`07 §1`](07-cli.md#1-prism-reference-forms)):

- As a `pspackage` input of another prism — a Shade input-spec attrset in the
  `inputs` set ([`shade 05 §4`](../shade/05-derivation.md#4-sources)):

  ```
  { type = "pspackage"; path = ./deps/mylib-0.3.0.pspkg; }
  # or a bundle-repo reference, 05 §2
  ```

  The bundled prism's own `outputs` govern the build; the outer entry
  contributes only the pin. CDF identity keys: `source.<i>.type=pspackage`,
  `source.<i>.tree` (64 lowercase hex, the bundle tree hash).
- As a dependency: bundle repositories in the prism registry,
  [`05 §2`](05-dependencies.md#2-shade-level-resolution).

Trust: a bundle bypasses first-resolution TOFU (§1 — its pins were created
by whoever built the bundle) but not verification; provenance
considerations in [`08 §4`](08-security.md#4-source-authenticity).

## 4. Fetch cache {#4-fetch-cache}

`/shade/cache/` holds verified downloads keyed by their content identity:

```
/shade/cache/crate/<sha256>.crate
/shade/cache/git/<url-digest>/          bare repo, url-digest = 32-char base32 BLAKE3 of URL
```

Cache entries are an optimization only: every use re-verifies against the
pinned identity, a cache miss falls back to the network, and GC may delete
any entry at any time ([`02 §8`](02-store.md#8-non-durable-areas)). The cache
is also the offline-install vehicle: pre-populating it (e.g. from the dev
host) lets fetch succeed with no network.

## 5. Lockfile {#5-lockfile}

`prism.lock`, TOML (a machine-written state serialization — not a recipe
language, [`01 §1`](01-overview.md#1-goals)), lives at the prism root beside
`prism.shade`, pins the prism's whole **input closure** (§1), and is copied
into each generation ([`02 §5`](02-store.md#5-generations)). It is the
flake.lock analog. Committing it to VCS is expected practice. Machine-written,
human-reviewable; shade rewrites it as a whole (no partial edits, stable sort
order → clean diffs).

**Unification.** The prism model **collapses the old two-lockfile split**: a
prism's `inputs` cover both its build sources *and* the evaluation inputs
(channels/other prisms) that older docs pinned separately in a `shade.lock`
([`shade 06 §4`](../shade/06-imports.md#4-shade-lock)). One prism → one
`prism.lock` covering every input identity + resolved commit/tree. `TODO(open):`
the **frozen unified schema** — whether the per-input tables below absorb the
channel-pin table verbatim, and the exact field set — is still open and lands
with the channel design ([`05 §2`](05-dependencies.md#2-shade-level-resolution)
`TODO`, [`shade 08 §5`](../shade/08-interop.md#5-unified-lockfile)). Until
frozen, treat the `[[source]]`/`[[crate]]`/`[[dep]]` tables below as the
build-input portion and the [`shade 06 §4`](../shade/06-imports.md#4-shade-lock)
`[[channel]]` table as the eval-input portion of the same `prism.lock`.

```toml
schema = 1

# One entry per [[source]] of the recipe, same order, index recorded.
[[source]]
index = 0
type = "crates-io"
crate = "rkilo"
version = "1.2.0"          # exact, post-resolution
sha256 = "9f1c…e0f"

[[source]]
index = 1
type = "git"
url = "https://github.com/user/repo"
requested = "v1.2.0"       # what the recipe said (branch/tag/short hash)
commit = "adf2135f…40hex"  # what it resolved to
submodules = false

[[source]]
index = 2
type = "local"
path = "../mylib"          # informational
tree = "77c3…64hex"

[[source]]
index = 3
type = "pspackage"
path = "./deps/mylib-0.3.0.pspkg"   # informational
tree = "a91d…64hex"        # bundle tree hash (§3.4)

# The resolved Cargo crate graph (05 §3), one entry per crate instance.
[[crate]]
name = "libc"
version = "0.2.161"
sha256 = "…"
features = ["default", "std"]   # post-unification feature set, sorted
deps = ["cfg-if 1.0.0"]         # "name version" of resolved crate deps, sorted

# shade-level dep pins (05 §2): name -> version chosen.
[[dep]]
name = "lythos-libstd"
version = "0.3.0"
```

Rules:

- Every identity field in a CDF `source.*`/crate source key **must** come
  from the lockfile; resolution never feeds the hasher directly. This makes
  "same recipe + same lockfile ⇒ same store paths" the reproducibility
  contract, on any machine, network up or down.
- A recipe/lockfile mismatch (recipe source not in lock, requirement no
  longer satisfied by pin, local tree hash drift) is an error naming the fix
  (`shade lock`); shade never silently re-resolves during `install`.
- `[[crate]]` entries are the authority for the Cargo layer; how they are
  produced from Cargo metadata (and their relation to upstream `Cargo.lock`
  when the source ships one) is [`05 §3`](05-dependencies.md#3-cargo-integration).
