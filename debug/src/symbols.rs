//! The line table and the functions it belongs to.

use crate::Error;
use object::{Object, ObjectSection, ObjectSymbol};
use std::path::Path;

/// One row of the line table: an address, and where in the source it came
/// from. This is DWARF's own shape, kept rather than reinvented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub address: u64,
    /// 1-based. A row with no line is dropped when the table is built, so this
    /// is never zero.
    pub line: u32,
    /// 1-based; 0 when the compiler recorded none.
    pub column: u32,
    /// Whether this is a place a breakpoint may go. A statement may compile to
    /// several rows and only the first is a statement boundary; stopping on
    /// any other would stop in the middle of a line.
    pub is_stmt: bool,
    /// Whether this row ends a sequence rather than starting one. It marks the
    /// address one past the last instruction, so it bounds the row before it
    /// and is never itself a stopping place.
    pub end_sequence: bool,
}

/// A local variable, as the compiler described it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variable {
    pub name: String,
    /// Its offset from the frame base, which for these binaries is `rbp`.
    pub frame_offset: i64,
    /// The type's name as the debug information spells it, which is what the
    /// value reader dispatches on.
    pub type_name: String,
    /// Which parameter this is, counting from one; `None` for a local the
    /// program declared in the body.
    pub parameter: Option<usize>,
    /// The address range of the subprogram it belongs to, so a frame can be
    /// matched to the variables in scope there.
    pub low_pc: u64,
}

/// A function, as the linker knows it.
///
/// These come from the symbol table rather than from DWARF: the compiler emits
/// line tables only, so there are no `DW_TAG_subprogram` entries to read yet.
/// The symbol table has what is needed — a name, an address and a size — and
/// it is what a `--release` build strips, which is the same condition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subprogram {
    /// The name in the source, with the compiler's prefix removed.
    pub name: String,
    /// The linker symbol.
    pub symbol: String,
    pub low_pc: u64,
    pub size: u64,
}

impl Subprogram {
    pub fn contains(&self, address: u64) -> bool {
        address >= self.low_pc && address < self.low_pc + self.size
    }
}

/// A built program's debug information, indexed for the questions a debugger
/// asks: which line is this address, and which address is this line.
#[derive(Debug)]
pub struct Program {
    /// The source file the compile unit names, as the compiler recorded it.
    pub source: String,
    /// The directory it was recorded relative to.
    pub directory: String,
    /// Every row, sorted by address. Sorted rather than left in the order the
    /// line program emitted them, because looking an address up is a binary
    /// search and the program's own order is not guaranteed to be ascending.
    rows: Vec<Row>,
    /// Every user function, sorted by address.
    subs: Vec<Subprogram>,
    /// Every local the compiler described, in the order it declared them.
    variables: Vec<Variable>,
}

impl Program {
    /// Every row, in address order.
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Every user function, in address order.
    pub fn subprograms(&self) -> &[Subprogram] {
        &self.subs
    }

    /// The locals in scope at an address, in declaration order.
    ///
    /// Scope is the whole subroutine: OpenEPL's locals are function-scoped, so
    /// a name declared anywhere in a body is a name that exists throughout it.
    /// A debugger showing one before its declaration has run shows whatever
    /// the slot happens to hold, which is the same thing the language itself
    /// would read.
    pub fn variables_at(&self, address: u64) -> Vec<&Variable> {
        let Some(sub) = self.subprogram_for(address) else {
            return Vec::new();
        };
        self.variables
            .iter()
            .filter(|v| v.low_pc == sub.low_pc)
            .collect()
    }

    /// Which source line an address is in.
    ///
    /// The row that covers an address is the last one at or before it, because
    /// a row states where a run of instructions *starts*. An address past the
    /// final `end_sequence` belongs to no row at all — it is the runtime, or
    /// libc, or anything else linked in without debug information.
    pub fn line_for(&self, address: u64) -> Option<&Row> {
        let i = match self.rows.binary_search_by_key(&address, |r| r.address) {
            Ok(i) => i,
            Err(0) => return None,
            Err(i) => i - 1,
        };
        let row = &self.rows[i];
        if row.end_sequence {
            None
        } else {
            Some(row)
        }
    }

    /// Where a breakpoint on a source line goes.
    ///
    /// The first statement boundary at or after the line asked for — "at or
    /// after" because a user may click a blank line or a comment, and the
    /// useful answer is the next line that runs rather than no answer at all.
    /// This is what every debugger does, and doing it here rather than in the
    /// caller means the CLI and the IDE cannot disagree about it.
    pub fn breakpoint_for(&self, line: u32) -> Option<&Row> {
        self.rows
            .iter()
            .filter(|r| r.is_stmt && !r.end_sequence && r.line >= line)
            .min_by_key(|r| (r.line, r.address))
    }

    /// Every address a line begins at. A line inside a loop body has one; a
    /// line reached from two branches may have several.
    pub fn addresses_for(&self, line: u32) -> Vec<u64> {
        self.rows
            .iter()
            .filter(|r| r.is_stmt && !r.end_sequence && r.line == line)
            .map(|r| r.address)
            .collect()
    }

    /// Which function an address is in, if it is in one of the user's.
    pub fn subprogram_for(&self, address: u64) -> Option<&Subprogram> {
        let i = match self.subs.binary_search_by_key(&address, |s| s.low_pc) {
            Ok(i) => i,
            Err(0) => return None,
            Err(i) => i - 1,
        };
        Some(&self.subs[i]).filter(|s| s.contains(address))
    }
}

/// The prefix the backend gives every user subroutine.
const USER_PREFIX: &str = "oe_user_";

/// What the backend writes as `DW_AT_producer`. Everything else in the binary
/// was compiled by something else and is not the user's code.
const PRODUCER: &str = "OpenEPL";

/// A compile unit's `DW_AT_producer`, or an empty string when it has none.
fn unit_producer(
    dwarf: &gimli::Dwarf<gimli::EndianSlice<gimli::RunTimeEndian>>,
    unit: &gimli::Unit<gimli::EndianSlice<gimli::RunTimeEndian>>,
) -> Result<String, Error> {
    let mut entries = unit.entries();
    let Some((_, root)) = entries.next_dfs()? else {
        return Ok(String::new());
    };
    let Some(attr) = root.attr(gimli::DW_AT_producer)? else {
        return Ok(String::new());
    };
    Ok(match dwarf.attr_string(unit, attr.value()) {
        Ok(s) => String::from_utf8_lossy(s.slice()).into_owned(),
        Err(_) => String::new(),
    })
}

pub(crate) fn load(path: &Path) -> Result<Program, Error> {
    let file = std::fs::File::open(path)?;
    // Mapped rather than read: a binary with its runtime statically linked is
    // megabytes, and all that is wanted is a few sections of it.
    let map = unsafe { memmap2::Mmap::map(&file)? };
    let object = object::File::parse(&*map)?;

    let endian = if object.is_little_endian() {
        gimli::RunTimeEndian::Little
    } else {
        gimli::RunTimeEndian::Big
    };

    // A missing section is empty rather than an error: DWARF says a producer
    // emits only the sections it needs, and `gimli` reads an empty one happily.
    let section = |id: gimli::SectionId| -> Result<&[u8], Error> {
        Ok(match object.section_by_name(id.name()) {
            Some(s) => s.data().unwrap_or(&[]),
            None => &[],
        })
    };
    let sections = gimli::DwarfSections::load(section)?;
    let dwarf = sections.borrow(|s| gimli::EndianSlice::new(s, endian));

    let mut source = String::new();
    let mut directory = String::new();
    let mut rows: Vec<Row> = Vec::new();
    let mut variables: Vec<Variable> = Vec::new();

    let mut units = dwarf.units();
    while let Some(header) = units.next()? {
        let unit = dwarf.unit(header)?;
        let Some(program) = unit.line_program.clone() else {
            continue;
        };
        // Only the units this compiler produced.
        //
        // A program links other people's code, and some of it arrives with
        // debug information of its own — glibc's `atexit.c` is in every binary
        // built here. Merging those rows into the user's table would put
        // `atexit.c`'s line 45 under the user's filename and step a user into
        // the C library. The producer string is what separates them, and it is
        // written by this compiler, so it is ours to rely on.
        if !unit_producer(&dwarf, &unit)?.starts_with(PRODUCER) {
            continue;
        }
        if source.is_empty() {
            if let Some(name) = unit.name {
                source = String::from_utf8_lossy(name.slice()).into_owned();
            }
            if let Some(dir) = unit.comp_dir {
                directory = String::from_utf8_lossy(dir.slice()).into_owned();
            }
        }
        variables.extend(read_variables(&dwarf, &unit)?);
        let mut state = program.rows();
        while let Some((_, row)) = state.next_row()? {
            // A row with no line is one the compiler could not attribute. It
            // is dropped rather than kept as line 0: a debugger that stopped
            // there would show no source, and a table that reports it as a
            // line would be lying.
            let line = match row.line() {
                Some(l) => l.get() as u32,
                None if row.end_sequence() => 0,
                None => continue,
            };
            rows.push(Row {
                address: row.address(),
                line,
                column: match row.column() {
                    gimli::ColumnType::Column(c) => c.get() as u32,
                    gimli::ColumnType::LeftEdge => 0,
                },
                is_stmt: row.is_stmt(),
                end_sequence: row.end_sequence(),
            });
        }
    }

    if rows.is_empty() {
        return Err(Error::NoDebugInfo);
    }
    rows.sort_by_key(|r| (r.address, r.end_sequence));

    let mut subs: Vec<Subprogram> = object
        .symbols()
        .filter(|s| s.kind() == object::SymbolKind::Text)
        .filter_map(|s| {
            let symbol = s.name().ok()?;
            let name = symbol.strip_prefix(USER_PREFIX)?;
            Some(Subprogram {
                name: name.to_string(),
                symbol: symbol.to_string(),
                low_pc: s.address(),
                size: s.size(),
            })
        })
        .collect();
    subs.sort_by_key(|s| s.low_pc);

    Ok(Program {
        source,
        directory,
        rows,
        subs,
        variables,
    })
}

/// The locals a compile unit describes, keyed to the function they belong to.
///
/// Read from the tree rather than guessed at: the compiler emits a
/// `DW_TAG_subprogram` per subroutine with its variables beneath it, and the
/// nesting is what says which function a name belongs to.
fn read_variables(
    dwarf: &gimli::Dwarf<gimli::EndianSlice<gimli::RunTimeEndian>>,
    unit: &gimli::Unit<gimli::EndianSlice<gimli::RunTimeEndian>>,
) -> Result<Vec<Variable>, Error> {
    let mut found = Vec::new();
    let mut low_pc = 0u64;
    let mut parameters = 0usize;
    let mut entries = unit.entries();
    while let Some((_, entry)) = entries.next_dfs()? {
        match entry.tag() {
            gimli::DW_TAG_subprogram => {
                low_pc = match entry.attr_value(gimli::DW_AT_low_pc)? {
                    Some(gimli::AttributeValue::Addr(a)) => a,
                    Some(gimli::AttributeValue::DebugAddrIndex(i)) => dwarf.address(unit, i)?,
                    _ => 0,
                };
                parameters = 0;
            }
            tag @ (gimli::DW_TAG_variable | gimli::DW_TAG_formal_parameter) => {
                if low_pc == 0 {
                    continue;
                }
                let Some(name) = entry.attr(gimli::DW_AT_name)? else {
                    continue;
                };
                let Ok(name) = dwarf.attr_string(unit, name.value()) else {
                    continue;
                };
                let name = String::from_utf8_lossy(name.slice()).into_owned();
                // Only a variable with a plain frame-relative location can be
                // read. Anything else — a register, a piece of an expression —
                // is skipped rather than guessed at, and at the optimisation
                // level a debug build uses there is nothing else.
                let Some(offset) = frame_offset(entry.attr_value(gimli::DW_AT_location)?) else {
                    continue;
                };
                let type_name = type_name_of(dwarf, unit, entry)?;
                let parameter = if tag == gimli::DW_TAG_formal_parameter {
                    parameters += 1;
                    Some(parameters)
                } else {
                    None
                };
                found.push(Variable {
                    name,
                    frame_offset: offset,
                    type_name,
                    parameter,
                    low_pc,
                });
            }
            _ => {}
        }
    }
    Ok(found)
}

/// The offset in a `DW_OP_fbreg` location, or `None` for any other kind.
fn frame_offset(
    location: Option<gimli::AttributeValue<gimli::EndianSlice<gimli::RunTimeEndian>>>,
) -> Option<i64> {
    let gimli::AttributeValue::Exprloc(expression) = location? else {
        return None;
    };
    let mut operations = expression.operations(gimli::Encoding {
        address_size: 8,
        format: gimli::Format::Dwarf32,
        version: 5,
    });
    match operations.next().ok()?? {
        gimli::Operation::FrameOffset { offset } => Some(offset),
        _ => None,
    }
}

/// The name of a variable's type, following one level of pointer so that a
/// heap record is named for the record rather than for the pointer to it.
fn type_name_of(
    dwarf: &gimli::Dwarf<gimli::EndianSlice<gimli::RunTimeEndian>>,
    unit: &gimli::Unit<gimli::EndianSlice<gimli::RunTimeEndian>>,
    entry: &gimli::DebuggingInformationEntry<gimli::EndianSlice<gimli::RunTimeEndian>>,
) -> Result<String, Error> {
    let Some(gimli::AttributeValue::UnitRef(offset)) = entry.attr_value(gimli::DW_AT_type)? else {
        return Ok(String::new());
    };
    let described = unit.entry(offset)?;
    if let Some(name) = described.attr(gimli::DW_AT_name)? {
        if let Ok(name) = dwarf.attr_string(unit, name.value()) {
            return Ok(String::from_utf8_lossy(name.slice()).into_owned());
        }
    }
    // An unnamed type is a pointer to a named one — a `text`, or a record on
    // the heap. The name that matters is the thing pointed at.
    if let Some(gimli::AttributeValue::UnitRef(inner)) = described.attr_value(gimli::DW_AT_type)? {
        let inner = unit.entry(inner)?;
        if let Some(name) = inner.attr(gimli::DW_AT_name)? {
            if let Ok(name) = dwarf.attr_string(unit, name.value()) {
                return Ok(String::from_utf8_lossy(name.slice()).into_owned());
            }
        }
    }
    Ok(String::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A table shaped like a real one: two statements, a non-statement row in
    /// the middle of the second, and the end marker.
    fn program() -> Program {
        Program {
            source: "demo.oir".into(),
            directory: "examples".into(),
            rows: vec![
                Row { address: 0x1000, line: 3, column: 3, is_stmt: true, end_sequence: false },
                Row { address: 0x1010, line: 5, column: 3, is_stmt: true, end_sequence: false },
                Row { address: 0x1018, line: 5, column: 9, is_stmt: false, end_sequence: false },
                Row { address: 0x1020, line: 7, column: 3, is_stmt: true, end_sequence: false },
                Row { address: 0x1030, line: 0, column: 0, is_stmt: false, end_sequence: true },
            ],
            subs: vec![Subprogram {
                name: "main".into(),
                symbol: "oe_user_main".into(),
                low_pc: 0x1000,
                size: 0x30,
            }],
            variables: vec![Variable {
                name: "total".into(),
                frame_offset: -8,
                type_name: "int".into(),
                parameter: None,
                low_pc: 0x1000,
            }],
        }
    }

    /// A row says where a run of instructions *starts*, so the row covering an
    /// address is the last one at or before it — not the nearest.
    #[test]
    fn an_address_belongs_to_the_row_that_starts_at_or_before_it() {
        let p = program();
        assert_eq!(p.line_for(0x1000).unwrap().line, 3);
        assert_eq!(p.line_for(0x1008).unwrap().line, 3);
        assert_eq!(p.line_for(0x1010).unwrap().line, 5);
        assert_eq!(p.line_for(0x101c).unwrap().line, 5);
        assert_eq!(p.line_for(0x1020).unwrap().line, 7);
    }

    /// Before the first row and past the end marker there is no line, and
    /// saying so is the answer — those are the runtime's addresses.
    #[test]
    fn an_address_outside_the_table_has_no_line() {
        let p = program();
        assert!(p.line_for(0x900).is_none());
        assert!(p.line_for(0x1030).is_none());
        assert!(p.line_for(0x2000).is_none());
    }

    /// A breakpoint goes on a statement boundary. Line 5's second row is in
    /// the middle of the line and must never be chosen.
    #[test]
    fn a_breakpoint_goes_on_a_statement_boundary() {
        let p = program();
        assert_eq!(p.breakpoint_for(5).unwrap().address, 0x1010);
        assert_eq!(p.breakpoint_for(5).unwrap().column, 3);
    }

    /// A blank line or a comment runs nothing. The useful answer is the next
    /// line that does, which is what every debugger does.
    #[test]
    fn a_line_that_runs_nothing_moves_to_the_next_one_that_does() {
        let p = program();
        assert_eq!(p.breakpoint_for(4).unwrap().line, 5);
        assert_eq!(p.breakpoint_for(6).unwrap().line, 7);
        assert!(p.breakpoint_for(8).is_none());
    }

    #[test]
    fn a_line_reports_every_address_it_begins_at() {
        let p = program();
        assert_eq!(p.addresses_for(5), vec![0x1010]);
        assert_eq!(p.addresses_for(4), Vec::<u64>::new());
    }

    /// A function's extent is half-open: the byte one past its last is the
    /// next function's first, and claiming both would put an address in two.
    /// A local belongs to the whole subroutine, because OpenEPL's locals are
    /// function-scoped — and to no other.
    #[test]
    fn the_locals_of_an_address_are_the_ones_its_function_declared() {
        let p = program();
        let names: Vec<&str> = p.variables_at(0x1010).iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, vec!["total"]);
        assert!(p.variables_at(0x9999).is_empty());
    }

    #[test]
    fn a_subprogram_owns_its_addresses_and_not_the_one_past_its_end() {
        let p = program();
        assert_eq!(p.subprogram_for(0x1000).unwrap().name, "main");
        assert_eq!(p.subprogram_for(0x102f).unwrap().name, "main");
        assert!(p.subprogram_for(0x1030).is_none());
        assert!(p.subprogram_for(0xfff).is_none());
    }
}
