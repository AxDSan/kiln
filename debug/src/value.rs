//! Rendering an OpenEPL value out of a stopped program's memory.
//!
//! This is the layer that makes the debugger OpenEPL's rather than a small
//! gdb, and every rule in it comes from something only the compiler knows:
//!
//! - **Arrays are 1-based.** `names = [1] "ada", [2] "grace"`, because that is
//!   what the language means by an index. A debugger that does not know the
//!   language has to hedge, and hedging about indexing is unusable.
//! - **`text` is a NUL-terminated UTF-8 pointer**, and `NULL` means empty
//!   rather than absent. It renders as the characters the user wrote.
//! - **An optional has no runtime representation.** It is two locals — the
//!   value, and a hidden companion holding whether it is there. Nothing in
//!   memory distinguishes an absent `int` from zero; only the emitter knows
//!   they are a pair, which is why `nothing` can be printed at all.
//! - **Record field names do not reach a shipped binary.** They exist here
//!   only because the compiler wrote them into the debug information.
//! - **Compiler-invented locals are hidden.** They are the only names
//!   containing `$`, and `$` is not a character an identifier may contain, so
//!   filtering them is exact rather than a guess.

use crate::Error;

/// A value as the user should see it.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i32),
    Int64(i64),
    Double(f64),
    Bool(bool),
    Text(String),
    /// An optional that is not there.
    Nothing,
    /// An array, already 1-based for display.
    Array(Vec<Value>),
    Record {
        name: String,
        fields: Vec<(String, Value)>,
    },
    Dict(Vec<(String, Value)>),
    /// A value that could not be read, with why. Shown rather than hidden: a
    /// blank row is indistinguishable from a bug.
    Unreadable(String),
}

impl std::fmt::Display for Value {
    fn fmt(&self, _f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        todo!("phase 5")
    }
}

/// A local, as the debug information describes it.
#[derive(Debug, Clone)]
pub struct Local {
    pub name: String,
    /// Where it lives, relative to the frame.
    pub frame_offset: i64,
    pub type_name: String,
}

/// Read a local out of a stopped frame.
pub fn read(
    _local: &Local,
    _frame_base: u64,
    _memory: &dyn crate::unwind::Memory,
) -> Result<Value, Error> {
    todo!("phase 5")
}
