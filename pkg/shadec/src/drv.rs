//! The `derivation` builtin and CDF emission per `docs/shade/05-derivation.md`,
//! path ingestion per `docs/shade/04-values.md` §4.2, and the fixed-output
//! fetch builtins (05 §5). All canonical bytes come from the shared
//! `shade-cdf` crate — nothing here writes CDF bytes directly.
//!
//! Realization boundary: shadec computes source-derivation *identities* and
//! CDF bytes; actually fetching bytes / writing the store is the store
//! services' job (08 §2), which do not exist yet — see TODO(open) notes.

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;

use shade_cdf::CdfBuilder;

use crate::builtins::{err, force_attrs, force_list, force_string, type_err};
use crate::error::{ErrorKind, Pos, Result};
use crate::eval::{CoerceMode, Evaluator};
use crate::io::FileKind;
use crate::value::{AttrsMap, ShStr, Thunk, ThunkRef, Value};

/// Sandbox-fixed environment variables (shade-pkg 06 §4): a recipe `env`
/// key colliding with these is an error (05 §2).
const SANDBOX_FIXED_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "TMPDIR",
    "SOURCE_DATE_EPOCH",
    "TZ",
    "LANG",
    "LC_ALL",
    "TARGET",
    "JOBS",
    "OUT",
];

fn is_sandbox_fixed(key: &str) -> bool {
    if SANDBOX_FIXED_ENV.contains(&key) {
        return true;
    }
    // SRC0, SRC1, … (the $src<i> family)
    key.strip_prefix("SRC").is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
}

fn valid_env_key(key: &str) -> bool {
    // [A-Z_][A-Z0-9_]* (05 §2)
    let mut bytes = key.bytes();
    match bytes.next() {
        Some(b) if b.is_ascii_uppercase() || b == b'_' => {}
        _ => return false,
    }
    bytes.all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

fn is_lower_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The closed argument schema (05 §2). Anything else is an eval error.
const SCHEMA: &[&str] = &[
    "name", "version", "system", "toolchain", "sandbox", "sources", "deps", "env", "phases",
    "outputs", "description", "license", "bootCritical",
];

// ---- derivation ------------------------------------------------------------

pub fn prim_derivation(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    let am = force_attrs(ev, &args[0], pos)?;
    for k in am.keys() {
        if !SCHEMA.contains(&k.as_str()) {
            let extra = if k == "unsafe" {
                " (`unsafe` is retired — shade synthesizes no recipe-less builds, shade-pkg 03 §7)"
            } else {
                " (the argument schema is closed: CDF's key set is closed, 05 §1)"
            };
            return Err(type_err(format!("derivation: unknown argument `{k}`{extra}"), pos));
        }
    }
    for req in ["name", "version", "system", "outputs"] {
        if !am.contains_key(req) {
            return Err(type_err(format!("derivation: missing required argument `{req}`"), pos));
        }
    }
    // name is forced eagerly: the value carries the normalized name (04 §6)
    let raw_name = force_string(ev, am.get("name").unwrap(), pos)?;
    let name = shade_cdf::normalize_name(&raw_name.s)
        .map_err(|e| type_err(format!("derivation: {e}"), pos))?;

    // drvPath/outPath computed lazily on first demand and memoized (04 §6);
    // the two thunks share one emission via this memo cell
    let memo: Rc<RefCell<Option<(String, String)>>> = Rc::new(RefCell::new(None));
    let spec_args = am.clone();
    let spec_pos = pos.clone();

    let drv_memo = memo.clone();
    let drv_args = spec_args.clone();
    let drv_pos = spec_pos.clone();
    let drv_path_thunk = Thunk::native(Box::new(move |ev: &mut Evaluator| {
        let (drv, _out) = ensure_emitted(ev, &drv_memo, &drv_args, &drv_pos)?;
        Ok(Value::Str(ShStr::plain(drv)))
    }));

    let out_memo = memo.clone();
    let out_args = spec_args.clone();
    let out_pos = spec_pos.clone();
    let out_path_thunk = Thunk::native(Box::new(move |ev: &mut Evaluator| {
        let (_drv, out) = ensure_emitted(ev, &out_memo, &out_args, &out_pos)?;
        let mut ctx = BTreeSet::new();
        ctx.insert(out.clone());
        // outPath carries a context referencing this derivation (04 §6)
        Ok(Value::Str(ShStr::with_ctx(out, Rc::new(ctx))))
    }));

    // the original argument attrs pass through (04 §6)
    let mut result = (*am).clone();
    result.insert("type".to_string(), Thunk::done(Value::Str(ShStr::plain("derivation"))));
    result.insert("name".to_string(), Thunk::done(Value::Str(ShStr::plain(name))));
    result.insert("outputName".to_string(), Thunk::done(Value::Str(ShStr::plain("out"))));
    result.insert("drvPath".to_string(), drv_path_thunk);
    result.insert("outPath".to_string(), out_path_thunk);
    Ok(Value::Attrs(Rc::new(result)))
}

fn ensure_emitted(
    ev: &mut Evaluator,
    memo: &Rc<RefCell<Option<(String, String)>>>,
    am: &Rc<AttrsMap>,
    pos: &Pos,
) -> Result<(String, String)> {
    if let Some(v) = memo.borrow().clone() {
        return Ok(v);
    }
    let v = emit_derivation(ev, am, pos)?;
    *memo.borrow_mut() = Some(v.clone());
    Ok(v)
}

/// A present-but-null optional attr is treated as absent (05 §2).
fn get_opt<'a>(ev: &mut Evaluator, am: &'a AttrsMap, key: &str, pos: &Pos) -> Result<Option<Value>> {
    match am.get(key) {
        None => Ok(None),
        Some(t) => match ev.force(&t.clone(), pos)? {
            Value::Null => Ok(None),
            v => Ok(Some(v)),
        },
    }
}

/// The total emission procedure (05 §3 steps 1-6). Step 7 (hand-off to the
/// store services) is recording into `ev.drvs`; the services that would
/// consume it are TODO(open) — they do not exist yet (shade-pkg store layer
/// unimplemented).
fn emit_derivation(ev: &mut Evaluator, am: &Rc<AttrsMap>, pos: &Pos) -> Result<(String, String)> {
    let mut deps: BTreeSet<String> = BTreeSet::new();
    let mut b = CdfBuilder::new();
    let cdf = |e: shade_cdf::CdfError, pos: &Pos| type_err(format!("derivation: {e}"), pos);

    // scalar string args; every string argument's context joins the dep set
    // (05 §2.2: implicit deps via string context)
    let raw_name = force_string(ev, am.get("name").unwrap(), pos)?;
    deps.extend(raw_name.ctx.iter().cloned());
    let name = shade_cdf::normalize_name(&raw_name.s).map_err(|e| cdf(e, pos))?;
    b.insert("name", &name).map_err(|e| cdf(e, pos))?;

    let version = force_string(ev, am.get("version").unwrap(), pos)?;
    deps.extend(version.ctx.iter().cloned());
    shade_cdf::validate_version(&version.s).map_err(|e| cdf(e, pos))?;
    b.insert("version", &version.s).map_err(|e| cdf(e, pos))?;

    let system = force_string(ev, am.get("system").unwrap(), pos)?;
    deps.extend(system.ctx.iter().cloned());
    b.insert("system", &system.s).map_err(|e| cdf(e, pos))?;

    // toolchain: explicit, else the ambient identity the driver passed
    // (05 §2; TODO(open) there: becomes a dep once toolchains are store
    // packages — CDF v2)
    let toolchain = match get_opt(ev, am, "toolchain", pos)? {
        Some(Value::Str(s)) => {
            deps.extend(s.ctx.iter().cloned());
            s.s.to_string()
        }
        Some(v) => return Err(type_err(format!("derivation: toolchain must be a string, got {}", v.type_of()), pos)),
        None => match &ev.toolchain {
            Some(t) => t.clone(),
            None => {
                return Err(type_err(
                    "derivation: no `toolchain` argument and no ambient toolchain identity was \
                     provided by the driver (pass --toolchain; shade-pkg 06 §4)",
                    pos,
                ));
            }
        },
    };
    b.insert("toolchain", &toolchain).map_err(|e| cdf(e, pos))?;

    // sandbox: int, default 1 — the only profile (shade-pkg 06 §3.1)
    let sandbox = match get_opt(ev, am, "sandbox", pos)? {
        None => 1,
        Some(Value::Int(i)) => i,
        Some(v) => return Err(type_err(format!("derivation: sandbox must be an int, got {}", v.type_of()), pos)),
    };
    b.insert("sandbox", &format!("{sandbox}")).map_err(|e| cdf(e, pos))?;

    // env (05 §2): argument keys match [A-Z_][A-Z0-9_]*, sandbox-fixed
    // rejection, null-drop; the CDF key is the name's lowercase fold —
    // lossless, builder restores uppercase (02 §3.2 rule 2 / §3.3, resolved
    // 2026-07-06).
    if let Some(envv) = get_opt(ev, am, "env", pos)? {
        let Value::Attrs(em) = envv else {
            return Err(type_err(format!("derivation: env must be a set, got {}", envv.type_of()), pos));
        };
        for (k, t) in em.iter() {
            if !valid_env_key(k) {
                return Err(type_err(
                    format!("derivation: env key `{k}` does not match [A-Z_][A-Z0-9_]*"),
                    pos,
                ));
            }
            if is_sandbox_fixed(k) {
                return Err(type_err(
                    format!("derivation: env key `{k}` is fixed by the sandbox (shade-pkg 06 §4)"),
                    pos,
                ));
            }
            let v = ev.force(&t.clone(), pos)?;
            if matches!(v, Value::Null) {
                continue; // conditional omission (05 §2)
            }
            let s = ev.coerce_to_string(&v, pos, CoerceMode::Full)?;
            deps.extend(s.ctx.iter().cloned());
            b.insert(&format!("env.{}", k.to_ascii_lowercase()), &s.s)
                .map_err(|e| cdf(e, pos))?;
        }
    }

    // phase.<i> in list order (05 §2). $out/$src<i>/$TARGET/$JOBS pass
    // through as literal bytes — the substitution seam (05 §3.1); Shade
    // interpolation has already run by the time the string exists.
    if let Some(phasesv) = get_opt(ev, am, "phases", pos)? {
        let Value::List(pl) = phasesv else {
            return Err(type_err(format!("derivation: phases must be a list, got {}", phasesv.type_of()), pos));
        };
        for (i, t) in pl.iter().enumerate() {
            let v = ev.force(t, pos)?;
            let s = ev.coerce_to_string(&v, pos, CoerceMode::Full)?;
            deps.extend(s.ctx.iter().cloned());
            b.insert(&format!("phase.{i}"), &s.s).map_err(|e| cdf(e, pos))?;
        }
    }

    // output.<i> in bin,lib,share order then list order (shade-pkg 03 §6)
    let outputsv = get_opt(ev, am, "outputs", pos)?
        .ok_or_else(|| type_err("derivation: missing required argument `outputs`", pos))?;
    let Value::Attrs(om) = outputsv else {
        return Err(type_err(format!("derivation: outputs must be a set, got {}", outputsv.type_of()), pos));
    };
    for k in om.keys() {
        if !["bin", "lib", "share"].contains(&k.as_str()) {
            return Err(type_err(format!("derivation: unknown outputs key `{k}`"), pos));
        }
    }
    let mut out_idx = 0usize;
    for cat in ["bin", "lib", "share"] {
        let Some(t) = om.get(cat) else { continue };
        let l = force_list(ev, t, pos)?;
        for e in l.iter() {
            let s = force_string(ev, e, pos)?;
            b.insert(&format!("output.{out_idx}"), &format!("{cat}/{}", s.s))
                .map_err(|e| cdf(e, pos))?;
            out_idx += 1;
        }
    }
    if out_idx == 0 {
        return Err(type_err(
            "derivation: outputs must declare at least one entry across bin/lib/share (shade-pkg 03 §6)",
            pos,
        ));
    }

    // explicit deps (05 §2.1: one flat list; build/runtime is sandbox
    // policy, not CDF)
    if let Some(depsv) = get_opt(ev, am, "deps", pos)? {
        let Value::List(dl) = depsv else {
            return Err(type_err(format!("derivation: deps must be a list, got {}", depsv.type_of()), pos));
        };
        for t in dl.iter() {
            let v = ev.force(t, pos)?;
            let Value::Attrs(dm) = &v else {
                return Err(type_err(format!("derivation: deps element is {}, expected a derivation", v.type_of()), pos));
            };
            if !ev.attrs_is_derivation(dm, pos)? {
                return Err(type_err("derivation: deps element is a set but not a derivation", pos));
            }
            // forcing outPath triggers the dependency's own emission —
            // recursion carries the whole closure (shade-pkg 02 §3.3)
            let out = ev.force_attr_string(dm, "outPath", pos)?;
            deps.insert(out.s.to_string());
        }
    }

    // source.<i>.* in list order (05 §4)
    if let Some(sourcesv) = get_opt(ev, am, "sources", pos)? {
        let Value::List(sl) = sourcesv else {
            return Err(type_err(format!("derivation: sources must be a list, got {}", sourcesv.type_of()), pos));
        };
        for (i, t) in sl.iter().enumerate() {
            let v = ev.force(t, pos)?;
            let kvs = source_identity(ev, &v, pos)?;
            for (k, val) in kvs {
                b.insert(&format!("source.{i}.{k}"), &val).map_err(|e| cdf(e, pos))?;
            }
        }
    }

    // display-only metadata: deep-forced (05 §3 step 1) but not hashed (05 §2)
    if let Some(v) = get_opt(ev, am, "description", pos)? {
        if !matches!(v, Value::Str(_)) {
            return Err(type_err("derivation: description must be a string", pos));
        }
    }
    if let Some(v) = get_opt(ev, am, "license", pos)? {
        if !matches!(v, Value::Str(_)) {
            return Err(type_err("derivation: license must be a string", pos));
        }
    }
    if let Some(v) = get_opt(ev, am, "bootCritical", pos)? {
        if !matches!(v, Value::Bool(_)) {
            return Err(type_err("derivation: bootCritical must be a bool", pos));
        }
    }

    // dep.<i>: union of explicit deps and string contexts, deduplicated by
    // store path, sorted bytewise (05 §2.2). The `sources` list emits
    // source.<i>.* identity keys, not dep entries — the shade-pkg 02 §3.3
    // worked example is normative here; TODO(open): 05 §3 step 3's wording
    // ("and source derivations") reads as if `sources` entries also become
    // deps, which would contradict that example. Flagged.
    for (i, d) in deps.iter().enumerate() {
        b.insert(&format!("dep.{i}"), d).map_err(|e| cdf(e, pos))?;
    }

    let bytes = b.build();
    let paths = shade_cdf::store_paths(&name, &version.s, &bytes).map_err(|e| cdf(e, pos))?;
    ev.drvs.insert(paths.drv_path.clone(), Rc::new(bytes));
    Ok((paths.drv_path, paths.out_path))
}

/// Identity keys (`type` + per-type fields) for one `sources` element
/// (05 §4): an ingested path, a fetch-builtin result, or an explicit pinned
/// attrset.
fn source_identity(ev: &mut Evaluator, v: &Value, pos: &Pos) -> Result<Vec<(String, String)>> {
    match v {
        Value::Path { path, store_origin } => {
            if *store_origin {
                return Err(type_err(
                    "derivation: a store path cannot be a source spec (04 §2.4 store-path literals are flagged; use the producing derivation)",
                    pos,
                ));
            }
            let (_out, tree) = ingest_path(ev, path, None, None, pos)?;
            Ok(alloc::vec![
                ("type".to_string(), "local".to_string()),
                ("tree".to_string(), tree),
            ])
        }
        Value::Attrs(m) => {
            if let Some(t) = m.get("__sourceType") {
                // a fetch-builtin result carries its pinned identity (05 §4)
                let ty = force_string(ev, &t.clone(), pos)?;
                let mut kvs = alloc::vec![("type".to_string(), ty.s.to_string())];
                match &*ty.s {
                    "crates-io" => {
                        for k in ["crate", "version", "sha256"] {
                            let s = ev.force_attr_string(m, k, pos)?;
                            kvs.push((k.to_string(), s.s.to_string()));
                        }
                    }
                    "git" => {
                        let c = ev.force_attr_string(m, "commit", pos)?;
                        kvs.push(("commit".to_string(), c.s.to_string()));
                        if let Some(sub) = m.get("submodules") {
                            if let Value::Bool(true) = ev.force(&sub.clone(), pos)? {
                                // present as 1 only when true (shade-pkg 04 §3.2)
                                kvs.push(("submodules".to_string(), "1".to_string()));
                            }
                        }
                    }
                    "local" => {
                        let t = ev.force_attr_string(m, "tree", pos)?;
                        kvs.push(("tree".to_string(), t.s.to_string()));
                    }
                    other => {
                        return Err(type_err(format!("derivation: unknown source type `{other}`"), pos));
                    }
                }
                Ok(kvs)
            } else if ev.attrs_is_derivation(m, pos)? {
                Err(type_err(
                    "derivation: a build derivation is not a source spec; put it in `deps`",
                    pos,
                ))
            } else {
                explicit_source_identity(ev, m, pos)
            }
        }
        v => Err(type_err(
            format!("derivation: source spec must be a path, fetch result, or source attrset, got {}", v.type_of()),
            pos,
        )),
    }
}

/// An explicit source attrset is a *pinned* identity — validated per type,
/// never re-resolved (05 §4).
fn explicit_source_identity(
    ev: &mut Evaluator,
    m: &Rc<AttrsMap>,
    pos: &Pos,
) -> Result<Vec<(String, String)>> {
    let ty = ev.force_attr_string(m, "type", pos)?;
    let allowed: &[&str] = match &*ty.s {
        "crates-io" => &["type", "crate", "version", "sha256"],
        // `url` is transport only, never a hash input (shade-pkg 04 §3.2) —
        // accepted and dropped
        "git" => &["type", "commit", "submodules", "url"],
        "local" => &["type", "tree"],
        "pspackage" => &["type", "tree", "path"], // `path` informational (shade-pkg 04 §5)
        other => return Err(type_err(format!("derivation: unknown source type `{other}`"), pos)),
    };
    for k in m.keys() {
        if !allowed.contains(&k.as_str()) {
            return Err(type_err(
                format!("derivation: unknown key `{k}` in `{}` source spec", ty.s),
                pos,
            ));
        }
    }
    let mut kvs = alloc::vec![("type".to_string(), ty.s.to_string())];
    match &*ty.s {
        "crates-io" => {
            for k in ["crate", "version", "sha256"] {
                let s = ev.force_attr_string(m, k, pos)?;
                if k == "sha256" && !is_lower_hex(&s.s, 64) {
                    return Err(type_err("derivation: sha256 must be 64 lowercase hex", pos));
                }
                kvs.push((k.to_string(), s.s.to_string()));
            }
        }
        "git" => {
            let c = ev.force_attr_string(m, "commit", pos)?;
            if !is_lower_hex(&c.s, 40) {
                return Err(type_err(
                    "derivation: git commit must be 40 lowercase hex (a resolved commit, never a branch/tag — 05 §5)",
                    pos,
                ));
            }
            kvs.push(("commit".to_string(), c.s.to_string()));
            if let Some(sub) = m.get("submodules") {
                match ev.force(&sub.clone(), pos)? {
                    Value::Bool(true) => kvs.push(("submodules".to_string(), "1".to_string())),
                    Value::Bool(false) => {}
                    v => return Err(type_err(format!("derivation: submodules must be a bool, got {}", v.type_of()), pos)),
                }
            }
        }
        "local" | "pspackage" => {
            let t = ev.force_attr_string(m, "tree", pos)?;
            if !is_lower_hex(&t.s, 64) {
                return Err(type_err("derivation: tree hash must be 64 lowercase hex", pos));
            }
            kvs.push(("tree".to_string(), t.s.to_string()));
        }
        _ => unreachable!(),
    }
    Ok(kvs)
}

// ---- ingestion (04 §4.2) --------------------------------------------------

/// Ingest a filesystem path: tree-hash it (shade-pkg 04 §3.3 algorithm via
/// shade-cdf) and emit its `local` source derivation. Returns
/// `(outPath, tree_hash)`.
///
/// The tree's *bytes* are not copied anywhere: shadec never writes the
/// store (08 §2). TODO(open): hand-off of ingested trees to the store
/// services — blocked on the shade store layer existing.
pub fn ingest_path(
    ev: &mut Evaluator,
    path: &str,
    name_override: Option<&str>,
    filter: Option<Value>,
    pos: &Pos,
) -> Result<(String, String)> {
    if path.starts_with(shade_cdf::STORE_PREFIX) {
        return Err(type_err("cannot ingest a path already inside the store", pos));
    }
    let plain = name_override.is_none() && filter.is_none();
    if plain {
        if let Some((out, tree)) = ev.ingest_memo.get(path) {
            return Ok((out.clone(), tree.clone()));
        }
    }

    let meta = ev
        .io
        .metadata(path)
        .map_err(|e| err(ErrorKind::Import, format!("ingest: {e}"), pos))?;
    let mut lines: Vec<String> = Vec::new();
    match meta.kind {
        FileKind::Directory => {
            walk_tree(ev, path, "", &filter, &mut lines, pos)?;
        }
        // a single file path ingests as a single-file tree (04 §4.2);
        // the manifest entry is its base name
        FileKind::Regular => {
            let content = ev
                .io
                .read_file(path)
                .map_err(|e| err(ErrorKind::Import, format!("ingest: {e}"), pos))?;
            let kind = if meta.exec {
                shade_cdf::treehash::EntryKind::ExecFile
            } else {
                shade_cdf::treehash::EntryKind::File
            };
            lines.push(shade_cdf::treehash::manifest_line(
                kind,
                base_name(path),
                &shade_cdf::blake3_hex(&content),
            ));
        }
        FileKind::Symlink => {
            let target = ev
                .io
                .read_link(path)
                .map_err(|e| err(ErrorKind::Import, format!("ingest: {e}"), pos))?;
            lines.push(shade_cdf::treehash::manifest_line(
                shade_cdf::treehash::EntryKind::Symlink,
                base_name(path),
                &shade_cdf::blake3_hex(target.as_bytes()),
            ));
        }
        FileKind::Other => {
            return Err(err(ErrorKind::Import, format!("ingest: unsupported file type at {path}"), pos));
        }
    }
    let tree = shade_cdf::treehash::tree_hash(lines);

    // TODO(open): source-derivation name/version for `local` sources —
    // shade-pkg 04 §2 says "version = pinned version (or commit/tree
    // shorthand as defined per type in §3)" but §3.3 defines no shorthand.
    // Chosen here: name = normalized base name, version = first 12 hex of
    // the tree hash. Byte-affecting once the store exists; freeze in 04 §3.3.
    let raw_name = name_override.map(str::to_string).unwrap_or_else(|| base_name(path).to_string());
    let name = shade_cdf::normalize_name(&raw_name)
        .map_err(|e| type_err(format!("ingest: {e} (pass `name` to builtins.path)"), pos))?;
    let version = tree[..12].to_string();
    let (drv_path, out_path) = emit_source_drv(
        ev,
        &name,
        &version,
        &[("type".to_string(), "local".to_string()), ("tree".to_string(), tree.clone())],
        pos,
    )?;
    let _ = drv_path;

    // tracked eval input (03 §5.3): ingested path + hash
    ev.eval_inputs.insert(format!("ingest:{path}={tree}"));
    if plain {
        ev.ingest_memo.insert(path.to_string(), (out_path.clone(), tree.clone()));
    }
    Ok((out_path, tree))
}

fn base_name(p: &str) -> &str {
    match p.rfind('/') {
        Some(i) => &p[i + 1..],
        None => p,
    }
}

fn walk_tree(
    ev: &mut Evaluator,
    abs: &str,
    rel: &str,
    filter: &Option<Value>,
    lines: &mut Vec<String>,
    pos: &Pos,
) -> Result<()> {
    let entries = ev
        .io
        .read_dir(abs)
        .map_err(|e| err(ErrorKind::Import, format!("ingest: {e}"), pos))?;
    for (name, kind) in entries {
        // exclude .git/, and nothing else (shade-pkg 04 §3.3 step 1)
        if name == ".git" && kind == FileKind::Directory {
            continue;
        }
        let child_abs = format!("{abs}/{name}");
        let child_rel = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
        if let Some(f) = filter {
            // filter sees (absolute path, type string) — pruned entries
            // never reach the hash (04 §4.2)
            let ty = match kind {
                FileKind::Regular => "regular",
                FileKind::Directory => "directory",
                FileKind::Symlink => "symlink",
                FileKind::Other => "unknown",
            };
            let g = ev.apply(f.clone(), Thunk::done(Value::Str(ShStr::plain(child_abs.as_str()))), pos)?;
            match ev.apply(g, Thunk::done(Value::Str(ShStr::plain(ty))), pos)? {
                Value::Bool(true) => {}
                Value::Bool(false) => continue,
                v => return Err(type_err(format!("source filter returned {}", v.type_of()), pos)),
            }
        }
        match kind {
            FileKind::Regular => {
                let meta = ev
                    .io
                    .metadata(&child_abs)
                    .map_err(|e| err(ErrorKind::Import, format!("ingest: {e}"), pos))?;
                let content = ev
                    .io
                    .read_file(&child_abs)
                    .map_err(|e| err(ErrorKind::Import, format!("ingest: {e}"), pos))?;
                let k = if meta.exec {
                    shade_cdf::treehash::EntryKind::ExecFile
                } else {
                    shade_cdf::treehash::EntryKind::File
                };
                lines.push(shade_cdf::treehash::manifest_line(
                    k,
                    &child_rel,
                    &shade_cdf::blake3_hex(&content),
                ));
            }
            FileKind::Directory => {
                lines.push(shade_cdf::treehash::manifest_line(
                    shade_cdf::treehash::EntryKind::Dir,
                    &child_rel,
                    "",
                ));
                walk_tree(ev, &child_abs, &child_rel, filter, lines, pos)?;
            }
            FileKind::Symlink => {
                let target = ev
                    .io
                    .read_link(&child_abs)
                    .map_err(|e| err(ErrorKind::Import, format!("ingest: {e}"), pos))?;
                lines.push(shade_cdf::treehash::manifest_line(
                    shade_cdf::treehash::EntryKind::Symlink,
                    &child_rel,
                    &shade_cdf::blake3_hex(target.as_bytes()),
                ));
            }
            FileKind::Other => {
                return Err(err(
                    ErrorKind::Import,
                    format!("ingest: unsupported file type at {child_abs}"),
                    pos,
                ));
            }
        }
    }
    Ok(())
}

/// Emit a source derivation: the trimmed CDF — `builder=fetch`, no
/// `system`/`toolchain`/`sandbox`, no dep/phase/output keys
/// (shade-pkg 04 §2; shade 05 §4.1). `name` is already normalized, the
/// `-src` suffix is appended here.
fn emit_source_drv(
    ev: &mut Evaluator,
    name: &str,
    version: &str,
    identity: &[(String, String)],
    pos: &Pos,
) -> Result<(String, String)> {
    let cdf = |e: shade_cdf::CdfError| type_err(format!("source derivation: {e}"), pos);
    let src_name = format!("{name}-src");
    let mut b = CdfBuilder::new();
    // TODO(open): `builder` is not in the shade-pkg 02 §3.3 key table yet —
    // 04 §2 flags folding it in at CDF v1 freeze; shadec follows shade (05 §4.1)
    b.insert("builder", "fetch").map_err(cdf)?;
    b.insert("name", &src_name).map_err(cdf)?;
    b.insert("version", version).map_err(cdf)?;
    for (k, v) in identity {
        b.insert(&format!("source.0.{k}"), v).map_err(cdf)?;
    }
    let bytes = b.build();
    let paths = shade_cdf::store_paths(&src_name, version, &bytes).map_err(cdf)?;
    ev.drvs.insert(paths.drv_path.clone(), Rc::new(bytes));
    Ok((paths.drv_path, paths.out_path))
}

/// Build the derivation *value* for a fetch builtin result (04 §6): a
/// derivation-marked attrset carrying its pinned identity for later
/// `sources` emission.
fn source_drv_value(
    ev: &mut Evaluator,
    name: &str,
    version: &str,
    source_type: &str,
    identity_attrs: &[(&str, Value)],
    cdf_identity: &[(String, String)],
    pos: &Pos,
) -> Result<Value> {
    let (drv_path, out_path) = emit_source_drv(ev, name, version, cdf_identity, pos)?;
    let mut m: AttrsMap = BTreeMap::new();
    m.insert("type".to_string(), Thunk::done(Value::Str(ShStr::plain("derivation"))));
    m.insert("name".to_string(), Thunk::done(Value::Str(ShStr::plain(format!("{name}-src")))));
    m.insert("version".to_string(), Thunk::done(Value::Str(ShStr::plain(version))));
    m.insert("outputName".to_string(), Thunk::done(Value::Str(ShStr::plain("out"))));
    m.insert("drvPath".to_string(), Thunk::done(Value::Str(ShStr::plain(drv_path))));
    let mut ctx = BTreeSet::new();
    ctx.insert(out_path.clone());
    m.insert(
        "outPath".to_string(),
        Thunk::done(Value::Str(ShStr::with_ctx(out_path, Rc::new(ctx)))),
    );
    m.insert("__sourceType".to_string(), Thunk::done(Value::Str(ShStr::plain(source_type))));
    for (k, v) in identity_attrs {
        m.insert((*k).to_string(), Thunk::done(v.clone()));
    }
    Ok(Value::Attrs(Rc::new(m)))
}

// ---- fetch builtins (05 §5) -------------------------------------------------

fn require_exact_keys(m: &AttrsMap, required: &[&str], optional: &[&str], what: &str, pos: &Pos) -> Result<()> {
    for k in m.keys() {
        if !required.contains(&k.as_str()) && !optional.contains(&k.as_str()) {
            return Err(type_err(format!("{what}: unknown argument `{k}`"), pos));
        }
    }
    for k in required {
        if !m.contains_key(*k) {
            return Err(type_err(format!("{what}: missing required argument `{k}`"), pos));
        }
    }
    Ok(())
}

pub fn prim_fetch_crates_io(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    let m = force_attrs(ev, &args[0], pos)?;
    require_exact_keys(&m, &["crate", "version", "sha256"], &[], "fetchCratesIo", pos)?;
    let krate = ev.force_attr_string(&m, "crate", pos)?;
    let version = ev.force_attr_string(&m, "version", pos)?;
    let sha256 = ev.force_attr_string(&m, "sha256", pos)?;
    // a missing/empty/placeholder hash is an eval error (05 §5)
    if !is_lower_hex(&sha256.s, 64) {
        return Err(type_err("fetchCratesIo: sha256 must be 64 lowercase hex (declared, pinned — no fetch-then-hash impurity)", pos));
    }
    shade_cdf::validate_version(&version.s).map_err(|e| type_err(format!("fetchCratesIo: {e}"), pos))?;
    let name = shade_cdf::normalize_name(&krate.s)
        .map_err(|e| type_err(format!("fetchCratesIo: {e}"), pos))?;
    source_drv_value(
        ev,
        &name,
        &version.s,
        "crates-io",
        &[
            ("crate", Value::Str(krate.clone())),
            ("version", Value::Str(version.clone())),
            ("sha256", Value::Str(sha256.clone())),
        ],
        &[
            ("type".to_string(), "crates-io".to_string()),
            ("crate".to_string(), krate.s.to_string()),
            ("version".to_string(), version.s.to_string()),
            ("sha256".to_string(), sha256.s.to_string()),
        ],
        pos,
    )
}

pub fn prim_fetch_git(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    let m = force_attrs(ev, &args[0], pos)?;
    require_exact_keys(&m, &["url", "commit"], &["submodules"], "fetchGit", pos)?;
    let url = ev.force_attr_string(&m, "url", pos)?;
    let commit = ev.force_attr_string(&m, "commit", pos)?;
    if !is_lower_hex(&commit.s, 40) {
        return Err(type_err(
            "fetchGit: commit must be a resolved 40-hex commit (branches/tags resolve at lock time, never at eval — 05 §5)",
            pos,
        ));
    }
    let submodules = match m.get("submodules") {
        None => false,
        Some(t) => match ev.force(&t.clone(), pos)? {
            Value::Bool(b) => b,
            v => return Err(type_err(format!("fetchGit: submodules must be a bool, got {}", v.type_of()), pos)),
        },
    };
    // TODO(open): source-derivation name/version for git sources is not
    // specified (shade-pkg 04 §2 defers the shorthand to §3, which defines
    // none for git). Chosen: name = last URL segment minus `.git`,
    // normalized; version = first 12 hex of the commit. The URL itself is
    // transport only and never hashed (shade-pkg 04 §3.2).
    let seg = url.s.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    let seg = seg.strip_suffix(".git").unwrap_or(seg);
    let name = shade_cdf::normalize_name(seg)
        .map_err(|e| type_err(format!("fetchGit: cannot derive a source name from the URL ({e})"), pos))?;
    let version = commit.s[..12].to_string();
    let mut cdf_id = alloc::vec![
        ("type".to_string(), "git".to_string()),
        ("commit".to_string(), commit.s.to_string()),
    ];
    if submodules {
        cdf_id.push(("submodules".to_string(), "1".to_string()));
    }
    source_drv_value(
        ev,
        &name,
        &version,
        "git",
        &[
            ("url", Value::Str(url.clone())),
            ("commit", Value::Str(commit.clone())),
            ("submodules", Value::Bool(submodules)),
        ],
        &cdf_id,
        pos,
    )
}

/// `builtins.path { path; name?; filter?; sha256?; }` — explicit ingestion
/// (05 §5, 07 §2.5).
pub fn prim_path(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    let m = force_attrs(ev, &args[0], pos)?;
    require_exact_keys(&m, &["path"], &["name", "filter", "sha256"], "builtins.path", pos)?;
    let p = match ev.force(&m.get("path").unwrap().clone(), pos)? {
        Value::Path { path, .. } => path.to_string(),
        v => return Err(type_err(format!("builtins.path: path must be a path value, got {}", v.type_of()), pos)),
    };
    let name = match m.get("name") {
        None => None,
        Some(t) => Some(ev.force(&t.clone(), pos)?),
    };
    let name_s = match &name {
        None => None,
        Some(Value::Str(s)) => Some(s.s.to_string()),
        Some(v) => return Err(type_err(format!("builtins.path: name must be a string, got {}", v.type_of()), pos)),
    };
    let filter = match m.get("filter") {
        None => None,
        Some(t) => Some(ev.force(&t.clone(), pos)?),
    };
    let (out_path, tree) = ingest_path(ev, &p, name_s.as_deref(), filter, pos)?;
    if let Some(t) = m.get("sha256") {
        let declared = force_string(ev, &t.clone(), pos)?;
        // verified fixed-output ingestion: fails closed on drift (05 §5).
        // TODO(open): the argument is named `sha256` but the local-source
        // identity is the BLAKE3 tree hash (shade-pkg 04 §3.3) — spec naming
        // mismatch, flagged. Compared against the tree hash.
        if *declared.s != tree {
            return Err(err(
                ErrorKind::Import,
                format!("builtins.path: declared hash {} does not match tree hash {tree} (fails closed, 05 §5)", declared.s),
                pos,
            ));
        }
    }
    let drv_path = format!("{out_path}.drv");
    let final_name = {
        // reconstruct the -src name the emission used
        let raw = name_s.unwrap_or_else(|| base_name(&p).to_string());
        shade_cdf::normalize_name(&raw).unwrap()
    };
    let mut mm: AttrsMap = BTreeMap::new();
    mm.insert("type".to_string(), Thunk::done(Value::Str(ShStr::plain("derivation"))));
    mm.insert("name".to_string(), Thunk::done(Value::Str(ShStr::plain(format!("{final_name}-src")))));
    mm.insert("version".to_string(), Thunk::done(Value::Str(ShStr::plain(tree[..12].to_string()))));
    mm.insert("outputName".to_string(), Thunk::done(Value::Str(ShStr::plain("out"))));
    mm.insert("drvPath".to_string(), Thunk::done(Value::Str(ShStr::plain(drv_path))));
    let mut ctx = BTreeSet::new();
    ctx.insert(out_path.clone());
    mm.insert("outPath".to_string(), Thunk::done(Value::Str(ShStr::with_ctx(out_path, Rc::new(ctx)))));
    mm.insert("__sourceType".to_string(), Thunk::done(Value::Str(ShStr::plain("local"))));
    mm.insert("tree".to_string(), Thunk::done(Value::Str(ShStr::plain(tree))));
    Ok(Value::Attrs(Rc::new(mm)))
}

/// `builtins.filterSource filter path` (07 §2.5) — ingest with a per-entry
/// filter, hashed post-filter.
pub fn prim_filter_source(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    let filter = ev.force(&args[0], pos)?;
    let p = match ev.force(&args[1], pos)? {
        Value::Path { path, .. } => path.to_string(),
        v => return Err(type_err(format!("filterSource: expected a path, got {}", v.type_of()), pos)),
    };
    let mut m: AttrsMap = BTreeMap::new();
    m.insert("path".to_string(), Thunk::done(Value::path_value(p)));
    m.insert("filter".to_string(), Thunk::done(filter));
    prim_path(ev, &[Thunk::done(Value::Attrs(Rc::new(m)))], pos)
}
