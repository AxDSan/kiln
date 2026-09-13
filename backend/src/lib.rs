//! Kiln backend (Phase 2): typed IR -> textual LLVM IR (`.ll`), emitting the
//! real **slot ABI** calling convention (abi/kiln_abi.h).
//!
//! Every command is invoked as `void cmd(Slot* ret, i32 argc, Slot* argv)`
//!.  For each call the backend allocates an argv array of `%Slot`s and
//! a return slot, stores each argument's tag + reinterpreted 64-bit value, calls
//! the command by its runtime symbol (no dispatch table, no ordinal indirection
//! — G8), then reads the return slot back.  `clang` assembles + links this
//! against the static-linked command implementations (BlackMoon model, D1).
//!
//! `%Slot = { i32 tag, i32 pad, i64 value }` mirrors `Kiln_Slot` (16 bytes,
//! value at offset 8), enforced by `_Static_assert` on the C side.
//!
//! Assumes the module passed `kiln_ir::validate`.  Entry is `ECodeStart`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;

mod debug;

/// The instruction stream of the function being lowered.
///
/// It is a `fmt::Write` rather than a `String` so that a source location can
/// be attached to every instruction without touching the two hundred places
/// that write one. Whatever `loc` holds when a line is completed becomes that
/// instruction's `!dbg`; leaving it `None` emits exactly what it always did.
#[derive(Default)]
struct Body {
    text: String,
    /// The part of a line written so far. A `writeln!` reaches `write_str` in
    /// pieces — one per literal and one per argument — so a line is only whole
    /// when its newline arrives.
    pending: String,
    /// The metadata node for the statement being lowered, or `None` in a
    /// function that carries no debug information.
    loc: Option<usize>,
}

impl Body {
    fn clear(&mut self) {
        self.text.clear();
        self.pending.clear();
        self.loc = None;
    }
    fn as_str(&self) -> &str {
        &self.text
    }
}

impl std::fmt::Write for Body {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        for ch in s.chars() {
            if ch != '\n' {
                self.pending.push(ch);
                continue;
            }
            // A completed line. Instructions are indented and labels are not,
            // and metadata attaches to an instruction only.
            if let Some(n) = self.loc {
                // A debug record is not an instruction and takes no trailing
                // metadata: `#dbg_declare` carries its location as its fourth
                // argument, and appending one makes the module fail to parse.
                // It is skipped by name rather than by the `!dbg` test below,
                // because the record's own spelling is `#dbg`, not `!dbg`.
                // The intrinsic spelling of the same thing *is* an instruction
                // and does end in `, !dbg !N` — the test below is what keeps it
                // from being given a second one.
                let record = self.pending.trim_start().starts_with("#dbg_");
                if self.pending.starts_with("  ") && !record && !self.pending.contains("!dbg") {
                    self.pending.push_str(&format!(", !dbg !{n}"));
                }
            }
            self.text.push_str(&self.pending);
            self.text.push('\n');
            self.pending.clear();
        }
        Ok(())
    }
}

impl std::fmt::Display for Body {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

use kiln_ir::{CallConv, Component, Module, Registry, TargetInfo, Ty};

/// Accessibility role for a form root (`KN_ROLE_WINDOW`, abi/kiln_abi.h).
fn form_role() -> i32 {
    1
}

#[derive(Debug, Clone, PartialEq)]
pub struct LowerError {
    pub msg: String,
}
impl std::fmt::Display for LowerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "lowering error: {}", self.msg)
    }
}
impl std::error::Error for LowerError {}

fn err<T>(msg: impl Into<String>) -> Result<T, LowerError> {
    Err(LowerError { msg: msg.into() })
}

/// The hidden local that carries "is the value there" for the optional `name`.
/// `$` is not an identifier character, so no program can name it.
fn has_name(name: &str) -> String {
    format!("{name}$has")
}

/// The zero of a type, for the value half of a `none`: nothing will read it
/// (the truth beside it says so), but leaving the slot untouched would make a
/// stray read a different answer on every run.
fn zero_operand(t: Ty) -> String {
    match t {
        Ty::Double => "0.000000e+00".to_string(),
        Ty::Text | Ty::Bytes | Ty::Ptr | Ty::Array(_) | Ty::Record(_) | Ty::Dict(_) => {
            "null".to_string()
        }
        _ => "0".to_string(),
    }
}

fn llvm_ty(t: Ty) -> &'static str {
    match t {
        Ty::Int => "i32",
        Ty::Int64 => "i64",
        Ty::Double => "double",
        // Text, byte-sets and arrays are all one pointer to runtime-owned
        // storage; the aggregates cost the marshaling path nothing because a
        // pointer already fits the slot's 8-byte value.
        // A record and a dictionary are runtime-owned aggregates held by
        // pointer, exactly as an array is — which is why neither costs the
        // marshaling path anything.
        // A raw machine pointer is exactly the slot's pointer union member, so
        // it lowers as `ptr` and marshals through the same ptrtoint/inttoptr
        // catch-all as text and the aggregates.
        Ty::Text | Ty::Bytes | Ty::Ptr | Ty::Array(_) | Ty::Record(_) | Ty::Dict(_) => "ptr",
        // Bool is int-sized, matching the ABI's BOOL: `icmp` yields i1, which we
        // widen immediately so slot marshaling has one less width to handle.
        Ty::Bool => "i32",
        // A byte is a c-record field width (`i8` in the flat struct). It never
        // becomes a `Val`'s type — a byte field reads as `int` — so this only
        // serves the field GEP's load/store.
        Ty::Byte => "i8",
        // A `WORD` field is two bytes in the struct; like `byte` it never
        // becomes a `Val`'s type (it reads as `int`), so this serves the field
        // load/store alone.
        Ty::Int16 => "i16",
        // A C `float` is four bytes in the struct and a `double` everywhere
        // else in the language; the conversion happens at the load and the
        // store, so this is only ever the width in the struct.
        Ty::Float => "float",
        // An inline array field is addressed, never loaded whole: `r.rgb`
        // evaluates to the address of its first element. This arm exists so
        // the match is exhaustive.
        Ty::CArray(_) => "ptr",
        // Signature-only types; `resolve_ret` replaces them with what the call
        // actually produced before any value carries one.
        Ty::AnyArray | Ty::AnyElem | Ty::AnyDict => "ptr",
        // An optional is two locals — the value in its own width, and a hidden
        // truth value beside it — so it never has a width of its own. Anything
        // that reaches this asked for storage of a shape optionals do not have;
        // the width returned is the value half's, which is the only half a
        // stray load could sensibly want.
        Ty::Optional(e) => llvm_ty(e.ty()),
    }
}

/// Lower a whole module to a `.ll` string using the given command registry.
///
/// Entry shapes depend on the module's target:
///  * **console** — `main` is lowered into `ECodeStart` as before.
///  * **GUI** — the module declares a form; `ECodeStart` becomes the generated
///    form constructor: init the UI, create components, set properties, bind
///    handlers by function pointer, run the loop. `main`, if present, runs
///    first as start-up code.
///  * **library** (shared or static) — no entry at all. Each subroutine gets an
///    exported wrapper under its plain name so a host can call it through the
///    C ABI. Internal calls keep the mangled name, so the two never collide.
///
/// User subroutines each lower to their own `@kn_user_<name>` function so an
/// event handler can be bound by pointer. Handler names never appear as data —
/// there is no name-based dispatch at runtime (G8).
pub fn lower_module(m: &Module, reg: &Registry) -> Result<String, LowerError> {
    lower_module_from(m, reg, None)
}

/// How a variable declaration is spelled in the debug information.
///
/// LLVM changed the spelling and then removed the old one: records arrived in
/// LLVM 19 and the `llvm.dbg.*` intrinsics were deleted in LLVM 21, so there
/// is no single form every supported toolchain accepts. The caller knows which
/// clang will assemble the module and so the caller chooses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DebugFormat {
    /// `#dbg_declare(...)` — LLVM 19 and later.
    Records,
    /// `call void @llvm.dbg.declare(...)` — LLVM 18 and earlier.
    Intrinsics,
}

/// Lower, naming the source the module was parsed from.
///
/// The path is what a debugger is told to open when it stops on a line, so it
/// is the path as the user wrote it rather than one canonicalised here.
/// Passing `None` emits no debug information at all.
pub fn lower_module_from(
    m: &Module,
    reg: &Registry,
    source: Option<&str>,
) -> Result<String, LowerError> {
    lower_module_with(m, reg, source, DebugFormat::Records)
}

/// Lower, naming the source *and* the spelling its debug information uses.
///
/// `lower_module_from` assumes a current LLVM. This is the same thing for a
/// caller that has asked the toolchain which one it has.
pub fn lower_module_with(
    m: &Module,
    reg: &Registry,
    source: Option<&str>,
    debug_format: DebugFormat,
) -> Result<String, LowerError> {
    lower_module_for(m, reg, source, debug_format, TargetInfo::X86_64_LINUX)
}

/// Lower for a specific machine.
///
/// The IR text is architecture-independent by construction: opaque `ptr`, a
/// fixed 16-byte slot, and `ptrtoint`/`inttoptr` through `i64`. Only two things
/// vary, and `machine` carries both — a c-record's pointer-sized fields, and
/// which LLVM calling convention `stdcall`/`system` name on a 32-bit target.
/// Everything else about the machine (endianness, the rest of the ABI) clang
/// learns from the `--target` triple the caller passes it.
pub fn lower_module_for(
    m: &Module,
    reg: &Registry,
    source: Option<&str>,
    debug_format: DebugFormat,
    machine: TargetInfo,
) -> Result<String, LowerError> {
    // User subroutines are callable names too. The validator has already proven
    // none of them collides with a library command, so registering them here
    // cannot change what any existing call means.
    let mut with_subs = reg.clone();
    with_subs.register_subs(m);
    with_subs.register_dlls(m);
    with_subs.register_records(m);
    with_subs.register_consts(m);
    let reg = &with_subs;

    // The same rewrite the checker ran: named arguments into positional ones,
    // omitted arguments into their defaults, an inferred `let` into a typed
    // one, a record update into the literal it stands for. Running it here as
    // well is what keeps the two from ever disagreeing about what a call means
    // — there is one implementation, and both call it.
    let (desugared, sugar_errs) = kiln_ir::desugar::desugar(m, reg);
    if let Some(first) = sugar_errs.first() {
        return err(first.msg.clone());
    }
    let m = &desugared;

    let target = m.target();
    let subs: Vec<_> = m.subs().collect();
    let forms: Vec<_> = m.forms().collect();
    if target.is_executable() && forms.is_empty() && !subs.iter().any(|s| s.name == "main") {
        return err("module has no `main` subroutine and no form");
    }
    if !target.is_executable() && subs.is_empty() {
        return err("a library target must define at least one subroutine to export");
    }

    let mut lo = Lowerer {
        reg,
        strings: Vec::new(),
        body: Body::default(),
        debug: source.map(|p| {
            debug::DebugInfo::new(
                p,
                concat!("Kiln ", env!("CARGO_PKG_VERSION")),
                debug_format == DebugFormat::Records,
                machine,
            )
        }),
        machine,
        scope: None,
        stmt_line: 0,
        vars: HashMap::new(),
        used: BTreeSet::new(),
        ui_used: BTreeSet::new(),
        component_libs: BTreeSet::new(),
        loop_used: false,
        thunks: BTreeMap::new(),
        aggr_used: BTreeSet::new(),
        globals: HashMap::new(),
        allocas: Vec::new(),
        locals: 0,
        handles: HashMap::new(),
        component_types: HashMap::new(),
        tmp: 0,
        label: 0,
        loops: Vec::new(),
        ret_ty: None,
        needs_notify: false,
        needs_error_clear: false,
        dll_cached: BTreeSet::new(),
        needs_dll_get: false,
        needs_dll_text: false,
        exit_code: None,
        sub_convs: m.subs().map(|s| (s.name.clone(), s.conv)).collect(),
    };
    for g in m.globals() {
        lo.globals.insert(g.name.clone(), g.ty);
    }

    // Assign component handles BEFORE lowering subroutines: a handler may
    // address a component, and handles are compile-time constants derived from
    // creation order, so they can be known up front.
    let module_components: Vec<&Component> = m.components().collect();
    lo.map_components(forms.first().copied(), &module_components);

    // Each subroutine becomes its own function, with its declared parameters
    // and return type as a plain native signature — so a call is a call, and
    // recursion needs nothing special. A sub with neither (an entry point, an
    // event handler) still lowers to exactly `void @kn_user_x()`.
    let mut functions = String::new();
    for sub in &subs {
        lo.body.clear();
        lo.vars.clear();
        lo.allocas.clear();
        lo.locals = 0;
        lo.label = 0;
        lo.ret_ty = sub.ret;
        // Parameters arrive in SSA registers; copy each into a stack slot so
        // the rest of lowering sees an ordinary local.
        // The subprogram this function's instructions are scoped to. Declared
        // before the body is lowered, because every location inside names it.
        let symbol = user_symbol(&sub.name);
        let scope = lo
            .debug
            .as_mut()
            .map(|d| d.subprogram(&sub.name, &symbol, sub.line));
        // The parameter copies belong to the `sub` line: they are the
        // prologue, and a debugger stopping at the start of the function
        // should show the header, not the first statement.
        lo.scope = scope;
        lo.set_loc(scope, sub.line.max(1), 1);
        let prologue_loc = lo.body.loc;
        for (i, (name, ty)) in sub.params.iter().enumerate() {
            let slot = lo.alloca(*ty);
            writeln!(lo.body, "  store {} %p{i}, ptr {slot}", llvm_ty(*ty)).unwrap();
            lo.describe_local(name, &slot, *ty, false, Some(i + 1));
            lo.vars.insert(name.clone(), (slot, *ty));
        }
        // `defer` is copied to the block exits here, where the sub's return
        // type is known: a deferred cleanup must not run before the value the
        // `return` is carrying has been computed.
        let body = kiln_ir::expand_defer(&sub.body, sub.ret);
        for stmt in &body {
            lo.stmt(stmt)?;
        }
        // A value-returning sub ends in `unreachable`: the validator has proven
        // every path returns, so falling off the end cannot happen.
        //
        // Written through the instruction stream rather than appended as text,
        // so it keeps the last statement's location. An instruction with no
        // location makes a row in the line table with no line, and a debugger
        // stepping into one shows no source at all — which is what stepping
        // off the end of a subroutine would otherwise do.
        let ret_ty = match sub.ret {
            None => {
                writeln!(lo.body, "  ret void").unwrap();
                "void"
            }
            Some(t) => {
                writeln!(lo.body, "  unreachable").unwrap();
                llvm_ty(t)
            }
        };
        // Everything after this point is the compiler's own code.
        lo.scope = None;
        lo.body.loc = None;
        // `#0` pins the frame pointer, and it is what makes a local readable
        // in any frame but the innermost. Without it clang omits the frame
        // pointer for these functions and describes every local relative to
        // the stack pointer, whose value in an outer frame has to be inferred
        // from the call that left it; with it the frame base is `rbp`, which
        // the unwinder recovers for every frame it walks. It costs one
        // register in a language whose functions are not register-starved.
        let dbg = match scope {
            Some(n) => format!(" #0 !dbg !{n}"),
            None => String::new(),
        };
        let cc = lo.cc_prefix(sub.conv);
        functions.push_str(&format!(
            "define {cc}{ret_ty} @{symbol}({}){dbg} {{\nentry:\n{}{}}}\n\n",
            param_decls(&sub.params, "p"),
            lo.prologue(prologue_loc),
            lo.body,
        ));
    }

    // A library has no entry point: it exports its subroutines and stops there.
    // The wrapper carries the plain name while the body keeps the mangled one,
    // so a host links against `greet` and internal calls still resolve.
    if !target.is_executable() {
        for sub in &subs {
            let decls = param_decls(&sub.params, "a");
            let args = param_args(&sub.params, "a");
            let inner = user_symbol(&sub.name);
            // The wrapper is the exported symbol a host calls; it carries the
            // sub's own convention, so a consumer reaches a `system` sub the way
            // the source said it would be reached.
            let cc = lo.cc_prefix(sub.conv);
            functions.push_str(&match sub.ret {
                None => format!(
                    "define {cc}void @{}({decls}) {{\nentry:\n  call {cc}void @{inner}({args})\n  ret void\n}}\n\n",
                    sub.name
                ),
                Some(t) => format!(
                    "define {cc}{t2} @{}({decls}) {{\nentry:\n  %r = call {cc}{t2} @{inner}({args})\n  ret {t2} %r\n}}\n\n",
                    sub.name,
                    t2 = llvm_ty(t)
                ),
            });
        }
        // Module variables still need initialising, but a library has no moment
        // that is obviously "start-up". Exported explicitly so the host can say
        // when — an implicit constructor would run before the host is ready.
        lo.body.clear();
        lo.vars.clear();
        lo.allocas.clear();
        lo.locals = 0;
        for g in m.globals() {
            let v = lo.eval_hinted(&g.value, Some(g.ty))?;
            lo.store_global(&g.name, &v);
        }
        let init = format!(
            "define void @{}_init() {{\nentry:\n{}{}  ret void\n}}\n\n",
            m.name,
            lo.allocas.join(""),
            lo.body
        );
        functions.push_str(&init);
        return Ok(lo.finish_library(&m.name, &functions));
    }

    // The entry function. Order matters: the form must be BUILT before any user
    // code runs, or `main` could address a component that does not exist yet
    // (a segfault that would only appear for modules having both). The event
    // loop starts last, after start-up code has had its say.
    lo.body.clear();
    lo.vars.clear();
    lo.allocas.clear();
    lo.locals = 0;
    // The collector's roots, handed over before the first module variable is
    // written: a `var` initialiser can allocate, and an allocation can collect.
    {
        let roots = lo.gc_root_symbols();
        if !roots.is_empty() {
            writeln!(
                lo.body,
                "  call void @kn_gc_set_roots(ptr @kn_gc_roots, i32 {})",
                roots.len()
            )
            .unwrap();
        }
    }
    // Module variables are initialised before anything else can observe them.
    for g in m.globals() {
        let v = lo.eval_hinted(&g.value, Some(g.ty))?;
        lo.store_global(&g.name, &v);
    }
    if let Some(form) = forms.first() {
        lo.form_build(form)?;
    }
    for c in &module_components {
        lo.build_component(c)?;
    }
    if subs.iter().any(|s| s.name == "main") {
        writeln!(lo.body, "  call void @{}()", user_symbol("main")).unwrap();
    }
    // The event loop runs last, after start-up code has had its say. A module
    // with a form enters the same loop through `kn_ui_run`, which registers the
    // window as one source among whatever else is live.
    if !forms.is_empty() {
        lo.form_run();
    } else {
        lo.loop_run();
    }

    Ok(lo.finish(&m.name, &functions))
}

/// Symbol for a user subroutine. Prefixed so user names can never collide with
/// runtime symbols.
fn user_symbol(name: &str) -> String {
    format!("kn_user_{name}")
}

/// The module-level global that caches a foreign function's resolved address.
/// One per `dll` declaration, so the symbol is looked up once however many
/// times it is called.
fn dll_cache_symbol(name: &str) -> String {
    format!("kn_dllp_{name}")
}

/// `i32 %p0, ptr %p1` — a parameter list for a `define`.
fn param_decls(params: &[(String, Ty)], prefix: &str) -> String {
    params
        .iter()
        .enumerate()
        .map(|(i, (_, t))| format!("{} %{prefix}{i}", llvm_ty(*t)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The same list as call arguments (identical text here, but named separately
/// so the two uses cannot drift apart silently).
fn param_args(params: &[(String, Ty)], prefix: &str) -> String {
    param_decls(params, prefix)
}

/// Symbol for a module variable. `internal` linkage, so it is not exported and
/// the name is dropped by `strip` in release builds (G8).
fn global_symbol(name: &str) -> String {
    format!("kn_g_{name}")
}

/// A lowered value: its slot type plus its LLVM operand (a literal or `%tN`).
#[derive(Clone)]
struct Val {
    ty: Ty,
    operand: String,
}

struct Lowerer<'a> {
    reg: &'a Registry,
    strings: Vec<String>,
    body: Body,
    /// Debug metadata, when the module is being lowered with a source path to
    /// name. `None` leaves every instruction bare, exactly as before.
    debug: Option<debug::DebugInfo>,
    /// The subprogram whose statements are being lowered, so a statement can
    /// name its own source position without the position being threaded
    /// through every call that lowers one.
    scope: Option<usize>,
    /// The line of the statement being lowered, so a local can be described as
    /// declared where the user declared it.
    stmt_line: usize,
    /// Local variables: name -> (alloca pointer, type). Every local is
    /// alloca-backed, `let` and `var` alike — one lowering path, and `opt`'s
    /// mem2reg reconstructs SSA for free when optimisation is enabled.
    vars: HashMap<String, (String, Ty)>,
    /// Module-level variables: name -> type. Storage is an LLVM global.
    globals: HashMap<String, Ty>,
    /// Allocas to emit at the top of the current function. Every stack slot
    /// goes here, named locals and command-call scratch alike: an `alloca` in
    /// a loop body is a fresh stack adjustment on every turn of the loop, and
    /// nothing gives the space back until the function returns.
    allocas: Vec<String>,
    /// How many *named* slots `allocas` holds. Their names cannot be derived
    /// from the vector's length any more, because scratch slots share it.
    locals: usize,
    /// Runtime command symbols actually referenced (drives declarations).
    used: BTreeSet<String>,
    /// UI-interface symbols referenced (declared separately; see finish()).
    ui_used: BTreeSet<&'static str>,
    /// The machine being built for: pointer width for c-record layout, and the
    /// OS that decides what `system` means on a 32-bit target.
    machine: TargetInfo,
    /// A user subroutine's calling convention, by name. `Signature` — what the
    /// registry keeps for argument checking — deliberately does not carry the
    /// marker, so an internal call reads it here; the `Sub` AST node carries it
    /// for the definition itself.
    sub_convs: HashMap<String, Option<CallConv>>,
    /// Libraries whose own component entry points are referenced. Their names
    /// are only known at build time, so unlike the UI interface these cannot be
    /// a fixed set of `&'static str`.
    component_libs: BTreeSet<String>,
    /// Whether the entry point runs the event loop itself (a module with a form
    /// enters it through `kn_ui_run`).
    loop_used: bool,
    /// Handler thunks, keyed by the symbol they define so two bindings of one
    /// subroutine emit one function. See `handler_symbol`.
    thunks: BTreeMap<String, String>,
    /// Array / byte-set helpers referenced.
    ///
    /// These are plain C functions rather than slot-ABI commands: indexing is
    /// syntax, and marshaling an argv array to read one element would cost more
    /// code than the element access itself. They move raw 64-bit values, which
    /// is exactly what a slot's value field already holds, so the same
    /// reinterpretation serves both.
    aggr_used: BTreeSet<&'static str>,
    /// Component id -> its runtime widget handle.
    ///
    /// Handles are assigned by creation order, and creation order is fully
    /// static, so every id resolves to a compile-time integer constant. This is
    /// why component ids need no interning table and never reach the binary:
    /// `ok_button` simply compiles to `3`.
    handles: HashMap<String, u64>,
    /// Component id -> component type name, for resolving property types.
    component_types: HashMap<String, String>,
    tmp: usize,
    /// Basic-block label counter. Labels must be unique within a function.
    label: usize,
    /// Enclosing loops, innermost last: `(continue target, break target)`.
    /// `continue` must land on a `for`'s increment block, not its condition —
    /// jumping to the condition would never advance the counter.
    loops: Vec<(String, String)>,
    /// The return type of the subroutine being lowered, so `return []` knows
    /// what an empty list should hold.
    ret_ty: Option<Ty>,
    /// Whether any lowered code aborts through `kn_notify`, which is declared
    /// only when it is actually called.
    needs_notify: bool,
    /// Whether the error slot is cleared anywhere — an optional's initializer
    /// is the only thing that does it.
    needs_error_clear: bool,
    /// Foreign functions called, keyed by their declaration name.  Each needs
    /// one module-level `ptr` global to cache its resolved address across
    /// calls, so the symbol is looked up once no matter how many call sites
    /// there are. Deduped by name here; emitted in `finish_with`.
    dll_cached: BTreeSet<String>,
    /// Whether any `dll` call was lowered, which declares `kn_dll_get`.
    needs_dll_get: bool,
    /// Whether any `dll` returns text, which declares the copy helper.
    needs_dll_text: bool,
    /// The register holding what the event loop returned, once one has been
    /// entered. `ECodeStart` gives it back as the program's exit status: a
    /// `quit(1)` — or a server that could not bind and stopped the loop —
    /// otherwise reports success to whatever ran the program.
    exit_code: Option<String>,
}

mod lower;

fn encode_llvm_string(s: &str) -> String {
    let mut out = String::new();
    for &b in s.as_bytes() {
        match b {
            b'"' | b'\\' => write!(out, "\\{:02X}", b).unwrap(),
            0x20..=0x7E => out.push(b as char),
            _ => write!(out, "\\{:02X}", b).unwrap(),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiln_ir::parse;

    fn lower(src: &str) -> Result<String, LowerError> {
        let m = parse(src).unwrap();
        lower_module(&m, &Registry::core())
    }

    /// Lower as a build with debug information does.
    fn lower_dbg(src: &str) -> String {
        let m = parse(src).unwrap();
        lower_module_from(&m, &Registry::core(), Some("examples/demo.kiln")).unwrap()
    }

    /// Lower for a named machine, with the core registry. What the CLI does
    /// when it is asked for a target other than the host.
    fn lower_for(src: &str, machine: TargetInfo) -> Result<String, LowerError> {
        let m = parse(src).unwrap();
        lower_module_for(&m, &Registry::core(), None, DebugFormat::Records, machine)
    }

    /// A c-record's pointer-sized field is four bytes on a 32-bit target and
    /// eight on a 64-bit one: the one number that makes every Win32 struct
    /// (and every `is c` record) lay out the way the target's C compiler does.
    #[test]
    fn c_record_pointer_fields_follow_the_target() {
        let src = "module m\nrecord S is c\n  a: int\n  p: ptr\n  b: int\nend\n\
                   sub main\n  call print_int(int64_to_int(size of S))\nend\n";
        let x64 = lower_for(src, TargetInfo::X86_64_LINUX).unwrap();
        assert!(x64.contains("store i64 24, ptr"), "{x64}");
        let x86 = lower_for(src, TargetInfo::X86_WINDOWS).unwrap();
        assert!(x86.contains("store i64 12, ptr"), "{x86}");
    }

    /// A Win32 `system` call is `stdcall` on 32-bit Windows and the ordinary C
    /// convention everywhere else. Getting this wrong corrupts the stack.
    #[test]
    fn system_dll_calls_are_stdcall_on_32_bit_windows_only() {
        let src = "module m\n\
                   dll MessageBoxA(h: ptr, t: text, c: text, k: int): int from \"user32\" system\n\
                   sub main\n  call MessageBoxA(ptr_null(), \"a\", \"b\", 0)\nend\n";
        let x64 = lower_for(src, TargetInfo::X86_64_WINDOWS).unwrap();
        assert!(!x64.contains("x86_stdcallcc"), "{x64}");
        let x86 = lower_for(src, TargetInfo::X86_WINDOWS).unwrap();
        assert!(x86.contains("call x86_stdcallcc i32"), "{x86}");
    }

    /// A `system` subroutine is defined `stdcall` on 32-bit Windows, and the
    /// call to it — the internal one and the exported wrapper — agrees, or the
    /// verifier rejects the module.
    #[test]
    fn a_system_sub_is_stdcall_throughout_on_32_bit_windows() {
        let src = "module m\n\
                   sub cb(a: int): int system\n  return a\nend\n\
                   sub main\n  call print_int(cb(1))\nend\n";
        let x86 = lower_for(src, TargetInfo::X86_WINDOWS).unwrap();
        assert!(
            x86.contains("define x86_stdcallcc i32 @kn_user_cb"),
            "{x86}"
        );
        assert!(x86.contains("call x86_stdcallcc i32 @kn_user_cb"), "{x86}");
        let x64 = lower_for(src, TargetInfo::X86_64_LINUX).unwrap();
        assert!(x64.contains("define i32 @kn_user_cb"), "{x64}");
        assert!(!x64.contains("x86_stdcallcc"), "{x64}");
    }

    /// `Registry::core()` plus a non-visual component whose `beep` event hands
    /// its handler an int — the shape the `timer`'s `tick` has, without putting
    /// an invented component into the hard-coded core set.
    fn lower_with_buzzer(src: &str) -> String {
        use kiln_ir::registry::{ComponentDesc, ComponentKind};
        let mut reg = Registry::core();
        reg.insert_component(ComponentDesc {
            name: "buzzer".into(),
            a11y_role: 0,
            kind: ComponentKind::NonVisual,
            library: "core".into(),
            properties: Vec::new(),
            events: vec!["beep".into()],
        });
        reg.set_event_params("buzzer", "beep", vec![Ty::Int]);
        lower_module(&parse(src).unwrap(), &reg).unwrap()
    }

    const BUZZER: &str =
        "module m\n\nbuzzer b\n  on beep: h\nend\n\nsub main\n  call print_int(1)\nend\n\n";

    /// The library is handed a pointer with the EVENT's signature, never the
    /// handler's: that is what makes the cast on the C side type-correct.
    #[test]
    fn a_parameterised_event_binds_through_a_thunk() {
        let ll = lower_with_buzzer(&format!(
            "{BUZZER}sub h(n: int)\n  call print_int(n)\nend\n"
        ));
        assert!(
            ll.contains("define internal void @kn_evt_h_i32(i32 %a0)"),
            "{ll}"
        );
        assert!(ll.contains("call void @kn_user_h(i32 %a0)"), "{ll}");
        assert!(
            ll.contains("@kn_core_component_on(") && ll.contains("ptr @kn_evt_h_i32)"),
            "{ll}"
        );
    }

    /// A handler that ignores the argument is bound through a thunk that drops
    /// it, rather than through a pointer the library would have to call with
    /// the wrong type.
    #[test]
    fn a_handler_that_ignores_the_argument_still_gets_the_event_signature() {
        let ll = lower_with_buzzer(&format!("{BUZZER}sub h\n  call print_int(1)\nend\n"));
        assert!(
            ll.contains("define internal void @kn_evt_h_i32(i32 %a0)"),
            "{ll}"
        );
        assert!(ll.contains("call void @kn_user_h()"), "{ll}");
    }

    /// Two components binding one subroutine to one event share a thunk. Two
    /// `define`s of one name is not a diagnostic anywhere in this compiler —
    /// it is invalid IR that `llc` rejects at the end of the build.
    #[test]
    fn two_bindings_of_one_handler_share_a_thunk() {
        let ll = lower_with_buzzer(
            "module m\n\nbuzzer b1\n  on beep: h\nend\n\nbuzzer b2\n  on beep: h\nend\n\n             sub main\n  call print_int(1)\nend\n\nsub h(n: int)\n  call print_int(n)\nend\n",
        );
        assert_eq!(
            ll.matches("define internal void @kn_evt_h_i32").count(),
            1,
            "{ll}"
        );
        assert_eq!(ll.matches("ptr @kn_evt_h_i32)").count(), 2, "{ll}");
    }

    /// An event that hands nothing over binds the subroutine itself, so every
    /// program written before events could carry anything lowers to what it
    /// lowered to before.
    #[test]
    fn an_event_with_no_parameters_binds_the_subroutine_directly() {
        let ll = lower_with_buzzer(&format!(
            "module m\n\nbuzzer b\nend\n\nsub main\n  call print_int(1)\nend\n"
        ));
        assert!(!ll.contains("kn_evt_"), "{ll}");
    }

    #[test]
    fn emits_slot_type_and_abi_call() {
        let ll = lower("module m\nsub main\n  call print_int(42)\nend\n").unwrap();
        assert!(ll.contains("%Slot = type { i32, i32, i64 }"));
        assert!(ll.contains("declare void @kn_print_int(ptr, i32, ptr)"));
        assert!(ll.contains("call void @kn_print_int(ptr %"));
        assert!(ll.contains("store i32 3, ptr")); // SDT_INT tag
    }

    #[test]
    fn call_expr_reads_return_slot() {
        let ll =
            lower("module m\nsub main\n  let n: int = length(\"hi\")\n  call print_int(n)\nend\n")
                .unwrap();
        assert!(ll.contains("call void @kn_length(ptr"));
        assert!(ll.contains("load i64, ptr")); // reads the return slot
        assert!(ll.contains("trunc i64")); // int result reinterpret
        assert!(ll.contains("ptrtoint ptr")); // text arg reinterpret
    }

    #[test]
    fn double_roundtrips_through_slot() {
        let ll =
            lower("module m\nsub main\n  let r: double = sqrt(2.0)\n  call print_double(r)\nend\n")
                .unwrap();
        assert!(ll.contains("bitcast double 0x")); // arg store
        assert!(ll.contains("bitcast i64")); // return reinterpret
    }

    /// A subroutine call is a direct native call, not a slot-ABI marshalling
    /// dance — which is what makes recursion cost nothing special.
    #[test]
    fn user_subs_lower_to_native_functions() {
        let ll = lower(
            "module m\nsub fib(n: int): int\n  if n < 2\n    return n\n  end\n  return fib(n - 1) + fib(n - 2)\nend\nsub main\n  call print_int(fib(10))\nend\n",
        )
        .unwrap();
        assert!(ll.contains("define i32 @kn_user_fib(i32 %p0)"), "{ll}");
        assert!(ll.contains("call i32 @kn_user_fib(i32 "), "{ll}");
        assert!(ll.contains("ret i32 "), "{ll}");
        // The fall-through past the last `return` is proven dead.
        assert!(ll.contains("unreachable"), "{ll}");
        // A user sub is defined, never declared: a declare + define of the same
        // symbol is not a valid module.
        assert!(!ll.contains("declare void @kn_user_fib"), "{ll}");
    }

    /// Entry points and event handlers must lower to exactly the shape they did
    /// before parameters existed, or a handler bound by pointer would be called
    /// through a mismatched signature.
    #[test]
    fn a_plain_sub_lowers_unchanged() {
        let ll = lower("module m\nsub main\n  call print_int(1)\nend\n").unwrap();
        assert!(ll.contains("define void @kn_user_main() {"), "{ll}");
        assert!(ll.contains("call void @kn_user_main()"), "{ll}");
    }

    #[test]
    fn a_library_export_forwards_its_arguments() {
        let ll = lower("module m\ntarget sharedlib\nsub twice(n: int): int\n  return n + n\nend\n")
            .unwrap();
        assert!(ll.contains("define i32 @twice(i32 %a0)"), "{ll}");
        assert!(ll.contains("call i32 @kn_user_twice(i32 %a0)"), "{ll}");
    }

    #[test]
    fn text_plus_lowers_to_the_concat_command() {
        let ll = lower("module m\nsub main\n  call print_text(\"a\" + \"b\")\nend\n").unwrap();
        assert!(ll.contains("declare void @kn_concat(ptr, i32, ptr)"));
        assert!(ll.contains("call void @kn_concat(ptr %"));
    }

    /// Each side of a text `+` must be evaluated exactly once. Forwarding the
    /// original expressions to `concat` instead of the values would make
    /// `read_line() + \"!\"` read two lines.
    #[test]
    fn text_plus_evaluates_each_side_once() {
        let ll =
            lower("module m\nsub main\n  call print_text(read_line() + \"!\")\nend\n").unwrap();
        assert_eq!(ll.matches("call void @kn_read_line(").count(), 1);
    }

    #[test]
    fn a_literal_divisor_needs_no_guard() {
        let ll =
            lower("module m\nsub main\n  var n: int = 9\n  call print_int(n / 3)\nend\n").unwrap();
        assert!(ll.contains("sdiv"));
        assert!(!ll.contains("kn_notify"), "a constant 3 cannot be zero");
    }

    /// A divisor that is not a literal is checked for the two values the
    /// hardware faults on: zero, and -1 against the most negative dividend.
    #[test]
    fn a_variable_divisor_is_guarded_both_ways() {
        let ll =
            lower("module m\nsub main\n  var d: int = 0\n  call print_int(10 / d)\nend\n").unwrap();
        assert!(ll.contains("declare ptr @kn_notify(i32, ptr, ptr)"));
        assert!(ll.contains("call ptr @kn_notify(i32 5,"));
        assert!(ll.contains("icmp eq i32 %"));
        assert!(ll.contains("-2147483648"), "the overflow check is missing");
        assert!(ll.contains("unreachable"));
    }

    #[test]
    fn remainder_lowers_to_srem_and_frem() {
        let i = lower("module m\nsub main\n  call print_int(7 % 2)\nend\n").unwrap();
        assert!(i.contains("srem i32"));
        let d = lower("module m\nsub main\n  call print_double(7.5 % 2.0)\nend\n").unwrap();
        assert!(d.contains("frem double"));
    }

    #[test]
    fn negation_uses_fneg_on_doubles_and_a_subtract_on_integers() {
        let ll = lower("module m\nsub main\n  var n: int = 1\n  var d: double = 1.0\n  call print_int(-n)\n  call print_double(-d)\nend\n").unwrap();
        assert!(ll.contains("sub i32 0,"));
        assert!(ll.contains("fneg double"));
    }

    /// `continue` in a `for` must reach the increment block. Branching back to
    /// the condition instead would leave the counter unchanged — an infinite
    /// loop that no test of the IR's shape alone would catch.
    #[test]
    fn continue_in_a_for_targets_the_increment_block() {
        let ll = lower("module m\nsub main\n  for i = 1 to 3\n    continue\n  end\nend\n").unwrap();
        let next = ll
            .lines()
            .map(|l| l.trim())
            .find(|l| l.starts_with("bb_fornext_"))
            .expect("an increment block")
            .trim_end_matches(':')
            .to_string();
        // The `continue` is the branch immediately followed by the dead block
        // the jump opens; that is the one whose target must be the increment.
        let lines: Vec<&str> = ll.lines().map(|l| l.trim()).collect();
        let i = lines
            .iter()
            .position(|l| l.starts_with("bb_postjump_"))
            .expect("continue opens a dead block");
        assert_eq!(lines[i - 1], format!("br label %{next}"), "{ll}");
    }

    #[test]
    fn break_leaves_the_innermost_loop_only() {
        let ll = lower("module m\nsub main\n  for i = 1 to 3\n    for j = 1 to 3\n      break\n    end\n  end\nend\n").unwrap();
        // Two loops, so two end blocks; the `break` must name the inner one.
        let ends: Vec<&str> = ll
            .lines()
            .map(|l| l.trim())
            .filter(|l| l.starts_with("bb_forend_"))
            .collect();
        assert_eq!(ends.len(), 2, "{ll}");
        let inner = ends[0].trim_end_matches(':');
        assert!(ll.contains(&format!("br label %{inner}")), "{ll}");
    }

    /// Indexing must NOT go through the slot ABI: marshaling an argv array to
    /// read one element would cost more code than the read.
    #[test]
    fn indexing_calls_the_helper_directly() {
        let ll =
            lower("module m\nsub main\n  var xs: int[] = [7, 8]\n  xs[0] = xs[1]\nend\n").unwrap();
        assert!(ll.contains("call ptr @kn_ary_new(i32 3, i32 2)"), "{ll}");
        assert!(ll.contains("call i64 @kn_ary_get(ptr"), "{ll}");
        assert!(ll.contains("call void @kn_ary_set(ptr"), "{ll}");
        assert!(ll.contains("declare i64 @kn_ary_get(ptr, i32)"), "{ll}");
    }

    /// A field is reached by POSITION, counting from 1. No field name may
    /// appear in the output: which record a value is, and where a field sits
    /// inside it, are both compile-time facts.
    #[test]
    fn a_field_is_reached_by_position_and_never_by_name() {
        let ll = lower(
            "module m\nrecord point\n  x: int\n  y: int\nend\n\
             sub main\n  var p: point = point(x: 7, y: 8)\n  p.y = p.x\nend\n",
        )
        .unwrap();
        assert!(ll.contains("call ptr @kn_rec_new(i32 2)"), "{ll}");
        // `x` is field 1 and `y` is field 2, in declaration order.
        assert!(ll.contains("@kn_rec_set(ptr %t0, i32 1,"), "{ll}");
        assert!(ll.contains("@kn_rec_set(ptr %t0, i32 2,"), "{ll}");
        // `p.y = p.x` reads field 1 and writes field 2.
        assert!(ll.contains(", i32 1)\n"), "{ll}");
        assert!(
            !ll.contains("\"x\\00\""),
            "a field name reached the output:\n{ll}"
        );
    }

    /// Reading a field is a direct helper call, not a marshalled command —
    /// the same bargain indexing already makes.
    #[test]
    fn a_field_read_does_not_go_through_the_slot_abi() {
        let ll = lower(
            "module m\nrecord point\n  x: int\nend\n\
             sub main\n  let p: point = point(x: 1)\n  call print_int(p.x)\nend\n",
        )
        .unwrap();
        assert!(ll.contains("declare i64 @kn_rec_get(ptr, i32)"), "{ll}");
        assert!(ll.contains("declare ptr @kn_rec_new(i32)"), "{ll}");
    }

    /// A record passed to a subroutine is one pointer, so a sub that takes one
    /// and a sub that returns one need nothing the ABI did not already have.
    #[test]
    fn a_record_crosses_a_subroutine_boundary_as_a_pointer() {
        let ll = lower(
            "module m\nrecord point\n  x: int\nend\n\
             sub bump(p: point): point\n  return point(x: p.x + 1)\nend\n\
             sub main\n  let a: point = bump(point(x: 1))\n  call print_int(a.x)\nend\n",
        )
        .unwrap();
        assert!(ll.contains("define ptr @kn_user_bump(ptr %p0)"), "{ll}");
    }

    /// `d["k"]` and `d["k"] = v` are `dict_get`/`dict_set` spelled as a
    /// subscript, and reach the same direct helpers indexing does.
    #[test]
    fn a_dictionary_subscript_calls_the_helper_directly() {
        let ll = lower(
            "module m\nsub main\n  var d: int{} = {\"a\": 1}\n               d[\"b\"] = d[\"a\"]\nend\n",
        )
        .unwrap();
        // KN_SDT_DICT_OF(KN_SDT_INT) is not a tag: the value tag alone is what
        // the dictionary is told to hold.
        assert!(ll.contains("call ptr @kn_dict_new(i32 3)"), "{ll}");
        assert!(ll.contains("call i64 @kn_dict_at(ptr"), "{ll}");
        assert!(ll.contains("call void @kn_dict_put(ptr"), "{ll}");
        assert!(ll.contains("declare i64 @kn_dict_at(ptr, ptr)"), "{ll}");
    }

    /// A dictionary reaches a command as a pointer with the dictionary flag
    /// above its value tag — KN_SDT_DICT_FLAG | KN_SDT_INT.
    #[test]
    fn a_dictionary_marshals_as_a_pointer() {
        let ll = lower(
            "module m\nsub main\n  var d: int{} = {}\n               call print_int(dict_count(d))\nend\n",
        )
        .unwrap();
        assert!(ll.contains("store i32 515,"), "{ll}");
    }

    /// An array is a pointer in the slot's 8-byte value field, marshalled
    /// exactly the way text already is — that is what let aggregates arrive
    /// without widening anything.
    #[test]
    fn an_array_marshals_as_a_pointer() {
        let ll =
            lower("module m\nsub main\n  var xs: int[] = [1]\n  call print_int(count(xs))\nend\n")
                .unwrap();
        assert!(ll.contains("ptrtoint ptr"), "{ll}");
        // KN_SDT_ARRAY_FLAG | KN_SDT_INT
        assert!(ll.contains("store i32 259,"), "{ll}");
    }

    #[test]
    fn a_byte_set_indexes_through_its_own_helper() {
        let ll = lower(
            "module m\nsub main\n  var b: bytes = bytes_new(1)\n  b[0] = 65\n  \
             call print_int(b[0])\nend\n",
        )
        .unwrap();
        assert!(ll.contains("call void @kn_bin_set(ptr"), "{ll}");
        assert!(ll.contains("call i32 @kn_bin_at(ptr"), "{ll}");
    }

    /// A module-level array starts as no array at all, and a pointer's zero is
    /// `null` — `0` would not even assemble.
    #[test]
    fn a_module_level_array_is_null_initialised() {
        let ll =
            lower("module m\nvar xs: int[] = [1]\nsub main\n  call print_int(count(xs))\nend\n")
                .unwrap();
        assert!(ll.contains("= internal global ptr null"), "{ll}");
    }

    #[test]
    fn only_used_commands_declared() {
        let ll = lower("module m\nsub main\n  call print_int(1)\nend\n").unwrap();
        assert!(ll.contains("declare void @kn_print_int(ptr, i32, ptr)"));
        assert!(!ll.contains("kn_sqrt"));
    }

    /// Without a source path nothing changes: this is what a `--release`
    /// build lowers, and it must be byte-for-byte what it always was.
    #[test]
    fn no_source_path_means_no_debug_information() {
        let ll = lower("module m\nsub main\n  call print_int(1)\nend\n").unwrap();
        assert!(!ll.contains("!dbg"), "{ll}");
        assert!(!ll.contains("!llvm.dbg.cu"), "{ll}");
        assert!(!ll.contains("DICompileUnit"), "{ll}");
    }

    #[test]
    fn a_module_with_a_source_path_carries_a_compile_unit() {
        let ll = lower_dbg("module m\nsub main\n  call print_int(1)\nend\n");
        assert!(ll.contains("!llvm.dbg.cu = !{!0}"), "{ll}");
        assert!(ll.contains("emissionKind: LineTablesOnly"), "{ll}");
        assert!(
            ll.contains(r#"!DIFile(filename: "demo.kiln", directory: "examples")"#),
            "{ll}"
        );
        assert!(ll.contains(r#"!{i32 7, !"Dwarf Version", i32 5}"#), "{ll}");
        assert!(
            ll.contains(r#"!{i32 2, !"Debug Info Version", i32 3}"#),
            "{ll}"
        );
    }

    /// A subroutine is scoped to a subprogram named for the line the `sub`
    /// keyword is on, and its `define` names that node.
    #[test]
    fn a_subroutine_becomes_a_subprogram_at_its_own_line() {
        let ll = lower_dbg(
            "module m\n\nsub greet\n  call print_int(1)\nend\n\nsub main\n  call greet()\nend\n",
        );
        assert!(
            ll.contains(r#"!DISubprogram(name: "greet", linkageName: "kn_user_greet""#),
            "{ll}"
        );
        assert!(ll.contains("scopeLine: 3,"), "{ll}");
        assert!(
            ll.contains("define void @kn_user_greet() #0 !dbg !6 {"),
            "{ll}"
        );
        // and `main`, three lines lower, gets its own subprogram
        assert!(
            ll.contains(r#"!DISubprogram(name: "main", linkageName: "kn_user_main""#),
            "{ll}"
        );
        assert!(ll.contains("scopeLine: 7,"), "{ll}");
    }

    /// The whole point: each statement's instructions carry that statement's
    /// line, so a debugger stepping one row of the table moves one statement.
    #[test]
    fn each_statement_gets_its_own_location() {
        let ll = lower_dbg(
            "module m\nsub main\n  call print_int(1)\n  call print_int(2)\n  call print_int(3)\nend\n",
        );
        for line in [3, 4, 5] {
            assert!(
                ll.contains(&format!("!DILocation(line: {line}, column: 3, scope: !6)")),
                "no location for line {line} in {ll}"
            );
        }
    }

    /// `!dbg` attaches to instructions. A label is not one, and LLVM refuses a
    /// module that puts metadata on it.
    #[test]
    fn a_label_never_carries_a_location() {
        let ll = lower_dbg("module m\nsub main\n  if 1 = 1\n    call print_int(1)\n  end\nend\n");
        for line in ll.lines() {
            if line.ends_with(':') && !line.starts_with(' ') {
                assert!(!line.contains("!dbg"), "label carried a location: {line}");
            }
        }
        // and the instructions inside the branch did get one
        assert!(ll.contains("call void @kn_print_int"), "{ll}");
        assert!(ll.contains(", !dbg !"), "{ll}");
    }

    /// The functions the compiler synthesises carry no debug information, so
    /// nothing inside them needs a location. A function that *has* debug info
    /// must give every call one, and there is no line in anyone's source to
    /// give the entry point's call to `main`.
    #[test]
    fn synthesised_functions_carry_no_debug_information() {
        let ll = lower_dbg("module m\nsub main\n  call print_int(1)\nend\n");
        let entry = ll
            .split("define i32 @ECodeStart()")
            .nth(1)
            .expect("an entry point");
        assert!(!entry.starts_with(" !dbg"), "{ll}");
        let body = entry.split("\n}").next().unwrap();
        assert!(
            !body.contains("!dbg"),
            "entry point carried locations: {body}"
        );
    }

    /// A debug record carries its location as an argument, not as trailing
    /// metadata. Appending `!dbg` to one makes the whole module fail to parse,
    /// and the instruction stream cannot tell the difference by the `!dbg`
    /// test alone, because a record's spelling is `#dbg`.
    #[test]
    fn a_debug_record_is_not_given_trailing_metadata() {
        use std::fmt::Write as _;
        let mut body = Body {
            loc: Some(7),
            ..Body::default()
        };
        writeln!(body, "  store i32 1, ptr %v0").unwrap();
        writeln!(body, "  #dbg_declare(ptr %v0, !8, !DIExpression(), !7)").unwrap();
        let text = body.as_str();
        assert!(text.contains("store i32 1, ptr %v0, !dbg !7"), "{text}");
        for line in text.lines().filter(|l| l.contains("#dbg_")) {
            assert!(
                !line.ends_with(", !dbg !7"),
                "record carried metadata: {line}"
            );
        }
    }

    /// Every `alloca` belongs to the `entry:` block, and this is not a matter
    /// of taste. LLVM turns an `alloca` in any other block into a *dynamic*
    /// stack adjustment made where it stands, and nothing gives that space
    /// back until the function returns — so a loop whose body reserves a slot
    /// reserves another one on every turn. A loop calling a command used to
    /// exhaust an 8 MiB stack at around a quarter of a million iterations and
    /// die with a segmentation fault.
    fn allocas_outside_entry(ll: &str) -> Vec<String> {
        let mut block = "entry:".to_string();
        let mut stray = Vec::new();
        for line in ll.lines() {
            if line.starts_with("define") {
                block = "entry:".to_string();
            } else if !line.starts_with(' ') && line.ends_with(':') {
                block = line.to_string();
            } else if line.contains(" = alloca ") && block != "entry:" {
                stray.push(format!("{block} {}", line.trim()));
            }
        }
        stray
    }

    #[test]
    fn a_command_call_in_a_loop_reserves_its_slots_once() {
        let ll = lower("module m\nsub main\n  for i in 1..10\n    call print_int(i)\n  end\nend\n")
            .unwrap();
        assert_eq!(allocas_outside_entry(&ll), Vec::<String>::new(), "{ll}");
    }

    /// The text commands reach the runtime through their own call path, so
    /// they get their own case rather than trusting the one above to cover it.
    #[test]
    fn joining_text_in_a_loop_reserves_its_slots_once() {
        let ll = lower(
            "module m\nsub main\n  var s: text = \"\"\n  for i in 1..10\n    s = s + \"x\"\n  end\n  call print_text(s)\nend\n",
        )
        .unwrap();
        assert_eq!(allocas_outside_entry(&ll), Vec::<String>::new(), "{ll}");
    }

    /// Branches are the other shape that used to strand an alloca outside
    /// `entry:` — harmless on its own, fatal inside a loop.
    #[test]
    fn a_command_call_in_a_branch_reserves_its_slots_once() {
        let ll =
            lower("module m\nsub main\n  if 1 = 1\n    call print_int(1)\n  end\nend\n").unwrap();
        assert_eq!(allocas_outside_entry(&ll), Vec::<String>::new(), "{ll}");
    }

    /// Hoisting the scratch slots must not renumber the named ones: they are
    /// `%v0`, `%v1`, … in declaration order, and share the prologue with
    /// slots named from the temporary counter.
    #[test]
    fn hoisting_scratch_slots_leaves_named_locals_numbered_in_order() {
        let ll = lower(
            "module m\nsub main\n  var a: int = 1\n  call print_int(a)\n  var b: int = 2\n  call print_int(b)\nend\n",
        )
        .unwrap();
        assert!(ll.contains("%v0 = alloca i32"), "{ll}");
        assert!(ll.contains("%v1 = alloca i32"), "{ll}");
        assert!(!ll.contains("%v2 = alloca"), "{ll}");
    }
}
