//! Stepping: what to do when the user says "next line".
//!
//! Stepping is not a loop of single-steps. Single-stepping a line that calls
//! `print_text` walks every instruction of the runtime and of libc, thousands
//! of them, and takes visibly long. Instead each step is a *plan*: a set of
//! addresses to trap, and a rule for deciding whether a trap that fires is the
//! one we meant. That is what every serious debugger does.
//!
//! **Frames are compared by CFA, and the stack grows down**, so a callee's CFA
//! is smaller than its caller's. Stated once, here, because it is the single
//! easiest thing to get backwards and it fails silently:
//!
//! | Step | Stop when |
//! |---|---|
//! | over (`next`) | `frame.cfa == start.cfa` — the same frame |
//! | in (`step`) | `frame.cfa < start.cfa` — deeper, i.e. a callee |
//! | out (`finish`) | `frame.cfa > start.cfa` — shallower, i.e. the caller |
//!
//! Reverse `<` and `>` and "step in" quietly becomes "continue": its condition
//! is false at every function entry, so nothing ever stops and no error is
//! reported.
//!
//! **Leaving the user's outermost frame means continue, not stop.** A click
//! handler is called from the runtime, under a dozen frames of C and C++ that
//! carry no line table. Stepping out of the handler lands there, and there is
//! no source to show — so the stepper resumes instead of reporting a stop the
//! IDE cannot draw.

use crate::symbols::Program;
use crate::unwind::Frame;

/// What the user asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Over,
    In,
    Out,
    /// Run to a line, without leaving a breakpoint behind.
    ToLine(u32),
}

/// A step in progress: where to trap, and how to judge a trap that fires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Addresses to trap for the duration of the step, then take back out.
    pub traps: Vec<u64>,
    /// The CFA of the frame the step started in.
    pub from_cfa: u64,
    /// The line it started on. A trap that fires on the same line in the same
    /// frame is not a stop — one source line is several instructions, and a
    /// loop's back edge returns to its own first instruction.
    pub from_line: u32,
    pub kind: Step,
}

impl Plan {
    /// Work out where to trap.
    pub fn new(_kind: Step, _program: &Program, _stack: &[Frame]) -> Plan {
        todo!("phase 4")
    }

    /// Whether a trap that fired is the end of this step, or something to
    /// resume through.
    pub fn arrived(&self, _program: &Program, _stack: &[Frame]) -> bool {
        todo!("phase 4")
    }
}
