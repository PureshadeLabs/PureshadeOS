//! Prism → package set → generation: evaluate a prism's entry file
//! (`prism.shade`, the only spelling — 10 intro), select outputs
//! (shade 08 §4), build every selected package through the shade-build
//! executor, and hand back manifest entries for [`GenLine::create`].
//!
//! Rebuild-time only: [`boot_activate`](crate::boot_activate) never reaches
//! this module (10 §6 — boot consumes built generations, not source prisms).
//!
//! Prism shape accepted by this seed: the evaluated entry file is either a
//! bare derivation (a singleton set), or an attrset whose `packages` attr —
//! or the attrset itself — maps names to derivations (the 04 §1
//! `outputs.packages` shape, minus input resolution, which is deferred with
//! the fetcher). Selector `#a.b.c` navigates that set; no selector means the
//! `default` member if present, else every member (shade 08 §4 leaning).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use shade_build::{
    plan_value, BuildError, DbRegistrar, Executor, LocalStore, PermissiveSandbox, Resolver,
};
use shadec::error::Pos;
use shadec::eval::Evaluator;
use shadec::io::EvalIo;
use shadec::value::Value;

use crate::{read_pointer, write_pointer, GenError, GenLine, PackageEntry, Pointer};

/// Where builds land — the executor's three roots (canonical `/shade/…` in
/// production, temp dirs in tests).
#[derive(Debug, Clone, Copy)]
pub struct BuildRoots<'a> {
    pub store: &'a Path,
    pub build: &'a Path,
    pub log: &'a Path,
}

/// Resolve a prism reference to its entry file: a directory means
/// `<dir>/prism.shade` (10 intro); a file is taken as-is. Relative
/// references resolve against the working directory — the evaluator's
/// `import` requires an absolute path.
fn entry_file(prism: &Path) -> PathBuf {
    let abs = if prism.is_absolute() {
        prism.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|c| c.join(prism))
            .unwrap_or_else(|_| prism.to_path_buf())
    };
    if abs.is_dir() {
        abs.join("prism.shade")
    } else {
        abs
    }
}

/// Evaluate `prism` (entry file or directory), select `selector`'s packages,
/// and build each one LOOKUP-THEN-BUILD through the executor (permissive
/// sandbox, db registrar — the same wiring as the host `shade-build` binary).
/// Returns one manifest entry per selected package, `requested = true`.
///
/// The evaluator recurses on deep recipes; run this on a generously sized
/// stack (the host CLI uses a large worker thread, as `shade-build` does).
pub fn build_prism_packages(
    prism: &Path,
    selector: Option<&str>,
    roots: &BuildRoots,
    toolchain: Option<&str>,
    jobs: u32,
    io: &dyn EvalIo,
) -> Result<Vec<PackageEntry>, GenError> {
    let entry = entry_file(prism);
    let mut ev = Evaluator::new(io);
    ev.toolchain = toolchain.map(str::to_string);
    let pos = Pos { file: Arc::from("<shade-gen>"), line: 0, col: 0 };

    let abs = shadec::parser::normalize_path(&entry.to_string_lossy());
    let value = ev.import(&abs, &pos)?;
    let selected = select_packages(&mut ev, &value, selector, &pos)?;

    let local = LocalStore;
    let resolvers: [&dyn Resolver; 1] = [&local];
    let sandbox = PermissiveSandbox;
    let registrar = DbRegistrar::for_store_root(roots.store);
    let mut exec = Executor::new(roots.store, roots.build, roots.log, &resolvers, &sandbox, &registrar);
    exec.jobs = jobs;

    let mut packages = Vec::with_capacity(selected.len());
    for v in &selected {
        let graph = plan_value(&mut ev, v, &roots.store.to_string_lossy())?;
        let outcome = exec.run_graph(&graph)?;
        packages.push(PackageEntry {
            name: graph.root.name,
            version: graph.root.version,
            store_path: crate::path_str(outcome.root_result().out_path()).to_string(),
            requested: true,
        });
    }
    Ok(packages)
}

/// Apply shade 08 §4 selection to an evaluated prism value. Returns the
/// selected derivation values (the caller plans/builds each).
fn select_packages(
    ev: &mut Evaluator,
    value: &Value,
    selector: Option<&str>,
    pos: &Pos,
) -> Result<Vec<Value>, GenError> {
    let Value::Attrs(top) = value else {
        return Err(GenError::NotAPackageSet(format!(
            "prism evaluated to {}, expected a derivation or a package set",
            value.type_of()
        )));
    };

    // A bare derivation is a singleton package set.
    if ev.attrs_is_derivation(top, pos)? {
        return Ok(vec![value.clone()]);
    }

    // The package set: `packages` if declared (the 04 §1 outputs shape),
    // else the attrset itself.
    let base = match top.get("packages") {
        Some(t) => ev.force(t, pos)?,
        None => value.clone(),
    };

    match selector {
        Some(sel) => {
            // `#a.b.c` navigates nested sets (02-grammar §6); the leaf must
            // be a derivation.
            let mut cur = base;
            for comp in sel.split('.') {
                let Value::Attrs(m) = &cur else {
                    return Err(GenError::NotAPackageSet(format!(
                        "selector `{sel}`: `{comp}` selects into a {}, expected a set",
                        cur.type_of()
                    )));
                };
                let Some(t) = m.get(comp) else {
                    return Err(GenError::NotAPackageSet(format!(
                        "selector `{sel}`: no output `{comp}`"
                    )));
                };
                cur = ev.force(t, pos)?;
            }
            expect_derivation(ev, &cur, pos, &format!("selector `{sel}`"))?;
            Ok(vec![cur])
        }
        None => {
            let Value::Attrs(set) = &base else {
                return Err(GenError::NotAPackageSet(format!(
                    "packages evaluated to {}, expected a set of derivations",
                    base.type_of()
                )));
            };
            // `default` if present, else every member (shade 08 §4 leaning).
            if let Some(t) = set.get("default") {
                let v = ev.force(t, pos)?;
                expect_derivation(ev, &v, pos, "output `default`")?;
                return Ok(vec![v]);
            }
            let mut out = Vec::with_capacity(set.len());
            for (k, t) in set.iter() {
                let v = ev.force(t, pos)?;
                expect_derivation(ev, &v, pos, &format!("output `{k}`"))?;
                out.push(v);
            }
            if out.is_empty() {
                return Err(GenError::NotAPackageSet("empty package set".to_string()));
            }
            Ok(out)
        }
    }
}

fn expect_derivation(
    ev: &mut Evaluator,
    v: &Value,
    pos: &Pos,
    what: &str,
) -> Result<(), GenError> {
    if let Value::Attrs(m) = v {
        if ev.attrs_is_derivation(m, pos)? {
            return Ok(());
        }
    }
    Err(GenError::NotAPackageSet(format!(
        "{what} is not a derivation (got {})",
        v.type_of()
    )))
}

// ---- Rebuild drivers (07 §2.1 / §2.2) ---------------------------------------------

/// Split a `<path>[#<selector>]` prism reference (07 §1).
fn split_ref(s: &str) -> (&str, Option<&str>) {
    match s.split_once('#') {
        Some((p, sel)) if !sel.is_empty() => (p, Some(sel)),
        Some((p, _)) => (p, None),
        None => (s, None),
    }
}

/// What [`os_rebuild`] did.
#[derive(Debug, Clone)]
pub struct OsRebuildOutcome {
    pub generation: u64,
    pub packages: usize,
    /// The prism path and selector recorded in the pointer.
    pub prism: String,
    pub selector: String,
}

/// `shade os rebuild` (07 §2.1): resolve the system prism source (10 §4),
/// build its package set, create + activate a new system generation, retire
/// the bootstrap default on first explicit rebuild (10 §3), and rewrite the
/// pointer with the pinned generation number (10 §2). `lth_bin`, when given,
/// wires the live view symlink after the flip.
pub fn os_rebuild(
    shade_root: &Path,
    cfg_root: &Path,
    prism_arg: Option<&str>,
    roots: &BuildRoots,
    toolchain: Option<&str>,
    jobs: u32,
    lth_bin: Option<&Path>,
    io: &dyn EvalIo,
) -> Result<OsRebuildOutcome, GenError> {
    // Source resolution (10 §4): an explicit argument wins; else the pointer
    // is authoritative; else `.bak`, else the live bootstrap default.
    let (prism_path, selector, explicit) = match prism_arg {
        Some(s) => {
            let (p, sel) = split_ref(s);
            (p.to_string(), sel.map(str::to_string), true)
        }
        None => match read_pointer(cfg_root)? {
            Some(ptr) => {
                let sel = (!ptr.selector.is_empty()).then(|| ptr.selector.clone());
                (ptr.prism, sel, false)
            }
            None => {
                let bak = cfg_root.join("prism.shade.bak");
                let live = cfg_root.join("prism.shade");
                if bak.exists() {
                    (bak.to_string_lossy().into_owned(), None, false)
                } else if live.exists() {
                    (live.to_string_lossy().into_owned(), None, false)
                } else {
                    return Err(GenError::NoSystemPrism);
                }
            }
        },
    };

    let prism = PathBuf::from(&prism_path);
    let entry = entry_file(&prism);
    if !entry.exists() {
        // Fail loud. For a pointer-named source this is specifically NOT a
        // `.bak` fallback (10 §4); for an explicit argument it is a plain
        // missing-file error.
        return Err(if explicit {
            GenError::Io(io::Error::new(
                io::ErrorKind::NotFound,
                format!("{} does not exist", entry.display()),
            ))
        } else {
            GenError::UnresolvablePointer(format!("{} does not exist", entry.display()))
        });
    }

    let packages = build_prism_packages(&prism, selector.as_deref(), roots, toolchain, jobs, io)?;

    let line = GenLine::system(shade_root);
    let parent = line.current()?.unwrap_or(0);
    let lock = fs::read(prism.join("prism.lock")).ok();
    let sel_str = selector.clone().unwrap_or_default();
    let reason = match &selector {
        Some(sel) => format!("os rebuild {prism_path}#{sel}"),
        None => format!("os rebuild {prism_path}"),
    };
    let generation = line.create(&packages, lock.as_deref(), &reason, parent)?;
    line.activate(generation)?;
    if let Some(link) = lth_bin {
        line.wire_view(crate::path_str(link))?;
    }

    // First-rebuild retirement (10 §3): an explicit prism supersedes the
    // bootstrap default — rename it to `.bak`, one-way per install.
    if explicit {
        let live = cfg_root.join("prism.shade");
        if live.exists() && live != entry {
            fs::rename(&live, cfg_root.join("prism.shade.bak"))?;
        }
    }

    // Pointer rewrite last: build → generation → pointer (10 §2), so a
    // failure anywhere above leaves the previous pointer (and its pinned,
    // still-live generation) intact.
    write_pointer(
        cfg_root,
        &Pointer { prism: prism_path.clone(), selector: sel_str.clone(), generation },
    )?;

    Ok(OsRebuildOutcome { generation, packages: packages.len(), prism: prism_path, selector: sel_str })
}

/// `shade home rebuild` (07 §2.2): build the user's prism and flip **only**
/// `/shade/gen/users/<user>/current`. No pointer, no `/lth/bin`, no system
/// line — a user rebuild is never folded into the system generation (10 §5).
/// Returns the new generation number and its package count.
pub fn home_rebuild(
    shade_root: &Path,
    user: &str,
    prism_ref: &str,
    roots: &BuildRoots,
    toolchain: Option<&str>,
    jobs: u32,
    io: &dyn EvalIo,
) -> Result<(u64, usize), GenError> {
    let (path, selector) = split_ref(prism_ref);
    let prism = PathBuf::from(path);
    let packages = build_prism_packages(&prism, selector, roots, toolchain, jobs, io)?;

    let line = GenLine::user(shade_root, user);
    let parent = line.current()?.unwrap_or(0);
    let lock = fs::read(prism.join("prism.lock")).ok();
    let generation = line.create(
        &packages,
        lock.as_deref(),
        &format!("home rebuild {prism_ref}"),
        parent,
    )?;
    line.activate(generation)?;
    Ok((generation, packages.len()))
}

impl From<shade_store::StoreError> for GenError {
    fn from(e: shade_store::StoreError) -> Self {
        GenError::Build(BuildError::Store(e))
    }
}
