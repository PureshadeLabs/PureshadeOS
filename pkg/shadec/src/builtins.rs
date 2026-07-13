//! MVP `builtins` surface per `docs/shade/07-stdlib.md` §2 (entries marked
//! [MVP]) and the initial scope per `docs/shade/03-semantics.md` §4.1.
//!
//! Purity (03 §5.1) holds structurally: there is no `getEnv`, no
//! `currentTime`, no `currentSystem` — the names simply do not exist, so a
//! recipe reaching for them gets `undefined variable` / missing attribute.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::error::{ErrorKind, EvalError, Pos, Result};
use crate::eval::{CoerceMode, Evaluator};
use crate::value::{
    empty_ctx, AttrsMap, Env, Prim, PrimApp, ShStr, Thunk, ThunkRef, Value,
};

// ---- helpers -------------------------------------------------------------

pub fn err(kind: ErrorKind, msg: impl Into<String>, pos: &Pos) -> EvalError {
    EvalError::at(kind, msg, pos)
}

pub fn type_err(msg: impl Into<String>, pos: &Pos) -> EvalError {
    err(ErrorKind::Type, msg, pos)
}

pub fn force_int(ev: &mut Evaluator, t: &ThunkRef, pos: &Pos) -> Result<i64> {
    match ev.force(t, pos)? {
        Value::Int(i) => Ok(i),
        v => Err(type_err(format!("expected int, got {}", v.type_of()), pos)),
    }
}

pub fn force_string(ev: &mut Evaluator, t: &ThunkRef, pos: &Pos) -> Result<ShStr> {
    match ev.force(t, pos)? {
        Value::Str(s) => Ok(s),
        v => Err(type_err(format!("expected string, got {}", v.type_of()), pos)),
    }
}

pub fn force_list(ev: &mut Evaluator, t: &ThunkRef, pos: &Pos) -> Result<Rc<Vec<ThunkRef>>> {
    match ev.force(t, pos)? {
        Value::List(l) => Ok(l),
        v => Err(type_err(format!("expected list, got {}", v.type_of()), pos)),
    }
}

pub fn force_attrs(ev: &mut Evaluator, t: &ThunkRef, pos: &Pos) -> Result<Rc<AttrsMap>> {
    match ev.force(t, pos)? {
        Value::Attrs(m) => Ok(m),
        v => Err(type_err(format!("expected set, got {}", v.type_of()), pos)),
    }
}

pub fn force_bool(ev: &mut Evaluator, t: &ThunkRef, pos: &Pos) -> Result<bool> {
    match ev.force(t, pos)? {
        Value::Bool(b) => Ok(b),
        v => Err(type_err(format!("expected bool, got {}", v.type_of()), pos)),
    }
}

/// Accept a path value or a context-bearing string as a filesystem target
/// (import and the read builtins; a context-free string is rejected —
/// 06 §1 signature).
pub fn force_pathlike(ev: &mut Evaluator, t: &ThunkRef, pos: &Pos) -> Result<String> {
    match ev.force(t, pos)? {
        Value::Path { path, .. } => Ok(path.to_string()),
        Value::Str(s) if !s.ctx.is_empty() => Ok(s.s.to_string()),
        Value::Str(_) => Err(type_err(
            "expected a path (a context-free string is not a filesystem reference)",
            pos,
        )),
        v => Err(type_err(format!("expected path, got {}", v.type_of()), pos)),
    }
}

fn apply2(ev: &mut Evaluator, f: &Value, a: ThunkRef, b: ThunkRef, pos: &Pos) -> Result<Value> {
    let g = ev.apply(f.clone(), a, pos)?;
    ev.apply(g, b, pos)
}

// ---- core / evaluation (07 §2.1) ------------------------------------------

fn prim_throw(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    let v = ev.force(&args[0], pos)?;
    let s = ev.coerce_to_string(&v, pos, CoerceMode::Full)?;
    Err(err(ErrorKind::Throw, s.s.to_string(), pos))
}

fn prim_abort(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    let v = ev.force(&args[0], pos)?;
    let s = ev.coerce_to_string(&v, pos, CoerceMode::Full)?;
    Err(err(ErrorKind::Abort, s.s.to_string(), pos))
}

fn prim_try_eval(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    let mut m: AttrsMap = BTreeMap::new();
    match ev.force(&args[0], pos) {
        Ok(v) => {
            m.insert("success".to_string(), Thunk::done(Value::Bool(true)));
            m.insert("value".to_string(), Thunk::done(v));
        }
        Err(e) if e.kind.catchable() => {
            m.insert("success".to_string(), Thunk::done(Value::Bool(false)));
            m.insert("value".to_string(), Thunk::done(Value::Bool(false)));
        }
        Err(e) => return Err(e), // abort / recursion / limits are non-recoverable (03 §8)
    }
    Ok(Value::Attrs(Rc::new(m)))
}

fn prim_seq(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    ev.force(&args[0], pos)?;
    ev.force(&args[1], pos)
}

fn prim_deep_seq(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    let v = ev.force(&args[0], pos)?;
    ev.deep_force(&v, pos)?;
    ev.force(&args[1], pos)
}

fn prim_trace(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    let v = ev.force(&args[0], pos)?;
    let msg = match &v {
        Value::Str(s) => s.s.to_string(),
        v => crate::print::show_value(ev, v, false, pos)?,
    };
    ev.io.trace(&msg);
    ev.force(&args[1], pos)
}

fn prim_type_of(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    let v = ev.force(&args[0], pos)?;
    Ok(Value::Str(ShStr::plain(v.type_of())))
}

// ---- numbers (07 §2.2) -----------------------------------------------------

fn arith(
    ev: &mut Evaluator,
    args: &[ThunkRef],
    pos: &Pos,
    f: fn(i64, i64) -> Result<i64>,
) -> Result<Value> {
    let a = force_int(ev, &args[0], pos)?;
    let b = force_int(ev, &args[1], pos)?;
    f(a, b).map(Value::Int).map_err(|mut e| {
        e.pos = Some(pos.clone());
        e
    })
}

fn prim_add(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    arith(ev, args, pos, |a, b| Ok(a.wrapping_add(b)))
}
fn prim_sub(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    arith(ev, args, pos, |a, b| Ok(a.wrapping_sub(b)))
}
fn prim_mul(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    arith(ev, args, pos, |a, b| Ok(a.wrapping_mul(b)))
}
fn prim_div(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    arith(ev, args, pos, |a, b| {
        if b == 0 {
            Err(EvalError::new(ErrorKind::Type, "division by zero"))
        } else {
            Ok(a.wrapping_div(b))
        }
    })
}

fn prim_less_than(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    let a = ev.force(&args[0], pos)?;
    let b = ev.force(&args[1], pos)?;
    Ok(Value::Bool(ev.cmp_values(&a, &b, pos)? == core::cmp::Ordering::Less))
}

// ---- lists (07 §2.3) --------------------------------------------------------

fn prim_length(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    Ok(Value::Int(force_list(ev, &args[0], pos)?.len() as i64))
}

fn prim_elem_at(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    let l = force_list(ev, &args[0], pos)?;
    let i = force_int(ev, &args[1], pos)?;
    if i < 0 || i as usize >= l.len() {
        return Err(type_err(format!("list index {i} out of range (length {})", l.len()), pos));
    }
    ev.force(&l[i as usize], pos)
}

fn prim_head(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    let l = force_list(ev, &args[0], pos)?;
    match l.first() {
        Some(t) => ev.force(t, pos),
        None => Err(type_err("head of empty list", pos)),
    }
}

fn prim_tail(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    let l = force_list(ev, &args[0], pos)?;
    if l.is_empty() {
        return Err(type_err("tail of empty list", pos));
    }
    Ok(Value::List(Rc::new(l[1..].to_vec())))
}

fn prim_map(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    let f = args[0].clone();
    let l = force_list(ev, &args[1], pos)?;
    // lazy in elements (07 §2.3): each result element is a thunk applying f
    let out: Vec<ThunkRef> = l
        .iter()
        .map(|x| {
            let f = f.clone();
            let x = x.clone();
            let pos = pos.clone();
            Thunk::native(alloc::boxed::Box::new(move |ev: &mut Evaluator| {
                let fv = ev.force(&f, &pos)?;
                ev.apply(fv, x, &pos)
            }))
        })
        .collect();
    Ok(Value::List(Rc::new(out)))
}

fn prim_filter(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    let f = ev.force(&args[0], pos)?;
    let l = force_list(ev, &args[1], pos)?;
    let mut out = Vec::new();
    for x in l.iter() {
        match ev.apply(f.clone(), x.clone(), pos)? {
            Value::Bool(true) => out.push(x.clone()),
            Value::Bool(false) => {}
            v => return Err(type_err(format!("filter predicate returned {}", v.type_of()), pos)),
        }
    }
    Ok(Value::List(Rc::new(out)))
}

fn prim_elem(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    let x = ev.force(&args[0], pos)?;
    let l = force_list(ev, &args[1], pos)?;
    for t in l.iter() {
        let v = ev.force(t, pos)?;
        if ev.eq_values(&x, &v, pos)? {
            return Ok(Value::Bool(true));
        }
    }
    Ok(Value::Bool(false))
}

fn prim_concat_lists(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    let outer = force_list(ev, &args[0], pos)?;
    let mut out = Vec::new();
    for t in outer.iter() {
        let l = force_list(ev, t, pos)?;
        out.extend(l.iter().cloned());
    }
    Ok(Value::List(Rc::new(out)))
}

fn prim_foldl_strict(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    let op = ev.force(&args[0], pos)?;
    let mut acc = ev.force(&args[1], pos)?; // accumulator forced each step
    let l = force_list(ev, &args[2], pos)?;
    for x in l.iter() {
        acc = apply2(ev, &op, Thunk::done(acc), x.clone(), pos)?;
    }
    Ok(acc)
}

fn prim_gen_list(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    let f = args[0].clone();
    let n = force_int(ev, &args[1], pos)?;
    if n < 0 {
        return Err(type_err(format!("genList: negative length {n}"), pos));
    }
    let out: Vec<ThunkRef> = (0..n)
        .map(|i| {
            let f = f.clone();
            let pos = pos.clone();
            Thunk::native(alloc::boxed::Box::new(move |ev: &mut Evaluator| {
                let fv = ev.force(&f, &pos)?;
                ev.apply(fv, Thunk::done(Value::Int(i)), &pos)
            }))
        })
        .collect();
    Ok(Value::List(Rc::new(out)))
}

fn prim_sort(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    let cmp = ev.force(&args[0], pos)?;
    let l = force_list(ev, &args[1], pos)?;
    // stable merge sort driven by the strict-weak `lessThan` predicate
    let mut items: Vec<ThunkRef> = l.iter().cloned().collect();
    let mut less = |ev: &mut Evaluator, a: &ThunkRef, b: &ThunkRef| -> Result<bool> {
        match apply2(ev, &cmp, a.clone(), b.clone(), pos)? {
            Value::Bool(b) => Ok(b),
            v => Err(type_err(format!("sort comparator returned {}", v.type_of()), pos)),
        }
    };
    merge_sort(ev, &mut items, &mut less)?;
    Ok(Value::List(Rc::new(items)))
}

fn merge_sort(
    ev: &mut Evaluator,
    items: &mut Vec<ThunkRef>,
    less: &mut impl FnMut(&mut Evaluator, &ThunkRef, &ThunkRef) -> Result<bool>,
) -> Result<()> {
    let n = items.len();
    if n <= 1 {
        return Ok(());
    }
    let mut right = items.split_off(n / 2);
    merge_sort(ev, items, less)?;
    merge_sort(ev, &mut right, less)?;
    let left = core::mem::take(items);
    let (mut i, mut j) = (0, 0);
    while i < left.len() && j < right.len() {
        // stability: take from the left unless right is strictly less
        if less(ev, &right[j], &left[i])? {
            items.push(right[j].clone());
            j += 1;
        } else {
            items.push(left[i].clone());
            i += 1;
        }
    }
    items.extend_from_slice(&left[i..]);
    items.extend_from_slice(&right[j..]);
    Ok(())
}

// ---- attrsets (07 §2.4) -----------------------------------------------------

fn prim_attr_names(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    let m = force_attrs(ev, &args[0], pos)?;
    // keys sorted bytewise — BTreeMap iteration order
    let out: Vec<ThunkRef> =
        m.keys().map(|k| Thunk::done(Value::Str(ShStr::plain(k.as_str())))).collect();
    Ok(Value::List(Rc::new(out)))
}

fn prim_attr_values(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    let m = force_attrs(ev, &args[0], pos)?;
    Ok(Value::List(Rc::new(m.values().cloned().collect())))
}

fn prim_get_attr(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    let name = force_string(ev, &args[0], pos)?;
    let m = force_attrs(ev, &args[1], pos)?;
    match m.get(&*name.s) {
        Some(t) => ev.force(&t.clone(), pos),
        None => Err(err(ErrorKind::MissingAttr, format!("attribute `{}` missing", name.s), pos)),
    }
}

fn prim_has_attr(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    let name = force_string(ev, &args[0], pos)?;
    let m = force_attrs(ev, &args[1], pos)?;
    Ok(Value::Bool(m.contains_key(&*name.s)))
}

fn prim_remove_attrs(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    let m = force_attrs(ev, &args[0], pos)?;
    let names = force_list(ev, &args[1], pos)?;
    let mut out = (*m).clone();
    for t in names.iter() {
        let n = force_string(ev, t, pos)?;
        out.remove(&*n.s);
    }
    Ok(Value::Attrs(Rc::new(out)))
}

fn prim_map_attrs(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    let f = args[0].clone();
    let m = force_attrs(ev, &args[1], pos)?;
    let mut out: AttrsMap = BTreeMap::new();
    for (k, v) in m.iter() {
        let f = f.clone();
        let k2 = k.clone();
        let v = v.clone();
        let pos2 = pos.clone();
        out.insert(
            k.clone(),
            Thunk::native(alloc::boxed::Box::new(move |ev: &mut Evaluator| {
                let fv = ev.force(&f, &pos2)?;
                let g = ev.apply(fv, Thunk::done(Value::Str(ShStr::plain(k2.as_str()))), &pos2)?;
                ev.apply(g, v, &pos2)
            })),
        );
    }
    Ok(Value::Attrs(Rc::new(out)))
}

fn prim_list_to_attrs(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    let l = force_list(ev, &args[0], pos)?;
    let mut out: AttrsMap = BTreeMap::new();
    for t in l.iter() {
        let m = force_attrs(ev, t, pos)?;
        let name = match m.get("name") {
            Some(nt) => force_string(ev, &nt.clone(), pos)?,
            None => return Err(err(ErrorKind::MissingAttr, "listToAttrs: entry has no `name`", pos)),
        };
        let value = m
            .get("value")
            .ok_or_else(|| err(ErrorKind::MissingAttr, "listToAttrs: entry has no `value`", pos))?
            .clone();
        out.insert(name.s.to_string(), value); // later duplicate name wins
    }
    Ok(Value::Attrs(Rc::new(out)))
}

// ---- strings (07 §2.6) --------------------------------------------------------

fn prim_to_string(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    let v = ev.force(&args[0], pos)?;
    Ok(Value::Str(ev.coerce_to_string(&v, pos, CoerceMode::Full)?))
}

fn prim_substring(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    let start = force_int(ev, &args[0], pos)?;
    let len = force_int(ev, &args[1], pos)?;
    let s = force_string(ev, &args[2], pos)?;
    if start < 0 {
        return Err(type_err("substring: negative start", pos));
    }
    let bytes = s.s.as_bytes();
    let begin = (start as usize).min(bytes.len());
    let end = if len < 0 { bytes.len() } else { (begin + len as usize).min(bytes.len()) };
    let sliced = core::str::from_utf8(&bytes[begin..end]).map_err(|_| {
        // Strings are UTF-8 in this implementation; byte-level slicing
        // through a multibyte character has no representable result.
        // TODO(open): 04 treats strings as byte strings ("byte length",
        // "byte-equal"); a full byte-string value representation is
        // deferred — flagged in the crate report.
        type_err("substring: slice does not fall on a UTF-8 boundary", pos)
    })?;
    Ok(Value::Str(ShStr::with_ctx(sliced.to_string(), s.ctx.clone()))) // context propagated
}

fn prim_string_length(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    let s = force_string(ev, &args[0], pos)?;
    Ok(Value::Int(s.s.len() as i64)) // byte length
}

fn prim_replace_strings(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    let from_l = force_list(ev, &args[0], pos)?;
    let to_l = force_list(ev, &args[1], pos)?;
    if from_l.len() != to_l.len() {
        return Err(type_err("replaceStrings: `from` and `to` lengths differ", pos));
    }
    let s = force_string(ev, &args[2], pos)?;
    let mut froms = Vec::with_capacity(from_l.len());
    let mut tos = Vec::with_capacity(to_l.len());
    for t in from_l.iter() {
        froms.push(force_string(ev, t, pos)?);
    }
    for t in to_l.iter() {
        tos.push(force_string(ev, t, pos)?);
    }
    let src = s.s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(src.len());
    let mut ctx = s.ctx.clone();
    let mut i = 0usize;
    // simultaneous replace, first matching pattern wins at each position;
    // an empty pattern matches once per position (Nix semantics)
    'outer: while i <= src.len() {
        for (f, t) in froms.iter().zip(tos.iter()) {
            let fb = f.s.as_bytes();
            if src[i..].starts_with(fb) {
                out.extend_from_slice(t.s.as_bytes());
                ctx = crate::eval::union_ctx(&ctx, &t.ctx);
                if fb.is_empty() {
                    if i < src.len() {
                        out.push(src[i]);
                    }
                    i += 1;
                } else {
                    i += fb.len();
                }
                continue 'outer;
            }
        }
        if i < src.len() {
            out.push(src[i]);
        }
        i += 1;
    }
    let out = String::from_utf8(out)
        .map_err(|_| type_err("replaceStrings: result is not valid UTF-8", pos))?;
    Ok(Value::Str(ShStr::with_ctx(out, ctx)))
}

fn prim_concat_strings_sep(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    let sep = force_string(ev, &args[0], pos)?;
    let l = force_list(ev, &args[1], pos)?;
    let mut out = String::new();
    let mut ctx = sep.ctx.clone();
    for (i, t) in l.iter().enumerate() {
        if i > 0 {
            out.push_str(&sep.s);
        }
        let s = force_string(ev, t, pos)?;
        out.push_str(&s.s);
        ctx = crate::eval::union_ctx(&ctx, &s.ctx); // unions contexts
    }
    Ok(Value::Str(ShStr::with_ctx(out, ctx)))
}

// ---- paths and reads (07 §2.5, purity 03 §5.2) --------------------------------

fn prim_read_file(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    let path = force_pathlike(ev, &args[0], pos)?;
    let bytes = ev
        .io
        .read_file(&path)
        .map_err(|e| err(ErrorKind::Import, format!("readFile: {e}"), pos))?;
    ev.eval_inputs.insert(format!("file:{path}")); // tracked read (03 §5.3)
    let s = String::from_utf8(bytes)
        .map_err(|_| type_err(format!("readFile: {path} is not valid UTF-8"), pos))?;
    Ok(Value::Str(ShStr::plain(s))) // context empty (07 §2.5)
}

fn prim_read_dir(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    let path = force_pathlike(ev, &args[0], pos)?;
    let entries = ev
        .io
        .read_dir(&path)
        .map_err(|e| err(ErrorKind::Import, format!("readDir: {e}"), pos))?;
    ev.eval_inputs.insert(format!("dir:{path}"));
    let mut m: AttrsMap = BTreeMap::new();
    for (name, kind) in entries {
        m.insert(name, Thunk::done(Value::Str(ShStr::plain(kind.tag()))));
    }
    Ok(Value::Attrs(Rc::new(m)))
}

fn prim_path_exists(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    let path = force_pathlike(ev, &args[0], pos)?;
    let exists = ev.io.exists(&path);
    ev.eval_inputs.insert(format!("exists:{path}={exists}")); // tracked check
    Ok(Value::Bool(exists))
}

fn base_name(s: &str) -> &str {
    match s.rfind('/') {
        Some(i) => &s[i + 1..],
        None => s,
    }
}

fn prim_base_name_of(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    match ev.force(&args[0], pos)? {
        Value::Str(s) => {
            Ok(Value::Str(ShStr::with_ctx(base_name(&s.s).to_string(), s.ctx.clone())))
        }
        Value::Path { path, .. } => Ok(Value::Str(ShStr::plain(base_name(&path).to_string()))),
        v => Err(type_err(format!("baseNameOf: expected path or string, got {}", v.type_of()), pos)),
    }
}

fn prim_dir_of(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    fn parent(s: &str) -> &str {
        match s.rfind('/') {
            Some(0) => "/",
            Some(i) => &s[..i],
            None => ".",
        }
    }
    match ev.force(&args[0], pos)? {
        // path -> path, string -> string (07 §2.5)
        Value::Str(s) => Ok(Value::Str(ShStr::with_ctx(parent(&s.s).to_string(), s.ctx.clone()))),
        Value::Path { path, .. } => Ok(Value::path_value(parent(&path).to_string())),
        v => Err(type_err(format!("dirOf: expected path or string, got {}", v.type_of()), pos)),
    }
}

// ---- introspection (07 §2.7) ---------------------------------------------------

fn is_type(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos, tag: &str) -> Result<Value> {
    let v = ev.force(&args[0], pos)?;
    Ok(Value::Bool(v.type_of() == tag))
}

fn prim_is_string(ev: &mut Evaluator, a: &[ThunkRef], p: &Pos) -> Result<Value> {
    is_type(ev, a, p, "string")
}
fn prim_is_int(ev: &mut Evaluator, a: &[ThunkRef], p: &Pos) -> Result<Value> {
    is_type(ev, a, p, "int")
}
fn prim_is_bool(ev: &mut Evaluator, a: &[ThunkRef], p: &Pos) -> Result<Value> {
    is_type(ev, a, p, "bool")
}
fn prim_is_null(ev: &mut Evaluator, a: &[ThunkRef], p: &Pos) -> Result<Value> {
    is_type(ev, a, p, "null")
}
fn prim_is_list(ev: &mut Evaluator, a: &[ThunkRef], p: &Pos) -> Result<Value> {
    is_type(ev, a, p, "list")
}
fn prim_is_attrs(ev: &mut Evaluator, a: &[ThunkRef], p: &Pos) -> Result<Value> {
    is_type(ev, a, p, "set")
}
fn prim_is_function(ev: &mut Evaluator, a: &[ThunkRef], p: &Pos) -> Result<Value> {
    is_type(ev, a, p, "lambda")
}
fn prim_is_path(ev: &mut Evaluator, a: &[ThunkRef], p: &Pos) -> Result<Value> {
    is_type(ev, a, p, "path")
}

fn prim_import(ev: &mut Evaluator, args: &[ThunkRef], pos: &Pos) -> Result<Value> {
    // import :: path | derivation | string-with-context -> value (06 §1)
    let v = ev.force(&args[0], pos)?;
    let target = match &v {
        Value::Path { path, .. } => path.to_string(),
        Value::Str(s) if !s.ctx.is_empty() => s.s.to_string(),
        Value::Str(_) => {
            return Err(type_err(
                "import: a context-free string is not an importable reference",
                pos,
            ));
        }
        Value::Attrs(m) => {
            // a derivation: its outPath is the import target; realizing it
            // mid-eval is IFD, deferred (06 §5) — the target must already
            // exist on disk
            let out = ev.force_attr_string(m, "outPath", pos)?;
            out.s.to_string()
        }
        v => return Err(type_err(format!("import: expected path, got {}", v.type_of()), pos)),
    };
    if target.starts_with(shade_cdf::STORE_PREFIX) && !ev.io.exists(&target) {
        return Err(err(
            ErrorKind::Import,
            format!(
                "import of unrealized store path {target}: import-from-derivation is not supported in v1 (docs/shade/06-imports.md §5)"
            ),
            pos,
        ));
    }
    ev.import(&target, pos)
}

// ---- table ---------------------------------------------------------------

macro_rules! prims {
    ($( $name:literal => $arity:literal $f:path ),* $(,)?) => {
        &[ $( Prim { name: $name, arity: $arity, f: $f } ),* ]
    };
}

/// Every MVP builtin. Fetchers + `derivation` live in `drv.rs`.
static PRIMS: &[Prim] = prims![
    // core
    "derivation" => 1 crate::drv::prim_derivation,
    "import" => 1 prim_import,
    "throw" => 1 prim_throw,
    "abort" => 1 prim_abort,
    "tryEval" => 1 prim_try_eval,
    "seq" => 2 prim_seq,
    "deepSeq" => 2 prim_deep_seq,
    "trace" => 2 prim_trace,
    "typeOf" => 1 prim_type_of,
    // numbers
    "add" => 2 prim_add,
    "sub" => 2 prim_sub,
    "mul" => 2 prim_mul,
    "div" => 2 prim_div,
    "lessThan" => 2 prim_less_than,
    // lists
    "length" => 1 prim_length,
    "elemAt" => 2 prim_elem_at,
    "head" => 1 prim_head,
    "tail" => 1 prim_tail,
    "map" => 2 prim_map,
    "filter" => 2 prim_filter,
    "elem" => 2 prim_elem,
    "concatLists" => 1 prim_concat_lists,
    "foldl'" => 3 prim_foldl_strict,
    "genList" => 2 prim_gen_list,
    "sort" => 2 prim_sort,
    // attrsets
    "attrNames" => 1 prim_attr_names,
    "attrValues" => 1 prim_attr_values,
    "getAttr" => 2 prim_get_attr,
    "hasAttr" => 2 prim_has_attr,
    "removeAttrs" => 2 prim_remove_attrs,
    "mapAttrs" => 2 prim_map_attrs,
    "listToAttrs" => 1 prim_list_to_attrs,
    // paths and filtering
    "path" => 1 crate::drv::prim_path,
    "filterSource" => 2 crate::drv::prim_filter_source,
    "readFile" => 1 prim_read_file,
    "readDir" => 1 prim_read_dir,
    "pathExists" => 1 prim_path_exists,
    "baseNameOf" => 1 prim_base_name_of,
    "dirOf" => 1 prim_dir_of,
    // strings
    "toString" => 1 prim_to_string,
    "substring" => 3 prim_substring,
    "stringLength" => 1 prim_string_length,
    "replaceStrings" => 3 prim_replace_strings,
    "concatStringsSep" => 2 prim_concat_strings_sep,
    // introspection
    "isString" => 1 prim_is_string,
    "isInt" => 1 prim_is_int,
    "isBool" => 1 prim_is_bool,
    "isNull" => 1 prim_is_null,
    "isList" => 1 prim_is_list,
    "isAttrs" => 1 prim_is_attrs,
    "isFunction" => 1 prim_is_function,
    "isPath" => 1 prim_is_path,
    // fetchers (05 §5)
    "fetchCratesIo" => 1 crate::drv::prim_fetch_crates_io,
    "fetchGit" => 1 crate::drv::prim_fetch_git,
];

/// The initial scope (03 §4.1): `true`, `false`, `null`, `builtins`, and
/// exactly the listed global re-exports.
pub fn initial_env() -> Env {
    let mut builtins_map: AttrsMap = BTreeMap::new();
    for p in PRIMS {
        builtins_map.insert(
            p.name.to_string(),
            Thunk::done(Value::Prim(Rc::new(PrimApp { prim: p, args: Vec::new() }))),
        );
    }
    let builtins_val = Value::Attrs(Rc::new(builtins_map.clone()));

    let mut top: AttrsMap = BTreeMap::new();
    top.insert("true".to_string(), Thunk::done(Value::Bool(true)));
    top.insert("false".to_string(), Thunk::done(Value::Bool(false)));
    top.insert("null".to_string(), Thunk::done(Value::Null));
    top.insert("builtins".to_string(), Thunk::done(builtins_val));
    // global re-exports — exactly these (03 §4.1); isNull deprecated, kept
    // for parity
    for name in [
        "import",
        "map",
        "throw",
        "abort",
        "toString",
        "derivation",
        "removeAttrs",
        "baseNameOf",
        "dirOf",
        "isNull",
    ] {
        let t = builtins_map.get(name).expect("re-export must exist").clone();
        top.insert(name.to_string(), t);
    }
    let _ = empty_ctx();
    Env::empty().push_lexical(top)
}
