//! One debugging session: the program, its breakpoints, and where it is.
//!
//! Everything asynchronous lives here, so the protocol layer above can be a
//! plain translation of requests into calls. The session owns the debuggee's
//! output pipes too — a byte of the program's own stdout reaching the
//! adapter's stdout corrupts the protocol stream permanently, so the two are
//! never the same file descriptor.

use crate::symbols::Program;
use crate::unwind::Frame;
use crate::Error;
use std::path::Path;

/// A breakpoint the user asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Breakpoint {
    pub id: u32,
    /// The line the user asked for.
    pub requested_line: u32,
    /// The line it actually landed on. A blank line or a comment runs nothing,
    /// so it moves to the next line that does — and the user is told, because
    /// a breakpoint that silently moved is a breakpoint that lies.
    pub line: u32,
    /// `None` when no address could be found, which the IDE draws differently.
    /// A breakpoint that silently never fires is the worst thing here.
    pub address: Option<u64>,
}

/// Why the program is stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stopped {
    Breakpoint(u32),
    Step,
    Pause,
    /// The runtime reported an error and is about to exit. Caught before it
    /// does, so the stack is still whole.
    RuntimeError(String),
    Exited(i32),
}

pub struct Session {
    #[allow(dead_code)]
    program: Program,
}

impl Session {
    pub fn launch(_binary: &Path, _args: &[String]) -> Result<Session, Error> {
        todo!("phase 4")
    }

    /// Replace the breakpoint set for a file. Not incremental: the protocol
    /// sends the whole list every time, and the reply must keep the order it
    /// was given, because the client matches them up by position.
    pub fn set_breakpoints(&mut self, _lines: &[u32]) -> Result<Vec<Breakpoint>, Error> {
        todo!("phase 4")
    }

    pub fn resume(&mut self) -> Result<Stopped, Error> {
        todo!("phase 4")
    }

    pub fn step(&mut self, _kind: crate::step::Step) -> Result<Stopped, Error> {
        todo!("phase 4")
    }

    /// The stack, with the runtime's frames removed. Filtered on which compile
    /// unit produced the code, never on a symbol prefix: the runtime's own C
    /// is compiled into the same binary and would pass a prefix test.
    pub fn stack(&mut self) -> Result<Vec<Frame>, Error> {
        todo!("phase 4")
    }

    pub fn locals(&mut self, _frame: usize) -> Result<Vec<(String, crate::value::Value)>, Error> {
        todo!("phase 5")
    }

    pub fn stop(&mut self) -> Result<(), Error> {
        todo!("phase 4")
    }
}
