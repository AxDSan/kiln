//! Debug information: the LLVM metadata that makes a built program one a
//! debugger can step through.
//!
//! The backend writes LLVM IR as text rather than through LLVM's own API, so
//! `-g` on the clang line buys nothing — there is no front end for it to ask.
//! The metadata has to be written into the IR by hand, and this is where it is
//! written.
//!
//! Two things are described. A *line table* — which machine address
//! corresponds to which line of which `.kiln` file — is what stepping and
//! breakpoints need. A *type graph* — a node per type, a `!DILocalVariable`
//! per named local and per module variable, and a `#dbg_declare` record
//! binding each to the slot holding it — is what reading a value needs, and it
//! is why the compile unit says `FullDebug`.
//!
//! Only user subroutines carry debug information. The functions the compiler
//! synthesises — the entry point, a library's initialiser, the export
//! wrappers, the event thunks — deliberately carry none, because a function
//! that *has* debug information must give every call inside it a location, and
//! there is no line in anyone's source to give.

use kiln_ir::{c_field_size_align, Registry, Ty};
use std::collections::HashMap;
use std::fmt::Write as _;

/// The four nodes every module needs, before anything specific to it.
const CU: usize = 0;
const FILE: usize = 1;
const DWARF_VERSION: usize = 2;
const DEBUG_INFO_VERSION: usize = 3;
/// The shared `!DISubroutineType`, which a line table does not describe.
const SUB_TYPE: usize = 4;
const SUB_TYPE_LIST: usize = 5;
/// The first node number free for subprograms and locations.
const FIRST_FREE: usize = 6;

/// A heap record is one runtime allocation: an 8-byte header (`Kiln_Record`
/// in `runtime/kiln_core.h`) and then one 8-byte cell per field, which is
/// what `fields(r)` — `(int64_t *)(r + 1)` — walks. Every offset a debugger is
/// given has to start past that header, in bits.
const RECORD_HEADER_BITS: u64 = 64;
/// The width of one heap record field. Every value the language has fits in
/// eight bytes, so a record is a flat grid of them whatever it holds.
const RECORD_CELL_BITS: u64 = 64;

/// One entry of the metadata block.
enum Node {
    /// Text that was complete when it was written.
    Text(String),
    /// A tuple gathered as the module is lowered — a subprogram's
    /// `retainedNodes`, or the compile unit's `globals`. It has to be a node
    /// before its members exist, because the node naming it is written first.
    Tuple(Vec<usize>),
}

/// Builds the metadata block a module ends with.
pub(crate) struct DebugInfo {
    filename: String,
    directory: String,
    producer: String,
    /// Node text by number, from `FIRST_FREE` upwards.
    nodes: Vec<Node>,
    /// `(scope, line, column)` -> node, so a statement lowered into several
    /// instructions names one location rather than one per instruction.
    locations: HashMap<(usize, usize, usize), usize>,
    /// The type of a value as a slot holds it, by the type it is.
    types: HashMap<Ty, usize>,
    /// The structure node for a record, by name. Kept apart from `types`
    /// because a record's *value* is a pointer to this and both are wanted.
    composites: HashMap<String, usize>,
    /// A subprogram's `retainedNodes` tuple, by the subprogram's node.
    retained: HashMap<usize, usize>,
    /// The compile unit's `globals` tuple, made when the first module variable
    /// is declared. A module with none says nothing about globals at all.
    globals: Option<usize>,
    /// The `!DIGlobalVariableExpression` of each module variable, by name — the
    /// node its `@` definition is annotated with.
    ///
    /// Built and unit-tested, but nothing in the lowering annotates a `@`
    /// definition yet, so a module variable is not inspectable in a debugger.
    /// Kept rather than deleted because the half that is hard — the two-node
    /// shape and the compile unit's `globals` list — is the half that is done.
    #[allow(dead_code)]
    global_exprs: HashMap<String, usize>,
    /// Emit variable declarations as debug *records* (`#dbg_declare`) rather
    /// than as calls to `@llvm.dbg.declare`. The two spellings are not
    /// interchangeable and neither is universally accepted: records were
    /// introduced in LLVM 19 and the intrinsics were removed in LLVM 21, so
    /// the toolchain that will assemble this module decides which one it gets.
    records: bool,
}

impl DebugInfo {
    /// `path` is the source as the user named it. It is split into a file and
    /// a directory because DWARF stores them separately, and a debugger that
    /// is handed a bare name cannot find the file to show.
    pub(crate) fn new(path: &str, producer: &str, records: bool) -> Self {
        let (directory, filename) = match path.rfind('/') {
            Some(i) => (path[..i].to_string(), path[i + 1..].to_string()),
            None => (".".to_string(), path.to_string()),
        };
        DebugInfo {
            filename,
            directory,
            producer: producer.to_string(),
            nodes: Vec::new(),
            locations: HashMap::new(),
            types: HashMap::new(),
            composites: HashMap::new(),
            retained: HashMap::new(),
            globals: None,
            global_exprs: HashMap::new(),
            records,
        }
    }

    fn push(&mut self, text: String) -> usize {
        self.nodes.push(Node::Text(text));
        FIRST_FREE + self.nodes.len() - 1
    }

    /// Reserve a node whose text is only known later — a structure, which its
    /// own members name as their scope.
    fn reserve(&mut self) -> usize {
        self.push(String::new())
    }

    fn set(&mut self, node: usize, text: String) {
        self.nodes[node - FIRST_FREE] = Node::Text(text);
    }

    fn push_tuple(&mut self) -> usize {
        self.nodes.push(Node::Tuple(Vec::new()));
        FIRST_FREE + self.nodes.len() - 1
    }

    fn extend_tuple(&mut self, tuple: usize, member: usize) {
        if let Node::Tuple(members) = &mut self.nodes[tuple - FIRST_FREE] {
            members.push(member);
        }
    }

    /// Declare a subroutine and return the node to name on its `define` line.
    ///
    /// `line` is where the `sub` keyword is. `scopeLine` is the same: Kiln
    /// has no separate opening brace for a debugger to step to.
    pub(crate) fn subprogram(&mut self, name: &str, symbol: &str, line: usize) -> usize {
        let line = line.max(1);
        let sp = self.reserve();
        let retained = self.push_tuple();
        self.set(
            sp,
            format!(
                "distinct !DISubprogram(name: \"{name}\", linkageName: \"{symbol}\", \
                 scope: !{FILE}, file: !{FILE}, line: {line}, type: !{SUB_TYPE}, \
                 scopeLine: {line}, spFlags: DISPFlagDefinition, unit: !{CU}, \
                 retainedNodes: !{retained})"
            ),
        );
        self.retained.insert(sp, retained);
        sp
    }

    /// The node for a source position inside `scope`.
    ///
    /// A line of 0 means the position was lost somewhere between the parser
    /// and here. Rather than emit `line: 0` — which a debugger reads as "no
    /// line", stepping straight past it — the caller is expected to pass a
    /// line it does have; this only guards against a zero slipping through.
    pub(crate) fn location(&mut self, scope: usize, line: usize, column: usize) -> usize {
        let line = line.max(1);
        let key = (scope, line, column);
        if let Some(n) = self.locations.get(&key) {
            return *n;
        }
        let n = self.push(format!(
            "!DILocation(line: {line}, column: {column}, scope: !{scope})"
        ));
        self.locations.insert(key, n);
        n
    }

    /// The type of a value as a slot holds it — what an `alloca llvm_ty(ty)`
    /// contains.
    ///
    /// Every aggregate is a pointer here, records included: a heap record's
    /// slot holds the address of the runtime object, and a c-record *parameter*
    /// is passed by address too. The one place that is not true is a c-record
    /// local, whose slot is the flat storage itself — that shape is
    /// [`Self::c_storage_type`], and handing a debugger this one instead
    /// renders the struct's first eight bytes as an address.
    pub(crate) fn value_type(&mut self, ty: Ty, reg: &Registry) -> usize {
        if let Some(n) = self.types.get(&ty) {
            return *n;
        }
        let n = match ty {
            Ty::Int => self.basic("int", 32, "DW_ATE_signed"),
            Ty::Int64 => self.basic("int64", 64, "DW_ATE_signed"),
            Ty::Double => self.basic("double", 64, "DW_ATE_float"),
            // Bool is int-sized, matching the ABI's BOOL, so a debugger reading
            // four bytes reads the whole of it.
            Ty::Bool => self.basic("bool", 32, "DW_ATE_boolean"),
            // The three c-record field widths. A field of one is read as `int`
            // or `double` by the language, but the bytes in the struct are
            // these, and it is the bytes a debugger walks.
            Ty::Byte => self.basic("byte", 8, "DW_ATE_unsigned"),
            Ty::Int16 => self.basic("int16", 16, "DW_ATE_unsigned"),
            Ty::Float => self.basic("float", 32, "DW_ATE_float"),
            // Text is a pointer to characters and is written as one, rather
            // than as an opaque address, so a reader can walk to the NUL and
            // show the string instead of the number.
            Ty::Text => {
                let ch = self.basic("char", 8, "DW_ATE_signed_char");
                self.pointer(Some("text"), Some(ch))
            }
            // A byte-set is the same shape with no promise about what is in it.
            Ty::Bytes => {
                let byte = self.basic("byte", 8, "DW_ATE_unsigned");
                self.pointer(Some("bytes"), Some(byte))
            }
            // A raw machine pointer is an address and nothing else — C's
            // `void *`, which DWARF spells as a pointer with no base type.
            Ty::Ptr => self.pointer(Some("ptr"), None),
            // An array and a dictionary are runtime-owned objects whose layout
            // is the runtime's business, not DWARF's. The pointer carries the
            // Kiln spelling as its name, which is what tells a reader that
            // `xs` holds text rather than ints.
            Ty::Array(e) => {
                let name = format!("{}[]", e.as_str());
                self.pointer(Some(&name), None)
            }
            Ty::Dict(e) => {
                let name = format!("{}{{}}", e.as_str());
                self.pointer(Some(&name), None)
            }
            Ty::Record(name) => {
                let structure = self.composite(name, reg);
                self.pointer(None, Some(structure))
            }
            // An optional is two slots, and this is the one holding the value;
            // the truth beside it is a separate `bool` local under a name no
            // program can write, which never reaches a debugger at all.
            Ty::Optional(e) => self.value_type(e.ty(), reg),
            // An inline array only ever appears as a c-record field, where it
            // is laid out in the struct rather than pointed at.
            Ty::CArray(_) => self.field_type(ty, reg),
            // Signature-only types. `resolve_ret` replaces them with what the
            // call actually produced long before a value carries one, so this
            // is unreachable rather than approximate — but an address is the
            // honest answer if one ever arrives.
            Ty::AnyArray | Ty::AnyElem | Ty::AnyDict => self.pointer(Some("ptr"), None),
        };
        self.types.insert(ty, n);
        n
    }

    /// The type of a c-record local's slot: the flat storage itself, because
    /// `alloca [N x i8]` *is* the struct rather than a pointer to one.
    pub(crate) fn c_storage_type(&mut self, record: &str, reg: &Registry) -> usize {
        self.composite(record, reg)
    }

    /// Declare a named local, scoped to the subprogram `scope`, and retain it
    /// there. `arg` is the parameter's 1-based position, or `None` for an
    /// ordinary local.
    ///
    /// A name the compiler invented for itself gets no entry: those all carry
    /// a `$`, which no program can write, and a Variables pane listing
    /// `$each$i$3` beside the user's own names is worse than one that does not.
    pub(crate) fn local(
        &mut self,
        scope: usize,
        name: &str,
        ty: usize,
        line: usize,
        arg: Option<usize>,
    ) -> Option<usize> {
        // Names the compiler invented are not the user's and are not shown.
        // The one exception is an optional's companion: an optional has no
        // runtime representation beyond a value and a separate truth beside
        // it, so nothing in memory distinguishes an absent `int` from zero.
        // Describing the companion is the only way a debugger can print
        // `nothing` rather than a plausible wrong answer, and the reader
        // filters it back out of what it shows.
        if name.contains('$') && !name.ends_with("$has") {
            return None;
        }
        let line = line.max(1);
        let arg = match arg {
            Some(i) => format!("arg: {i}, "),
            None => String::new(),
        };
        let n = self.push(format!(
            "!DILocalVariable(name: \"{}\", {arg}scope: !{scope}, file: !{FILE}, \
             line: {line}, type: !{ty})",
            escape(name)
        ));
        if let Some(tuple) = self.retained.get(&scope).copied() {
            self.extend_tuple(tuple, n);
        }
        Some(n)
    }

    /// The line binding a variable to the stack slot holding it.
    ///
    /// This is a whole line, indentation and newline included, and it must
    /// reach the function body *verbatim*.
    ///
    /// In the record form `#dbg_declare` is not an instruction, so a trailing
    /// `, !dbg !N` — which the instruction stream appends to every indented
    /// line it is given — makes it unparseable; the location is the fourth
    /// argument instead. In the intrinsic form it is an ordinary call and the
    /// location *is* the trailing `!dbg`, written here rather than left to the
    /// stream because the stream attaches the location of the enclosing
    /// statement, which is not necessarily the one this variable was declared
    /// at. Either way the location must be scoped to the same subprogram as
    /// `var`.
    pub(crate) fn declare(&self, slot: &str, var: usize, loc: usize) -> String {
        if self.records {
            format!("  #dbg_declare(ptr {slot}, !{var}, !DIExpression(), !{loc})\n")
        } else {
            format!(
                "  call void @llvm.dbg.declare(metadata ptr {slot}, metadata !{var}, \
                 metadata !DIExpression()), !dbg !{loc}\n"
            )
        }
    }

    /// Declare a module variable and return the `!DIGlobalVariableExpression`
    /// its `@` definition must be annotated with.
    ///
    /// Both halves are needed. The expression is what the definition points at;
    /// the compile unit's `globals` list is what makes a debugger look for it.
    /// Without them a module variable is unreadable however good the engine
    /// reading it.
    #[allow(dead_code)]
    pub(crate) fn global(&mut self, name: &str, ty: usize, line: usize) -> usize {
        let line = line.max(1);
        // Module variables are `internal`, which is what `isLocal` says.
        let var = self.push(format!(
            "distinct !DIGlobalVariable(name: \"{}\", scope: !{CU}, file: !{FILE}, \
             line: {line}, type: !{ty}, isLocal: true, isDefinition: true)",
            escape(name)
        ));
        let expr = self.push(format!(
            "!DIGlobalVariableExpression(var: !{var}, expr: !DIExpression())"
        ));
        let list = match self.globals {
            Some(t) => t,
            None => {
                let t = self.push_tuple();
                self.globals = Some(t);
                t
            }
        };
        self.extend_tuple(list, expr);
        self.global_exprs.insert(name.to_string(), expr);
        expr
    }

    /// The node a module variable's definition is annotated with, once
    /// [`Self::global`] has declared it.
    #[allow(dead_code)]
    pub(crate) fn global_expr(&self, name: &str) -> Option<usize> {
        self.global_exprs.get(name).copied()
    }

    fn basic(&mut self, name: &str, bits: u64, encoding: &str) -> usize {
        self.interned(format!(
            "!DIBasicType(name: \"{name}\", size: {bits}, encoding: {encoding})"
        ))
    }

    /// A pointer, named or not, to `base` — or to nothing, which is how DWARF
    /// spells an address with no promise about what is at it.
    fn pointer(&mut self, name: Option<&str>, base: Option<usize>) -> usize {
        let name = match name {
            Some(n) => format!("name: \"{}\", ", escape(n)),
            None => String::new(),
        };
        let base = match base {
            Some(b) => format!("!{b}"),
            None => "null".to_string(),
        };
        self.interned(format!(
            "!DIDerivedType(tag: DW_TAG_pointer_type, {name}baseType: {base}, size: 64)"
        ))
    }

    /// A node written once however often it is asked for. LLVM uniques
    /// identical non-`distinct` metadata anyway; doing it here keeps the
    /// emitted text short enough to read.
    fn interned(&mut self, text: String) -> usize {
        for (i, node) in self.nodes.iter().enumerate() {
            if let Node::Text(t) = node {
                if *t == text {
                    return FIRST_FREE + i;
                }
            }
        }
        self.push(text)
    }

    /// The structure describing a record, in whichever of its two layouts it
    /// has. Cached before its members are built, so a record holding itself
    /// describes itself once rather than for ever.
    fn composite(&mut self, record: &str, reg: &Registry) -> usize {
        if let Some(n) = self.composites.get(record) {
            return *n;
        }
        let n = self.reserve();
        self.composites.insert(record.to_string(), n);
        let Some(def) = reg.record(record) else {
            // The lowerer refuses an unknown record with a real message before
            // reaching here; saying "declared elsewhere" is what DWARF has for
            // a shape it cannot see, and it keeps a bug from becoming a lie
            // about the layout.
            self.set(
                n,
                format!(
                    "!DICompositeType(tag: DW_TAG_structure_type, name: \"{}\", \
                     file: !{FILE}, flags: DIFlagFwdDecl)",
                    escape(record)
                ),
            );
            return n;
        };
        let line = def.line.max(1);
        let (offsets, size) = if def.is_c {
            match def.c_layout(reg) {
                Some((offsets, size, _)) => (
                    offsets.iter().map(|o| *o as u64 * 8).collect::<Vec<_>>(),
                    size as u64 * 8,
                ),
                // A c-record whose layout cannot be computed is a build error
                // the lowerer raises; leaving the members out beats guessing an
                // offset a debugger would then read from.
                None => (Vec::new(), 0),
            }
        } else {
            // A heap record is the header and then one eight-byte cell per
            // field, whatever the field's own width: `kn_rec_set` widens every
            // value to 64 bits, and a little-endian read of the low half is the
            // narrower value back.
            let offsets = (0..def.fields.len())
                .map(|i| RECORD_HEADER_BITS + i as u64 * RECORD_CELL_BITS)
                .collect::<Vec<_>>();
            let size = RECORD_HEADER_BITS + def.fields.len() as u64 * RECORD_CELL_BITS;
            (offsets, size)
        };
        let mut members = Vec::new();
        for (i, (fname, fty)) in def.fields.iter().enumerate() {
            let Some(offset) = offsets.get(i).copied() else {
                continue;
            };
            let (ty, bits) = if def.is_c {
                let bits = c_field_size_align(*fty, reg).map(|(s, _)| s as u64 * 8);
                (self.field_type(*fty, reg), bits)
            } else {
                (self.value_type(*fty, reg), None)
            };
            let bits = bits.unwrap_or_else(|| value_bits(*fty));
            members.push(self.push(format!(
                "!DIDerivedType(tag: DW_TAG_member, name: \"{}\", scope: !{n}, \
                 file: !{FILE}, line: {line}, baseType: !{ty}, size: {bits}, \
                 offset: {offset})",
                escape(fname)
            )));
        }
        let elements = self.tuple_of(&members);
        self.set(
            n,
            format!(
                "distinct !DICompositeType(tag: DW_TAG_structure_type, name: \"{}\", \
                 file: !{FILE}, line: {line}, size: {size}, elements: !{elements})",
                escape(record)
            ),
        );
        n
    }

    /// The type of a c-record *field* as it sits in the struct. A nested
    /// c-record and an inline array are laid out in place rather than pointed
    /// at, which is the whole difference between this and a value's type.
    fn field_type(&mut self, ty: Ty, reg: &Registry) -> usize {
        match ty {
            Ty::Record(name) => self.composite(name, reg),
            Ty::CArray(a) => {
                let elem = self.field_type(a.elem, reg);
                let bits = c_field_size_align(a.elem, reg)
                    .map(|(s, _)| s as u64 * 8)
                    .unwrap_or(8)
                    * a.count as u64;
                let range = self.push(format!("!DISubrange(count: {})", a.count));
                let elements = self.tuple_of(&[range]);
                self.push(format!(
                    "!DICompositeType(tag: DW_TAG_array_type, baseType: !{elem}, \
                     size: {bits}, elements: !{elements})"
                ))
            }
            other => self.value_type(other, reg),
        }
    }

    /// A fixed tuple: a member list that was complete before it was written.
    fn tuple_of(&mut self, members: &[usize]) -> usize {
        let body = members
            .iter()
            .map(|m| format!("!{m}"))
            .collect::<Vec<_>>()
            .join(", ");
        self.push(format!("!{{{body}}}"))
    }

    /// Whether anything was declared. A module with no user subroutines gets
    /// no metadata block at all rather than an empty compile unit.
    /// Whether anything beyond a line table was described.
    fn describes_types(&self) -> bool {
        !self.types.is_empty() || !self.composites.is_empty() || self.globals.is_some()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// The block appended to the module, after every function.
    pub(crate) fn render(&self) -> String {
        let mut out = String::new();
        out.push('\n');
        // The intrinsic is an ordinary function and must be declared before it
        // is called. The record form is syntax and needs nothing.
        if !self.records {
            out.push_str("declare void @llvm.dbg.declare(metadata, metadata, metadata)\n\n");
        }
        writeln!(out, "!llvm.dbg.cu = !{{!{CU}}}").unwrap();
        writeln!(
            out,
            "!llvm.module.flags = !{{!{DWARF_VERSION}, !{DEBUG_INFO_VERSION}}}"
        )
        .unwrap();
        // DW_LANG_C99 is a deliberate lie of convenience: DWARF has no
        // language code for Kiln, and every debugger knows what to do with
        // C99's basic types and scoping. Nothing here depends on it.
        let globals = match self.globals {
            Some(n) => format!(", globals: !{n}"),
            None => String::new(),
        };
        // The compile unit says what was actually emitted, not what this
        // module is capable of emitting. `FullDebug` on a unit carrying no
        // types tells a debugger to expect a type graph and then hands it
        // none, and it makes the linked binary carry duplicate function
        // extents for nothing. It becomes true the moment the lowering starts
        // describing variables, and not before.
        let kind = if self.describes_types() {
            "FullDebug"
        } else {
            "LineTablesOnly"
        };
        writeln!(
            out,
            "!{CU} = distinct !DICompileUnit(language: DW_LANG_C99, file: !{FILE}, \
             producer: \"{}\", isOptimized: false, runtimeVersion: 0, \
             emissionKind: {kind}{globals})",
            self.producer
        )
        .unwrap();
        writeln!(
            out,
            "!{FILE} = !DIFile(filename: \"{}\", directory: \"{}\")",
            escape(&self.filename),
            escape(&self.directory)
        )
        .unwrap();
        writeln!(out, "!{DWARF_VERSION} = !{{i32 7, !\"Dwarf Version\", i32 5}}").unwrap();
        writeln!(
            out,
            "!{DEBUG_INFO_VERSION} = !{{i32 2, !\"Debug Info Version\", i32 3}}"
        )
        .unwrap();
        writeln!(out, "!{SUB_TYPE} = !DISubroutineType(types: !{SUB_TYPE_LIST})").unwrap();
        writeln!(out, "!{SUB_TYPE_LIST} = !{{null}}").unwrap();
        for (i, node) in self.nodes.iter().enumerate() {
            let n = FIRST_FREE + i;
            match node {
                Node::Text(t) => writeln!(out, "!{n} = {t}").unwrap(),
                Node::Tuple(members) => {
                    let body = members
                        .iter()
                        .map(|m| format!("!{m}"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    writeln!(out, "!{n} = !{{{body}}}").unwrap();
                }
            }
        }
        out
    }
}

/// How wide a value of `ty` is, in bits, for a heap record member that has no
/// C layout to ask. It is the width the value is *stored* in, not the cell it
/// sits in — a debugger reading four bytes of an int reads the whole int.
fn value_bits(ty: Ty) -> u64 {
    match ty {
        Ty::Byte => 8,
        Ty::Int16 => 16,
        Ty::Int | Ty::Bool | Ty::Float => 32,
        _ => 64,
    }
}

/// A path as an LLVM metadata string. Backslashes and quotes are the two
/// characters that would end the string early — on Windows the first is what
/// every path is made of.
fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\22")
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiln_ir::parse;

    /// A registry holding the records `src` declares, which is what the
    /// lowerer hands the type builder.
    fn registry(src: &str) -> Registry {
        let m = parse(src).unwrap();
        let mut reg = Registry::core();
        reg.register_records(&m);
        reg
    }

    fn info() -> DebugInfo {
        DebugInfo::new("examples/demo.kiln", "Kiln test", true)
    }

    /// A unit that describes nothing but lines must say so. Claiming
    /// `FullDebug` and emitting no types costs space in the binary and tells
    /// a debugger to expect something it will not find.
    #[test]
    fn a_compile_unit_with_only_lines_says_only_lines() {
        let d = info();
        assert!(
            d.render().contains("emissionKind: LineTablesOnly"),
            "{}",
            d.render()
        );
    }

    #[test]
    fn a_compile_unit_that_describes_a_type_says_so() {
        let mut d = info();
        d.value_type(Ty::Int, &Registry::core());
        assert!(d.render().contains("emissionKind: FullDebug"), "{}", d.render());
    }

    #[test]
    fn the_basic_types_carry_their_widths_and_encodings() {
        let reg = Registry::core();
        let mut d = info();
        for ty in [Ty::Int, Ty::Int64, Ty::Double, Ty::Bool] {
            d.value_type(ty, &reg);
        }
        let out = d.render();
        for want in [
            r#"!DIBasicType(name: "int", size: 32, encoding: DW_ATE_signed)"#,
            r#"!DIBasicType(name: "int64", size: 64, encoding: DW_ATE_signed)"#,
            r#"!DIBasicType(name: "double", size: 64, encoding: DW_ATE_float)"#,
            r#"!DIBasicType(name: "bool", size: 32, encoding: DW_ATE_boolean)"#,
        ] {
            assert!(out.contains(want), "{want} missing from\n{out}");
        }
    }

    /// A reader that is handed an address cannot know a string is at the end of
    /// it. Text is a pointer to characters so that it can walk to the NUL.
    #[test]
    fn text_is_a_pointer_to_characters() {
        let reg = Registry::core();
        let mut d = info();
        let t = d.value_type(Ty::Text, &reg);
        let out = d.render();
        assert!(
            out.contains(r#"!DIBasicType(name: "char", size: 8, encoding: DW_ATE_signed_char)"#),
            "{out}"
        );
        let line = out
            .lines()
            .find(|l| l.starts_with(&format!("!{t} = ")))
            .unwrap();
        assert!(line.contains("DW_TAG_pointer_type"), "{line}");
        assert!(line.contains(r#"name: "text""#), "{line}");
    }

    #[test]
    fn a_type_is_written_once_however_often_it_is_asked_for() {
        let reg = Registry::core();
        let mut d = info();
        assert_eq!(d.value_type(Ty::Int, &reg), d.value_type(Ty::Int, &reg));
        assert_eq!(d.render().matches(r#"name: "int""#).count(), 1);
    }

    /// The trap the design names: one rule for both record shapes renders
    /// garbage for one of them.
    #[test]
    fn a_heap_record_local_is_a_pointer_and_a_c_record_local_is_not() {
        let reg = registry(
            "module m\nrecord point\n  x: int\n  y: int\nend\n\
             record rect is c\n  left: int\n  top: int\nend\n",
        );
        let mut d = info();
        let heap = d.value_type(Ty::Record(kiln_ir::intern("point")), &reg);
        let flat = d.c_storage_type("rect", &reg);
        let out = d.render();
        let heap_line = out
            .lines()
            .find(|l| l.starts_with(&format!("!{heap} = ")))
            .unwrap();
        assert!(heap_line.contains("DW_TAG_pointer_type"), "{heap_line}");
        let flat_line = out
            .lines()
            .find(|l| l.starts_with(&format!("!{flat} = ")))
            .unwrap();
        assert!(flat_line.contains("DW_TAG_structure_type"), "{flat_line}");
    }

    /// The header `kn_rec_new` allocates is eight bytes, so field one begins at
    /// bit 64. Starting at zero shows the field count where `x` should be.
    #[test]
    fn heap_record_fields_sit_past_the_runtime_header() {
        let reg = registry("module m\nrecord point\n  x: int\n  y: text\nend\n");
        let mut d = info();
        d.value_type(Ty::Record(kiln_ir::intern("point")), &reg);
        let out = d.render();
        assert!(
            out.contains(r#"tag: DW_TAG_member, name: "x""#) && out.contains("offset: 64"),
            "{out}"
        );
        assert!(
            out.contains(r#"tag: DW_TAG_member, name: "y""#) && out.contains("offset: 128"),
            "{out}"
        );
        assert!(out.contains("size: 192"), "{out}");
    }

    /// A c-record's members sit where a C compiler would put them, padding
    /// included — that is the whole promise of `is c`.
    #[test]
    fn c_record_members_follow_the_c_layout() {
        let reg = registry("module m\nrecord r is c\n  a: byte\n  b: int\n  c: double\nend\n");
        let mut d = info();
        d.c_storage_type("r", &reg);
        let out = d.render();
        assert!(out.contains(r#"name: "a", scope"#) && out.contains("size: 8, offset: 0"), "{out}");
        assert!(out.contains("size: 32, offset: 32"), "{out}");
        assert!(out.contains("size: 64, offset: 64"), "{out}");
        assert!(out.contains("size: 128, elements:"), "{out}");
    }

    /// A record holding itself must describe itself once. The composite is
    /// cached before its members are built, which is what stops the recursion.
    #[test]
    fn a_self_referential_record_terminates() {
        let reg = registry("module m\nrecord node\n  next: node\n  value: int\nend\n");
        let mut d = info();
        d.value_type(Ty::Record(kiln_ir::intern("node")), &reg);
        let out = d.render();
        assert_eq!(out.matches(r#"DW_TAG_structure_type, name: "node""#).count(), 1);
    }

    #[test]
    fn a_parameter_is_marked_as_one_and_a_local_is_not() {
        let reg = Registry::core();
        let mut d = info();
        let sp = d.subprogram("greet", "kn_user_greet", 2);
        let int = d.value_type(Ty::Int, &reg);
        let arg = d.local(sp, "n", int, 2, Some(1)).unwrap();
        let var = d.local(sp, "total", int, 3, None).unwrap();
        let out = d.render();
        assert!(out.contains(&format!(
            "!{arg} = !DILocalVariable(name: \"n\", arg: 1, scope: !{sp}"
        )), "{out}");
        assert!(out.contains(&format!(
            "!{var} = !DILocalVariable(name: \"total\", scope: !{sp}"
        )), "{out}");
    }

    /// A variable that is not retained by its subprogram is one a debugger can
    /// only find by walking the instruction stream.
    #[test]
    fn a_subprogram_retains_its_variables() {
        let reg = Registry::core();
        let mut d = info();
        let sp = d.subprogram("main", "kn_user_main", 2);
        let int = d.value_type(Ty::Int, &reg);
        let a = d.local(sp, "a", int, 3, None).unwrap();
        let b = d.local(sp, "b", int, 4, None).unwrap();
        let out = d.render();
        let retained = out
            .lines()
            .find(|l| l.starts_with(&format!("!{} = ", sp + 1)))
            .unwrap();
        assert_eq!(retained, format!("!{} = !{{!{a}, !{b}}}", sp + 1));
    }

    /// The names the compiler makes up for itself all carry a `$`, and a
    /// Variables pane listing them is worse than one that does not.
    #[test]
    fn a_compiler_invented_name_is_not_a_variable() {
        let reg = Registry::core();
        let mut d = info();
        let sp = d.subprogram("main", "kn_user_main", 2);
        let int = d.value_type(Ty::Int, &reg);
        assert!(d.local(sp, "$each$i$0", int, 3, None).is_none());
        assert!(d.local(sp, "$t7", int, 3, None).is_none());
        assert!(!d.render().contains("DILocalVariable"));
    }

    /// An optional's companion is the one invented name that is described.
    ///
    /// An optional has no runtime representation beyond a value and a truth
    /// beside it, so nothing in memory tells an absent `int` from a zero. The
    /// companion is what lets a debugger print `nothing`, and the reader
    /// filters it back out of what it shows.
    #[test]
    fn an_optionals_companion_is_described_so_it_can_be_read() {
        let reg = Registry::core();
        let mut d = info();
        let sp = d.subprogram("main", "kn_user_main", 2);
        let truth = d.value_type(Ty::Bool, &reg);
        assert!(d.local(sp, "v$has", truth, 3, None).is_some());
        assert!(d.render().contains(r#"name: "v$has""#), "{}", d.render());
    }

    /// The record is a standalone line: a `, !dbg` on the end of it, which is
    /// what the instruction stream would add, makes it unparseable.
    #[test]
    fn a_declare_record_is_a_whole_line_with_the_location_inside_it() {
        assert_eq!(
            info().declare("%v0", 12, 9),
            "  #dbg_declare(ptr %v0, !12, !DIExpression(), !9)\n"
        );
    }

    /// The intrinsic is the opposite in both respects: an ordinary call, whose
    /// location is the trailing `!dbg` the record could not have.
    #[test]
    fn a_declare_intrinsic_is_a_call_carrying_a_trailing_location() {
        let d = DebugInfo::new("examples/demo.kiln", "Kiln test", false);
        assert_eq!(
            d.declare("%v0", 12, 9),
            "  call void @llvm.dbg.declare(metadata ptr %v0, metadata !12, \
             metadata !DIExpression()), !dbg !9\n"
        );
    }

    /// And it is a function, so it must be declared. Nothing declares it for
    /// the record form, which is syntax.
    #[test]
    fn the_intrinsic_form_declares_the_intrinsic_and_the_record_form_does_not() {
        let reg = Registry::core();
        let mut records = DebugInfo::new("a.kiln", "Kiln test", true);
        let mut calls = DebugInfo::new("a.kiln", "Kiln test", false);
        for d in [&mut records, &mut calls] {
            let sp = d.subprogram("main", "ECodeStart", 1);
            let ty = d.value_type(Ty::Int, &reg);
            d.local(sp, "v", ty, 1, None);
        }
        let want = "declare void @llvm.dbg.declare(metadata, metadata, metadata)";
        assert!(calls.render().contains(want), "{}", calls.render());
        assert!(!records.render().contains(want), "{}", records.render());
    }

    #[test]
    fn a_module_variable_is_declared_and_listed_on_the_compile_unit() {
        let reg = Registry::core();
        let mut d = info();
        let int = d.value_type(Ty::Int, &reg);
        let expr = d.global("total", int, 1);
        assert_eq!(d.global_expr("total"), Some(expr));
        assert_eq!(d.global_expr("missing"), None);
        let out = d.render();
        assert!(out.contains(&format!(
            "!{expr} = !DIGlobalVariableExpression(var: !{}, expr: !DIExpression())",
            expr - 1
        )), "{out}");
        assert!(out.contains(&format!(
            "!{} = distinct !DIGlobalVariable(name: \"total\"",
            expr - 1
        )), "{out}");
        assert!(out.contains(&format!("globals: !{}", expr + 1)), "{out}");
        assert!(out.contains(&format!("!{} = !{{!{expr}}}", expr + 1)), "{out}");
    }

    /// A module with no module variables says nothing about globals, rather
    /// than naming an empty tuple.
    #[test]
    fn a_module_with_no_variables_has_no_globals_field() {
        assert!(!info().render().contains("globals:"));
    }

    /// The first subprogram has to stay `!6`: it is what a `define` line names,
    /// and moving it would mean the type graph had been built before any
    /// function was.
    #[test]
    fn declaring_types_does_not_move_the_first_subprogram() {
        let mut d = info();
        assert_eq!(d.subprogram("main", "kn_user_main", 2), FIRST_FREE);
    }
}

