//! The lazy evaluator: forcing/WHNF (03 §2), application (03 §3), scoping
//! (03 §4), equality (03 §7), coercions (04 §4). Purity holds by
//! construction: the only IO reachable from here is the `EvalIo` trait, and
//! every call is recorded as an eval input (03 §5.3).

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::rc::Rc;
use alloc::sync::Arc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cmp::Ordering;

use crate::ast::{AttrName, Attrs, BinOp, BindVal, Expr, ExprKind, ExprRef, Param, SPart};
use crate::error::{ErrorKind, EvalError, Pos, Result};
use crate::io::EvalIo;
use crate::value::{
    empty_ctx, AttrsMap, Ctx, Env, FrameKind, ShStr, Thunk, ThunkRef, ThunkState, Value,
};

/// Stack-depth resource guard (03 §1: MAY impose, reported as an eval
/// error, never as a value).
const MAX_DEPTH: usize = 1500;

pub enum ImportEntry {
    InProgress,
    Done(Value),
}

pub struct Evaluator<'io> {
    pub io: &'io dyn EvalIo,
    /// Import memo table, keyed by resolved absolute path (06 §1 step 4);
    /// the in-progress mark detects import cycles (06 §2).
    pub import_cache: BTreeMap<String, ImportEntry>,
    /// Recorded eval inputs (03 §5.3), rendered one per line.
    pub eval_inputs: BTreeSet<String>,
    /// Emitted CDFs: drvPath → canonical bytes. This is the hand-off set;
    /// writing them into the store is the store services' job (08 §2).
    pub drvs: BTreeMap<String, Rc<Vec<u8>>>,
    /// Ingestion memo: absolute path (unfiltered) → (outPath, tree hash).
    pub ingest_memo: BTreeMap<String, (String, String)>,
    /// Ambient toolchain identity passed by the driver (05 §2); evaluation
    /// itself cannot observe the environment.
    pub toolchain: Option<String>,
    initial_env: Env,
    depth: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CoerceMode {
    /// The full `${…}` / `toString` table (04 §4.1).
    Full,
    /// `string + x`: only the context-bearing types — string, path,
    /// derivation (04 §4.3, flagged decision).
    PlusRhs,
}

impl<'io> Evaluator<'io> {
    pub fn new(io: &'io dyn EvalIo) -> Self {
        let mut ev = Evaluator {
            io,
            import_cache: BTreeMap::new(),
            eval_inputs: BTreeSet::new(),
            drvs: BTreeMap::new(),
            ingest_memo: BTreeMap::new(),
            toolchain: None,
            initial_env: Env::empty(),
            depth: 0,
        };
        ev.initial_env = crate::builtins::initial_env();
        ev
    }

    pub fn initial_env(&self) -> Env {
        self.initial_env.clone()
    }

    // ---- forcing (03 §2) -------------------------------------------------

    pub fn force(&mut self, t: &ThunkRef, pos: &Pos) -> Result<Value> {
        {
            let st = t.state.borrow();
            match &*st {
                ThunkState::Done(v) => return Ok(v.clone()),
                ThunkState::Failed(e) => return Err((**e).clone()),
                ThunkState::Blackhole => {
                    return Err(EvalError::at(
                        ErrorKind::InfiniteRecursion,
                        "infinite recursion encountered",
                        pos,
                    ));
                }
                _ => {}
            }
        }
        let taken = t.state.replace(ThunkState::Blackhole);
        let r = match taken {
            ThunkState::Susp { expr, env } => self.eval(&expr, &env),
            ThunkState::Native(f) => f(self),
            _ => unreachable!("checked above"),
        };
        match r {
            Ok(v) => {
                t.state.replace(ThunkState::Done(v.clone()));
                Ok(v)
            }
            Err(e) => {
                t.state.replace(ThunkState::Failed(alloc::boxed::Box::new(e.clone())));
                Err(e)
            }
        }
    }

    /// Deep force (03 §2): only CDF serialization, `--strict`, and
    /// `builtins.deepSeq` use this.
    pub fn deep_force(&mut self, v: &Value, pos: &Pos) -> Result<()> {
        self.depth_in(pos)?;
        let r = (|| match v {
            Value::List(xs) => {
                for t in xs.iter() {
                    let ev = self.force(t, pos)?;
                    self.deep_force(&ev, pos)?;
                }
                Ok(())
            }
            Value::Attrs(m) => {
                for t in m.values() {
                    let ev = self.force(t, pos)?;
                    self.deep_force(&ev, pos)?;
                }
                Ok(())
            }
            _ => Ok(()),
        })();
        self.depth -= 1;
        r
    }

    fn depth_in(&mut self, pos: &Pos) -> Result<()> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(EvalError::at(
                ErrorKind::ResourceLimit,
                format!("evaluation depth limit ({MAX_DEPTH}) exceeded"),
                pos,
            ));
        }
        Ok(())
    }

    // ---- evaluation --------------------------------------------------

    pub fn eval(&mut self, e: &ExprRef, env: &Env) -> Result<Value> {
        self.depth_in(&e.pos)?;
        let r = self.eval_inner(e, env);
        self.depth -= 1;
        r
    }

    fn eval_inner(&mut self, e: &ExprRef, env: &Env) -> Result<Value> {
        let pos = &e.pos;
        match &e.kind {
            ExprKind::Int(i) => Ok(Value::Int(*i)),
            ExprKind::Var(name) => {
                let t = self.lookup(name, env, pos)?;
                self.force(&t, pos)
            }
            ExprKind::Str(parts) => self.eval_str(parts, env, pos),
            ExprKind::Path(p) => Ok(Value::path_value(p.as_str())),
            ExprKind::List(xs) => Ok(Value::List(Rc::new(
                xs.iter().map(|x| Thunk::susp(x.clone(), env.clone())).collect(),
            ))),
            ExprKind::Attrs(a) => self.eval_attrs(a, env, pos),
            ExprKind::Let { binds, body } => {
                let env2 = self.bind_recursive(binds, env)?;
                self.eval(body, &env2)
            }
            ExprKind::With { scope, body } => {
                let env2 = env.push_with(Thunk::susp(scope.clone(), env.clone()));
                self.eval(body, &env2)
            }
            ExprKind::If { cond, then_, else_ } => {
                if self.eval_bool(cond, env)? {
                    self.eval(then_, env)
                } else {
                    self.eval(else_, env)
                }
            }
            ExprKind::Assert { cond, body } => {
                if self.eval_bool(cond, env)? {
                    self.eval(body, env)
                } else {
                    Err(EvalError::at(ErrorKind::Assert, "assertion failed", &cond.pos))
                }
            }
            ExprKind::Lambda(def) => Ok(Value::Lambda(Rc::new(crate::value::LambdaVal {
                def: def.clone(),
                env: env.clone(),
            }))),
            ExprKind::Apply { f, arg } => {
                let fv = self.eval(f, env)?;
                let argt = Thunk::susp(arg.clone(), env.clone());
                self.apply(fv, argt, pos)
            }
            ExprKind::Select { base, path, default } => {
                let mut cur = self.eval(base, env)?;
                for (i, a) in path.iter().enumerate() {
                    let name = self.attr_name(a, env)?;
                    let found = match &cur {
                        Value::Attrs(m) => m.get(&name).cloned(),
                        _ => None,
                    };
                    match found {
                        Some(t) => cur = self.force(&t, pos)?,
                        None => {
                            if let Some(d) = default {
                                return self.eval(d, env);
                            }
                            if !matches!(cur, Value::Attrs(_)) {
                                return Err(EvalError::at(
                                    ErrorKind::Type,
                                    format!(
                                        "attempt to select `{name}` from a value of type {}",
                                        cur.type_of()
                                    ),
                                    pos,
                                ));
                            }
                            let so_far = render_attrpath_prefix(path, i, self, env)?;
                            return Err(EvalError::at(
                                ErrorKind::MissingAttr,
                                format!("attribute `{so_far}` missing"),
                                pos,
                            ));
                        }
                    }
                }
                Ok(cur)
            }
            ExprKind::HasAttr { base, path } => {
                let mut cur = self.eval(base, env)?;
                for (i, a) in path.iter().enumerate() {
                    let name = self.attr_name(a, env)?;
                    match &cur {
                        Value::Attrs(m) => match m.get(&name) {
                            Some(t) => {
                                if i + 1 == path.len() {
                                    return Ok(Value::Bool(true));
                                }
                                let t = t.clone();
                                cur = self.force(&t, pos)?;
                            }
                            None => return Ok(Value::Bool(false)),
                        },
                        _ => return Ok(Value::Bool(false)),
                    }
                }
                Ok(Value::Bool(true))
            }
            ExprKind::Not(x) => {
                let v = self.eval(x, env)?;
                match v {
                    Value::Bool(b) => Ok(Value::Bool(!b)),
                    v => Err(self.type_err(pos, "!", "bool", &v)),
                }
            }
            ExprKind::Neg(x) => {
                let v = self.eval(x, env)?;
                match v {
                    Value::Int(i) => Ok(Value::Int(i.wrapping_neg())),
                    v => Err(self.type_err(pos, "unary -", "int", &v)),
                }
            }
            ExprKind::BinOp { op, l, r } => self.eval_binop(*op, l, r, env, pos),
        }
    }

    fn eval_bool(&mut self, e: &ExprRef, env: &Env) -> Result<bool> {
        match self.eval(e, env)? {
            Value::Bool(b) => Ok(b),
            v => Err(self.type_err(&e.pos, "condition", "bool", &v)),
        }
    }

    fn type_err(&self, pos: &Pos, what: &str, want: &str, got: &Value) -> EvalError {
        EvalError::at(
            ErrorKind::Type,
            format!("{what}: expected {want}, got {}", got.type_of()),
            pos,
        )
    }

    // ---- scoping (03 §4) --------------------------------------------

    /// Name lookup: lexical frames first (inner to outer); `with` frames
    /// are weaker than any lexical binding, innermost `with` wins among
    /// `with`s (03 §4.3).
    fn lookup(&mut self, name: &str, env: &Env, pos: &Pos) -> Result<ThunkRef> {
        // pass 1: lexical only
        let mut cur = env.clone();
        while let Some(f) = cur.0 {
            if let FrameKind::Lexical(map) = &f.kind {
                if let Some(t) = map.borrow().get(name) {
                    return Ok(t.clone());
                }
            }
            cur = f.parent.clone();
        }
        // pass 2: with frames, innermost first
        let mut cur = env.clone();
        while let Some(f) = cur.0 {
            if let FrameKind::With(scope) = &f.kind {
                let scope = scope.clone();
                let v = self.force(&scope, pos)?;
                match v {
                    Value::Attrs(m) => {
                        if let Some(t) = m.get(name) {
                            return Ok(t.clone());
                        }
                    }
                    v => {
                        return Err(self.type_err(pos, "with scope", "set", &v));
                    }
                }
            }
            cur = f.parent.clone();
        }
        Err(EvalError::at(
            ErrorKind::UndefinedVar,
            format!("undefined variable `{name}`"),
            pos,
        ))
    }

    /// Build the thunk map for a non-recursive attrset literal.
    fn eval_attrs(&mut self, a: &Attrs, env: &Env, pos: &Pos) -> Result<Value> {
        if a.rec {
            let env2 = self.bind_recursive(a, env)?;
            // The frame's names are also the attrset's attributes (03 §4.2).
            if let Some(f) = &env2.0 {
                if let FrameKind::Lexical(map) = &f.kind {
                    return Ok(Value::Attrs(Rc::new(map.borrow().clone())));
                }
            }
            unreachable!("bind_recursive returns a lexical frame");
        }
        let mut map: AttrsMap = BTreeMap::new();
        for entry in &a.entries {
            let t = self.bind_thunk(entry, env, env);
            map.insert(entry.name.clone(), t);
        }
        // Dynamic attributes: names are forced when the attrset reaches
        // WHNF (the key set must exist, 03 §2 WHNF table).
        for (name_e, val_e) in &a.dynamics {
            let nv = self.eval(name_e, env)?;
            let name = match nv {
                Value::Str(s) => s.s.to_string(),
                v => {
                    return Err(self.type_err(&name_e.pos, "dynamic attribute name", "string", &v));
                }
            };
            if map.contains_key(&name) {
                return Err(EvalError::at(
                    ErrorKind::Type,
                    format!("duplicate attribute `{name}` (dynamic)"),
                    &name_e.pos,
                ));
            }
            map.insert(name, Thunk::susp(val_e.clone(), env.clone()));
        }
        let _ = pos;
        Ok(Value::Attrs(Rc::new(map)))
    }

    /// `let` / `rec { }` frame: all binds mutually recursive (03 §4.2).
    fn bind_recursive(&mut self, a: &Attrs, env: &Env) -> Result<Env> {
        debug_assert!(a.dynamics.is_empty(), "parser rejects dynamics in rec/let");
        let env2 = env.push_lexical(BTreeMap::new());
        let frame = env2.0.as_ref().unwrap();
        let FrameKind::Lexical(map) = &frame.kind else { unreachable!() };
        for entry in &a.entries {
            // `inherit x;` resolves in the enclosing scope, not the rec
            // frame (02 §3.3 / 03 §4.2); `inherit (e) x;` sees the frame.
            let outer = matches!(entry.val, BindVal::InheritPlain);
            let t = self.bind_thunk(entry, if outer { env } else { &env2 }, env);
            map.borrow_mut().insert(entry.name.clone(), t);
        }
        Ok(env2)
    }

    fn bind_thunk(&self, entry: &crate::ast::AttrEntry, env: &Env, _outer: &Env) -> ThunkRef {
        match &entry.val {
            BindVal::Expr(x) => Thunk::susp(x.clone(), env.clone()),
            BindVal::InheritPlain => Thunk::susp(
                Rc::new(Expr {
                    kind: ExprKind::Var(entry.name.clone()),
                    pos: entry.pos.clone(),
                }),
                env.clone(),
            ),
            BindVal::InheritFrom(from) => Thunk::susp(
                Rc::new(Expr {
                    kind: ExprKind::Select {
                        base: from.clone(),
                        path: alloc::vec![AttrName::Static(entry.name.clone())],
                        default: None,
                    },
                    pos: entry.pos.clone(),
                }),
                env.clone(),
            ),
        }
    }

    fn attr_name(&mut self, a: &AttrName, env: &Env) -> Result<String> {
        match a {
            AttrName::Static(s) => Ok(s.clone()),
            AttrName::Dynamic(e) => match self.eval(e, env)? {
                Value::Str(s) => Ok(s.s.to_string()),
                v => Err(self.type_err(&e.pos, "attribute name", "string", &v)),
            },
        }
    }

    // ---- application (03 §3) -----------------------------------------

    pub fn apply(&mut self, f: Value, arg: ThunkRef, pos: &Pos) -> Result<Value> {
        match f {
            Value::Lambda(l) => match &l.def.param {
                Param::Ident(name) => {
                    let mut m = BTreeMap::new();
                    m.insert(name.clone(), arg);
                    let env2 = l.env.push_lexical(m);
                    self.eval(&l.def.body, &env2)
                }
                Param::Pattern { formals, ellipsis, at } => {
                    // Step 1: force to WHNF; must be an attrset. Presence of
                    // all non-defaulted formals IS checked at application
                    // time; their values are not forced (03 §3.2, normative).
                    let argv = self.force(&arg, pos)?;
                    let Value::Attrs(am) = argv else {
                        return Err(self.type_err(pos, "function argument", "set", &argv));
                    };
                    if !ellipsis {
                        for k in am.keys() {
                            if !formals.iter().any(|f| &f.name == k) {
                                return Err(EvalError::at(
                                    ErrorKind::Type,
                                    format!("unexpected argument attribute `{k}`"),
                                    pos,
                                ));
                            }
                        }
                    }
                    for f in formals {
                        if f.default.is_none() && !am.contains_key(&f.name) {
                            return Err(EvalError::at(
                                ErrorKind::Type,
                                format!("missing required argument `{}`", f.name),
                                pos,
                            ));
                        }
                    }
                    let env2 = l.env.push_lexical(BTreeMap::new());
                    let frame = env2.0.as_ref().unwrap();
                    let FrameKind::Lexical(map) = &frame.kind else { unreachable!() };
                    {
                        let mut m = map.borrow_mut();
                        if let Some(at) = at {
                            // the original argument, including extras (03 §3.2.4)
                            m.insert(at.clone(), Thunk::done(Value::Attrs(am.clone())));
                        }
                        for f in formals {
                            let t = match am.get(&f.name) {
                                Some(t) => t.clone(),
                                None => {
                                    // default evaluated in the body's env, so
                                    // defaults may reference other formals
                                    Thunk::susp(
                                        f.default.as_ref().unwrap().clone(),
                                        env2.clone(),
                                    )
                                }
                            };
                            m.insert(f.name.clone(), t);
                        }
                    }
                    self.eval(&l.def.body, &env2)
                }
            },
            Value::Prim(p) => {
                let mut args = p.args.clone();
                args.push(arg);
                if args.len() == p.prim.arity {
                    (p.prim.f)(self, &args, pos)
                } else {
                    Ok(Value::Prim(Rc::new(crate::value::PrimApp { prim: p.prim, args })))
                }
            }
            v => Err(self.type_err(pos, "application", "function", &v)),
        }
    }

    // ---- strings and coercion (04 §4) ---------------------------------

    fn eval_str(&mut self, parts: &[SPart], env: &Env, pos: &Pos) -> Result<Value> {
        let mut bytes = String::new();
        let mut ctx: Option<BTreeSet<String>> = None;
        for p in parts {
            match p {
                SPart::Lit(s) => bytes.push_str(s),
                SPart::Interp(e) => {
                    let v = self.eval(e, env)?;
                    let s = self.coerce_to_string(&v, &e.pos, CoerceMode::Full)?;
                    bytes.push_str(&s.s);
                    if !s.ctx.is_empty() {
                        ctx.get_or_insert_with(BTreeSet::new).extend(s.ctx.iter().cloned());
                    }
                }
            }
        }
        let _ = pos;
        let ctx: Ctx = match ctx {
            Some(set) => Rc::new(set),
            None => empty_ctx(),
        };
        Ok(Value::Str(ShStr::with_ctx(bytes, ctx)))
    }

    /// The exhaustive coercion table (04 §4.1); `PlusRhs` restricts it to
    /// the context-bearing types (04 §4.3).
    pub fn coerce_to_string(&mut self, v: &Value, pos: &Pos, mode: CoerceMode) -> Result<ShStr> {
        match v {
            Value::Str(s) => Ok(s.clone()),
            Value::Path { path, store_origin } => {
                if *store_origin {
                    // store-origin paths coerce to their own string without
                    // re-ingesting (04 §4.2 step 1)
                    Ok(ShStr::plain(path.clone()))
                } else {
                    let (out_path, _tree) = crate::drv::ingest_path(self, path, None, None, pos)?;
                    let mut ctx = BTreeSet::new();
                    ctx.insert(out_path.clone());
                    Ok(ShStr::with_ctx(out_path, Rc::new(ctx)))
                }
            }
            Value::Attrs(m) => {
                if mode == CoerceMode::PlusRhs && !self.attrs_is_derivation(m, pos)? {
                    return Err(EvalError::at(
                        ErrorKind::Type,
                        "`+` right operand: expected string, path, or derivation",
                        pos,
                    ));
                }
                if let Some(f) = m.get("__toString") {
                    let f = self.force(&f.clone(), pos)?;
                    let r = self.apply(f, Thunk::done(Value::Attrs(m.clone())), pos)?;
                    match r {
                        Value::Str(s) => Ok(s),
                        v => Err(self.type_err(pos, "__toString result", "string", &v)),
                    }
                } else if let Some(t) = m.get("outPath") {
                    let v = self.force(&t.clone(), pos)?;
                    self.coerce_to_string(&v, pos, CoerceMode::Full)
                } else {
                    Err(EvalError::at(
                        ErrorKind::Type,
                        "cannot coerce a set without `__toString` or `outPath` to a string",
                        pos,
                    ))
                }
            }
            Value::Int(i) if mode == CoerceMode::Full => Ok(ShStr::plain(format!("{i}"))),
            v => Err(EvalError::at(
                ErrorKind::Type,
                format!("cannot coerce {} to a string", v.type_of()),
                pos,
            )),
        }
    }

    pub fn attrs_is_derivation(&mut self, m: &Rc<AttrsMap>, pos: &Pos) -> Result<bool> {
        match m.get("type") {
            Some(t) => match self.force(&t.clone(), pos) {
                Ok(Value::Str(s)) => Ok(&*s.s == "derivation"),
                Ok(_) => Ok(false),
                Err(e) => Err(e),
            },
            None => Ok(false),
        }
    }

    // ---- operators -----------------------------------------------------

    fn eval_binop(
        &mut self,
        op: BinOp,
        l: &ExprRef,
        r: &ExprRef,
        env: &Env,
        pos: &Pos,
    ) -> Result<Value> {
        match op {
            BinOp::And => {
                if !self.eval_bool(l, env)? {
                    return Ok(Value::Bool(false));
                }
                Ok(Value::Bool(self.eval_bool(r, env)?))
            }
            BinOp::Or => {
                if self.eval_bool(l, env)? {
                    return Ok(Value::Bool(true));
                }
                Ok(Value::Bool(self.eval_bool(r, env)?))
            }
            BinOp::Impl => {
                if !self.eval_bool(l, env)? {
                    return Ok(Value::Bool(true));
                }
                Ok(Value::Bool(self.eval_bool(r, env)?))
            }
            BinOp::Add => {
                let lv = self.eval(l, env)?;
                match lv {
                    // `+` result type follows the left operand (04 §4.3)
                    Value::Int(a) => match self.eval(r, env)? {
                        Value::Int(b) => Ok(Value::Int(a.wrapping_add(b))),
                        v => Err(self.type_err(pos, "+", "int", &v)),
                    },
                    Value::Str(a) => {
                        let rv = self.eval(r, env)?;
                        let b = self.coerce_to_string(&rv, &r.pos, CoerceMode::PlusRhs)?;
                        let mut s = String::with_capacity(a.s.len() + b.s.len());
                        s.push_str(&a.s);
                        s.push_str(&b.s);
                        let ctx = union_ctx(&a.ctx, &b.ctx);
                        Ok(Value::Str(ShStr::with_ctx(s, ctx)))
                    }
                    Value::Path { path, .. } => {
                        let rv = self.eval(r, env)?;
                        let suffix: String = match rv {
                            Value::Path { path: rp, .. } => rp.to_string(),
                            // string suffix: bytes only; a path value carries
                            // no context (04 §2.4)
                            Value::Str(s) => s.s.to_string(),
                            v => return Err(self.type_err(pos, "path +", "path or string", &v)),
                        };
                        let joined = format!("{path}{}{suffix}", if suffix.starts_with('/') { "" } else { "/" });
                        let norm = crate::parser::normalize_path(&joined);
                        Ok(Value::path_value(norm))
                    }
                    v => Err(self.type_err(pos, "+", "int, string, or path", &v)),
                }
            }
            BinOp::Sub | BinOp::Mul | BinOp::Div => {
                let a = self.eval_int(l, env)?;
                let b = self.eval_int(r, env)?;
                let v = match op {
                    BinOp::Sub => a.wrapping_sub(b),
                    BinOp::Mul => a.wrapping_mul(b),
                    BinOp::Div => {
                        if b == 0 {
                            return Err(EvalError::at(
                                ErrorKind::Type,
                                "division by zero",
                                pos,
                            ));
                        }
                        a.wrapping_div(b)
                    }
                    _ => unreachable!(),
                };
                Ok(Value::Int(v))
            }
            BinOp::Concat => {
                let lv = self.eval(l, env)?;
                let rv = self.eval(r, env)?;
                match (lv, rv) {
                    (Value::List(a), Value::List(b)) => {
                        let mut v = Vec::with_capacity(a.len() + b.len());
                        v.extend(a.iter().cloned());
                        v.extend(b.iter().cloned());
                        Ok(Value::List(Rc::new(v)))
                    }
                    (a, b) => {
                        let bad = if matches!(a, Value::List(_)) { b } else { a };
                        Err(self.type_err(pos, "++", "list", &bad))
                    }
                }
            }
            BinOp::Update => {
                let lv = self.eval(l, env)?;
                let rv = self.eval(r, env)?;
                match (lv, rv) {
                    (Value::Attrs(a), Value::Attrs(b)) => {
                        // shallow, right biased (04 §4)
                        let mut m = (*a).clone();
                        for (k, v) in b.iter() {
                            m.insert(k.clone(), v.clone());
                        }
                        Ok(Value::Attrs(Rc::new(m)))
                    }
                    (a, b) => {
                        let bad = if matches!(a, Value::Attrs(_)) { b } else { a };
                        Err(self.type_err(pos, "//", "set", &bad))
                    }
                }
            }
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                let a = self.eval(l, env)?;
                let b = self.eval(r, env)?;
                let ord = self.cmp_values(&a, &b, pos)?;
                let res = match op {
                    BinOp::Lt => ord == Ordering::Less,
                    BinOp::Le => ord != Ordering::Greater,
                    BinOp::Gt => ord == Ordering::Greater,
                    BinOp::Ge => ord != Ordering::Less,
                    _ => unreachable!(),
                };
                Ok(Value::Bool(res))
            }
            BinOp::Eq | BinOp::Ne => {
                let a = self.eval(l, env)?;
                let b = self.eval(r, env)?;
                let eq = self.eq_values(&a, &b, pos)?;
                Ok(Value::Bool(if op == BinOp::Eq { eq } else { !eq }))
            }
        }
    }

    fn eval_int(&mut self, e: &ExprRef, env: &Env) -> Result<i64> {
        match self.eval(e, env)? {
            Value::Int(i) => Ok(i),
            v => Err(self.type_err(&e.pos, "arithmetic", "int", &v)),
        }
    }

    /// Ordering (03 §7): int~int, string~string bytewise, lists
    /// lexicographic; anything else is a type error.
    pub fn cmp_values(&mut self, a: &Value, b: &Value, pos: &Pos) -> Result<Ordering> {
        match (a, b) {
            (Value::Int(x), Value::Int(y)) => Ok(x.cmp(y)),
            (Value::Str(x), Value::Str(y)) => Ok(x.s.as_bytes().cmp(y.s.as_bytes())),
            (Value::List(xs), Value::List(ys)) => {
                let n = xs.len().min(ys.len());
                for i in 0..n {
                    let xv = self.force(&xs[i], pos)?;
                    let yv = self.force(&ys[i], pos)?;
                    match self.cmp_values(&xv, &yv, pos)? {
                        Ordering::Equal => {}
                        o => return Ok(o),
                    }
                }
                Ok(xs.len().cmp(&ys.len()))
            }
            _ => Err(EvalError::at(
                ErrorKind::Type,
                format!("cannot compare {} with {}", a.type_of(), b.type_of()),
                pos,
            )),
        }
    }

    /// Deep equality (03 §7). String context ignored; path ≠ string;
    /// derivations compare by `drvPath` alone; functions are never equal.
    pub fn eq_values(&mut self, a: &Value, b: &Value, pos: &Pos) -> Result<bool> {
        match (a, b) {
            (Value::Int(x), Value::Int(y)) => Ok(x == y),
            (Value::Bool(x), Value::Bool(y)) => Ok(x == y),
            (Value::Null, Value::Null) => Ok(true),
            (Value::Str(x), Value::Str(y)) => Ok(x.s == y.s),
            (Value::Path { path: x, .. }, Value::Path { path: y, .. }) => Ok(x == y),
            (Value::List(xs), Value::List(ys)) => {
                if xs.len() != ys.len() {
                    return Ok(false);
                }
                for (x, y) in xs.iter().zip(ys.iter()) {
                    let xv = self.force(x, pos)?;
                    let yv = self.force(y, pos)?;
                    if !self.eq_values(&xv, &yv, pos)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            (Value::Attrs(x), Value::Attrs(y)) => {
                // derivation exception: compare by drvPath alone (03 §7)
                if self.attrs_is_derivation(x, pos)? && self.attrs_is_derivation(y, pos)? {
                    let dx = self.force_attr_string(x, "drvPath", pos)?;
                    let dy = self.force_attr_string(y, "drvPath", pos)?;
                    return Ok(dx.s == dy.s);
                }
                if x.len() != y.len() {
                    return Ok(false);
                }
                for ((kx, tx), (ky, ty)) in x.iter().zip(y.iter()) {
                    if kx != ky {
                        return Ok(false);
                    }
                    let xv = self.force(tx, pos)?;
                    let yv = self.force(ty, pos)?;
                    if !self.eq_values(&xv, &yv, pos)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            // functions never equal anything, including themselves —
            // `false`, kept total (03 §7, flagged TODO(open) there)
            (Value::Lambda(_), _) | (_, Value::Lambda(_)) => Ok(false),
            (Value::Prim(_), _) | (_, Value::Prim(_)) => Ok(false),
            _ => Ok(false), // different types compare false, never an error
        }
    }

    pub fn force_attr_string(
        &mut self,
        m: &Rc<AttrsMap>,
        name: &str,
        pos: &Pos,
    ) -> Result<ShStr> {
        let t = m.get(name).ok_or_else(|| {
            EvalError::at(ErrorKind::MissingAttr, format!("attribute `{name}` missing"), pos)
        })?;
        match self.force(&t.clone(), pos)? {
            Value::Str(s) => Ok(s),
            v => Err(self.type_err(pos, name, "string", &v)),
        }
    }

    // ---- import (06 §1-2) ------------------------------------------------

    pub fn import(&mut self, target: &str, pos: &Pos) -> Result<Value> {
        let mut path = crate::parser::normalize_path(target);
        // directory → fixed entry-file name (06 §1 step 2; TODO(open) there
        // confirms `default.shade`)
        if matches!(
            self.io.metadata(&path),
            Ok(crate::io::FileMeta { kind: crate::io::FileKind::Directory, .. })
        ) {
            path.push_str("/default.shade");
        }
        match self.import_cache.get(&path) {
            Some(ImportEntry::Done(v)) => return Ok(v.clone()),
            Some(ImportEntry::InProgress) => {
                return Err(EvalError::at(
                    ErrorKind::Import,
                    format!("import cycle via {path}"),
                    pos,
                ));
            }
            None => {}
        }
        self.import_cache.insert(path.clone(), ImportEntry::InProgress);
        let r = self.import_uncached(&path, pos);
        match &r {
            Ok(v) => {
                self.import_cache.insert(path.clone(), ImportEntry::Done(v.clone()));
            }
            Err(_) => {
                self.import_cache.remove(&path);
            }
        }
        r
    }

    fn import_uncached(&mut self, path: &str, pos: &Pos) -> Result<Value> {
        let bytes = self.io.read_file(path).map_err(|e| {
            EvalError::at(ErrorKind::Import, format!("cannot import {path}: {e}"), pos)
        })?;
        let src = String::from_utf8(bytes).map_err(|_| {
            EvalError::at(ErrorKind::Parse, format!("{path}: invalid UTF-8"), pos)
        })?;
        // tracked read → eval input (03 §5.3)
        self.eval_inputs.insert(format!("file:{path}"));
        let dir = match path.rfind('/') {
            Some(0) => "/",
            Some(i) => &path[..i],
            None => "/",
        };
        let expr = crate::parser::parse_str(&src, Arc::from(path), dir)?;
        // fresh scope: only the initial scope — imports are hermetic (06 §1 step 3)
        let env = self.initial_env();
        self.eval(&expr, &env)
    }
}

pub fn union_ctx(a: &Ctx, b: &Ctx) -> Ctx {
    if b.is_empty() {
        return a.clone();
    }
    if a.is_empty() {
        return b.clone();
    }
    let mut s: BTreeSet<String> = (**a).clone();
    s.extend(b.iter().cloned());
    Rc::new(s)
}

fn render_attrpath_prefix(
    path: &[AttrName],
    upto: usize,
    ev: &mut Evaluator,
    env: &Env,
) -> Result<String> {
    let mut s = String::new();
    for (i, a) in path.iter().enumerate() {
        if i > upto {
            break;
        }
        if i > 0 {
            s.push('.');
        }
        match a {
            AttrName::Static(n) => s.push_str(n),
            AttrName::Dynamic(e) => s.push_str(&ev.attr_name(&AttrName::Dynamic(e.clone()), env)?),
        }
    }
    Ok(s)
}
