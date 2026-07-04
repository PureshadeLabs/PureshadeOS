# rpkg — Recipe Format

Recipes are declarative TOML files describing how to build one package. They
are what humans write and review; rpkg *compiles* a recipe plus a lockfile
([`04 §5`](04-sources.md#5-lockfile)) into a derivation in CDF
([`02 §3.2`](02-store.md#32-canonical-derivation-form-cdf)). The recipe
format is **[rpkg-local]** — the OS-general layer sees only derivations
([`01 §5`](01-overview.md#5-os-general-vs-rpkg-local)).

File name: `<name>.rpkg.toml` standalone, or `rpkg.toml` at the root of a git
repository (the *in-repo recipe*, [`04 §3.2`](04-sources.md#32-git)).

There is no expression language: no interpolation, no conditionals, no
includes. A recipe that can't be expressed in this schema motivates a schema
change, not an escape hatch. Two deliberate exceptions: the `$out` /
`$src<i>` substitution variables in phase strings (§5) and env values (§5.2).

Unknown keys anywhere in the file are a **hard error** (they would silently
not participate in the hash).

---

## 1. Complete schema at a glance

```toml
schema = 1                     # required, recipe format version

[package]
name = "rkilo"                 # required
version = "1.2.0"              # required
description = "text editor"    # optional
license = "MIT"                # optional, SPDX expression
boot-critical = false          # optional, default false — see 02 §6.2

[[source]]                     # one or more
type = "crates-io"             # "crates-io" | "git" | "local" | "pspackage"
# ... type-specific keys, §4

[deps]
build = ["lythos-libstd"]      # optional, rpkg package names — 05 §2
runtime = []                   # optional

[build]
phases = [                     # optional — defaults per source type, §5.1
  "cargo build --release --offline --target $TARGET",
  "install -m755 target/$TARGET/release/rkilo $out/bin/rkilo",
]

[build.env]                    # optional
RUSTFLAGS = "-C opt-level=3"

[outputs]                      # required
bin = ["rkilo"]                # files that must exist under $out/bin/
lib = []
share = []
```

---

## 2. `[package]` {#2-package}

| Key | Type | Required | Constraints |
|---|---|---|---|
| `name` | string | yes | after normalization must match `[a-z0-9][a-z0-9_-]*`, ≤ 64 bytes ([`02 §2`](02-store.md#2-store-path-format)). Normalization: ASCII-lowercase; any other character is an error (no lossy mapping — a recipe author fixes the name, rpkg doesn't guess) |
| `version` | string | yes | must match `[0-9a-z.+-]+`, ≤ 32 bytes. Semver strongly recommended; required when the package is depended on with a version requirement ([`05 §2`](05-dependencies.md#2-rpkg-level-resolution)) |
| `description` | string | no | ≤ 256 bytes; not hashed, not in CDF — display only |
| `license` | string | no | SPDX license expression; not hashed — display/audit only |
| `boot-critical` | bool | no | default `false`. If true, activating a generation that adds/changes this package arms the boot rollback flag ([`02 §6.2`](02-store.md#6-activation)) |

CDF mapping: `name`, `version` copied verbatim. `description` and `license`
are deliberately excluded from the hash: they don't affect build output, and
excluding them lets docs-only recipe edits not force rebuilds.

`TODO(open):` whether `boot-critical` belongs in the hash. It doesn't change
the build, but it changes activation behavior; current decision is **not
hashed**, carried in the generation manifest instead
([`02 §5`](02-store.md#5-generations)). Revisit if manifest-only carriage
proves fragile.

## 3. `[[source]]` — array of tables

One or more sources. Source *order is significant*: it fixes the `source.<i>`
CDF indexes and the `$src0`, `$src1`, … variables (§5.2). Most packages have
exactly one source. The type-specific keys, their lockfile resolution, and
their exact CDF identity keys are specified in
[`04 §3`](04-sources.md#3-resolution-per-source-type) — not restated here.

```toml
[[source]]
type = "crates-io"
crate = "rkilo"          # defaults to package.name
version = "1.2.0"        # semver requirement; resolved + pinned by the lockfile

[[source]]
type = "git"
url = "https://github.com/user/repo"
rev = "v1.2.0"           # branch, tag, or commit; locked to a full commit hash

[[source]]
type = "local"
path = "../mylib"        # relative to the recipe file; locked to a tree hash

[[source]]
type = "pspackage"
path = "./deps/mylib-0.3.0.pspkg"  # self-contained bundle; locked to its
                                   # bundle tree hash (04 §3.4)
```

## 4. `[deps]`

| Key | Type | Meaning |
|---|---|---|
| `build` | array of strings | rpkg packages whose outputs are available during the build (compilers, code generators, libraries linked at build time) |
| `runtime` | array of strings | rpkg packages that must be in the closure of the output but aren't needed to build (e.g. a binary this package `exec`s) |

Entries are rpkg package names with optional semver requirement:
`"lythos-libstd"` or `"lythos-libstd@^0.3"`. Resolution — from what recipe
universe names are resolved, version selection, cycles — is
[`05 §2`](05-dependencies.md#2-rpkg-level-resolution).

**Cargo crate dependencies do not appear here.** They come from the source's
own Cargo metadata and are resolved per
[`05 §3`](05-dependencies.md#3-cargo-integration). `[deps]` is for
dependencies *between rpkg packages*.

CDF mapping: each resolved build dep contributes a `dep.<i>` store path.
Runtime deps contribute `dep.<i>` as well (they must exist before the build
so their paths can be embedded and reference-scanned,
[`02 §7.2`](02-store.md#72-references)). The build/runtime distinction is
sandbox policy ([`06 §3`](06-build.md#3-sandbox)), not hash-relevant: both
kinds are hash inputs identically.

## 5. `[build]` {#5-build}

### 5.1 `phases`

An array of command strings, executed in order inside the sandbox
([`06 §2`](06-build.md#2-phases) defines the fixed phase skeleton around
them; these strings are the *build* and *install* payload). Each string is
one command line: `argv` split on unquoted whitespace, single- and
double-quote grouping, **no** shell — no pipes, no redirection, no `&&`, no
globbing, no variable expansion beyond §5.2. A step that needs shell logic
ships a script in its source and invokes it.

If `phases` is omitted, the default for the first source's type applies —
defined in one place, [`03 §7`](#7-unsafe-default-recipes) table, because the
`--unsafe` synthesized recipe uses exactly the same defaults.

CDF mapping: each string verbatim as `phase.<i>`, after §5.2 variables are
*left unexpanded* (`$out` hashes as the literal bytes `$out` — the concrete
store path can't be an input to its own hash).

### 5.2 Substitution variables

Recognized in phase strings and `[build.env]` values, expanded at build time:

| Variable | Expands to |
|---|---|
| `$out` | the output store path being built |
| `$src0`, `$src1`, … | store path of source *i* ([`04 §2`](04-sources.md#2-source-derivations)) |
| `$TARGET` | the `system` value ([`02 §3.3`](02-store.md#33-hash-inputs)) |
| `$JOBS` | build parallelism (not hashed; determinism requirement [`06 §6`](06-build.md#6-determinism)) |

`$` followed by anything else is a literal (no escaping mechanism needed:
`$$` is not special).

### 5.3 `[build.env]`

String-to-string map of extra environment variables, set after the sandbox's
scrubbed base environment ([`06 §4`](06-build.md#4-environment)). Values may
use §5.2 variables. CDF mapping: `env.<KEY>=<value>` per entry, unexpanded,
sorted by key. Keys must match `[A-Z_][A-Z0-9_]*`. Attempting to set a
variable the sandbox defines as fixed (e.g. `PATH`, `HOME`,
`SOURCE_DATE_EPOCH` — the fixed list is [`06 §4`](06-build.md#4-environment))
is an error.

## 6. `[outputs]` {#6-outputs}

Declares what the build must produce, relative to `$out`:

| Key | Type | Meaning |
|---|---|---|
| `bin` | array | files that must exist and be executable under `$out/bin/` |
| `lib` | array | files under `$out/lib/`; entries may end in `*` for a prefix match (rlib names embed metadata hashes, [`05 §4`](05-dependencies.md#4-crate-derivations)) |
| `share` | array | files or directories under `$out/share/` |

At least one entry across the three arrays is required. Registration fails
the build if a declared output is missing, and fails it if anything exists
in `$out` outside `bin/`, `lib/`, `share/`
([`06 §5`](06-build.md#5-registration)). CDF mapping: `output.<i>` in recipe
order, each entry as `bin/rkilo`, `lib/libfoo*`, etc.

## 7. `--unsafe` default recipes {#7-unsafe-default-recipes}

Fixed decision: a git URL with no in-repo `rpkg.toml` can still be built with
`rpkg install --unsafe <url>` ([`07`](07-cli.md)). rpkg then *synthesizes* a
recipe:

1. `name` = last path segment of the URL, stripped of `.git`, normalized
   per §2; `version` = `0.0.0+git.<12-hex-commit-prefix>`.
2. One `[[source]]` of type `git` at the locked commit.
3. `[deps]` empty.
4. `phases` = the **default phase table** below, selected by probing the
   checkout root.
5. `[outputs]` inferred: for the cargo default, every `[[bin]]` target from
   `cargo metadata` becomes an `outputs.bin` entry.

Default phase table (normative for omitted `phases` in real recipes too,
§5.1):

| Probe (checkout root) | Default phases |
|---|---|
| `Cargo.toml` present | `cargo build --release --offline --target $TARGET` then, per bin target `<t>`: `install -m755 target/$TARGET/release/<t> $out/bin/<t>` |
| anything else | error — `--unsafe` refuses to guess beyond Cargo (`TODO(open):` probe table grows with supported ecosystems) |

The synthesized derivation gets `unsafe=1` in its CDF
([`02 §3.3`](02-store.md#33-hash-inputs)) so an unsafe build can never
collide with, and thus never be shadowed by or substitute for, a reviewed
recipe's build of the same source. The generation manifest marks the package
`unsafe = true` so [`07`](07-cli.md) can display it and
[`08 §3`](08-security.md#3-unsafe) risks stay visible post-install.

`--unsafe` weakens *review*, not *isolation*: the synthesized build runs in
the same sandbox as any other ([`06 §3`](06-build.md#3-sandbox)). What you
lose is any human having looked at what the build does within its
permissions — see [`08 §3`](08-security.md#3-unsafe).

## 8. Recipe → derivation compilation (summary)

Pipeline (each step's own doc is authoritative):

1. Parse + validate recipe (this doc). Unknown key ⇒ error.
2. Resolve sources against the lockfile; create/verify lockfile entries
   ([`04`](04-sources.md)).
3. Resolve `[deps]` and Cargo crate graph to concrete store paths, building
   dep derivations first as needed ([`05`](05-dependencies.md)).
4. Emit CDF ([`02 §3`](02-store.md#3-input-addressing)); the digest is the
   store path. If `/r/db/valid/<digest>` exists, done — no build.
5. Otherwise build ([`06`](06-build.md)).
