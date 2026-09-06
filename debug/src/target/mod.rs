//! Controlling a running program.
//!
//! Everything the rest of the debugger needs from a live process is behind one
//! trait. That is the seam Windows arrives through: `WaitForDebugEvent` and
//! `ptrace` share no vocabulary, and the layers above must not learn either.

use crate::unwind::Registers;
use crate::Error;
use std::path::Path;

pub mod linux;
#[cfg(not(target_os = "linux"))]
pub mod unsupported;

/// The engine for this platform.
///
/// Named rather than chosen at each use, so that everything above knows one
/// type and a second engine is one line here rather than a `cfg` in every
/// file that runs a program.
#[cfg(target_os = "linux")]
pub use linux::{Interrupt, LinuxTarget as Native};
#[cfg(not(target_os = "linux"))]
pub use unsupported::{Interrupt, UnsupportedTarget as Native};

/// Why a program stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// It hit a trap we planted. The program counter has already been rewound
    /// to the breakpoint's own address — the trap instruction is one byte and
    /// the processor reports the address after it, and every layer above would
    /// otherwise have to remember that.
    Breakpoint,
    /// A single step finished.
    Step,
    /// It ran to completion.
    Exited(i32),
    /// A signal it did not ask for. Passed through rather than swallowed: a
    /// segmentation fault is something the user must be told about.
    Signal(i32),
}

/// A program under our control.
pub trait Target {
    /// Start a program stopped before its first instruction, so breakpoints
    /// can be planted before anything runs.
    fn launch(program: &Path, args: &[String]) -> Result<Self, Error>
    where
        Self: Sized;

    fn registers(&self) -> Result<Registers, Error>;
    fn set_registers(&mut self, registers: Registers) -> Result<(), Error>;
    fn read_memory(&self, address: u64, into: &mut [u8]) -> Result<(), Error>;
    fn write_memory(&mut self, address: u64, from: &[u8]) -> Result<(), Error>;

    /// Let it run until something stops it.
    ///
    /// Every thread must be stopped before this returns. Making one thread's
    /// trap stop only that thread is the mistake to avoid: the others keep
    /// running, and a variable read while another thread writes it is a torn
    /// value reported as a fact.
    fn resume(&mut self) -> Result<Stop, Error>;

    /// One instruction.
    fn single_step(&mut self) -> Result<Stop, Error>;

    /// Stop a running program, for a Pause button.
    fn interrupt(&mut self) -> Result<(), Error>;

    /// End the session and the program with it.
    fn stop(&mut self) -> Result<(), Error>;

    /// What to add to an address in the debug information to get the address
    /// it was loaded at. Zero for a non-relocatable program.
    fn load_bias(&self) -> u64;

    /// Every object mapped into the program, for the unwinder.
    fn modules(&self) -> Result<Vec<crate::unwind::Module>, Error>;
}
