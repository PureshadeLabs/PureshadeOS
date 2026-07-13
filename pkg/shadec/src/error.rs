//! Error model per `docs/shade/03-semantics.md` §8: errors are not values;
//! every error carries a kind, a message, a source position, and a forcing
//! trace. `tryEval` catches exactly throw/assert/type; `abort` escapes it
//! (03 §8 + 07 §2.1, aligned 2026-07-06 — the old 03 §8 prose that listed
//! `abort` as catchable was a spec typo, fixed in the docs).

use alloc::sync::Arc;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pos {
    pub file: Arc<str>,
    pub line: u32,
    pub col: u32,
}

impl fmt::Display for Pos {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.file, self.line, self.col)
    }
}

/// Error kinds, one per row of the 03 §8 table (plus Parse for the lexer /
/// parser, which the table folds under eval errors' source trace).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Parse,
    Type,
    UndefinedVar,
    MissingAttr,
    Assert,
    Throw,
    Abort,
    InfiniteRecursion,
    ResourceLimit,
    Purity,
    Import,
}

impl ErrorKind {
    /// Catchable by `builtins.tryEval` (07 §2.1): throw / assert / type.
    pub fn catchable(self) -> bool {
        matches!(self, ErrorKind::Throw | ErrorKind::Assert | ErrorKind::Type)
    }

    pub fn label(self) -> &'static str {
        match self {
            ErrorKind::Parse => "parse error",
            ErrorKind::Type => "type error",
            ErrorKind::UndefinedVar => "undefined variable",
            ErrorKind::MissingAttr => "missing attribute",
            ErrorKind::Assert => "assertion failure",
            ErrorKind::Throw => "throw",
            ErrorKind::Abort => "abort",
            ErrorKind::InfiniteRecursion => "infinite recursion",
            ErrorKind::ResourceLimit => "resource limit",
            ErrorKind::Purity => "purity violation",
            ErrorKind::Import => "import error",
        }
    }
}

#[derive(Debug, Clone)]
pub struct EvalError {
    pub kind: ErrorKind,
    pub msg: String,
    pub pos: Option<Pos>,
    /// Forcing trace, innermost last.
    pub trace: Vec<String>,
}

impl EvalError {
    pub fn new(kind: ErrorKind, msg: impl Into<String>) -> Self {
        EvalError { kind, msg: msg.into(), pos: None, trace: Vec::new() }
    }

    pub fn at(kind: ErrorKind, msg: impl Into<String>, pos: &Pos) -> Self {
        EvalError { kind, msg: msg.into(), pos: Some(pos.clone()), trace: Vec::new() }
    }

    pub fn with_trace(mut self, frame: impl Into<String>) -> Self {
        self.trace.push(frame.into());
        self
    }
}

impl fmt::Display for EvalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind.label(), self.msg)?;
        if let Some(p) = &self.pos {
            write!(f, " at {p}")?;
        }
        for t in self.trace.iter().rev() {
            write!(f, "\n  while {t}")?;
        }
        Ok(())
    }
}

pub type Result<T> = core::result::Result<T, EvalError>;
