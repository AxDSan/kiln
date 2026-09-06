//! Walking a stopped program's stack.
//!
//! The unwinder is pure: it is given the registers of the innermost frame and
//! something that can read the stopped program's memory, and it produces the
//! frames above. It never touches a process, which is what makes it testable
//! against a register set written by hand.
//!
//! It reads CFI — `.eh_frame` — rather than following the frame-pointer chain.
//! A frame-pointer walk is right only when a frame pointer is live at the
//! moment you stop, and it is not live in a function's first instructions:
//! stopping at a function's low address, before its prologue has run, silently
//! drops its caller. CFI is correct at every address, which is the whole
//! reason it exists.

use crate::Error;

/// The registers a frame is described by. Only the three the unwinder and the
/// stepper need; the process-control layer holds the rest.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Registers {
    /// The program counter, `rip`.
    pub pc: u64,
    /// The stack pointer, `rsp`.
    pub sp: u64,
    /// The frame pointer, `rbp`.
    pub bp: u64,
}

/// One frame of a stopped stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    pub registers: Registers,
    /// The Canonical Frame Address: the stack pointer as it was in this
    /// frame's *caller*, just before the call. It is the one value that
    /// identifies a frame across the instructions inside it — `sp` moves and
    /// `pc` moves, and the CFA does not — which is why stepping compares
    /// CFAs and never stack pointers.
    ///
    /// **The stack grows down**, so a callee's CFA is *smaller* than its
    /// caller's. Every comparison in `step.rs` depends on that direction, and
    /// getting it backwards makes stepping fail silently rather than loudly.
    pub cfa: u64,
}

/// Reading a stopped program's memory.
///
/// The unwinder needs to follow saved registers out of the stack, and that is
/// all it needs a process for. Taking it as a trait keeps every process
/// concern on the other side of one small interface, and lets the unwinder be
/// tested against a `Vec`.
pub trait Memory {
    /// Eight bytes at an address, or `None` when that address is not mapped.
    fn read_u64(&self, address: u64) -> Option<u64>;
}

/// Walks stacks for one program.
pub struct Unwinder {
    #[allow(dead_code)]
    modules: Vec<Module>,
}

/// One mapped object and the CFI inside it.
///
/// There is a list of these rather than one, because a GUI program links SDL2
/// as a shared object and it brings its own `.eh_frame`. Pausing an idle form
/// stops inside `libSDL2.so`, and an unwinder that only knows the executable's
/// CFI produces a one-frame stack for the most common "why is my form stuck"
/// gesture there is.
#[allow(dead_code)]
pub struct Module {
    /// Where the object is mapped, for turning a runtime address into the
    /// static one its CFI is written against.
    pub bias: u64,
    /// The object's `.eh_frame`.
    pub eh_frame: Vec<u8>,
    /// The address `.eh_frame` was linked at.
    pub eh_frame_address: u64,
}

impl Unwinder {
    pub fn new(modules: Vec<Module>) -> Self {
        Unwinder { modules }
    }

    /// The stack, innermost frame first.
    ///
    /// Stops when a frame cannot be unwound rather than failing: the outermost
    /// frames are libc's, which is exactly where the CFI runs out, and "the
    /// stack ends here" is the right answer rather than an error.
    pub fn walk(&mut self, _top: Registers, _memory: &dyn Memory) -> Result<Vec<Frame>, Error> {
        todo!("phase 3")
    }
}
