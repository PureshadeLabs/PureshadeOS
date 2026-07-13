//! AST per `docs/shade/02-grammar.md` §3. Nested attrpath binds are already
//! desugared (02 §3.3) by the parser; dynamic-attr restrictions and
//! duplicate detection are parse-time and do not appear here.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use crate::error::Pos;

#[derive(Debug, Clone)]
pub struct Expr {
    pub kind: ExprKind,
    pub pos: Pos,
}

pub type ExprRef = Rc<Expr>;

#[derive(Debug, Clone)]
pub enum ExprKind {
    Var(String),
    Int(i64),
    /// String literal: literal/interp parts (both quoted and indented forms
    /// reach here post-stripping).
    Str(Vec<SPart>),
    /// Absolute, normalized path (resolved at parse time, 04 §2.4).
    Path(String),
    List(Vec<ExprRef>),
    Attrs(Rc<Attrs>),
    Select {
        base: ExprRef,
        path: Vec<AttrName>,
        /// `e.a.b or d`
        default: Option<ExprRef>,
    },
    HasAttr {
        base: ExprRef,
        path: Vec<AttrName>,
    },
    Apply {
        f: ExprRef,
        arg: ExprRef,
    },
    Lambda(Rc<LambdaDef>),
    Let {
        binds: Rc<Attrs>, // rec by definition; dynamics forbidden at parse
        body: ExprRef,
    },
    With {
        scope: ExprRef,
        body: ExprRef,
    },
    If {
        cond: ExprRef,
        then_: ExprRef,
        else_: ExprRef,
    },
    Assert {
        cond: ExprRef,
        body: ExprRef,
    },
    BinOp {
        op: BinOp,
        l: ExprRef,
        r: ExprRef,
    },
    Neg(ExprRef),
    Not(ExprRef),
}

#[derive(Debug, Clone)]
pub enum SPart {
    Lit(String),
    Interp(ExprRef),
}

#[derive(Debug, Clone)]
pub enum AttrName {
    Static(String),
    Dynamic(ExprRef),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Concat, // ++
    Update, // //
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    And,
    Or,
    Impl,
}

#[derive(Debug, Clone)]
pub struct Attrs {
    pub rec: bool,
    pub entries: Vec<AttrEntry>,
    /// Dynamic binds (non-`rec` attrsets only, 02 §3.3): name-expr → value.
    pub dynamics: Vec<(ExprRef, ExprRef)>,
}

#[derive(Debug, Clone)]
pub struct AttrEntry {
    pub name: String,
    pub pos: Pos,
    pub val: BindVal,
}

#[derive(Debug, Clone)]
pub enum BindVal {
    Expr(ExprRef),
    /// `inherit x;` — resolves `x` in the scope *enclosing* the rec frame
    /// (otherwise `rec { inherit x; }` would be `x = x`, infinite recursion).
    InheritPlain,
    /// `inherit (e) x;` — `e` is shared among the names of one inherit.
    InheritFrom(ExprRef),
}

#[derive(Debug, Clone)]
pub struct LambdaDef {
    pub param: Param,
    pub body: ExprRef,
    pub pos: Pos,
}

#[derive(Debug, Clone)]
pub enum Param {
    Ident(String),
    Pattern {
        formals: Vec<Formal>,
        ellipsis: bool,
        at: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub struct Formal {
    pub name: String,
    pub default: Option<ExprRef>,
}

/// Compact s-expression printer for golden AST tests.
pub fn to_sexpr(e: &Expr) -> String {
    use alloc::format;
    use ExprKind::*;
    match &e.kind {
        Var(n) => format!("(var {n})"),
        Int(i) => format!("(int {i})"),
        Str(parts) => {
            let mut s = String::from("(str");
            for p in parts {
                match p {
                    SPart::Lit(t) => s.push_str(&format!(" {t:?}")),
                    SPart::Interp(x) => s.push_str(&format!(" (interp {})", to_sexpr(x))),
                }
            }
            s.push(')');
            s
        }
        Path(p) => format!("(path {p})"),
        List(xs) => {
            let mut s = String::from("(list");
            for x in xs {
                s.push(' ');
                s.push_str(&to_sexpr(x));
            }
            s.push(')');
            s
        }
        Attrs(a) => attrs_sexpr(a),
        Select { base, path, default } => {
            let mut s = format!("(select {} {}", to_sexpr(base), path_sexpr(path));
            if let Some(d) = default {
                s.push_str(&format!(" or {}", to_sexpr(d)));
            }
            s.push(')');
            s
        }
        HasAttr { base, path } => format!("(hasattr {} {})", to_sexpr(base), path_sexpr(path)),
        Apply { f, arg } => format!("(apply {} {})", to_sexpr(f), to_sexpr(arg)),
        Lambda(l) => {
            let p = match &l.param {
                Param::Ident(n) => format!("{n}"),
                Param::Pattern { formals, ellipsis, at } => {
                    let mut s = String::from("{");
                    for (i, f) in formals.iter().enumerate() {
                        if i > 0 {
                            s.push_str(", ");
                        }
                        s.push_str(&f.name);
                        if let Some(d) = &f.default {
                            s.push_str(&format!(" ? {}", to_sexpr(d)));
                        }
                    }
                    if *ellipsis {
                        if !formals.is_empty() {
                            s.push_str(", ");
                        }
                        s.push_str("...");
                    }
                    s.push('}');
                    if let Some(a) = at {
                        s.push_str(&format!(" @ {a}"));
                    }
                    s
                }
            };
            format!("(lambda {p} {})", to_sexpr(&l.body))
        }
        Let { binds, body } => format!("(let {} {})", attrs_sexpr(binds), to_sexpr(body)),
        With { scope, body } => format!("(with {} {})", to_sexpr(scope), to_sexpr(body)),
        If { cond, then_, else_ } => {
            format!("(if {} {} {})", to_sexpr(cond), to_sexpr(then_), to_sexpr(else_))
        }
        Assert { cond, body } => format!("(assert {} {})", to_sexpr(cond), to_sexpr(body)),
        BinOp { op, l, r } => format!("({} {} {})", op_name(*op), to_sexpr(l), to_sexpr(r)),
        Neg(x) => format!("(neg {})", to_sexpr(x)),
        Not(x) => format!("(not {})", to_sexpr(x)),
    }
}

fn path_sexpr(path: &[AttrName]) -> String {
    use alloc::format;
    let mut s = String::from("[");
    for (i, a) in path.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        match a {
            AttrName::Static(n) => s.push_str(n),
            AttrName::Dynamic(e) => s.push_str(&format!("(dyn {})", to_sexpr(e))),
        }
    }
    s.push(']');
    s
}

fn attrs_sexpr(a: &Attrs) -> String {
    use alloc::format;
    let mut s = String::from(if a.rec { "(rec-attrs" } else { "(attrs" });
    for e in &a.entries {
        match &e.val {
            BindVal::Expr(x) => s.push_str(&format!(" ({} = {})", e.name, to_sexpr(x))),
            BindVal::InheritPlain => s.push_str(&format!(" (inherit {})", e.name)),
            BindVal::InheritFrom(f) => {
                s.push_str(&format!(" (inherit-from {} {})", to_sexpr(f), e.name))
            }
        }
    }
    for (n, v) in &a.dynamics {
        s.push_str(&format!(" (dyn {} = {})", to_sexpr(n), to_sexpr(v)));
    }
    s.push(')');
    s
}

fn op_name(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Concat => "++",
        BinOp::Update => "//",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::And => "&&",
        BinOp::Or => "||",
        BinOp::Impl => "->",
    }
}
