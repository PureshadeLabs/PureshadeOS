//! Value display for `shadec eval`. Non-strict shows WHNF: already-forced
//! interior thunks print recursively, suspended ones print as `…` — deep
//! forcing happens only under `--strict` (03 §2).

use alloc::format;
use alloc::string::{String, ToString};

use crate::error::{Pos, Result};
use crate::eval::Evaluator;
use crate::value::{quote_string, ThunkRef, ThunkState, Value};

pub fn show_value(ev: &mut Evaluator, v: &Value, strict: bool, pos: &Pos) -> Result<String> {
    match v {
        Value::Int(i) => Ok(format!("{i}")),
        Value::Bool(true) => Ok("true".into()),
        Value::Bool(false) => Ok("false".into()),
        Value::Null => Ok("null".into()),
        Value::Str(s) => Ok(quote_string(&s.s)), // context is not part of the printed form (04 §5)
        Value::Path { path, .. } => Ok(path.to_string()),
        Value::List(xs) => {
            let mut out = String::from("[ ");
            for t in xs.iter() {
                match show_thunk(ev, t, strict, pos)? {
                    Some(s) => out.push_str(&s),
                    None => out.push('…'),
                }
                out.push(' ');
            }
            out.push(']');
            Ok(out)
        }
        Value::Attrs(m) => {
            let mut out = String::from("{ ");
            for (k, t) in m.iter() {
                out.push_str(k);
                out.push_str(" = ");
                match show_thunk(ev, t, strict, pos)? {
                    Some(s) => out.push_str(&s),
                    None => out.push('…'),
                }
                out.push_str("; ");
            }
            out.push('}');
            Ok(out)
        }
        Value::Lambda(_) => Ok("<lambda>".into()),
        Value::Prim(p) => {
            if p.args.is_empty() {
                Ok(format!("<primop {}>", p.prim.name))
            } else {
                Ok(format!("<primop {}, partially applied>", p.prim.name))
            }
        }
    }
}

fn show_thunk(ev: &mut Evaluator, t: &ThunkRef, strict: bool, pos: &Pos) -> Result<Option<String>> {
    if strict {
        let v = ev.force(t, pos)?;
        return Ok(Some(show_value(ev, &v, true, pos)?));
    }
    let done = matches!(&*t.state.borrow(), ThunkState::Done(_));
    if done {
        let v = ev.force(t, pos)?; // cheap: memoized
        Ok(Some(show_value(ev, &v, false, pos)?))
    } else {
        Ok(None)
    }
}
