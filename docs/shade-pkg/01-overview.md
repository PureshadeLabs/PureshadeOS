# shade — Overview

shade is the PureshadeOS package manager: **source-based**, **input-addressed**,
**Nix-style**. Packages are described by **Shade** recipes
([`shade`](../shade/01-overview.md)) — a pure, lazy, functional language
evaluated to derivations — built in isolation, and installed into an immutable
content store at computed paths. Installation is the atomic activation of a
new *generation*; rollback is switching back to a prior one.

Shade is the **sole** recipe frontend. There is no TOML recipe format; every
recipe is a `.shade` file evaluated by **shadec** to CDF
([`02 §3.2`](02-store.md#32-canonical-derivation-form-cdf)). TOML survives in
the package system **only** as a machine-written serialization for state that
no human authors — lockfiles ([`04 §5`](04-sources.md#5-lockfile)), generation
manifests ([`02 §5`](02-store.md#5-generations)), the bootstrap pin
([`09 §3`](09-bootstrap.md#3-trust-and-pinning)) — never as a recipe or
config language.

This document set is the design specification. It precedes and governs the
implementation (the current `pkg/rpkg` crate is a CLI stub). Specs only — no
implementation code lives here.

Doc set:

| Doc | Contents |
|-----|----------|
| [`01-overview.md`](01-overview.md) | goals, glossary, relation to Nix, OS-general vs shade-local boundary |
| [`02-store.md`](02-store.md) | the `/shade/` hierarchy, store path format, input-addressing hash, generations, atomic activation, rollback, GC |
| [`03-recipe-format.md`](03-recipe-format.md) | recipes are Shade; frontend-independent CDF field policy; `--unsafe` synthesis |
| [`04-sources.md`](04-sources.md) | the **prism** (sole install unit), its four input types (crates.io, git, local, PsPackage), source derivations, the prism lock |
| [`05-dependencies.md`](05-dependencies.md) | dependency resolution, Cargo metadata integration, derivation graph, build order |
| [`06-build.md`](06-build.md) | isolated build model, phases, sandbox, determinism goals |
| [`07-cli.md`](07-cli.md) | command surface and UX |
| [`08-security.md`](08-security.md) | trust model, `--unsafe` risks, source authenticity, sandbox guarantees |
| [`09-bootstrap.md`](09-bootstrap.md) | the shadec bootstrap: seed evaluator, trust/pin, rebuild through shade |

The split follows the seed proposal, plus 09 for the bootstrap: each doc owns
one layer, and the layering is real — 02 defines the store that 03–06 build
into, 04–05 define the inputs whose hash 02 consumes, 06 consumes all of them,
07–08 sit on top, and 09 defines how the recipe evaluator (shadec) comes to
exist at all. The recipe *language* is a separate doc set
([`shade`](../shade/01-overview.md)); shade references it, never restates it.
Definitions live in exactly one doc and are cross-referenced, never restated
(rfs-v2 house style).

---

## 1. Goals

- **Source-based.** Every installed artifact is built from source on (or for)
  the target system. There is no binary package format and no binary
  distribution channel in v1. (A future binary *substitution* cache is a
  non-goal here but the design must not preclude it — see
  [`08 §6`](08-security.md#6-future-binary-substitution).)
- **Input-addressed store.** A build's location in the store is a pure
  function of its *inputs* — recipe, resolved dependencies, source identity,
  build environment — computed **before** the build runs
  ([`02 §3`](02-store.md#3-input-addressing)). Same inputs ⇒ same path ⇒ the
  build can be skipped if the path already exists.
- **Atomic installs, generations, rollback.** No observable intermediate
  states. Every mutation of the installed set produces a new generation;
  activation is one atomic symlink flip; rollback is flipping back
  ([`02 §5`](02-store.md#5-generations)–[`§6`](02-store.md#6-activation)).
- **The prism is the only install unit.** shade installs a **prism** — the
  flake analog: a unit declaring pinned *inputs* and computing *outputs*
  (packages) from them ([`04 §1`](04-sources.md#1-the-prism),
  [`07 §1`](07-cli.md#1-prism-reference-forms)). There is **no** path that
  installs a bare crate, git repo, local recipe, or `.pspkg` — each is only an
  *input to a prism*.
- **Uniform inputs.** crates.io crates, git repositories, local trees, and
  PsPackage bundles are the four prism **input types**; all resolve to source
  derivations that build into the store the same way. crates.io is *just
  another input*, not a special-cased ecosystem ([`04`](04-sources.md)).
- **OS-general primitives.** The store, the derivation format, and atomic
  activation are designed as system primitives the wider OS will adopt, not
  shade-private machinery (§5 below).

## 2. Non-goals (v1)

- Binary package distribution or substitution from a remote cache.
- Multi-user / per-user profiles. One system-wide installed set
  (`TODO(open):` per-user generations, [`02 §5`](02-store.md#5-generations)).
- Output-addressed or content-addressed store entries. Explicitly rejected —
  see §4.
- Language ecosystems beyond Rust/Cargo. The source model is
  ecosystem-neutral, but only Cargo metadata integration is specified
  ([`05`](05-dependencies.md)).
- Recipe signing / signed repositories. Flagged throughout as future work
  ([`08 §4`](08-security.md#4-source-authenticity)).
- A Nix-like *language inside shade-core*. The recipe language (Shade) is its
  own layer ([`shade`](../shade/01-overview.md)); shade-core sees only CDF and
  stays language-agnostic below it. Shade evaluates to the very same CDF shade
  defines and adds no store concept, hash input, or `.drv` key
  ([`shade 01 §1`](../shade/01-overview.md#1-goals)) — the store cannot tell a
  Shade-produced derivation from any other.

---

## 3. Glossary

Terms are used consistently across the doc set.

- **Prism** — the flake analog and **sole install unit**: a `prism.shade`
  (with its `prism.lock`) declaring pinned **inputs** and computing **outputs**
  (packages) from them ([`04 §1`](04-sources.md#1-the-prism),
  [`07 §1`](07-cli.md#1-prism-reference-forms)). A bare crate/git/local/`.pspkg`
  is only ever a prism *input*, never installed on its own.
- **Recipe** — a Shade (`.shade`) file describing how to build one package:
  name, version, sources, deps, build steps. Evaluated by shadec to a
  derivation ([`03`](03-recipe-format.md), [`shade`](../shade/01-overview.md)).
  A prism *is* a recipe with declared inputs/outputs.
- **shadec** — the Shade evaluator and CDF serializer; the recipe frontend.
  Bootstrapped per [`09`](09-bootstrap.md).
- **Derivation** — the fully *resolved*, canonical build plan derived from a
  recipe: every dep is a concrete store path, every source a pinned identity,
  every env var a literal. Serialized in Canonical Derivation Form (CDF,
  [`02 §3.2`](02-store.md#32-canonical-derivation-form-cdf)) and stored as a
  `.drv` file in the store. Recipes are what humans write; derivations are
  what shade hashes and builds.
- **Source derivation** — a derivation whose "build" is fetching and unpacking
  a source (crate tarball, git checkout, local tree, PsPackage vendor entry)
  into the store ([`04 §2`](04-sources.md#2-source-derivations)).
- **PsPackage** — a self-contained bundle: recipe + lockfile + vendored
  sources shipped together, buildable with no network and no upstream
  availability ([`04 §3.4`](04-sources.md#34-pspackage)).
- **Store path** — an immutable directory (or `.drv` file) under `/shade/store/`,
  named by digest ([`02 §2`](02-store.md#2-store-path-format)).
- **Closure** — a store path plus everything it references, transitively
  ([`02 §7.2`](02-store.md#72-references)).
- **Generation** — one immutable snapshot of the installed set: a manifest
  plus a symlink forest (*profile*) into the store ([`02 §5`](02-store.md#5-generations)).
- **Activation** — atomically making a generation the current one
  ([`02 §6`](02-store.md#6-activation)).
- **GC root** — a reference that keeps a closure alive across garbage
  collection ([`02 §7`](02-store.md#7-garbage-collection)).
- **Lockfile** — `prism.lock`, the pinned resolution of all sources and
  dependency versions for a recipe ([`04 §5`](04-sources.md#5-lockfile)).
- **`--unsafe`** — build a git source that carries no in-repo recipe using a
  synthesized derivation ([`03 §7`](03-recipe-format.md#7-unsafe-default-recipes),
  risks in [`08 §3`](08-security.md#3-unsafe)).

---

## 4. Relation to Nix

shade deliberately borrows Nix's load-bearing ideas and deliberately drops its
surface. For orientation only — **no Nix compatibility is intended or
implied**, and Nix behavior must never be assumed where these docs are silent.

Same shape:

- Immutable store of hash-named paths; builds are pure-ish functions from
  inputs to store paths.
- Input-addressing: like Nix's default derivation model, the store path is
  computed from the build's inputs before building. Consequence shared with
  Nix: a store path's content **cannot be verified from its name** — trust in
  a path is trust in whoever built it ([`08 §2`](08-security.md#2-trust-model)).
- Generations + profile symlink flip for atomic activation and rollback.
- Mark-and-sweep GC from explicit roots.

Different:

| | Nix | shade |
|---|-----|------|
| Recipe language | Nix expression language (lazy functional DSL) | Shade — a pure lazy functional language, closely modeled on Nix's ([`shade`](../shade/01-overview.md)) |
| Derivation format | ATerm `.drv` | CDF, a line-based canonical text form ([`02 §3.2`](02-store.md#32-canonical-derivation-form-cdf)) |
| Store location | `/nix/store` | `/shade/store` |
| Substitution | binary caches, first-class | none in v1 |
| Fixed-output derivations | output-hash-addressed fetches | source derivations are input-addressed like everything else; the declared content hash is an *input* ([`04 §2`](04-sources.md#2-source-derivations)) |
| Ecosystem integration | external tools (crate2nix etc.) | Cargo metadata integration is native ([`05`](05-dependencies.md)) |
| Sandbox | Linux namespaces / macOS sandbox | Lythos capability model ([`06 §3`](06-build.md#3-sandbox)) |

---

## 5. OS-general vs shade-local {#5-os-general-vs-shade-local}

**Forward-looking constraint (fixed):** PureshadeOS will become more Nix-like
over time. The store, the derivation format, and atomic activation are
**OS-general primitives** — designed so the wider system (lythd service
definitions, kernel updates, system configuration) can adopt them without
shade being in the loop. Everything else is shade-local policy.

| Primitive | Class | Why |
|---|---|---|
| `/shade/` hierarchy, store path format, CDF + input hash ([`02 §1–3`](02-store.md)) | **OS-general** | any future system component that produces or consumes store paths must agree on these; they are versioned formats, not shade internals |
| References, closures, GC roots ([`02 §7`](02-store.md#7-garbage-collection)) | **OS-general** | liveness must be a system-wide notion or GC is unsound |
| Generations + atomic activation ([`02 §5–6`](02-store.md)) | **OS-general** | the same mechanism must eventually activate kernel + config + services atomically (cf. `docs/spec/fhs.md` snapshot-atomicity story) |
| Build sandbox contract ([`06 §3`](06-build.md#3-sandbox)) | **OS-general** | defined as a capability profile any supervisor could grant; shade is just the first client |
| Shade recipe language ([`03`](03-recipe-format.md), [`shade`](../shade/01-overview.md)) | **shade-local** | recipes *evaluate down to* derivations; the OS-general layer sees only CDF |
| shadec bootstrap ([`09`](09-bootstrap.md)) | **shade-local** rebuild policy over an **OS-general** seed-trust format | the seed-trust/pin format is inherited by any store client; the rebuild is shade policy |
| Source resolution + lockfile ([`04`](04-sources.md)), Cargo integration ([`05`](05-dependencies.md)) | **shade-local** | ecosystem policy; produces derivations, is not itself a primitive |
| CLI ([`07`](07-cli.md)) | **shade-local** | UX |

Each doc marks its decisions with **[OS-general]** or **[shade-local]** where
the classification matters.

Naming note: the OS-general layer is referred to as the **Lythos store
services** where the distinction from shade-the-tool matters. In v1 both live
in the `shade` binary; the boundary is a documentation and API-design
commitment, not yet a process boundary. `TODO(open):` whether store services
eventually become a daemon (store writes mediated by a privileged service,
shade an unprivileged client — the Nix-daemon shape) or stay a library +
capability grant. Affects [`08 §5`](08-security.md#5-sandbox-guarantees).

---

## 6. Known system gaps (design-time flags)

These are gaps in the *platform*, not open questions inside shade's own design.
Specs below are written against the target platform state and flag their
dependencies on these items.

1. **`TODO(open):` toolchain bootstrap.** OROS has no native `rustc`. A
   source-based package manager needs a compiler. Until an OROS-hosted
   toolchain exists, builds execute in *host-assisted mode*: the build runs on
   the development host, cross-compiled with `targets/x86_64-oros.json`, and
   the outputs are ingested into the target's store at the (host-computed,
   identical) store path. The derivation format is designed so hashes are
   identical in both modes ([`02 §3.3`](02-store.md#33-hash-inputs), the
   `system` and `toolchain` fields). Host-assisted mode is a bringup vehicle,
   not a supported end state.
2. **`TODO(open):` kernel filesystem isolation.** The capability system
   (`docs/spec/capabilities.md`) has kinds `Memory`, `Ipc`, `Device`,
   `Rollback` — no filesystem capability and no per-task fs namespace. The
   sandbox contract ([`06 §3`](06-build.md#3-sandbox)) requires path-scoped fs
   authority; the kernel mechanism for it does not exist yet and must be
   designed (fs cap kind, or per-task root, or VFS-level enforcement).
3. **`TODO(open):` network stack maturity.** Fetching from crates.io/git
   requires TLS + HTTP(S) + git transport on OROS. Until then, fetch runs
   host-assisted; the fetch/build split ([`06 §2`](06-build.md#2-phases)) is
   designed so this changes nothing above it. PsPackage bundles
   ([`04 §3.4`](04-sources.md#34-pspackage)) sidestep the gap entirely for
   pre-vendored software.

(`docs/spec/fhs.md` now specifies the `/shade/` hierarchy and the
`/lth/bin → /shade/gen/current/profile/bin` symlink consistently with this doc
set; the earlier `/lth/store/` layout is gone.)
