# shade — Command Surface

The `shade` CLI. **[shade-local]** in full
([`01 §5`](01-overview.md#5-os-general-vs-shade-local)). The existing
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

## 1. Prism reference forms {#1-prism-reference-forms}

**A prism is the sole install unit** ([`04 §1`](04-sources.md#1-the-prism)) —
the flake-analog: a unit declaring pinned *inputs* and computing *outputs*
(packages) from them. There is **no** command form that installs a bare crate,
git repo, local recipe, or `.pspkg` directly; each of those is an *input to a
prism* ([`04 §3`](04-sources.md#3-resolution-per-source-type)), never a standalone target. To
install "a crate" or "a git project" you name (or write) a prism whose output
is that thing.

Commands taking `<prism>` accept a **prism reference**, optionally with an
output selector `#<output>`:

| Form | Meaning |
|---|---|
| `.` / `./path` | local prism — a directory containing `prism.shade`, or the file itself ([`04 §1`](04-sources.md#1-the-prism)); evaluated by shadec ([`09 §6`](09-bootstrap.md#6-evaluator-selection)) |
| `<prism>#<output>` | select output `<output>` from the prism's `outputs` ([`shade 08 §4`](../shade/08-interop.md#4-package-set-selection)); `#a.b.c` for nested ([`shade 02 §6`](../shade/02-grammar.md#6-package-set-selectors)) |
| `name` | resolve in the prism registry ([`05 §2`](05-dependencies.md#2-shade-level-resolution)); first-wins on collision ([`05 §2`](05-dependencies.md#2-shade-level-resolution)) |
| `name@<semver-req>` | ditto, constrained |
| `git+<url>` / `git+<url>?rev=<rev>` | a remote prism — a repo whose root holds `prism.shade`; the `?rev=` pins branch/tag/commit |

The `#<output>` selector is **CLI/argument syntax applied to the prism's
evaluated result**, not part of the `.shade` language
([`shade 08 §4`](../shade/08-interop.md#4-package-set-selection)). When
omitted, the default-output rule
([`shade 08 §4`](../shade/08-interop.md#4-package-set-selection)) applies.

A git *revision* is written `?rev=` on the prism reference itself; it never
collides with `#<output>` — the old `<git-url>#<rev>` ambiguity is gone
because a git URL is now a *prism location*, not a source-install target. The
git repo referenced this way **must** contain a `prism.shade`; a bare repo
without one is not installable (it can only be a *git input* of some other
prism, [`04 §3.2`](04-sources.md#32-git)).

## 2. Commands

`shade` is a **subcommand** CLI: `shade <subcommand> [args] [flags]`. Most
subcommands are flat verbs (`install`, `remove`, `build`, …). Two subcommands
are **groups**: `os` (system-line verbs, [§2.1](#21-shade-os)) and `home`
(per-user-line verbs, [§2.2](#22-shade-home)) — `shade os <verb> …`,
`shade home <verb> …`.

### 2.1 `shade os` — system prism management {#21-shade-os}

The `os` subcommand group operates on the **system prism**
([`10 §1`](10-system-prism.md#1-the-system-prism)) and the **system generation
line** `/shade/gen/system/`. **Privileged** — system-line writes need root.

#### `shade os rebuild [<prism>[#<selector>]] [--dry-run]`

Build and activate the named prism as the **system prism**
([`10 §1`](10-system-prism.md#1-the-system-prism)): resolve its input closure
and build its system output exactly as `install` does (below), create and
activate a new generation in `/shade/gen/system/`
([`02 §5–6`](02-store.md#5-generations)), and — when run by the owner — activate
the owner's own user line too ([`10 §1`](10-system-prism.md#1-the-system-prism)).

Pointer handling ([`10 §2–3`](10-system-prism.md#2-the-pointer-file)):

- **First rebuild** — if the default `/cfg/shade/prism.shade` is present, rename
  it to `/cfg/shade/prism.shade.bak` and stop using it as the main config.
- On success, rewrite `/cfg/shade/current.pointer` (three lines: prism path,
  selector, resolved system generation number, [`10 §2`](10-system-prism.md#2-the-pointer-file)).
  The pinned generation is what boot activates ([`10 §6`](10-system-prism.md#6-boot-dependency)).

With no `<prism>` argument, rebuild the pointer's current target (lines 1–2). If
the pointer is present but its target is **unresolvable**, **fail loud** (exit 1,
name the target) — never silently fall back to `.bak`
([`10 §4`](10-system-prism.md#4-resolution-order)). `--dry-run` prints the plan
and the pointer change, mutates nothing.

#### `shade os clean [--keep-last <K>] [--dry-run]`

**Prune old system generations, then collect.** Two steps:

1. Delete old `/shade/gen/system/` generation records (default policy
   `TODO(open):`; `--keep-last <K>` keeps the K newest plus `current`), never
   deleting `current` — same record deletion as
   `shade generations delete` (below).
2. Trigger a store sweep (`shade gc`, below) to reclaim paths the pruned
   generations were the last to reference.

`os clean` is thus **generation pruning + gc**, distinct from bare `gc` (which
only collects already-unreferenced paths and prunes no generation). `--dry-run`
reports what would be pruned and freed. `TODO(open):` whether `os clean` prunes
only the system line or also offers a system-wide sweep across user lines
(user-line pruning is `shade home clean`, [§2.2](#22-shade-home)).

### 2.2 `shade home` — per-user prism management {#22-shade-home}

The `home` subcommand group operates on the caller's **own** per-user prism
([`10 §5`](10-system-prism.md#5-per-user-prisms)) and their **own** generation
line `/shade/gen/users/<user>/`. **Unprivileged** — a user activates only their
own line; **no root** ([`10 §5`](10-system-prism.md#5-per-user-prisms)). Contrast
`shade os rebuild` (system line, privileged, [§2.1](#21-shade-os)).

#### `shade home rebuild [~/.prism[#<selector>]] [--dry-run]`

Build and activate the caller's user prism — default `~/.prism`
([`10 §5`](10-system-prism.md#5-per-user-prisms)) — into a new generation under
`/shade/gen/users/<user>/`, then flip **only** `/shade/gen/users/<user>/current`
([`10 §1.1`](10-system-prism.md#11-hm-activation)). Writes the user's profile
symlink set, HM environment, and dotfiles; PATH composes ahead of the system
profile ([`10 §1.1`](10-system-prism.md#11-hm-activation)). It **never** touches
`/shade/gen/system/` or any other user's line, and is **not** folded into the
system generation ([`10 §5`](10-system-prism.md#5-per-user-prisms)). No pointer
is involved — `current.pointer` names the system prism only.

#### `shade home clean [--keep-last <K>] [--dry-run]`

Prune the caller's own old user generations under `/shade/gen/users/<user>/`,
then trigger `gc` — the per-user analog of `shade os clean` ([§2.1](#21-shade-os)),
scoped to the caller's line and unprivileged.

`TODO(open):` administrative operations across **all** user lines (e.g. root
pruning every user's history), and behavior when a user is deleted (their
`/shade/gen/users/<user>/` line's disposition) — unspecified.

### `shade install <prism>[#<output>]… [--dry-run]`

Resolve the prism's **input closure** from its lock
([`04 §5`](04-sources.md#5-lockfile); create entries if absent, refuse on
drift with exit 3) → evaluate its `outputs` and select the requested
output(s) ([`04 §1`](04-sources.md#1-the-prism)) → build what's missing
([`05 §6`](05-dependencies.md#6-build-order-and-scheduling)) → create a new
generation with the selected package(s) added, `requested = true`
([`02 §5`](02-store.md#5-generations)) → activate
([`02 §6`](02-store.md#6-activation)).

There is no `--unsafe` install path: a prism carries its own build logic in
its `outputs`, so there is no bare-repo-with-no-recipe case to synthesize
around. A raw source input with no builder is **not** buildable — shade never
synthesizes a `builder = default`; the enclosing prism must give every
buildable input explicit `phases`/`outputs` or resolution fails
([`03 §7`](03-recipe-format.md#7-unsafe-default-recipes),
[`04 §3.2`](04-sources.md#32-git); decided **explicit-required**).

### `shade -t <prism>[#<output>]… [-- <cmd> [args…]]` {#shade-t}

**Temporary environment** — the nix-shell analog. Makes the named packages
available in an **ephemeral subshell** without installing them to any profile.

1. Resolve each argument to package outputs exactly as `install` does
   (prism input closure from the lock, or the prism registry
   [`05 §2`](05-dependencies.md#2-shade-level-resolution) for a bare `name`;
   the `shadepkgs` prism is the default registry, [`shade 06 §3`](../shade/06-imports.md#3-channels)).
2. Build/fetch every output and its runtime closure into the store
   ([`02`](02-store.md)) — the store is the one durable side effect, and it is
   shared with everything else (a temp env never rebuilds a path a profile
   already has).
3. Construct a **transient environment**: prepend the selected outputs' `bin/`
   to `PATH`, export the usual per-package env, and exec either a subshell
   (default) or `<cmd>` after `--`. The environment lives only for that
   process tree.
   The session's store paths are held live by a transient GC root
   **`/shade/roots/tmp-<pid>`** ([`02 §7.1`](02-store.md#7-garbage-collection)),
   written at start.
4. On exit: nothing persists. The `tmp-<pid>` root is **removed**, so **no GC
   root is held beyond the session**, **no generation is created**
   ([`02 §5`](02-store.md#5-generations)), and **no profile is touched**
   ([`02 §5.1`](02-store.md#5-generations)). A later `gc` may reclaim the paths
   once no temp env references them. Running `shade -t` twice for the same
   packages is cheap (store hit), never cumulative.

**`--pure`.** Start from a **cleared** environment rather than inheriting the
caller's — mirroring `nix-shell --pure` exactly. The environment is wiped and
only a fixed **whitelist** of variables is passed through from the caller
(then step 3's `PATH` and per-package exports are applied on top). The
whitelist mirrors Nix's `keepVars` one-for-one, with shade-namespaced names
substituted for the three Nix-internal ones:

| Passed through | Notes |
|---|---|
| `HOME` | |
| `XDG_RUNTIME_DIR` | |
| `USER` | |
| `LOGNAME` | |
| `DISPLAY` | |
| `PATH` | inherited value kept, then **overwritten** by step 3's temp-env `PATH` |
| `TERM` | |
| `TZ` | |
| `PAGER` | |
| `SHLVL` | |
| `IN_SHADE_SHELL` | shade's `IN_NIX_SHELL` analog; set to mark an active temp env |
| `SHADE_SHELL_PRESERVE_PROMPT` | `NIX_SHELL_PRESERVE_PROMPT` analog |
| `SHADE_BUILD_SHELL` | `NIX_BUILD_SHELL` analog |

Every other caller variable is dropped. Without `--pure`, the caller's full
environment is inherited and only `PATH`/per-package vars are layered on.

Other flags: `--dry-run` (print what would be built, enter nothing). `shade -t`
is read-only with respect to profiles and generations by construction; it is
the only "install-like" command that creates no generation.

### `shade remove <prism>…`

New generation without the named packages. Removing a package that others
still depend on: error listing the dependents (exit 3). Removing a
`requested = false` package: error suggesting the requester. Store paths are
untouched (that's `gc`).

### `shade build <prism>[#<output>]… [--keep-failed]`

Resolve + build the selected output(s) into the store, **no generation
change**. Prints the store path(s). For a local prism, refreshes local-input
lockfile entries ([`04 §3.3`](04-sources.md#33-local)). This is the
development loop command; `--keep-failed` keeps the build dir even on success
for inspection.

### `shade lock <prism>`

Run resolution only; write/refresh the prism's lock `prism.lock`
([`04 §5`](04-sources.md#5-lockfile)) — resolve every input to a pinned
identity. The only command (besides first-install) that touches the network
for *resolution*; the only command that may change pins.

### `shade bundle <prism> [-o <dir>]`

Produce a PsPackage from a prism + its lock: fetch and verify every pinned
input and crate, vendor them per the bundle layout, record tree hashes for
vendored git inputs ([`04 §3.4`](04-sources.md#34-pspackage)). The resulting
`.pspkg` is a self-contained, offline-buildable form of the prism, and is
itself usable as a `pspackage` input of another prism
([`04 §3.4`](04-sources.md#34-pspackage)); it builds at the same store paths
an online build would produce. Requires an up-to-date lock (exit 3 on drift);
the network use is fetch-only — no pins change.

### `shade generations [list]`

List generations: number, created, reason, package-count, marker on
`current`. `list` is the default subcommand.

### `shade generations diff <A> [<B>]`

Package-level diff between generations (default `B` = current): added /
removed / version-changed / store-path-changed-same-version (i.e. rebuilt —
input drift made visible).

### `shade generations delete <N>… | --keep-last <K>`

Delete generation records (never `current`; error). This is what actually
releases store paths for GC ([`02 §7.1`](02-store.md#7-garbage-collection)).
Warn about the running-process window documented there.

### `shade rollback [<N>]`

New generation copying generation `N`'s manifest (default: the generation
before current), then activate ([`02 §5–6`](02-store.md#5-generations)).
History stays append-only; `rollback` twice returns to where you started.

### `shade gc [--dry-run] [--cache-max-age <d>] [--force]`

Mark-and-sweep per [`02 §7.3`](02-store.md#7-garbage-collection). Reports
paths and bytes freed. `--force` overrides the in-flight-build refusal
(documented as dangerous, not default).

### `shade info <prism>[#<output>]`

For an installed package: version, store path, requested flag, deriver
digest, closure size. For a not-installed prism: what install would do
(resolved input closure, outputs, store paths).

### `shade verify`

Re-walk `/shade/db/valid/` against `/shade/store/` (existence, grammar,
declared-output presence). Input-addressing means content can't be
re-verified against the path name ([`02 §3.3`](02-store.md#33-hash-inputs));
this checks structural integrity only. Exit 1 on any finding.

## 3. Deferred commands

- `shade update` — re-resolve a prism's input pins and rebuild the world;
  needs the prism registry / channels
  ([`05 §2`](05-dependencies.md#2-shade-level-resolution) `TODO`) to mean
  anything. Deferred.
- `shade search` — needs an indexed prism registry / channel metadata.
  Deferred with it.

## 4. UX rules

- Never auto-resolve on drift; name the command that fixes it
  ([`04 §5`](04-sources.md#5-lockfile) rules).
- Build output: one status line per derivation
  (`building 3/14 serde-1.0.219`), full log streamed only with `-v`,
  always on disk at `/shade/log/…` ([`02 §8`](02-store.md#8-non-durable-areas)) —
  print the log path on failure.
- Every activation prints the generation number and, if boot-critical
  packages changed, a note that the boot rollback flag is armed
  ([`02 §6.2`](02-store.md#6-activation)).
- Color/progress only on a TTY; plain lines otherwise.
