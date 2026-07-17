//! Generations/profiles/activation tests — the prompt-4 gate: two
//! generations switch and roll back via the symlink flip, a system generation
//! built from `prism.shade` activates, `list` shows the history, every
//! generation is a GC root, and boot activates pre-built generations only.
//!
//! The engine is backend-injected (the B1 [`StoreFs`] seam): the suite runs
//! the gate on the host backend (`GenLine::system`, real symlinks) and again
//! through a shared [`MemFs`] (`seam_*` tests) to prove no test leans on
//! `std::fs` behavior the seam does not promise.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use shade_store::{MemFs, StoreFs};
use shade_store_db::{GcOptions, StoreDb};
use shadec::io::HostIo;

use super::*;

/// The evaluator is `Rc`-based and recurses; run eval-driven cases on a big
/// stack (same as shade-build's tests).
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
        let p = std::env::temp_dir().join(format!("shade-gen-test-{tag}-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        TmpDir(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for TmpDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A fake realized store entry `<digest>-<name>-<version>` with one
/// `bin/<name>` file holding `content`. The digest is a valid 32-char base32
/// string so root registration and GC digest extraction work on it.
fn fake_store_pkg(shade_root: &Path, digest_char: char, name: &str, content: &str) -> PackageEntry {
    let digest: String = std::iter::repeat(digest_char).take(32).collect();
    let store_name = format!("{digest}-{name}-1.0");
    let out = shade_root.join("store").join(&store_name);
    fs::create_dir_all(out.join("bin")).unwrap();
    fs::write(out.join("bin").join(name), content).unwrap();
    PackageEntry {
        name: name.to_string(),
        version: "1.0".to_string(),
        store_path: path_str(&out).to_string(),
        requested: true,
    }
}

fn read_current<F: StoreFs>(line: &GenLine<F>) -> u64 {
    line.current().unwrap().expect("current symlink present")
}

/// The line root as a host `Path` (host-backend tests dereference the real
/// symlinks with `std::fs`).
fn root(line: &GenLine<HostFs>) -> &Path {
    Path::new(line.line_root())
}

// ---- GATE: two generations, switch, rollback, history --------------------------

#[test]
fn gate_switch_and_rollback_via_symlink_flip() {
    let tmp = TmpDir::new("gate");
    let shade = tmp.path();
    let line = GenLine::system(shade);

    let v1 = fake_store_pkg(shade, 'a', "tool", "v1");
    let v2 = fake_store_pkg(shade, 'b', "tool", "v2");

    let g1 = line.create(&[v1.clone()], None, "install tool v1", 0).unwrap();
    let g2 = line.create(&[v2.clone()], None, "upgrade tool", g1).unwrap();
    assert_eq!((g1, g2), (1, 2), "monotonic counter from 1");

    // Nothing is active until the flip (create ≠ activate, 02 §6.1).
    assert_eq!(line.current().unwrap(), None);

    line.activate(g1).unwrap();
    assert_eq!(read_current(&line), 1);
    // The live path goes through `current`: reading through the flip point
    // sees generation 1's package.
    let through_current = root(&line).join("current/profile/bin/tool");
    assert_eq!(fs::read_to_string(&through_current).unwrap(), "v1");

    // Switch = one symlink flip; same path now resolves to v2.
    line.activate(g2).unwrap();
    assert_eq!(read_current(&line), 2);
    assert_eq!(fs::read_to_string(&through_current).unwrap(), "v2");
    // The flip staging link never survives.
    assert!(!root(&line).join(".current.new").exists());

    // Rollback appends generation 3 copying 1's manifest, then flips.
    let g3 = line.rollback(None).unwrap();
    assert_eq!(g3, 3);
    assert_eq!(read_current(&line), 3);
    assert_eq!(fs::read_to_string(&through_current).unwrap(), "v1");
    let m3 = line.read_manifest(3).unwrap().unwrap();
    assert_eq!(m3.packages, vec![v1.clone()]);
    assert_eq!(m3.parent, 1, "derived from the rollback target");

    // Rollback twice returns to where you started (07 §`shade rollback`) —
    // as a NEW generation, history append-only.
    let g4 = line.rollback(None).unwrap();
    assert_eq!(g4, 4);
    assert_eq!(fs::read_to_string(&through_current).unwrap(), "v2");

    // `list` shows the full linear history with `current` marked.
    let infos = line.list().unwrap();
    assert_eq!(infos.iter().map(|g| g.number).collect::<Vec<_>>(), vec![1, 2, 3, 4]);
    assert_eq!(
        infos.iter().filter(|g| g.current).map(|g| g.number).collect::<Vec<_>>(),
        vec![4]
    );
    assert_eq!(infos[0].manifest.reason, "install tool v1");
    assert!(infos[2].manifest.reason.contains("rollback to 1"));
}

#[test]
fn activation_refuses_missing_or_incomplete_generations() {
    let tmp = TmpDir::new("refuse");
    let line = GenLine::system(tmp.path());
    assert!(matches!(line.activate(7), Err(GenError::NoSuchGeneration(7))));

    // A numbered dir without manifest/profile must never be activated
    // (02 §6.1 step 1: built completely first).
    fs::create_dir_all(root(&line).join("9")).unwrap();
    assert!(matches!(line.activate(9), Err(GenError::Incomplete(9))));
}

#[test]
fn profile_collision_is_an_error_and_leaves_the_line_untouched() {
    let tmp = TmpDir::new("collision");
    let shade = tmp.path();
    let line = GenLine::system(shade);
    let a = fake_store_pkg(shade, 'c', "clash", "from-a");
    let mut b = fake_store_pkg(shade, 'd', "other", "from-b");
    // Make b also provide bin/clash.
    fs::write(Path::new(&b.store_path).join("bin/clash"), "b-clash").unwrap();
    b.requested = true;

    let err = line.create(&[a, b], None, "collide", 0).unwrap_err();
    match err {
        GenError::Collision { rel, package, .. } => {
            assert_eq!(rel, "bin/clash");
            assert_eq!(package, "other");
        }
        other => panic!("expected Collision, got {other:?}"),
    }
    // No generation appeared; no temp tree left behind.
    assert_eq!(line.numbers().unwrap(), Vec::<u64>::new());
    let leftovers: Vec<_> = fs::read_dir(root(&line))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(leftovers.is_empty(), "leftovers: {leftovers:?}");
}

#[test]
fn user_line_is_independent_of_system_line() {
    let tmp = TmpDir::new("user-line");
    let shade = tmp.path();
    let sys = GenLine::system(shade);
    let usr = GenLine::user(shade, "lyon");

    let p = fake_store_pkg(shade, 'f', "systool", "s");
    let q = fake_store_pkg(shade, 'g', "usertool", "u");
    let s1 = sys.create(&[p], None, "sys", 0).unwrap();
    sys.activate(s1).unwrap();

    // Each line has its own counter and its own current.
    let u1 = usr.create(&[q], None, "usr", 0).unwrap();
    assert_eq!(u1, 1, "independent monotonic counter");
    usr.activate(u1).unwrap();
    assert_eq!(read_current(&sys), s1);
    assert_eq!(read_current(&usr), u1);

    // Flipping the user line does not move the system line.
    let q2 = fake_store_pkg(shade, 'h', "usertool", "u2");
    let u2 = usr.create(&[q2], None, "usr2", u1).unwrap();
    usr.activate(u2).unwrap();
    assert_eq!(read_current(&sys), s1);
    assert_eq!(read_current(&usr), u2);
}

// ---- Roots: every generation is a GC root ---------------------------------------

#[test]
fn generations_register_as_gc_roots_and_survive_gc() {
    let tmp = TmpDir::new("gc-roots");
    let shade = tmp.path();
    let line = GenLine::system(shade);
    let db = StoreDb::new(shade);

    let live = fake_store_pkg(shade, 'i', "kept", "x");
    let dead = fake_store_pkg(shade, 'j', "doomed", "y");
    let g = line.create(&[live.clone()], None, "root me", 0).unwrap();
    line.activate(g).unwrap();

    // The roots API seam: the generation's packages appear under /shade/roots.
    let roots = db.list_roots().unwrap();
    assert!(
        roots.iter().any(|(name, target)| name == "gen-system-1-0" && *target == live.store_path),
        "generation root missing from roots API: {roots:?}"
    );

    // GC keeps the generation's store path, collects the unrooted one.
    let report = db.gc(&GcOptions::default()).unwrap();
    assert!(Path::new(&live.store_path).exists(), "rooted generation package collected");
    assert!(!Path::new(&dead.store_path).exists(), "unrooted path survived");
    assert_eq!(report.kept, 1);
    let _ = g;
}

// ---- The seam gate: same engine, injected MemFs backend ---------------------------

/// A shared handle to one [`MemFs`] so the line, the roots db, and the test's
/// own assertions see the same in-memory tree (the shade-build B3 pattern).
#[derive(Clone)]
struct SharedMem(std::rc::Rc<std::cell::RefCell<MemFs>>);

impl SharedMem {
    fn new() -> Self {
        SharedMem(std::rc::Rc::new(std::cell::RefCell::new(MemFs::new())))
    }
}

impl StoreFs for SharedMem {
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
        self.0.borrow_mut().unique_token()
    }
}

/// [`fake_store_pkg`] staged through the seam instead of `std::fs`.
fn seam_store_pkg(fs: &SharedMem, digest_char: char, name: &str, content: &str) -> PackageEntry {
    let digest: String = std::iter::repeat(digest_char).take(32).collect();
    let store_path = format!("/shade/store/{digest}-{name}-1.0");
    let mut f = fs.clone();
    backend::create_dir_all(&mut f, &format!("{store_path}/bin")).unwrap();
    f.write_file(&format!("{store_path}/bin/{name}"), content.as_bytes(), false).unwrap();
    PackageEntry {
        name: name.to_string(),
        version: "1.0".to_string(),
        store_path,
        requested: true,
    }
}

#[test]
fn seam_switch_rollback_list_and_roots_on_memfs() {
    let mem = SharedMem::new();
    let line = GenLine::system_on(mem.clone(), "/shade");

    let v1 = seam_store_pkg(&mem, 'a', "tool", "v1");
    let v2 = seam_store_pkg(&mem, 'b', "tool", "v2");

    // Two generations switch via the symlink flip — pure seam operations.
    let g1 = line.create(&[v1.clone()], None, "install tool v1", 0).unwrap();
    let g2 = line.create(&[v2.clone()], None, "upgrade tool", g1).unwrap();
    assert_eq!((g1, g2), (1, 2));
    assert_eq!(line.current().unwrap(), None);

    line.activate(g1).unwrap();
    assert_eq!(read_current(&line), 1);
    // The profile forest is symlinks into the store, written via the seam.
    assert_eq!(
        mem.clone().read_link("/shade/gen/system/1/profile/bin/tool").unwrap(),
        v1.store_path.clone() + "/bin/tool"
    );

    line.activate(g2).unwrap();
    assert_eq!(read_current(&line), 2);
    assert!(!mem.clone().exists("/shade/gen/system/.current.new"));

    // Rollback appends generation 3 copying 1's manifest, then flips.
    let g3 = line.rollback(None).unwrap();
    assert_eq!(g3, 3);
    assert_eq!(read_current(&line), 3);
    let m3 = line.read_manifest(3).unwrap().unwrap();
    assert_eq!(m3.packages, vec![v1.clone()]);
    assert_eq!(m3.parent, 1);

    // History with `current` marked.
    let infos = line.list().unwrap();
    assert_eq!(infos.iter().map(|g| g.number).collect::<Vec<_>>(), vec![1, 2, 3]);
    assert_eq!(
        infos.iter().filter(|g| g.current).map(|g| g.number).collect::<Vec<_>>(),
        vec![3]
    );

    // Generations registered as roots through the same backend.
    let db = StoreDb::with_backend(mem.clone(), "/shade");
    let roots = db.list_roots().unwrap();
    assert!(
        roots.iter().any(|(name, target)| name == "gen-system-1-0" && *target == v1.store_path),
        "generation root missing on the seam backend: {roots:?}"
    );

    // Live view wiring is a seam symlink too.
    line.wire_view("/lth/bin").unwrap();
    assert_eq!(
        mem.clone().read_link("/lth/bin").unwrap(),
        "/shade/gen/system/current/profile/bin"
    );
}

#[test]
fn seam_boot_and_pointer_on_memfs() {
    let mem = SharedMem::new();
    let line = GenLine::system_on(mem.clone(), "/shade");
    let p1 = seam_store_pkg(&mem, 'k', "one", "1");
    let p2 = seam_store_pkg(&mem, 'l', "two", "2");
    line.create(&[p1], None, "g1", 0).unwrap();
    line.create(&[p2], None, "g2", 1).unwrap();

    // Pointer round-trip through the seam (atomic temp+rename write).
    let mut f = mem.clone();
    write_pointer_on(
        &mut f,
        "/cfg/shade",
        &Pointer { prism: "/user/lyon/.prism".into(), selector: "ws".into(), generation: 1 },
    )
    .unwrap();
    assert_eq!(
        read_pointer_on(&mut f, "/cfg/shade").unwrap().unwrap().generation,
        1
    );

    // Boot activates the pinned pre-built generation; idempotent re-run.
    let out = boot_activate_on(mem.clone(), "/shade", "/cfg/shade", Some("/lth/bin")).unwrap();
    assert_eq!(out, BootOutcome { generation: 1, pinned: Some(1), fell_back: false });
    assert_eq!(read_current(&line), 1);
    let again = boot_activate_on(mem.clone(), "/shade", "/cfg/shade", Some("/lth/bin")).unwrap();
    assert_eq!(again.generation, 1);

    // Re-pin line 3 only; boot follows.
    repin_generation_on(&mut f, "/cfg/shade", 2).unwrap();
    let ptr = read_pointer_on(&mut f, "/cfg/shade").unwrap().unwrap();
    assert_eq!((ptr.prism.as_str(), ptr.selector.as_str(), ptr.generation), ("/user/lyon/.prism", "ws", 2));
    let out = boot_activate_on(mem.clone(), "/shade", "/cfg/shade", None).unwrap();
    assert_eq!(out, BootOutcome { generation: 2, pinned: Some(2), fell_back: false });
}

// ---- Prism build → system generation → activation --------------------------------

const PRISM: &str = r#"{
  packages = {
    alpha = derivation {
      name = "alpha";
      version = "1.0";
      system = "x86_64-oros";
      phases = [ "mkdir -p $out/bin" "printf a > $out/bin/alpha" ];
      outputs = { bin = [ "alpha" ]; };
    };
    beta = derivation {
      name = "beta";
      version = "2.0";
      system = "x86_64-oros";
      phases = [ "mkdir -p $out/bin" "printf b > $out/bin/beta" ];
      outputs = { bin = [ "beta" ]; };
    };
  };
}"#;

struct Sys {
    shade: PathBuf,
    cfg: PathBuf,
    build: PathBuf,
    log: PathBuf,
}

fn sys_dirs(tmp: &TmpDir) -> Sys {
    let s = Sys {
        shade: tmp.path().join("shade"),
        cfg: tmp.path().join("cfg/shade"),
        build: tmp.path().join("build"),
        log: tmp.path().join("log"),
    };
    fs::create_dir_all(&s.cfg).unwrap();
    s
}

#[test]
fn system_generation_from_prism_shade_builds_and_activates() {
    with_stack(|| {
        let tmp = TmpDir::new("os-rebuild");
        let s = sys_dirs(&tmp);
        // The bootstrap default at /cfg/shade/prism.shade (10 §3).
        fs::write(s.cfg.join("prism.shade"), PRISM).unwrap();

        let store = s.shade.join("store");
        let roots = BuildRoots { store: &store, build: &s.build, log: &s.log };
        let lth_bin = tmp.path().join("lth/bin");

        // No argument, no pointer: builds the live bootstrap default (10 §4).
        let out = os_rebuild(
            &s.shade, &s.cfg, None, &roots, Some("tc-test"), 1, Some(&lth_bin), &HostIo,
        )
        .unwrap();
        assert_eq!(out.generation, 1);
        assert_eq!(out.packages, 2);

        // Activated: current -> 1, and the live view resolves through it.
        let line = GenLine::system(&s.shade);
        assert_eq!(read_current(&line), 1);
        assert_eq!(fs::read_to_string(lth_bin.join("alpha")).unwrap(), "a");
        assert_eq!(fs::read_to_string(lth_bin.join("beta")).unwrap(), "b");

        // Profile entries are symlinks into the store (a symlink forest, not
        // copies) — the byte-scan root path for GC.
        let alpha_link = root(&line).join("1/profile/bin/alpha");
        let target = fs::read_link(&alpha_link).unwrap();
        assert!(target.starts_with(&store), "forest must point into the store: {target:?}");

        // Pointer written: source, selector (empty), pinned generation (10 §2).
        let ptr = read_pointer(&s.cfg).unwrap().unwrap();
        assert_eq!(ptr.generation, 1);
        assert_eq!(ptr.selector, "");
        // Building the default itself does not retire it (10 §3 retires it
        // only when an explicit prism supersedes it).
        assert!(s.cfg.join("prism.shade").exists());

        // Second rebuild: pure store hits, new generation, pointer moves.
        let out2 = os_rebuild(
            &s.shade, &s.cfg, None, &roots, Some("tc-test"), 1, Some(&lth_bin), &HostIo,
        )
        .unwrap();
        assert_eq!(out2.generation, 2);
        assert_eq!(read_pointer(&s.cfg).unwrap().unwrap().generation, 2);
    });
}

#[test]
fn explicit_rebuild_retires_bootstrap_default_and_selector_selects() {
    with_stack(|| {
        let tmp = TmpDir::new("retire");
        let s = sys_dirs(&tmp);
        fs::write(s.cfg.join("prism.shade"), PRISM).unwrap();
        // The user's own prism directory (10 §5 ~/.prism shape).
        let user_prism = tmp.path().join("home/.prism");
        fs::create_dir_all(&user_prism).unwrap();
        fs::write(user_prism.join("prism.shade"), PRISM).unwrap();

        let store = s.shade.join("store");
        let roots = BuildRoots { store: &store, build: &s.build, log: &s.log };

        let arg = format!("{}#alpha", user_prism.display());
        let out =
            os_rebuild(&s.shade, &s.cfg, Some(&arg), &roots, Some("tc-test"), 1, None, &HostIo)
                .unwrap();
        assert_eq!(out.packages, 1, "#alpha selects one output");
        assert_eq!(out.selector, "alpha");

        // First explicit rebuild retires the default: prism.shade -> .bak (10 §3).
        assert!(!s.cfg.join("prism.shade").exists());
        assert!(s.cfg.join("prism.shade.bak").exists());

        // Pointer names the user prism; rebuild-without-arg now follows it.
        let ptr = read_pointer(&s.cfg).unwrap().unwrap();
        assert_eq!(ptr.prism, user_prism.display().to_string());
        assert_eq!(ptr.selector, "alpha");

        // Only alpha is in the profile.
        let line = GenLine::system(&s.shade);
        let profile_bin = root(&line).join("1/profile/bin");
        assert!(profile_bin.join("alpha").exists());
        assert!(!profile_bin.join("beta").exists());
    });
}

#[test]
fn pointer_target_unresolvable_fails_loud_never_bak() {
    with_stack(|| {
        let tmp = TmpDir::new("fail-loud");
        let s = sys_dirs(&tmp);
        // A retired default exists AND a pointer names a missing prism.
        fs::write(s.cfg.join("prism.shade.bak"), PRISM).unwrap();
        write_pointer(
            &s.cfg,
            &Pointer { prism: "/nonexistent/prism".into(), selector: String::new(), generation: 1 },
        )
        .unwrap();

        let store = s.shade.join("store");
        let roots = BuildRoots { store: &store, build: &s.build, log: &s.log };
        let err = os_rebuild(&s.shade, &s.cfg, None, &roots, Some("tc-test"), 1, None, &HostIo)
            .unwrap_err();
        assert!(matches!(err, GenError::UnresolvablePointer(_)), "got {err:?}");
        // Nothing was built or activated (10 §4: changes no generation).
        assert_eq!(GenLine::system(&s.shade).numbers().unwrap(), Vec::<u64>::new());
    });
}

#[test]
fn home_rebuild_flips_only_the_user_line() {
    with_stack(|| {
        let tmp = TmpDir::new("home");
        let s = sys_dirs(&tmp);
        let prism = tmp.path().join("prism");
        fs::create_dir_all(&prism).unwrap();
        fs::write(prism.join("prism.shade"), PRISM).unwrap();

        let store = s.shade.join("store");
        let roots = BuildRoots { store: &store, build: &s.build, log: &s.log };
        let arg = format!("{}#beta", prism.display());
        let (n, pkgs) =
            home_rebuild(&s.shade, "lyon", &arg, &roots, Some("tc-test"), 1, &HostIo).unwrap();
        assert_eq!((n, pkgs), (1, 1));

        let usr = GenLine::user(&s.shade, "lyon");
        assert_eq!(read_current(&usr), 1);
        assert!(root(&usr).join("current/profile/bin/beta").exists());
        // The system line is untouched — no pointer, no system generation (10 §5).
        assert_eq!(GenLine::system(&s.shade).current().unwrap(), None);
        assert!(read_pointer(&s.cfg).unwrap().is_none());
    });
}

// ---- Boot: pre-built only, pinned, last-good fallback -----------------------------

#[test]
fn boot_activates_the_pinned_prebuilt_generation() {
    let tmp = TmpDir::new("boot");
    let s = sys_dirs(&tmp);
    let line = GenLine::system(&s.shade);
    let p1 = fake_store_pkg(&s.shade, 'k', "one", "1");
    let p2 = fake_store_pkg(&s.shade, 'l', "two", "2");
    line.create(&[p1], None, "g1", 0).unwrap();
    line.create(&[p2], None, "g2", 1).unwrap();
    write_pointer(
        &s.cfg,
        &Pointer { prism: "/user/lyon/.prism".into(), selector: "ws".into(), generation: 1 },
    )
    .unwrap();

    let lth_bin = tmp.path().join("lth/bin");
    let out = boot_activate(&s.shade, &s.cfg, Some(&lth_bin)).unwrap();
    assert_eq!(out, BootOutcome { generation: 1, pinned: Some(1), fell_back: false });
    assert_eq!(read_current(&line), 1);
    // Boot re-run is idempotent.
    let again = boot_activate(&s.shade, &s.cfg, Some(&lth_bin)).unwrap();
    assert_eq!(again.generation, 1);
    // The live view dereferences through current.
    assert_eq!(fs::read_to_string(lth_bin.join("one")).unwrap(), "1");
}

#[test]
fn repin_moves_only_the_generation_line_and_boot_follows() {
    let tmp = TmpDir::new("repin");
    let s = sys_dirs(&tmp);
    let line = GenLine::system(&s.shade);
    let p1 = fake_store_pkg(&s.shade, 'p', "one", "1");
    line.create(&[p1.clone()], None, "g1", 0).unwrap();
    line.create(&[p1], None, "g2", 1).unwrap();
    write_pointer(
        &s.cfg,
        &Pointer { prism: "/user/lyon/.prism".into(), selector: "ws".into(), generation: 2 },
    )
    .unwrap();
    line.activate(2).unwrap();

    // Rollback + re-pin: boot now follows the rolled-back-to generation.
    let n = line.rollback(None).unwrap();
    repin_generation(&s.cfg, n).unwrap();
    let ptr = read_pointer(&s.cfg).unwrap().unwrap();
    assert_eq!((ptr.prism.as_str(), ptr.selector.as_str(), ptr.generation), ("/user/lyon/.prism", "ws", n));
    let out = boot_activate(&s.shade, &s.cfg, None).unwrap();
    assert_eq!(out, BootOutcome { generation: n, pinned: Some(n), fell_back: false });

    // Without a pointer, re-pin is a no-op (a user line has none).
    fs::remove_file(s.cfg.join(POINTER_FILE)).unwrap();
    repin_generation(&s.cfg, 1).unwrap();
    assert!(read_pointer(&s.cfg).unwrap().is_none());
}

#[test]
fn boot_falls_back_to_last_good_when_pinned_is_missing() {
    let tmp = TmpDir::new("boot-fallback");
    let s = sys_dirs(&tmp);
    let line = GenLine::system(&s.shade);
    let p1 = fake_store_pkg(&s.shade, 'm', "one", "1");
    let p2 = fake_store_pkg(&s.shade, 'n', "two", "2");
    line.create(&[p1], None, "g1", 0).unwrap();
    let g2 = line.create(&[p2], None, "g2", 1).unwrap();
    // Pointer pins a generation that does not exist (corruption / lost dir).
    write_pointer(
        &s.cfg,
        &Pointer { prism: "x".into(), selector: String::new(), generation: 99 },
    )
    .unwrap();

    let out = boot_activate(&s.shade, &s.cfg, None).unwrap();
    assert_eq!(out, BootOutcome { generation: g2, pinned: Some(99), fell_back: true });
    assert_eq!(read_current(&line), g2);
}

#[test]
fn boot_falls_back_when_pinned_closure_is_incomplete() {
    // Cold-boot integrity (10 §6): the pinned generation's tree is structurally
    // intact (manifest + profile present) but a referenced store path no longer
    // exists — exactly the state a persistent store can land in (store loss /
    // partial persist). Boot must NOT activate it (no-build-at-boot cannot
    // repair it) and must fall back to the newest generation with a COMPLETE
    // closure.
    let tmp = TmpDir::new("boot-closure");
    let s = sys_dirs(&tmp);
    let line = GenLine::system(&s.shade);
    let p1 = fake_store_pkg(&s.shade, 'm', "one", "1");
    let p2 = fake_store_pkg(&s.shade, 'n', "two", "2");
    let g1 = line.create(&[p1], None, "g1", 0).unwrap();
    let g2 = line.create(&[p2.clone()], None, "g2", 1).unwrap();
    // Pin the newest generation, then destroy its store closure on disk.
    write_pointer(
        &s.cfg,
        &Pointer { prism: "x".into(), selector: String::new(), generation: g2 },
    )
    .unwrap();
    fs::remove_dir_all(&p2.store_path).unwrap();
    assert!(!line.closure_complete(g2), "g2 closure must read as incomplete");
    assert!(line.closure_complete(g1), "g1 closure is intact");

    let out = boot_activate(&s.shade, &s.cfg, None).unwrap();
    assert_eq!(out, BootOutcome { generation: g1, pinned: Some(g2), fell_back: true });
    assert_eq!(read_current(&line), g1);
}

#[test]
fn boot_errors_when_no_generation_has_a_complete_closure() {
    // Every generation's closure is gone: boot fails loud (never builds).
    let tmp = TmpDir::new("boot-noclosure");
    let s = sys_dirs(&tmp);
    let line = GenLine::system(&s.shade);
    let p1 = fake_store_pkg(&s.shade, 'm', "one", "1");
    let g1 = line.create(&[p1.clone()], None, "g1", 0).unwrap();
    write_pointer(
        &s.cfg,
        &Pointer { prism: "x".into(), selector: String::new(), generation: g1 },
    )
    .unwrap();
    fs::remove_dir_all(&p1.store_path).unwrap();
    let err = boot_activate(&s.shade, &s.cfg, None).unwrap_err();
    assert!(matches!(err, GenError::NoGeneration), "got {err:?}");
}

#[test]
fn boot_with_nothing_built_errors_and_never_builds() {
    let tmp = TmpDir::new("boot-empty");
    let s = sys_dirs(&tmp);
    // Even with a source prism sitting right there, boot must not build it:
    // it has no builder to call — it can only fail.
    fs::write(s.cfg.join("prism.shade"), PRISM).unwrap();
    let err = boot_activate(&s.shade, &s.cfg, None).unwrap_err();
    assert!(matches!(err, GenError::NoGeneration), "got {err:?}");
    assert!(!s.shade.join("store").exists(), "boot must not create store state");
}

// ---- Manifest + time --------------------------------------------------------------

#[test]
fn manifest_round_trips_byte_stably() {
    // Enough packages to cross into two-digit indices — the bytewise key
    // order (`package.10.*` before `package.2.*`) must survive the
    // parse → re-serialize cycle byte-for-byte.
    let packages: Vec<PackageEntry> = (0..12)
        .map(|i| PackageEntry {
            name: format!("pkg{i}"),
            version: format!("{i}.0"),
            store_path: format!(
                "/shade/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-pkg{i}-{i}.0"
            ),
            requested: i % 2 == 0,
        })
        .collect();
    let m = Manifest {
        created: 1_783_814_400,
        parent: 3,
        reason: "install with = sign and \"quotes\"".into(),
        packages,
    };

    let bytes = m.serialize();
    // Canonical record shape: header first, key=value lines, trailing LF.
    assert!(bytes.starts_with("shade-gen=1\n"));
    assert!(bytes.ends_with('\n'));
    assert!(!bytes.contains("toml"), "no TOML anywhere in the record");

    // Value round-trip.
    let parsed = Manifest::parse(&bytes).unwrap();
    assert_eq!(parsed, m);
    // Byte stability: re-serializing the parsed manifest reproduces the
    // input exactly (the CDF / db-record byte-identity discipline).
    assert_eq!(parsed.serialize(), bytes);

    // Embedded newlines cannot corrupt the line-based record.
    let sneaky = Manifest { reason: "line1\nline2".into(), ..m };
    let bytes = sneaky.serialize();
    let reparsed = Manifest::parse(&bytes).unwrap();
    assert_eq!(reparsed.reason, "line1 line2");
    assert_eq!(reparsed.serialize(), bytes);
}

#[test]
fn generation_writes_canonical_manifest_and_no_toml() {
    let tmp = TmpDir::new("no-toml");
    let shade = tmp.path();
    let line = GenLine::system(shade);
    let p = fake_store_pkg(shade, 'r', "tool", "x");
    let n = line.create(&[p.clone()], None, "canonical", 0).unwrap();

    // The manifest is the extensionless canonical record; nothing under the
    // freshly created line is TOML.
    let gen_dir = root(&line).join(n.to_string());
    assert!(gen_dir.join("manifest").exists());
    fn assert_no_toml(dir: &Path) {
        for e in fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let name = e.file_name().to_string_lossy().into_owned();
            assert!(!name.ends_with(".toml"), "TOML file written: {name}");
            if e.file_type().unwrap().is_dir() {
                assert_no_toml(&e.path());
            }
        }
    }
    assert_no_toml(root(&line));

    // On-disk bytes are the canonical record and round-trip byte-stably.
    let bytes = fs::read_to_string(gen_dir.join("manifest")).unwrap();
    assert!(bytes.starts_with("shade-gen=1\n"));
    let parsed = Manifest::parse(&bytes).unwrap();
    assert_eq!(parsed.packages, vec![p]);
    assert_eq!(parsed.serialize(), bytes);
}

#[test]
fn rfc3339_formatting_is_sane() {
    assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
    // 2026-07-12 00:00:00 UTC (cross-checked against date(1)).
    assert_eq!(rfc3339_utc(1_783_814_400), "2026-07-12T00:00:00Z");
    // Leap-day and end-of-year edges.
    assert_eq!(rfc3339_utc(1_709_164_799), "2024-02-28T23:59:59Z");
    assert_eq!(rfc3339_utc(1_709_164_800), "2024-02-29T00:00:00Z");
    assert_eq!(rfc3339_utc(1_735_689_599), "2024-12-31T23:59:59Z");
}
