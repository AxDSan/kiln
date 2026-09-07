//! Rendering a Kiln value out of a stopped program's memory.
//!
//! This is the layer that makes the debugger Kiln's rather than a small
//! gdb, and every rule in it comes from something only the compiler knows:
//!
//! - **Arrays are 1-based.** `names = [1] "ada", [2] "grace"`, because that is
//!   what the language means by an index. A debugger that does not know the
//!   language has to hedge, and hedging about indexing is unusable.
//! - **`text` is a NUL-terminated UTF-8 pointer**, and `NULL` means empty
//!   rather than absent. It renders as the characters the user wrote.
//! - **An optional has no runtime representation.** It is two locals — the
//!   value, and a hidden companion holding whether it is there. Nothing in
//!   memory distinguishes an absent `int` from zero; only the emitter knows
//!   they are a pair, which is why `nothing` can be printed at all.
//! - **Record field names do not reach a shipped binary.** They exist here
//!   only because the compiler wrote them into the debug information.
//! - **Compiler-invented locals are hidden.** They are the only names
//!   containing `$`, and `$` is not a character an identifier may contain, so
//!   filtering them is exact rather than a guess.
//!
//! Nothing here ever reports a blank. A value that cannot be read renders as
//! [`Value::Unreadable`] carrying why, because a blank row in a Variables pane
//! is indistinguishable from a bug in the pane.

use crate::unwind::Memory;
use crate::Error;

/// The width of one stored value, mirroring the value union of
/// `Kiln_Slot` in `abi/kiln_abi.h`. An array element, a heap record's
/// field and a dictionary's value are each one of these whatever narrower type
/// they hold, because the runtime widens everything it stores to 64 bits.
const CELL_BYTES: u64 = 8;

/// `Kiln_Array` in `abi/kiln_abi.h`: `{ int32 elem_tag; int32 len;
/// int32 cap; int32 _pad; }`, with the elements laid out immediately after it
/// (`runtime/kn_array.c:29`).
const ARRAY_HEADER_BYTES: u64 = 16;

/// Where `len` sits inside that header, one `int32` past `elem_tag`.
const ARRAY_LEN_OFFSET: u64 = 4;

/// `Kiln_Record` in `runtime/kiln_core.h`: `{ int32 count; int32 _pad; }`
/// followed by `count` cells (`runtime/kn_record.c:23`). Only the count is read
/// from it; every field's position comes from the debug information, which
/// already measures from the start of this header.
const RECORD_HEADER_BYTES: u64 = 8;

/// Where the entry block hangs off `Kiln_Dict`
/// (`runtime/kiln_core.h:46`): three `int32`s and a pad, then the pointer.
const DICT_ENTRIES_OFFSET: u64 = 16;

/// Where `len` sits in that same header, one `int32` past `val_tag`.
const DICT_LEN_OFFSET: u64 = 4;

/// One `Kiln_DictEntry` (`runtime/kiln_core.h:34`): a `char *` key and
/// an `int64` value, in that order.
const DICT_ENTRY_BYTES: u64 = 16;

/// Where `len` sits in `Kiln_Bin` (`abi/kiln_abi.h`), which is
/// `{ int32 dims; int32 len; }` with the bytes immediately after it.
const BIN_LEN_OFFSET: u64 = 4;

/// The size of that same header, and where the bytes start.
const BIN_HEADER_BYTES: u64 = 4 + 4;

/// The most elements a collection may claim before its header is read as
/// garbage rather than as a length.
///
/// Nothing in the runtime enforces this. It is here because a stale pointer
/// reads as a plausible header with an implausible length, and walking a
/// billion elements would hang the debugger instead of reporting anything —
/// and a debugger that hangs on a wild pointer is worse than one that says so.
const MAX_ELEMENTS: i64 = 1 << 20;

/// The furthest a text walk will go looking for the NUL.
///
/// The same bound and the same reason. It is not a length the runtime knows:
/// a heap text carries an eight-byte header, but that header is a `next` link
/// in the runtime's allocation list (`runtime/kn_mem.c:11`), not a size, so
/// walking to the terminator is the only way to measure one.
const MAX_TEXT_BYTES: u64 = 1 << 20;

/// The `KN_SDT_*` tags an element or a dictionary value can carry, mirrored
/// from `abi/kiln_abi.h`. Their numeric values are frozen there.
mod tag {
    pub const INT: i32 = 3;
    pub const INT64: i32 = 4;
    pub const DOUBLE: i32 = 6;
    pub const BOOL: i32 = 8;
    pub const TEXT: i32 = 9;
    pub const BIN: i32 = 10;
    pub const RECORD: i32 = 13;
    pub const PTR: i32 = 14;
}

/// A value as the user should see it.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i32),
    Int64(i64),
    Double(f64),
    Bool(bool),
    Text(String),
    /// An optional that is not there.
    Nothing,
    /// An array, already 1-based for display.
    Array(Vec<Value>),
    Record {
        name: String,
        fields: Vec<(String, Value)>,
    },
    Dict(Vec<(String, Value)>),
    /// A value that could not be read, with why. Shown rather than hidden: a
    /// blank row is indistinguishable from a bug.
    Unreadable(String),
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Int(v) => write!(f, "{v}"),
            Value::Int64(v) => write!(f, "{v}"),
            // Rust's own formatting rather than the `%g` the runtime prints
            // with: `%g` stops at six significant digits, and a debugger that
            // rounds is a debugger that hides the bug being looked for.
            Value::Double(v) => write!(f, "{v}"),
            Value::Bool(v) => write!(f, "{v}"),
            // Quoted and escaped, so trailing spaces and a newline in the
            // middle of a string are visible rather than merely wrong-looking.
            Value::Text(v) => write!(f, "{v:?}"),
            Value::Nothing => write!(f, "nothing"),
            Value::Array(elements) => {
                if elements.is_empty() {
                    return write!(f, "[]");
                }
                for (i, element) in elements.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    // The index the user would write, which is the position
                    // counting from one.
                    write!(f, "[{}] {element}", i + 1)?;
                }
                Ok(())
            }
            Value::Record { name, fields } => {
                if fields.is_empty() {
                    return write!(f, "{name} {{}}");
                }
                write!(f, "{name} {{ ")?;
                for (i, (field, value)) in fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{field}: {value}")?;
                }
                write!(f, " }}")
            }
            Value::Dict(entries) => {
                if entries.is_empty() {
                    return write!(f, "{{}}");
                }
                write!(f, "{{ ")?;
                for (i, (key, value)) in entries.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{key:?}: {value}")?;
                }
                write!(f, " }}")
            }
            Value::Unreadable(why) => write!(f, "<unreadable: {why}>"),
        }
    }
}

/// A local, as the debug information describes it.
#[derive(Debug, Clone)]
pub struct Local {
    pub name: String,
    /// Where it lives, relative to the frame.
    pub frame_offset: i64,
    pub type_name: String,
}

impl Local {
    /// The address of the local's own storage.
    ///
    /// A frame's locals sit below its canonical frame address, so the offset
    /// is negative and this wraps deliberately rather than saturating: an
    /// address is a bit pattern, and a stack near the top of the address space
    /// is not an error.
    fn address(&self, frame_base: u64) -> u64 {
        frame_base.wrapping_add(self.frame_offset as u64)
    }
}

/// Which of a record's two layouts a local holds.
///
/// The distinction is not cosmetic: the two are read from different places.
/// A heap record's slot holds a pointer to the runtime's allocation, so the
/// fields are one dereference away; a c-record's slot *is* the struct — an
/// `alloca [N x i8]` — so the fields are in the frame itself. One sentence
/// covering both would render garbage for one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordShape {
    /// A `record`: `kn_rec_new`'s allocation, held by pointer.
    Heap,
    /// A `c record`: flat bytes laid out where the local lives.
    Flat,
}

/// One field of a record, as the debug information describes it.
#[derive(Debug, Clone)]
pub struct Field {
    /// The name the user gave it, which reaches here only because the compiler
    /// wrote it into the debug information.
    pub name: String,
    /// Its offset from the start of the record's storage, in bytes — DWARF's
    /// `DW_AT_data_member_location`. For a heap record that already counts the
    /// eight-byte header, because the backend writes the offsets that way.
    pub byte_offset: u64,
    /// The field's type, spelled as the debug information spells it.
    pub type_name: String,
}

/// Read a local out of a stopped frame.
///
/// Every failure is a [`Value::Unreadable`] carrying its reason rather than an
/// `Err`: a variable that cannot be read is one row of an answer, not the
/// failure of the whole request, and the `Result` is here for the day some
/// other kind of failure needs it.
///
/// A record needs its field names and their offsets, which a `Local` does not
/// carry and no shipped binary holds; [`read_record`] takes them and this
/// reports what is missing instead of inventing positional names.
pub fn read(local: &Local, frame_base: u64, memory: &dyn Memory) -> Result<Value, Error> {
    let address = local.address(frame_base);
    Ok(read_typed(&local.type_name, address, memory))
}

/// Read a record, given the fields the debug information describes.
///
/// `shape` says where the fields are; see [`RecordShape`]. The record is named
/// by the local's own type name, which is the record's name — the compiler
/// writes it into the debug information precisely so this can be shown.
pub fn read_record(
    local: &Local,
    shape: RecordShape,
    fields: &[Field],
    frame_base: u64,
    memory: &dyn Memory,
) -> Value {
    let name = local.type_name.clone();
    let base = match shape {
        RecordShape::Flat => local.address(frame_base),
        RecordShape::Heap => {
            let slot = local.address(frame_base);
            let Some(pointer) = read_u64_at(memory, slot) else {
                return unreadable_at("this record", slot);
            };
            if pointer == 0 {
                // `var p: Point` is null until its initializer runs, and the
                // runtime answers a read through one with an error rather than
                // a field — so there is genuinely nothing to show.
                return Value::Unreadable(format!("{name} has not been created yet"));
            }
            let Some(count) = read_i32_at(memory, pointer) else {
                return unreadable_at(&format!("the header of {name}"), pointer);
            };
            if count < 0 || count as i64 > MAX_ELEMENTS {
                return Value::Unreadable(format!(
                    "{name} claims {count} field(s), so 0x{pointer:x} is not a record"
                ));
            }
            // The debug information and the program disagreeing about a record
            // is a compiler bug, and reading fields on that basis would show
            // one field's name over another field's bytes.
            if count as usize != fields.len() {
                return Value::Unreadable(format!(
                    "{name} holds {count} field(s) but the debug information \
                     describes {}",
                    fields.len()
                ));
            }
            // A field before the header would read the count as a value, and
            // that can only mean the emitter and this file disagree about
            // where a record's fields begin.
            if let Some(field) = fields.iter().find(|f| f.byte_offset < RECORD_HEADER_BYTES) {
                return Value::Unreadable(format!(
                    "field {} of {name} is described as sitting inside the record's header",
                    field.name
                ));
            }
            pointer
        }
    };

    let mut read_fields = Vec::with_capacity(fields.len());
    for field in fields {
        let value = match base.checked_add(field.byte_offset) {
            Some(address) => read_typed(&field.type_name, address, memory),
            None => Value::Unreadable(format!(
                "field {} sits past the end of the address space",
                field.name
            )),
        };
        read_fields.push((field.name.clone(), value));
    }
    Value::Record {
        name,
        fields: read_fields,
    }
}

/// Render a frame's locals, pairing each optional with its hidden companion.
///
/// This is where the two halves of an optional become one row. The value half
/// carries the plain type — the compiler emits `int` for an `int?`, because
/// there is no optional to describe — so the only thing that says a value is
/// absent is the `name$has` local beside it. They are joined by name here, and
/// every `$` name is then dropped: those are the compiler's own, `$` is not a
/// character an identifier may contain, so the filter is exact.
///
/// A local that cannot be read keeps its row and carries its reason. Dropping
/// it would leave the user looking for a variable that is right there.
pub fn locals(locals: &[Local], frame_base: u64, memory: &dyn Memory) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    for local in locals {
        if local.name.contains('$') {
            continue;
        }
        let companion = format!("{}$has", local.name);
        let present = locals
            .iter()
            .find(|l| l.name == companion)
            .map(|l| read_bool_at(memory, l.address(frame_base)));
        let value = match present {
            // The companion exists and says the value is not there. Its own
            // bytes are untouched in that case, so reading them would show a
            // zero that means nothing at all.
            Some(Some(false)) => Value::Nothing,
            Some(None) => {
                Value::Unreadable(format!("whether {} is there could not be read", local.name))
            }
            _ => {
                read(local, frame_base, memory).unwrap_or_else(|e| Value::Unreadable(e.to_string()))
            }
        };
        out.push((local.name.clone(), value));
    }
    out
}

/// Read a value of the named type from an address.
///
/// The names are the ones the backend writes into the debug information
/// (`backend/src/debug.rs`), which are the language's own spellings.
fn read_typed(type_name: &str, address: u64, memory: &dyn Memory) -> Value {
    match type_name {
        "int" => match read_i32_at(memory, address) {
            Some(v) => Value::Int(v),
            None => unreadable_at("this value", address),
        },
        // A byte and an int16 are c-record field widths only, and the language
        // reads both as `int` — so they are widened here exactly as `Ty::surface`
        // widens them, unsigned.
        "byte" => match read_bytes(memory, address, 1) {
            Some(b) => Value::Int(b[0] as i32),
            None => unreadable_at("this value", address),
        },
        "int16" => match read_bytes(memory, address, 2) {
            Some(b) => Value::Int(u16::from_le_bytes([b[0], b[1]]) as i32),
            None => unreadable_at("this value", address),
        },
        "int64" => match read_u64_at(memory, address) {
            Some(v) => Value::Int64(v as i64),
            None => unreadable_at("this value", address),
        },
        "double" => match read_u64_at(memory, address) {
            Some(v) => Value::Double(f64::from_bits(v)),
            None => unreadable_at("this value", address),
        },
        // A C `float` is a c-record field width; the language calls it a
        // double everywhere else, and the widening is what a field read does.
        "float" => match read_bytes(memory, address, 4) {
            Some(b) => Value::Double(f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64),
            None => unreadable_at("this value", address),
        },
        // Bool is int-sized, matching the ABI's BOOL (`backend/src/lib.rs`
        // lowers it to `i32`), so four bytes is the whole of it.
        "bool" => match read_bool_at(memory, address) {
            Some(v) => Value::Bool(v),
            None => unreadable_at("this value", address),
        },
        "text" => match read_u64_at(memory, address) {
            Some(pointer) => read_text(memory, pointer),
            None => unreadable_at("this text", address),
        },
        "bytes" => match read_u64_at(memory, address) {
            Some(pointer) => read_bin(memory, pointer),
            None => unreadable_at("this byte-set", address),
        },
        // A raw machine pointer is an address and nothing else. `Value` has no
        // variant for one, so it is shown as the 64-bit number it is rather
        // than dressed up as something with meaning.
        "ptr" => match read_u64_at(memory, address) {
            Some(v) => Value::Int64(v as i64),
            None => unreadable_at("this pointer", address),
        },
        _ if type_name.ends_with("[]") => match read_u64_at(memory, address) {
            Some(pointer) => read_array(memory, pointer),
            None => unreadable_at("this list", address),
        },
        _ if type_name.ends_with("{}") => match read_u64_at(memory, address) {
            Some(pointer) => read_dict(memory, pointer),
            None => unreadable_at("this dictionary", address),
        },
        // Everything left is a record, and a record needs the field names the
        // debug information holds. Positional names would be worse than this
        // message: they look like an answer.
        _ => Value::Unreadable(format!(
            "{type_name} is a record, and its fields are not described here"
        )),
    }
}

/// Read one array, 1-based and already rendered element by element.
///
/// The element type comes from the array's own header rather than from the
/// local's type name, which is what `elem_text` does in `runtime/kn_array.c`:
/// the header is the runtime's own answer, and a disagreement between the two
/// is a bug worth seeing rather than papering over.
fn read_array(memory: &dyn Memory, pointer: u64) -> Value {
    // `ary_len(NULL)` is 0 (`runtime/kn_array.c:35`) — an array that was never
    // created reads as empty, which is the language's own answer and not a
    // failure to report.
    if pointer == 0 {
        return Value::Array(Vec::new());
    }
    let (Some(elem_tag), Some(len)) = (
        read_i32_at(memory, pointer),
        read_i32_offset(memory, pointer, ARRAY_LEN_OFFSET),
    ) else {
        return unreadable_at("this list's header", pointer);
    };
    if len < 0 || len as i64 > MAX_ELEMENTS {
        return Value::Unreadable(format!(
            "a list of {len} element(s) at 0x{pointer:x} is not a list"
        ));
    }
    let mut elements = Vec::with_capacity(len as usize);
    for i in 0..len as u64 {
        let Some(cell) = pointer
            .checked_add(ARRAY_HEADER_BYTES)
            .and_then(|start| start.checked_add(i * CELL_BYTES))
        else {
            elements.push(Value::Unreadable(format!(
                "element {} sits past the end of the address space",
                i + 1
            )));
            continue;
        };
        elements.push(match read_u64_at(memory, cell) {
            Some(bits) => read_cell(memory, elem_tag, bits),
            None => unreadable_at(&format!("element {}", i + 1), cell),
        });
    }
    Value::Array(elements)
}

/// Read one dictionary, in the insertion order its entries are kept in.
fn read_dict(memory: &dyn Memory, pointer: u64) -> Value {
    // A dictionary that was never created reads as empty for the reason an
    // array does (`runtime/kn_dict.c:41`).
    if pointer == 0 {
        return Value::Dict(Vec::new());
    }
    let (Some(val_tag), Some(len), Some(entries)) = (
        read_i32_at(memory, pointer),
        read_i32_offset(memory, pointer, DICT_LEN_OFFSET),
        read_u64_at(memory, pointer.wrapping_add(DICT_ENTRIES_OFFSET)),
    ) else {
        return unreadable_at("this dictionary's header", pointer);
    };
    if len < 0 || len as i64 > MAX_ELEMENTS {
        return Value::Unreadable(format!(
            "a dictionary of {len} entries at 0x{pointer:x} is not a dictionary"
        ));
    }
    if len > 0 && entries == 0 {
        return Value::Unreadable(format!(
            "this dictionary claims {len} entries but holds none"
        ));
    }
    let mut out = Vec::with_capacity(len as usize);
    for i in 0..len as u64 {
        let Some(entry) = entries.checked_add(i * DICT_ENTRY_BYTES) else {
            out.push((
                format!("entry {}", i + 1),
                Value::Unreadable("this entry sits past the end of the address space".into()),
            ));
            continue;
        };
        let key = match read_u64_at(memory, entry) {
            Some(pointer) => match read_text(memory, pointer) {
                Value::Text(k) => k,
                other => other.to_string(),
            },
            None => format!("entry {}", i + 1),
        };
        let value = match read_u64_at(memory, entry.wrapping_add(CELL_BYTES)) {
            Some(bits) => read_cell(memory, val_tag, bits),
            None => unreadable_at("this entry's value", entry),
        };
        out.push((key, value));
    }
    Value::Dict(out)
}

/// Interpret one 64-bit cell — an array element or a dictionary value — under
/// the tag its container carries.
///
/// Every cell is 64 raw bits and only the tag says what they mean, which is
/// the same sentence `elem_cmp` and `elem_text` are written under in
/// `runtime/kn_array.c:117`.
fn read_cell(memory: &dyn Memory, tag: i32, bits: u64) -> Value {
    match tag {
        tag::INT => Value::Int(bits as i32),
        tag::INT64 => Value::Int64(bits as i64),
        tag::DOUBLE => Value::Double(f64::from_bits(bits)),
        tag::BOOL => Value::Bool(bits != 0),
        tag::TEXT => read_text(memory, bits),
        tag::BIN => read_bin(memory, bits),
        tag::PTR => Value::Int64(bits as i64),
        tag::RECORD => {
            Value::Unreadable("a record inside a collection has no field names here".to_string())
        }
        other => Value::Unreadable(format!("type {other} is not one this can read")),
    }
}

/// Read a `text` pointer as the characters the user wrote.
///
/// `NULL` is the empty text and not a failure: that is what the ABI says
/// (`abi/kiln_abi.h`, `KN_SDT_TEXT`) and what every command that takes text
/// already does with one.
///
/// The eight bytes *before* the payload are never touched. A heap text has a
/// header there and a literal has whatever the linker put there, and even for
/// the heap text the header is a `next` link in the allocation list
/// (`runtime/kn_mem.c:11`) rather than a length — so reading it would be
/// meaningless for one provenance and wrong for the other. Walking to the NUL
/// is correct for both, which is why there is one path here and not two.
fn read_text(memory: &dyn Memory, pointer: u64) -> Value {
    if pointer == 0 {
        return Value::Text(String::new());
    }
    // Word by aligned word, and never the word after the terminator. Asking
    // for eight bytes from wherever the walk has reached would straddle into
    // the next one, and a string whose NUL is the last byte of its mapping —
    // a literal at the end of `.rodata` — would read as unreadable though
    // every byte of it was there.
    let mut bytes: Vec<u8> = Vec::new();
    let mut word = pointer & !(CELL_BYTES - 1);
    let mut skip = (pointer & (CELL_BYTES - 1)) as usize;
    while (bytes.len() as u64) < MAX_TEXT_BYTES {
        let Some(raw) = memory.read_u64(word) else {
            return unreadable_at("this text", pointer.wrapping_add(bytes.len() as u64));
        };
        let chunk = raw.to_le_bytes();
        if let Some(end) = chunk[skip..].iter().position(|b| *b == 0) {
            bytes.extend_from_slice(&chunk[skip..skip + end]);
            // Lossily, because a torn read of a program mid-write is a real
            // possibility and a replacement character is a better answer than
            // refusing to show the rest of the string.
            return Value::Text(String::from_utf8_lossy(&bytes).into_owned());
        }
        bytes.extend_from_slice(&chunk[skip..]);
        skip = 0;
        let Some(next) = word.checked_add(CELL_BYTES) else {
            return Value::Unreadable(format!(
                "the text at 0x{pointer:x} runs past the end of the address space"
            ));
        };
        word = next;
    }
    Value::Unreadable(format!(
        "the text at 0x{pointer:x} has no terminator within {MAX_TEXT_BYTES} bytes"
    ))
}

/// Read a byte-set as the 1-based list of numbers `byte_at` would answer.
///
/// `Value` has no variant of its own for one, and a list of its bytes is both
/// truthful and indexed the way the language indexes it
/// (`runtime/kn_array.c:90`).
fn read_bin(memory: &dyn Memory, pointer: u64) -> Value {
    if pointer == 0 {
        return Value::Array(Vec::new());
    }
    let Some(len) = read_i32_offset(memory, pointer, BIN_LEN_OFFSET) else {
        return unreadable_at("this byte-set's header", pointer);
    };
    if len < 0 || len as i64 > MAX_ELEMENTS {
        return Value::Unreadable(format!(
            "a byte-set of {len} byte(s) at 0x{pointer:x} is not a byte-set"
        ));
    }
    let Some(start) = pointer.checked_add(BIN_HEADER_BYTES) else {
        return unreadable_at("this byte-set", pointer);
    };
    match read_bytes(memory, start, len as usize) {
        Some(bytes) => Value::Array(bytes.into_iter().map(|b| Value::Int(b as i32)).collect()),
        None => unreadable_at("this byte-set", start),
    }
}

/// The reason a read of `what` at `address` failed, phrased for the pane it
/// will be shown in.
fn unreadable_at(what: &str, address: u64) -> Value {
    Value::Unreadable(format!("{what} is at 0x{address:x}, which is not mapped"))
}

/// Bytes from the debuggee, whatever their alignment.
///
/// [`Memory`] reads a word at a time because that is what a traced process
/// offers, so an `int` in a frame — four bytes, and half the time at an odd
/// multiple of four — has to be sliced out of the word around it. Every read
/// this makes is eight-byte aligned, which is the granularity the trap door
/// below actually supports.
///
/// Bytes come back in the order they sit in memory, which is what
/// `to_le_bytes` gives on the little-endian targets Kiln builds for.
fn read_bytes(memory: &dyn Memory, address: u64, len: usize) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(len);
    let mut word = address & !7;
    let mut skip = (address & 7) as usize;
    while out.len() < len {
        let bytes = memory.read_u64(word)?.to_le_bytes();
        for byte in &bytes[skip..] {
            if out.len() == len {
                break;
            }
            out.push(*byte);
        }
        skip = 0;
        if out.len() < len {
            word = word.checked_add(CELL_BYTES)?;
        }
    }
    Some(out)
}

fn read_u64_at(memory: &dyn Memory, address: u64) -> Option<u64> {
    let bytes = read_bytes(memory, address, 8)?;
    Some(u64::from_le_bytes(bytes.try_into().ok()?))
}

fn read_i32_at(memory: &dyn Memory, address: u64) -> Option<i32> {
    let bytes = read_bytes(memory, address, 4)?;
    Some(i32::from_le_bytes(bytes.try_into().ok()?))
}

fn read_i32_offset(memory: &dyn Memory, base: u64, offset: u64) -> Option<i32> {
    read_i32_at(memory, base.checked_add(offset)?)
}

fn read_bool_at(memory: &dyn Memory, address: u64) -> Option<bool> {
    read_i32_at(memory, address).map(|v| v != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The base of the fake stack every fixture is built at. Eight-byte
    /// aligned, because [`Ram`] refuses anything else.
    const BASE: u64 = 0x1000;

    /// The debuggee address of a fixture offset.
    fn at(offset: u64) -> u64 {
        BASE + offset
    }

    /// A block of the debuggee's memory, with nothing else mapped.
    ///
    /// It refuses an unaligned read on purpose: a traced process reads a word
    /// at a time, so a reader that quietly depended on byte addressing would
    /// pass here and fail against a real program.
    struct Ram {
        bytes: Vec<u8>,
    }

    impl Ram {
        fn new() -> Ram {
            Ram { bytes: Vec::new() }
        }

        /// Write raw bytes at an offset from the base, growing to fit.
        ///
        /// Growth is rounded up to a whole word, because a real mapping is
        /// pages rather than bytes and a fixture that ended mid-word would
        /// make the last value in it unreadable for a reason no program has.
        fn put(&mut self, offset: u64, bytes: &[u8]) -> &mut Ram {
            let at = offset as usize;
            let end = at + bytes.len();
            if self.bytes.len() < end {
                self.bytes.resize(end.next_multiple_of(8), 0);
            }
            self.bytes[at..end].copy_from_slice(bytes);
            self
        }

        fn put_i32(&mut self, offset: u64, v: i32) -> &mut Ram {
            self.put(offset, &v.to_le_bytes())
        }

        fn put_u64(&mut self, offset: u64, v: u64) -> &mut Ram {
            self.put(offset, &v.to_le_bytes())
        }

        /// A NUL-terminated string, as a program's memory holds one.
        fn put_text(&mut self, offset: u64, s: &str) -> &mut Ram {
            self.put(offset, s.as_bytes());
            self.put(offset + s.len() as u64, &[0])
        }
    }

    impl Memory for Ram {
        fn read_u64(&self, address: u64) -> Option<u64> {
            assert_eq!(address % 8, 0, "a traced process only reads whole words");
            let offset = address.checked_sub(BASE)? as usize;
            let end = offset.checked_add(8)?;
            if end > self.bytes.len() {
                return None;
            }
            Some(u64::from_le_bytes(
                self.bytes[offset..end].try_into().unwrap(),
            ))
        }
    }

    /// A local at `offset` bytes into the fixture, described the way the debug
    /// information describes one: relative to the frame.
    fn local(name: &str, type_name: &str, offset: u64) -> Local {
        Local {
            name: name.to_string(),
            frame_offset: offset as i64,
            type_name: type_name.to_string(),
        }
    }

    fn rendered(name: &str, type_name: &str, offset: u64, ram: &Ram) -> String {
        read(&local(name, type_name, offset), BASE, ram)
            .unwrap()
            .to_string()
    }

    #[test]
    fn each_scalar_reads_as_itself() {
        let mut ram = Ram::new();
        ram.put_i32(0, -7)
            .put_u64(8, 9_000_000_000)
            .put_u64(16, 2.5f64.to_bits())
            .put_i32(24, 1)
            .put_i32(28, 0)
            .put(32, &[200])
            .put(40, &1234u16.to_le_bytes())
            .put(48, &0.5f32.to_le_bytes())
            .put_u64(56, 0x7fff_1234_5678);

        assert_eq!(rendered("n", "int", 0, &ram), "-7");
        assert_eq!(rendered("big", "int64", 8, &ram), "9000000000");
        assert_eq!(rendered("d", "double", 16, &ram), "2.5");
        assert_eq!(rendered("yes", "bool", 24, &ram), "true");
        assert_eq!(rendered("no", "bool", 28, &ram), "false");
        assert_eq!(rendered("b", "byte", 32, &ram), "200");
        assert_eq!(rendered("w", "int16", 40, &ram), "1234");
        assert_eq!(rendered("f", "float", 48, &ram), "0.5");
        assert_eq!(rendered("p", "ptr", 56, &ram), "140733498807928");
    }

    #[test]
    fn an_int_that_does_not_start_a_word_reads_correctly() {
        let mut ram = Ram::new();
        ram.put_i32(0, 111).put_i32(4, 222);
        assert_eq!(rendered("low", "int", 0, &ram), "111");
        assert_eq!(rendered("high", "int", 4, &ram), "222");
    }

    #[test]
    fn text_reads_the_same_from_the_heap_and_from_a_literal() {
        // A heap text has the allocator's `next` link in the eight bytes
        // before it; a literal has whatever the linker left there. Neither is
        // a length, and the reader must reach the same answer through both.
        let mut ram = Ram::new();
        ram.put_u64(0, at(32))
            .put_u64(8, at(48))
            .put_u64(24, 0xDEAD_BEEF_DEAD_BEEF)
            .put_text(32, "ada")
            .put_text(48, "grace");

        assert_eq!(rendered("heap", "text", 0, &ram), "\"ada\"");
        assert_eq!(rendered("literal", "text", 8, &ram), "\"grace\"");
    }

    #[test]
    fn text_longer_than_a_word_is_walked_to_its_terminator() {
        let mut ram = Ram::new();
        ram.put_u64(0, at(16))
            .put_text(16, "a name that does not fit in one word");
        assert_eq!(
            rendered("t", "text", 0, &ram),
            "\"a name that does not fit in one word\""
        );
    }

    #[test]
    fn text_whose_terminator_ends_the_mapping_is_still_read() {
        // The NUL is the last byte of the last mapped word. A reader that asks
        // for eight bytes from wherever it has got to asks for the word after
        // it, faults, and calls a perfectly readable string unreadable — which
        // is what a literal at the end of `.rodata` looks like.
        let mut ram = Ram::new();
        ram.put_u64(0, at(61)).put_text(61, "ab");
        assert_eq!(rendered("t", "text", 0, &ram), "\"ab\"");
    }

    #[test]
    fn a_null_text_is_empty_rather_than_absent() {
        let mut ram = Ram::new();
        ram.put_u64(0, 0);
        assert_eq!(rendered("t", "text", 0, &ram), "\"\"");
    }

    #[test]
    fn an_array_is_shown_one_based() {
        let mut ram = Ram::new();
        ram.put_u64(0, at(16))
            // The header: elem_tag, len, cap, pad — then the elements.
            .put_i32(16, tag::TEXT)
            .put_i32(20, 2)
            .put_i32(24, 2)
            .put_u64(32, at(48))
            .put_u64(40, at(56))
            .put_text(48, "ada")
            .put_text(56, "grace");

        assert_eq!(
            rendered("names", "text[]", 0, &ram),
            "[1] \"ada\", [2] \"grace\""
        );
    }

    #[test]
    fn an_array_reads_its_elements_through_its_own_tag() {
        let mut ram = Ram::new();
        ram.put_u64(0, at(16))
            .put_i32(16, tag::DOUBLE)
            .put_i32(20, 3)
            .put_i32(24, 3)
            .put_u64(32, 1.5f64.to_bits())
            .put_u64(40, 2.0f64.to_bits())
            .put_u64(48, (-0.25f64).to_bits());
        assert_eq!(
            rendered("xs", "double[]", 0, &ram),
            "[1] 1.5, [2] 2, [3] -0.25"
        );
    }

    #[test]
    fn an_array_that_was_never_created_is_empty() {
        let mut ram = Ram::new();
        ram.put_u64(0, 0);
        assert_eq!(rendered("nums", "int[]", 0, &ram), "[]");
    }

    #[test]
    fn a_heap_record_shows_its_field_names() {
        let mut ram = Ram::new();
        // `{ int32 count; int32 _pad; }` and then one cell per field.
        ram.put_u64(0, at(16))
            .put_i32(16, 2)
            .put_u64(24, 3)
            .put_u64(32, 4);

        let fields = vec![
            Field {
                name: "x".into(),
                byte_offset: RECORD_HEADER_BYTES,
                type_name: "int".into(),
            },
            Field {
                name: "y".into(),
                byte_offset: RECORD_HEADER_BYTES + CELL_BYTES,
                type_name: "int".into(),
            },
        ];
        let value = read_record(
            &local("p", "Point", 0),
            RecordShape::Heap,
            &fields,
            BASE,
            &ram,
        );
        assert_eq!(value.to_string(), "Point { x: 3, y: 4 }");
    }

    #[test]
    fn a_c_record_is_read_where_it_lies() {
        // A c-record's alloca is the struct itself, so there is no pointer to
        // follow and the field offsets are the C ones.
        let mut ram = Ram::new();
        ram.put_i32(0, 3).put(4, &[7]).put(6, &640u16.to_le_bytes());

        let fields = vec![
            Field {
                name: "count".into(),
                byte_offset: 0,
                type_name: "int".into(),
            },
            Field {
                name: "flags".into(),
                byte_offset: 4,
                type_name: "byte".into(),
            },
            Field {
                name: "width".into(),
                byte_offset: 6,
                type_name: "int16".into(),
            },
        ];
        let value = read_record(
            &local("r", "Rect", 0),
            RecordShape::Flat,
            &fields,
            BASE,
            &ram,
        );
        assert_eq!(value.to_string(), "Rect { count: 3, flags: 7, width: 640 }");
    }

    #[test]
    fn a_record_read_without_its_fields_says_so() {
        let mut ram = Ram::new();
        ram.put_u64(0, 0);
        assert_eq!(
            rendered("p", "Point", 0, &ram),
            "<unreadable: Point is a record, and its fields are not described here>"
        );
    }

    #[test]
    fn a_record_the_debug_information_disagrees_with_is_not_guessed_at() {
        let mut ram = Ram::new();
        ram.put_u64(0, at(16)).put_i32(16, 3).put_u64(24, 1);
        let fields = vec![Field {
            name: "x".into(),
            byte_offset: RECORD_HEADER_BYTES,
            type_name: "int".into(),
        }];
        let value = read_record(
            &local("p", "Point", 0),
            RecordShape::Heap,
            &fields,
            BASE,
            &ram,
        );
        assert_eq!(
            value.to_string(),
            "<unreadable: Point holds 3 field(s) but the debug information describes 1>"
        );
    }

    #[test]
    fn a_record_that_has_not_been_created_says_so_rather_than_reading_zero() {
        let mut ram = Ram::new();
        ram.put_u64(0, 0);
        let fields = vec![Field {
            name: "x".into(),
            byte_offset: RECORD_HEADER_BYTES,
            type_name: "int".into(),
        }];
        let value = read_record(
            &local("p", "Point", 0),
            RecordShape::Heap,
            &fields,
            BASE,
            &ram,
        );
        assert_eq!(
            value.to_string(),
            "<unreadable: Point has not been created yet>"
        );
    }

    #[test]
    fn a_dictionary_keeps_its_keys_and_its_order() {
        let mut ram = Ram::new();
        ram.put_u64(0, at(16))
            // The header: val_tag, len, cap, pad, entries.
            .put_i32(16, tag::INT)
            .put_i32(20, 2)
            .put_i32(24, 2)
            .put_u64(32, at(40))
            // Two `{ char *key; int64 val; }` entries, in insertion order.
            .put_u64(40, at(72))
            .put_u64(48, 1)
            .put_u64(56, at(80))
            .put_u64(64, 2)
            .put_text(72, "b")
            .put_text(80, "a");

        assert_eq!(
            rendered("counts", "int{}", 0, &ram),
            "{ \"b\": 1, \"a\": 2 }"
        );
    }

    #[test]
    fn a_byte_set_reads_as_its_bytes() {
        let mut ram = Ram::new();
        // `{ int32 dims; int32 len; }` and then the bytes themselves.
        ram.put_u64(0, at(16))
            .put_i32(16, 1)
            .put_i32(20, 3)
            .put(24, &[137, 80, 78]);
        assert_eq!(rendered("png", "bytes", 0, &ram), "[1] 137, [2] 80, [3] 78");
    }

    #[test]
    fn an_optional_prints_nothing_when_its_companion_says_so() {
        let mut ram = Ram::new();
        // The value half of the absent one is left at zero, which is exactly
        // the point: nothing in memory tells an absent int from a zero one.
        ram.put_i32(0, 0)
            .put_i32(4, 0)
            .put_i32(8, 42)
            .put_i32(12, 1);

        let described = vec![
            local("missing", "int", 0),
            local("missing$has", "bool", 4),
            local("found", "int", 8),
            local("found$has", "bool", 12),
        ];
        let shown: Vec<String> = locals(&described, BASE, &ram)
            .into_iter()
            .map(|(name, value)| format!("{name} = {value}"))
            .collect();
        assert_eq!(shown, vec!["missing = nothing", "found = 42"]);
    }

    #[test]
    fn hidden_locals_are_never_shown() {
        let mut ram = Ram::new();
        ram.put_i32(0, 5).put_i32(8, 1);
        let described = vec![
            local("total", "int", 0),
            local("$each$i$3", "int", 8),
            local("$defer$1", "int", 8),
        ];
        let rows = locals(&described, BASE, &ram);
        let names: Vec<&str> = rows.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, vec!["total"]);
    }

    #[test]
    fn a_local_that_cannot_be_read_keeps_its_row() {
        let mut ram = Ram::new();
        ram.put_i32(0, 5);
        let described = vec![local("total", "int", 0), local("gone", "int", 4096)];
        let shown: Vec<String> = locals(&described, BASE, &ram)
            .into_iter()
            .map(|(name, value)| format!("{name} = {value}"))
            .collect();
        assert_eq!(
            shown,
            vec![
                "total = 5",
                "gone = <unreadable: this value is at 0x2000, which is not mapped>"
            ]
        );
    }

    #[test]
    fn an_unmapped_address_renders_its_reason_rather_than_a_blank() {
        let ram = Ram::new();
        let shown = rendered("n", "int", 4096, &ram);
        assert_eq!(
            shown,
            "<unreadable: this value is at 0x2000, which is not mapped>"
        );
        assert!(!shown.is_empty());
    }

    #[test]
    fn text_through_an_unmapped_pointer_says_where() {
        let mut ram = Ram::new();
        ram.put_u64(0, 0xF000);
        assert_eq!(
            rendered("t", "text", 0, &ram),
            "<unreadable: this text is at 0xf000, which is not mapped>"
        );
    }

    #[test]
    fn an_implausible_length_is_reported_rather_than_walked() {
        let mut ram = Ram::new();
        ram.put_u64(0, at(16))
            .put_i32(16, tag::INT)
            .put_i32(20, i32::MAX);
        assert_eq!(
            rendered("nums", "int[]", 0, &ram),
            "<unreadable: a list of 2147483647 element(s) at 0x1010 is not a list>"
        );
    }

    #[test]
    fn every_variant_renders_something_a_reader_can_use() {
        assert_eq!(Value::Int(-1).to_string(), "-1");
        assert_eq!(Value::Int64(1 << 40).to_string(), "1099511627776");
        assert_eq!(Value::Double(0.125).to_string(), "0.125");
        assert_eq!(Value::Bool(false).to_string(), "false");
        assert_eq!(Value::Text("a\nb".into()).to_string(), "\"a\\nb\"");
        assert_eq!(Value::Text(String::new()).to_string(), "\"\"");
        assert_eq!(Value::Nothing.to_string(), "nothing");
        assert_eq!(Value::Array(Vec::new()).to_string(), "[]");
        assert_eq!(Value::Dict(Vec::new()).to_string(), "{}");
        assert_eq!(
            Value::Record {
                name: "Empty".into(),
                fields: Vec::new()
            }
            .to_string(),
            "Empty {}"
        );
        assert_eq!(
            Value::Unreadable("the stack moved".into()).to_string(),
            "<unreadable: the stack moved>"
        );
    }

    #[test]
    fn a_nested_value_renders_inside_its_container() {
        let value = Value::Array(vec![
            Value::Record {
                name: "Point".into(),
                fields: vec![("x".into(), Value::Int(1))],
            },
            Value::Nothing,
        ]);
        assert_eq!(value.to_string(), "[1] Point { x: 1 }, [2] nothing");
    }
}
