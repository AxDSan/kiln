//! Walking a stopped program's stack.
//!
//! The unwinder is pure: it is given the registers of the innermost frame and
//! something that can read the stopped program's memory, and it produces the
//! frames above. It never touches a process, which is what makes it testable
//! against a register set written by hand.
//!
//! It reads CFI — `.eh_frame` — rather than following the frame-pointer chain.
//! A frame-pointer walk is right only when a frame pointer is live at the
//! moment you stop, and in an Kiln program it is never live: clang omits
//! the frame pointer, so `kn_user_main` opens with `sub $0x238, %rsp` and
//! `ECodeStart` with `push %rax`, and neither ever writes `rbp`. Stopped
//! anywhere in the user's own code, `rbp` therefore still holds *`main`'s*
//! frame pointer, and following it yields `main`'s caller — dropping both
//! `ECodeStart` and `main` without any sign that anything was lost. CFI is
//! correct at every address, which is the whole reason it exists.

use crate::Error;
use gimli::{
    BaseAddresses, CfaRule, EhFrame, EndianSlice, LittleEndian, RegisterRule, UnwindContext,
    UnwindSection, X86_64,
};

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
    /// Where this frame is executing. For every frame but the innermost, `pc`
    /// is the address a call will *return* to rather than the address of the
    /// call itself.
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

/// The most frames a walk will produce.
///
/// A stack deeper than this is a runaway recursion rather than something
/// anyone means to read, and the walk has to end somewhere: CFI that has been
/// corrupted, or memory that has, can describe a stack with no end at all.
const MAX_FRAMES: usize = 512;

/// Walks stacks for one program.
pub struct Unwinder {
    modules: Vec<Module>,
    /// Reused across every lookup rather than built per frame, which is what
    /// `gimli` asks for: a context is where the CFI program's rows accumulate,
    /// and rebuilding it means reallocating on each frame of every walk.
    context: UnwindContext<usize>,
    /// The module the previous lookup was satisfied by, tried first on the
    /// next one. Consecutive frames are almost always in the same object, and
    /// a lookup that misses costs a linear scan of that object's every FDE.
    recent: usize,
}

/// One mapped object and the CFI inside it.
///
/// There is a list of these rather than one, because a GUI program links SDL2
/// as a shared object and it brings its own `.eh_frame`. Pausing an idle form
/// stops inside `libSDL2.so`, and an unwinder that only knows the executable's
/// CFI produces a one-frame stack for the most common "why is my form stuck"
/// gesture there is.
pub struct Module {
    /// Where the object is mapped, for turning a runtime address into the
    /// static one its CFI is written against.
    pub bias: u64,
    /// The object's `.eh_frame`.
    pub eh_frame: Vec<u8>,
    /// The address `.eh_frame` was linked at.
    ///
    /// The *static* address, as the section header records it — not the
    /// address the section ended up at once the object was mapped. The
    /// unwinder subtracts `bias` from a program counter before every lookup,
    /// so both sides of the comparison are static addresses. Handing it the
    /// runtime address instead fails silently: clang encodes FDE pointers
    /// `pcrel`, so every FDE would resolve to a runtime address, no lookup
    /// would match, and a full stack would come back as one frame that looks
    /// exactly like "the CFI ran out here".
    pub eh_frame_address: u64,
}

impl Unwinder {
    /// An unwinder over the objects mapped into one program.
    pub fn new(modules: Vec<Module>) -> Self {
        Unwinder {
            modules,
            context: UnwindContext::new(),
            recent: 0,
        }
    }

    /// The stack, innermost frame first.
    ///
    /// Stops when a frame cannot be unwound rather than failing: the outermost
    /// frames are libc's, which is exactly where the CFI runs out, and "the
    /// stack ends here" is the right answer rather than an error.
    ///
    /// The same applies to the innermost frame, which is why an empty stack is
    /// a possible answer: a program stopped in an object whose CFI we do not
    /// have cannot be described at all, and inventing a frame for it would
    /// mean inventing its CFA, which everything above compares against.
    pub fn walk(&mut self, top: Registers, memory: &dyn Memory) -> Result<Vec<Frame>, Error> {
        let mut frames: Vec<Frame> = Vec::new();
        let mut registers = top;

        while frames.len() < MAX_FRAMES {
            // A return address is the instruction *after* the call, and a call
            // that never returns can be the last instruction of its function —
            // so the return address is one past the end of the FDE describing
            // the caller. Every frame but the innermost is looked up one byte
            // back, and reported unchanged.
            let lookup = if frames.is_empty() {
                registers.pc
            } else {
                registers.pc.wrapping_sub(1)
            };

            let Some(rules) = self.rules_at(lookup) else {
                break;
            };
            let Some(cfa) = canonical_frame_address(&rules.cfa, &registers) else {
                break;
            };
            // Each frame out is further up the stack, and the stack grows
            // down, so the CFA must strictly increase. CFI that says otherwise
            // describes a cycle, and walking it would not terminate.
            if frames.last().is_some_and(|inner| cfa <= inner.cfa) {
                break;
            }
            frames.push(Frame { registers, cfa });

            let Some(pc) = return_address(&rules.return_address, cfa, memory) else {
                break;
            };
            if pc == 0 {
                break;
            }
            let Some(bp) = saved_register(&rules.frame_pointer, cfa, registers.bp, memory) else {
                break;
            };
            // The CFA is by definition the caller's stack pointer at the call,
            // which is where the caller's stack pointer returns to.
            registers = Registers { pc, sp: cfa, bp };
        }

        Ok(frames)
    }

    /// The unwind rules covering one address, from whichever module holds it.
    ///
    /// The module is chosen by asking each in turn for an FDE rather than by
    /// comparing against a mapped range, because a range is one more fact the
    /// process layer would have to supply correctly and "which object's CFI
    /// actually describes this address" is the question being asked anyway.
    fn rules_at(&mut self, pc: u64) -> Option<Rules> {
        let Unwinder {
            modules,
            context,
            recent,
        } = self;
        let count = modules.len();
        for step in 0..count {
            let index = (*recent + step) % count;
            let module = &modules[index];
            // An address below where an object is mapped is not in it, and
            // wrapping around into an enormous static address would make a
            // meaningless lookup rather than an obviously failed one.
            let Some(static_pc) = pc.checked_sub(module.bias) else {
                continue;
            };
            if let Some(rules) = rules_in(module, static_pc, context) {
                *recent = index;
                return Some(rules);
            }
        }
        None
    }
}

/// What the CFI says about one address: how to find this frame's CFA, and
/// where its caller's return address and frame pointer went.
///
/// Cloned out of `gimli`'s row rather than borrowed, because the row borrows
/// the unwind context and the next frame's lookup needs it back.
struct Rules {
    cfa: CfaRule<usize>,
    return_address: RegisterRule<usize>,
    frame_pointer: RegisterRule<usize>,
}

/// The unwind rules one module has for a static address, if it has any.
///
/// Little-endian is assumed rather than read from the object: both targets
/// Kiln builds for are x86-64, and a big-endian one would need a great deal
/// more than this line changed.
fn rules_in(module: &Module, pc: u64, context: &mut UnwindContext<usize>) -> Option<Rules> {
    let eh_frame: EhFrame<EndianSlice<'_, LittleEndian>> =
        EhFrame::new(&module.eh_frame, LittleEndian);
    let bases = BaseAddresses::default().set_eh_frame(module.eh_frame_address);

    let fde = eh_frame
        .fde_for_address(&bases, pc, EhFrame::cie_from_offset)
        .ok()?;
    // Which column holds the return address is the CIE's to say, not ours: it
    // is 16 on x86-64 today and the field exists because that is not a
    // universal truth.
    let return_address_column = fde.cie().return_address_register();
    let row = fde
        .unwind_info_for_address(&eh_frame, &bases, context, pc)
        .ok()?;

    Some(Rules {
        cfa: row.cfa().clone(),
        return_address: row.register(return_address_column),
        frame_pointer: row.register(X86_64::RBP),
    })
}

/// This frame's CFA, from the rule that describes it.
///
/// Both registers a CFA is ever based on must work. `rbp` is the textbook
/// case and `rsp` is the one every Kiln function actually uses, since clang
/// omits the frame pointer: supporting only the first would produce a correct
/// stack for hand-written C and a one-frame stack for the user's own program.
///
/// A CFA given as a DWARF expression is `None` — the same answer as no CFI at
/// all. `_start` and several of libc's functions use one, and they are the
/// outermost frames, where stopping is what we wanted anyway.
fn canonical_frame_address(rule: &CfaRule<usize>, registers: &Registers) -> Option<u64> {
    match rule {
        CfaRule::RegisterAndOffset { register, offset } => {
            let base = if *register == X86_64::RBP {
                registers.bp
            } else if *register == X86_64::RSP {
                registers.sp
            } else {
                return None;
            };
            Some(base.wrapping_add(*offset as u64))
        }
        CfaRule::Expression(_) => None,
    }
}

/// The caller's return address, or `None` when the stack ends here.
///
/// `Undefined` is how the outermost frame is marked — `_start` carries a
/// literal `DW_CFA_undefined: r16` — so it is a clean end rather than a
/// failure, and so is a rule this does not understand.
fn return_address(rule: &RegisterRule<usize>, cfa: u64, memory: &dyn Memory) -> Option<u64> {
    match rule {
        RegisterRule::Offset(offset) => memory.read_u64(cfa.wrapping_add(*offset as u64)),
        RegisterRule::ValOffset(offset) => Some(cfa.wrapping_add(*offset as u64)),
        RegisterRule::Constant(value) => Some(*value),
        _ => None,
    }
}

/// The caller's value of a callee-saved register.
///
/// A rule of `SameValue`, and the `Undefined` that a register no rule mentions
/// gets, both mean the current value carries through. That is not a guess: the
/// register is callee-saved, so a function that never saved it never changed
/// it — and it is the ordinary case here, because no Kiln function touches
/// `rbp` at all.
///
/// A rule this does not understand is `None`, which ends the walk. Carrying
/// the current value through instead would be a guess, and the frame above
/// would compute its CFA from it and be believed.
fn saved_register(
    rule: &RegisterRule<usize>,
    cfa: u64,
    current: u64,
    memory: &dyn Memory,
) -> Option<u64> {
    match rule {
        RegisterRule::Offset(offset) => memory.read_u64(cfa.wrapping_add(*offset as u64)),
        RegisterRule::ValOffset(offset) => Some(cfa.wrapping_add(*offset as u64)),
        RegisterRule::Constant(value) => Some(*value),
        RegisterRule::SameValue | RegisterRule::Undefined => Some(current),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A stack written out by hand: the addresses a test stored something at,
    /// and nothing else mapped.
    struct Stack(HashMap<u64, u64>);

    impl Stack {
        fn new(cells: &[(u64, u64)]) -> Stack {
            Stack(cells.iter().copied().collect())
        }
    }

    impl Memory for Stack {
        fn read_u64(&self, address: u64) -> Option<u64> {
            self.0.get(&address).copied()
        }
    }

    /// Assembles the `.eh_frame` a test unwinds against.
    ///
    /// Real bytes rather than a stubbed reader, because the encoding is where
    /// this goes wrong quietly: clang writes FDE pointers `pcrel|sdata4`, so
    /// every address in the section is stored relative to the section's own
    /// link address, and getting that base wrong yields no error at all — just
    /// FDEs that cover the wrong addresses and a lookup that finds nothing.
    /// The builder therefore uses the same encoding the fixture does.
    struct Cfi {
        address: u64,
        bytes: Vec<u8>,
    }

    /// `DW_CFA_advance_loc`, whose delta is packed into the opcode.
    fn advance(delta: u8) -> Vec<u8> {
        vec![0x40 | delta]
    }

    /// `DW_CFA_def_cfa_offset`: keep the CFA's register, change its offset.
    fn def_cfa_offset(offset: u64) -> Vec<u8> {
        let mut out = vec![0x0e];
        out.extend(uleb(offset));
        out
    }

    /// `DW_CFA_def_cfa_register`: keep the offset, change the register.
    fn def_cfa_register(register: u8) -> Vec<u8> {
        vec![0x0d, register]
    }

    /// `DW_CFA_def_cfa`: both at once.
    fn def_cfa(register: u8, offset: u64) -> Vec<u8> {
        let mut out = vec![0x0c, register];
        out.extend(uleb(offset));
        out
    }

    /// `DW_CFA_offset`: the register was saved at the CFA plus `factored`
    /// times the CIE's data alignment factor, which is -8 here.
    fn saved_at(register: u8, factored: u64) -> Vec<u8> {
        let mut out = vec![0x80 | register];
        out.extend(uleb(factored));
        out
    }

    /// `DW_CFA_undefined`: the register has no value in the caller. On the
    /// return address column it is how an object says the stack ends.
    fn undefined(register: u64) -> Vec<u8> {
        let mut out = vec![0x07];
        out.extend(uleb(register));
        out
    }

    fn uleb(mut value: u64) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            if value == 0 {
                out.push(byte);
                return out;
            }
            out.push(byte | 0x80);
        }
    }

    impl Cfi {
        /// A section holding one CIE, linked at `address`.
        ///
        /// The CIE is clang's: version 1, augmentation `zR` carrying a
        /// `pcrel|sdata4` FDE encoding, code alignment 1, data alignment -8,
        /// return address in column 16, and initial rules putting the CFA at
        /// `rsp + 8` with the return address just below it — which is exactly
        /// the machine state at a function's first instruction.
        fn new(address: u64) -> Cfi {
            let mut content = vec![0, 0, 0, 0, 1];
            content.extend_from_slice(b"zR\0");
            content.extend(uleb(1));
            content.push(0x78);
            content.push(16);
            content.extend(uleb(1));
            content.push(0x1b);
            content.extend(def_cfa(7, 8));
            content.extend(saved_at(16, 1));

            let mut cfi = Cfi {
                address,
                bytes: Vec::new(),
            };
            cfi.entry(content);
            cfi
        }

        /// An FDE covering `[low, low + length)`, with the instructions that
        /// describe how the CFA moves through it.
        fn fde(mut self, low: u64, length: u64, instructions: &[Vec<u8>]) -> Cfi {
            // The CIE is at offset zero and this field is four bytes into the
            // entry, and `.eh_frame` states the distance backwards.
            let pointer = (self.bytes.len() + 4) as u32;
            let mut content = pointer.to_le_bytes().to_vec();

            // `pcrel` is relative to the pointer field itself, so the value
            // depends on where in the section this FDE landed — past the
            // entry's own four-byte length prefix, and past the CIE pointer.
            let field = self.address + self.bytes.len() as u64 + 4 + content.len() as u64;
            content.extend((low.wrapping_sub(field) as i32).to_le_bytes());
            content.extend((length as i32).to_le_bytes());
            content.extend(uleb(0));
            for instruction in instructions {
                content.extend(instruction);
            }

            self.entry(content);
            self
        }

        /// Prefix an entry with its length and pad it out. Entries are aligned
        /// to the address size, and `DW_CFA_nop` is the padding.
        fn entry(&mut self, mut content: Vec<u8>) {
            while !(content.len() + 4).is_multiple_of(8) {
                content.push(0);
            }
            self.bytes.extend((content.len() as u32).to_le_bytes());
            self.bytes.extend(content);
        }

        fn module(self, bias: u64) -> Module {
            Module {
                bias,
                eh_frame_address: self.address,
                eh_frame: self.bytes,
            }
        }
    }

    /// A function with clang's textbook frame-pointer prologue: push `rbp`,
    /// then make it the frame pointer.
    fn frame_pointer_prologue() -> Vec<Vec<u8>> {
        vec![
            advance(1),
            def_cfa_offset(16),
            saved_at(6, 2),
            advance(3),
            def_cfa_register(6),
        ]
    }

    #[test]
    fn walks_two_frames_over_a_frame_pointer_cfa() {
        let cfi = Cfi::new(0x1000)
            .fde(0x2000, 0x40, &frame_pointer_prologue())
            .fde(0x3000, 0x40, &{
                let mut outer = frame_pointer_prologue();
                outer.push(undefined(16));
                outer
            });
        let mut unwinder = Unwinder::new(vec![cfi.module(0)]);

        // The innermost frame's `rbp` is 0x7000, so its CFA is 0x7010: the
        // return address sits at 0x7008 and the caller's `rbp` at 0x7000.
        let stack = Stack::new(&[(0x7008, 0x3020), (0x7000, 0x7100)]);
        let top = Registers {
            pc: 0x2010,
            sp: 0x6f00,
            bp: 0x7000,
        };

        let frames = unwinder.walk(top, &stack).unwrap();

        assert_eq!(frames.len(), 2, "{frames:?}");
        assert_eq!(frames[0].registers.pc, 0x2010);
        assert_eq!(frames[0].cfa, 0x7010);
        assert_eq!(frames[1].registers.pc, 0x3020);
        assert_eq!(frames[1].registers.sp, 0x7010);
        assert_eq!(frames[1].registers.bp, 0x7100);
        assert_eq!(frames[1].cfa, 0x7110);
    }

    #[test]
    fn walks_over_a_stack_pointer_cfa() {
        // `kn_user_main`'s own CFI, to the byte: `sub $0x238, %rsp` and
        // nothing else, so the CFA is the stack pointer for the whole
        // function and `rbp` is never mentioned.
        let cfi = Cfi::new(0x1000)
            .fde(0x2000, 0x400, &[advance(7), def_cfa_offset(576)])
            .fde(
                0x3000,
                0x40,
                &[advance(1), def_cfa_offset(16), advance(5), undefined(16)],
            );
        let mut unwinder = Unwinder::new(vec![cfi.module(0)]);

        // Stack pointer 0x6000 plus the 576-byte frame puts the CFA at
        // 0x6240 and the return address at 0x6238.
        let stack = Stack::new(&[(0x6238, 0x3010)]);
        let top = Registers {
            pc: 0x2100,
            sp: 0x6000,
            bp: 0x9990,
        };

        let frames = unwinder.walk(top, &stack).unwrap();

        assert_eq!(frames.len(), 2, "{frames:?}");
        assert_eq!(frames[0].cfa, 0x6240);
        assert_eq!(frames[1].registers.pc, 0x3010);
        assert_eq!(frames[1].registers.sp, 0x6240);
        assert_eq!(frames[1].cfa, 0x6250);
        // Neither function saves `rbp`, so the caller's value is the one still
        // in the register. Losing it here would lose it for every Kiln
        // frame, because no Kiln function saves it either.
        assert_eq!(frames[1].registers.bp, 0x9990);
    }

    #[test]
    fn a_callee_frame_has_a_smaller_cfa_than_its_caller() {
        let cfi = Cfi::new(0x1000)
            .fde(0x2000, 0x40, &frame_pointer_prologue())
            .fde(0x3000, 0x40, &frame_pointer_prologue())
            .fde(0x4000, 0x40, &{
                let mut outermost = frame_pointer_prologue();
                outermost.push(undefined(16));
                outermost
            });
        let mut unwinder = Unwinder::new(vec![cfi.module(0)]);

        let stack = Stack::new(&[
            (0x7008, 0x3020),
            (0x7000, 0x7100),
            (0x7108, 0x4020),
            (0x7100, 0x7200),
        ]);
        let top = Registers {
            pc: 0x2010,
            sp: 0x6f00,
            bp: 0x7000,
        };

        let frames = unwinder.walk(top, &stack).unwrap();

        assert_eq!(frames.len(), 3, "{frames:?}");
        // Everything downstream — every stepping decision in `step.rs` — reads
        // this direction, and reading it backwards turns "step in" into
        // "continue" with no error anywhere.
        for pair in frames.windows(2) {
            assert!(
                pair[0].cfa < pair[1].cfa,
                "callee CFA {:#x} is not below its caller's {:#x}",
                pair[0].cfa,
                pair[1].cfa
            );
        }
    }

    #[test]
    fn stops_rather_than_walking_a_cycle() {
        let cfi = Cfi::new(0x1000).fde(0x2000, 0x40, &frame_pointer_prologue());
        let mut unwinder = Unwinder::new(vec![cfi.module(0)]);

        // A stack that returns into itself and saves its own frame pointer.
        // Corrupt memory does this, and so does CFI read for the wrong
        // address; either way the frames repeat, and the walk has to notice
        // rather than run until it hits its ceiling.
        let stack = Stack::new(&[(0x7008, 0x2010), (0x7000, 0x7000)]);
        let top = Registers {
            pc: 0x2010,
            sp: 0x6f00,
            bp: 0x7000,
        };

        let frames = unwinder.walk(top, &stack).unwrap();

        // The second frame's CFA comes out equal to the first's, which cannot
        // happen on a stack that grows down.
        assert_eq!(frames.len(), 1, "{frames:?}");
    }

    #[test]
    fn stops_where_memory_stops() {
        let cfi = Cfi::new(0x1000).fde(0x2000, 0x40, &frame_pointer_prologue());
        let mut unwinder = Unwinder::new(vec![cfi.module(0)]);

        // Nothing is mapped, so the return address cannot be read.
        let stack = Stack::new(&[]);
        let top = Registers {
            pc: 0x2010,
            sp: 0x6f00,
            bp: 0x7000,
        };

        let frames = unwinder.walk(top, &stack).unwrap();

        // The frame we were given is still a fact; only its caller is unknown.
        assert_eq!(frames.len(), 1, "{frames:?}");
        assert_eq!(frames[0].cfa, 0x7010);
    }

    #[test]
    fn stops_where_the_cfi_stops() {
        let cfi = Cfi::new(0x1000).fde(0x2000, 0x40, &frame_pointer_prologue());
        let mut unwinder = Unwinder::new(vec![cfi.module(0)]);

        // The return address lands in libc, which this program has no CFI for.
        let stack = Stack::new(&[(0x7008, 0x7f0000001234), (0x7000, 0x7100)]);
        let top = Registers {
            pc: 0x2010,
            sp: 0x6f00,
            bp: 0x7000,
        };

        let frames = unwinder.walk(top, &stack).unwrap();

        assert_eq!(frames.len(), 1, "{frames:?}");
    }

    #[test]
    fn describes_nothing_when_it_has_no_cfi_at_all() {
        let mut unwinder = Unwinder::new(Vec::new());
        let frames = unwinder
            .walk(Registers::default(), &Stack::new(&[]))
            .unwrap();
        assert!(frames.is_empty());
    }

    #[test]
    fn crosses_from_a_shared_object_into_the_executable() {
        // Pausing an idle form stops inside `libSDL2.so`, which is mapped
        // somewhere far from the executable and carries its own `.eh_frame`.
        let library = Cfi::new(0x9000)
            .fde(0x5000, 0x40, &frame_pointer_prologue())
            .module(0x7f0000000000);
        let program = Cfi::new(0x1000)
            .fde(0x3000, 0x40, &{
                let mut outer = frame_pointer_prologue();
                outer.push(undefined(16));
                outer
            })
            .module(0);
        let mut unwinder = Unwinder::new(vec![program, library]);

        let stack = Stack::new(&[(0x7008, 0x3020), (0x7000, 0x7100)]);
        let top = Registers {
            pc: 0x7f0000005010,
            sp: 0x6f00,
            bp: 0x7000,
        };

        let frames = unwinder.walk(top, &stack).unwrap();

        assert_eq!(frames.len(), 2, "{frames:?}");
        assert_eq!(frames[0].registers.pc, 0x7f0000005010);
        assert_eq!(frames[0].cfa, 0x7010);
        assert_eq!(frames[1].registers.pc, 0x3020);
    }

    #[test]
    fn the_section_address_is_what_makes_a_pcrel_lookup_land() {
        // The same CFI, told the wrong link address for its own section. Every
        // FDE then covers addresses 0x800 away from the ones it describes, no
        // lookup matches, and the answer is an empty stack rather than an
        // error — which is why the field's meaning is spelled out on it.
        let mut cfi = Cfi::new(0x1000).fde(0x2000, 0x40, &frame_pointer_prologue());
        cfi.address = 0x1800;
        let mut unwinder = Unwinder::new(vec![cfi.module(0)]);

        let stack = Stack::new(&[(0x7008, 0x3020), (0x7000, 0x7100)]);
        let top = Registers {
            pc: 0x2010,
            sp: 0x6f00,
            bp: 0x7000,
        };

        assert!(unwinder.walk(top, &stack).unwrap().is_empty());
    }

    #[test]
    fn cfi_recovers_the_frames_a_frame_pointer_walk_drops() {
        // The three frames of a real Kiln program, with the CFI its own
        // build emits: `kn_user_main` at 0x400550 opening `sub $0x238, %rsp`,
        // `ECodeStart` at 0x400c30 opening `push %rax`, and `main` at 0x401660
        // with the only frame-pointer prologue in the stack.
        let cfi = Cfi::new(0x402808)
            .fde(0x400550, 0x572, &[advance(7), def_cfa_offset(576)])
            .fde(
                0x400c30,
                0xd,
                &[
                    advance(1),
                    def_cfa_offset(16),
                    advance(11),
                    def_cfa_offset(8),
                ],
            )
            .fde(
                0x401660,
                0x3d,
                &[
                    advance(1),
                    def_cfa_offset(16),
                    saved_at(6, 2),
                    advance(3),
                    def_cfa_register(6),
                    advance(56),
                    def_cfa(7, 8),
                ],
            );
        let mut unwinder = Unwinder::new(vec![cfi.module(0)]);

        // `main` holds a frame pointer of 0x8100, saving libc's 0x8300 below
        // it and its own return address above; `ECodeStart` and
        // `kn_user_main` then push return addresses without touching `rbp`.
        let stack = Stack::new(&[
            (0x8100, 0x8300),
            (0x8108, 0x7f0000001234),
            (0x80d8, 0x40168c),
            (0x80c8, 0x400c36),
        ]);
        // Stopped at `kn_user_main`'s very first instruction, which is where
        // the frame-pointer case is at its worst and where a breakpoint on the
        // subroutine header lands.
        let top = Registers {
            pc: 0x400550,
            sp: 0x80c8,
            bp: 0x8100,
        };

        let frames = unwinder.walk(top, &stack).unwrap();

        assert_eq!(frames.len(), 3, "{frames:?}");
        assert_eq!(frames[0].registers.pc, 0x400550);
        assert_eq!(frames[1].registers.pc, 0x400c36);
        assert_eq!(frames[2].registers.pc, 0x40168c);

        // The same stack walked by following `rbp`, which is what this is not
        // allowed to become: `rbp` still holds `main`'s frame pointer, so the
        // chain skips straight past `ECodeStart` and `main` into libc and
        // reports a two-frame stack with no sign that anything went missing.
        let mut chain = vec![top.pc];
        let mut bp = top.bp;
        while let (Some(caller), Some(outer)) = (stack.read_u64(bp + 8), stack.read_u64(bp)) {
            chain.push(caller);
            bp = outer;
        }
        assert_eq!(chain, vec![0x400550, 0x7f0000001234]);
    }
}
