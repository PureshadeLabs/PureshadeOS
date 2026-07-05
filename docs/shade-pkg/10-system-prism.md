# shade — The System Prism, Pointer, and Per-User Prisms

This doc specs how a running PureshadeOS system is described by a **prism**
([`04 §1`](04-sources.md#1-the-prism)): the **system prism** that builds the OS
and is activated as a generation ([`02 §5`](02-store.md#5-generations)), the
**pointer file** that names it at a stable system path, the bootstrap default
and its one-time migration, and the **per-user prisms** that additional users
own. **[shade-local]**.

Nothing here changes the prism shape (§1 of [`04`](04-sources.md)) or the store
(`02`). A system prism is an ordinary prism whose `outputs` happen to describe
a whole system plus its owner's home; the pointer and `shade os rebuild`
([`07 §2.1`](07-cli.md#21-shade-os)) are the frontend that selects and
activates it.

The prism entry file is always `prism.shade`, written in **Shade**
([`shade`](../shade/01-overview.md)) — the sole recipe language
([`01`](01-overview.md)). There is no other spelling of the entry file.

---

## 1. The system prism {#1-the-system-prism}

A **system prism** is the prism that `shade os rebuild` activates as the current
**system generation** ([`02 §5`](02-store.md#5-generations)). Its `outputs`
build the OS package set **and** perform home-manager-style (HM) activation for
the prism's **owner** — the user who authors and rebuilds it. One prism, two
jobs:

- **System build** — the package set built into a new generation in the
  **system line** `/shade/gen/system/` ([`02 §5`](02-store.md#5-generations)),
  activated by flipping `/shade/gen/system/current` and reached through
  `/lth/bin → /shade/gen/system/current/profile/bin` (`docs/spec/fhs.md`).
- **Owner HM** — the owner's user prism activated into the owner's **own**
  per-user generation line `/shade/gen/users/<owner>/` (§5), exactly as any
  other user. `shade os rebuild` run by the owner builds both lines; the system
  line is privileged, the owner's user line is the owner's own (§5). The two
  activations are **independent flips** (§5), not one atomic generation.

### 1.1 HM activation — what gets written {#11-hm-activation}

A user prism activates into **that user's own profile** under
`/shade/gen/users/<user>/`, by the same symlink mechanism as the system profile
([`02 §5.1`](02-store.md#5-generations)), scoped to the user. Activation writes:

- **Profile symlink set** — a new generation `N/` under
  `/shade/gen/users/<user>/` whose `profile/` is a symlink forest of the user
  prism's declared outputs into `/shade/store/*` (`bin/`, `lib/`, `share/`, …),
  built and fsynced, then activated by flipping
  `/shade/gen/users/<user>/current -> N` ([`02 §6`](02-store.md#6-activation)).
- **Environment** — the user prism's declared session environment (per-package
  exports, HM-managed dotfiles under `/user/home/<user>/`). `TODO(open):` the
  concrete dotfile write/link mechanism (in-place copy vs symlink into the
  profile, backup of pre-existing files) — the Nix-HM generation-symlink model
  is the prior art.
- **PATH composition with the system profile** — a user's shell `PATH` is the
  user profile **ahead of** the system profile:
  `/shade/gen/users/<user>/current/profile/bin` precedes
  `/shade/gen/system/current/profile/bin` (which `/lth/bin` points at). The
  user profile therefore **shadows** system tools by PATH order — no tree merge,
  no build-time collision between the two lines
  ([`02 §5.1`](02-store.md#5-generations)). `TODO(open):` where this PATH order
  is assembled (login shell profile, session manager) and how a user with no
  activated generation degrades to system-only PATH.

The **system-builder prism** is whichever prism the pointer (§2) targets. There
is exactly one at a time.

---

## 2. The pointer file {#2-the-pointer-file}

`/cfg/shade/current.pointer` names the active system prism. It lives on `@cfg`
(read-write, rolls back with the system; `docs/spec/fhs.md`), a **stable system
path** that does not depend on any user profile being mounted.

**Format:** plain UTF-8, **one field per line**, in this fixed order:

```
/user/lyon/.prism      line 1 — prism path (directory holding prism.shade, §5)
workstation            line 2 — output selector (#<selector>, without the '#')
7                      line 3 — resolved system generation number
```

- **Line 1 — prism path.** The source prism directory, a path resolvable at
  rebuild time. Typically a user path on `@home`.
- **Line 2 — selector.** The system output
  ([`shade 08 §4`](../shade/08-interop.md#4-package-set-selection)) to build.
- **Line 3 — resolved generation number.** The `/shade/gen/system/` generation
  the last successful rebuild produced (§5 of [`02`](02-store.md)). This **pins
  the built generation**, so rebuild and boot are reproducible and boot never
  re-evaluates the source prism (§6): boot activates generation N directly.

`shade os rebuild` rewrites all three lines atomically on success (build →
write generation N → update pointer). Trailing newline required; no comments.
`TODO(open):` whether a lock digest is added as a fourth field for
rebuild-time drift detection — deferred until a consumer needs it.

---

## 3. Bootstrap default and first rebuild {#3-bootstrap-default-and-first-rebuild}

A freshly installed system ships a **default system prism** at
`/cfg/shade/prism.shade`. It contains **only what is needed to build the user's
prism — nothing more**: enough of a system to reach a shell and run
`shade os rebuild`. It is not a full desktop.

`/cfg/shade/` is the **prism-authoring area**: it holds the default prism,
prism-authoring reference docs, and the pointer file. After the first rebuild
it does **not** hold the active user config — that lives in the user's prism
(§5). `/cfg/shade/` is authoring + pointer only.

**First rebuild** — `shade os rebuild <path>#<selector>`
([`07 §2.1`](07-cli.md#21-shade-os)):

1. If the default `/cfg/shade/prism.shade` is present, **rename it to
   `/cfg/shade/prism.shade.bak`** and stop using it as the main config.
2. Write `current.pointer` = `<path>#<selector>`.
3. The named user prism becomes the **authoritative** system prism; subsequent
   rebuilds and boots resolve through the pointer (§4).

The rename is one-way per install: once `prism.shade.bak` exists, the default is
retired. It is retained only as the fallback of last resort (§4).

---

## 4. Resolution order {#4-resolution-order}

Resolution depends on whether the pointer file **exists**, not on whether its
target is reachable:

1. **Pointer present** (`/cfg/shade/current.pointer` exists) → it is
   authoritative. `shade os rebuild` uses lines 1–2 (path + selector) as the
   source; boot uses line 3 (the pinned system generation, §6). This is the
   steady state after first rebuild.
2. **Pointer absent** → fall back to **`/cfg/shade/prism.shade.bak`** (the
   retired default, §3), else the live default `/cfg/shade/prism.shade` on a
   never-rebuilt system.

**Pointer present but target unresolvable** (source prism missing, bad
selector, `@home` not mounted): **fail loud.** Do **not** silently fall back to
`.bak` — a present pointer means the default is retired, and silently reverting
to it would activate the wrong system. Specifically:

- **`shade os rebuild`** errors (exit 1) naming the unresolvable target; it
  changes no generation and does not rewrite the pointer.
- **Boot** does not need the source at all — it activates the pinned system
  generation from `/shade/gen/system/` (§6). If even that generation is
  missing/corrupt, boot recovers to the **last-good system generation** in
  `/shade/gen/system/` (the rollback protocol,
  [`02 §6.2`](02-store.md#6-activation)), **not** to `.bak`.

`.bak` fallback applies **only** when the pointer is absent entirely (case 2),
never as a recovery path while a pointer is present.

---

## 5. Per-user prisms {#5-per-user-prisms}

The system-builder prism (§1) builds the system and does HM for its **owner**
only. **Additional users** get their own HM-style **per-user prisms**, separate
from the system-builder prism — each user configures their own home without
touching the system prism.

A user prism lives at **`~/.prism`** — the per-user profile directory under the
user's home (`/user/home/<user>/.prism/`, `docs/spec/fhs.md`) — with entry file
`prism.shade`. Its `outputs` describe that user's HM activation (§1.1), not the
system package set.

**Build/activation — the Nix-HM model.** Each user **builds and activates their
own prism independently**, into their own generation line
`/shade/gen/users/<user>/` (§1.1, [`02 §5`](02-store.md#5-generations)). A user
rebuild is **not** folded into the system generation and does **not** create a
combined atomic super-generation: it flips only `/shade/gen/users/<user>/current`,
leaving `/shade/gen/system/current` and every other user's line untouched. The
command is `shade home rebuild` ([`07 §2.2`](07-cli.md#22-shade-home)).

**Privilege.** A user activates **their own** profile **unprivileged** — no
root. Building into `/shade/store/` is the shared store operation any build
uses (the store services mediate writes, [`02 §1`](02-store.md#1-the-shade-hierarchy));
flipping `/shade/gen/users/<user>/current` requires only ownership of that
user's own line. The **system** line stays privileged: only `shade os rebuild`
(§1, [`07 §2.1`](07-cli.md#21-shade-os)) writes `/shade/gen/system/`, and it
runs privileged.

`TODO(open):` the store-write authorization boundary — how the unprivileged
per-user builder is granted store writes for its own paths without being able
to write the system line's paths (capability scoping;
[`02 §1`](02-store.md#1-the-shade-hierarchy) permissions model). Ownership of
`/shade/gen/users/<user>/` vs. shared `/shade/store/` writes needs an explicit
rule.

---

## 6. Boot dependency {#6-boot-dependency}

**Boot consumes BUILT generations, not source prisms.** This is the load-bearing
rule that decouples boot from user storage.

On **rebuild** (`shade os rebuild`, §1), the built system configuration is
written as a complete generation into **`/shade/gen/system/`** — manifest, lock,
and the profile symlink forest — and the pointer's line-3 generation number
(§2) is updated to it. The source prism and its inputs are **rebuild-time
inputs only**.

On **boot**, the system is activated by pointing
`/shade/gen/system/current` at the pinned generation
([`02 §6`](02-store.md#6-activation)) and reaching it through
`/lth/bin → /shade/gen/system/current/profile/bin`. Boot **never re-evaluates
the source prism and never reads a user path**:

- `/shade/gen/system/` is on the system store (`@core`/store domain), mounted
  before user data (`docs/spec/fhs.md` boot sequence).
- The source prism at `/user/<owner>/.prism` on `@home` being **unmounted at
  boot does not block boot** — boot uses the already-built system generation,
  not the prism.
- Therefore the pointer + source prism are **rebuild-time dependencies, not
  boot-time dependencies**. Boot depends only on `/shade/gen/system/`.

If the pinned generation is missing or fails its stability window, boot recovers
to the **last-good system generation** in `/shade/gen/system/` via the rollback
protocol ([`02 §6.2`](02-store.md#6-activation)) — never to `.bak`, never by
re-reading the prism (§4).

Per-user lines (`/shade/gen/users/<user>/`, §5) are **not** part of boot: they
activate per user at login/session start (§1.1), after `@home` is mounted, and a
user with no built generation simply gets the system-only PATH (§1.1
`TODO(open)`). `TODO(open):` the exact login/session hook that flips a user's
`current` and assembles per-user PATH is a session-manager concern, unspecified
here.
