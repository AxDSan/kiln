//! Process control on a platform that has none yet.
//!
//! The `Target` trait exists so that a second engine can arrive without the
//! layers above learning a second vocabulary — `ptrace` and Windows's
//! `WaitForDebugEvent` share none. Until that engine is written, this stands
//! in its place so the toolchain still builds and runs everywhere: everything
//! that does not need a live program works, and everything that does says why
//! rather than failing to compile.

use super::{Stop, Target};
use crate::unwind::{Module, Registers};
use crate::Error;
use std::path::Path;

/// A handle to nothing, so callers that hold one need no platform test.
#[derive(Debug, Clone, Copy)]
pub struct Interrupt;

impl Interrupt {
    pub fn none() -> Interrupt {
        Interrupt
    }
    pub fn stop(&self) {}
}

pub struct UnsupportedTarget;

fn unsupported<T>() -> Result<T, Error> {
    Err(Error::Unsupported)
}

impl UnsupportedTarget {
    pub fn arm(&mut self, _address: u64) -> Result<(), Error> {
        unsupported()
    }
    pub fn disarm(&mut self, _address: u64) -> Result<(), Error> {
        unsupported()
    }
    pub fn armed(&self, _address: u64) -> bool {
        false
    }
    pub fn output(&mut self) -> Vec<u8> {
        Vec::new()
    }
    pub fn interrupt_handle(&self) -> Interrupt {
        Interrupt
    }
}

impl Target for UnsupportedTarget {
    fn launch(_program: &Path, _args: &[String]) -> Result<Self, Error> {
        unsupported()
    }
    fn registers(&self) -> Result<Registers, Error> {
        unsupported()
    }
    fn set_registers(&mut self, _registers: Registers) -> Result<(), Error> {
        unsupported()
    }
    fn read_memory(&self, _address: u64, _into: &mut [u8]) -> Result<(), Error> {
        unsupported()
    }
    fn write_memory(&mut self, _address: u64, _from: &[u8]) -> Result<(), Error> {
        unsupported()
    }
    fn resume(&mut self) -> Result<Stop, Error> {
        unsupported()
    }
    fn single_step(&mut self) -> Result<Stop, Error> {
        unsupported()
    }
    fn interrupt(&mut self) -> Result<(), Error> {
        unsupported()
    }
    fn stop(&mut self) -> Result<(), Error> {
        Ok(())
    }
    fn load_bias(&self) -> u64 {
        0
    }
    fn modules(&self) -> Result<Vec<Module>, Error> {
        Ok(Vec::new())
    }
}

impl crate::unwind::Memory for UnsupportedTarget {
    fn read_u64(&self, _address: u64) -> Option<u64> {
        None
    }
}
