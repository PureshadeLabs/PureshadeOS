//! Recursive-descent parser, a direct transcription of the syntactic
//! grammar in `docs/shade/02-grammar.md` §3 (the EBNF cascade in §3.1 is
//! followed rule-for-rule, including the non-associative levels).
//!
//! Parse-time obligations handled here: attrpath desugaring + prefix
//! merging (02 §3.3), dynamic-attr restrictions (non-`rec` only), duplicate
//! attribute/formal detection, `{` pattern-vs-attrset lookahead (02 §4.1),
//! and path resolution against the file's directory (04 §2.4).

use alloc::rc::Rc;
use alloc::sync::Arc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use alloc::{format, vec};

use crate::ast::*;
use crate::error::{ErrorKind, EvalError, Pos, Result};
use crate::lexer::{self, Kw, StrPart, Tok, Token};

pub fn parse_str(src: &str, file: Arc<str>, base_dir: &str) -> Result<ExprRef> {
    let toks = lexer::lex(src, file.clone())?;
    let mut p = Parser { toks: &toks, i: 0, file, base_dir: base_dir.to_string() };
    let e = p.parse_expr()?;
    p.expect_end()?;
    Ok(e)
}

struct Parser<'t> {
    toks: &'t [Token],
    i: usize,
    file: Arc<str>,
    base_dir: String,
}

const EOF: Tok = Tok::Eof;

impl<'t> Parser<'t> {
    fn peek(&self) -> &Tok {
        self.toks.get(self.i).map(|t| &t.tok).unwrap_or(&EOF)
    }

    fn peek_at(&self, off: usize) -> &Tok {
        self.toks.get(self.i + off).map(|t| &t.tok).unwrap_or(&EOF)
    }

    fn pos(&self) -> Pos {
        self.toks
            .get(self.i.min(self.toks.len().saturating_sub(1)))
            .map(|t| t.pos.clone())
            .unwrap_or(Pos { file: self.file.clone(), line: 0, col: 0 })
    }

    fn bump(&mut self) -> &'t Token {
        let t = &self.toks[self.i.min(self.toks.len() - 1)];
        if self.i < self.toks.len() {
            self.i += 1;
        }
        t
    }

    fn err(&self, msg: impl Into<String>) -> EvalError {
        EvalError::at(ErrorKind::Parse, msg, &self.pos())
    }

    fn expect(&mut self, tok: Tok, what: &str) -> Result<()> {
        if self.peek() == &tok {
            self.bump();
            Ok(())
        } else {
            Err(self.err(format!("expected {what}, found {:?}", self.peek())))
        }
    }

    fn expect_end(&self) -> Result<()> {
        if matches!(self.peek(), Tok::Eof) {
            Ok(())
        } else {
            Err(self.err(format!("unexpected token {:?}", self.peek())))
        }
    }

    fn mk(&self, kind: ExprKind, pos: Pos) -> ExprRef {
        Rc::new(Expr { kind, pos })
    }

    // ---- expr ----------------------------------------------------------

    fn parse_expr(&mut self) -> Result<ExprRef> {
        let pos = self.pos();
        match self.peek() {
            Tok::Kw(Kw::Assert) => {
                self.bump();
                let cond = self.parse_expr()?;
                self.expect(Tok::Semi, "`;` after assert condition")?;
                let body = self.parse_expr()?;
                Ok(self.mk(ExprKind::Assert { cond, body }, pos))
            }
            Tok::Kw(Kw::With) => {
                self.bump();
                let scope = self.parse_expr()?;
                self.expect(Tok::Semi, "`;` after with scope")?;
                let body = self.parse_expr()?;
                Ok(self.mk(ExprKind::With { scope, body }, pos))
            }
            Tok::Kw(Kw::Let) => {
                self.bump();
                let binds = self.parse_binds_until_in()?;
                let body = self.parse_expr()?;
                Ok(self.mk(ExprKind::Let { binds: Rc::new(binds), body }, pos))
            }
            Tok::Kw(Kw::If) => {
                self.bump();
                let cond = self.parse_expr()?;
                self.expect(Tok::Kw(Kw::Then), "`then`")?;
                let then_ = self.parse_expr()?;
                self.expect(Tok::Kw(Kw::Else), "`else`")?;
                let else_ = self.parse_expr()?;
                Ok(self.mk(ExprKind::If { cond, then_, else_ }, pos))
            }
            Tok::Id(_) if matches!(self.peek_at(1), Tok::Colon) => {
                let name = self.bump_id();
                self.bump(); // :
                let body = self.parse_expr()?;
                Ok(self.mk(
                    ExprKind::Lambda(Rc::new(LambdaDef {
                        param: Param::Ident(name),
                        body,
                        pos: pos.clone(),
                    })),
                    pos,
                ))
            }
            Tok::Id(_) if matches!(self.peek_at(1), Tok::At) => {
                // ID @ pattern : body
                let at = self.bump_id();
                self.bump(); // @
                let (formals, ellipsis) = self.parse_pattern_braces(Some(&at))?;
                self.expect(Tok::Colon, "`:` after pattern")?;
                let body = self.parse_expr()?;
                Ok(self.mk(
                    ExprKind::Lambda(Rc::new(LambdaDef {
                        param: Param::Pattern { formals, ellipsis, at: Some(at) },
                        body,
                        pos: pos.clone(),
                    })),
                    pos,
                ))
            }
            Tok::LBrace if self.brace_is_pattern() => {
                let (formals, ellipsis) = self.parse_pattern_braces(None)?;
                let at = if matches!(self.peek(), Tok::At) {
                    self.bump();
                    let name = self.bump_id_or_err("identifier after `@`")?;
                    // duplicate check between formals and @-binding (02 §3)
                    if formals.iter().any(|f| f.name == name) {
                        return Err(self.err(format!("duplicate formal `{name}` (also bound by `@`)")));
                    }
                    Some(name)
                } else {
                    None
                };
                self.expect(Tok::Colon, "`:` after pattern")?;
                let body = self.parse_expr()?;
                Ok(self.mk(
                    ExprKind::Lambda(Rc::new(LambdaDef {
                        param: Param::Pattern { formals, ellipsis, at },
                        body,
                        pos: pos.clone(),
                    })),
                    pos,
                ))
            }
            _ => self.parse_impl(),
        }
    }

    fn bump_id(&mut self) -> String {
        match &self.bump().tok {
            Tok::Id(s) => s.clone(),
            _ => unreachable!("caller checked Id"),
        }
    }

    fn bump_id_or_err(&mut self, what: &str) -> Result<String> {
        match self.peek() {
            Tok::Id(_) => Ok(self.bump_id()),
            _ => Err(self.err(format!("expected {what}, found {:?}", self.peek()))),
        }
    }

    /// 02 §4.1: `{` opens a pattern iff the matching `}` is followed by `:`
    /// or `@`.
    fn brace_is_pattern(&self) -> bool {
        debug_assert!(matches!(self.peek(), Tok::LBrace));
        let mut depth = 0usize;
        let mut j = self.i;
        loop {
            match self.toks.get(j).map(|t| &t.tok) {
                None => return false,
                Some(Tok::LBrace) | Some(Tok::DollarBrace) => depth += 1,
                Some(Tok::RBrace) => {
                    depth -= 1;
                    if depth == 0 {
                        return matches!(
                            self.toks.get(j + 1).map(|t| &t.tok),
                            Some(Tok::Colon) | Some(Tok::At)
                        );
                    }
                }
                Some(_) => {}
            }
            j += 1;
        }
    }

    fn parse_pattern_braces(&mut self, at_outer: Option<&str>) -> Result<(Vec<Formal>, bool)> {
        self.expect(Tok::LBrace, "`{`")?;
        let mut formals: Vec<Formal> = Vec::new();
        let mut ellipsis = false;
        loop {
            match self.peek() {
                Tok::RBrace => {
                    self.bump();
                    break;
                }
                Tok::Ellipsis => {
                    self.bump();
                    ellipsis = true;
                    self.expect(Tok::RBrace, "`}` after `...`")?;
                    break;
                }
                Tok::Id(_) => {
                    let name = self.bump_id();
                    if formals.iter().any(|f| f.name == name) || at_outer == Some(name.as_str()) {
                        return Err(self.err(format!("duplicate formal `{name}`")));
                    }
                    let default = if matches!(self.peek(), Tok::Question) {
                        self.bump();
                        Some(self.parse_expr()?)
                    } else {
                        None
                    };
                    formals.push(Formal { name, default });
                    match self.peek() {
                        Tok::Comma => {
                            self.bump();
                        }
                        Tok::RBrace => {}
                        t => return Err(self.err(format!("expected `,` or `}}` in pattern, found {t:?}"))),
                    }
                }
                t => return Err(self.err(format!("expected formal, `...` or `}}`, found {t:?}"))),
            }
        }
        Ok((formals, ellipsis))
    }

    // ---- operator cascade (02 §3.1, normative EBNF) ---------------------

    fn parse_impl(&mut self) -> Result<ExprRef> {
        let l = self.parse_or()?;
        if matches!(self.peek(), Tok::Impl) {
            let pos = self.pos();
            self.bump();
            let r = self.parse_impl()?; // right-assoc
            return Ok(self.mk(ExprKind::BinOp { op: BinOp::Impl, l, r }, pos));
        }
        Ok(l)
    }

    fn parse_or(&mut self) -> Result<ExprRef> {
        let mut l = self.parse_and()?;
        while matches!(self.peek(), Tok::Or) {
            let pos = self.pos();
            self.bump();
            let r = self.parse_and()?;
            l = self.mk(ExprKind::BinOp { op: BinOp::Or, l, r }, pos);
        }
        Ok(l)
    }

    fn parse_and(&mut self) -> Result<ExprRef> {
        let mut l = self.parse_eq()?;
        while matches!(self.peek(), Tok::And) {
            let pos = self.pos();
            self.bump();
            let r = self.parse_eq()?;
            l = self.mk(ExprKind::BinOp { op: BinOp::And, l, r }, pos);
        }
        Ok(l)
    }

    fn parse_eq(&mut self) -> Result<ExprRef> {
        let l = self.parse_rel()?;
        let op = match self.peek() {
            Tok::Eq => BinOp::Eq,
            Tok::Ne => BinOp::Ne,
            _ => return Ok(l),
        };
        let pos = self.pos();
        self.bump();
        let r = self.parse_rel()?;
        if matches!(self.peek(), Tok::Eq | Tok::Ne) {
            return Err(self.err("`==`/`!=` are non-associative"));
        }
        Ok(self.mk(ExprKind::BinOp { op, l, r }, pos))
    }

    fn parse_rel(&mut self) -> Result<ExprRef> {
        let l = self.parse_update()?;
        let op = match self.peek() {
            Tok::Lt => BinOp::Lt,
            Tok::Le => BinOp::Le,
            Tok::Gt => BinOp::Gt,
            Tok::Ge => BinOp::Ge,
            _ => return Ok(l),
        };
        let pos = self.pos();
        self.bump();
        let r = self.parse_update()?;
        if matches!(self.peek(), Tok::Lt | Tok::Le | Tok::Gt | Tok::Ge) {
            return Err(self.err("ordering operators are non-associative"));
        }
        Ok(self.mk(ExprKind::BinOp { op, l, r }, pos))
    }

    fn parse_update(&mut self) -> Result<ExprRef> {
        let l = self.parse_not()?;
        if matches!(self.peek(), Tok::Update) {
            let pos = self.pos();
            self.bump();
            let r = self.parse_update()?; // right-assoc
            return Ok(self.mk(ExprKind::BinOp { op: BinOp::Update, l, r }, pos));
        }
        Ok(l)
    }

    fn parse_not(&mut self) -> Result<ExprRef> {
        if matches!(self.peek(), Tok::Not) {
            let pos = self.pos();
            self.bump();
            let e = self.parse_not()?;
            return Ok(self.mk(ExprKind::Not(e), pos));
        }
        self.parse_add()
    }

    fn parse_add(&mut self) -> Result<ExprRef> {
        let mut l = self.parse_mul()?;
        loop {
            let op = match self.peek() {
                Tok::Plus => BinOp::Add,
                Tok::Minus => BinOp::Sub,
                _ => return Ok(l),
            };
            let pos = self.pos();
            self.bump();
            let r = self.parse_mul()?;
            l = self.mk(ExprKind::BinOp { op, l, r }, pos);
        }
    }

    fn parse_mul(&mut self) -> Result<ExprRef> {
        let mut l = self.parse_concat()?;
        loop {
            let op = match self.peek() {
                Tok::Star => BinOp::Mul,
                Tok::Slash => BinOp::Div,
                _ => return Ok(l),
            };
            let pos = self.pos();
            self.bump();
            let r = self.parse_concat()?;
            l = self.mk(ExprKind::BinOp { op, l, r }, pos);
        }
    }

    fn parse_concat(&mut self) -> Result<ExprRef> {
        let l = self.parse_hasattr()?;
        if matches!(self.peek(), Tok::Concat) {
            let pos = self.pos();
            self.bump();
            let r = self.parse_concat()?; // right-assoc
            return Ok(self.mk(ExprKind::BinOp { op: BinOp::Concat, l, r }, pos));
        }
        Ok(l)
    }

    fn parse_hasattr(&mut self) -> Result<ExprRef> {
        let l = self.parse_neg()?;
        if matches!(self.peek(), Tok::Question) {
            let pos = self.pos();
            self.bump();
            let path = self.parse_attrpath()?;
            if matches!(self.peek(), Tok::Question) {
                return Err(self.err("`?` is non-associative"));
            }
            return Ok(self.mk(ExprKind::HasAttr { base: l, path }, pos));
        }
        Ok(l)
    }

    fn parse_neg(&mut self) -> Result<ExprRef> {
        if matches!(self.peek(), Tok::Minus) {
            let pos = self.pos();
            self.bump();
            let e = self.parse_neg()?;
            return Ok(self.mk(ExprKind::Neg(e), pos));
        }
        self.parse_app()
    }

    fn starts_select(&self) -> bool {
        matches!(
            self.peek(),
            Tok::Id(_)
                | Tok::Int(_)
                | Tok::Str(_)
                | Tok::IndStr(_)
                | Tok::Path(_)
                | Tok::LParen
                | Tok::LBracket
                | Tok::LBrace
                | Tok::Kw(Kw::Rec)
        )
    }

    fn parse_app(&mut self) -> Result<ExprRef> {
        let mut f = self.parse_select()?;
        // application by juxtaposition; keywords terminate it (02 §4.2)
        while self.starts_select() {
            let pos = self.pos();
            let arg = self.parse_select()?;
            f = self.mk(ExprKind::Apply { f, arg }, pos);
        }
        Ok(f)
    }

    fn parse_select(&mut self) -> Result<ExprRef> {
        let base = self.parse_simple()?;
        if matches!(self.peek(), Tok::Dot) {
            let pos = self.pos();
            self.bump();
            let path = self.parse_attrpath()?;
            let default = if matches!(self.peek(), Tok::Kw(Kw::Or)) {
                self.bump();
                Some(self.parse_select()?)
            } else {
                None
            };
            return Ok(self.mk(ExprKind::Select { base, path, default }, pos));
        }
        Ok(base)
    }

    fn parse_attrpath(&mut self) -> Result<Vec<AttrName>> {
        let mut path = vec![self.parse_attr()?];
        while matches!(self.peek(), Tok::Dot) {
            self.bump();
            path.push(self.parse_attr()?);
        }
        Ok(path)
    }

    fn parse_attr(&mut self) -> Result<AttrName> {
        let pos = self.pos();
        match self.peek().clone() {
            Tok::Id(_) => Ok(AttrName::Static(self.bump_id())),
            Tok::Str(parts) => {
                self.bump();
                // Interpolation-free strings are static names; interpolated
                // ones evaluate like `${…}` attrs (02 §3.3).
                if parts.iter().all(|p| matches!(p, StrPart::Lit(_))) {
                    let mut s = String::new();
                    for p in &parts {
                        if let StrPart::Lit(t) = p {
                            s.push_str(t);
                        }
                    }
                    Ok(AttrName::Static(s))
                } else {
                    Ok(AttrName::Dynamic(self.str_expr(parts, pos)?))
                }
            }
            Tok::DollarBrace => {
                self.bump();
                let toks = self.collect_brace_tokens()?;
                let e = self.sub_parse(&toks)?;
                Ok(AttrName::Dynamic(e))
            }
            t => Err(self.err(format!("expected attribute name, found {t:?}"))),
        }
    }

    /// Consume tokens after a `DollarBrace` up to the matching `RBrace`.
    fn collect_brace_tokens(&mut self) -> Result<Vec<Token>> {
        let mut depth = 0usize;
        let mut toks = Vec::new();
        loop {
            match self.peek() {
                Tok::Eof => return Err(self.err("unterminated `${`")),
                Tok::LBrace | Tok::DollarBrace => {
                    depth += 1;
                    toks.push(self.bump().clone());
                }
                Tok::RBrace => {
                    if depth == 0 {
                        self.bump();
                        return Ok(toks);
                    }
                    depth -= 1;
                    toks.push(self.bump().clone());
                }
                _ => toks.push(self.bump().clone()),
            }
        }
    }

    fn sub_parse(&self, toks: &[Token]) -> Result<ExprRef> {
        let mut p = Parser {
            toks,
            i: 0,
            file: self.file.clone(),
            base_dir: self.base_dir.clone(),
        };
        let e = p.parse_expr()?;
        p.expect_end()?;
        Ok(e)
    }

    fn str_expr(&self, parts: Vec<StrPart>, pos: Pos) -> Result<ExprRef> {
        let mut out = Vec::with_capacity(parts.len());
        for p in parts {
            match p {
                StrPart::Lit(s) => out.push(SPart::Lit(s)),
                StrPart::Interp(toks) => out.push(SPart::Interp(self.sub_parse(&toks)?)),
            }
        }
        Ok(self.mk(ExprKind::Str(out), pos))
    }

    fn parse_simple(&mut self) -> Result<ExprRef> {
        let pos = self.pos();
        match self.peek().clone() {
            Tok::Id(_) => {
                let n = self.bump_id();
                Ok(self.mk(ExprKind::Var(n), pos))
            }
            Tok::Int(i) => {
                self.bump();
                Ok(self.mk(ExprKind::Int(i), pos))
            }
            Tok::Str(parts) => {
                self.bump();
                self.str_expr(parts, pos)
            }
            Tok::IndStr(parts) => {
                self.bump();
                self.str_expr(parts, pos)
            }
            Tok::Path(text) => {
                self.bump();
                let abs = self.resolve_path(&text);
                Ok(self.mk(ExprKind::Path(abs), pos))
            }
            Tok::LParen => {
                self.bump();
                let e = self.parse_expr()?;
                self.expect(Tok::RParen, "`)`")?;
                Ok(e)
            }
            Tok::LBracket => {
                self.bump();
                let mut xs = Vec::new();
                while !matches!(self.peek(), Tok::RBracket) {
                    if matches!(self.peek(), Tok::Eof) {
                        return Err(self.err("unterminated list"));
                    }
                    // list elements bind at select level (02 §3.2)
                    xs.push(self.parse_select()?);
                }
                self.bump();
                Ok(self.mk(ExprKind::List(xs), pos))
            }
            Tok::LBrace => {
                self.bump();
                let attrs = self.parse_attrset_body(false)?;
                Ok(self.mk(ExprKind::Attrs(Rc::new(attrs)), pos))
            }
            Tok::Kw(Kw::Rec) => {
                self.bump();
                self.expect(Tok::LBrace, "`{` after `rec`")?;
                let attrs = self.parse_attrset_body(true)?;
                Ok(self.mk(ExprKind::Attrs(Rc::new(attrs)), pos))
            }
            t => Err(self.err(format!("unexpected token {t:?}"))),
        }
    }

    // ---- attrsets and binds ---------------------------------------------

    fn parse_attrset_body(&mut self, rec: bool) -> Result<Attrs> {
        let mut b = AttrsBuilder::new(rec, /*dynamics_allowed=*/ !rec);
        loop {
            match self.peek() {
                Tok::RBrace => {
                    self.bump();
                    break;
                }
                Tok::Eof => return Err(self.err("unterminated attrset")),
                _ => self.parse_bind(&mut b)?,
            }
        }
        b.finish()
    }

    fn parse_binds_until_in(&mut self) -> Result<Attrs> {
        // `let` binds: same bind rule, dynamic attrs forbidden (02 §3.4),
        // at least one bind (binds = 1*bind).
        let mut b = AttrsBuilder::new(/*rec=*/ true, /*dynamics_allowed=*/ false);
        let mut count = 0usize;
        loop {
            match self.peek() {
                Tok::Kw(Kw::In) => {
                    self.bump();
                    break;
                }
                Tok::Eof => return Err(self.err("expected `in`")),
                _ => {
                    self.parse_bind(&mut b)?;
                    count += 1;
                }
            }
        }
        if count == 0 {
            return Err(self.err("`let` requires at least one binding"));
        }
        b.finish()
    }

    fn parse_bind(&mut self, b: &mut AttrsBuilder) -> Result<()> {
        if matches!(self.peek(), Tok::Kw(Kw::Inherit)) {
            self.bump();
            let from = if matches!(self.peek(), Tok::LParen) {
                self.bump();
                let e = self.parse_expr()?;
                self.expect(Tok::RParen, "`)`")?;
                Some(e)
            } else {
                None
            };
            loop {
                match self.peek() {
                    Tok::Semi => {
                        self.bump();
                        break;
                    }
                    Tok::Id(_) => {
                        let pos = self.pos();
                        let name = self.bump_id();
                        let val = match &from {
                            Some(e) => BindVal::InheritFrom(e.clone()),
                            None => BindVal::InheritPlain,
                        };
                        b.insert_leaf(name, pos, val, self)?;
                    }
                    t => {
                        return Err(self.err(format!(
                            "expected identifier or `;` in inherit, found {t:?}"
                        )));
                    }
                }
            }
            return Ok(());
        }
        let pos = self.pos();
        let path = self.parse_attrpath()?;
        self.expect(Tok::Assign, "`=`")?;
        let val = self.parse_expr()?;
        self.expect(Tok::Semi, "`;` after binding")?;
        b.insert_path(&path, pos, val, self)
    }

    fn resolve_path(&self, text: &str) -> String {
        let joined = if text.starts_with('/') {
            text.to_string()
        } else {
            format!("{}/{}", self.base_dir, text)
        };
        normalize_path(&joined)
    }
}

/// Syntactic path normalization (04 §2.4): `.` dropped, `..` resolved
/// without following symlinks, trailing slash removed.
pub fn normalize_path(p: &str) -> String {
    let mut stack: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            s => stack.push(s),
        }
    }
    let mut out = String::from("/");
    out.push_str(&stack.join("/"));
    out
}

// ---- attrpath desugar + merge (02 §3.3) ---------------------------------

struct AttrsBuilder {
    rec: bool,
    dynamics_allowed: bool,
    entries: Vec<(String, Pos, BNode)>,
    dynamics: Vec<(ExprRef, ExprRef)>,
}

enum BNode {
    Leaf(BindVal),
    Tree(AttrsBuilder),
}

impl AttrsBuilder {
    fn new(rec: bool, dynamics_allowed: bool) -> Self {
        AttrsBuilder { rec, dynamics_allowed, entries: Vec::new(), dynamics: Vec::new() }
    }

    fn insert_leaf(&mut self, name: String, pos: Pos, val: BindVal, p: &Parser) -> Result<()> {
        if self.entries.iter().any(|(n, _, _)| n == &name) {
            return Err(EvalError::at(
                ErrorKind::Parse,
                format!("duplicate attribute `{name}`"),
                &pos,
            ));
        }
        let _ = p;
        self.entries.push((name, pos, BNode::Leaf(val)));
        Ok(())
    }

    fn insert_path(
        &mut self,
        path: &[AttrName],
        pos: Pos,
        val: ExprRef,
        p: &Parser,
    ) -> Result<()> {
        match &path[0] {
            AttrName::Dynamic(name_expr) => {
                // Dynamic attributes: non-`rec` attrsets only (02 §3.3).
                if !self.dynamics_allowed {
                    return Err(EvalError::at(
                        ErrorKind::Parse,
                        "dynamic attributes are not allowed in `rec` attrsets or `let`",
                        &pos,
                    ));
                }
                let value = nest_rest(&path[1..], val, &pos);
                self.dynamics.push((name_expr.clone(), value));
                Ok(())
            }
            AttrName::Static(name) => {
                if path.len() == 1 {
                    // Whole-value bind. Merging an attrset literal into an
                    // existing nested tree (or vice versa) is permitted when
                    // the literal is a plain non-rec attrset (02 §3.3).
                    if let Some(idx) = self.entries.iter().position(|(n, _, _)| n == name) {
                        let dup = || {
                            EvalError::at(
                                ErrorKind::Parse,
                                format!("duplicate attribute `{name}`"),
                                &pos,
                            )
                        };
                        let (_, _, node) = &mut self.entries[idx];
                        match node {
                            BNode::Tree(t) => {
                                if let ExprKind::Attrs(a) = &val.kind {
                                    if !a.rec {
                                        return t.merge_literal(a, &pos);
                                    }
                                }
                                Err(dup())
                            }
                            BNode::Leaf(_) => Err(dup()),
                        }
                    } else {
                        self.entries.push((name.clone(), pos, BNode::Leaf(BindVal::Expr(val))));
                        Ok(())
                    }
                } else {
                    let idx = match self.entries.iter().position(|(n, _, _)| n == name) {
                        Some(i) => i,
                        None => {
                            self.entries.push((
                                name.clone(),
                                pos.clone(),
                                // nested trees behave like plain attrset
                                // literals: non-rec, dynamics allowed only
                                // if the host allows them
                                BNode::Tree(AttrsBuilder::new(false, self.dynamics_allowed)),
                            ));
                            self.entries.len() - 1
                        }
                    };
                    let (_, _, node) = &mut self.entries[idx];
                    match node {
                        BNode::Tree(t) => t.insert_path(&path[1..], pos, val, p),
                        BNode::Leaf(BindVal::Expr(e)) => {
                            // Shared level bound to an expression: merge only
                            // if it is a plain non-rec attrset literal.
                            if let ExprKind::Attrs(a) = &e.kind {
                                if !a.rec {
                                    let mut t =
                                        AttrsBuilder::from_literal(a, self.dynamics_allowed, &pos)?;
                                    t.insert_path(&path[1..], pos.clone(), val, p)?;
                                    *node = BNode::Tree(t);
                                    return Ok(());
                                }
                            }
                            Err(EvalError::at(
                                ErrorKind::Parse,
                                format!("duplicate attribute `{name}` (bound to a non-attrset expression)"),
                                &pos,
                            ))
                        }
                        BNode::Leaf(_) => Err(EvalError::at(
                            ErrorKind::Parse,
                            format!("duplicate attribute `{name}` (inherited)"),
                            &pos,
                        )),
                    }
                }
            }
        }
    }

    fn from_literal(a: &Attrs, dynamics_allowed: bool, pos: &Pos) -> Result<AttrsBuilder> {
        let mut t = AttrsBuilder::new(false, dynamics_allowed);
        for e in &a.entries {
            if t.entries.iter().any(|(n, _, _)| n == &e.name) {
                return Err(EvalError::at(
                    ErrorKind::Parse,
                    format!("duplicate attribute `{}`", e.name),
                    pos,
                ));
            }
            t.entries.push((e.name.clone(), e.pos.clone(), BNode::Leaf(e.val.clone())));
        }
        for (n, v) in &a.dynamics {
            t.dynamics.push((n.clone(), v.clone()));
        }
        Ok(t)
    }

    fn merge_literal(&mut self, a: &Attrs, pos: &Pos) -> Result<()> {
        for e in &a.entries {
            if self.entries.iter().any(|(n, _, _)| n == &e.name) {
                return Err(EvalError::at(
                    ErrorKind::Parse,
                    format!("duplicate attribute `{}`", e.name),
                    pos,
                ));
            }
            self.entries.push((e.name.clone(), e.pos.clone(), BNode::Leaf(e.val.clone())));
        }
        for (n, v) in &a.dynamics {
            self.dynamics.push((n.clone(), v.clone()));
        }
        Ok(())
    }

    fn finish(self) -> Result<Attrs> {
        let mut entries = Vec::with_capacity(self.entries.len());
        for (name, pos, node) in self.entries {
            let val = match node {
                BNode::Leaf(v) => v,
                BNode::Tree(t) => {
                    let inner = t.finish()?;
                    BindVal::Expr(Rc::new(Expr {
                        kind: ExprKind::Attrs(Rc::new(inner)),
                        pos: pos.clone(),
                    }))
                }
            };
            entries.push(AttrEntry { name, pos, val });
        }
        Ok(Attrs { rec: self.rec, entries, dynamics: self.dynamics })
    }
}

/// Desugar the remainder of an attrpath after a dynamic head into nested
/// attrset literals.
fn nest_rest(rest: &[AttrName], val: ExprRef, pos: &Pos) -> ExprRef {
    if rest.is_empty() {
        return val;
    }
    let inner = nest_rest(&rest[1..], val, pos);
    let attrs = match &rest[0] {
        AttrName::Static(n) => Attrs {
            rec: false,
            entries: vec![AttrEntry {
                name: n.clone(),
                pos: pos.clone(),
                val: BindVal::Expr(inner),
            }],
            dynamics: Vec::new(),
        },
        AttrName::Dynamic(e) => Attrs {
            rec: false,
            entries: Vec::new(),
            dynamics: vec![(e.clone(), inner)],
        },
    };
    Rc::new(Expr { kind: ExprKind::Attrs(Rc::new(attrs)), pos: pos.clone() })
}
