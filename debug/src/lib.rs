//! Reads the debug information a Kiln build emits.
//!
//! This crate knows how to *read* a built program: which address belongs to
//! which line of which source, and which function an address is in. It never
//! runs anything — there is no `ptrace` here, no child process, nothing that
//! needs a program to be running. That separation is deliberate, and it is the
//! lesson every from-scratch debugger reports learning late: the code that
//! understands debug information is long-lived and worth testing on its own,
//! and the code that drives a process is neither.
//!
//! Nothing here shells out to `gdb` or `lldb`, and nothing here requires
//! either to be installed. `gimli` parses DWARF and `object` parses the
//! container; everything above them is ours.

use std::path::Path;

pub mod session;
pub mod step;
mod symbols;
pub mod target;
pub mod unwind;
pub mod value;

pub use symbols::{Program, Row, Subprogram, Variable};

/// Anything that stopped a program being read.
#[derive(Debug)]
pub enum Error {
    /// The file could not be opened or mapped.
    Io(std::io::Error),
    /// The file is not an object file this can read.
    Object(object::Error),
    /// The DWARF inside it is malformed.
    Dwarf(gimli::Error),
    /// This platform has no process-control engine yet. Reading a program's
    /// debug information works everywhere; running one under control does not.
    Unsupported,
    /// The file is an object this can read, and carries no debug information.
    /// Separate from a malformed one because the answer is different: this is
    /// what a `--release` build looks like, and the fix is to build without it.
    NoDebugInfo,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::Object(e) => write!(f, "not an object file this can read: {e}"),
            Error::Dwarf(e) => write!(f, "malformed debug information: {e}"),
            Error::Unsupported => write!(
                f,
                "debugging is not supported on this platform yet — the engine is `ptrace`, \
                 and Windows needs one of its own"
            ),
            Error::NoDebugInfo => write!(
                f,
                "this program carries no debug information — it was built with `--release`, \
                 which strips it. Build without `--release` to debug it."
            ),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}
impl From<object::Error> for Error {
    fn from(e: object::Error) -> Self {
        Error::Object(e)
    }
}
impl From<gimli::Error> for Error {
    fn from(e: gimli::Error) -> Self {
        Error::Dwarf(e)
    }
}

/// Read a built program's debug information.
pub fn load(path: &Path) -> Result<Program, Error> {
    symbols::load(path)
}
