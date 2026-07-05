# shade — Trust Model and Security

What shade trusts, what it verifies, why the `--unsafe` build category is
retired (§3), and what
the sandbox does and does not guarantee today. Written against v1 reality —
where a guarantee depends on unbuilt kernel work, that is stated, not
implied away.

---

## 1. Actors and assets

- **Assets:** the store (its integrity is every future execution on the
  system), the generation history (rollback safety), the running system's
  capability boundaries.
- **Trusted:** the kernel, the store services code, the toolchain, **the seed
  shadec** ([`09 §3`](09-bootstrap.md#3-trust-and-pinning) — the recipe
  evaluator is a trust root; every store path descends from trusting it), RFS,
  and in host-assisted mode
  ([`01 §6.1`](01-overview.md#6-known-system-gaps-design-time-flags)) the
  entire dev host.
- **Untrusted:** everything a derivation causes to execute in phase 3
  ([`06 §2`](06-build.md#2-phases)) — recipe build commands, build scripts,
  proc-macros, compilers *as driven by* untrusted input; and every byte
  fetched from the network.

## 2. Trust model {#2-trust-model}

Layered, from strongest to weakest:

1. **Pinned identity → bytes** (verified): once a lockfile pins a sha256 /
   commit / tree hash, fetch verifies bytes against the pin and fails closed
   ([`04 §1–3`](04-sources.md)). Repeatability against a hostile mirror or
   MITM is *guaranteed* given an honest first resolution.
2. **First resolution** (TOFU): the pin itself is created by asking the
   network (crates.io index, git remote). A compromised registry/remote at
   *lock time* poisons the pin. Mitigations in §4.
3. **Recipe content** (human review): a recipe is a Shade expression
   ([`03`](03-recipe-format.md)) that decides what commands run in the sandbox;
   reviewing it means reading that expression (and any `lib`/channel code it
   imports — a wider surface than a static recipe, since evaluation can
   compute the build). Installing from the prism registry means trusting
   whoever populates `/cfg/shade/recipes/`
   ([`05 §2`](05-dependencies.md#2-shade-level-resolution)); installing an
   in-repo recipe means trusting the repo author. There is no recipe signing
   in v1 (`TODO(open):` channel signing, blocked on the channel design,
   [`05 §2`](05-dependencies.md#2-shade-level-resolution)). Evaluation is pure
   ([`shade 03 §5`](../shade/03-semantics.md#5-purity)) — a recipe cannot do
   IO at eval time beyond pinned fixed-output fetches and tracked reads — so
   review scope is the *build commands* the expression produces, not arbitrary
   eval-time effects.
4. **Build behavior** (sandbox): whatever the recipe runs is confined per
   the §5 contract — with the v1 gaps stated there.

Input-addressing corollary, third and final restatement because each doc
needs its consequence: a store path name proves *nothing* about content
([`02 §3.3`](02-store.md#33-hash-inputs)). v1's store is trustworthy solely
because every path in it was built locally by the trusted store services.
Any path that entered another way is an integrity breach, not a degraded
state.

## 3. `--unsafe` — retired {#3-unsafe}

**Status: retired, not just removed.** The prism-only model
([`04 §1`](04-sources.md#1-the-prism)) removed the "install a recipe-less git
URL" command, and the follow-on decision is **explicit-required**: shade never
synthesizes a builder for a recipe-less input
([`03 §7`](03-recipe-format.md#7-unsafe-default-recipes)). There is therefore
**no unbuilt-from-unreviewed-instructions path at all** — no
`shade install --unsafe <url>`, no `builder = default`, and no `unsafe=1` CDF
key. The whole "unsafe build" category is gone by construction, which is the
strongest possible resolution of this section's former concern.

The **security rationale** that drove the decision (kept for the record):

- **No review of build behavior.** A synthesized `cargo build` would execute
  `build.rs` and proc-macros from the repo and its whole crate graph —
  arbitrary code, confined only by §5. With the v1 fs-isolation gap such a
  build would be effectively unconfined; **on-target that is handing the repo
  author the system.** Making the build spec explicit forces this code path to
  be authored and reviewable, not conjured from a URL.
- **No provenance floor.** A `name` resolves through the prism registry
  (something a person placed there, [`05 §2`](05-dependencies.md#2-shade-level-resolution));
  a bare URL is just a URL — typosquatting and lookalike URLs were the obvious
  vector, now closed since a URL alone builds nothing.
- **Output trust.** Any built binary lands in `bin/` and, once installed, in
  `$PATH` via the profile; there is no "marking helps audit but does not
  contain" gap because there is no unreviewed build to mark in the first place.

`TODO(open):` none — the explicit-required decision closes this section. (The
anchor and the `--unsafe` name are retained only so older cross-references
still resolve.)

## 4. Source authenticity {#4-source-authenticity}

Per type, what is verified vs. assumed:

| Source | Verified | Assumed / open |
|---|---|---|
| crates.io | `.crate` sha256 against the pin; pin against the registry index at lock time | index integrity at lock time (no TUF/sigstore in v1 — `TODO(open):` adopt registry signing when the ecosystem settles); yanked-crate status only checked at resolution |
| git | commit hash equality after fetch | SHA-1 collision resistance (`TODO(open):` record and additionally verify a BLAKE3 tree hash of the checkout in the lockfile — closes the SHA-1 gap and the submodule-drift gap at once; cheap, should land in schema 1 before freeze); no tag/commit signature verification in v1 |
| local | tree hash ([`04 §3.3`](04-sources.md#33-local)) | the local tree is the user's own responsibility by definition |
| pspackage | bundle tree hash against the outer pin; every vendored entry against the bundled lockfile's pins ([`04 §3.4`](04-sources.md#34-pspackage)) | bundle provenance — the bundler chose the pins, so trusting a bundle is trusting its builder (TOFU moves from the network to the bundle author); no bundle signing in v1, folds into the channel-signing `TODO` above |

Lockfile as the audit surface: because every network-derived fact lands in
`prism.lock` ([`04 §5`](04-sources.md#5-lockfile)) and CDF hashes only
lockfile values, reviewing a diff of `prism.lock` reviews the entire
resolution change. Keep lockfiles in VCS; treat unexplained pin changes as
incidents.

## 5. Sandbox guarantees {#5-sandbox-guarantees}

The contract is [`06 §3.1`](06-build.md#31-contract-sandbox-profile-1).
Honest status per row, native mode:

| Contract row | v1 status |
|---|---|
| no ambient capabilities (IPC, device, rollback) | **enforced** — kernel capability model, `docs/spec/capabilities.md` |
| memory cap | **partial** — pending range-restricted Memory caps ([`06 §3.1`](06-build.md#31-contract-sandbox-profile-1) `TODO`) |
| fs read/write scoping | **not enforced** — kernel gap ([`01 §6.2`](01-overview.md#6-known-system-gaps-design-time-flags)); honor-system |
| no network | **enforced trivially** (no net stack reachable without caps) |
| env scrubbing | **enforced** (store services construct the env, [`06 §4`](06-build.md#4-environment)) |
| undeclared store inputs | **detected at registration** (reference scan, [`06 §5`](06-build.md#5-registration) step 3) — detection, not prevention |

Host-assisted mode: all rows are whatever the host provides; the host is in
the trusted set (§1) for the duration of bringup. Exiting bringup means
native builds with the fs gap closed — that kernel work is the single
biggest security dependency of this design and should be tracked in
`docs/plans/followup-code-tasks.md` when implementation starts.

Store immutability enforcement (only store services write `/shade/store`,
[`02 §1`](02-store.md#1-the-shade-hierarchy)) sits on the same kernel gap.

## 6. Future binary substitution {#6-future-binary-substitution}

Non-goal for v1 ([`01 §2`](01-overview.md#2-non-goals-v1)), but the security
shape is fixed now because input-addressing forces it: a substituted path
cannot be validated by hashing (no output hash to check against the name),
so substitution is **pure trust transfer** — a substituter's signature over
`(store path, content hash)` must be verified against locally configured
trusted keys, and the content hash then verified over the received bytes.
Design consequences already locked in:

- `/shade/db/valid/` records keep the full untruncated content-relevant metadata
  (deriver, registration provenance) so "where did this path come from" is
  answerable ([`02 §7.2`](02-store.md#72-references)).
- Determinism work ([`06 §6`](06-build.md#6-determinism)) graduates from
  aspiration to requirement the day two machines are supposed to agree on a
  path's contents.
- Signature format, key distribution, trust roots: `TODO(open):` entirely —
  do not improvise pieces of it early; it arrives as one design with the
  channel mechanism ([`05 §2`](05-dependencies.md#2-shade-level-resolution)).
