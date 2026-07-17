//! Build-core tests. They target [`build`]/[`plan`] directly (not argv): a
//! resolver miss realizes, a hit is a no-op, the same recipe addresses to the
//! same path twice, and the resolver seam is swappable — a mock second source
//! short-circuits the build, proving a substituter drops in without touching
//! [`build`].

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use shadec::io::HostIo;

use super::*;

/// The evaluator is `Rc`-based and recurses; run each case on a big stack.
fn with_stack<F>(f: F)
where
    F: FnOnce() + Send + 'static,
{
    std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}

/// A throwaway unique directory under the OS temp dir; removed on drop.
struct TmpDir(PathBuf);
impl TmpDir {
    fn new(tag: &str) -> Self {
        static C: AtomicU64 = AtomicU64::new(0);
        let n = C.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir()
            .join(format!("shade-build-test-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        TmpDir(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for TmpDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A minimal recipe: one phase, one bin output. Toolchain is supplied via the
/// `toolchain` argument to [`build`] so the expression stays tiny.
fn demo_recipe() -> RecipeRef {
    RecipeRef::Expr {
        src: r#"
derivation {
  name = "demo";
  version = "1.0";
  system = "x86_64-oros";
  phases = [ "true" ];
  outputs = { bin = [ "demo" ]; };
}"#
        .to_string(),
        base_dir: "/base".to_string(),
    }
}

/// A builder that stages a fixed tree and counts how many times it ran (so a
/// no-op hit is observable). Stages into its own scratch dir, cleaned on drop.
struct CountingBuilder {
    runs: Arc<AtomicUsize>,
    content: &'static [u8],
    scratch_root: PathBuf,
}
impl CountingBuilder {
    fn new(scratch_root: &Path, content: &'static [u8]) -> Self {
        CountingBuilder {
            runs: Arc::new(AtomicUsize::new(0)),
            content,
            scratch_root: scratch_root.to_path_buf(),
        }
    }
    fn runs(&self) -> usize {
        self.runs.load(Ordering::Relaxed)
    }
}
impl Builder for CountingBuilder {
    fn build(&self, plan: &BuildPlan) -> std::io::Result<StagedOutput> {
        let n = self.runs.fetch_add(1, Ordering::Relaxed);
        let scratch = self.scratch_root.join(format!("scratch-{}-{n}", plan.name));
        let root = scratch.join("out");
        std::fs::create_dir_all(root.join("bin"))?;
        std::fs::write(root.join("bin/demo"), self.content)?;
        Ok(StagedOutput::with_cleanup(root, scratch))
    }
}

/// A stand-in for a remote substituter (shade-pkg 08 §6): a second resolver
/// source. Records whether it was consulted; when armed, claims the plan.
struct MockSubstituter {
    consulted: Arc<AtomicUsize>,
    hit: bool,
}
impl MockSubstituter {
    fn new(hit: bool) -> Self {
        MockSubstituter { consulted: Arc::new(AtomicUsize::new(0)), hit }
    }
    fn consulted(&self) -> usize {
        self.consulted.load(Ordering::Relaxed)
    }
}
impl Resolver for MockSubstituter {
    fn source(&self) -> &str {
        "mock"
    }
    fn resolve(&self, plan: &BuildPlan) -> std::io::Result<Option<PathBuf>> {
        self.consulted.fetch_add(1, Ordering::Relaxed);
        Ok(self.hit.then(|| PathBuf::from(&plan.paths.out_path)))
    }
}

#[test]
fn build_miss_realizes() {
    with_stack(|| {
        let tmp = TmpDir::new("miss");
        let store = tmp.path().join("store");
        let io = HostIo;
        let builder = CountingBuilder::new(tmp.path(), b"\x7fELF demo");
        let local = LocalStore;

        let out = build(&demo_recipe(), &store, &[&local], &builder, Some("tc-1"), &io)
            .expect("build");

        assert_eq!(builder.runs(), 1, "a miss must build exactly once");
        match &out.result {
            Built::Realized { out_path } => {
                assert!(out_path.join("bin/demo").exists(), "output tree installed");
                assert_eq!(
                    std::fs::read(out_path.join("bin/demo")).unwrap(),
                    b"\x7fELF demo"
                );
                // .drv written next to the output, carrying the CDF bytes.
                assert_eq!(std::fs::read(&out.plan.paths.drv_path).unwrap(), out.plan.cdf);
                // path lives under the chosen store root, input-addressed.
                assert!(out_path.starts_with(&store));
                assert!(out.plan.paths.store_name.ends_with("-demo-1.0"));
            }
            other => panic!("expected Realized, got {other:?}"),
        }
    });
}

#[test]
fn build_hit_is_noop() {
    with_stack(|| {
        let tmp = TmpDir::new("hit");
        let store = tmp.path().join("store");
        let io = HostIo;
        let builder = CountingBuilder::new(tmp.path(), b"\x7fELF demo");
        let local = LocalStore;

        // First build realizes.
        let first = build(&demo_recipe(), &store, &[&local], &builder, Some("tc-1"), &io)
            .expect("first build");
        assert_eq!(builder.runs(), 1);
        assert!(matches!(first.result, Built::Realized { .. }));

        // Second build: local store now hits; the builder must NOT run again.
        let second = build(&demo_recipe(), &store, &[&local], &builder, Some("tc-1"), &io)
            .expect("second build");
        assert_eq!(builder.runs(), 1, "a store hit must not rebuild");
        match &second.result {
            Built::Resolved { source, out_path } => {
                assert_eq!(source, "local");
                assert_eq!(out_path, first.result.out_path());
            }
            other => panic!("expected Resolved hit, got {other:?}"),
        }
    });
}

#[test]
fn same_recipe_same_path_twice() {
    with_stack(|| {
        let tmp = TmpDir::new("stable");
        // Two independent store roots: the digest is input-addressed, so the
        // store_name (digest-name-version) must match regardless of root.
        let store_a = tmp.path().join("a");
        let store_b = tmp.path().join("b");
        let io = HostIo;

        let pa = plan(&demo_recipe(), store_a.to_str().unwrap(), Some("tc-1"), &io).expect("plan a");
        let pb = plan(&demo_recipe(), store_b.to_str().unwrap(), Some("tc-1"), &io).expect("plan b");

        assert_eq!(pa.paths.digest, pb.paths.digest, "same recipe ⇒ same digest");
        assert_eq!(pa.paths.store_name, pb.paths.store_name);
        assert_eq!(pa.cdf, pb.cdf, "same recipe ⇒ same CDF bytes");
        // Only the root differs.
        assert!(pa.paths.out_path.starts_with(store_a.to_str().unwrap()));
        assert!(pb.paths.out_path.starts_with(store_b.to_str().unwrap()));

        // And realizing twice into the same root converges on one path.
        let builder = CountingBuilder::new(tmp.path(), b"x");
        let local = LocalStore;
        let o1 = build(&demo_recipe(), &store_a, &[&local], &builder, Some("tc-1"), &io).unwrap();
        let o2 = build(&demo_recipe(), &store_a, &[&local], &builder, Some("tc-1"), &io).unwrap();
        assert_eq!(o1.result.out_path(), o2.result.out_path());
    });
}

#[test]
fn resolver_seam_is_swappable() {
    with_stack(|| {
        let tmp = TmpDir::new("seam");
        let store = tmp.path().join("store");
        let io = HostIo;
        let builder = CountingBuilder::new(tmp.path(), b"x");

        // Swap in a second resolver source after the local store. On a fresh
        // store the local source misses, the substituter is consulted next and
        // claims the plan — so nothing builds. This is the whole point of the
        // seam: a substituter drops in as a Resolver, build() is untouched.
        let local = LocalStore;
        let subst = MockSubstituter::new(/* hit */ true);
        let out = build(
            &demo_recipe(),
            &store,
            &[&local, &subst],
            &builder,
            Some("tc-1"),
            &io,
        )
        .expect("build");

        assert_eq!(subst.consulted(), 1, "second source must be consulted on a local miss");
        assert_eq!(builder.runs(), 0, "a resolver hit must short-circuit the build");
        match &out.result {
            Built::Resolved { source, out_path } => {
                assert_eq!(source, "mock", "the second source satisfied the plan");
                assert_eq!(out_path, &out.plan.paths.out_path);
            }
            other => panic!("expected Resolved from the mock, got {other:?}"),
        }

        // Control: with a non-hitting substituter and an empty store, the build
        // falls through to the builder — proving the source, not build(), drove
        // the difference.
        let subst_miss = MockSubstituter::new(false);
        let store2 = tmp.path().join("store2");
        let out2 = build(
            &demo_recipe(),
            &store2,
            &[&local, &subst_miss],
            &builder,
            Some("tc-1"),
            &io,
        )
        .expect("build 2");
        assert_eq!(subst_miss.consulted(), 1);
        assert_eq!(builder.runs(), 1, "all sources missed ⇒ build once");
        assert!(matches!(out2.result, Built::Realized { .. }));
    });
}

// ---- executor tests ---------------------------------------------------------

use std::fs;
use std::sync::Mutex;

/// The executor's three roots under one tmp dir.
fn exec_roots(tmp: &TmpDir) -> (PathBuf, PathBuf, PathBuf) {
    (tmp.path().join("store"), tmp.path().join("build"), tmp.path().join("log"))
}

/// Wraps [`PermissiveSandbox`], counting spawns — makes "a lookup hit runs
/// no builder" observable.
struct CountingSandbox {
    inner: PermissiveSandbox,
    spawns: AtomicUsize,
}
impl CountingSandbox {
    fn new() -> Self {
        CountingSandbox { inner: PermissiveSandbox, spawns: AtomicUsize::new(0) }
    }
    fn spawns(&self) -> usize {
        self.spawns.load(Ordering::Relaxed)
    }
}
impl BuildSandbox for CountingSandbox {
    fn prepare(&self, spec: &SandboxSpec) -> std::io::Result<BuildEnv> {
        self.inner.prepare(spec)
    }
    fn spawn(&self, env: &BuildEnv, command: &str, log: &fs::File) -> std::io::Result<i32> {
        self.spawns.fetch_add(1, Ordering::Relaxed);
        self.inner.spawn(env, command, log)
    }
    fn collect_outputs(&self, env: &BuildEnv, declared: &[String]) -> std::io::Result<Vec<PathBuf>> {
        self.inner.collect_outputs(env, declared)
    }
}

/// Records every registration (owned) — proves the registrar seam is called
/// once per realization, in build order, with the right refs.
#[derive(Default)]
struct RecordingRegistrar {
    seen: Mutex<Vec<(String, String, Vec<String>)>>, // (store_name, cdf_hash, refs)
}
impl StoreRegistrar for RecordingRegistrar {
    fn register(&self, reg: &Registration) -> std::io::Result<()> {
        self.seen.lock().unwrap().push((
            reg.store_name.to_string(),
            reg.cdf_hash.to_string(),
            reg.refs.to_vec(),
        ));
        Ok(())
    }
}

/// A one-file trivial derivation: the gate case.
fn trivial_recipe() -> RecipeRef {
    RecipeRef::Expr {
        src: r#"
derivation {
  name = "demo";
  version = "1.0";
  system = "x86_64-oros";
  phases = [ "mkdir -p $out/bin" "printf hi > $out/bin/demo" ];
  outputs = { bin = [ "demo" ]; };
}"#
        .to_string(),
        base_dir: "/base".to_string(),
    }
}

/// GATE: end-to-end `shade build` on a trivial derivation produces a real
/// store path; a second run is a pure lookup (no builder spawn) — the
/// idempotence proof.
#[test]
fn executor_gate_build_then_pure_lookup() {
    with_stack(|| {
        let tmp = TmpDir::new("exec-gate");
        let (store, build, log) = exec_roots(&tmp);
        let io = HostIo;
        let local = LocalStore;
        let resolvers: [&dyn Resolver; 1] = [&local];
        let sandbox = CountingSandbox::new();
        let registrar = RecordingRegistrar::default();
        let exec = Executor::new(&store, &build, &log, &resolvers, &sandbox, &registrar);

        // Run 1: builds and realizes.
        let out = exec.run(&trivial_recipe(), Some("tc-1"), &io).expect("first run");
        let Built::Realized { out_path } = out.root_result() else {
            panic!("expected Realized, got {:?}", out.root_result());
        };
        assert!(out_path.starts_with(&store));
        assert_eq!(fs::read(out_path.join("bin/demo")).unwrap(), b"hi");
        // The .drv sits next to the output, byte-equal to the CDF.
        assert_eq!(fs::read(&out.root.paths.drv_path).unwrap(), out.root.cdf);
        // Log captured, named by store name, carrying the phase trace.
        let log_file = log.join(format!("{}.log", out.root.paths.store_name));
        let log_text = fs::read_to_string(&log_file).unwrap();
        assert!(log_text.contains("phase 0: mkdir -p $out/bin"));
        // Scratch cleaned on success.
        assert!(!build.join(&out.root.paths.store_name).exists());
        // Registrar called once with the CDF hash and no refs.
        {
            let seen = registrar.seen.lock().unwrap();
            assert_eq!(seen.len(), 1);
            assert_eq!(seen[0].0, out.root.paths.store_name);
            assert_eq!(seen[0].1, shade_cdf::blake3_hex(&out.root.cdf));
            assert!(seen[0].2.is_empty());
        }
        assert_eq!(sandbox.spawns(), 2, "two phases, two spawns");

        // Run 2: pure lookup — no spawn, no new registration, same path.
        let out2 = exec.run(&trivial_recipe(), Some("tc-1"), &io).expect("second run");
        match out2.root_result() {
            Built::Resolved { source, out_path: p2 } => {
                assert_eq!(source, "local");
                assert_eq!(p2, out_path);
            }
            other => panic!("expected a lookup hit, got {other:?}"),
        }
        assert_eq!(sandbox.spawns(), 2, "a hit must not spawn");
        assert_eq!(registrar.seen.lock().unwrap().len(), 1, "a hit must not re-register");
    });
}

/// Dependencies build before dependents, land in the store, and the
/// dependent's registration carries the dep as a ref.
#[test]
fn executor_orders_deps_before_dependents() {
    with_stack(|| {
        let tmp = TmpDir::new("exec-topo");
        let (store, build, log) = exec_roots(&tmp);
        let io = HostIo;
        let recipe = RecipeRef::Expr {
            src: r#"
let
  depa = derivation {
    name = "depa";
    version = "1.0";
    system = "x86_64-oros";
    phases = [ "mkdir -p $out/bin" "printf '#!/bin/sh\necho depa' > $out/bin/depa" "chmod +x $out/bin/depa" ];
    outputs = { bin = [ "depa" ]; };
  };
in derivation {
  name = "top";
  version = "1.0";
  system = "x86_64-oros";
  deps = [ depa ];
  phases = [ "mkdir -p $out/bin" "depa > $out/bin/top" ];
  outputs = { bin = [ "top" ]; };
}"#
            .to_string(),
            base_dir: "/base".to_string(),
        };
        let local = LocalStore;
        let resolvers: [&dyn Resolver; 1] = [&local];
        let sandbox = PermissiveSandbox;
        let registrar = RecordingRegistrar::default();
        let exec = Executor::new(&store, &build, &log, &resolvers, &sandbox, &registrar);

        let out = exec.run(&recipe, Some("tc-1"), &io).expect("run");
        assert_eq!(out.results.len(), 2);
        assert!(out.results[0].0.contains("-depa-"), "dep builds first");
        assert!(out.results[1].0.contains("-top-"), "root builds last");
        // top's build ran depa's binary off PATH (dep bin dirs head PATH),
        // proving the dep was realized and wired in before top built.
        assert_eq!(
            fs::read(out.root_result().out_path().join("bin/top")).unwrap(),
            b"depa\n"
        );
        // Registration refs: top carries depa's canonical store path.
        let seen = registrar.seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert!(seen[1].2.len() == 1 && seen[1].2[0].contains("-depa-1.0"));
        assert!(seen[0].2.is_empty());
    });
}

/// Recipe env vars round-trip the CDF's lowercase fold back to uppercase.
#[test]
fn executor_restores_env_uppercase() {
    with_stack(|| {
        let tmp = TmpDir::new("exec-env");
        let (store, build, log) = exec_roots(&tmp);
        let io = HostIo;
        let recipe = RecipeRef::Expr {
            src: r#"
derivation {
  name = "envy";
  version = "1.0";
  system = "x86_64-oros";
  env = { GREETING = "hello"; };
  phases = [ "mkdir -p $out/bin && printf %s \"$GREETING\" > $out/bin/envy" ];
  outputs = { bin = [ "envy" ]; };
}"#
            .to_string(),
            base_dir: "/base".to_string(),
        };
        let local = LocalStore;
        let resolvers: [&dyn Resolver; 1] = [&local];
        let (sandbox, registrar) = (PermissiveSandbox, NoopRegistrar);
        let exec = Executor::new(&store, &build, &log, &resolvers, &sandbox, &registrar);
        let out = exec.run(&recipe, Some("tc-1"), &io).expect("run");
        assert_eq!(
            fs::read(out.root_result().out_path().join("bin/envy")).unwrap(),
            b"hello"
        );
    });
}

/// Wraps [`PermissiveSandbox`], recording the `$out` each phase actually runs
/// with — makes "every phase of a multi-phase build sees the identical, correct
/// `$out`" observable at the seam.
struct OutRecordingSandbox {
    inner: PermissiveSandbox,
    per_phase_out: Mutex<Vec<String>>,
}
impl OutRecordingSandbox {
    fn new() -> Self {
        OutRecordingSandbox { inner: PermissiveSandbox, per_phase_out: Mutex::new(Vec::new()) }
    }
}
impl BuildSandbox for OutRecordingSandbox {
    fn prepare(&self, spec: &SandboxSpec) -> std::io::Result<BuildEnv> {
        self.inner.prepare(spec)
    }
    fn spawn(&self, env: &BuildEnv, command: &str, log: &fs::File) -> std::io::Result<i32> {
        let out = env
            .vars
            .iter()
            .find(|(k, _)| k == "out")
            .map(|(_, v)| v.clone())
            .expect("$out must be set for every phase");
        self.per_phase_out.lock().unwrap().push(out);
        self.inner.spawn(env, command, log)
    }
    fn collect_outputs(&self, env: &BuildEnv, declared: &[String]) -> std::io::Result<Vec<PathBuf>> {
        self.inner.collect_outputs(env, declared)
    }
}

/// Regression (`$out` under phased builds): every phase of a multi-phase build
/// runs with the **same** `$out` — the per-node staging dir prepared once, not
/// re-derived per phase. Each phase records its own view of `$out`; the seam's
/// per-phase capture and the on-disk trace the phases wrote must all agree.
#[test]
fn out_is_identical_across_every_phase() {
    with_stack(|| {
        let tmp = TmpDir::new("exec-out-phases");
        let (store, build, log) = exec_roots(&tmp);
        let io = HostIo;
        // Four phases: one to make the tree, three that each append their view
        // of `$out`. If any phase saw a different (or empty) `$out`, the trace
        // lines diverge or the writes miss the staging tree entirely.
        let recipe = RecipeRef::Expr {
            src: r#"
derivation {
  name = "phased";
  version = "1.0";
  system = "x86_64-oros";
  phases = [
    "mkdir -p $out/bin"
    "printf '%s\n' \"$out\" >> $out/bin/trace"
    "printf '%s\n' \"$out\" >> $out/bin/trace"
    "printf '%s\n' \"$out\" >> $out/bin/trace"
  ];
  outputs = { bin = [ "trace" ]; };
}"#
            .to_string(),
            base_dir: "/base".to_string(),
        };
        let local = LocalStore;
        let resolvers: [&dyn Resolver; 1] = [&local];
        let sandbox = OutRecordingSandbox::new();
        let registrar = NoopRegistrar;
        let exec = Executor::new(&store, &build, &log, &resolvers, &sandbox, &registrar);

        let out = exec.run(&recipe, Some("tc-1"), &io).expect("run");

        // Seam view: four phases, four recorded `$out`s, all identical, each the
        // `<scratch>/out` staging dir (never empty, never re-derived).
        let recorded = sandbox.per_phase_out.lock().unwrap().clone();
        assert_eq!(recorded.len(), 4, "one $out recorded per phase");
        let staging = &recorded[0];
        assert!(!staging.is_empty(), "$out must be non-empty");
        assert!(staging.ends_with("/out"), "$out is the staging dir: {staging}");
        assert!(recorded.iter().all(|o| o == staging), "phases saw different $out: {recorded:?}");

        // On-disk view: the three append phases each wrote the same staging
        // `$out`, and that value matches what the seam recorded.
        let trace = fs::read_to_string(out.root_result().out_path().join("bin/trace")).unwrap();
        let lines: Vec<&str> = trace.lines().collect();
        assert_eq!(lines.len(), 3, "three append phases, three lines: {trace:?}");
        assert!(lines.iter().all(|l| *l == staging), "trace disagrees with $out: {trace:?}");
    });
}

/// A nonzero phase exit fails the derivation: correct error (with errno),
/// scratch cleaned, log kept, store untouched.
#[test]
fn executor_phase_failure_cleans_scratch_store_untouched() {
    with_stack(|| {
        let tmp = TmpDir::new("exec-fail");
        let (store, build, log) = exec_roots(&tmp);
        let io = HostIo;
        let recipe = RecipeRef::Expr {
            src: r#"
derivation {
  name = "boom";
  version = "1.0";
  system = "x86_64-oros";
  phases = [ "echo starting" "exit 3" ];
  outputs = { bin = [ "boom" ]; };
}"#
            .to_string(),
            base_dir: "/base".to_string(),
        };
        let plan = plan(&recipe, store.to_str().unwrap(), Some("tc-1"), &io).expect("plan");
        let local = LocalStore;
        let resolvers: [&dyn Resolver; 1] = [&local];
        let (sandbox, registrar) = (PermissiveSandbox, NoopRegistrar);
        let exec = Executor::new(&store, &build, &log, &resolvers, &sandbox, &registrar);

        let err = exec.run(&recipe, Some("tc-1"), &io).unwrap_err();
        match &err {
            BuildError::PhaseFailed { phase, code, log: log_file, .. } => {
                assert_eq!(*phase, 1);
                assert_eq!(*code, 3);
                let text = fs::read_to_string(log_file).unwrap();
                assert!(text.contains("starting"), "phase 0 output captured");
                assert!(text.contains("exit code 3"));
            }
            other => panic!("expected PhaseFailed, got {other:?}"),
        }
        assert_eq!(err.errno(), lythos_abi::errno::EINVAL);
        // Store untouched: no out path, no .drv.
        assert!(!Path::new(&plan.paths.out_path).exists());
        assert!(!Path::new(&plan.paths.drv_path).exists());
        // Scratch (with any partial outputs) removed.
        assert!(!build.join(&plan.paths.store_name).exists());
    });
}

/// A build that exits 0 but does not produce a declared output fails, and
/// nothing reaches the store.
#[test]
fn executor_missing_declared_output_fails() {
    with_stack(|| {
        let tmp = TmpDir::new("exec-missing");
        let (store, build, log) = exec_roots(&tmp);
        let io = HostIo;
        let recipe = RecipeRef::Expr {
            src: r#"
derivation {
  name = "hollow";
  version = "1.0";
  system = "x86_64-oros";
  phases = [ "true" ];
  outputs = { bin = [ "hollow" ]; };
}"#
            .to_string(),
            base_dir: "/base".to_string(),
        };
        let plan = plan(&recipe, store.to_str().unwrap(), Some("tc-1"), &io).expect("plan");
        let local = LocalStore;
        let resolvers: [&dyn Resolver; 1] = [&local];
        let (sandbox, registrar) = (PermissiveSandbox, NoopRegistrar);
        let exec = Executor::new(&store, &build, &log, &resolvers, &sandbox, &registrar);

        let err = exec.run(&recipe, Some("tc-1"), &io).unwrap_err();
        match &err {
            BuildError::MissingOutput { detail, .. } => {
                assert!(detail.contains("bin/hollow"), "names the missing output: {detail}");
            }
            other => panic!("expected MissingOutput, got {other:?}"),
        }
        assert_eq!(err.errno(), lythos_abi::errno::ENOENT);
        assert!(!Path::new(&plan.paths.out_path).exists());
        assert!(!build.join(&plan.paths.store_name).exists());
    });
}

/// `keep_failed` preserves the scratch dir of a failed build for autopsy.
#[test]
fn executor_keep_failed_keeps_scratch() {
    with_stack(|| {
        let tmp = TmpDir::new("exec-keep");
        let (store, build, log) = exec_roots(&tmp);
        let io = HostIo;
        let recipe = RecipeRef::Expr {
            src: r#"
derivation {
  name = "keepme";
  version = "1.0";
  system = "x86_64-oros";
  phases = [ "touch partial-artifact" "false" ];
  outputs = { bin = [ "keepme" ]; };
}"#
            .to_string(),
            base_dir: "/base".to_string(),
        };
        let plan = plan(&recipe, store.to_str().unwrap(), Some("tc-1"), &io).expect("plan");
        let local = LocalStore;
        let resolvers: [&dyn Resolver; 1] = [&local];
        let (sandbox, registrar) = (PermissiveSandbox, NoopRegistrar);
        let mut exec = Executor::new(&store, &build, &log, &resolvers, &sandbox, &registrar);
        exec.keep_failed = true;

        exec.run(&recipe, Some("tc-1"), &io).unwrap_err();
        let scratch = build.join(&plan.paths.store_name);
        assert!(scratch.join("partial-artifact").exists(), "scratch kept with its contents");
        assert!(!Path::new(&plan.paths.out_path).exists(), "store still untouched");
    });
}

/// A source derivation (`builder=fetch`) that is not already in the store
/// cannot be built — fetch is unimplemented — and says so with ENOSYS.
#[test]
fn executor_fetch_dep_unrealized_is_enosys() {
    with_stack(|| {
        let tmp = TmpDir::new("exec-fetch");
        let (store, build, log) = exec_roots(&tmp);
        let io = HostIo;
        let recipe = RecipeRef::Expr {
            src: r#"
derivation {
  name = "wants-src";
  version = "1.0";
  system = "x86_64-oros";
  deps = [ (builtins.fetchCratesIo {
    crate = "serde";
    version = "1.0.0";
    sha256 = "9f1c2ab34c1d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e0f";
  }) ];
  phases = [ "mkdir -p $out/bin && touch $out/bin/x" ];
  outputs = { bin = [ "x" ]; };
}"#
            .to_string(),
            base_dir: "/base".to_string(),
        };
        let local = LocalStore;
        let resolvers: [&dyn Resolver; 1] = [&local];
        let (sandbox, registrar) = (PermissiveSandbox, NoopRegistrar);
        let exec = Executor::new(&store, &build, &log, &resolvers, &sandbox, &registrar);
        let err = exec.run(&recipe, Some("tc-1"), &io).unwrap_err();
        assert!(matches!(err, BuildError::FetchUnrealized { .. }), "got {err}");
        assert_eq!(err.errno(), lythos_abi::errno::ENOSYS);
    });
}

/// GATE (store-db): the real [`DbRegistrar`] records every realization under
/// `/shade/db/`, and a dependent's record carries its dep's digest as a
/// reference — the seed `shade gc` marks from.
#[test]
fn executor_dbregistrar_records_refs() {
    with_stack(|| {
        let tmp = TmpDir::new("exec-db");
        // Canonical /shade layout: store is a sibling of db/ under the prefix.
        let shade = tmp.path().join("shade");
        let store = shade.join("store");
        let build = shade.join("build");
        let log = shade.join("log");
        let io = HostIo;
        let recipe = RecipeRef::Expr {
            src: r#"
let
  depa = derivation {
    name = "depa";
    version = "1.0";
    system = "x86_64-oros";
    phases = [ "mkdir -p $out/bin" "printf '#!/bin/sh\necho depa' > $out/bin/depa" "chmod +x $out/bin/depa" ];
    outputs = { bin = [ "depa" ]; };
  };
in derivation {
  name = "top";
  version = "1.0";
  system = "x86_64-oros";
  deps = [ depa ];
  phases = [ "mkdir -p $out/bin" "depa > $out/bin/top" ];
  outputs = { bin = [ "top" ]; };
}"#
            .to_string(),
            base_dir: "/base".to_string(),
        };
        let local = LocalStore;
        let resolvers: [&dyn Resolver; 1] = [&local];
        let sandbox = PermissiveSandbox;
        let registrar = DbRegistrar::for_store_root(&store);
        let exec = Executor::new(&store, &build, &log, &resolvers, &sandbox, &registrar);

        let out = exec.run(&recipe, Some("tc-1"), &io).expect("run");
        assert_eq!(out.results.len(), 2);

        // Both realizations are registered valid in the db.
        let db = shade_store_db::StoreDb::for_store_root(&store);
        let dep_name = &out.results[0].0; // <digest>-depa-1.0
        let top_name = &out.results[1].0; // <digest>-top-1.0
        let dep_digest = &dep_name[..32];
        let top_digest = &top_name[..32];
        assert!(db.is_valid(dep_digest), "dep registered valid");
        assert!(db.is_valid(top_digest), "top registered valid");

        // top's reference record carries depa's digest (declared dep).
        let refs = db.read_refs(top_digest).unwrap();
        assert!(refs.contains(&dep_digest.to_string()), "top refs depa: {refs:?}");
        assert!(db.read_refs(dep_digest).unwrap().is_empty(), "leaf dep has no refs");

        // GATE tail: root top → gc keeps both; unroot → gc collects both.
        db.add_root("test-top", out.root_result().out_path().to_str().unwrap()).unwrap();
        let r1 = db.gc(&shade_store_db::GcOptions::default()).unwrap();
        assert_eq!(r1.collected.len(), 0, "rooted closure fully kept");
        assert!(store.join(dep_name).exists() && store.join(top_name).exists());

        db.remove_root("test-top").unwrap();
        let r2 = db.gc(&shade_store_db::GcOptions::default()).unwrap();
        // Four store entries collected: both output dirs and their `.drv`s.
        assert_eq!(r2.collected.len(), 4, "unrooted closure collected (dirs + .drv)");
        assert!(r2.collected.contains(dep_name) && r2.collected.contains(top_name));
        assert!(!store.join(dep_name).exists() && !store.join(top_name).exists());
        assert!(!store.join(format!("{dep_name}.drv")).exists());
    });
}

#[test]
fn non_derivation_is_rejected() {
    with_stack(|| {
        let tmp = TmpDir::new("notdrv");
        let store = tmp.path().join("store");
        let io = HostIo;
        let recipe = RecipeRef::Expr { src: "42".to_string(), base_dir: "/base".to_string() };
        let err = plan(&recipe, store.to_str().unwrap(), Some("tc-1"), &io).unwrap_err();
        assert!(matches!(err, BuildError::NotADerivation), "got {err}");
    });
}

// ---- LythosSandbox tests ------------------------------------------------------
//
// The enforcement vehicle is macOS Seatbelt (docs/shade/build-sandbox.md §3),
// so the tests that need real denial are macOS-gated; the pure SandboxPlan
// model is tested host-independently in sandbox.rs.

/// Recursively collect (rel_path, bytes) for every file under `root`, sorted.
#[cfg(target_os = "macos")]
fn tree_bytes(root: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        for e in fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let p = e.path();
            if p.is_dir() {
                walk(base, &p, out);
            } else {
                let rel = p.strip_prefix(base).unwrap().to_string_lossy().into_owned();
                out.push((rel, fs::read(&p).unwrap()));
            }
        }
    }
    let mut v = Vec::new();
    walk(root, root, &mut v);
    v.sort();
    v
}

/// GATE (sandbox): the same derivation built twice through [`LythosSandbox`]
/// yields byte-identical outputs. The recipe deliberately samples every
/// determinism knob the sandbox pins (epoch, TZ, locale, HOME, umask-affected
/// file bits) so drift in any of them changes the bytes.
#[cfg(target_os = "macos")]
#[test]
fn lythos_sandbox_rebuild_is_byte_identical() {
    with_stack(|| {
        let recipe_src = r#"
derivation {
  name = "det";
  version = "1.0";
  system = "x86_64-oros";
  phases = [
    "mkdir -p $out/bin"
    "printf '%s|%s|%s|%s|%s|' \"$TZ\" \"$LC_ALL\" \"$HOME\" \"$SOURCE_DATE_EPOCH\" \"$LANG\" > $out/bin/stamp"
    "date -u -r 0 >> $out/bin/stamp"
    "umask >> $out/bin/stamp"
  ];
  outputs = { bin = [ "stamp" ]; };
}"#;
        let recipe = |base: &str| RecipeRef::Expr {
            src: recipe_src.to_string(),
            base_dir: base.to_string(),
        };
        let io = HostIo;
        let local = LocalStore;
        let resolvers: [&dyn Resolver; 1] = [&local];
        let registrar = NoopRegistrar;
        let sandbox = LythosSandbox::new().expect("sandbox-exec available on macOS");

        let mut trees = Vec::new();
        for run in 0..2 {
            let tmp = TmpDir::new(&format!("lyth-det-{run}"));
            let (store, build, log) = exec_roots(&tmp);
            let exec = Executor::new(&store, &build, &log, &resolvers, &sandbox, &registrar);
            let out = exec.run(&recipe("/base"), Some("tc-1"), &io).expect("build");
            let Built::Realized { out_path } = out.root_result() else {
                panic!("fresh store must build, got {:?}", out.root_result());
            };
            trees.push(tree_bytes(out_path));
        }
        assert_eq!(trees[0], trees[1], "same derivation twice ⇒ byte-identical outputs");
        assert!(!trees[0].is_empty());
    });
}

/// A builder that attempts network access fails: the plan answers ENOPERM
/// (no capability reaches a network device or daemon), and the vehicle
/// enforces it — the phase dies with "Operation not permitted".
#[cfg(target_os = "macos")]
#[test]
fn lythos_sandbox_denies_network() {
    with_stack(|| {
        let tmp = TmpDir::new("lyth-net");
        let (store, build, log) = exec_roots(&tmp);
        let io = HostIo;
        let recipe = RecipeRef::Expr {
            src: r#"
derivation {
  name = "netprobe";
  version = "1.0";
  system = "x86_64-oros";
  phases = [ "mkdir -p $out/bin" "echo probe > /dev/tcp/127.0.0.1/9 && echo reached > $out/bin/net" ];
  outputs = { bin = [ "net" ]; };
}"#
            .to_string(),
            base_dir: "/base".to_string(),
        };
        let local = LocalStore;
        let resolvers: [&dyn Resolver; 1] = [&local];
        let registrar = NoopRegistrar;
        let sandbox = LythosSandbox::new().unwrap();
        let exec = Executor::new(&store, &build, &log, &resolvers, &sandbox, &registrar);

        let err = exec.run(&recipe, Some("tc-1"), &io).unwrap_err();
        let BuildError::PhaseFailed { phase, log: log_file, .. } = &err else {
            panic!("expected PhaseFailed, got {err}");
        };
        assert_eq!(*phase, 1);
        let text = fs::read_to_string(log_file).unwrap();
        assert!(
            text.contains("Operation not permitted"),
            "network denial must surface as EPERM in the builder: {text}"
        );

        // The model answers the same question with the ABI errno.
        let scratch = build.join("netprobe-scratch-model");
        let staging = scratch.join("out");
        fs::create_dir_all(scratch.join("tmp")).unwrap();
        fs::create_dir_all(&staging).unwrap();
        let spec = SandboxSpec {
            store_name: "x-netprobe-1.0",
            scratch: &scratch,
            staging: &staging,
            system: "x86_64-oros",
            env: &[],
            inputs: &[],
            jobs: 1,
        };
        let plan = SandboxPlan::from_spec(&spec).unwrap();
        assert_eq!(plan.check_network(), Err(lythos_abi::errno::ENOPERM));
    });
}

/// A builder that reads outside its declared inputs fails — and the same
/// phase under PermissiveSandbox succeeds, proving the denial came from the
/// sandbox, not the recipe.
#[cfg(target_os = "macos")]
#[test]
fn lythos_sandbox_denies_out_of_tree_read() {
    with_stack(|| {
        let tmp = TmpDir::new("lyth-read");
        let secret = tmp.path().join("secret.txt");
        fs::write(&secret, b"TOPSECRET").unwrap();
        let recipe = RecipeRef::Expr {
            src: format!(
                r#"
derivation {{
  name = "peek";
  version = "1.0";
  system = "x86_64-oros";
  phases = [ "mkdir -p $out/bin" "cat {} > $out/bin/loot" ];
  outputs = {{ bin = [ "loot" ]; }};
}}"#,
                secret.display()
            ),
            base_dir: "/base".to_string(),
        };
        let io = HostIo;
        let local = LocalStore;
        let resolvers: [&dyn Resolver; 1] = [&local];
        let registrar = NoopRegistrar;

        // Sandboxed: the read is denied and the phase fails.
        {
            let (store, build, log) =
                (tmp.path().join("s1"), tmp.path().join("b1"), tmp.path().join("l1"));
            let sandbox = LythosSandbox::new().unwrap();
            let exec = Executor::new(&store, &build, &log, &resolvers, &sandbox, &registrar);
            let err = exec.run(&recipe, Some("tc-1"), &io).unwrap_err();
            let BuildError::PhaseFailed { log: log_file, .. } = &err else {
                panic!("expected PhaseFailed, got {err}");
            };
            let text = fs::read_to_string(log_file).unwrap();
            assert!(
                text.contains("Operation not permitted"),
                "out-of-tree read must be EPERM: {text}"
            );
            // Model errno for the same access.
            let scratch = build.join("model");
            let staging = scratch.join("out");
            fs::create_dir_all(scratch.join("tmp")).unwrap();
            fs::create_dir_all(&staging).unwrap();
            let spec = SandboxSpec {
                store_name: "x-peek-1.0",
                scratch: &scratch,
                staging: &staging,
                system: "x86_64-oros",
                env: &[],
                inputs: &[],
                jobs: 1,
            };
            let plan = SandboxPlan::from_spec(&spec).unwrap();
            assert_eq!(
                plan.check_read(&fs::canonicalize(&secret).unwrap()),
                Err(lythos_abi::errno::ENOPERM)
            );
        }

        // Control: PermissiveSandbox lets the identical phase through.
        {
            let (store, build, log) =
                (tmp.path().join("s2"), tmp.path().join("b2"), tmp.path().join("l2"));
            let sandbox = PermissiveSandbox;
            let exec = Executor::new(&store, &build, &log, &resolvers, &sandbox, &registrar);
            let out = exec.run(&recipe, Some("tc-1"), &io).expect("permissive control build");
            assert_eq!(
                fs::read(out.root_result().out_path().join("bin/loot")).unwrap(),
                b"TOPSECRET",
                "control proves the sandbox, not the recipe, caused the denial"
            );
        }
    });
}

/// Declared inputs are visible (their binaries run off PATH); an undeclared
/// store sibling is not readable even though it sits in the same store root.
#[cfg(target_os = "macos")]
#[test]
fn lythos_sandbox_hides_undeclared_store_paths() {
    with_stack(|| {
        let tmp = TmpDir::new("lyth-store");
        let (store, build, log) = exec_roots(&tmp);
        // Plant an undeclared entry directly in the store root.
        let undeclared = store.join("zzzzzzzz-undeclared-1.0");
        fs::create_dir_all(&undeclared).unwrap();
        fs::write(undeclared.join("secret"), b"UNDECLARED").unwrap();

        let recipe = RecipeRef::Expr {
            src: r#"
let
  depa = derivation {
    name = "depa";
    version = "1.0";
    system = "x86_64-oros";
    phases = [ "mkdir -p $out/bin" "printf '#!/bin/sh\necho depa-ok' > $out/bin/depa" "chmod +x $out/bin/depa" ];
    outputs = { bin = [ "depa" ]; };
  };
in derivation {
  name = "scoped";
  version = "1.0";
  system = "x86_64-oros";
  deps = [ depa ];
  phases = [
    "mkdir -p $out/bin"
    "depa > $out/bin/dep-out"
    "ls ../../store/zzzzzzzz-undeclared-1.0/secret > /dev/null 2>&1 || true"
    "cat ../../store/zzzzzzzz-undeclared-1.0/secret >> $out/bin/dep-out 2>/dev/null || echo denied >> $out/bin/dep-out"
  ];
  outputs = { bin = [ "dep-out" ]; };
}"#
            .to_string(),
            base_dir: "/base".to_string(),
        };
        let io = HostIo;
        let local = LocalStore;
        let resolvers: [&dyn Resolver; 1] = [&local];
        let registrar = NoopRegistrar;
        let sandbox = LythosSandbox::new().unwrap();
        let exec = Executor::new(&store, &build, &log, &resolvers, &sandbox, &registrar);

        let out = exec.run(&recipe, Some("tc-1"), &io).expect("build");
        let bytes = fs::read(out.root_result().out_path().join("bin/dep-out")).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("depa-ok"), "declared dep runs: {text}");
        assert!(text.contains("denied"), "undeclared store sibling unreadable: {text}");
        assert!(!text.contains("UNDECLARED"), "no secret bytes leaked: {text}");
    });
}

/// The build environment is hermetic: no host variables leak; the fixed
/// identity/env table is exactly what the builder sees.
#[cfg(target_os = "macos")]
#[test]
fn lythos_sandbox_env_is_hermetic() {
    with_stack(|| {
        let tmp = TmpDir::new("lyth-env");
        let (store, build, log) = exec_roots(&tmp);
        let recipe = RecipeRef::Expr {
            src: r#"
derivation {
  name = "envdump";
  version = "1.0";
  system = "x86_64-oros";
  phases = [ "mkdir -p $out/bin" "printenv | sort > $out/bin/dump" ];
  outputs = { bin = [ "dump" ]; };
}"#
            .to_string(),
            base_dir: "/base".to_string(),
        };
        let io = HostIo;
        let local = LocalStore;
        let resolvers: [&dyn Resolver; 1] = [&local];
        let registrar = NoopRegistrar;
        let sandbox = LythosSandbox::new().unwrap();
        let exec = Executor::new(&store, &build, &log, &resolvers, &sandbox, &registrar);

        let out = exec.run(&recipe, Some("tc-1"), &io).expect("build");
        let dump = fs::read_to_string(out.root_result().out_path().join("bin/dump")).unwrap();
        // Host identity/env must not leak.
        for host_var in ["USER=", "LOGNAME=", "SSH_AUTH_SOCK=", "XDG_", "CARGO_"] {
            assert!(!dump.contains(host_var), "host env leaked ({host_var}): {dump}");
        }
        // The fixed table is present with the pinned values.
        assert!(dump.contains(&format!("HOME={}\n", shade_build_home())));
        assert!(dump.contains("TZ=UTC\n"));
        assert!(dump.contains("LC_ALL=C.UTF-8\n"));
        assert!(dump.contains("SOURCE_DATE_EPOCH=0\n"));
        // PATH is inputs + the fixed host tool tail only — no host PATH.
        let path_line = dump.lines().find(|l| l.starts_with("PATH=")).unwrap();
        assert_eq!(path_line, "PATH=/usr/bin:/bin", "no deps ⇒ tool tail only: {path_line}");
    });
}

#[cfg(target_os = "macos")]
fn shade_build_home() -> &'static str {
    crate::sandbox::SANDBOX_HOME
}

/// Output confinement, staging half: a build that stages an undeclared
/// top-level tree under $out is rejected before anything reaches the store.
#[cfg(target_os = "macos")]
#[test]
fn lythos_sandbox_rejects_undeclared_staged_output() {
    with_stack(|| {
        let tmp = TmpDir::new("lyth-smuggle");
        let (store, build, log) = exec_roots(&tmp);
        let recipe = RecipeRef::Expr {
            src: r#"
derivation {
  name = "smuggle";
  version = "1.0";
  system = "x86_64-oros";
  phases = [ "mkdir -p $out/bin $out/contraband" "printf ok > $out/bin/smuggle" "printf x > $out/contraband/x" ];
  outputs = { bin = [ "smuggle" ]; };
}"#
            .to_string(),
            base_dir: "/base".to_string(),
        };
        let plan = plan(&recipe, store.to_str().unwrap(), Some("tc-1"), &HostIo).expect("plan");
        let io = HostIo;
        let local = LocalStore;
        let resolvers: [&dyn Resolver; 1] = [&local];
        let registrar = NoopRegistrar;
        let sandbox = LythosSandbox::new().unwrap();
        let exec = Executor::new(&store, &build, &log, &resolvers, &sandbox, &registrar);

        let err = exec.run(&recipe, Some("tc-1"), &io).unwrap_err();
        let BuildError::MissingOutput { detail, .. } = &err else {
            panic!("expected output-confinement rejection, got {err}");
        };
        assert!(detail.contains("contraband"), "names the undeclared tree: {detail}");
        assert!(!Path::new(&plan.paths.out_path).exists(), "store untouched");
        assert!(!Path::new(&plan.paths.drv_path).exists());
    });
}

// ---- StoreFs seam routing (B3) --------------------------------------------------

/// A shared handle to one [`shade_store::MemFs`] so the executor's injected
/// backend and the sandbox test double see the same in-memory tree.
#[derive(Clone)]
struct SharedMem(std::rc::Rc<std::cell::RefCell<shade_store::MemFs>>);

impl SharedMem {
    fn new() -> Self {
        SharedMem(std::rc::Rc::new(std::cell::RefCell::new(shade_store::MemFs::new())))
    }
    fn read(&self, path: &str) -> Vec<u8> {
        use shade_store::StoreFs;
        self.0.borrow_mut().read_file(path).unwrap_or_else(|e| panic!("read {path}: {e}"))
    }
    fn has(&self, path: &str) -> bool {
        use shade_store::StoreFs;
        self.0.borrow_mut().exists(path)
    }
}

impl shade_store::StoreFs for SharedMem {
    fn metadata(&mut self, path: &str) -> shade_store::FsResult<shade_store::NodeMeta> {
        self.0.borrow_mut().metadata(path)
    }
    fn read_file(&mut self, path: &str) -> shade_store::FsResult<Vec<u8>> {
        self.0.borrow_mut().read_file(path)
    }
    fn write_file(&mut self, path: &str, data: &[u8], exec: bool) -> shade_store::FsResult<()> {
        self.0.borrow_mut().write_file(path, data, exec)
    }
    fn create_exclusive(&mut self, path: &str, data: &[u8]) -> shade_store::FsResult<()> {
        self.0.borrow_mut().create_exclusive(path, data)
    }
    fn mkdir(&mut self, path: &str) -> shade_store::FsResult<()> {
        self.0.borrow_mut().mkdir(path)
    }
    fn rename(&mut self, old: &str, new: &str) -> shade_store::FsResult<()> {
        self.0.borrow_mut().rename(old, new)
    }
    fn unlink(&mut self, path: &str) -> shade_store::FsResult<()> {
        self.0.borrow_mut().unlink(path)
    }
    fn rmdir(&mut self, path: &str) -> shade_store::FsResult<()> {
        self.0.borrow_mut().rmdir(path)
    }
    fn read_dir(&mut self, path: &str) -> shade_store::FsResult<Vec<(String, shade_store::NodeKind)>> {
        self.0.borrow_mut().read_dir(path)
    }
    fn read_link(&mut self, path: &str) -> shade_store::FsResult<String> {
        self.0.borrow_mut().read_link(path)
    }
    fn symlink(&mut self, target: &str, link: &str) -> shade_store::FsResult<()> {
        self.0.borrow_mut().symlink(target, link)
    }
    fn unique_token(&mut self) -> u64 {
        use shade_store::StoreFs;
        self.0.borrow_mut().unique_token()
    }
}

/// Sandbox double that stages the declared output **through the seam** and
/// writes its phase output to the sandbox log fd — the executor around it
/// must not need the host filesystem for scratch, staging, log, or store.
struct SeamSandbox(SharedMem);

impl BuildSandbox for SeamSandbox {
    fn prepare(&self, spec: &SandboxSpec) -> std::io::Result<BuildEnv> {
        Ok(BuildEnv {
            cwd: spec.scratch.to_path_buf(),
            staging: spec.staging.to_path_buf(),
            vars: Vec::new(),
        })
    }
    fn spawn(&self, env: &BuildEnv, command: &str, log: &fs::File) -> std::io::Result<i32> {
        // Phase output goes to the sandbox-provided fd, like a real child.
        use std::io::Write;
        writeln!(&mut &*log, "SEAM-PHASE ran: {command}")?;
        // "Build": stage the declared output tree through the shared seam.
        let staging = env.staging.to_str().unwrap();
        let mut fs = self.0.clone();
        let bin = format!("{staging}/bin");
        shade_store::backend::create_dir_all(&mut fs, &bin)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        let demo = format!("{bin}/demo");
        if !fs.0.borrow_mut().exists(&demo) {
            fs.write_file(&demo, b"hi", true)
                .map_err(|e| std::io::Error::other(e.to_string()))?;
        }
        Ok(0)
    }
    fn collect_outputs(&self, env: &BuildEnv, declared: &[String]) -> std::io::Result<Vec<PathBuf>> {
        let staging = env.staging.to_str().unwrap();
        let mut out = Vec::new();
        for rel in declared {
            let p = format!("{staging}/{rel}");
            if !self.0.clone().0.borrow_mut().exists(&p) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("declared output `{rel}` was not staged"),
                ));
            }
            out.push(PathBuf::from(p));
        }
        Ok(out)
    }
}

/// GATE (B3): every filesystem operation the executor does *itself* —
/// scratch setup/teardown, the build log, the store realization — goes
/// through the injected [`StoreFs`] backend. A full build against a MemFs
/// under the canonical `/shade` roots leaves output, `.drv`, and log in the
/// MemFs, cleans the scratch there, and never creates `/shade` on the host.
/// Together with the (default-`HostFs`) suite above, this is the
/// both-backends proof.
#[test]
fn executor_scratch_log_store_route_through_seam() {
    with_stack(|| {
        let mem = SharedMem::new();
        let sandbox = SeamSandbox(mem.clone());
        let resolvers: [&dyn Resolver; 0] = [];
        let registrar = NoopRegistrar;
        let mut exec = Executor::new(
            "/shade/store",
            "/shade/build",
            "/shade/log",
            &resolvers,
            &sandbox,
            &registrar,
        );
        exec.set_fs(mem.clone());

        let out = exec.run(&trivial_recipe(), Some("tc-1"), &HostIo).expect("seam build");
        let Built::Realized { out_path } = out.root_result() else {
            panic!("expected Realized, got {:?}", out.root_result());
        };
        let out_str = out_path.to_str().unwrap();
        let store_name = &out.root.paths.store_name;

        // Output + .drv realized in the MemFs, addressed under /shade/store.
        assert!(out_str.starts_with("/shade/store/"), "canonical root: {out_str}");
        assert_eq!(mem.read(&format!("{out_str}/bin/demo")), b"hi");
        assert_eq!(mem.read(&out.root.paths.drv_path), out.root.cdf);

        // Log written through the seam: executor phase headers interleaved
        // with the child output the sandbox streamed to the log fd.
        let log = String::from_utf8(mem.read(&format!("/shade/log/{store_name}.log"))).unwrap();
        assert!(log.contains("phase 0: mkdir -p $out/bin"), "phase header: {log}");
        assert!(log.contains("SEAM-PHASE ran:"), "child output folded in: {log}");

        // Scratch created and cleaned in the MemFs.
        assert!(!mem.has(&format!("/shade/build/{store_name}")), "scratch cleaned");

        // The host filesystem never saw the canonical roots.
        assert!(
            !Path::new("/shade/build").join(store_name).exists()
                && !Path::new("/shade/store").join(store_name).exists(),
            "host /shade untouched"
        );
    });
}
