//! Value model per `docs/shade/04-values.md` §1 and the thunk machinery per
//! `docs/shade/03-semantics.md` §2. Nine value types; derivations are
//! attrsets with the `type = "derivation"` marker, so they have no variant.

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use crate::ast::{ExprRef, LambdaDef};
use crate::error::{Pos, Result};
use crate::eval::Evaluator;

/// String context (04 §5): the hidden set of store paths (derivation
/// outputs / ingested sources) the string's bytes depend on.
pub type Ctx = Rc<BTreeSet<String>>;

pub fn empty_ctx() -> Ctx {
    Rc::new(BTreeSet::new())
}

#[derive(Clone)]
pub struct ShStr {
    pub s: Rc<str>,
    pub ctx: Ctx,
}

impl ShStr {
    pub fn plain(s: impl Into<Rc<str>>) -> Self {
        ShStr { s: s.into(), ctx: empty_ctx() }
    }

    pub fn with_ctx(s: impl Into<Rc<str>>, ctx: Ctx) -> Self {
        ShStr { s: s.into(), ctx }
    }
}

pub type AttrsMap = BTreeMap<String, ThunkRef>;

#[derive(Clone)]
pub enum Value {
    Int(i64),
    Bool(bool),
    Null,
    Str(ShStr),
    Path {
        /// Absolute, normalized (04 §2.4).
        path: Rc<str>,
        /// Originated inside `/shade/store/` — coerces without re-ingestion.
        store_origin: bool,
    },
    List(Rc<Vec<ThunkRef>>),
    Attrs(Rc<AttrsMap>),
    Lambda(Rc<LambdaVal>),
    Prim(Rc<PrimApp>),
}

impl Value {
    /// `builtins.typeOf` tag (04 §1). Derivations report `"set"`.
    pub fn type_of(&self) -> &'static str {
        match self {
            Value::Int(_) => "int",
            Value::Bool(_) => "bool",
            Value::Null => "null",
            Value::Str(_) => "string",
            Value::Path { .. } => "path",
            Value::List(_) => "list",
            Value::Attrs(_) => "set",
            Value::Lambda(_) | Value::Prim(_) => "lambda",
        }
    }

    pub fn path_value(path: impl Into<Rc<str>>) -> Value {
        let p: Rc<str> = path.into();
        let store_origin = p.starts_with(shade_cdf::STORE_PREFIX);
        Value::Path { path: p, store_origin }
    }
}

pub struct LambdaVal {
    pub def: Rc<LambdaDef>,
    pub env: Env,
}

/// A builtin, possibly partially applied (builtins are curried, 07 §1).
pub struct PrimApp {
    pub prim: &'static Prim,
    pub args: Vec<ThunkRef>,
}

pub struct Prim {
    pub name: &'static str,
    pub arity: usize,
    pub f: fn(&mut Evaluator<'_>, &[ThunkRef], &Pos) -> Result<Value>,
}

// ---- thunks (03 §2) ------------------------------------------------------

pub type NativeFn = Box<dyn FnOnce(&mut Evaluator<'_>) -> Result<Value>>;

pub enum ThunkState {
    Susp { expr: ExprRef, env: Env },
    Native(NativeFn),
    /// Blackhole mark: set on entry, cleared on completion — a thunk caught
    /// forcing itself is a deterministic infinite-recursion error (03 §2).
    Blackhole,
    Done(Value),
    /// A force that failed. Memoized like success (evaluation is pure, the
    /// error is deterministic); re-forcing after a caught `tryEval` must
    /// not turn into a bogus blackhole hit.
    Failed(alloc::boxed::Box<crate::error::EvalError>),
}

pub struct Thunk {
    pub state: RefCell<ThunkState>,
}

pub type ThunkRef = Rc<Thunk>;

impl Thunk {
    pub fn susp(expr: ExprRef, env: Env) -> ThunkRef {
        Rc::new(Thunk { state: RefCell::new(ThunkState::Susp { expr, env }) })
    }

    pub fn native(f: NativeFn) -> ThunkRef {
        Rc::new(Thunk { state: RefCell::new(ThunkState::Native(f)) })
    }

    pub fn done(v: Value) -> ThunkRef {
        Rc::new(Thunk { state: RefCell::new(ThunkState::Done(v)) })
    }
}

// ---- environments (03 §4) ------------------------------------------------

#[derive(Clone)]
pub struct Env(pub Option<Rc<Frame>>);

pub struct Frame {
    pub parent: Env,
    pub kind: FrameKind,
}

pub enum FrameKind {
    /// `let` / lambda / `rec` bindings. RefCell because recursive frames
    /// are created empty and filled with thunks that close over the frame.
    Lexical(RefCell<AttrsMap>),
    /// `with e;` — attributes become a scope weaker than any lexical
    /// binding (03 §4.3). Forced to an attrset on first consultation.
    With(ThunkRef),
}

impl Env {
    pub fn empty() -> Env {
        Env(None)
    }

    pub fn push_lexical(&self, map: AttrsMap) -> Env {
        Env(Some(Rc::new(Frame {
            parent: self.clone(),
            kind: FrameKind::Lexical(RefCell::new(map)),
        })))
    }

    pub fn push_with(&self, scope: ThunkRef) -> Env {
        Env(Some(Rc::new(Frame { parent: self.clone(), kind: FrameKind::With(scope) })))
    }
}

/// Escape a string for display, Nix-style: `$` is escaped only where it
/// would start an interpolation (`${`).
pub fn quote_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '$' if chars.peek() == Some(&'{') => out.push_str("\\$"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
