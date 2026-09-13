//! Debug information for a KIR module: enough DWARF for a debugger to step
//! through the source and name the function it is stopped in.
//!
//! A line table is the whole of it. Each `Stmt::Line` the front end emits
//! becomes a `DILocation`, and the instructions after it carry that location;
//! each function becomes a `DISubprogram`. Variables and types come later —
//! the compile unit says `LineTablesOnly` so a debugger is told exactly what it
//! is being given rather than promising a type graph that is not there.

use std::collections::HashMap;
use std::fmt::Write as _;

/// Metadata node numbers are assigned as they are needed; these are fixed.
const CU: usize = 0;
const FILE: usize = 1;
const DWARF_VERSION: usize = 2;
const DEBUG_INFO_VERSION: usize = 3;
const SUBPROGRAM_TYPE: usize = 4;
const FIRST_FREE: usize = 5;

pub struct Debug {
    file: String,
    dir: String,
    next: usize,
    /// Function symbol → its subprogram node.
    subprograms: Vec<(String, usize, usize)>,
    /// `(line, scope)` → its location node.
    locations: HashMap<(usize, usize), usize>,
    producer: String,
    /// Type descriptors, by their rendered text.
    types: Vec<(String, usize)>,
    /// Local variables: their rendered text, in order.
    variables: Vec<String>,
    /// Whether any variable was described, which is what turns the compile unit
    /// from a line table into full debug information.
    described: bool,
}

impl Debug {
    pub fn new(source: &str, producer: &str) -> Debug {
        let p = std::path::Path::new(source);
        let file = p
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_else(|| source.to_string());
        let dir = p
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
            .map(|d| d.to_string_lossy().to_string())
            .unwrap_or_else(|| ".".to_string());
        Debug {
            file,
            dir,
            next: FIRST_FREE,
            subprograms: Vec::new(),
            locations: HashMap::new(),
            producer: producer.to_string(),
            types: Vec::new(),
            variables: Vec::new(),
            described: false,
        }
    }

    fn fresh(&mut self) -> usize {
        let n = self.next;
        self.next += 1;
        n
    }

    /// The subprogram for a function, as a metadata node number. A synthetic
    /// function — a thunk, a generic instance's body — has no source position
    /// worth stopping in, and gets none.
    pub fn subprogram(&mut self, symbol: &str, line: usize) -> usize {
        if let Some((_, _, n)) = self.subprograms.iter().find(|(s, _, _)| s == symbol) {
            return *n;
        }
        let n = self.fresh();
        self.subprograms.push((symbol.to_string(), line, n));
        n
    }

    /// The location for a line inside a subprogram.
    pub fn location(&mut self, line: usize, scope: usize) -> usize {
        if let Some(n) = self.locations.get(&(line, scope)) {
            return *n;
        }
        let n = self.fresh();
        self.locations.insert((line, scope), n);
        n
    }

    /// A type descriptor for a value of the given shape.
    ///
    /// Aggregates are described as an opaque pointer: a debugger can show the
    /// address and the program is steppable, which is the point. Describing a
    /// record's fields comes with the type graph.
    pub fn basic_type(&mut self, name: &str, bits: u32, encoding: &str) -> usize {
        let text = if encoding == "pointer" {
            format!("!DIDerivedType(tag: DW_TAG_pointer_type, baseType: null, size: 64)")
        } else {
            format!("!DIBasicType(name: \"{name}\", size: {bits}, encoding: {encoding})")
        };
        if let Some((_, n)) = self.types.iter().find(|(t, _)| *t == text) {
            return *n;
        }
        let n = self.fresh();
        self.types.push((text, n));
        n
    }

    /// Describe a local variable, or a parameter when `arg` is its 1-based
    /// position. Returns the node to name in a `#dbg_declare`.
    pub fn local(
        &mut self,
        name: &str,
        scope: usize,
        line: usize,
        ty: usize,
        arg: Option<usize>,
    ) -> usize {
        let n = self.fresh();
        let argpart = match arg {
            Some(i) => format!("arg: {i}, "),
            None => String::new(),
        };
        self.variables.push(format!(
            "!{n} = !DILocalVariable(name: \"{name}\", {argpart}scope: !{scope}, \
             file: !{FILE}, line: {line}, type: !{ty})"
        ));
        self.described = true;
        n
    }

    pub fn is_empty(&self) -> bool {
        self.subprograms.is_empty()
    }

    /// The metadata block, appended after the functions.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push('\n');
        writeln!(out, "!llvm.dbg.cu = !{{!{CU}}}").unwrap();
        writeln!(
            out,
            "!llvm.module.flags = !{{!{DWARF_VERSION}, !{DEBUG_INFO_VERSION}}}"
        )
        .unwrap();
        // DW_LANG_C99 is a convenience: DWARF has no code for Kiln, and every
        // debugger knows what to do with C99's scoping. Nothing depends on it.
        writeln!(
            out,
            "!{CU} = distinct !DICompileUnit(language: DW_LANG_C99, file: !{FILE}, \
             producer: \"{}\", isOptimized: false, runtimeVersion: 0, \
             emissionKind: {})",
            self.producer,
            if self.described {
                "FullDebug"
            } else {
                "LineTablesOnly"
            }
        )
        .unwrap();
        writeln!(
            out,
            "!{FILE} = !DIFile(filename: \"{}\", directory: \"{}\")",
            self.file, self.dir
        )
        .unwrap();
        writeln!(
            out,
            "!{DWARF_VERSION} = !{{i32 7, !\"Dwarf Version\", i32 5}}"
        )
        .unwrap();
        writeln!(
            out,
            "!{DEBUG_INFO_VERSION} = !{{i32 2, !\"Debug Info Version\", i32 3}}"
        )
        .unwrap();
        writeln!(out, "!{SUBPROGRAM_TYPE} = !DISubroutineType(types: !{{}})").unwrap();
        for (symbol, line, n) in &self.subprograms {
            writeln!(
                out,
                "!{n} = distinct !DISubprogram(name: \"{symbol}\", scope: !{FILE}, file: !{FILE}, \
                 line: {line}, type: !{SUBPROGRAM_TYPE}, scopeLine: {line}, \
                 spFlags: DISPFlagDefinition, unit: !{CU})"
            )
            .unwrap();
        }
        for (text, n) in &self.types {
            writeln!(out, "!{n} = {text}").unwrap();
        }
        for v in &self.variables {
            writeln!(out, "{v}").unwrap();
        }
        let mut locs: Vec<_> = self.locations.iter().collect();
        locs.sort_by_key(|(_, n)| **n);
        for ((line, scope), n) in locs {
            writeln!(
                out,
                "!{n} = !DILocation(line: {line}, column: 1, scope: !{scope})"
            )
            .unwrap();
        }
        out
    }
}
