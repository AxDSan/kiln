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
//!
//! Two rules the kernel imposes that nothing in the signatures shows:
//!
//! - **Every `ptrace` call must come from the thread that forked the child.**
//!   The kernel checks that the tracee's parent is the calling *thread*, not
//!   the calling process, and a call from anywhere else fails with `ESRCH` —
//!   which reads exactly like "the program went away". A `LinuxTarget` must
//!   therefore be driven from one thread. `interrupt` is the single exception,
//!   because it sends a signal rather than tracing.
//! - **The debuggee's output is drained by a thread of ours.** A pipe holds
//!   64 kilobytes; a program that writes more than that with nobody reading
//!   blocks inside `write`, and our `waitpid` then blocks forever waiting for
//!   a program that is waiting for us. The deadlock does not appear until a
//!   program is chatty enough, which is the worst way to find it.

// Everything below is Linux: `/proc`, `ptrace`, and the x86-64 register block.
// The module is declared unconditionally by `target/mod.rs`, so the guard has
// to be here for a Windows build of the crate to keep compiling — the trait
// in `target/mod.rs` is the seam that port arrives through, and it must not be
// walled off behind a file that cannot be built.
#![cfg(target_os = "linux")]

use super::{Stop, Target};
use crate::unwind::{Memory, Module, Registers};
use crate::Error;
use object::{Object, ObjectSection, ObjectSegment};
use std::collections::BTreeMap;
use std::ffi::{c_char, c_int, c_long, c_ulong, c_void, CString};
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::fd::FromRawFd;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The requests this file makes of `ptrace`.
///
/// Spelled out rather than taken from a crate, because `openepl-debug` depends
/// on `gimli` and `object` and nothing else, and a process-control binding is
/// a large dependency to acquire for two dozen integers.
mod request {
    use std::ffi::c_int;

    pub const TRACEME: c_int = 0;
    pub const PEEKDATA: c_int = 2;
    pub const POKEDATA: c_int = 5;
    pub const CONT: c_int = 7;
    pub const SINGLESTEP: c_int = 9;
    pub const GETREGS: c_int = 12;
    pub const SETREGS: c_int = 13;
    pub const SETOPTIONS: c_int = 0x4200;
    pub const GETEVENTMSG: c_int = 0x4201;
}

/// Kill the tracee if we die, and trace the threads it starts.
///
/// `EXITKILL` is what stops a crashed debugger leaving a stopped process
/// behind for the user to find with `ps`. `TRACECLONE` makes new threads
/// tracees automatically — it does *not* stop them, which is why
/// [`LinuxTarget::stop_the_others`] exists.
const OPTIONS: c_long = 0x0000_0008 | 0x0010_0000;

/// A clone-event stop: a new thread exists and is waiting for us.
const EVENT_CLONE: c_int = 3;

const SIGSTOP: c_int = 19;
const SIGTRAP: c_int = 5;
const SIGKILL: c_int = 9;

/// Wait for threads as well as processes, and only for this thread's own
/// children.
///
/// `__WALL` is needed because a cloned thread does not report with `SIGCHLD`
/// and would otherwise be invisible. `__WNOTHREAD` is needed because the test
/// binary runs its tests on sibling threads of one process: without it, one
/// test's `waitpid(-1)` reaps another test's tracee stop, and the other test
/// blocks forever on a stop that has already been consumed.
const WAIT_FLAGS: c_int = 0x4000_0000 | 0x2000_0000;

/// Do not randomise the child's address space.
const ADDR_NO_RANDOMIZE: c_ulong = 0x0004_0000;

/// `tgkill`, which is how a single thread is signalled. `kill` addresses a
/// whole thread group and cannot stop one sibling.
const SYS_TGKILL: c_long = 234;

/// Close on exec, so a pipe we hold does not leak into an unrelated child —
/// the adapter compiles the program with `clang` before it traces it.
const O_CLOEXEC: c_int = 0o2000000;

unsafe extern "C" {
    fn fork() -> c_int;
    fn execv(path: *const c_char, argv: *const *const c_char) -> c_int;
    fn waitpid(pid: c_int, status: *mut c_int, options: c_int) -> c_int;
    fn ptrace(request: c_int, ...) -> c_long;
    fn kill(pid: c_int, signal: c_int) -> c_int;
    fn syscall(number: c_long, ...) -> c_long;
    fn personality(persona: c_ulong) -> c_int;
    fn pipe2(fds: *mut c_int, flags: c_int) -> c_int;
    fn dup2(old: c_int, new: c_int) -> c_int;
    fn close(fd: c_int) -> c_int;
    fn _exit(code: c_int) -> !;
    fn __errno_location() -> *mut c_int;
}

/// The x86-64 register block `PTRACE_GETREGS` fills in.
///
/// All twenty-seven of them, in the kernel's order, because `SETREGS` writes
/// the whole block back: handing it a struct built from the three registers
/// the debugger models would zero `rax`, the flags and the segment selectors,
/// and the program would die on its next instruction with nothing to say why.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct UserRegs {
    r15: u64,
    r14: u64,
    r13: u64,
    r12: u64,
    rbp: u64,
    rbx: u64,
    r11: u64,
    r10: u64,
    r9: u64,
    r8: u64,
    rax: u64,
    rcx: u64,
    rdx: u64,
    rsi: u64,
    rdi: u64,
    orig_rax: u64,
    rip: u64,
    cs: u64,
    eflags: u64,
    rsp: u64,
    ss: u64,
    fs_base: u64,
    gs_base: u64,
    ds: u64,
    es: u64,
    fs: u64,
    gs: u64,
}

/// What we know about one thread of the debuggee.
#[derive(Clone, Copy, Default)]
struct Thread {
    /// Whether it is stopped and waiting for us to let it go. Only a stopped
    /// thread may be traced, and only a running one may be waited for.
    stopped: bool,
    /// A signal the thread was stopped to receive, which we hand back when it
    /// is resumed. Swallowing it would make a debugger that quietly disarms
    /// the program's own signal handling.
    pending: c_int,
    /// A `SIGSTOP` we sent to stop the thread and have not yet seen arrive.
    /// It must be swallowed rather than delivered, and a thread that stopped
    /// for its own reason in the meantime still has ours queued behind it.
    interrupting: bool,
}

/// A program we launched and are tracing.
pub struct LinuxTarget {
    pid: i32,
    /// The thread that reported the current stop. Registers and memory are
    /// read through this one, because it is the thread whose stack the user is
    /// looking at.
    current: i32,
    threads: BTreeMap<i32, Thread>,
    /// The byte each planted trap replaced, by address.
    saved: BTreeMap<u64, u8>,
    /// `/proc/<pid>/mem`, opened once. Reading through it is one syscall for
    /// any length, where `PEEKDATA` is one syscall per eight bytes.
    ///
    /// Opened *after* the exec stop, and it has to be: the handle binds to the
    /// address space that exists when it is opened, so one opened before
    /// `execv` would keep reading the forked image of the debugger for the
    /// rest of the session.
    memory: File,
    program: PathBuf,
    bias: u64,
    output: Arc<Mutex<Vec<u8>>>,
    /// The exit status, once there is one. Every operation after this is an
    /// error rather than a hang.
    exited: Option<i32>,
}

impl LinuxTarget {
    /// Plant a trap, remembering the byte it replaced.
    ///
    /// Arming an address that is already armed does nothing, which matters:
    /// re-arming would save the trap byte as though it were the program's own
    /// code, and disarming would then leave the trap in place forever.
    pub fn arm(&mut self, address: u64) -> Result<(), Error> {
        if self.saved.contains_key(&address) {
            return Ok(());
        }
        let mut original = [0u8; 1];
        self.read_memory(address, &mut original)?;
        self.write_memory(address, &[0xCC])?;
        self.saved.insert(address, original[0]);
        Ok(())
    }

    /// Take one out and put its byte back.
    pub fn disarm(&mut self, address: u64) -> Result<(), Error> {
        let Some(original) = self.saved.remove(&address) else {
            return Ok(());
        };
        match self.write_memory(address, &[original]) {
            Ok(()) => Ok(()),
            Err(e) => {
                // The trap is still in the program, so the record of it must
                // survive too; dropping it would leave a trap nothing knows
                // how to remove.
                self.saved.insert(address, original);
                Err(e)
            }
        }
    }

    /// Whether a trap is currently planted at an address.
    pub fn armed(&self, address: u64) -> bool {
        self.saved.contains_key(&address)
    }

    /// The thread group we are tracing.
    pub fn pid(&self) -> i32 {
        self.pid
    }

    /// The program being traced.
    ///
    /// The symbol layer is loaded separately from the same file, and a
    /// debugger reading one binary's line table against another binary's
    /// addresses reports lines that are wrong rather than lines that are
    /// missing — so the two are taken from one place.
    pub fn program(&self) -> &Path {
        &self.program
    }

    /// Whatever the program has written to its stdout and stderr since this
    /// was last called.
    ///
    /// Taken rather than copied. The caller forwards these bytes to whoever is
    /// watching, and handing back everything written so far on every call
    /// would print the program's output again at every stop, growing by the
    /// whole history each time.
    ///
    /// The debuggee's output never touches ours. A byte of it reaching the
    /// adapter's stdout corrupts the protocol stream permanently, and there is
    /// no recovering a `Content-Length` framing that has had "42\n" inserted
    /// into it.
    ///
    /// Note that a program writing to a pipe has its own buffering, so its
    /// output usually arrives in a rush when it flushes rather than line by
    /// line as it runs. That is the program's libc, not this.
    pub fn output(&mut self) -> Vec<u8> {
        std::mem::take(&mut *self.output.lock().expect("output buffer"))
    }

    /// Whether the program is still there.
    pub fn running(&self) -> bool {
        self.exited.is_none()
    }

    /// The registers of one thread.
    fn get_regs(&self, tid: i32) -> Result<UserRegs, Error> {
        let mut regs = UserRegs::default();
        let ptr = (&raw mut regs).cast::<c_void>();
        // GETREGS returns 0 on success, so no errno dance is needed here.
        let rc = unsafe { ptrace(request::GETREGS, tid, std::ptr::null_mut::<c_void>(), ptr) };
        if rc < 0 {
            return Err(failed("could not read the program's registers"));
        }
        Ok(regs)
    }

    fn set_regs(&self, tid: i32, regs: &UserRegs) -> Result<(), Error> {
        let ptr = (&raw const *regs).cast::<c_void>();
        let rc = unsafe { ptrace(request::SETREGS, tid, std::ptr::null_mut::<c_void>(), ptr) };
        if rc < 0 {
            return Err(failed("could not write the program's registers"));
        }
        Ok(())
    }

    /// One word of the program's memory, exactly as it is — traps included.
    ///
    /// `PEEKDATA` returns the word it read, so `-1` is both a plausible value
    /// and the failure indication. `errno` is the only thing that separates
    /// them, and it has to be cleared first because a successful call does not
    /// clear it.
    fn peek(&self, address: u64) -> Result<u64, Error> {
        unsafe {
            *__errno_location() = 0;
            let word = ptrace(
                request::PEEKDATA,
                self.current,
                address as *mut c_void,
                std::ptr::null_mut::<c_void>(),
            );
            if word == -1 && *__errno_location() != 0 {
                return Err(failed("could not read the program's memory"));
            }
            Ok(word as u64)
        }
    }

    fn poke(&self, address: u64, word: u64) -> Result<(), Error> {
        let rc = unsafe {
            ptrace(
                request::POKEDATA,
                self.current,
                address as *mut c_void,
                word as *mut c_void,
            )
        };
        if rc < 0 {
            return Err(failed("could not write the program's memory"));
        }
        Ok(())
    }

    /// Read through `PEEKDATA`, a word at a time, for a kernel that will not
    /// let us have `/proc/<pid>/mem`.
    fn peek_into(&self, address: u64, into: &mut [u8]) -> Result<(), Error> {
        let mut done = 0;
        while done < into.len() {
            let at = address.wrapping_add(done as u64);
            let aligned = at & !7;
            let word = self.peek(aligned)?.to_le_bytes();
            let offset = (at - aligned) as usize;
            let take = (8 - offset).min(into.len() - done);
            into[done..done + take].copy_from_slice(&word[offset..offset + take]);
            done += take;
        }
        Ok(())
    }

    /// Write through `POKEDATA`, which can only write whole words — so a
    /// partial word is read back first and patched.
    ///
    /// The read-back is deliberately the raw one rather than [`Self::read_memory`]:
    /// a neighbouring breakpoint's `0xCC` must be written out again exactly as
    /// it is, and a masked read would replace it with the program's own byte
    /// and quietly remove somebody else's trap.
    fn poke_from(&self, address: u64, from: &[u8]) -> Result<(), Error> {
        let mut done = 0;
        while done < from.len() {
            let at = address.wrapping_add(done as u64);
            let aligned = at & !7;
            let mut word = self.peek(aligned)?.to_le_bytes();
            let offset = (at - aligned) as usize;
            let take = (8 - offset).min(from.len() - done);
            word[offset..offset + take].copy_from_slice(&from[done..done + take]);
            self.poke(aligned, u64::from_le_bytes(word))?;
            done += take;
        }
        Ok(())
    }

    /// Resume every stopped thread, each with whatever signal it was holding.
    fn continue_all(&mut self, step: bool) -> Result<(), Error> {
        let requested = if step {
            request::SINGLESTEP
        } else {
            request::CONT
        };
        for (&tid, thread) in self.threads.iter_mut() {
            if !thread.stopped {
                continue;
            }
            let signal = thread.pending;
            thread.pending = 0;
            let rc = unsafe {
                ptrace(
                    requested,
                    tid,
                    std::ptr::null_mut::<c_void>(),
                    signal as *mut c_void,
                )
            };
            if rc < 0 {
                return Err(failed("could not resume the program"));
            }
            thread.stopped = false;
        }
        Ok(())
    }

    /// Step the current thread one instruction and wait for it to land.
    ///
    /// Only the current thread moves. Every other thread is already stopped —
    /// nothing here ever resumes one without stopping it again before
    /// returning — so the invariant the trait asks for holds throughout.
    fn step_current(&mut self) -> Result<c_int, Error> {
        let signal = match self.threads.get_mut(&self.current) {
            Some(thread) => {
                let signal = thread.pending;
                thread.pending = 0;
                thread.stopped = false;
                signal
            }
            None => 0,
        };
        let rc = unsafe {
            ptrace(
                request::SINGLESTEP,
                self.current,
                std::ptr::null_mut::<c_void>(),
                signal as *mut c_void,
            )
        };
        if rc < 0 {
            return Err(failed("could not single-step the program"));
        }
        let mut status: c_int = 0;
        let waited = unsafe { waitpid(self.current, &raw mut status, WAIT_FLAGS) };
        if waited < 0 {
            return Err(failed("lost the program while stepping it"));
        }
        if let Some(thread) = self.threads.get_mut(&self.current) {
            thread.stopped = stopped(status);
        }
        Ok(status)
    }

    /// Get off a trap the current thread is sitting on.
    ///
    /// The instruction under a breakpoint has been replaced by `0xCC` and has
    /// not run. It has to be put back, executed on its own, and covered up
    /// again — and the trap must go back before anything else is resumed, or a
    /// loop passes the breakpoint a second time without stopping.
    ///
    /// Returns a stop when the single step itself ended the program.
    fn step_off_trap(&mut self) -> Result<Option<Stop>, Error> {
        let pc = self.get_regs(self.current)?.rip;
        if !self.armed(pc) {
            return Ok(None);
        }
        self.disarm(pc)?;
        let status = self.step_current()?;
        if let Some(stop) = self.ended(self.current, status)? {
            return Ok(Some(stop));
        }
        self.arm(pc)?;
        // The step itself can raise a signal, and a faulting instruction does
        // not advance the program counter. Swallowing that would put the trap
        // back under an unchanged `rip`, resume into it, and report a
        // breakpoint at the same address for as long as the user kept pressing
        // continue — with the segmentation fault that actually happened never
        // mentioned once.
        let signal = stop_signal(status);
        if signal != SIGTRAP {
            if let Some(thread) = self.threads.get_mut(&self.current) {
                thread.pending = if signal == SIGSTOP { 0 } else { signal };
            }
            return Ok(Some(Stop::Signal(signal)));
        }
        Ok(None)
    }

    /// Turn an exit into a stop, or `None` for a status that is a stop.
    ///
    /// A thread that is not the group leader dying is not the program ending:
    /// it is a thread ending, and the answer is to forget it and keep waiting.
    fn ended(&mut self, tid: i32, status: c_int) -> Result<Option<Stop>, Error> {
        if stopped(status) {
            return Ok(None);
        }
        self.threads.remove(&tid);
        let code = if exited(status) {
            exit_status(status)
        } else {
            // Killed by a signal. The trait has no shape for "died from a
            // signal", so it takes the shell's convention: the signal number
            // plus 128, which is what the user's terminal would have shown.
            128 + termination_signal(status)
        };
        if tid == self.pid {
            self.exited = Some(code);
            return Ok(Some(Stop::Exited(code)));
        }
        Ok(None)
    }

    /// Bring every other thread to a stop, so nothing moves while the user
    /// reads the program.
    ///
    /// `PTRACE_O_TRACECLONE` made the siblings tracees; it did not stop them.
    /// A local read taken while another thread is halfway through writing it
    /// is a torn value, and it is reported as a fact — there is nothing in the
    /// answer to say it was read during a race.
    fn stop_the_others(&mut self) -> Result<(), Error> {
        let others: Vec<i32> = self
            .threads
            .iter()
            .filter(|(&tid, thread)| tid != self.current && !thread.stopped)
            .map(|(&tid, _)| tid)
            .collect();
        for tid in others {
            let rc = unsafe { syscall(SYS_TGKILL, self.pid, tid, SIGSTOP) };
            if rc < 0 {
                // The thread exited between the listing and the signal, which
                // is a race nothing can close. It is stopped in the only sense
                // that matters.
                self.threads.remove(&tid);
                continue;
            }
            if let Some(thread) = self.threads.get_mut(&tid) {
                thread.interrupting = true;
            }
            let mut status: c_int = 0;
            let waited = unsafe { waitpid(tid, &raw mut status, WAIT_FLAGS) };
            if waited < 0 {
                self.threads.remove(&tid);
                continue;
            }
            if self.ended(tid, status)?.is_some() || !self.threads.contains_key(&tid) {
                continue;
            }
            let signal = stop_signal(status);
            let thread = self.threads.get_mut(&tid).expect("a thread we just kept");
            thread.stopped = true;
            // Our own `SIGSTOP` is not the program's to receive. Any other
            // signal arrived on its own account and is held for delivery.
            if signal == SIGSTOP && thread.interrupting {
                thread.interrupting = false;
            } else if signal != SIGTRAP {
                thread.pending = signal;
            } else {
                // It reached a breakpoint of its own while we were bringing it
                // to a halt. Its program counter needs the same rewind the
                // reporting thread gets, or resuming it later starts one byte
                // past the trap and the instruction underneath never runs at
                // all — a thread that quietly skips a line every time it
                // passes a breakpoint another thread stopped on.
                let mut regs = self.get_regs(tid)?;
                let breakpoint = regs.rip.wrapping_sub(1);
                if self.armed(breakpoint) {
                    regs.rip = breakpoint;
                    self.set_regs(tid, &regs)?;
                }
            }
        }
        Ok(())
    }

    /// Wait until something worth reporting happens, stopping the world before
    /// reporting it.
    fn wait_for_stop(&mut self) -> Result<Stop, Error> {
        loop {
            let mut status: c_int = 0;
            let tid = unsafe { waitpid(-1, &raw mut status, WAIT_FLAGS) };
            if tid < 0 {
                // No children left at all: the program is gone and we missed
                // the notice, which beats blocking on a wait that can never
                // return.
                let code = self.exited.unwrap_or(0);
                self.exited = Some(code);
                return Ok(Stop::Exited(code));
            }
            // A thread we have not met yet is one `TRACECLONE` created. Its
            // first stop is a `SIGSTOP` the kernel raised to tell us it
            // exists, and it is recorded as one we are waiting for so that it
            // is swallowed rather than reported: a program that starts a
            // thread would otherwise stop the user with a signal nothing sent.
            self.threads.entry(tid).or_insert(Thread {
                stopped: false,
                pending: 0,
                interrupting: true,
            });
            if let Some(stop) = self.ended(tid, status)? {
                return Ok(stop);
            }
            if !self.threads.contains_key(&tid) {
                continue;
            }
            let thread = self.threads.get_mut(&tid).expect("a live thread");
            thread.stopped = true;
            let signal = stop_signal(status);

            if event(status) == EVENT_CLONE {
                let mut spawned: u64 = 0;
                unsafe {
                    ptrace(
                        request::GETEVENTMSG,
                        tid,
                        std::ptr::null_mut::<c_void>(),
                        (&raw mut spawned).cast::<c_void>(),
                    );
                }
                if spawned != 0 {
                    // Whichever of the two arrives first — the parent's clone
                    // event or the new thread's own stop — the thread is
                    // recorded the same way, holding a `SIGSTOP` of the
                    // kernel's making that we swallow.
                    self.threads.entry(spawned as i32).or_insert(Thread {
                        stopped: false,
                        pending: 0,
                        interrupting: true,
                    });
                }
                self.continue_one(tid, 0)?;
                continue;
            }

            if signal == SIGSTOP && self.threads[&tid].interrupting {
                if let Some(thread) = self.threads.get_mut(&tid) {
                    thread.interrupting = false;
                }
                self.continue_one(tid, 0)?;
                continue;
            }

            if signal == SIGTRAP {
                let mut regs = self.get_regs(tid)?;
                // The processor reports the address *after* the trap, because
                // it executed one. Rewinding here is the only place that knows
                // the trap is one byte long, and every layer above would
                // otherwise have to remember it.
                let breakpoint = regs.rip.wrapping_sub(1);
                self.current = tid;
                if self.armed(breakpoint) {
                    regs.rip = breakpoint;
                    self.set_regs(tid, &regs)?;
                    self.stop_the_others()?;
                    return Ok(Stop::Breakpoint);
                }
                self.stop_the_others()?;
                return Ok(Stop::Step);
            }

            self.current = tid;
            if let Some(thread) = self.threads.get_mut(&tid) {
                // A `SIGSTOP` is never handed back. Delivering one to a tracee
                // puts it into a group stop we would then have to undo, and
                // the only `SIGSTOP` reaching a debuggee here is one we sent.
                thread.pending = if signal == SIGSTOP { 0 } else { signal };
            }
            self.stop_the_others()?;
            return Ok(Stop::Signal(signal));
        }
    }

    fn continue_one(&mut self, tid: i32, signal: c_int) -> Result<(), Error> {
        let rc = unsafe {
            ptrace(
                request::CONT,
                tid,
                std::ptr::null_mut::<c_void>(),
                signal as *mut c_void,
            )
        };
        if rc < 0 {
            return Err(failed("could not resume a thread"));
        }
        if let Some(thread) = self.threads.get_mut(&tid) {
            thread.stopped = false;
        }
        Ok(())
    }
}

impl Target for LinuxTarget {
    fn launch(program: &Path, args: &[String]) -> Result<Self, Error> {
        // Everything the child needs is built before the fork. Between `fork`
        // and `execv` only async-signal-safe calls are allowed, and this
        // process has other threads: allocating there can deadlock on a heap
        // lock a thread that no longer exists was holding.
        let program = program.canonicalize()?;
        let path = CString::new(program.as_os_str().as_encoded_bytes())
            .map_err(|_| Error::Io(std::io::Error::other("the program's path contains a NUL")))?;
        let mut owned = vec![path.clone()];
        for arg in args {
            owned.push(
                CString::new(arg.as_bytes())
                    .map_err(|_| Error::Io(std::io::Error::other("an argument contains a NUL")))?,
            );
        }
        let mut argv: Vec<*const c_char> = owned.iter().map(|a| a.as_ptr()).collect();
        argv.push(std::ptr::null());

        let mut fds: [c_int; 2] = [0, 0];
        if unsafe { pipe2(fds.as_mut_ptr(), O_CLOEXEC) } < 0 {
            return Err(failed("could not make a pipe for the program's output"));
        }
        let (reading, writing) = (fds[0], fds[1]);

        let pid = unsafe { fork() };
        if pid < 0 {
            unsafe {
                close(reading);
                close(writing);
            }
            return Err(failed("could not start the program"));
        }
        if pid == 0 {
            unsafe {
                ptrace(
                    request::TRACEME,
                    0,
                    std::ptr::null_mut::<c_void>(),
                    std::ptr::null_mut::<c_void>(),
                );
                // Without this the stack, the heap and every shared library
                // move on each run, and an address the user noted a moment ago
                // means nothing. The failure is ignored on purpose: a hardened
                // container refuses it, and a randomised debuggee is far
                // better than one that will not start.
                personality(ADDR_NO_RANDOMIZE);
                // The program's own output must never land on ours; `dup2`
                // also clears close-on-exec, which is why the pipe can carry
                // it across the `execv` that closes everything else.
                dup2(writing, 1);
                dup2(writing, 2);
                close(reading);
                close(writing);
                execv(path.as_ptr(), argv.as_ptr());
                // Reached only when the program could not be run at all. The
                // parent sees an exit rather than a stop and says so.
                _exit(127);
            }
        }

        unsafe { close(writing) };
        let mut status: c_int = 0;
        if unsafe { waitpid(pid, &raw mut status, WAIT_FLAGS) } < 0 {
            unsafe { close(reading) };
            return Err(failed("lost the program before it started"));
        }
        if !stopped(status) || stop_signal(status) != SIGTRAP {
            unsafe { close(reading) };
            return Err(Error::Io(std::io::Error::other(format!(
                "{} could not be run",
                program.display()
            ))));
        }
        let rc = unsafe {
            ptrace(
                request::SETOPTIONS,
                pid,
                std::ptr::null_mut::<c_void>(),
                OPTIONS as *mut c_void,
            )
        };
        if rc < 0 {
            unsafe { close(reading) };
            return Err(failed("could not set the tracing options"));
        }

        let memory = OpenOptions::new()
            .read(true)
            .write(true)
            .open(format!("/proc/{pid}/mem"))?;

        let output = Arc::new(Mutex::new(Vec::new()));
        let collecting = Arc::clone(&output);
        let mut pipe = unsafe { File::from_raw_fd(reading) };
        std::thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            while let Ok(read) = pipe.read(&mut chunk) {
                if read == 0 {
                    break;
                }
                collecting
                    .lock()
                    .expect("output buffer")
                    .extend_from_slice(&chunk[..read]);
            }
        });

        let mut threads = BTreeMap::new();
        threads.insert(
            pid,
            Thread {
                stopped: true,
                pending: 0,
                interrupting: false,
            },
        );
        let bias = load_bias(pid, &program);
        Ok(LinuxTarget {
            pid,
            current: pid,
            threads,
            saved: BTreeMap::new(),
            memory,
            program,
            bias,
            output,
            exited: None,
        })
    }

    fn registers(&self) -> Result<Registers, Error> {
        let regs = self.get_regs(self.current)?;
        Ok(Registers {
            pc: regs.rip,
            sp: regs.rsp,
            bp: regs.rbp,
        })
    }

    fn set_registers(&mut self, registers: Registers) -> Result<(), Error> {
        let mut regs = self.get_regs(self.current)?;
        regs.rip = registers.pc;
        regs.rsp = registers.sp;
        regs.rbp = registers.bp;
        self.set_regs(self.current, &regs)
    }

    /// Read the program's memory, showing the code it would run rather than
    /// the traps we planted in it.
    ///
    /// A debugger that lets its own `0xCC` bytes back out reports a program
    /// that is not the one the user wrote — a disassembly full of `int3`, and
    /// a byte comparison that fails for no visible reason.
    fn read_memory(&self, address: u64, into: &mut [u8]) -> Result<(), Error> {
        if into.is_empty() {
            return Ok(());
        }
        if self.memory.read_exact_at(into, address).is_err() {
            self.peek_into(address, into)?;
        }
        let end = address.wrapping_add(into.len() as u64);
        for (&at, &original) in self.saved.range(address..end) {
            into[(at - address) as usize] = original;
        }
        Ok(())
    }

    fn write_memory(&mut self, address: u64, from: &[u8]) -> Result<(), Error> {
        if from.is_empty() {
            return Ok(());
        }
        if self.memory.write_all_at(from, address).is_err() {
            self.poke_from(address, from)?;
        }
        Ok(())
    }

    fn resume(&mut self) -> Result<Stop, Error> {
        if let Some(code) = self.exited {
            return Ok(Stop::Exited(code));
        }
        if let Some(stop) = self.step_off_trap()? {
            return Ok(stop);
        }
        self.continue_all(false)?;
        self.wait_for_stop()
    }

    fn single_step(&mut self) -> Result<Stop, Error> {
        if let Some(code) = self.exited {
            return Ok(Stop::Exited(code));
        }
        // A step that starts on a trap is the same dance as resuming from one,
        // minus the trap going back before anything else moves — except that
        // here the step *is* the answer, so the trap is restored and we are
        // already where the user asked to be.
        let pc = self.get_regs(self.current)?.rip;
        let on_a_trap = self.armed(pc);
        if on_a_trap {
            self.disarm(pc)?;
        }
        let status = self.step_current()?;
        if let Some(stop) = self.ended(self.current, status)? {
            return Ok(stop);
        }
        if on_a_trap {
            self.arm(pc)?;
        }
        let signal = stop_signal(status);
        if signal != SIGTRAP {
            if let Some(thread) = self.threads.get_mut(&self.current) {
                thread.pending = if signal == SIGSTOP { 0 } else { signal };
            }
            return Ok(Stop::Signal(signal));
        }
        // A completed step reports its own SIGTRAP, and the program counter is
        // wherever the stepped instruction left it. It is NOT one past a trap,
        // and treating it as one is a trap of its own: when the instruction
        // just executed was a single byte, `rip - 1` is the address of that
        // instruction, and if a breakpoint happens to be planted there the
        // program counter is rewound onto code that has already run. The step
        // then makes no progress and the stepper spins.
        //
        // A step that lands *on* an armed address needs no special handling
        // here either: the next resume or step calls `step_off_trap`, which
        // restores the byte, steps, and re-arms.
        Ok(Stop::Step)
    }

    /// Stop a running program, for a Pause button.
    ///
    /// This is the one method that may be called from another thread, because
    /// it sends a signal rather than tracing: the stop it causes is collected
    /// by whichever thread is inside `resume`.
    fn interrupt(&mut self) -> Result<(), Error> {
        if self.exited.is_some() {
            return Ok(());
        }
        if unsafe { kill(self.pid, SIGSTOP) } < 0 {
            return Err(failed("could not interrupt the program"));
        }
        Ok(())
    }

    fn stop(&mut self) -> Result<(), Error> {
        if self.exited.is_some() {
            return Ok(());
        }
        unsafe { kill(self.pid, SIGKILL) };
        // Reaped rather than left: an unreaped tracee stays a zombie for as
        // long as we live, and `PTRACE_KILL` is documented as unreliable, so
        // the signal and the wait are what actually end it.
        loop {
            let mut status: c_int = 0;
            let tid = unsafe { waitpid(-1, &raw mut status, WAIT_FLAGS) };
            if tid < 0 {
                break;
            }
            if tid == self.pid && !stopped(status) {
                break;
            }
            if stopped(status) {
                unsafe {
                    ptrace(
                        request::CONT,
                        tid,
                        std::ptr::null_mut::<c_void>(),
                        SIGKILL as *mut c_void,
                    )
                };
            }
        }
        self.threads.clear();
        self.exited = Some(128 + SIGKILL);
        Ok(())
    }

    fn load_bias(&self) -> u64 {
        self.bias
    }

    /// Every object mapped into the program, for the unwinder.
    ///
    /// Read afresh every time rather than cached at launch, and that is the
    /// point of it: at the exec stop only the executable and the dynamic
    /// loader are mapped, and libc, SDL2 and the rest arrive later. A list
    /// taken once at the start describes a program that has not run yet, and
    /// the first pause inside libc would produce a one-frame stack that looks
    /// exactly like CFI running out.
    fn modules(&self) -> Result<Vec<Module>, Error> {
        let maps = std::fs::read_to_string(format!("/proc/{}/maps", self.pid))?;
        // Where each file was mapped, taken as the mapping address minus its
        // offset into the file — which is the same number for every mapping of
        // one object and is what the first byte of the file would sit at.
        let mut bases: BTreeMap<&str, u64> = BTreeMap::new();
        for line in maps.lines() {
            let mut fields = line.split_whitespace();
            let (Some(range), Some(_perms), Some(offset)) =
                (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            // The path is everything from the first `/`, not the sixth
            // whitespace-separated field: a directory name may contain a
            // space, and splitting on whitespace then truncates the path to
            // its first word. The mapping would be filed under a name no file
            // has, and the object — the program itself, if it is the one with
            // the space — would vanish from the unwinder's list. Nothing
            // before the path can contain a `/`: the device is `hh:hh` and the
            // inode is digits.
            let Some(slash) = line.find('/') else {
                continue;
            };
            let path = line[slash..].trim_end();
            // A file replaced while it is mapped keeps its mapping under a
            // marked name that cannot be opened.
            let path = path.strip_suffix(" (deleted)").unwrap_or(path);
            // Anonymous mappings and the kernel's own — `[stack]`, `[vdso]` —
            // have no file to read CFI out of. The vDSO does carry its own,
            // and reaching it would mean reading the object out of the process
            // rather than off disk; a stack that ends at a signal trampoline
            // is a smaller loss than pretending to have unwound one.
            if !path.starts_with('/') {
                continue;
            }
            let (Some(start), Some(offset)) = (
                range
                    .split('-')
                    .next()
                    .and_then(|s| u64::from_str_radix(s, 16).ok()),
                u64::from_str_radix(offset, 16).ok(),
            ) else {
                continue;
            };
            let base = start.wrapping_sub(offset);
            bases
                .entry(path)
                .and_modify(|known| *known = (*known).min(base))
                .or_insert(base);
        }

        let mut modules = Vec::new();
        for (path, base) in bases {
            let Ok(bytes) = std::fs::read(path) else {
                continue;
            };
            let Ok(file) = object::File::parse(&*bytes) else {
                continue;
            };
            let Some(section) = file.section_by_name(".eh_frame") else {
                continue;
            };
            let Ok(eh_frame) = section.uncompressed_data() else {
                continue;
            };
            // An executable is linked at a fixed address and a shared object
            // at zero, so the difference between where the file's first byte
            // landed and where its first loadable segment was linked is the
            // bias for both without a special case.
            let linked = file.segments().map(|s| s.address()).min().unwrap_or(0);
            modules.push(Module {
                bias: base.wrapping_sub(linked),
                eh_frame: eh_frame.into_owned(),
                eh_frame_address: section.address(),
            });
        }
        Ok(modules)
    }
}

/// Reading the stopped program's memory, for the unwinder.
///
/// The unwinder asks for eight bytes at a time and takes `None` for "not
/// mapped", which is what ends a walk that has run off the end of a stack.
impl Memory for LinuxTarget {
    fn read_u64(&self, address: u64) -> Option<u64> {
        let mut bytes = [0u8; 8];
        self.read_memory(address, &mut bytes).ok()?;
        Some(u64::from_le_bytes(bytes))
    }
}

/// A dropped target must not leave a stopped process behind.
///
/// `PTRACE_O_EXITKILL` covers the debugger dying; it does nothing for a
/// debugger that is still running and has simply let go. Without this a test
/// run leaves one stopped program per launch, waiting forever for a tracer
/// that has forgotten it.
impl Drop for LinuxTarget {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// The last syscall failure, said in the debugger's own words.
///
/// `Error` has no variant for a process that would not do as it was told, so
/// these arrive as `Io` — accurate enough, since every one of them is an
/// `errno`.
fn failed(what: &str) -> Error {
    Error::Io(std::io::Error::other(format!(
        "{what}: {}",
        std::io::Error::last_os_error()
    )))
}

/// What has to be added to an address in the debug information to reach the
/// address the program is actually running at.
///
/// Taken from `AT_ENTRY`, which the kernel puts in the auxiliary vector: it is
/// the entry point as loaded, and the ELF header holds the entry point as
/// linked, so the difference is the bias for the executable itself. Zero for
/// the non-relocatable programs OpenEPL builds today, and correct without a
/// change on the day it builds a position-independent one.
fn load_bias(pid: i32, program: &Path) -> u64 {
    /// The entry point, as the kernel actually loaded it.
    const AT_ENTRY: u64 = 9;

    let Ok(auxv) = std::fs::read(format!("/proc/{pid}/auxv")) else {
        return 0;
    };
    let mut loaded = None;
    for pair in auxv.chunks_exact(16) {
        let key = u64::from_le_bytes(pair[..8].try_into().expect("eight bytes"));
        if key == AT_ENTRY {
            loaded = Some(u64::from_le_bytes(
                pair[8..].try_into().expect("eight bytes"),
            ));
            break;
        }
    }
    let (Some(loaded), Ok(bytes)) = (loaded, std::fs::read(program)) else {
        return 0;
    };
    match object::File::parse(&*bytes) {
        Ok(file) => loaded.wrapping_sub(file.entry()),
        Err(_) => 0,
    }
}

/// `waitpid` reports through a bit-packed integer, and the macros that read it
/// live in a C header. These are those macros.
fn stopped(status: c_int) -> bool {
    status & 0xff == 0x7f
}

fn exited(status: c_int) -> bool {
    status & 0x7f == 0
}

fn exit_status(status: c_int) -> i32 {
    (status >> 8) & 0xff
}

fn termination_signal(status: c_int) -> i32 {
    status & 0x7f
}

fn stop_signal(status: c_int) -> c_int {
    (status >> 8) & 0xff
}

/// The `ptrace` event a stop carries, which is packed above the signal.
fn event(status: c_int) -> c_int {
    (status >> 16) & 0xff
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::unwind::Unwinder;

    /// Where `line 14` of `hello.oir` begins — `call print_int(answer)`.
    ///
    /// Taken from the line table rather than written down, so a change in the
    /// backend moves the test rather than breaking it.
    const LINE: u32 = 14;

    /// A built copy of `examples/hello.oir`, or nothing when the compiler has
    /// not been built yet.
    ///
    /// Skipping is said out loud. A test that quietly passes because it did
    /// not run is the exact failure this whole design is written against, and
    /// a `cargo test` before a `cargo build --release` is a real thing to do.
    fn fixture() -> Option<PathBuf> {
        // Built once for the whole test binary, not once per test. Every test
        // here wants the same program, and `cargo` runs them on parallel
        // threads: eight compilers writing one path means a test reading a
        // half-written ELF, which reports itself as "this is not an object
        // file" and looks like a bug in the debugger.
        static BUILT: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
        let built = BUILT.get_or_init(|| {
            let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .expect("the workspace root")
                .to_path_buf();
            let compiler = root.join("target/release/openepl");
            if !compiler.exists() {
                return None;
            }
            let built = PathBuf::from("/tmp/openepl-debug-linux-fixture");
            let made = std::process::Command::new(&compiler)
                .arg("build")
                .arg(root.join("examples/hello.oir"))
                .arg("-o")
                .arg(&built)
                .output()
                .expect("the compiler runs");
            assert!(
                made.status.success(),
                "could not build the fixture: {}",
                String::from_utf8_lossy(&made.stderr)
            );
            Some(built)
        });
        if built.is_none() {
            eprintln!(
                "skipped: target/release/openepl is not built, so there is \
                 nothing to trace"
            );
        }
        built.clone()
    }

    /// A launched fixture, and the address a breakpoint on `LINE` goes at.
    fn traced() -> Option<(LinuxTarget, u64)> {
        let built = fixture()?;
        let program = crate::load(&built).expect("the fixture carries debug information");
        let address = program
            .breakpoint_for(LINE)
            .expect("hello.oir line 14 runs something")
            .address;
        let target = LinuxTarget::launch(&built, &[]).expect("the fixture launches");
        Some((target, address))
    }

    #[test]
    fn a_launched_program_is_stopped_before_it_has_run() {
        let Some((mut target, _)) = traced() else { return };
        let registers = target.registers().expect("registers of a stopped program");
        // The first instruction is the dynamic loader's, not the program's,
        // and either way the program counter is somewhere real.
        assert_ne!(registers.pc, 0);
        assert_ne!(registers.sp, 0);
        assert!(target.running());
        // Nothing has run, so nothing has been printed.
        assert!(target.output().is_empty());
    }

    #[test]
    fn a_non_relocatable_program_has_no_bias() {
        let Some((target, _)) = traced() else { return };
        assert_eq!(target.load_bias(), 0);
    }

    #[test]
    fn two_breakpoints_in_one_word_keep_their_own_bytes() {
        let Some((mut target, _)) = traced() else {
            return;
        };
        // The first two rows of the line table are seven bytes apart, so both
        // addresses fall inside the same eight-byte word `POKEDATA` writes.
        // This is the case a word-keyed cache gets wrong.
        let built = fixture().expect("a fixture, since we have a target");
        let program = crate::load(&built).expect("debug information");
        let rows: Vec<u64> = program
            .rows()
            .iter()
            .filter(|r| !r.end_sequence)
            .map(|r| r.address)
            .take(2)
            .collect();
        let (first, second) = (rows[0], rows[1]);
        assert_eq!(first & !7, second & !7, "the two must share one word");

        let before = target.peek(first & !7).expect("the word before arming");
        target.arm(first).expect("the first trap");
        target.arm(second).expect("the second trap");

        // Both traps are in the program, and neither is in what we report.
        let armed = target.peek(first & !7).expect("the word with both traps");
        assert_eq!(armed.to_le_bytes()[(first & 7) as usize], 0xCC);
        assert_eq!(armed.to_le_bytes()[(second & 7) as usize], 0xCC);
        let mut shown = [0u8; 8];
        target
            .read_memory(first & !7, &mut shown)
            .expect("a masked read");
        assert_eq!(u64::from_le_bytes(shown), before);

        // Taking the first one out must restore the program's byte and leave
        // the second one's trap exactly where it was.
        target.disarm(first).expect("the first trap comes out");
        let left = target.peek(first & !7).expect("the word after disarming");
        assert_eq!(
            left.to_le_bytes()[(first & 7) as usize],
            before.to_le_bytes()[(first & 7) as usize],
            "the original byte did not come back"
        );
        assert_eq!(
            left.to_le_bytes()[(second & 7) as usize],
            0xCC,
            "disarming one breakpoint removed another's trap"
        );
        assert!(!target.armed(first));
        assert!(target.armed(second));
    }

    #[test]
    fn a_breakpoint_reports_its_own_address_and_not_the_one_after_it() {
        let Some((mut target, address)) = traced() else {
            return;
        };
        target.arm(address).expect("a trap on line 14");
        assert_eq!(
            target.resume().expect("running to the trap"),
            Stop::Breakpoint
        );
        assert_eq!(
            target.registers().expect("registers at the stop").pc,
            address,
            "the program counter was not rewound past the trap"
        );
    }

    #[test]
    fn stepping_off_a_trap_moves_on_and_leaves_the_trap_behind() {
        let Some((mut target, address)) = traced() else {
            return;
        };
        target.arm(address).expect("a trap");
        assert_eq!(target.resume().expect("run"), Stop::Breakpoint);

        assert_eq!(target.single_step().expect("one instruction"), Stop::Step);
        let moved = target.registers().expect("registers").pc;
        assert_ne!(moved, address, "the step did not leave the trap's address");
        assert!(
            target.armed(address),
            "the trap was not put back after stepping off it"
        );

        // And the program still finishes, which it cannot do if the byte under
        // the trap was lost or the trap was left in a place it now runs into.
        assert_eq!(target.resume().expect("run to the end"), Stop::Exited(0));
        assert!(!target.running());
    }

    #[test]
    fn the_programs_output_is_ours_to_read_and_never_ours_to_print() {
        let Some((mut target, _)) = traced() else {
            return;
        };
        assert_eq!(target.resume().expect("run to the end"), Stop::Exited(0));
        // The drain thread is racing the exit; the pipe is closed, so it is
        // only ever a few microseconds behind.
        let mut printed = String::new();
        for _ in 0..200 {
            printed = String::from_utf8_lossy(&target.output()).into_owned();
            if printed.contains("42") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            printed.contains("OpenEPL — arithmetic demo"),
            "the program's own output was lost: {printed:?}"
        );
        assert!(printed.contains("42"), "expected 6 * 7 in {printed:?}");
    }

    #[test]
    fn the_word_at_a_time_fallback_reads_and_writes_across_a_word_boundary() {
        let Some((target, address)) = traced() else {
            return;
        };
        // `/proc/<pid>/mem` answers on every kernel this runs on, so the
        // `PEEKDATA` path would otherwise never be exercised at all — and it
        // is the one that has to read a word back and patch it, which is where
        // a neighbour's bytes get lost.
        let word = address & !7;
        let mut before = [0u8; 16];
        target.peek_into(word, &mut before).expect("a raw read");

        let straddling = word + 5;
        let written = [1u8, 2, 3, 4, 5, 6];
        target
            .poke_from(straddling, &written)
            .expect("a raw write over a word boundary");
        let mut after = [0u8; 16];
        target.peek_into(word, &mut after).expect("a raw read back");

        assert_eq!(&after[5..11], &written, "the bytes did not land");
        assert_eq!(
            &after[..5],
            &before[..5],
            "the word before it was disturbed"
        );
        assert_eq!(
            &after[11..],
            &before[11..],
            "the word after it was disturbed"
        );

        target.poke_from(word, &before).expect("putting it back");
        let mut restored = [0u8; 16];
        target.peek_into(word, &mut restored).expect("a raw read");
        assert_eq!(restored, before);
    }

    #[test]
    fn the_child_is_launched_without_address_space_randomisation() {
        let Some((first, _)) = traced() else { return };
        let Some((second, _)) = traced() else { return };
        // Two runs of one program land in the same place, so an address the
        // user noted a moment ago still means something. This is what
        // `personality(ADDR_NO_RANDOMIZE)` buys, and it is only visible by
        // running twice.
        assert_eq!(
            first.registers().expect("registers").sp,
            second.registers().expect("registers").sp,
            "the stack moved between runs, so randomisation is still on"
        );
    }

    #[test]
    fn a_program_that_cannot_be_run_fails_rather_than_hanging() {
        let result = LinuxTarget::launch(Path::new("/nonexistent/openepl-not-a-program"), &[]);
        assert!(result.is_err(), "launching nothing reported success");
    }

    #[test]
    fn the_modules_describe_the_whole_stack_once_the_program_is_running() {
        let Some((mut target, address)) = traced() else {
            return;
        };
        target.arm(address).expect("a trap");
        assert_eq!(target.resume().expect("run"), Stop::Breakpoint);

        let modules = target.modules().expect("the mapped objects");
        assert!(
            modules.len() >= 2,
            "only {} object(s) mapped; libc arrives after the exec stop, so a \
             list taken once at launch would look like this",
            modules.len()
        );
        // The executable is not relocated, so exactly one module has no bias,
        // and the shared objects all have one.
        assert!(modules.iter().any(|m| m.bias == 0), "no executable found");
        assert!(
            modules.iter().any(|m| m.bias != 0),
            "no shared object found, so nothing was mapped after the exec stop"
        );

        let registers = target.registers().expect("registers at the stop");
        let mut unwinder = Unwinder::new(modules);
        let frames = unwinder.walk(registers, &target).expect("a stack");
        assert!(
            frames.len() >= 3,
            "expected oe_user_main, ECodeStart and main; got {} frame(s)",
            frames.len()
        );
        assert_eq!(frames[0].registers.pc, address);
        // The stack grows down, so each frame out is at a higher address.
        for pair in frames.windows(2) {
            assert!(
                pair[1].cfa > pair[0].cfa,
                "a caller's CFA was not above its callee's"
            );
        }
    }

    #[test]
    fn stopping_a_program_ends_it() {
        let Some((mut target, _)) = traced() else {
            return;
        };
        let pid = target.pid();
        target.stop().expect("the program stops");
        assert!(!target.running());
        // A killed and reaped tracee has no `/proc` entry left behind.
        assert!(
            !Path::new(&format!("/proc/{pid}/stat")).exists(),
            "the program is still there after being stopped"
        );
    }
}
