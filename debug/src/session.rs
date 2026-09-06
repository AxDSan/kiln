//! One debugging session: the program, its breakpoints, and where it is.
//!
//! This is where the pieces meet. Symbols say which address a line is at, the
//! target arms a trap there and runs the program, the unwinder turns a stop
//! into a stack, and the stepper decides whether a trap that fired is the one
//! the user's last gesture was waiting for. Each of those is testable alone;
//! this is the part that can only be judged by running a program.
//!
//! Everything asynchronous stops here, so the protocol layer above is a plain
//! translation of requests into calls. The debuggee's output belongs to the
//! session too — a byte of the program's own stdout reaching the adapter's
//! stdout corrupts the protocol stream permanently, so the two are never the
//! same file descriptor.

use crate::step::{Plan, Step};
use crate::symbols::Program;
use crate::target::{linux::LinuxTarget, Stop, Target};
use crate::unwind::{Frame, Unwinder};
use crate::value::{self, Value};
use crate::Error;
use std::path::Path;

/// The signals that mean a person asked for the program to stop, rather than
/// something the program should be told about.
const SIGINT: i32 = 2;
const SIGSTOP: i32 = 19;

/// Whether a frame is the one the program is actually stopped in.
///
/// Compared by identity rather than by index because the caller has already
/// resolved the frame, and passing both a frame and its position invites the
/// two to disagree.
fn frame_index_is_innermost(frame: Frame, stack: &[Frame]) -> bool {
    stack.first().is_some_and(|first| *first == frame)
}

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
    program: Program,
    target: LinuxTarget,
    /// Built on the first stack walk, not at launch.
    ///
    /// A program stopped at its entry point has not run its dynamic loader
    /// yet, so the only objects mapped are the program and the loader itself.
    /// Reading the list then leaves out the C library — and every shared
    /// object a GUI program uses — so a stack that passes through one ends at
    /// the first frame, looking exactly like unwind information running out.
    unwinder: Option<Unwinder>,
    breakpoints: Vec<Breakpoint>,
    next_id: u32,
    /// What to add to an address in the debug information to reach the address
    /// the program was loaded at. Held rather than asked for each time, so
    /// every conversion in the session uses one answer.
    bias: u64,
    /// Whether the program has ended. Every entry point checks it, because
    /// `ptrace` on a process that has exited fails in a way that reads as a
    /// debugger bug rather than as the ordinary end of a program.
    ended: Option<i32>,
}

impl Session {
    /// Start a program stopped before its first instruction, so breakpoints
    /// can be planted before anything runs.
    pub fn launch(binary: &Path, args: &[String]) -> Result<Session, Error> {
        let program = crate::load(binary)?;
        let target = LinuxTarget::launch(binary, args)?;
        let bias = target.load_bias();
        Ok(Session {
            program,
            target,
            unwinder: None,
            breakpoints: Vec::new(),
            next_id: 1,
            bias,
            ended: None,
        })
    }

    /// The symbols, for a caller that needs to describe an address.
    pub fn program(&self) -> &Program {
        &self.program
    }

    /// Whatever the program has written since this was last called.
    ///
    /// Drained rather than copied: the caller forwards it, and forwarding the
    /// same bytes twice is worse than losing them.
    pub fn output(&mut self) -> Vec<u8> {
        self.target.output()
    }

    /// A way to stop the program from another thread.
    ///
    /// Pause is the one gesture that must work *while* the session is blocked
    /// waiting for the program, which is exactly when `&mut self` is held by
    /// the waiting thread. The handle carries no borrow, so a reader thread
    /// can hold one and fire it the moment a pause request arrives.
    pub fn interrupt_handle(&self) -> crate::target::linux::Interrupt {
        self.target.interrupt_handle()
    }

    /// Replace the breakpoint set.
    ///
    /// Not incremental: the protocol sends the whole list every time. The
    /// reply keeps the order it was given, because the client matches them up
    /// by position.
    pub fn set_breakpoints(&mut self, lines: &[u32]) -> Result<Vec<Breakpoint>, Error> {
        for old in &self.breakpoints {
            if let Some(address) = old.address {
                // A program that has ended has nothing to disarm, and saying so
                // is not an error worth failing a request over.
                if self.ended.is_none() {
                    self.target.disarm(address)?;
                }
            }
        }
        self.breakpoints.clear();

        let mut set = Vec::with_capacity(lines.len());
        for &line in lines {
            let resolved = self.program.breakpoint_for(line);
            let id = self.next_id;
            self.next_id += 1;
            let breakpoint = match resolved {
                Some(row) => Breakpoint {
                    id,
                    requested_line: line,
                    line: row.line,
                    address: Some(row.address + self.bias),
                },
                None => Breakpoint {
                    id,
                    requested_line: line,
                    line,
                    address: None,
                },
            };
            if let Some(address) = breakpoint.address {
                if self.ended.is_none() {
                    self.target.arm(address)?;
                }
            }
            set.push(breakpoint);
        }
        self.breakpoints = set.clone();
        Ok(set)
    }

    /// Let the program run until something stops it.
    pub fn resume(&mut self) -> Result<Stopped, Error> {
        if let Some(code) = self.ended {
            return Ok(Stopped::Exited(code));
        }
        let stop = self.target.resume()?;
        self.classify(stop)
    }

    /// Take one step of the kind the user asked for.
    ///
    /// A step is a plan — a set of addresses to trap and a rule for judging a
    /// trap that fires — rather than a loop of single steps. Single-stepping a
    /// line that calls `print_text` walks thousands of instructions through the
    /// runtime and the C library, and takes long enough to see.
    pub fn step(&mut self, kind: Step) -> Result<Stopped, Error> {
        if let Some(code) = self.ended {
            return Ok(Stopped::Exited(code));
        }
        let stack = self.stack()?;
        if stack.is_empty() {
            // Nowhere to step from: the program is somewhere with no debug
            // information, so let it run rather than plant a plan that can
            // never be satisfied.
            return self.resume();
        }
        let plan = Plan::new(kind, &self.program, &stack);

        // Only the addresses this plan added are taken back out afterwards. A
        // plan trap that lands on a user breakpoint must leave the user's
        // breakpoint armed.
        let mut planted = Vec::new();
        for &address in &plan.traps {
            let runtime = address + self.bias;
            if !self.target.armed(runtime) {
                self.target.arm(runtime)?;
                planted.push(runtime);
            }
        }

        let outcome = self.run_plan(&plan);
        for address in planted {
            if self.ended.is_none() {
                self.target.disarm(address)?;
            }
        }
        outcome
    }

    /// Run until the plan is satisfied, the user's own breakpoint fires, or the
    /// program ends.
    fn run_plan(&mut self, plan: &Plan) -> Result<Stopped, Error> {
        loop {
            let stop = self.target.resume()?;
            let stopped = self.classify(stop)?;
            match stopped {
                // A user's breakpoint outranks the step. Reporting the step
                // instead — or worse, resuming through it — is the classic way
                // a stepping debugger silently swallows a breakpoint.
                Stopped::Breakpoint(_) | Stopped::Exited(_) | Stopped::RuntimeError(_) => {
                    return Ok(stopped)
                }
                Stopped::Pause => return Ok(stopped),
                Stopped::Step => {
                    let stack = self.stack()?;
                    if stack.is_empty() || plan.arrived(&self.program, &stack) {
                        return Ok(Stopped::Step);
                    }
                }
            }
        }
    }

    /// Work out what a stop from the target means to the session.
    fn classify(&mut self, stop: Stop) -> Result<Stopped, Error> {
        match stop {
            Stop::Exited(code) => {
                self.ended = Some(code);
                Ok(Stopped::Exited(code))
            }
            Stop::Step => Ok(Stopped::Step),
            // Only a signal that means "stop" is a stop. A program is sent
            // signals in the ordinary course of running — a child of its own
            // exiting, a timer, a sleep being interrupted — and reporting each
            // as a pause makes a debugger appear to stop at random. The rest
            // are the program's own business and are delivered to it.
            Stop::Signal(SIGSTOP | SIGINT) => Ok(Stopped::Pause),
            Stop::Signal(_) => {
                let stop = self.target.resume()?;
                self.classify(stop)
            }
            Stop::Breakpoint => {
                let pc = self.target.registers()?.pc;
                match self.breakpoints.iter().find(|b| b.address == Some(pc)) {
                    Some(b) => Ok(Stopped::Breakpoint(b.id)),
                    // A trap the plan planted, not one the user asked for. It
                    // is a stop as far as the stepper is concerned and not one
                    // the user should be told about.
                    None => Ok(Stopped::Step),
                }
            }
        }
    }

    /// The stack, innermost frame first, with everything that is not the
    /// user's code removed.
    ///
    /// Filtered by whether an address has a row in the line table, never by a
    /// symbol prefix: the runtime's own C is compiled into the same binary and
    /// would pass a prefix test. A click handler sits under a dozen frames of
    /// C and C++ that carry no line table, and showing them makes the pane
    /// useless.
    pub fn stack(&mut self) -> Result<Vec<Frame>, Error> {
        if self.ended.is_some() {
            return Ok(Vec::new());
        }
        let top = self.target.registers()?;
        // Built on demand and then kept: reading every mapped object's unwind
        // information off disk at every step would cost more than the step.
        if self.unwinder.is_none() {
            self.unwinder = Some(Unwinder::new(self.target.modules()?));
        }
        let unwinder = self.unwinder.as_mut().expect("just built");
        let frames = unwinder.walk(top, &self.target)?;
        let bias = self.bias;
        let program = &self.program;
        Ok(frames
            .into_iter()
            .enumerate()
            .filter(|(i, frame)| {
                // Every frame but the innermost holds a *return* address, and
                // the call it returns to is the byte before it. A call that
                // ends a source line would otherwise be attributed to the next
                // line, or fall outside the function altogether.
                let pc = if *i == 0 {
                    frame.registers.pc
                } else {
                    frame.registers.pc.wrapping_sub(1)
                };
                pc.checked_sub(bias)
                    .is_some_and(|static_pc| program.line_for(static_pc).is_some())
            })
            .map(|(_, frame)| frame)
            .collect())
    }

    /// The named values visible in a frame.
    pub fn locals(&mut self, frame: usize) -> Result<Vec<(String, Value)>, Error> {
        if self.ended.is_some() {
            return Ok(Vec::new());
        }
        let stack = self.stack()?;
        let Some(frame) = stack.get(frame).copied() else {
            return Ok(Vec::new());
        };
        // Every frame but the innermost holds a return address, and the code
        // it belongs to is the byte before it.
        let pc = if frame_index_is_innermost(frame, &stack) {
            frame.registers.pc
        } else {
            frame.registers.pc.wrapping_sub(1)
        };
        let Some(static_pc) = pc.checked_sub(self.bias) else {
            return Ok(Vec::new());
        };
        let described: Vec<value::Local> = self
            .program
            .variables_at(static_pc)
            .into_iter()
            .map(|v| value::Local {
                name: v.name.clone(),
                frame_offset: v.frame_offset,
                type_name: v.type_name.clone(),
            })
            .collect();
        // The frame base is the frame pointer, which the compiler pins for
        // exactly this reason: it is a register the unwinder recovers for
        // every frame, where the stack pointer in an outer frame would have to
        // be inferred from the call that left it.
        Ok(value::locals(&described, frame.registers.bp, &self.target))
    }

    /// End the session and the program with it.
    pub fn stop(&mut self) -> Result<(), Error> {
        if self.ended.is_some() {
            return Ok(());
        }
        self.ended = Some(0);
        self.target.stop()
    }
}
