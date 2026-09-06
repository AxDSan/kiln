//! `ptrace`, and nothing above it.
//!
//! The breakpoint mechanics live here because they are the platform's:
//! `0xCC` is an x86 instruction and `PTRACE_POKEDATA` writes a word at a time.
//! A byte is saved before a trap replaces it, and stepping over an armed
//! breakpoint means restoring the byte, stepping one instruction, and putting
//! the trap back — in that order, because the processor cannot execute the
//! instruction that is no longer there.
//!
//! Saved bytes are keyed by *address*, never by the eight-byte word `POKEDATA`
//! writes. Two breakpoints inside one word are ordinary — a line is a handful
//! of instructions — and a word-keyed cache lets the second one save the first
//! one's trap as if it were the original code.

use super::{Stop, Target};
use crate::unwind::{Module, Registers};
use crate::Error;
use std::path::Path;

pub struct LinuxTarget {
    #[allow(dead_code)]
    pid: i32,
}

impl LinuxTarget {
    /// Plant a trap, remembering the byte it replaced.
    pub fn arm(&mut self, _address: u64) -> Result<(), Error> {
        todo!("phase 4")
    }

    /// Take one out and put its byte back.
    pub fn disarm(&mut self, _address: u64) -> Result<(), Error> {
        todo!("phase 4")
    }

    /// Whether a trap is currently planted at an address.
    pub fn armed(&self, _address: u64) -> bool {
        todo!("phase 4")
    }
}

impl Target for LinuxTarget {
    fn launch(_program: &Path, _args: &[String]) -> Result<Self, Error> {
        todo!("phase 4")
    }
    fn registers(&self) -> Result<Registers, Error> {
        todo!("phase 4")
    }
    fn set_registers(&mut self, _registers: Registers) -> Result<(), Error> {
        todo!("phase 4")
    }
    fn read_memory(&self, _address: u64, _into: &mut [u8]) -> Result<(), Error> {
        todo!("phase 4")
    }
    fn write_memory(&mut self, _address: u64, _from: &[u8]) -> Result<(), Error> {
        todo!("phase 4")
    }
    fn resume(&mut self) -> Result<Stop, Error> {
        todo!("phase 4")
    }
    fn single_step(&mut self) -> Result<Stop, Error> {
        todo!("phase 4")
    }
    fn interrupt(&mut self) -> Result<(), Error> {
        todo!("phase 4")
    }
    fn stop(&mut self) -> Result<(), Error> {
        todo!("phase 4")
    }
    fn load_bias(&self) -> u64 {
        todo!("phase 4")
    }
    fn modules(&self) -> Result<Vec<Module>, Error> {
        todo!("phase 4")
    }
}
