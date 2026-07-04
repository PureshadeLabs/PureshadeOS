# rpkg — Command Surface

The `rpkg` CLI. **[rpkg-local]** in full
([`01 §5`](01-overview.md#5-os-general-vs-rpkg-local)). The existing
`pkg/rpkg` stub's usage screen is superseded by this doc (its `update`,
`search`, `snapshot`, `status` commands are respectively deferred, deferred,
replaced by generations, and folded into `generations list`).

Conventions:

- Exit codes: `0` success, `1` operation failed (build error, verification
  mismatch), `2` usage error, `3` state error (lockfile drift, dirty db,
  held locks).
- All mutation commands print the resulting generation number as their last
  line: `generation 7 active`.
- `--dry-run` on every mutating command: print the plan (derivations to
  build, store paths to create, generation diff), change nothing.
- Structured output: `--json` on read-only commands
  (`TODO(open):` schema per command — defer until first consumer exists;
  don't hand-design speculatively).

---

## 1. Package argument forms

Commands taking `<pkg>` accept:

| Form | Meaning |
|---|---|
| `name` | resolve in the recipe universe — recipes and bundle repos ([`05 §2`](05-dependencies.md#2-rpkg-level-resolution)) |
| `name@<semver-req>` | ditto, constrained |
| `./path/to/recipe.rpkg.toml` | local recipe file ([`03`](03-recipe-format.md)) |
| `./path/to/<name>.pspkg` | PsPackage bundle — recipe + vendored sources, builds fully offline ([`04 §3.4`](04-sources.md#34-pspackage)) |
| `<git-url>` | git source; in-repo recipe required unless `--unsafe` ([`04 §3.2`](04-sources.md#32-git)) |
| `<git-url>#<rev>` | ditto, pinned to branch/tag/commit |

## 2. Commands

### `rpkg install <pkg>… [--unsafe] [--dry-run]`

Resolve → lock (create lockfile entries if absent; refuse on drift with
exit 3, [`04 §5`](04-sources.md#5-lockfile)) → build what's missing
([`05 §6`](05-dependencies.md#6-build-order-and-scheduling)) → create new
generation with the packages added, `requested = true`
([`02 §5`](02-store.md#5-generations)) → activate
([`02 §6`](02-store.md#6-activation)).

`--unsafe`: required for a git URL without in-repo recipe; synthesizes the
default recipe ([`03 §7`](03-recipe-format.md#7-unsafe-default-recipes)).
Prints a warning block before building and requires an interactive `yes`
unless `--yes` (risks: [`08 §3`](08-security.md#3-unsafe)). Refused entirely
for `name` forms — the universe is supposed to have recipes.

### `rpkg remove <pkg>…`

New generation without the named packages. Removing a package that others
still depend on: error listing the dependents (exit 3). Removing a
`requested = false` package: error suggesting the requester. Store paths are
untouched (that's `gc`).

### `rpkg build <pkg>… [--keep-failed]`

Resolve + build into the store, **no generation change**. Prints the store
path(s). For a local recipe, refreshes local-source lockfile entries
([`04 §3.3`](04-sources.md#33-local)). This is the development loop command;
`--keep-failed` keeps the build dir even on success for inspection.

### `rpkg lock <recipe>`

Run resolution only; write/refresh `rpkg.lock`
([`04 §5`](04-sources.md#5-lockfile)). The only command (besides
first-install) that touches the network for *resolution*; the only command
that may change pins.

### `rpkg bundle <recipe> [-o <dir>]`

Produce a PsPackage from a recipe + its lockfile: fetch and verify every
pinned source and crate, vendor them per the bundle layout, record tree
hashes for vendored git sources ([`04 §3.4`](04-sources.md#34-pspackage)).
The resulting `.pspkg` builds offline, forever, at the same store paths an
online build would produce. Requires an up-to-date lockfile (exit 3 on
drift); the network use is fetch-only — no pins change.

### `rpkg generations [list]`

List generations: number, created, reason, package-count, marker on
`current`. `list` is the default subcommand.

### `rpkg generations diff <A> [<B>]`

Package-level diff between generations (default `B` = current): added /
removed / version-changed / store-path-changed-same-version (i.e. rebuilt —
input drift made visible).

### `rpkg generations delete <N>… | --keep-last <K>`

Delete generation records (never `current`; error). This is what actually
releases store paths for GC ([`02 §7.1`](02-store.md#7-garbage-collection)).
Warn about the running-process window documented there.

### `rpkg rollback [<N>]`

New generation copying generation `N`'s manifest (default: the generation
before current), then activate ([`02 §5–6`](02-store.md#5-generations)).
History stays append-only; `rollback` twice returns to where you started.

### `rpkg gc [--dry-run] [--cache-max-age <d>] [--force]`

Mark-and-sweep per [`02 §7.3`](02-store.md#7-garbage-collection). Reports
paths and bytes freed. `--force` overrides the in-flight-build refusal
(documented as dangerous, not default).

### `rpkg info <pkg>`

For an installed package: version, store path, `unsafe` flag, requested
flag, deriver digest, closure size. For a not-installed recipe: what install
would do.

### `rpkg verify`

Re-walk `/r/db/valid/` against `/r/store/` (existence, grammar,
declared-output presence). Input-addressing means content can't be
re-verified against the path name ([`02 §3.3`](02-store.md#33-hash-inputs));
this checks structural integrity only. Exit 1 on any finding.

## 3. Deferred commands

- `rpkg update` — re-resolve pins and rebuild the world; needs channels
  ([`05 §2`](05-dependencies.md#2-rpkg-level-resolution) `TODO`) to mean
  anything. Deferred.
- `rpkg search` — needs an indexed recipe universe / channel metadata.
  Deferred with it.

## 4. UX rules

- Never auto-resolve on drift; name the command that fixes it
  ([`04 §5`](04-sources.md#5-lockfile) rules).
- Build output: one status line per derivation
  (`building 3/14 serde-1.0.219`), full log streamed only with `-v`,
  always on disk at `/r/log/…` ([`02 §8`](02-store.md#8-non-durable-areas)) —
  print the log path on failure.
- Every activation prints the generation number and, if boot-critical
  packages changed, a note that the boot rollback flag is armed
  ([`02 §6.2`](02-store.md#6-activation)).
- Color/progress only on a TTY; plain lines otherwise.
