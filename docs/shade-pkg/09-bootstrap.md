# shade — Bootstrap

Shade is the **sole** recipe frontend ([`03`](03-recipe-format.md)): every
recipe is a `.shade` file evaluated by **shadec** to CDF
([`02 §3.2`](02-store.md#32-canonical-derivation-form-cdf),
[`shade 05`](../shade/05-derivation.md)). This creates a **founding
circularity**: no package builds without CDF, no CDF without shadec, and
shadec is itself a package. This doc defines how the circle is broken — the
seed shadec, how it is trusted and pinned, and how the system rebuilds
shadec through shade so the seed can be retired. **This gates the entire
package system**; it is specified before any build can run.

**[OS-general]** where it defines the seed-trust and pin format (any future
frontend or store client inherits it); **[shade-local]** for the rebuild
policy. Prerequisites: [`08 §2`](08-security.md#2-trust-model) (trust model),
[`01 §6.1`](01-overview.md#6-known-system-gaps-design-time-flags)
(host-assisted mode — the seed is built there).

---

## 1. The circularity, stated exactly {#1-circularity}

```
recipe (.shade) ──shadec eval──▶ CDF ──shade build──▶ store path
                     ▲                                    │
                     └──────────── shadec is a package ───┘
```

shadec cannot be a shade package **whose build requires shadec**: evaluating
its own recipe to produce its own CDF would require a running shadec. This is
the same bootstrap problem Nix has (a working `nix` is needed to build `nix`;
nixpkgs' stdenv is seeded from prebuilt `bootstrap-tools` fetched as
fixed-output). Shade resolves it the same way: a **seed** that enters the
store *without* evaluation, plus a one-time trust transfer to a
store-resident, recipe-built shadec.

Note what is **not** circular and needs no bootstrap:

- **`lib`** is Shade *code*, not a compiled artifact — the `shadepkgs` channel's
  `.shade` files ([`shade 07 §3`](../shade/07-stdlib.md#3-lib)). shadec reads
  and evaluates it; nothing builds it. Once a shadec exists, `lib` works.
- **CDF canonicalization** is inside shadec ([`shade 08 §1`](../shade/08-interop.md#1-single-frontend)).
  It ships with the seed; no separate bootstrap.

So the bootstrap has exactly **one** object: the shadec binary.

## 2. The seed shadec {#2-seed-shadec}

The seed is a prebuilt shadec binary that enters the store as a
**fixed-output source derivation** ([`04 §2`](04-sources.md#2-source-derivations)),
not as a sandbox-built package. It is ingested by identity, exactly like a
crate tarball or a git checkout — the one mechanism that puts bytes in the
store with **no evaluator and no builder in the loop**.

- **How it is produced.** Cross-compiled on the dev host with
  `targets/x86_64-oros.json`, host-assisted
  ([`01 §6.1`](01-overview.md#6-known-system-gaps-design-time-flags)),
  during bringup. The host is in the trusted set for the duration
  ([`08 §1`](08-security.md#1-actors-and-assets)).
- **How it enters the store.** As a `local`-type source derivation
  ([`04 §3.3`](04-sources.md#33-local)) with a **declared BLAKE3 tree/content
  hash** — a fixed-output ingestion that fails closed on mismatch
  ([`04 §2`](04-sources.md#2-source-derivations)). Its store path is a pure
  function of that hash; ingesting it requires only the store services, never
  shadec.
- **Where it lives before ingestion.** Shipped inside the OS image.
  `TODO(open):` exact staging path for the pre-ingestion bytes — candidate
  `/shade/boot/shadec` (a reserved bootstrap area under the store hierarchy,
  [`02 §1`](02-store.md#1-the-shade-hierarchy)) vs. carried in the image's
  read-only region and ingested at first boot. Must be decided with
  `docs/spec/fhs.md`'s image layout; flagged.

The seed is a **bringup and re-bootstrap vehicle, not a supported end
state** — the same status host-assisted mode has
([`01 §6.1`](01-overview.md#6-known-system-gaps-design-time-flags)). Steady
state runs the store shadec (§4).

## 3. Trust and pinning {#3-trust-and-pinning}

The seed is a **trust root**: it is the first thing that evaluates recipes,
so everything the store contains descends from trusting it. It is trusted the
way the toolchain is ([`08 §1`](08-security.md#1-actors-and-assets)) — by
provenance, not by verifying its output against its name (input-addressing
forbids the latter, [`02 §3.3`](02-store.md#33-hash-inputs)).

**Pin format.** A bootstrap pin records the seed's identity so it is
reproducible and auditable:

```toml
# bootstrap pin (TOML — machine-written state, not a recipe; see 01 §1 note)
schema = 1

[shadec]
version = "0.1.0"                       # seed shadec version
lang-version = 1                        # Shade language version it evaluates (shade 07 §2.7)
toolchain = "rustc-1.86.0-adf2135f0"    # toolchain that built the seed
hash = "77c3…64hex"                     # BLAKE3 of the seed binary — the fixed-output identity
store-path = "/shade/store/<digest>-shadec-seed-0.1.0"
```

- **Location.** `TODO(open):` `/cfg/shade/bootstrap` vs. carried in the OS
  image manifest. Leaning image manifest, so the seed and its pin ship and
  are signed as one unit; flagged, tied to the image-signing story
  ([`08 §6`](08-security.md#6-future-binary-substitution) `TODO`).
- **GC root.** The seed's store path is a permanent GC root
  ([`02 §7.1`](02-store.md#7-garbage-collection)) — it must never be
  collected, or re-bootstrapping (§5) becomes impossible on a system with no
  network and no image.
- **Signing.** Seed authenticity folds into the **same** deferred
  design as channel signing and binary substitution
  ([`08 §4`](08-security.md#4-source-authenticity),
  [`08 §6`](08-security.md#6-future-binary-substitution)): a signature over
  `(store path, content hash)` verified against a trust root shipped in the
  image. `TODO(open):` not improvised early — arrives with that one design.

**Trust transfer, once:** you trust the seed because it came from a signed
image (or, in bringup, from the trusted host). Everything the seed evaluates
and shade then builds is trusted *transitively*. The store shadec (§4) is
trusted because the trusted seed evaluated its recipe and the trusted store
services built it — no independent trust decision.

## 4. Rebuilding shadec through shade {#4-rebuild}

shadec has its own `.shade` recipe like any package. The bootstrap **uses the
seed to build the real thing**:

1. shade needs to evaluate some recipe; the active evaluator is the seed
   (§2), located via the bootstrap pin (§3).
2. shade evaluates **shadec's own recipe** with the seed shadec → CDF for a
   store-resident shadec.
3. shade builds that CDF normally ([`06`](06-build.md)) — sandboxed (or
   host-assisted during bringup), landing shadec at an **input-addressed
   store path** ([`02 §3`](02-store.md#3-input-addressing)) like any package.
4. The store shadec is installed into the current generation's profile
   ([`02 §5`](02-store.md#5-generations)); `/lth/bin/shadec` now resolves to
   it through `current` ([`02 §6`](02-store.md#6-activation)).
5. shade's "which shadec do I invoke" rule (§6) now prefers the profile
   shadec; the seed is used only when no profile shadec exists.

This is exactly Nix building `nix`/stdenv from seed binaries: the seed is
never *itself* rebuilt in place; it builds a successor that supersedes it.
The seed stays only as the re-bootstrap anchor (§3 GC root) — retirable in
principle once a store shadec is trusted, retained in practice so a
network-less, image-less system can re-derive shadec.

**Circularity broken:** step 2's evaluator is the seed (a fixed-output
ingestion, §2 — needs no evaluator to exist), and step 3's product is a real
package (needs an evaluator, has one: the seed). Neither step requires shadec
to build shadec.

### 4.1 The store shadec's recipe

`.shade`, evaluated to a CDF whose `dep.*` include the toolchain (or the
toolchain's store path once it is itself a package,
[`06 §4`](06-build.md#4-environment) `TODO`) and whose sources are shadec's
own source tree. Nothing special: shadec is a Rust package built by the
default cargo phases ([`03 §7`](03-recipe-format.md#7-unsafe-default-recipes)
table) or by `lib.rustPackage` once it lands
([`shade 07 §4`](../shade/07-stdlib.md#4-deferred-lib)). The **only**
distinction from any other package is that its builder (the current shadec)
is chosen by §6, not assumed to be the profile one.

## 5. Version skew and staged bootstrap {#5-version-skew}

The seed may be **older** than the shadec recipe it must evaluate — the
recipe (or `lib`) may use language features the seed's `lang-version`
([`shade 07 §2.7`](../shade/07-stdlib.md#27-introspection)) predates.

v1 rule: **the store shadec's recipe must evaluate under the seed's
`lang-version`.** shadec self-reports `lang-version`; the recipe declares the
minimum it needs; a recipe needing newer than the seed provides is a
bootstrap error naming the required staged upgrade.

`TODO(open):` **staged bootstrap.** When the target shadec needs a newer
language than the seed, bootstrap in stages: seed (langN) builds an
intermediate shadec (langN, newer codegen) → intermediate builds the final
(langN+1). Nix does exactly this for stdenv stages. Deferred until the
language actually versions past the seed; until then a single stage suffices.
The pin's `lang-version` field (§3) exists so this check is mechanical.
Flagged.

## 6. Evaluator selection {#6-evaluator-selection}

When shade must evaluate a `.shade` recipe, it selects the shadec to invoke,
in order:

1. The **profile** shadec — `/shade/gen/current/profile/bin/shadec` if present
   (the store shadec, §4). Steady state.
2. The **seed** shadec — the bootstrap pin's `store-path` (§3). Used at first
   boot, during re-bootstrap, and whenever no profile shadec exists.

`TODO(open):` a `--bootstrap` / `--seed-shadec` override for `shade` to force
the seed (re-bootstrap after a broken store shadec, or verifying the seed
still reproduces the store shadec's CDF — a `shadec cdf` byte-diff,
[`shade 08 §3`](../shade/08-interop.md#3-shadec-cdf)). Deferred with the rest
of shade's bootstrap CLI surface; flagged.

Selection is **not** a hash input: which shadec evaluated a recipe does not
appear in the CDF ([`02 §3.3`](02-store.md#33-hash-inputs) excluded list —
the *evaluator* is like the *building machine*, not part of the derivation).
Two shadecs that implement the language correctly must produce byte-identical
CDF for the same recipe ([`shade 08 §1`](../shade/08-interop.md#1-single-frontend),
one canonicalizer) — that requirement is what makes seed→store handoff sound:
the store shadec must agree with the seed on every CDF it emits, or store
paths would shift under the system. `TODO(open):` a bootstrap acceptance test
— seed and freshly-built store shadec must emit identical CDF over a recipe
corpus before the store shadec is activated (§4 step 4). Cheap, high-value;
should land with the first shadec. Flagged.

## 7. Relation to the toolchain bootstrap {#7-toolchain-relation}

Two seeds exist during bringup and compose:

- the **toolchain** seed (no native `rustc` on OROS,
  [`01 §6.1`](01-overview.md#6-known-system-gaps-design-time-flags)), and
- the **shadec** seed (this doc).

The host builds both. They are independent trust roots with the same shape
(prebuilt, pinned, host-provenance-trusted, retired when a store-built
successor exists). Ordering: the shadec seed only needs to *evaluate*
recipes, so it can exist before any toolchain package; the toolchain seed is
needed to *build* the store shadec (§4 step 3). Neither depends on the other
to *exist*; both are needed for the first real build. `TODO(open):` a single
combined bringup manifest listing every seed (shadec, toolchain, and any
future one) with pins and signatures, so "what does this system trust at
its root" is one auditable file. Folds into the image-manifest decision (§3).
Flagged.

## 8. Open items (bootstrap)

- **§2** — seed staging path (`/shade/boot/shadec` vs. image region). With
  `docs/spec/fhs.md`.
- **§3** — pin location (`/cfg/shade/bootstrap` vs. image manifest) and seed
  signing. Folds into channel-signing / substitution design
  ([`08 §6`](08-security.md#6-future-binary-substitution)).
- **§5** — staged bootstrap for language-version skew. Deferred until the
  language versions past the seed.
- **§6** — `--bootstrap`/`--seed-shadec` override; seed↔store CDF acceptance
  test (should land with first shadec).
- **§7** — combined bringup seed manifest (shadec + toolchain + future).

None blocks the MVP evaluator ([`shade 01 §5`](../shade/01-overview.md#5-tiering)):
the seed (§2) + evaluator selection (§6) + single-stage rebuild (§4) are the
whole bootstrap MVP, and all three are fully specified here. Signing, staging,
and staged upgrades are deferred and design-flagged.
