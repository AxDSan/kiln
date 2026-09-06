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
//! One clause belongs to the first two rows of that table and is not in it: a
//! step that meant to stop in its own frame stops in the *caller* too, when
//! the frame returns before the step could finish. `next` on a subroutine's
//! last statement is exactly that, and it is why a step over traps the return
//! address at all — with a bare `==` the return trap could never be the one we
//! meant, and `next` off the end of a subroutine would silently become
//! `continue`.
//!
//! **Leaving the user's outermost frame means continue, not stop.** A click
//! handler is called from the runtime, under a dozen frames of C and C++ that
//! carry no line table. Stepping out of the handler lands there, and there is
//! no source to show — so the stepper resumes instead of reporting a stop the
//! IDE cannot draw. Whether an address is the user's is decided by whether the
//! line table covers it, never by a symbol prefix: the runtime's own C is
//! compiled into the same binary and would pass a prefix test.

use crate::symbols::{Program, Row, Subprogram};
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
    ///
    /// `stack` is the stopped stack, innermost frame first, as the unwinder
    /// produced it. A step is planned from where the program is *now*, so
    /// everything the plan needs is read out of `stack[0]` and the frame above
    /// it; a plan outlives neither.
    pub fn new(kind: Step, program: &Program, stack: &[Frame]) -> Plan {
        let pc = stack.first().map(|f| f.registers.pc);
        let at = pc.and_then(|pc| program.line_for(pc));
        let inside = pc.and_then(|pc| program.subprogram_for(pc));

        // A line the user picked may run nothing — they clicked a comment, or
        // the blank line after a loop. Moving to the next line that does run
        // is what a breakpoint on it would do, and running to the cursor has
        // to agree with breaking on it or the two features contradict each
        // other on the same click.
        let kind = match kind {
            Step::ToLine(line) => {
                Step::ToLine(program.breakpoint_for(line).map_or(line, |row| row.line))
            }
            other => other,
        };

        Plan::from_parts(
            kind,
            program.rows(),
            program.subprograms(),
            inside,
            at,
            stack,
        )
    }

    /// Whether a trap that fired is the end of this step, or something to
    /// resume through.
    ///
    /// `stack[0].registers.pc` must be the trapped address itself. A trap is
    /// an `int3`, so the stopped `rip` is one byte past it and the process
    /// layer rewinds it before unwinding; a plan asked about an un-rewound
    /// program counter would look the byte up in the previous row and judge
    /// the step against the wrong line.
    pub fn arrived(&self, program: &Program, stack: &[Frame]) -> bool {
        let at = stack.first().and_then(|f| program.line_for(f.registers.pc));
        self.arrived_at(at, stack)
    }

    /// Planning, once every question that needs the whole `Program` has been
    /// asked of it: the rows and the subroutines it holds, plus the row and
    /// the subroutine covering the stopped program counter.
    ///
    /// Split out because the rules below are the part worth testing and a
    /// `Program` can only be built by reading a real binary. Nothing here
    /// searches for a covering row or a containing function — those searches
    /// live in `symbols.rs`, are tested there, and are not repeated.
    fn from_parts(
        kind: Step,
        rows: &[Row],
        subs: &[Subprogram],
        inside: Option<&Subprogram>,
        at: Option<&Row>,
        stack: &[Frame],
    ) -> Plan {
        let from_cfa = stack.first().map_or(0, |f| f.cfa);
        let from_line = at.map_or(0, |row| row.line);
        // The frame above the stopped one is executing the call, so its
        // program counter is already the address that call returns to.
        let returns_to = stack.get(1).map(|f| f.registers.pc);

        let mut traps = Vec::new();
        match kind {
            Step::ToLine(line) => traps.extend(
                rows.iter()
                    .filter(|r| boundary(r) && r.line == line)
                    .map(|r| r.address),
            ),
            Step::Out => traps.extend(returns_to),
            Step::Over | Step::In => {
                // Every other statement of this subroutine, so the step ends
                // at whichever one control reaches next. The line we are on is
                // left untrapped deliberately: a loop's back edge lands on its
                // own first instruction, and trapping it would stop `next` on
                // the line it started on, every iteration, for ever.
                if let Some(sub) = inside {
                    // From the body rather than from `low_pc`: the prologue's
                    // row is reachable only by entering the subroutine again,
                    // and a recursive call has to stop where every other call
                    // into it stops. Trapping the prologue would put a step in
                    // on the one instruction whose arguments are not stored
                    // yet, which is what `prologue_end` exists to avoid.
                    let body = prologue_end(rows, sub).unwrap_or(sub.low_pc);
                    traps.extend(
                        rows.iter()
                            .filter(|r| {
                                boundary(r)
                                    && sub.contains(r.address)
                                    && r.address >= body
                                    && r.line != from_line
                            })
                            .map(|r| r.address),
                    );
                }
                traps.extend(returns_to);
                if kind == Step::In {
                    // Every user subroutine, not only the ones this line looks
                    // like it calls: which one a call reaches is a question
                    // about values, and the CFA test below throws away the
                    // traps that were not on the way. This subroutine is in
                    // the set too, so stepping into a recursive call arrives.
                    traps.extend(subs.iter().filter_map(|sub| prologue_end(rows, sub)));
                }
            }
        }

        traps.sort_unstable();
        traps.dedup();
        Plan {
            traps,
            from_cfa,
            from_line,
            kind,
        }
    }

    /// Judging a trap, once the row covering it has been looked up.
    fn arrived_at(&self, at: Option<&Row>, stack: &[Frame]) -> bool {
        let (Some(frame), Some(row)) = (stack.first(), at) else {
            // No row means the line table does not cover this address, so it
            // is the runtime's, or a library's, or libc's. There is no source
            // to show and the answer is to resume. This is tested before every
            // frame rule below, because stepping out of the outermost user
            // frame satisfies `>` and still has nothing to draw.
            return false;
        };
        match self.kind {
            Step::ToLine(line) => row.line == line,
            Step::Out => frame.cfa > self.from_cfa,
            Step::In => frame.cfa < self.from_cfa || self.settled(frame.cfa, row.line),
            Step::Over => self.settled(frame.cfa, row.line),
        }
    }

    /// Whether a trap in this frame, or in one the step returned into, is
    /// somewhere new. A deeper frame is never it: for a step over that is the
    /// call the step exists to run through, and for a step in it is handled
    /// before this is asked.
    fn settled(&self, cfa: u64, line: u32) -> bool {
        if cfa > self.from_cfa {
            return true;
        }
        cfa == self.from_cfa && line != self.from_line
    }
}

/// Whether a row is somewhere a step may stop.
///
/// A statement compiles to several rows and only the first is a boundary;
/// stopping on any other stops in the middle of a line. The row that ends a
/// sequence marks the address one past the last instruction and is not a
/// place at all.
fn boundary(row: &Row) -> bool {
    row.is_stmt && !row.end_sequence
}

/// Where a call into `sub` is trapped.
///
/// Never `low_pc`. The prologue is what runs between there and the body, and
/// it is where the arguments are written into their slots: stopping before it
/// shows the subroutine's header line with none of its arguments, and every
/// local reading as whatever the stack last held. The body starts at the first
/// statement boundary on a line other than the header's — the prologue is
/// attributed to that header line and is the only thing on it. DWARF marks the
/// row `prologue_end` and `Row` does not carry the flag yet, so the change of
/// line stands in for it.
fn prologue_end(rows: &[Row], sub: &Subprogram) -> Option<u64> {
    let mut inside = rows
        .iter()
        .filter(|r| boundary(r) && sub.contains(r.address));
    let header = inside.next()?;
    Some(
        inside
            .find(|r| r.line != header.line)
            .map_or(header.address, |r| r.address),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::unwind::Registers;

    /// A table shaped like one this compiler emits: `main` with a loop in it,
    /// and a second subroutine to step into. Line 3 is `main`'s header, so its
    /// row is the prologue; line 6 is a loop body reached again from line 7's
    /// back edge; line 8 runs nothing.
    fn rows() -> Vec<Row> {
        let row = |address, line, is_stmt| Row {
            address,
            line,
            column: 3,
            is_stmt,
            end_sequence: false,
        };
        vec![
            row(0x1000, 3, true),
            row(0x1008, 5, true),
            row(0x1010, 6, true),
            row(0x1018, 6, false),
            row(0x1020, 7, true),
            row(0x1028, 9, true),
            row(0x1040, 12, true),
            row(0x1048, 13, true),
            row(0x1050, 14, true),
            Row {
                address: 0x1060,
                line: 0,
                column: 0,
                is_stmt: false,
                end_sequence: true,
            },
        ]
    }

    fn subs() -> Vec<Subprogram> {
        vec![
            Subprogram {
                name: "main".into(),
                symbol: "oe_user_main".into(),
                low_pc: 0x1000,
                size: 0x40,
            },
            Subprogram {
                name: "helper".into(),
                symbol: "oe_user_helper".into(),
                low_pc: 0x1040,
                size: 0x20,
            },
        ]
    }

    /// The row starting at an address. Every address these tests stop at is a
    /// row's first byte, so the covering-row search `Program::line_for` does —
    /// and `symbols.rs` tests — is not needed here and not repeated.
    fn at(address: u64) -> Row {
        rows().into_iter().find(|r| r.address == address).unwrap()
    }

    fn frame(pc: u64, cfa: u64) -> Frame {
        Frame {
            registers: Registers {
                pc,
                sp: cfa - 0x20,
                bp: 0,
            },
            cfa,
        }
    }

    /// Stopped on line 6, inside `main`'s loop, called from an address in the
    /// runtime that no line covers.
    fn in_the_loop() -> Vec<Frame> {
        vec![frame(0x1010, 0x7000), frame(0x2000, 0x7100)]
    }

    fn plan(kind: Step, stack: &[Frame]) -> Plan {
        let rows = rows();
        let subs = subs();
        let pc = stack[0].registers.pc;
        let at = rows.iter().find(|r| r.address == pc);
        let inside = subs.iter().find(|s| s.contains(pc));
        Plan::from_parts(kind, &rows, &subs, inside, at, stack)
    }

    #[test]
    fn a_step_over_traps_the_rest_of_its_own_subroutine_and_the_return() {
        let over = plan(Step::Over, &in_the_loop());

        // Line 6 is missing on purpose — see the back-edge test below — and so
        // is 0x1018, which is line 6's second row and not a boundary. 0x1000 is
        // the prologue, reachable only by re-entering `main`. 0x1040 onwards
        // belongs to `helper` and a step over does not enter it.
        assert_eq!(over.traps, vec![0x1008, 0x1020, 0x1028, 0x2000]);
        assert_eq!(over.from_cfa, 0x7000);
        assert_eq!(over.from_line, 6);
    }

    #[test]
    fn a_step_in_adds_every_subroutines_prologue_end() {
        let step_in = plan(Step::In, &in_the_loop());

        // 0x1048 is `helper`'s, and 0x1008 is `main`'s own, so a recursive
        // call arrives. Neither subroutine's `low_pc` is in the set: stopping
        // at 0x1000 or 0x1040 would stop before the arguments were stored,
        // and the recursive case is the one that hides it, because `main`'s
        // header row is a trap candidate on its own account as well.
        assert_eq!(step_in.traps, vec![0x1008, 0x1020, 0x1028, 0x1048, 0x2000]);
        assert!(!step_in.traps.contains(&0x1000));
        assert!(!step_in.traps.contains(&0x1040));
    }

    #[test]
    fn a_step_out_traps_only_the_return_address() {
        assert_eq!(plan(Step::Out, &in_the_loop()).traps, vec![0x2000]);
    }

    #[test]
    fn running_to_a_line_traps_that_line_alone() {
        let to_line = plan(Step::ToLine(13), &in_the_loop());
        assert_eq!(to_line.traps, vec![0x1048]);
        // The frame it started in is still recorded, but nothing consults it:
        // running to a line means that line in whatever frame reaches it.
        assert_eq!(to_line.from_cfa, 0x7000);
    }

    /// The direction test for a step over. A trap in a deeper frame is the
    /// call this step exists to run straight through; one in a shallower frame
    /// is the subroutine having returned, which ends the step wherever it
    /// meant to end.
    #[test]
    fn a_step_over_stays_in_its_own_frame_and_is_not_fooled_by_a_deeper_one() {
        let over = plan(Step::Over, &in_the_loop());

        assert!(over.arrived_at(Some(&at(0x1020)), &[frame(0x1020, 0x7000)]));
        assert!(!over.arrived_at(Some(&at(0x1048)), &[frame(0x1048, 0x6f00)]));
        assert!(over.arrived_at(Some(&at(0x1028)), &[frame(0x1028, 0x7100)]));
    }

    /// The direction test for a step in, and the one the module doc warns
    /// about: swap `<` for `>` and a callee's first instruction stops
    /// satisfying it, so the step silently becomes a continue.
    #[test]
    fn a_step_in_arrives_in_a_deeper_frame_whatever_line_it_lands_on() {
        let step_in = plan(Step::In, &in_the_loop());

        assert!(step_in.arrived_at(Some(&at(0x1048)), &[frame(0x1048, 0x6f00)]));
        // A recursive call reaches the same subroutine on the line the step
        // started on, so nothing but the CFA distinguishes it from standing
        // still, and nothing but the CFA's direction distinguishes it from the
        // caller. Swap `<` for `>` and this one goes false.
        assert!(step_in.arrived_at(Some(&at(0x1010)), &[frame(0x1010, 0x6f00)]));
        // Standing still in the same frame is not a step in.
        assert!(!step_in.arrived_at(Some(&at(0x1010)), &[frame(0x1010, 0x7000)]));
    }

    /// The direction test for a step out. A recursive call is the case that
    /// separates the two: it reaches the same subroutine on the same line, and
    /// only the CFA says it is the wrong way.
    #[test]
    fn a_step_out_arrives_in_a_shallower_frame_and_not_a_deeper_one() {
        let out = plan(Step::Out, &in_the_loop());

        assert!(out.arrived_at(Some(&at(0x1028)), &[frame(0x1028, 0x7100)]));
        assert!(!out.arrived_at(Some(&at(0x1010)), &[frame(0x1010, 0x6f00)]));
        assert!(!out.arrived_at(Some(&at(0x1020)), &[frame(0x1020, 0x7000)]));
    }

    /// One source line is several instructions. A trap that fires on another
    /// of them has not moved anywhere the user can see.
    #[test]
    fn the_same_line_in_the_same_frame_is_not_an_arrival() {
        let over = plan(Step::Over, &in_the_loop());
        assert!(!over.arrived_at(Some(&at(0x1010)), &[frame(0x1010, 0x7000)]));

        let step_in = plan(Step::In, &in_the_loop());
        assert!(!step_in.arrived_at(Some(&at(0x1010)), &[frame(0x1010, 0x7000)]));
    }

    /// A loop's back edge returns to its body's own first instruction. Miss
    /// this and `next` never leaves a loop body: it stops on the same line
    /// again on every iteration.
    #[test]
    fn a_loops_back_edge_is_neither_trapped_nor_an_arrival() {
        let over = plan(Step::Over, &in_the_loop());

        assert!(!over.traps.contains(&0x1010));
        // Trapped anyway, because a user breakpoint sits on the same address:
        // the trap set and the arrival rule have to reject it independently.
        assert!(!over.arrived_at(Some(&at(0x1010)), &[frame(0x1010, 0x7000)]));
    }

    /// Stepping out of a click handler lands in RmlUi, which is built without
    /// debug information. Reporting a stop there gives the IDE a frame it
    /// cannot draw, so the step resumes instead.
    #[test]
    fn leaving_the_users_code_resumes_rather_than_stopping() {
        let runtime = [frame(0x2000, 0x7100)];

        // Shallower, which is exactly what a step out asks for, and still not
        // an arrival — so the user-code test has to come first.
        assert!(!plan(Step::Out, &in_the_loop()).arrived_at(None, &runtime));
        assert!(!plan(Step::Over, &in_the_loop()).arrived_at(None, &runtime));
        assert!(!plan(Step::In, &in_the_loop()).arrived_at(None, &runtime));
        assert!(!plan(Step::ToLine(13), &in_the_loop()).arrived_at(None, &runtime));
    }

    /// `next` on a subroutine's last statement has nowhere left in the frame
    /// to stop, and the return address is the only trap that can fire.
    #[test]
    fn a_step_over_the_last_statement_arrives_in_the_caller() {
        let last = vec![frame(0x1028, 0x7000), frame(0x1048, 0x7100)];
        let over = plan(Step::Over, &last);

        assert!(over.traps.contains(&0x1048));
        assert!(over.arrived_at(Some(&at(0x1048)), &[frame(0x1048, 0x7100)]));

        // A step in ends there too: it is a step over that was willing to
        // descend, and there was nothing to descend into.
        let step_in = plan(Step::In, &last);
        assert!(step_in.arrived_at(Some(&at(0x1048)), &[frame(0x1048, 0x7100)]));
    }

    /// A step in on a line that calls nothing is a step over. It has to be, or
    /// F11 on an assignment would run to the end of the program.
    #[test]
    fn a_step_in_that_finds_no_call_ends_on_the_next_line_of_this_frame() {
        let step_in = plan(Step::In, &in_the_loop());

        assert!(step_in.traps.contains(&0x1020));
        assert!(step_in.arrived_at(Some(&at(0x1020)), &[frame(0x1020, 0x7000)]));
    }

    /// A step planned before the stack was walked, or after the program
    /// exited. It traps nothing and arrives nowhere rather than panicking on
    /// an index.
    #[test]
    fn a_step_with_no_stack_traps_nothing() {
        let rows = rows();
        let subs = subs();
        let over = Plan::from_parts(Step::Over, &rows, &subs, None, None, &[]);

        assert!(over.traps.is_empty());
        assert_eq!(over.from_cfa, 0);
        assert_eq!(over.from_line, 0);
        assert!(!over.arrived_at(Some(&at(0x1010)), &[]));
    }

    /// Stopped in the runtime — paused rather than stepped there — a step over
    /// has no subroutine to enumerate and only the return address to trap.
    #[test]
    fn a_step_over_from_outside_the_users_code_traps_the_return_only() {
        let outside = vec![frame(0x2000, 0x7000), frame(0x1028, 0x7100)];
        let over = plan(Step::Over, &outside);

        assert_eq!(over.traps, vec![0x1028]);
    }
}
