//! The lythos standard prelude.
//!
//! Add `use lythos_std::prelude::*;` to import the most commonly needed items
//! without spelling out full paths.
//!
//! Mirrors the items in `std::prelude::rust_2021`.

// ── From core ────────────────────────────────────────────────────────────────

pub use core::{
    clone::Clone,
    cmp::{Eq, Ord, PartialEq, PartialOrd},
    convert::{AsMut, AsRef, From, Into, TryFrom, TryInto},
    default::Default,
    fmt,
    iter::{
        DoubleEndedIterator, ExactSizeIterator, Extend, FromIterator, IntoIterator, Iterator,
    },
    marker::{Copy, Send, Sized, Sync},
    mem::drop,
    ops::{Drop, Fn, FnMut, FnOnce},
    option::Option::{self, None, Some},
    result::Result::{self, Err, Ok},
};

// ── From alloc ────────────────────────────────────────────────────────────────

pub use alloc::{
    borrow::ToOwned,
    boxed::Box,
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};

// ── From lythos-std ───────────────────────────────────────────────────────────

pub use crate::{
    // Macros
    eprint, eprintln, print, println,
    // Sync
    sync::{Arc, Mutex},
    // I/O
    io::{self, Read, Write},
    // Time
    time::Duration,
    // Task
    task::TaskId,
};
