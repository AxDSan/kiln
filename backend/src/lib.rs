//! OpenEPL backend (Phase 2): typed IR -> textual LLVM IR (`.ll`), emitting the
//! real **slot ABI** calling convention (abi/openepl_abi.h).
//!
//! Every command is invoked as `void cmd(Slot* ret, i32 argc, Slot* argv)`
//!.  For each call the backend allocates an argv array of `%Slot`s and
//! a return slot, stores each argument's tag + reinterpreted 64-bit value, calls
//! the command by its runtime symbol (no dispatch table, no ordinal indirection
//! — G8), then reads the return slot back.  `clang` assembles + links this
//! against the static-linked command implementations (BlackMoon model, D1).
//!
//! `%Slot = { i32 tag, i32 pad, i64 value }` mirrors `OpenEPL_Slot` (16 bytes,
//! value at offset 8), enforced by `_Static_assert` on the C side.
//!
//! Assumes the module passed `openepl_ir::validate`.  Entry is `ECodeStart`.

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


use openepl_ir::sema::resolve_ret;
use openepl_ir::registry::{ComponentKind, DllSig};
use openepl_ir::{
    BinOp, BitOp, CmpOp, Component, Elem, Expr, LogicalOp, Module, Registry, Signature, Ty,
};

/// Accessibility role for a form root (`OE_ROLE_WINDOW`, abi/openepl_abi.h).
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
/// User subroutines each lower to their own `@oe_user_<name>` function so an
/// event handler can be bound by pointer. Handler names never appear as data —
/// there is no name-based dispatch at runtime (G8).
pub fn lower_module(m: &Module, reg: &Registry) -> Result<String, LowerError> {
    lower_module_from(m, reg, None)
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
    let (desugared, sugar_errs) = openepl_ir::desugar::desugar(m, reg);
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
        debug: source.map(|p| debug::DebugInfo::new(p, concat!("OpenEPL ", env!("CARGO_PKG_VERSION")))),
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
    // event handler) still lowers to exactly `void @oe_user_x()`.
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
        let body = openepl_ir::expand_defer(&sub.body, sub.ret);
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
        functions.push_str(&format!(
            "define {ret_ty} @{symbol}({}){dbg} {{\nentry:\n{}{}}}\n\n",
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
            functions.push_str(&match sub.ret {
                None => format!(
                    "define void @{}({decls}) {{\nentry:\n  call void @{inner}({args})\n  ret void\n}}\n\n",
                    sub.name
                ),
                Some(t) => format!(
                    "define {t2} @{}({decls}) {{\nentry:\n  %r = call {t2} @{inner}({args})\n  ret {t2} %r\n}}\n\n",
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
    // with a form enters the same loop through `oe_ui_run`, which registers the
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
    format!("oe_user_{name}")
}

/// The module-level global that caches a foreign function's resolved address.
/// One per `dll` declaration, so the symbol is looked up once however many
/// times it is called.
fn dll_cache_symbol(name: &str) -> String {
    format!("oe_dllp_{name}")
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
    format!("oe_g_{name}")
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
    /// Libraries whose own component entry points are referenced. Their names
    /// are only known at build time, so unlike the UI interface these cannot be
    /// a fixed set of `&'static str`.
    component_libs: BTreeSet<String>,
    /// Whether the entry point runs the event loop itself (a module with a form
    /// enters it through `oe_ui_run`).
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
    /// Whether any lowered code aborts through `oe_notify`, which is declared
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
    /// Whether any `dll` call was lowered, which declares `oe_dll_get`.
    needs_dll_get: bool,
    /// Whether any `dll` returns text, which declares the copy helper.
    needs_dll_text: bool,
    /// The register holding what the event loop returned, once one has been
    /// entered. `ECodeStart` gives it back as the program's exit status: a
    /// `quit(1)` — or a server that could not bind and stopped the loop —
    /// otherwise reports success to whatever ran the program.
    exit_code: Option<String>,
}

impl Lowerer<'_> {
    fn fresh(&mut self) -> String {
        let t = format!("%t{}", self.tmp);
        self.tmp += 1;
        t
    }

    /// A hidden local variable name for a desugaring — the counter and the
    /// collection snapshot a `for each` lowers through. It carries a `$`, which
    /// no source identifier can, so it never shadows a user variable that lands
    /// in the same `self.vars` table.
    fn fresh_hidden(&mut self, tag: &str) -> String {
        let s = format!("$each${tag}${}", self.tmp);
        self.tmp += 1;
        s
    }

    /// Emit a string constant and return an operand pointing at it.
    fn cstr(&mut self, text: &str) -> String {
        let id = self.strings.len();
        self.strings.push(text.to_string());
        let bytes = text.len() + 1;
        format!("getelementptr inbounds ([{bytes} x i8], ptr @.str{id}, i64 0, i64 0)")
    }

    /// Render a property value literal as the text the UI layer expects.
    /// Values are textual at the D10 boundary in v0 (see abi/openepl_ui.h).
    fn property_text(&self, e: &Expr) -> Result<String, LowerError> {
        Ok(match e {
            Expr::TextLit(s) => s.clone(),
            Expr::IntLit(v) => v.to_string(),
            // A property written as a bit pattern (`width = 0x1E0`) is a
            // literal like any other; the UI layer wants the number.
            Expr::BitsLit(v) => openepl_ir::sema::bits_value(*v).to_string(),
            Expr::DoubleLit(v) => format!("{v}"),
            Expr::BoolLit(b) => b.to_string(),
            _ => return err("component property values must be literals in v0.2"),
        })
    }

    /// Assign each component its compile-time handle constant.
    ///
    /// Handles count from 1 in creation order, per library (abi/openepl_abi.h),
    /// which is exactly the order the create calls below are emitted in. The
    /// form root is the `ui` library's handle 1, so its children start at 2;
    /// every other library starts at 1. Two libraries' counters never meet,
    /// because a handle is only ever passed back to the entry points of the
    /// library that issued it.
    fn map_components(&mut self, form: Option<&openepl_ir::Form>, module_components: &[&Component]) {
        let mut next: HashMap<String, u64> = HashMap::new();
        if form.is_some() {
            next.insert("ui".to_string(), 2);
        }
        let children = form.map(|f| f.children.iter()).into_iter().flatten();
        for child in children.chain(module_components.iter().copied()) {
            let lib = self
                .reg
                .component(&child.type_name)
                .map(|d| d.library.clone())
                .unwrap_or_default();
            let slot = next.entry(lib).or_insert(1);
            self.handles.insert(child.id.clone(), *slot);
            *slot += 1;
            self.component_types
                .insert(child.id.clone(), child.type_name.clone());
        }
    }

    /// The library whose own entry points address this component, or `None`
    /// when it is visual and goes through the `ui` widget interface instead.
    fn owner(&self, id: &str) -> Option<String> {
        let type_name = self.component_types.get(id)?;
        let desc = self.reg.component(type_name)?;
        match desc.kind {
            ComponentKind::Visual => None,
            ComponentKind::NonVisual => Some(desc.library.clone()),
        }
    }

    /// Create a non-visual component and apply its properties and bindings.
    ///
    /// Every step has a visual counterpart in `form_build` doing the same job
    /// through `oe_ui_*`: that symmetry is the point of the `kind` field, and
    /// is why the inspector, the checker and the code preview need no new
    /// concepts to show a timer.
    fn build_component(&mut self, c: &Component) -> Result<(), LowerError> {
        let lib = self.owner(&c.id).ok_or_else(|| LowerError {
            msg: format!("unknown component type `{}`", c.type_name),
        })?;
        self.component_libs.insert(lib.clone());
        let type_op = self.cstr(&c.type_name);
        let handle = self.fresh();
        writeln!(
            self.body,
            "  {handle} = call i64 @oe_{lib}_component_create(ptr {type_op})"
        )
        .unwrap();
        for (name, value) in &c.properties {
            let text = self.property_text(value)?;
            self.set_property(Some(&lib), &handle, name, &text);
        }
        self.bind_handlers_to(Some(&lib), &c.type_name, &handle, &c.handlers);
        Ok(())
    }

    fn form_build(&mut self, form: &openepl_ir::Form) -> Result<(), LowerError> {
        // Window geometry/title come from the form's own properties.
        let mut title = "OpenEPL Application".to_string();
        let (mut width, mut height) = (800i64, 600i64);
        for (name, value) in &form.properties {
            match name.as_str() {
                "title" => title = self.property_text(value)?,
                "width" => width = self.property_text(value)?.parse().unwrap_or(800),
                "height" => height = self.property_text(value)?.parse().unwrap_or(600),
                _ => {}
            }
        }

        let title_op = self.cstr(&title);
        self.ui_used.insert("oe_ui_init");
        writeln!(
            self.body,
            "  call i32 @oe_ui_init(ptr {title_op}, i32 {width}, i32 {height})"
        )
        .unwrap();

        // The root is always handle 1; children follow in creation order.
        self.ui_used.insert("oe_ui_root");
        let root_tmp = self.fresh();
        writeln!(self.body, "  {root_tmp} = call i64 @oe_ui_root()").unwrap();
        let root = "1".to_string();

        // Root properties (skip the window-level ones already consumed).
        for (name, value) in &form.properties {
            if matches!(name.as_str(), "title" | "width" | "height") {
                continue;
            }
            self.set_property(None, &root, name, &self.property_text(value)?);
        }
        // The accessible name is user-facing TEXT (the title), never the form's
        // identifier — identifiers must not reach the binary (G8).
        self.a11y(&root, form_role(), &title);
        self.bind_handlers_to(None, "form", &root, &form.handlers);

        // Children.
        for child in &form.children {
            let desc = self
                .reg
                .component(&child.type_name)
                .ok_or_else(|| LowerError {
                    msg: format!("unknown component type `{}`", child.type_name),
                })?;
            let role = desc.a11y_role;
            let type_op = self.cstr(&child.type_name);
            self.ui_used.insert("oe_ui_create");
            let handle = self.fresh();
            writeln!(
                self.body,
                "  {handle} = call i64 @oe_ui_create(i64 {root}, ptr {type_op})"
            )
            .unwrap();

            for (name, value) in &child.properties {
                let text = self.property_text(value)?;
                self.set_property(None, &handle, name, &text);
            }
            // The accessible name comes from user-facing text. If a component
            // has none, we emit the role only rather than falling back to the
            // instance id: ids are compile-time and must not ship (G8).
            // A future designer should prompt for an explicit accessible name
            // when a component has no text (D16).
            match child.properties.iter().find(|(n, _)| n == "text") {
                Some((_, v)) => {
                    let name = self.property_text(v)?;
                    self.a11y(&handle, role, &name);
                }
                None => self.a11y_role_only(&handle, role),
            }
            self.bind_handlers_to(None, &child.type_name, &handle, &child.handlers);
        }

        Ok(())
    }

    /// Enter the runtime event loop. A module with no window has nothing to
    /// register on its own, but a library command may have — a timer, a
    /// listening socket — so the call is unconditional and returns at once when
    /// nothing is live.
    fn loop_run(&mut self) {
        self.loop_used = true;
        let rc = self.fresh();
        writeln!(self.body, "  {rc} = call i32 @oe_loop_run()").unwrap();
        self.exit_code = Some(rc);
    }

    /// Start the event loop and tear down. Emitted after start-up code.
    fn form_run(&mut self) {
        self.ui_used.insert("oe_ui_run");
        let rc = self.fresh();
        writeln!(self.body, "  {rc} = call i32 @oe_ui_run()").unwrap();
        self.exit_code = Some(rc);
        self.ui_used.insert("oe_ui_shutdown");
        writeln!(self.body, "  call void @oe_ui_shutdown()").unwrap();
    }

    fn set_property(&mut self, lib: Option<&str>, handle: &str, name: &str, value: &str) {
        let n = self.cstr(name);
        let v = self.cstr(value);
        let f = self.setter(lib);
        writeln!(self.body, "  call i32 @{f}(i64 {handle}, ptr {n}, ptr {v})").unwrap();
    }

    /// The property setter for a component: the widget interface, or the
    /// declaring library's own.
    fn setter(&mut self, lib: Option<&str>) -> String {
        match lib {
            None => {
                self.ui_used.insert("oe_ui_set");
                "oe_ui_set".to_string()
            }
            Some(lib) => {
                self.component_libs.insert(lib.to_string());
                format!("oe_{lib}_component_set")
            }
        }
    }

    /// Record the a11y role with no accessible name (see the G8 note above).
    fn a11y_role_only(&mut self, handle: &str, role: i32) {
        self.ui_used.insert("oe_ui_set_a11y");
        writeln!(
            self.body,
            "  call i32 @oe_ui_set_a11y(i64 {handle}, i32 {role}, ptr null)"
        )
        .unwrap();
    }

    fn a11y(&mut self, handle: &str, role: i32, name: &str) {
        let n = self.cstr(name);
        self.ui_used.insert("oe_ui_set_a11y");
        writeln!(
            self.body,
            "  call i32 @oe_ui_set_a11y(i64 {handle}, i32 {role}, ptr {n})"
        )
        .unwrap();
    }

    /// Bind events to handler FUNCTION POINTERS (never names — G8).
    fn bind_handlers_to(
        &mut self,
        lib: Option<&str>,
        type_name: &str,
        handle: &str,
        handlers: &[(String, String)],
    ) {
        for (event, sub) in handlers {
            let ev = self.cstr(event);
            let f = match lib {
                None => {
                    self.ui_used.insert("oe_ui_on");
                    "oe_ui_on".to_string()
                }
                Some(lib) => {
                    self.component_libs.insert(lib.to_string());
                    format!("oe_{lib}_component_on")
                }
            };
            let target = self.handler_symbol(type_name, event, sub);
            writeln!(
                self.body,
                "  call i32 @{f}(i64 {handle}, ptr {ev}, ptr @{target})"
            )
            .unwrap();
        }
    }

    /// The function a component is handed for `event`.
    ///
    /// An event that hands nothing over binds the subroutine itself: the two
    /// signatures already agree, and a program with no parameterised event
    /// lowers to exactly what it did before events could carry anything.
    ///
    /// An event that DOES hand something over binds a thunk written with the
    /// event's signature, whatever the handler's is. The library then always
    /// calls through a pointer whose type it declared, so a handler that
    /// ignores the argument is one forwarding jump rather than a call through
    /// a mismatched pointer — which happens to work on the machines we build
    /// for and is undefined everywhere.
    fn handler_symbol(&mut self, type_name: &str, event: &str, sub: &str) -> String {
        let reg = self.reg;
        let params = reg.event_params(type_name, event);
        if params.is_empty() {
            return user_symbol(sub);
        }
        // The LLVM types alone name the thunk: they ARE its signature, so two
        // events handing the same shapes to the same subroutine want one.
        let shape = params
            .iter()
            .map(|t| llvm_ty(*t))
            .collect::<Vec<_>>()
            .join("_");
        let name = format!("oe_evt_{sub}_{shape}");
        if !self.thunks.contains_key(&name) {
            let decls = params
                .iter()
                .enumerate()
                .map(|(i, t)| format!("{} %a{i}", llvm_ty(*t)))
                .collect::<Vec<_>>()
                .join(", ");
            let takes = reg.sub(sub).is_some_and(|s| !s.params.is_empty());
            let args = if takes { decls.clone() } else { String::new() };
            self.thunks.insert(
                name.clone(),
                format!(
                    "define internal void @{name}({decls}) {{\nentry:\n  call void @{}({args})\n  ret void\n}}\n\n",
                    user_symbol(sub)
                ),
            );
        }
        name
    }

    fn fresh_label(&mut self, kind: &str) -> String {
        let l = format!("bb_{kind}_{}", self.label);
        self.label += 1;
        l
    }

    /// Evaluate a condition and branch. LLVM needs an `i1`, and bools are held
    /// as `i32`, so compare against zero at the branch.
    fn branch_on(&mut self, cond: &Expr, yes: &str, no: &str) -> Result<(), LowerError> {
        let v = self.eval(cond)?;
        if v.ty != Ty::Bool {
            return err(format!(
                "condition must be a truth value, got {}",
                v.ty.as_str()
            ));
        }
        let t = self.fresh();
        writeln!(self.body, "  {t} = icmp ne i32 {}, 0", v.operand).unwrap();
        writeln!(self.body, "  br i1 {t}, label %{yes}, label %{no}").unwrap();
        Ok(())
    }

    fn block(&mut self, stmts: &[openepl_ir::Stmt]) -> Result<(), LowerError> {
        for s in stmts {
            self.stmt(s)?;
        }
        Ok(())
    }

    /// The prologue, with every slot attributed to the subroutine's header.
    ///
    /// The slots are reserved before any statement runs, so there is no
    /// statement to attribute them to — but leaving them bare makes a row in
    /// the line table with no line, covering the addresses a breakpoint on the
    /// function's first line would land on.
    fn prologue(&self, loc: Option<usize>) -> String {
        match loc {
            None => self.allocas.join(""),
            Some(n) => self
                .allocas
                .iter()
                .map(|l| format!("{}, !dbg !{n}\n", l.trim_end_matches('\n')))
                .collect(),
        }
    }

    /// Point the instruction stream at a source position. Every instruction
    /// written after this carries it, until it is changed or cleared.
    /// Describe a named local in the debug information, and bind it to the
    /// stack slot holding it.
    ///
    /// Does nothing when the build carries no debug information, which is what
    /// makes this safe to call from every place a local is bound. A name the
    /// compiler invented is refused by `local` rather than filtered here, so
    /// there is one rule about which names are the user's.
    ///
    /// `flat` is a c-record, whose slot holds the object itself; everything
    /// else holds a value or a pointer to one. Getting that wrong renders a
    /// record's fields out of the eight bytes of a pointer.
    fn describe_local(&mut self, name: &str, slot: &str, ty: Ty, flat: bool, arg: Option<usize>) {
        let (Some(scope), Some(loc)) = (self.scope, self.body.loc) else {
            return;
        };
        // The line the statement is on, which is where the user declared it.
        let line = self.stmt_line;
        let reg = self.reg;
        let Some(debug) = self.debug.as_mut() else {
            return;
        };
        let ty = match (flat, ty) {
            (true, Ty::Record(record)) => debug.c_storage_type(record, reg),
            (_, ty) => debug.value_type(ty, reg),
        };
        let Some(var) = debug.local(scope, name, ty, line, arg) else {
            return;
        };
        let record = debug::DebugInfo::declare(slot, var, loc);
        // Written through the instruction stream like everything else. It is a
        // record rather than an instruction, and the stream knows not to give
        // one a trailing location.
        write!(self.body, "{record}").unwrap();
    }

    fn set_loc(&mut self, scope: Option<usize>, line: usize, column: usize) {
        self.body.loc = match (scope, self.debug.as_mut()) {
            (Some(sp), Some(d)) => Some(d.location(sp, line, column)),
            _ => None,
        };
    }

    /// Reserve a stack slot, emitted at the top of the function.
    fn alloca(&mut self, ty: Ty) -> String {
        let slot = format!("%v{}", self.locals);
        self.locals += 1;
        self.allocas
            .push(format!("  {slot} = alloca {}\n", llvm_ty(ty)));
        slot
    }

    /// Reserve a scratch slot for a command call's slot ABI. It is named from
    /// the temporary counter rather than the local one, because it belongs to
    /// an expression rather than to anything the program named — but it is
    /// emitted in `entry:` like every other slot, so a call inside a loop
    /// reserves its space once rather than once per turn.
    fn alloca_temp(&mut self, ty: &str) -> String {
        let slot = self.fresh();
        self.allocas.push(format!("  {slot} = alloca {ty}\n"));
        slot
    }

    /// Reserve `size` raw bytes on the stack — the flat storage of a c-record.
    /// `[size x i8]` gives one whole object with byte-addressable fields, so a
    /// field GEP is a plain byte offset and the layout is ours, not LLVM's.
    fn alloca_bytes(&mut self, size: i64) -> String {
        let slot = format!("%v{}", self.locals);
        self.locals += 1;
        self.allocas
            .push(format!("  {slot} = alloca [{size} x i8]\n"));
        slot
    }

    /// The `sizeof` of a c-record, from the one layout function every consumer
    /// shares.
    fn c_record_size(&self, rec: &str) -> Result<i64, LowerError> {
        let def = self.reg.record(rec).ok_or_else(|| LowerError {
            msg: format!("unknown record `{rec}`"),
        })?;
        let (_, size, _) = def.c_layout(self.reg).ok_or_else(|| LowerError {
            msg: format!("c-record `{rec}` has a field with no C layout"),
        })?;
        Ok(size)
    }

    /// The byte offset and *declared* field type (a `byte` stays `byte` here —
    /// the load/store needs its real width; `surface` maps it to `int` only for
    /// the resulting `Val`) of one field of a c-record.
    fn c_field(&self, rec: &str, field: &str) -> Result<(i64, Ty), LowerError> {
        let def = self.reg.record(rec).ok_or_else(|| LowerError {
            msg: format!("unknown record `{rec}`"),
        })?;
        let (pos, ty) = def.field(field).ok_or_else(|| LowerError {
            msg: format!("c-record `{rec}` has no field `{field}`"),
        })?;
        let (offsets, _, _) = def.c_layout(self.reg).ok_or_else(|| LowerError {
            msg: format!("c-record `{rec}` has a field with no C layout"),
        })?;
        Ok((offsets[pos - 1], ty))
    }

    /// A pointer to a field inside a c-record's flat storage: `base` + offset.
    /// A zero offset is `base` itself, which reads better and is the same
    /// address.
    fn c_field_ptr(&mut self, base: &str, offset: i64) -> String {
        if offset == 0 {
            return base.to_string();
        }
        let p = self.fresh();
        writeln!(
            self.body,
            "  {p} = getelementptr inbounds i8, ptr {base}, i64 {offset}"
        )
        .unwrap();
        p
    }

    fn store_global(&mut self, name: &str, v: &Val) {
        writeln!(
            self.body,
            "  store {} {}, ptr @{}",
            llvm_ty(v.ty),
            v.operand,
            global_symbol(name)
        )
        .unwrap();
    }

    /// Lower one statement, at its own place in the source.
    ///
    /// The position is set here rather than by the caller, because statements
    /// nest: a `for` lowers the statements of its body through this same
    /// function, and positioning only the outermost ones gave a whole loop
    /// body the loop header's line. There was then no row in the line table
    /// for anything inside a block — a breakpoint could not be put inside a
    /// loop, and stepping through one was impossible.
    ///
    /// A statement whose line was lost keeps the position of whatever encloses
    /// it, rather than reporting line 0, which a debugger reads as "no line
    /// here" and steps straight past.
    ///
    /// The enclosing position is restored afterwards, so a compound
    /// statement's own trailing instructions — a loop's increment, its branch
    /// back — belong to the statement that owns them rather than to the last
    /// statement of its body.
    fn stmt(&mut self, s: &openepl_ir::Stmt) -> Result<(), LowerError> {
        let enclosing = self.body.loc;
        let enclosing_line = self.stmt_line;
        if s.line > 0 {
            self.stmt_line = s.line;
        }
        if self.scope.is_some() && s.line > 0 {
            let scope = self.scope;
            self.set_loc(scope, s.line, s.span.col.max(1));
        }
        let result = self.stmt_at(s);
        self.body.loc = enclosing;
        self.stmt_line = enclosing_line;
        result
    }

    fn stmt_at(&mut self, s: &openepl_ir::Stmt) -> Result<(), LowerError> {
        use openepl_ir::StmtKind;
        match &s.kind {
            // Erased by the desugar; reaching one means the module was lowered
            // without it.
            StmtKind::LetInfer { name, .. } => {
                return err(format!("the type of `{name}` was never worked out"))
            }
            StmtKind::Let {
                name,
                ty,
                value,
                mutable: _,
            } => {
                // A c-record local is a flat struct on the stack, not a pointer
                // to a heap object: allocate its exact size, zero it, and record
                // the name as bound to that storage. There is no value to
                // evaluate — a c-record `var` is only ever the zeroed default —
                // and reading the name later hands back this address, not a
                // load (see the `Var` arm).
                if let Ty::Record(rec) = ty {
                    if self.reg.record(rec).map(|d| d.is_c).unwrap_or(false) {
                        let size = self.c_record_size(rec)?;
                        let slot = self.alloca_bytes(size);
                        // The zero-init sits at the statement, not the function
                        // top, so a `var r: RECT` inside a loop re-zeroes each
                        // pass, exactly as C's block-scoped struct would.
                        writeln!(
                            self.body,
                            "  store [{size} x i8] zeroinitializer, ptr {slot}"
                        )
                        .unwrap();
                        self.describe_local(name, &slot, *ty, true, None);
                        self.vars.insert(name.clone(), (slot, *ty));
                        return Ok(());
                    }
                }
                // An optional is two locals, not one: the value in its own
                // width, and the truth beside it saying whether the value is
                // there. Nothing else in lowering knows that — `Unwrap` reads
                // the first, `HasValue` the second, and the checker has already
                // refused every other reading of the name.
                if let Ty::Optional(elem) = ty {
                    let slot = self.alloca(elem.ty());
                    let has = self.alloca(Ty::Bool);
                    self.describe_local(name, &slot, elem.ty(), false, None);
                    self.describe_local(&has_name(name), &has, Ty::Bool, false, None);
                    self.vars.insert(name.clone(), (slot, *ty));
                    self.vars.insert(has_name(name), (has, Ty::Bool));
                    return self.store_optional(name, *elem, value);
                }
                let v = self.eval_hinted(value, Some(*ty))?;
                if v.ty != *ty {
                    return err(format!(
                        "type mismatch in `let {name}`: declared {}, expression is {}",
                        ty.as_str(),
                        v.ty.as_str()
                    ));
                }
                let slot = self.alloca(*ty);
                writeln!(
                    self.body,
                    "  store {} {}, ptr {slot}",
                    llvm_ty(*ty),
                    v.operand
                )
                .unwrap();
                self.describe_local(name, &slot, *ty, false, None);
                self.vars.insert(name.clone(), (slot, *ty));
                Ok(())
            }
            StmtKind::Assign { name, value } => {
                // Assigning to a `var v: T?` rewrites both halves, through the
                // same code the declaration used — so `v = env_get("K")` and
                // `let v: text? = env_get("K")` mean the same thing.
                if let Some((_, Ty::Optional(elem))) = self.vars.get(name).cloned() {
                    return self.store_optional(name, elem, value);
                }
                let want = self
                    .vars
                    .get(name)
                    .map(|(_, t)| *t)
                    .or_else(|| self.globals.get(name).copied());
                let v = self.eval_hinted(value, want)?;
                if let Some((slot, ty)) = self.vars.get(name).cloned() {
                    if v.ty != ty {
                        return err(format!("cannot assign {} to `{name}`", v.ty.as_str()));
                    }
                    writeln!(
                        self.body,
                        "  store {} {}, ptr {slot}",
                        llvm_ty(ty),
                        v.operand
                    )
                    .unwrap();
                    Ok(())
                } else if self.globals.contains_key(name) {
                    self.store_global(name, &v);
                    Ok(())
                } else {
                    err(format!("assignment to undefined variable `{name}`"))
                }
            }
            StmtKind::SetIndex { name, index, value } => {
                let target = self.eval(&Expr::Var(name.clone()))?;
                let i = self.eval(index)?;
                // A position is an int and a key is text; which one this is
                // depends on what is being subscripted, so the check belongs
                // with each arm below rather than ahead of them.
                if !matches!(target.ty, Ty::Dict(_)) && i.ty != Ty::Int {
                    return err(format!(
                        "an index counts with `int` values, got {}",
                        i.ty.as_str()
                    ));
                }
                match target.ty {
                    Ty::Dict(value_ty) => {
                        if i.ty != Ty::Text {
                            return err(format!(
                                "a dictionary is keyed by text, got {}",
                                i.ty.as_str()
                            ));
                        }
                        let v = self.eval_hinted(value, Some(value_ty.ty()))?;
                        if v.ty != value_ty.ty() {
                            return err(format!(
                                "`{name}` holds {} values, cannot store {}",
                                value_ty.as_str(),
                                v.ty.as_str()
                            ));
                        }
                        let raw = self.emit_arg_i64(&v);
                        self.aggr_used.insert("oe_dict_put");
                        writeln!(
                            self.body,
                            "  call void @oe_dict_put(ptr {}, ptr {}, i64 {raw})",
                            target.operand, i.operand
                        )
                        .unwrap();
                        Ok(())
                    }
                    Ty::Bytes => {
                        let v = self.eval(value)?;
                        if v.ty != Ty::Int {
                            return err(format!(
                                "a byte is written as an `int`, got {}",
                                v.ty.as_str()
                            ));
                        }
                        self.aggr_used.insert("oe_bin_set");
                        writeln!(
                            self.body,
                            "  call void @oe_bin_set(ptr {}, i32 {}, i32 {})",
                            target.operand, i.operand, v.operand
                        )
                        .unwrap();
                        Ok(())
                    }
                    Ty::Array(elem) => {
                        let v = self.eval_hinted(value, Some(elem.ty()))?;
                        if v.ty != elem.ty() {
                            return err(format!(
                                "`{name}` holds {} values, cannot store {}",
                                elem.as_str(),
                                v.ty.as_str()
                            ));
                        }
                        let raw = self.emit_arg_i64(&v);
                        self.aggr_used.insert("oe_ary_set");
                        writeln!(
                            self.body,
                            "  call void @oe_ary_set(ptr {}, i32 {}, i64 {raw})",
                            target.operand, i.operand
                        )
                        .unwrap();
                        Ok(())
                    }
                    other => err(format!(
                        "`{name}` is {} — only an array or a byte-set has elements",
                        other.as_str()
                    )),
                }
            }
            StmtKind::Call { cmd, args } => {
                self.eval_call(cmd, args)?; // any return value discarded
                Ok(())
            }
            StmtKind::CallThrough { callee, args, ret, conv: _ } => {
                self.eval_call_through(callee, args, *ret)?; // result discarded
                Ok(())
            }
            StmtKind::If { arms, otherwise } => {
                let done = self.fresh_label("endif");
                for (cond, body) in arms {
                    let then = self.fresh_label("then");
                    let next = self.fresh_label("elif");
                    self.branch_on(cond, &then, &next)?;
                    writeln!(self.body, "{then}:").unwrap();
                    self.block(body)?;
                    writeln!(self.body, "  br label %{done}").unwrap();
                    writeln!(self.body, "{next}:").unwrap();
                }
                if let Some(body) = otherwise {
                    self.block(body)?;
                }
                writeln!(self.body, "  br label %{done}").unwrap();
                writeln!(self.body, "{done}:").unwrap();
                Ok(())
            }
            // A `match` is an if/else-if chain that tests one evaluation of
            // the value. The binding is made here rather than in the parser
            // for the reason `for each`'s is: the hidden local's type is the
            // value's type, and nothing before this knows it. Past the store,
            // every arm is the ordinary `x = v` (or-joined for a `when` that
            // lists several) a hand-written chain would have.
            StmtKind::Match {
                scrutinee,
                arms,
                otherwise,
            } => {
                use openepl_ir::Stmt;
                let line = s.line;
                let v = self.eval(scrutinee)?;
                let ty = v.ty;
                let slot = self.alloca(ty);
                writeln!(
                    self.body,
                    "  store {} {}, ptr {slot}",
                    llvm_ty(ty),
                    v.operand
                )
                .unwrap();
                let name = self.fresh_hidden("match");
                self.vars.insert(name.clone(), (slot, ty));
                let subject = Expr::Var(name);
                let mut if_arms: Vec<(Expr, Vec<Stmt>)> = Vec::new();
                for (values, body) in arms {
                    let mut cond: Option<Expr> = None;
                    for val in values {
                        let test = Expr::Cmp(
                            CmpOp::Eq,
                            Box::new(subject.clone()),
                            Box::new(val.clone()),
                        );
                        cond = Some(match cond {
                            None => test,
                            Some(c) => {
                                Expr::Logical(LogicalOp::Or, Box::new(c), Box::new(test))
                            }
                        });
                    }
                    let Some(cond) = cond else {
                        return err("a `when` must list at least one value".to_string());
                    };
                    if_arms.push((cond, body.clone()));
                }
                self.stmt(&Stmt::new(
                    StmtKind::If {
                        arms: if_arms,
                        otherwise: otherwise.clone(),
                    },
                    line,
                ))?;
                Ok(())
            }
            StmtKind::While { cond, body } => {
                let head = self.fresh_label("while");
                let inner = self.fresh_label("do");
                let done = self.fresh_label("done");
                writeln!(self.body, "  br label %{head}").unwrap();
                writeln!(self.body, "{head}:").unwrap();
                self.branch_on(cond, &inner, &done)?;
                writeln!(self.body, "{inner}:").unwrap();
                // A `while` re-tests its condition, so `continue` goes to the
                // head; `break` leaves.
                self.loops.push((head.clone(), done.clone()));
                let r = self.block(body);
                self.loops.pop();
                r?;
                writeln!(self.body, "  br label %{head}").unwrap();
                writeln!(self.body, "{done}:").unwrap();
                Ok(())
            }
            StmtKind::For {
                var,
                start,
                limit,
                step,
                body,
            } => {
                // Both bounds are read once, into stack slots, before the loop
                // starts: `for i = 1 to n` where the body changes `n` still
                // runs the number of times it said it would.
                let iv = self.alloca(Ty::Int);
                let sv = self.eval(start)?;
                writeln!(self.body, "  store i32 {}, ptr {iv}", sv.operand).unwrap();
                let lv = self.alloca(Ty::Int);
                let lval = self.eval(limit)?;
                writeln!(self.body, "  store i32 {}, ptr {lv}", lval.operand).unwrap();
                self.describe_local(var, &iv, Ty::Int, false, None);
                self.vars.insert(var.clone(), (iv.clone(), Ty::Int));

                let head = self.fresh_label("for");
                let inner = self.fresh_label("fordo");
                let next = self.fresh_label("fornext");
                let done = self.fresh_label("forend");
                writeln!(self.body, "  br label %{head}").unwrap();
                writeln!(self.body, "{head}:").unwrap();
                let i = self.fresh();
                writeln!(self.body, "  {i} = load i32, ptr {iv}").unwrap();
                let l = self.fresh();
                writeln!(self.body, "  {l} = load i32, ptr {lv}").unwrap();
                let c = self.fresh();
                // The step's sign is a compile-time fact, so which way the
                // loop counts costs nothing at run time.
                let pred = if *step > 0 { "sle" } else { "sge" };
                writeln!(self.body, "  {c} = icmp {pred} i32 {i}, {l}").unwrap();
                writeln!(self.body, "  br i1 {c}, label %{inner}, label %{done}").unwrap();
                writeln!(self.body, "{inner}:").unwrap();
                self.loops.push((next.clone(), done.clone()));
                let r = self.block(body);
                self.loops.pop();
                r?;
                writeln!(self.body, "  br label %{next}").unwrap();
                writeln!(self.body, "{next}:").unwrap();
                let cur = self.fresh();
                writeln!(self.body, "  {cur} = load i32, ptr {iv}").unwrap();
                let inc = self.fresh();
                writeln!(self.body, "  {inc} = add i32 {cur}, {step}").unwrap();
                writeln!(self.body, "  store i32 {inc}, ptr {iv}").unwrap();
                writeln!(self.body, "  br label %{head}").unwrap();
                writeln!(self.body, "{done}:").unwrap();
                Ok(())
            }
            // `for each` is sugar: it lowers to the very `for` above, over a
            // hidden `int` counter from 1 to the collection's length, with the
            // element read out by index each turn. Building that `for` here and
            // handing it back to `self.stmt` keeps every loop — counter,
            // once-only bounds, break/continue — lowered in exactly one place.
            StmtKind::ForEach { elem, value, index, coll, body } => {
                use openepl_ir::Stmt;
                let line = s.line;
                // Read the collection once and pin it to a hidden slot: the loop
                // runs over one snapshot, so a body that grows it cannot make it
                // longer, matching `for`. Its type is what decides the bindings.
                let cv = self.eval(coll)?;
                let cty = cv.ty;
                let (elem_ty, value_ty) =
                    openepl_ir::foreach_elem_types(cty).ok_or_else(|| LowerError {
                        msg: format!("`for each` cannot iterate {}", cty.as_str()),
                    })?;
                let coll_slot = self.alloca(cty);
                writeln!(
                    self.body,
                    "  store {} {}, ptr {coll_slot}",
                    llvm_ty(cty),
                    cv.operand
                )
                .unwrap();
                let coll_name = self.fresh_hidden("coll");
                self.vars.insert(coll_name.clone(), (coll_slot, cty));
                let coll_ref = Expr::Var(coll_name);
                let i_name = self.fresh_hidden("i");
                let i_ref = Expr::Var(i_name.clone());
                let call = |cmd: &str, args: Vec<Expr>| Expr::Call {
                    cmd: cmd.to_string(),
                    args,
                };

                // How to count the collection and how to read one element into
                // `elem` differ by kind; a dictionary also pulls its keys out
                // once, before the loop, and looks each value up by key.
                let mut loop_body: Vec<Stmt> = Vec::new();
                let count_expr = match cty {
                    Ty::Array(_) => {
                        loop_body.push(Stmt::new(
                            StmtKind::Let {
                                name: elem.clone(),
                                ty: elem_ty,
                                value: Expr::Index {
                                    base: Box::new(coll_ref.clone()),
                                    index: Box::new(i_ref.clone()),
                                },
                                mutable: false,
                            },
                            line,
                        ));
                        call("count", vec![coll_ref.clone()])
                    }
                    // Each byte is an `int`, 0..255, the way `bytes_at` reads one.
                    Ty::Bytes => {
                        loop_body.push(Stmt::new(
                            StmtKind::Let {
                                name: elem.clone(),
                                ty: elem_ty,
                                value: call("bytes_at", vec![coll_ref.clone(), i_ref.clone()]),
                                mutable: false,
                            },
                            line,
                        ));
                        call("bytes_count", vec![coll_ref.clone()])
                    }
                    // Each character is a one-character `text`; `length` and
                    // `substr` both count characters, so the slice never splits
                    // one and the loop runs once per character.
                    Ty::Text => {
                        loop_body.push(Stmt::new(
                            StmtKind::Let {
                                name: elem.clone(),
                                ty: elem_ty,
                                value: call(
                                    "substr",
                                    vec![coll_ref.clone(), i_ref.clone(), Expr::IntLit(1)],
                                ),
                                mutable: false,
                            },
                            line,
                        ));
                        call("length", vec![coll_ref.clone()])
                    }
                    Ty::Dict(_) => {
                        let keys_name = self.fresh_hidden("keys");
                        self.stmt(&Stmt::new(
                            StmtKind::Let {
                                name: keys_name.clone(),
                                ty: Ty::Array(Elem::Text),
                                value: call("dict_keys", vec![coll_ref.clone()]),
                                mutable: false,
                            },
                            line,
                        ))?;
                        let keys_ref = Expr::Var(keys_name);
                        // The element binding is the key.
                        loop_body.push(Stmt::new(
                            StmtKind::Let {
                                name: elem.clone(),
                                ty: elem_ty,
                                value: Expr::Index {
                                    base: Box::new(keys_ref.clone()),
                                    index: Box::new(i_ref.clone()),
                                },
                                mutable: false,
                            },
                            line,
                        ));
                        if let Some(v) = value {
                            loop_body.push(Stmt::new(
                                StmtKind::Let {
                                    name: v.clone(),
                                    ty: value_ty.unwrap_or(elem_ty),
                                    value: call(
                                        "dict_get",
                                        vec![coll_ref.clone(), Expr::Var(elem.clone())],
                                    ),
                                    mutable: false,
                                },
                                line,
                            ));
                        }
                        call("count", vec![keys_ref])
                    }
                    other => return err(format!("`for each` cannot iterate {}", other.as_str())),
                };
                // `at IDX` — the 1-based position, which is the counter itself.
                if let Some(idx) = index {
                    loop_body.push(Stmt::new(
                        StmtKind::Let {
                            name: idx.clone(),
                            ty: Ty::Int,
                            value: i_ref,
                            mutable: false,
                        },
                        line,
                    ));
                }
                // The author's body runs after the bindings, each turn.
                loop_body.extend(body.iter().cloned());
                self.stmt(&Stmt::new(
                    StmtKind::For {
                        var: i_name,
                        start: Expr::IntLit(1),
                        limit: count_expr,
                        step: 1,
                        body: loop_body,
                    },
                    line,
                ))?;
                Ok(())
            }
            StmtKind::Break | StmtKind::Continue => {
                let is_break = matches!(s.kind, StmtKind::Break);
                let Some((next, done)) = self.loops.last().cloned() else {
                    return err(format!(
                        "`{}` outside a loop",
                        if is_break { "break" } else { "continue" }
                    ));
                };
                let target = if is_break { done } else { next };
                writeln!(self.body, "  br label %{target}").unwrap();
                // The jump terminates this block; whatever follows it in the
                // source is unreachable but still needs somewhere to live.
                let dead = self.fresh_label("postjump");
                writeln!(self.body, "{dead}:").unwrap();
                Ok(())
            }
            // Copied to the block's exits by `expand_defer` before lowering
            // begins; reaching one means a body was lowered without that pass.
            StmtKind::Defer(_) => err("a `defer` was never copied to the block's exits"),
            // Expanded by the desugar into an `if`; reaching one means the
            // module was lowered without that pass.
            StmtKind::IfSome { bind, .. } => {
                err(format!("`if some ... as {bind}` was never expanded into an `if`"))
            }
            StmtKind::Return { value } => {
                match value {
                    None => writeln!(self.body, "  ret void").unwrap(),
                    Some(e) => {
                        let v = self.eval_hinted(e, self.ret_ty)?;
                        writeln!(self.body, "  ret {} {}", llvm_ty(v.ty), v.operand).unwrap();
                    }
                }
                // `ret` terminates the block. Anything the author wrote after it
                // is unreachable, but LLVM still needs somewhere to put it — so
                // open a fresh block rather than emitting into a closed one.
                let dead = self.fresh_label("postret");
                writeln!(self.body, "{dead}:").unwrap();
                Ok(())
            }
            // `r.pt.x = v`, `r.rgb[3] = v` — a store through a path into a
            // c-record's flat storage. The address comes from the same walker a
            // read uses, so a write can never land at a different offset than
            // the read of the same words would.
            StmtKind::SetPlace { place, value } => {
                let (p, fty) = self.c_place_ptr(place)?;
                self.c_store(&p, fty, value, "that field")
            }
            StmtKind::SetProperty {
                component,
                property,
                value,
            } => {
                if let Some(Ty::Record(rec)) = self.var_ty(component) {
                    // A c-record's field is a store into flat storage; the plain
                    // record's stays the heap `oe_rec_set` below.
                    if self.reg.record(rec).map(|d| d.is_c).unwrap_or(false) {
                        let base = self.eval(&Expr::Var(component.clone()))?;
                        return self.emit_c_field_write(rec, &base.operand, property, value);
                    }
                    let def = self.reg.record(rec).cloned().ok_or_else(|| LowerError {
                        msg: format!("unknown record `{rec}`"),
                    })?;
                    let (pos, want) = def.field(property).ok_or_else(|| LowerError {
                        msg: format!("record `{rec}` has no field `{property}`"),
                    })?;
                    let base = self.eval(&Expr::Var(component.clone()))?;
                    let v = self.eval_hinted(value, Some(want))?;
                    if v.ty != want {
                        return err(format!(
                            "`{component}.{property}` is {}, cannot store {}",
                            want.as_str(),
                            v.ty.as_str()
                        ));
                    }
                    let raw = self.emit_arg_i64(&v);
                    self.aggr_used.insert("oe_rec_set");
                    writeln!(
                        self.body,
                        "  call void @oe_rec_set(ptr {}, i32 {pos}, i64 {raw})",
                        base.operand
                    )
                    .unwrap();
                    return Ok(());
                }
                let handle = self.handle_of(component)?;
                let v = self.eval(value)?;
                // The D10 boundary takes textual values, so convert first.
                let text = self.value_as_text(&v)?;
                let n = self.cstr(property);
                let f = self.setter(self.owner(component).as_deref());
                writeln!(self.body, "  call i32 @{f}(i64 {handle}, ptr {n}, ptr {text})").unwrap();
                Ok(())
            }
        }
    }

    /// The compile-time handle constant for a component id.
    fn handle_of(&self, id: &str) -> Result<u64, LowerError> {
        self.handles.get(id).copied().ok_or_else(|| LowerError {
            msg: format!("unknown component `{id}`"),
        })
    }

    /// Render a value as a `ptr` to text, converting numbers via the runtime.
    fn value_as_text(&mut self, v: &Val) -> Result<String, LowerError> {
        match v.ty {
            Ty::Text => Ok(v.operand.clone()),
            // A property is textual at the D10 boundary, and `true`/`false` is
            // what both a descriptor's default value and the property parser on
            // the other side already spell — so `t.enabled = false` reaches the
            // component as the same words the source wrote.
            Ty::Bool => {
                let yes = self.cstr("true");
                let no = self.cstr("false");
                let c = self.fresh();
                writeln!(self.body, "  {c} = icmp ne i32 {}, 0", v.operand).unwrap();
                let t = self.fresh();
                writeln!(self.body, "  {t} = select i1 {c}, ptr {yes}, ptr {no}").unwrap();
                Ok(t)
            }
            other if !other.is_numeric() => err(format!(
                "cannot use {} where text is expected",
                other.as_str()
            )),
            _ => {
                let sym = match v.ty {
                    Ty::Int => "oe_int_to_text",
                    Ty::Int64 => "oe_int64_to_text",
                    _ => "oe_double_to_text",
                };
                let converted = self.call_symbol_1(sym, v)?;
                Ok(converted)
            }
        }
    }

    /// Compare two text values by content via the runtime, yielding i32 0/1.
    fn call_text_eq(&mut self, a: &Val, b: &Val) -> Result<String, LowerError> {
        self.call_symbol_2("oe_text_eq", a, b, Ty::Bool)
    }

    /// Abort with a runtime error message. `oe_notify(OE_NRS_RUNTIME_ERR, ...)`
    /// prints and exits, so the block ends `unreachable`.
    fn runtime_error(&mut self, message: &str) {
        let m = self.cstr(message);
        self.needs_notify = true;
        writeln!(self.body, "  call ptr @oe_notify(i32 5, ptr {m}, ptr null)").unwrap();
        writeln!(self.body, "  unreachable").unwrap();
    }

    /// Emit the checks integer `/` and `%` need before the hardware sees them:
    /// a zero divisor, and the one overflowing case (the most negative value
    /// divided by -1). A literal divisor is checked here at compile time, so
    /// `x / 2` still lowers to a bare `sdiv`.
    fn guard_divisor(&mut self, op: BinOp, lv: &Val, rv: &Val) -> Result<(), LowerError> {
        let what = if op == BinOp::Div { "division" } else { "remainder" };
        let ity = llvm_ty(lv.ty);
        let literal = rv.operand.parse::<i64>().ok();
        if literal == Some(0) {
            return err(format!("{what} by zero"));
        }
        if literal.is_none() {
            let bad = self.fresh();
            writeln!(self.body, "  {bad} = icmp eq {ity} {}, 0", rv.operand).unwrap();
            let trap = self.fresh_label("divzero");
            let ok = self.fresh_label("divok");
            writeln!(self.body, "  br i1 {bad}, label %{trap}, label %{ok}").unwrap();
            writeln!(self.body, "{trap}:").unwrap();
            self.runtime_error(&format!("{what} by zero"));
            writeln!(self.body, "{ok}:").unwrap();
        }
        // `MIN / -1` has no representable answer and faults just as hard as a
        // zero divisor. Only reachable when the divisor can be -1.
        if literal.is_none() || literal == Some(-1) {
            let min = if lv.ty == Ty::Int64 {
                i64::MIN.to_string()
            } else {
                i32::MIN.to_string()
            };
            let is_min = self.fresh();
            writeln!(self.body, "  {is_min} = icmp eq {ity} {}, {min}", lv.operand).unwrap();
            let bad = if literal == Some(-1) {
                is_min
            } else {
                let neg1 = self.fresh();
                writeln!(self.body, "  {neg1} = icmp eq {ity} {}, -1", rv.operand).unwrap();
                let both = self.fresh();
                writeln!(self.body, "  {both} = and i1 {is_min}, {neg1}").unwrap();
                both
            };
            let trap = self.fresh_label("divover");
            let ok = self.fresh_label("divok");
            writeln!(self.body, "  br i1 {bad}, label %{trap}, label %{ok}").unwrap();
            writeln!(self.body, "{trap}:").unwrap();
            self.runtime_error(&format!(
                "{what} overflowed: the most negative {} divided by -1",
                lv.ty.as_str()
            ));
            writeln!(self.body, "{ok}:").unwrap();
        }
        Ok(())
    }

    /// Call a two-argument slot-ABI runtime command and return its result
    /// operand. `call_text_eq` and text `+` are both this call with different
    /// symbols.
    fn call_symbol_2(
        &mut self,
        symbol: &str,
        a: &Val,
        b: &Val,
        ret: Ty,
    ) -> Result<String, LowerError> {
        let argv = self.alloca_temp("[2 x %Slot]");
        for (i, v) in [a, b].iter().enumerate() {
            let raw = self.emit_arg_i64(v);
            let slot = self.fresh();
            writeln!(
                self.body,
                "  {slot} = getelementptr [2 x %Slot], ptr {argv}, i64 0, i64 {i}"
            )
            .unwrap();
            let tagp = self.fresh();
            writeln!(
                self.body,
                "  {tagp} = getelementptr %Slot, ptr {slot}, i32 0, i32 0"
            )
            .unwrap();
            writeln!(self.body, "  store i32 {}, ptr {tagp}", v.ty.sdt_tag()).unwrap();
            let valp = self.fresh();
            writeln!(
                self.body,
                "  {valp} = getelementptr %Slot, ptr {slot}, i32 0, i32 2"
            )
            .unwrap();
            writeln!(self.body, "  store i64 {raw}, ptr {valp}").unwrap();
        }
        let base = self.fresh();
        writeln!(
            self.body,
            "  {base} = getelementptr [2 x %Slot], ptr {argv}, i64 0, i64 0"
        )
        .unwrap();
        let ret_slot = self.alloca_temp("%Slot");
        self.used.insert(symbol.to_string());
        writeln!(
            self.body,
            "  call void @{symbol}(ptr {ret_slot}, i32 2, ptr {base})"
        )
        .unwrap();
        let valp = self.fresh();
        writeln!(
            self.body,
            "  {valp} = getelementptr %Slot, ptr {ret_slot}, i32 0, i32 2"
        )
        .unwrap();
        let raw = self.fresh();
        writeln!(self.body, "  {raw} = load i64, ptr {valp}").unwrap();
        Ok(self.emit_ret_from_i64(ret, &raw))
    }

    /// Call a one-argument slot-ABI runtime command and return its text result.
    fn call_symbol_1(&mut self, symbol: &str, arg: &Val) -> Result<String, LowerError> {
        let raw = self.emit_arg_i64(arg);
        let argv = self.alloca_temp("[1 x %Slot]");
        let slot = self.fresh();
        writeln!(
            self.body,
            "  {slot} = getelementptr [1 x %Slot], ptr {argv}, i64 0, i64 0"
        )
        .unwrap();
        let tagp = self.fresh();
        writeln!(
            self.body,
            "  {tagp} = getelementptr %Slot, ptr {slot}, i32 0, i32 0"
        )
        .unwrap();
        writeln!(self.body, "  store i32 {}, ptr {tagp}", arg.ty.sdt_tag()).unwrap();
        let valp = self.fresh();
        writeln!(
            self.body,
            "  {valp} = getelementptr %Slot, ptr {slot}, i32 0, i32 2"
        )
        .unwrap();
        writeln!(self.body, "  store i64 {raw}, ptr {valp}").unwrap();
        let ret = self.alloca_temp("%Slot");
        self.used.insert(symbol.to_string());
        writeln!(
            self.body,
            "  call void @{symbol}(ptr {ret}, i32 1, ptr {slot})"
        )
        .unwrap();
        let rvalp = self.fresh();
        writeln!(
            self.body,
            "  {rvalp} = getelementptr %Slot, ptr {ret}, i32 0, i32 2"
        )
        .unwrap();
        let rraw = self.fresh();
        writeln!(self.body, "  {rraw} = load i64, ptr {rvalp}").unwrap();
        Ok(self.emit_ret_from_i64(Ty::Text, &rraw))
    }

    fn eval(&mut self, e: &Expr) -> Result<Val, LowerError> {
        self.eval_hinted(e, None)
    }

    /// As `eval`, told the type the destination declares.
    ///
    /// Only an empty `[]` needs it — there is no element to take a type from,
    /// so the destination's declaration is the only thing that knows. The
    /// validator has already agreed the hint fits.
    fn eval_hinted(&mut self, e: &Expr, hint: Option<Ty>) -> Result<Val, LowerError> {
        // A bare name that is a constant (and is not a local or a module
        // variable) folds to its literal here, before any other rule — so the
        // constant is lowered exactly as the literal it stands for, including
        // the `int64` widening below. The checker agreed to the same fold.
        if let Expr::Var(name) = e {
            if !self.vars.contains_key(name) && !self.globals.contains_key(name) {
                if let Some(c) = self.reg.const_(name) {
                    let value = c.value.clone();
                    return self.eval_hinted(&value, hint);
                }
            }
        }
        if let Expr::ArrayLit(items) = e {
            let elem = match (items.first(), hint) {
                (Some(first), _) => {
                    let v = self.eval(first)?;
                    Elem::from_ty(v.ty).ok_or_else(|| LowerError {
                        msg: format!("a list cannot hold {} values", v.ty.as_str()),
                    })?
                }
                (None, Some(Ty::Array(elem))) => elem,
                (None, _) => {
                    return err("`[]` here does not say what it holds");
                }
            };
            return self.eval_array_lit(elem, items);
        }
        if let Expr::DictLit(pairs) = e {
            let value = match (pairs.first(), hint) {
                (Some((_, first)), _) => {
                    let v = self.eval(first)?;
                    Elem::from_ty(v.ty).ok_or_else(|| LowerError {
                        msg: format!("a dictionary cannot hold {} values", v.ty.as_str()),
                    })?
                }
                (None, Some(Ty::Dict(value))) => value,
                (None, _) => return err("`{}` here does not say what it holds"),
            };
            return self.eval_dict_lit(value, pairs);
        }
        // The literal-to-`int64` widening the checker already agreed to (see
        // `type_of_expr_hinted`): emit the constant as an i64 so an `int64`
        // parameter or `let` receives it with no `int_to_int64` at the source.
        if let Expr::IntLit(v) = e {
            if hint == Some(Ty::Int64) {
                return Ok(Val {
                    ty: Ty::Int64,
                    operand: v.to_string(),
                });
            }
        }
        // A hex/binary pattern takes the width its destination declares, and
        // gains zeros rather than a sign doing it — `0x8000_0000` is the `int`
        // sign bit on its own and `int64` 2147483648 here. The checker agreed
        // to the same reading (`bits_value_int64`).
        if let Expr::BitsLit(v) = e {
            if hint == Some(Ty::Int64) {
                return Ok(Val {
                    ty: Ty::Int64,
                    operand: openepl_ir::sema::bits_value_int64(*v).to_string(),
                });
            }
        }
        // Bitwise operators are intercepted here, not in `eval_inner`, because
        // their operands want the surrounding hint: `var s: int64 = A bor B`
        // must read both patterns as 64-bit ones, exactly as the checker did.
        if let Expr::Bit(op, l, r) = e {
            return self.eval_bit(*op, l, r, hint);
        }
        if let Expr::BitNot(inner) = e {
            return self.eval_bitnot(inner, hint);
        }
        // Both conditional forms pass the hint to their arms, for the same
        // reason a bitwise operator does: `let n: int64 = if c then 0 else 1`
        // must emit i64 constants in both, and only the destination knows.
        if let Expr::IfElse { cond, then, els } = e {
            return self.eval_ifelse(cond, then, els, hint);
        }
        if let Expr::Otherwise { value, fallback } = e {
            return self.eval_otherwise(value, fallback, hint);
        }
        self.eval_inner(e)
    }

    /// `{"a": 1}` — one empty dictionary, then one store per pair, for the
    /// reason an array literal is built the same way: a value may be any
    /// expression, so there is nothing constant to initialise from.
    fn eval_dict_lit(
        &mut self,
        value: Elem,
        pairs: &[(Expr, Expr)],
    ) -> Result<Val, LowerError> {
        self.aggr_used.insert("oe_dict_new");
        let d = self.fresh();
        writeln!(
            self.body,
            "  {d} = call ptr @oe_dict_new(i32 {})",
            value.ty().sdt_tag()
        )
        .unwrap();
        for (key, val) in pairs {
            let k = self.eval(key)?;
            if k.ty != Ty::Text {
                return err(format!(
                    "a dictionary is keyed by text, got {}",
                    k.ty.as_str()
                ));
            }
            let v = self.eval_hinted(val, Some(value.ty()))?;
            if v.ty != value.ty() {
                return err(format!(
                    "every value in a dictionary has one type: expected {}, got {}",
                    value.as_str(),
                    v.ty.as_str()
                ));
            }
            let raw = self.emit_arg_i64(&v);
            self.aggr_used.insert("oe_dict_put");
            writeln!(
                self.body,
                "  call void @oe_dict_put(ptr {d}, ptr {}, i64 {raw})",
                k.operand
            )
            .unwrap();
        }
        Ok(Val {
            ty: Ty::Dict(value),
            operand: d,
        })
    }

    /// `point(x: 1, y: 2)` — one allocation of the declared width, then one
    /// store per field. A field is written by POSITION: the declaration order
    /// is the layout, so no field name reaches the shipped binary.
    fn eval_record_lit(
        &mut self,
        name: &str,
        fields: &[(String, Expr)],
    ) -> Result<Val, LowerError> {
        let def = self.reg.record(name).cloned().ok_or_else(|| LowerError {
            msg: format!("unknown record `{name}`"),
        })?;
        self.aggr_used.insert("oe_rec_new");
        let r = self.fresh();
        writeln!(
            self.body,
            "  {r} = call ptr @oe_rec_new(i32 {})",
            def.fields.len()
        )
        .unwrap();
        for (fname, value) in fields {
            let (pos, want) = def.field(fname).ok_or_else(|| LowerError {
                msg: format!("record `{name}` has no field `{fname}`"),
            })?;
            let v = self.eval_hinted(value, Some(want))?;
            if v.ty != want {
                return err(format!(
                    "record `{name}` field `{fname}` is {}, got {}",
                    want.as_str(),
                    v.ty.as_str()
                ));
            }
            let raw = self.emit_arg_i64(&v);
            self.aggr_used.insert("oe_rec_set");
            writeln!(
                self.body,
                "  call void @oe_rec_set(ptr {r}, i32 {pos}, i64 {raw})"
            )
            .unwrap();
        }
        Ok(Val {
            ty: Ty::Record(openepl_ir::intern(name)),
            operand: r,
        })
    }

    /// Read one field of an already-lowered record. `base` is the record's
    /// value: a heap pointer for a plain record, the flat storage's address for
    /// a c-record (a c-record `Var` yields its own address, not a load).
    fn emit_field_read(&mut self, rec: &str, base: &Val, field: &str) -> Result<Val, LowerError> {
        if self.reg.record(rec).map(|d| d.is_c).unwrap_or(false) {
            return self.emit_c_field_read(rec, &base.operand, field);
        }
        let def = self.reg.record(rec).cloned().ok_or_else(|| LowerError {
            msg: format!("unknown record `{rec}`"),
        })?;
        let (pos, ty) = def.field(field).ok_or_else(|| LowerError {
            msg: format!("record `{rec}` has no field `{field}`"),
        })?;
        self.aggr_used.insert("oe_rec_get");
        let raw = self.fresh();
        writeln!(
            self.body,
            "  {raw} = call i64 @oe_rec_get(ptr {}, i32 {pos})",
            base.operand
        )
        .unwrap();
        let res = self.emit_ret_from_i64(ty, &raw);
        Ok(Val { ty, operand: res })
    }

    /// Read one field of a c-record from its flat storage: a GEP to the field's
    /// byte offset, then a load of the field's real width.
    fn emit_c_field_read(
        &mut self,
        rec: &str,
        base: &str,
        field: &str,
    ) -> Result<Val, LowerError> {
        let (offset, fty) = self.c_field(rec, field)?;
        let fp = self.c_field_ptr(base, offset);
        Ok(self.c_load(&fp, fty))
    }

    /// Load one C-layout value at `fp`. The result is the field's *surface*
    /// type — a `byte` and an `int16` come back as an `int`, a `float` as a
    /// `double` — which is the type the language reads and writes it as.
    ///
    /// A nested c-record and an inline array are the exception: there is no
    /// value to load, so the result IS the address, typed as the field. That
    /// is the same rule a c-record `Var` already follows, which is what lets
    /// `r.pt.x` and `r.rgb[3]` chain through this one function.
    fn c_load(&mut self, fp: &str, fty: Ty) -> Val {
        match fty {
            Ty::Record(_) | Ty::CArray(_) => Val {
                ty: fty,
                operand: fp.to_string(),
            },
            // A byte is one `i8` widened to the `int` it reads as, unsigned so
            // 200 is 200 and not -56.
            Ty::Byte => {
                let raw = self.fresh();
                writeln!(self.body, "  {raw} = load i8, ptr {fp}").unwrap();
                let t = self.fresh();
                writeln!(self.body, "  {t} = zext i8 {raw} to i32").unwrap();
                Val { ty: Ty::Int, operand: t }
            }
            // Widened unsigned for the same reason: the field this exists for is
            // a Win32 `WORD`, and 0xFFFF there means 65535, not -1.
            Ty::Int16 => {
                let raw = self.fresh();
                writeln!(self.body, "  {raw} = load i16, ptr {fp}").unwrap();
                let t = self.fresh();
                writeln!(self.body, "  {t} = zext i16 {raw} to i32").unwrap();
                Val { ty: Ty::Int, operand: t }
            }
            // The struct holds a 4-byte float; the language has one floating
            // type, so widen on the way out.
            Ty::Float => {
                let raw = self.fresh();
                writeln!(self.body, "  {raw} = load float, ptr {fp}").unwrap();
                let t = self.fresh();
                writeln!(self.body, "  {t} = fpext float {raw} to double").unwrap();
                Val { ty: Ty::Double, operand: t }
            }
            // C truth is any non-zero int, and a C API fills a `BOOL` field with
            // whatever its flag arithmetic produced (`7`, `0x100`), not always
            // `1`. Normalise to 0/1 so `f.on = true` and `not f.on` are right —
            // the same normalisation a returned `dll` bool gets.
            Ty::Bool => {
                let raw = self.fresh();
                writeln!(self.body, "  {raw} = load i32, ptr {fp}").unwrap();
                let nz = self.fresh();
                writeln!(self.body, "  {nz} = icmp ne i32 {raw}, 0").unwrap();
                let t = self.fresh();
                writeln!(self.body, "  {t} = zext i1 {nz} to i32").unwrap();
                Val { ty: Ty::Bool, operand: t }
            }
            // A `char*` a C API wrote into the struct is borrowed and outlives
            // nothing in particular, so copy it into a managed text exactly as a
            // `dll` that returns text does — `oe_dll_text` also turns a NULL
            // field into the empty text.
            Ty::Text => {
                self.needs_dll_text = true;
                let raw = self.fresh();
                writeln!(self.body, "  {raw} = load ptr, ptr {fp}").unwrap();
                let t = self.fresh();
                writeln!(self.body, "  {t} = call ptr @oe_dll_text(ptr {raw})").unwrap();
                Val { ty: Ty::Text, operand: t }
            }
            _ => {
                let t = self.fresh();
                writeln!(self.body, "  {t} = load {}, ptr {fp}", llvm_ty(fty)).unwrap();
                Val { ty: fty.surface(), operand: t }
            }
        }
    }

    /// Write one field of a c-record into its flat storage: evaluate the value
    /// against the field's surface type, then store it at the field's offset in
    /// the field's real width.
    fn emit_c_field_write(
        &mut self,
        rec: &str,
        base: &str,
        field: &str,
        value: &openepl_ir::Expr,
    ) -> Result<(), LowerError> {
        let (offset, fty) = self.c_field(rec, field)?;
        let fp = self.c_field_ptr(base, offset);
        self.c_store(&fp, fty, value, &format!("c-record `{rec}` field `{field}`"))
    }

    /// Store one C-layout value at `fp`, narrowing to the field's real width.
    /// `what` names the destination for the type error.
    fn c_store(
        &mut self,
        fp: &str,
        fty: Ty,
        value: &openepl_ir::Expr,
        what: &str,
    ) -> Result<(), LowerError> {
        if matches!(fty, Ty::Record(_) | Ty::CArray(_)) {
            return err(format!(
                "{what} is a whole nested struct or inline array — set its parts, or copy \
                 bytes through `address of`"
            ));
        }
        let want = fty.surface();
        let v = self.eval_hinted(value, Some(want))?;
        if v.ty != want {
            return err(format!(
                "{what} is {}, cannot store {}",
                want.as_str(),
                v.ty.as_str()
            ));
        }
        match fty {
            // The `int` value narrows to the one byte the field holds; the top
            // 24 bits are the author's to keep in range, as with `ptr_write_byte`.
            Ty::Byte => {
                let b = self.fresh();
                writeln!(self.body, "  {b} = trunc i32 {} to i8", v.operand).unwrap();
                writeln!(self.body, "  store i8 {b}, ptr {fp}").unwrap();
            }
            // The low 16 bits, for the same reason.
            Ty::Int16 => {
                let b = self.fresh();
                writeln!(self.body, "  {b} = trunc i32 {} to i16", v.operand).unwrap();
                writeln!(self.body, "  store i16 {b}, ptr {fp}").unwrap();
            }
            // A `double` narrows to the 4-byte float the struct holds — the
            // rounding C's own `float x = d;` does.
            Ty::Float => {
                let f = self.fresh();
                writeln!(self.body, "  {f} = fptrunc double {} to float", v.operand).unwrap();
                writeln!(self.body, "  store float {f}, ptr {fp}").unwrap();
            }
            // The stored `char*` is borrowed: it is valid while the text that
            // backs it is, exactly like `ptr_of_text`. A runtime-produced empty
            // text is NULL, so store a pointer to `""` instead, matching how a
            // text argument crosses to a `dll`.
            Ty::Text => {
                let empty = self.cstr("");
                let isnull = self.fresh();
                writeln!(self.body, "  {isnull} = icmp eq ptr {}, null", v.operand).unwrap();
                let sel = self.fresh();
                writeln!(
                    self.body,
                    "  {sel} = select i1 {isnull}, ptr {empty}, ptr {}",
                    v.operand
                )
                .unwrap();
                writeln!(self.body, "  store ptr {sel}, ptr {fp}").unwrap();
            }
            _ => {
                writeln!(self.body, "  store {} {}, ptr {fp}", llvm_ty(fty), v.operand).unwrap();
            }
        }
        Ok(())
    }

    /// The address of a *place* inside a c-record's flat storage, and the place's
    /// declared C field type: `r`, `r.pt`, `r.pt.x`, `r.rgb[3]`, however deep.
    ///
    /// One walker for every path in the language — the read of `r.pt.x`, the
    /// write to it, `address of r.rgb`, and an element read — so a chained GEP
    /// is computed in exactly one place and the three cannot disagree about an
    /// offset.
    fn c_place_ptr(&mut self, place: &Expr) -> Result<(String, Ty), LowerError> {
        match place {
            Expr::Var(name) => match self.vars.get(name).cloned() {
                Some((slot, ty @ Ty::Record(rec)))
                    if self.reg.record(rec).map(|d| d.is_c).unwrap_or(false) =>
                {
                    Ok((slot, ty))
                }
                _ => err(format!(
                    "`{name}` is not a c-record local — a path reaches into a c-record's own \
                     storage and nothing else"
                )),
            },
            // `r.pt` on its own arrives as a property read; past the first step
            // the parser builds `Field`. Both are one step into a record.
            Expr::GetProperty { component, property } => {
                let base = Expr::Var(component.clone());
                self.c_place_step(&base, property)
            }
            Expr::Field { base, name } => self.c_place_step(base, name),
            Expr::Index { base, index } => {
                let (bp, bty) = self.c_place_ptr(base)?;
                let Ty::CArray(a) = bty else {
                    return err(format!(
                        "`{}` is not an inline array — only an array field is indexed inside a \
                         c-record",
                        bty.as_str()
                    ));
                };
                let (esize, _) = openepl_ir::c_field_size_align(a.elem, self.reg)
                    .ok_or_else(|| LowerError {
                        msg: format!("`{}` has no C layout", a.elem.as_str()),
                    })?;
                // Positions count from 1, so element `k` starts `(k-1)*esize`
                // bytes in. A literal index folds to that constant; anything
                // else is a runtime GEP with no bounds check, exactly as every
                // other `ptr` operation is.
                let p = match self.const_index(index) {
                    Some(k) => self.c_field_ptr(&bp, (k - 1) * esize),
                    None => {
                        let iv = self.eval(index)?;
                        if iv.ty != Ty::Int {
                            return err(format!(
                                "an index counts with `int` values, got {}",
                                iv.ty.as_str()
                            ));
                        }
                        let zero = self.fresh();
                        writeln!(self.body, "  {zero} = sub i32 {}, 1", iv.operand).unwrap();
                        let wide = self.fresh();
                        writeln!(self.body, "  {wide} = sext i32 {zero} to i64").unwrap();
                        let off = self.fresh();
                        writeln!(self.body, "  {off} = mul i64 {wide}, {esize}").unwrap();
                        let g = self.fresh();
                        writeln!(
                            self.body,
                            "  {g} = getelementptr inbounds i8, ptr {bp}, i64 {off}"
                        )
                        .unwrap();
                        g
                    }
                };
                Ok((p, a.elem))
            }
            other => err(format!(
                "{other:?} is not a place inside a c-record"
            )),
        }
    }

    /// The value of an index the compiler can see: a literal, or a `const` that
    /// stands for one. Folding the constant here is what keeps `r.rgb[LIMIT]` a
    /// plain constant offset rather than an address computed at run time.
    fn const_index(&self, index: &Expr) -> Option<i64> {
        match index {
            Expr::IntLit(k) => Some(*k),
            Expr::Var(n) if !self.vars.contains_key(n) => {
                match self.reg.const_(n).map(|c| &c.value) {
                    Some(Expr::IntLit(k)) => Some(*k),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Whether a place expression bottoms out in a c-record local — the test
    /// that decides whether `xs[i]` is a struct's inline array or a runtime
    /// array. It answers from the variable table alone, so it never emits.
    fn rooted_in_c_record(&self, place: &Expr) -> bool {
        match place {
            Expr::Var(name) | Expr::GetProperty { component: name, .. } => {
                matches!(self.vars.get(name), Some((_, Ty::Record(rec)))
                    if self.reg.record(rec).map(|d| d.is_c).unwrap_or(false))
            }
            Expr::Field { base, .. } | Expr::Index { base, .. } => self.rooted_in_c_record(base),
            _ => false,
        }
    }

    /// One `.field` step of a place walk.
    fn c_place_step(&mut self, base: &Expr, field: &str) -> Result<(String, Ty), LowerError> {
        let (bp, bty) = self.c_place_ptr(base)?;
        let Ty::Record(rec) = bty else {
            return err(format!(
                "`.{field}` reads a field, and {} has none",
                bty.as_str()
            ));
        };
        let (offset, fty) = self.c_field(rec, field)?;
        Ok((self.c_field_ptr(&bp, offset), fty))
    }

    /// The declared type of a variable, local first then module-level — the
    /// order the checker resolves a name in.
    fn var_ty(&self, name: &str) -> Option<Ty> {
        self.vars
            .get(name)
            .map(|(_, t)| *t)
            .or_else(|| self.globals.get(name).copied())
    }

    /// `[a, b, c]` — one allocation of the right length, then one store per
    /// element. Building it through `oe_ary_set` rather than a constant
    /// initializer is what lets an element be any expression.
    fn eval_array_lit(&mut self, elem: Elem, items: &[Expr]) -> Result<Val, LowerError> {
        self.aggr_used.insert("oe_ary_new");
        let arr = self.fresh();
        writeln!(
            self.body,
            "  {arr} = call ptr @oe_ary_new(i32 {}, i32 {})",
            elem.ty().sdt_tag(),
            items.len()
        )
        .unwrap();
        for (i, item) in items.iter().enumerate() {
            let v = self.eval(item)?;
            if v.ty != elem.ty() {
                return err(format!(
                    "every element of a list has one type: expected {}, got {}",
                    elem.as_str(),
                    v.ty.as_str()
                ));
            }
            let raw = self.emit_arg_i64(&v);
            self.aggr_used.insert("oe_ary_set");
            // `enumerate` counts from 0 and the store counts from 1. Getting
            // this wrong writes every element one place low and drops the
            // first, which looks like a broken literal rather than an
            // off-by-one.
            let pos = i + 1;
            writeln!(
                self.body,
                "  call void @oe_ary_set(ptr {arr}, i32 {pos}, i64 {raw})"
            )
            .unwrap();
        }
        Ok(Val {
            ty: Ty::Array(elem),
            operand: arr,
        })
    }

    /// Read one element. The bounds check lives in the runtime helper, which
    /// reports through the error slot — reading past the end must never reach
    /// whatever is next in memory.
    fn eval_index(&mut self, base: &Expr, index: &Expr) -> Result<Val, LowerError> {
        let b = self.eval(base)?;
        let i = self.eval(index)?;
        // A dictionary is subscripted by key. The miss is reported by the
        // runtime through the error slot, so a lookup that finds nothing is
        // still a value the caller can hold.
        if let Ty::Dict(value) = b.ty {
            if i.ty != Ty::Text {
                return err(format!(
                    "a dictionary is keyed by text, got {}",
                    i.ty.as_str()
                ));
            }
            self.aggr_used.insert("oe_dict_at");
            let raw = self.fresh();
            writeln!(
                self.body,
                "  {raw} = call i64 @oe_dict_at(ptr {}, ptr {})",
                b.operand, i.operand
            )
            .unwrap();
            let res = self.emit_ret_from_i64(value.ty(), &raw);
            return Ok(Val {
                ty: value.ty(),
                operand: res,
            });
        }
        if i.ty != Ty::Int {
            return err(format!(
                "an index counts with `int` values, got {}",
                i.ty.as_str()
            ));
        }
        match b.ty {
            Ty::Bytes => {
                self.aggr_used.insert("oe_bin_at");
                let t = self.fresh();
                writeln!(
                    self.body,
                    "  {t} = call i32 @oe_bin_at(ptr {}, i32 {})",
                    b.operand, i.operand
                )
                .unwrap();
                Ok(Val {
                    ty: Ty::Int,
                    operand: t,
                })
            }
            Ty::Array(elem) => {
                self.aggr_used.insert("oe_ary_get");
                let raw = self.fresh();
                writeln!(
                    self.body,
                    "  {raw} = call i64 @oe_ary_get(ptr {}, i32 {})",
                    b.operand, i.operand
                )
                .unwrap();
                let res = self.emit_ret_from_i64(elem.ty(), &raw);
                Ok(Val {
                    ty: elem.ty(),
                    operand: res,
                })
            }
            other => err(format!("{} is not something you can index", other.as_str())),
        }
    }

    /// `a band b`, `x shl 8`. The checker has already agreed the types; this
    /// repeats enough of the rule to pick the right LLVM instruction and to
    /// refuse rather than emit nonsense if it is ever reached without one.
    fn eval_bit(
        &mut self,
        op: BitOp,
        l: &Expr,
        r: &Expr,
        hint: Option<Ty>,
    ) -> Result<Val, LowerError> {
        let want = match hint {
            Some(Ty::Int) | Some(Ty::Int64) => hint,
            _ => None,
        };
        let lv = self.eval_hinted(l, want)?;
        if !matches!(lv.ty, Ty::Int | Ty::Int64) {
            return err(format!(
                "`{}` works on int and int64 values; its left side is {}",
                op.word(),
                lv.ty.as_str()
            ));
        }
        if op.is_shift() {
            return self.eval_shift(op, lv, r);
        }
        // The narrow side of a mixed pair is widened by re-reading it as a
        // 64-bit pattern. Only a literal (or a constant, which is one) changes
        // type under a hint — the checker proved that — so the value already
        // emitted for it is a bare constant and emitting it again costs no
        // instruction.
        let rv = self.eval_hinted(r, if lv.ty == Ty::Int64 { Some(Ty::Int64) } else { want })?;
        let (lv, rv) = if lv.ty == rv.ty {
            (lv, rv)
        } else if rv.ty == Ty::Int64 {
            (self.eval_hinted(l, Some(Ty::Int64))?, rv)
        } else {
            return err(format!(
                "`{}` needs both sides to be the same width: {} vs {}",
                op.word(),
                lv.ty.as_str(),
                rv.ty.as_str()
            ));
        };
        let opcode = match op {
            BitOp::And => "and",
            BitOp::Or => "or",
            BitOp::Xor => "xor",
            _ => unreachable!("shifts left above"),
        };
        let t = self.fresh();
        writeln!(
            self.body,
            "  {t} = {opcode} {} {}, {}",
            llvm_ty(lv.ty),
            lv.operand,
            rv.operand
        )
        .unwrap();
        Ok(Val { ty: lv.ty, operand: t })
    }

    /// `x shl n`, `x shr n`, `x ushr n`.
    ///
    /// The count is a count, not a second value: it is brought to the value's
    /// own width and the result is the value's type. A count at or beyond that
    /// width has no answer the hardware agrees on — LLVM calls the result
    /// poison, which is a silent wrong answer later — so a count written as a
    /// literal is refused at build time, and one computed at run time is taken
    /// modulo the width. That is one `and` instruction, no branch, and what
    /// the machine would have done anyway.
    fn eval_shift(&mut self, op: BitOp, lv: Val, r: &Expr) -> Result<Val, LowerError> {
        let rv = self.eval(r)?;
        if !matches!(rv.ty, Ty::Int | Ty::Int64) {
            return err(format!(
                "the count `{}` shifts by must be int or int64, not {}",
                op.word(),
                rv.ty.as_str()
            ));
        }
        let width: i64 = if lv.ty == Ty::Int64 { 64 } else { 32 };
        let count = if let Ok(k) = rv.operand.parse::<i64>() {
            if !(0..width).contains(&k) {
                return err(format!(
                    "`{}` by {k}: an {} can be shifted by 0 to {}",
                    op.word(),
                    lv.ty.as_str(),
                    width - 1
                ));
            }
            k.to_string()
        } else {
            let c = if rv.ty == lv.ty {
                rv.operand.clone()
            } else {
                let t = self.fresh();
                if lv.ty == Ty::Int64 {
                    writeln!(self.body, "  {t} = sext i32 {} to i64", rv.operand).unwrap();
                } else {
                    writeln!(self.body, "  {t} = trunc i64 {} to i32", rv.operand).unwrap();
                }
                t
            };
            let m = self.fresh();
            writeln!(self.body, "  {m} = and {} {c}, {}", llvm_ty(lv.ty), width - 1).unwrap();
            m
        };
        let opcode = match op {
            BitOp::Shl => "shl",
            BitOp::Shr => "ashr",
            BitOp::Ushr => "lshr",
            _ => unreachable!("only shifts reach here"),
        };
        let t = self.fresh();
        writeln!(
            self.body,
            "  {t} = {opcode} {} {}, {count}",
            llvm_ty(lv.ty),
            lv.operand
        )
        .unwrap();
        Ok(Val { ty: lv.ty, operand: t })
    }

    /// `bnot x` — every bit flipped, which is `xor` with all ones.
    fn eval_bitnot(&mut self, e: &Expr, hint: Option<Ty>) -> Result<Val, LowerError> {
        let want = match hint {
            Some(Ty::Int) | Some(Ty::Int64) => hint,
            _ => None,
        };
        let v = self.eval_hinted(e, want)?;
        if !matches!(v.ty, Ty::Int | Ty::Int64) {
            return err(format!(
                "`bnot` flips the bits of an int or an int64, got {}",
                v.ty.as_str()
            ));
        }
        let t = self.fresh();
        writeln!(self.body, "  {t} = xor {} {}, -1", llvm_ty(v.ty), v.operand).unwrap();
        Ok(Val { ty: v.ty, operand: t })
    }

    /// Evaluate `e` once, store it in a fresh synthetic local, and return that
    /// local's name. The value can then be read back through `Expr::Var(name)`
    /// as many times as the desugar needs while running `e` only once. The name
    /// begins with `$`, which no source identifier can contain, so a temp never
    /// shadows a program's own variable.
    /// The local an optional's two halves are kept under. Only a name reaches
    /// here: `HasValue` and `Unwrap` are made by the desugar out of a binding,
    /// never out of a computed value, because the two halves have to be read
    /// from the same storage the one test wrote.
    fn optional_name(&self, e: &Expr) -> Result<String, LowerError> {
        match e {
            Expr::Var(n) if matches!(self.vars.get(n), Some((_, Ty::Optional(_)))) => Ok(n.clone()),
            _ => err("a value that may be absent is read through the name it was bound to"),
        }
    }

    /// Write both halves of the optional local `name`: the value, and the truth
    /// beside it.
    ///
    /// What sets the truth depends on what the initializer is, and there are
    /// only three kinds:
    ///
    ///  * `none` — nothing is there, and the value half is the zero of its type
    ///    so that a stray read is a zero rather than whatever was on the stack;
    ///  * a **call** — the value is there when the call did not fail. The error
    ///    slot is cleared first, so what is read afterwards is *this* call's
    ///    verdict and not one an earlier failure left behind — a command that
    ///    cannot fail never touches the slot, and without the clear it would
    ///    inherit the last failure in the program;
    ///  * anything else — a value written down is there.
    ///
    /// Copying one optional into another copies both halves, since the source
    /// already carries its own answer.
    fn store_optional(&mut self, name: &str, elem: Elem, value: &Expr) -> Result<(), LowerError> {
        let (slot, _) = self.vars[name].clone();
        let (has_slot, _) = self.vars[&has_name(name)].clone();
        let store = |lo: &mut Self, operand: &str| {
            writeln!(
                lo.body,
                "  store {} {operand}, ptr {slot}",
                llvm_ty(elem.ty())
            )
            .unwrap();
        };
        let set_has = |lo: &mut Self, operand: &str| {
            writeln!(lo.body, "  store i32 {operand}, ptr {has_slot}").unwrap();
        };
        match value {
            Expr::NoneLit => {
                let zero = zero_operand(elem.ty());
                store(self, &zero);
                set_has(self, "0");
            }
            // One optional into another: both halves travel together, because
            // the answer to "is it there" belongs to the value and not to the
            // moment of copying.
            Expr::Var(n) if matches!(self.vars.get(n), Some((_, Ty::Optional(_)))) => {
                let v = self.eval(&Expr::Unwrap(Box::new(value.clone())))?;
                store(self, &v.operand);
                let h = self.eval(&Expr::HasValue(Box::new(value.clone())))?;
                set_has(self, &h.operand);
            }
            Expr::Call { .. } | Expr::CallThrough { .. } => {
                self.needs_error_clear = true;
                writeln!(self.body, "  call void @oe_error_clear()").unwrap();
                let v = self.eval_hinted(value, Some(elem.ty()))?;
                if v.ty != elem.ty() {
                    return err(format!(
                        "`{name}` holds {}, and the call yields {}",
                        elem.as_str(),
                        v.ty.as_str()
                    ));
                }
                store(self, &v.operand);
                let code = self.eval(&Expr::Call {
                    cmd: "last_error_code".to_string(),
                    args: Vec::new(),
                })?;
                let t = self.fresh();
                writeln!(self.body, "  {t} = icmp eq i32 {}, 0", code.operand).unwrap();
                let w = self.fresh();
                writeln!(self.body, "  {w} = zext i1 {t} to i32").unwrap();
                set_has(self, &w);
            }
            _ => {
                let v = self.eval_hinted(value, Some(elem.ty()))?;
                if v.ty != elem.ty() {
                    return err(format!(
                        "`{name}` holds {}, and the value is {}",
                        elem.as_str(),
                        v.ty.as_str()
                    ));
                }
                store(self, &v.operand);
                set_has(self, "1");
            }
        }
        Ok(())
    }

    /// `[EXPR for each x in xs where COND]` — build the array a loop would have
    /// built, and hand it back as the value.
    ///
    /// The three statements written here are the three a program writes by hand:
    /// an empty list, a `for each` over the same collection, and an `append`
    /// inside it — under an `if` when a `where` was written. They go through
    /// `self.stmt`, so the loop is the *same* loop, break/continue and all, and
    /// the append is the same `append` (which copies, so the hidden name is
    /// reassigned each turn, exactly as a hand-written accumulator would be).
    fn eval_comprehension(&mut self, e: &Expr) -> Result<Val, LowerError> {
        use openepl_ir::{Stmt, StmtKind};
        let Expr::Comprehension {
            body,
            elem,
            value,
            index,
            coll,
            cond,
            holds,
        } = e
        else {
            return err("not a list comprehension");
        };
        let Some(holds) = holds else {
            return err("a list built by a loop was never told what it holds");
        };
        let acc = self.fresh_hidden("list");
        let line = 0;
        self.stmt(&Stmt::new(
            StmtKind::Let {
                name: acc.clone(),
                ty: Ty::Array(*holds),
                value: Expr::ArrayLit(Vec::new()),
                mutable: true,
            },
            line,
        ))?;
        let push = Stmt::new(
            StmtKind::Assign {
                name: acc.clone(),
                value: Expr::Call {
                    cmd: "append".to_string(),
                    args: vec![Expr::Var(acc.clone()), (**body).clone()],
                },
            },
            line,
        );
        let inner = match cond {
            None => vec![push],
            Some(c) => vec![Stmt::new(
                StmtKind::If {
                    arms: vec![((**c).clone(), vec![push])],
                    otherwise: None,
                },
                line,
            )],
        };
        self.stmt(&Stmt::new(
            StmtKind::ForEach {
                elem: elem.clone(),
                value: value.clone(),
                index: index.clone(),
                coll: (**coll).clone(),
                body: inner,
            },
            line,
        ))?;
        self.eval(&Expr::Var(acc))
    }

    fn bind_temp(&mut self, e: &Expr) -> Result<String, LowerError> {
        let v = self.eval(e)?;
        let slot = self.alloca(v.ty);
        writeln!(
            self.body,
            "  store {} {}, ptr {slot}",
            llvm_ty(v.ty),
            v.operand
        )
        .unwrap();
        let name = format!("$t{}", self.tmp);
        self.tmp += 1;
        self.vars.insert(name.clone(), (slot, v.ty));
        Ok(name)
    }

    /// `lo <op1> mid <op2> hi` — bind `lo` then `mid` to temps, in that order,
    /// so evaluation runs left to right and the middle runs exactly once, then
    /// lower the plain conjunction `lo <op1> mid and mid <op2> hi`. Reusing
    /// `Cmp` and `Logical` gets text comparison and the `and`'s short circuit
    /// (which keeps `hi` lazy) for free.
    fn eval_chain(
        &mut self,
        lo: &Expr,
        lo_op: CmpOp,
        mid: &Expr,
        hi_op: CmpOp,
        hi: &Expr,
    ) -> Result<Val, LowerError> {
        let lo_t = self.bind_temp(lo)?;
        let mid_t = self.bind_temp(mid)?;
        let desugared = Expr::Logical(
            LogicalOp::And,
            Box::new(Expr::Cmp(
                lo_op,
                Box::new(Expr::Var(lo_t)),
                Box::new(Expr::Var(mid_t.clone())),
            )),
            Box::new(Expr::Cmp(
                hi_op,
                Box::new(Expr::Var(mid_t)),
                Box::new(hi.clone()),
            )),
        );
        self.eval(&desugared)
    }

    /// `xs[a..b]` — bind the base and both bounds to temps, in the order they
    /// were written, then lower to the command the base's type answers to:
    /// `substr` for text, `bytes_slice` for a byte-set, `slice` for an array.
    ///
    /// Temps rather than the expressions themselves because each appears twice
    /// in the rewrite — the base in the length, `from` in the count — and
    /// `s[f()..g()]` must call each of them once. A missing bound is filled in
    /// from the temp: `from` absent is 1, `to` absent is the base's own length,
    /// which is a read of a value already computed and not a second evaluation.
    ///
    /// The commands take a **count**, and the slice is inclusive at both ends,
    /// so the count is `to - from + 1`. Clamping is the command's: a bound
    /// outside the collection is trimmed, never an error.
    fn eval_slice(
        &mut self,
        base: &Expr,
        from: Option<&Expr>,
        to: Option<&Expr>,
    ) -> Result<Val, LowerError> {
        let base_t = self.bind_temp(base)?;
        let bty = self.var_ty(&base_t).expect("temp just bound");
        let (cmd, len_cmd) = match bty {
            Ty::Text => ("substr", "length"),
            Ty::Bytes => ("bytes_slice", "bytes_count"),
            Ty::Array(_) => ("slice", "count"),
            other => {
                return err(format!(
                    "{} cannot be sliced — `a..b` takes a run of text, of bytes, or of a list",
                    other.as_str()
                ))
            }
        };
        // The three commands clamp a start below 1 up to 1 but leave the count
        // alone, so a raw `substr(s, 0, 4)` reads four characters from the
        // first. `a..b` counts POSITIONS, and position 0 is not one of them, so
        // the start is raised to 1 here — before the count is measured from it —
        // and `s[0..3]` is the three characters at 1, 2 and 3.
        let start = match from {
            // A literal start already at or past 1 needs no guard, which is
            // almost every slice ever written.
            Some(Expr::IntLit(v)) if *v >= 1 => Expr::IntLit(*v),
            Some(e) => Expr::Call {
                cmd: "max_int".to_string(),
                args: vec![Expr::Var(self.bind_temp(e)?), Expr::IntLit(1)],
            },
            None => Expr::IntLit(1),
        };
        let end = match to {
            Some(e) => Expr::Var(self.bind_temp(e)?),
            None => Expr::Call {
                cmd: len_cmd.to_string(),
                args: vec![Expr::Var(base_t.clone())],
            },
        };
        // `to - from + 1`: both ends are included, so a one-position slice
        // `s[3..3]` is a count of one. `start` may be a `max_int` call, so it is
        // bound to a temp when it is one — it appears twice below.
        let start = match start {
            Expr::IntLit(v) => Expr::IntLit(v),
            other => Expr::Var(self.bind_temp(&other)?),
        };
        let count = Expr::Bin(
            BinOp::Add,
            Box::new(Expr::Bin(
                BinOp::Sub,
                Box::new(end),
                Box::new(start.clone()),
            )),
            Box::new(Expr::IntLit(1)),
        );
        self.eval(&Expr::Call {
            cmd: cmd.to_string(),
            args: vec![Expr::Var(base_t), start, count],
        })
    }

    /// `e in xs` / `k in d` / `sub in text` — bind the haystack once (its type
    /// picks the command and its value feeds it, and it may have side effects),
    /// then lower to the command that answers membership: `index_of(xs, e) <> 0`
    /// for an array, `dict_has(d, k)` for a dictionary, `find(text, sub) <> 0`
    /// for a substring. `not in` wraps the result in `not`. The needle appears
    /// once in the desugar, so it stays inline and runs once too.
    fn eval_in(
        &mut self,
        needle: &Expr,
        haystack: &Expr,
        negated: bool,
    ) -> Result<Val, LowerError> {
        let hay = self.bind_temp(haystack)?;
        let hty = self.var_ty(&hay).expect("temp just bound");
        let hvar = Expr::Var(hay);
        let desugared = match hty {
            Ty::Array(_) => Expr::Cmp(
                CmpOp::Ne,
                Box::new(Expr::Call {
                    cmd: "index_of".to_string(),
                    args: vec![hvar, needle.clone()],
                }),
                Box::new(Expr::IntLit(0)),
            ),
            Ty::Dict(_) => Expr::Call {
                cmd: "dict_has".to_string(),
                args: vec![hvar, needle.clone()],
            },
            Ty::Text => Expr::Cmp(
                CmpOp::Ne,
                Box::new(Expr::Call {
                    cmd: "find".to_string(),
                    args: vec![hvar, needle.clone()],
                }),
                Box::new(Expr::IntLit(0)),
            ),
            other => {
                return err(format!(
                    "`in` cannot test membership in {}",
                    other.as_str()
                ))
            }
        };
        let desugared = if negated {
            Expr::Not(Box::new(desugared))
        } else {
            desugared
        };
        self.eval(&desugared)
    }

    /// `if COND then A else B` as a value — one slot, two branches, exactly the
    /// shape `and`/`or` already lower to.
    ///
    /// The `then` arm is evaluated first so its type is known before the slot
    /// is reserved; allocas are emitted at the top of the function, so
    /// reserving one part-way through the body is only a bookkeeping order.
    /// Only one arm runs, so a call in the arm not taken never happens.
    fn eval_ifelse(
        &mut self,
        cond: &Expr,
        then: &Expr,
        els: &Expr,
        hint: Option<Ty>,
    ) -> Result<Val, LowerError> {
        let then_l = self.fresh_label("then_v");
        let else_l = self.fresh_label("else_v");
        let done = self.fresh_label("ifval");
        self.branch_on(cond, &then_l, &else_l)?;

        writeln!(self.body, "{then_l}:").unwrap();
        let tv = self.eval_hinted(then, hint)?;
        let slot = self.alloca(tv.ty);
        writeln!(self.body, "  store {} {}, ptr {slot}", llvm_ty(tv.ty), tv.operand).unwrap();
        writeln!(self.body, "  br label %{done}").unwrap();

        writeln!(self.body, "{else_l}:").unwrap();
        let ev = self.eval_hinted(els, hint.or(Some(tv.ty)))?;
        if ev.ty != tv.ty {
            return err(format!(
                "both sides of `if` must have one type; `then` is {} and `else` is {}",
                tv.ty.as_str(),
                ev.ty.as_str()
            ));
        }
        writeln!(self.body, "  store {} {}, ptr {slot}", llvm_ty(ev.ty), ev.operand).unwrap();
        writeln!(self.body, "  br label %{done}").unwrap();

        writeln!(self.body, "{done}:").unwrap();
        let t = self.fresh();
        writeln!(self.body, "  {t} = load {}, ptr {slot}", llvm_ty(tv.ty)).unwrap();
        Ok(Val { ty: tv.ty, operand: t })
    }

    /// `EXPR otherwise FALLBACK` — run `EXPR` into a temporary, then lower
    /// *literally* the desugar the language documents:
    /// `if last_error_code() <> 0 then FALLBACK else <that temporary>`.
    ///
    /// Reusing the conditional means there is one branch semantics in the
    /// backend, not two, and the fallback stays lazy for free: it is an arm, so
    /// it runs only when the call failed.
    fn eval_otherwise(
        &mut self,
        value: &Expr,
        fallback: &Expr,
        hint: Option<Ty>,
    ) -> Result<Val, LowerError> {
        // A value that may be absent carries its own answer: `otherwise` reads
        // the truth beside it rather than the error slot, so a fallback taken
        // long after the call that failed is still taken for the right reason.
        if let Expr::Var(n) = value {
            if let Some((_, Ty::Optional(elem))) = self.vars.get(n).cloned() {
                let desugared = Expr::IfElse {
                    cond: Box::new(Expr::HasValue(Box::new(value.clone()))),
                    then: Box::new(Expr::Unwrap(Box::new(value.clone()))),
                    els: Box::new(fallback.clone()),
                };
                return self.eval_hinted(&desugared, hint.or(Some(elem.ty())));
            }
        }
        let t = self.bind_temp(value)?;
        // The fallback becomes the `then` arm, so it is the one an untyped
        // literal sits in: `f() otherwise []` in a position that declares
        // nothing has only the value's own type to take. The checker agreed to
        // exactly this fallback (`hint.or(Some(value_ty))`).
        let vty = self.var_ty(&t);
        let desugared = Expr::IfElse {
            cond: Box::new(Expr::Cmp(
                CmpOp::Ne,
                Box::new(Expr::Call {
                    cmd: "last_error_code".to_string(),
                    args: Vec::new(),
                }),
                Box::new(Expr::IntLit(0)),
            )),
            then: Box::new(fallback.clone()),
            els: Box::new(Expr::Var(t)),
        };
        self.eval_hinted(&desugared, hint.or(vty))
    }

    fn eval_inner(&mut self, e: &Expr) -> Result<Val, LowerError> {
        match e {
            // Intercepted by `eval_hinted`, which is the only caller.
            Expr::Bit(..) | Expr::BitNot(_) => {
                err("a bitwise operator is lowered by `eval_hinted`")
            }
            Expr::IfElse { .. } | Expr::Otherwise { .. } => {
                err("a conditional value is lowered by `eval_hinted`")
            }
            Expr::Comprehension { .. } => self.eval_comprehension(e),
            // `none` never reaches here: it is only ever the initializer of an
            // optional, and `store_optional` reads it before evaluating.
            Expr::NoneLit => err("`none` here does not say what it is the absence of"),
            Expr::HasValue(x) => {
                let name = self.optional_name(x)?;
                let (slot, _) = self.vars[&has_name(&name)].clone();
                let t = self.fresh();
                writeln!(self.body, "  {t} = load i32, ptr {slot}").unwrap();
                Ok(Val {
                    ty: Ty::Bool,
                    operand: t,
                })
            }
            Expr::Unwrap(x) => {
                let name = self.optional_name(x)?;
                let (slot, ty) = self.vars[&name].clone();
                let Ty::Optional(elem) = ty else {
                    return err(format!("`{name}` holds no value to unwrap"));
                };
                let t = self.fresh();
                writeln!(self.body, "  {t} = load {}, ptr {slot}", llvm_ty(elem.ty())).unwrap();
                Ok(Val {
                    ty: elem.ty(),
                    operand: t,
                })
            }
            Expr::ArrayLit(_) => err("`[]` here does not say what it holds"),
            Expr::DictLit(_) => err("`{}` here does not say what it holds"),
            Expr::RecordLit { name, fields } => self.eval_record_lit(name, fields),
            // Erased by the desugar; reaching one means the module was lowered
            // without it.
            Expr::Labeled { name, .. } => err(format!(
                "the named argument `{name}:` was never matched to a parameter"
            )),
            Expr::RecordUpdate { name, .. } => {
                err(format!("`{name}{{...}}` was never expanded into its fields"))
            }
            Expr::Field { base, name } => {
                let b = self.eval(base)?;
                match b.ty {
                    Ty::Record(rec) => self.emit_field_read(rec, &b, name),
                    other => err(format!(
                        "`.{name}` reads a field, and {} has none",
                        other.as_str()
                    )),
                }
            }
            // One element of a c-record's inline array is an address inside the
            // struct, not a runtime array lookup — route it through the one
            // place walker so a read, a write and `address of` all compute the
            // same offset.
            Expr::Index { base, index } if self.rooted_in_c_record(base) => {
                let (p, ty) = self.c_place_ptr(&Expr::Index {
                    base: base.clone(),
                    index: index.clone(),
                })?;
                Ok(self.c_load(&p, ty))
            }
            Expr::Index { base, index } => self.eval_index(base, index),
            Expr::Slice { base, from, to } => {
                self.eval_slice(base, from.as_deref(), to.as_deref())
            }
            // `1 <= x <= 12` and `e in xs` are sugar that only becomes a
            // concrete command once the operand types are known — which they are
            // here. Both lower to expressions built entirely from nodes that
            // already exist, so the work is one more `eval`.
            Expr::Chain { lo, lo_op, mid, hi_op, hi } => {
                self.eval_chain(lo, *lo_op, mid, *hi_op, hi)
            }
            Expr::In { needle, haystack, negated } => {
                self.eval_in(needle, haystack, *negated)
            }
            // One interpolation hole. The value is rendered to text by the very
            // routine that renders a value assigned to a component property, so
            // a bool is `true`/`false` and a number goes through its
            // `*_to_text` — the checker has already refused a type with no text
            // form, naming the hole.
            Expr::ToText { value, .. } => {
                let v = self.eval(value)?;
                let operand = self.value_as_text(&v)?;
                Ok(Val { ty: Ty::Text, operand })
            }
            Expr::IntLit(v) => {
                if let Ok(v32) = i32::try_from(*v) {
                    Ok(Val {
                        ty: Ty::Int,
                        operand: v32.to_string(),
                    })
                } else {
                    Ok(Val {
                        ty: Ty::Int64,
                        operand: v.to_string(),
                    })
                }
            }
            Expr::BitsLit(v) => {
                let ty = openepl_ir::sema::bits_bare_type(*v);
                Ok(Val {
                    ty,
                    operand: openepl_ir::sema::bits_value(*v).to_string(),
                })
            }
            Expr::DoubleLit(v) => Ok(Val {
                ty: Ty::Double,
                operand: format!("0x{:016X}", v.to_bits()),
            }),
            Expr::TextLit(s) => {
                let id = self.strings.len();
                self.strings.push(s.clone());
                let bytes = s.len() + 1;
                Ok(Val {
                    ty: Ty::Text,
                    operand: format!(
                        "getelementptr inbounds ([{bytes} x i8], ptr @.str{id}, i64 0, i64 0)"
                    ),
                })
            }
            Expr::Var(name) => {
                if let Some((slot, ty)) = self.vars.get(name).cloned() {
                    // A c-record local IS its storage: the name evaluates to the
                    // address of the flat struct, not a value loaded out of a
                    // slot. That address is what a field GEP walks, what `dll`
                    // passes for a struct pointer, and what `address of` hands
                    // to C — so there is nothing to load, and loading would read
                    // the first bytes of the struct as if they were a pointer.
                    if let Ty::Record(rec) = ty {
                        if self.reg.record(rec).map(|d| d.is_c).unwrap_or(false) {
                            return Ok(Val { ty, operand: slot });
                        }
                    }
                    // A value that may be absent is not a value yet. The
                    // checker refuses every reading of one that has not been
                    // unwrapped, so this is unreachable from a program that
                    // type-checked — and if it ever were reached, a silent load
                    // would hand back a value nothing had stored.
                    if let Ty::Optional(_) = ty {
                        return err(format!(
                            "`{name}` may hold no value — read it with `{name} otherwise ...` or \
                             `if some {name} as ...`"
                        ));
                    }
                    let t = self.fresh();
                    writeln!(self.body, "  {t} = load {}, ptr {slot}", llvm_ty(ty)).unwrap();
                    Ok(Val { ty, operand: t })
                } else if let Some(ty) = self.globals.get(name).copied() {
                    let t = self.fresh();
                    writeln!(
                        self.body,
                        "  {t} = load {}, ptr @{}",
                        llvm_ty(ty),
                        global_symbol(name)
                    )
                    .unwrap();
                    Ok(Val { ty, operand: t })
                } else {
                    Err(LowerError {
                        msg: format!("use of undefined variable `{name}`"),
                    })
                }
            }
            Expr::Bin(op, l, r) => {
                let lv = self.eval(l)?;
                let rv = self.eval(r)?;
                // `+` on text is concatenation, not arithmetic: it forwards to
                // the same `concat` command an author could call by name.
                if lv.ty == Ty::Text && rv.ty == Ty::Text && *op == BinOp::Add {
                    let t = self.call_symbol_2("oe_concat", &lv, &rv, Ty::Text)?;
                    return Ok(Val {
                        ty: Ty::Text,
                        operand: t,
                    });
                }
                // `text * count` repeats the text: it forwards to the `repeat`
                // command, the same operation an author could call by name. The
                // checker has already required the text on the left and an `int`
                // count on the right.
                if lv.ty == Ty::Text && rv.ty == Ty::Int && *op == BinOp::Mul {
                    let t = self.call_symbol_2("oe_repeat", &lv, &rv, Ty::Text)?;
                    return Ok(Val {
                        ty: Ty::Text,
                        operand: t,
                    });
                }
                if lv.ty != rv.ty || !lv.ty.is_numeric() {
                    return err("arithmetic requires matching numeric operands");
                }
                // Integer division and remainder trap on the two inputs the
                // hardware cannot answer. Without this the process dies of
                // SIGFPE with nothing said; the runtime has an error channel,
                // so use it.
                if matches!(op, BinOp::Div | BinOp::Rem) && lv.ty != Ty::Double {
                    self.guard_divisor(*op, &lv, &rv)?;
                }
                let opcode = match (op, lv.ty) {
                    (BinOp::Add, Ty::Double) => "fadd",
                    (BinOp::Sub, Ty::Double) => "fsub",
                    (BinOp::Mul, Ty::Double) => "fmul",
                    (BinOp::Div, Ty::Double) => "fdiv",
                    (BinOp::Rem, Ty::Double) => "frem",
                    (BinOp::Add, _) => "add",
                    (BinOp::Sub, _) => "sub",
                    (BinOp::Mul, _) => "mul",
                    (BinOp::Div, _) => "sdiv",
                    (BinOp::Rem, _) => "srem",
                };
                let t = self.fresh();
                writeln!(
                    self.body,
                    "  {t} = {opcode} {} {}, {}",
                    llvm_ty(lv.ty),
                    lv.operand,
                    rv.operand
                )
                .unwrap();
                Ok(Val {
                    ty: lv.ty,
                    operand: t,
                })
            }
            Expr::Call { cmd, args } => {
                let v = self.eval_call(cmd, args)?;
                v.ok_or_else(|| LowerError {
                    msg: format!("command `{cmd}` returns nothing and cannot be used as a value"),
                })
            }
            Expr::BoolLit(b) => Ok(Val {
                ty: Ty::Bool,
                operand: (*b as i32).to_string(),
            }),
            Expr::Neg(e) => {
                let v = self.eval(e)?;
                if !v.ty.is_numeric() {
                    return err(format!("`-` negates numbers, got {}", v.ty.as_str()));
                }
                let t = self.fresh();
                if v.ty == Ty::Double {
                    writeln!(self.body, "  {t} = fneg double {}", v.operand).unwrap();
                } else {
                    writeln!(self.body, "  {t} = sub {} 0, {}", llvm_ty(v.ty), v.operand).unwrap();
                }
                Ok(Val {
                    ty: v.ty,
                    operand: t,
                })
            }
            Expr::Not(e) => {
                let v = self.eval(e)?;
                let t = self.fresh();
                writeln!(self.body, "  {t} = xor i32 {}, 1", v.operand).unwrap();
                Ok(Val {
                    ty: Ty::Bool,
                    operand: t,
                })
            }
            // `address of NAME` — the subroutine's own function symbol, as a
            // `ptr`. Under opaque pointers a function is already a `ptr`-typed
            // constant, so there is no bitcast to emit (the C mental model of
            // "cast the function pointer to void*" is a no-op here); the bare
            // `@oe_user_<name>` constant is a valid operand in every position a
            // `Val` is spliced into. The checker has proven `NAME` is a sub with
            // a C-representable signature, so the symbol both exists (all subs
            // are emitted) and is callable across the C ABI. The reference is a
            // relocation from this — reachable — function to the sub's own
            // section, which is exactly what keeps `--gc-sections` from dropping
            // a sub whose address is taken but which nothing calls directly, the
            // same way an event handler's thunk keeps its handler alive.
            Expr::AddressOf(name) => {
                // `address of r.pt` / `address of r.rgb` — the address of that
                // field inside the struct's own storage (for an inline array,
                // of its first element, which is where C's own `&r.rgb` points).
                if let Some((root, rest)) = name.split_once('.') {
                    let mut place = Expr::Var(root.to_string());
                    for step in rest.split('.') {
                        place = Expr::Field {
                            base: Box::new(place),
                            name: step.to_string(),
                        };
                    }
                    let (p, _) = self.c_place_ptr(&place)?;
                    return Ok(Val { ty: Ty::Ptr, operand: p });
                }
                // `address of r` for a c-record local is that local's own
                // address — the pointer a C API is handed. A c-record `Var`
                // already evaluates to its address, so this is the same operand,
                // typed `ptr`. Otherwise it is a subroutine's function symbol
                // (see the note below), which the checker has proven callable.
                if let Some((slot, Ty::Record(rec))) = self.vars.get(name).cloned() {
                    if self.reg.record(rec).map(|d| d.is_c).unwrap_or(false) {
                        return Ok(Val { ty: Ty::Ptr, operand: slot });
                    }
                }
                // The sub's own convention marker (`Sub::conv`), if any, is not
                // encoded on this function pointer for the same reason a `dll`'s
                // is not: one C convention per 64-bit target. A 32-bit backend
                // would read it from the `Sub` AST node when emitting the sub's
                // definition, not here.
                Ok(Val {
                    ty: Ty::Ptr,
                    operand: format!("@{}", user_symbol(name)),
                })
            }
            // `call through EXPR(args...): T` in a value position. The checker
            // has proven the callee is a `ptr` and that the site declares a
            // return type, so the result is always there to hand back.
            Expr::CallThrough { callee, args, ret, conv: _ } => {
                match self.eval_call_through(callee, args, *ret)? {
                    Some(v) => Ok(v),
                    None => err(
                        "a `call through` with no return type has no value".to_string(),
                    ),
                }
            }
            // `size of TYPE` is a compile-time constant: a c-record's flat
            // `sizeof`, or a scalar's C width. The checker has already agreed the
            // type has one, so this folds to the number with no runtime cost.
            Expr::SizeOf(t) => {
                let size = match t {
                    Ty::Record(rec) => self.c_record_size(rec)?,
                    other => other.c_size_align().map(|(s, _)| s).ok_or_else(|| LowerError {
                        msg: format!("`size of {}` has no C layout", other.as_str()),
                    })?,
                };
                Ok(Val {
                    ty: Ty::Int64,
                    operand: size.to_string(),
                })
            }
            // A bare `ZeroInit` never reaches here: it is the initializer of a
            // c-record `var`, consumed by the `Let` arm before any value is
            // evaluated. Reaching it means the checker let one through somewhere
            // it should not have.
            Expr::ZeroInit => err(
                "an uninitialised c-record value is only valid as `var r: RECT`".to_string(),
            ),
            Expr::Cmp(op, l, r) => {
                let lv = self.eval(l)?;
                let rv = self.eval(r)?;
                if lv.ty == Ty::Text {
                    // Text comparison must compare CONTENT, not pointers.
                    let eq = self.call_text_eq(&lv, &rv)?;
                    return match op {
                        CmpOp::Eq => Ok(Val {
                            ty: Ty::Bool,
                            operand: eq,
                        }),
                        CmpOp::Ne => {
                            let t = self.fresh();
                            writeln!(self.body, "  {t} = xor i32 {eq}, 1").unwrap();
                            Ok(Val {
                                ty: Ty::Bool,
                                operand: t,
                            })
                        }
                        _ => err("text values support only `=` and `<>`"),
                    };
                }
                let pred = match (op, lv.ty) {
                    (CmpOp::Eq, Ty::Double) => "fcmp oeq",
                    (CmpOp::Ne, Ty::Double) => "fcmp one",
                    (CmpOp::Lt, Ty::Double) => "fcmp olt",
                    (CmpOp::Le, Ty::Double) => "fcmp ole",
                    (CmpOp::Gt, Ty::Double) => "fcmp ogt",
                    (CmpOp::Ge, Ty::Double) => "fcmp oge",
                    (CmpOp::Eq, _) => "icmp eq",
                    (CmpOp::Ne, _) => "icmp ne",
                    (CmpOp::Lt, _) => "icmp slt",
                    (CmpOp::Le, _) => "icmp sle",
                    (CmpOp::Gt, _) => "icmp sgt",
                    (CmpOp::Ge, _) => "icmp sge",
                };
                let bit = self.fresh();
                writeln!(
                    self.body,
                    "  {bit} = {pred} {} {}, {}",
                    llvm_ty(lv.ty),
                    lv.operand,
                    rv.operand
                )
                .unwrap();
                let t = self.fresh();
                writeln!(self.body, "  {t} = zext i1 {bit} to i32").unwrap();
                Ok(Val {
                    ty: Ty::Bool,
                    operand: t,
                })
            }
            Expr::Logical(op, l, r) => {
                // Short-circuit: the right side is evaluated only when needed,
                // so `x > 0 and 100 / x > 2` is safe.
                let slot = self.alloca(Ty::Bool);
                let rhs_label = self.fresh_label("rhs");
                let done = self.fresh_label("logic");
                let lv = self.eval(l)?;
                writeln!(self.body, "  store i32 {}, ptr {slot}", lv.operand).unwrap();
                let c = self.fresh();
                writeln!(self.body, "  {c} = icmp ne i32 {}, 0", lv.operand).unwrap();
                match op {
                    LogicalOp::And => {
                        writeln!(self.body, "  br i1 {c}, label %{rhs_label}, label %{done}")
                            .unwrap()
                    }
                    LogicalOp::Or => {
                        writeln!(self.body, "  br i1 {c}, label %{done}, label %{rhs_label}")
                            .unwrap()
                    }
                }
                writeln!(self.body, "{rhs_label}:").unwrap();
                let rv = self.eval(r)?;
                writeln!(self.body, "  store i32 {}, ptr {slot}", rv.operand).unwrap();
                writeln!(self.body, "  br label %{done}").unwrap();
                writeln!(self.body, "{done}:").unwrap();
                let t = self.fresh();
                writeln!(self.body, "  {t} = load i32, ptr {slot}").unwrap();
                Ok(Val {
                    ty: Ty::Bool,
                    operand: t,
                })
            }
            Expr::GetProperty {
                component,
                property,
            } => {
                // Variables first, then component ids — the order the checker
                // resolved it in, or the two would lower different programs.
                if let Some(Ty::Record(rec)) = self.var_ty(component) {
                    let b = self.eval(&Expr::Var(component.clone()))?;
                    return self.emit_field_read(rec, &b, property);
                }
                let handle = self.handle_of(component)?;
                let n = self.cstr(property);
                let ty = self.property_ty(component, property)?;
                let lib = self.owner(component);
                match ty {
                    Ty::Int => {
                        let f = match &lib {
                            None => {
                                self.ui_used.insert("oe_ui_get_int");
                                "oe_ui_get_int".to_string()
                            }
                            Some(lib) => {
                                self.component_libs.insert(lib.clone());
                                format!("oe_{lib}_component_get_int")
                            }
                        };
                        let t = self.fresh();
                        writeln!(self.body, "  {t} = call i32 @{f}(i64 {handle}, ptr {n})").unwrap();
                        Ok(Val {
                            ty: Ty::Int,
                            operand: t,
                        })
                    }
                    // A truth value has no getter of its own: every
                    // implementation of the property ABI answers a bool as the
                    // text `true` or `false`, so the read is the text read and
                    // the comparison is what makes it a bool again. Without
                    // this arm the value falls through as text and `if
                    // agree.checked` fails to lower at all.
                    Ty::Bool => {
                        let f = match &lib {
                            None => {
                                self.ui_used.insert("oe_ui_get");
                                "oe_ui_get".to_string()
                            }
                            Some(lib) => {
                                self.component_libs.insert(lib.clone());
                                format!("oe_{lib}_component_get")
                            }
                        };
                        let t = self.fresh();
                        writeln!(self.body, "  {t} = call ptr @{f}(i64 {handle}, ptr {n})").unwrap();
                        let read = Val {
                            ty: Ty::Text,
                            operand: t,
                        };
                        let yes = self.eval(&Expr::TextLit("true".into()))?;
                        let eq = self.call_text_eq(&read, &yes)?;
                        Ok(Val {
                            ty: Ty::Bool,
                            operand: eq,
                        })
                    }
                    _ => {
                        let f = match &lib {
                            None => {
                                self.ui_used.insert("oe_ui_get");
                                "oe_ui_get".to_string()
                            }
                            Some(lib) => {
                                self.component_libs.insert(lib.clone());
                                format!("oe_{lib}_component_get")
                            }
                        };
                        let t = self.fresh();
                        writeln!(self.body, "  {t} = call ptr @{f}(i64 {handle}, ptr {n})").unwrap();
                        Ok(Val {
                            ty: Ty::Text,
                            operand: t,
                        })
                    }
                }
            }
        }
    }

    /// The declared type of a component property, from the introspected
    /// descriptor (the validator has already proven it exists).
    fn property_ty(&self, component: &str, property: &str) -> Result<Ty, LowerError> {
        let type_name = self
            .component_types
            .get(component)
            .ok_or_else(|| LowerError {
                msg: format!("unknown component `{component}`"),
            })?;
        self.reg
            .component(type_name)
            .and_then(|d| d.property(property))
            .map(|p| p.ty)
            .ok_or_else(|| LowerError {
                msg: format!("`{type_name}` has no property `{property}`"),
            })
    }

    /// Reinterpret a value's operand as the raw `i64` stored in a slot's value
    /// field; returns the operand holding the i64.
    fn emit_arg_i64(&mut self, v: &Val) -> String {
        match v.ty {
            Ty::Int64 => v.operand.clone(),
            Ty::Int | Ty::Bool => {
                let t = self.fresh();
                writeln!(self.body, "  {t} = sext i32 {} to i64", v.operand).unwrap();
                t
            }
            Ty::Double => {
                let t = self.fresh();
                writeln!(self.body, "  {t} = bitcast double {} to i64", v.operand).unwrap();
                t
            }
            _ => {
                let t = self.fresh();
                writeln!(self.body, "  {t} = ptrtoint ptr {} to i64", v.operand).unwrap();
                t
            }
        }
    }

    /// Reinterpret an `i64` loaded from a slot's value field back to `ty`.
    fn emit_ret_from_i64(&mut self, ty: Ty, raw: &str) -> String {
        match ty {
            Ty::Int64 => raw.to_string(),
            Ty::Int | Ty::Bool => {
                let t = self.fresh();
                writeln!(self.body, "  {t} = trunc i64 {raw} to i32").unwrap();
                t
            }
            Ty::Double => {
                let t = self.fresh();
                writeln!(self.body, "  {t} = bitcast i64 {raw} to double").unwrap();
                t
            }
            _ => {
                let t = self.fresh();
                writeln!(self.body, "  {t} = inttoptr i64 {raw} to ptr").unwrap();
                t
            }
        }
    }

    /// Lower a call to a user subroutine: a direct native call, not the slot
    /// ABI. Nothing is marshalled, so recursion and a value return are exactly
    /// what LLVM already does for a C function.
    fn eval_user_call(
        &mut self,
        name: &str,
        sig: &Signature,
        args: &[Expr],
    ) -> Result<Option<Val>, LowerError> {
        if args.len() != sig.params.len() {
            return err(format!(
                "subroutine `{name}` expects {} argument(s), got {}",
                sig.params.len(),
                args.len()
            ));
        }
        let mut ops = Vec::new();
        for (i, a) in args.iter().enumerate() {
            // Hint each argument with the parameter's declared type, exactly as
            // the command path does — otherwise an `int` literal passed to an
            // `int64` parameter would type-check (the validator hints) and then
            // fail here (this did not). A callback sub taking `int64`/`ptr`
            // parameters is the first thing a later stage calls from user code.
            let v = self.eval_hinted(a, Some(sig.params[i]))?;
            if v.ty != sig.params[i] {
                return err(format!(
                    "subroutine `{name}` argument {} expects {}, got {}",
                    i + 1,
                    sig.params[i].as_str(),
                    v.ty.as_str()
                ));
            }
            ops.push(format!("{} {}", llvm_ty(v.ty), v.operand));
        }
        let arglist = ops.join(", ");
        let symbol = user_symbol(name);
        match sig.ret {
            None => {
                writeln!(self.body, "  call void @{symbol}({arglist})").unwrap();
                Ok(None)
            }
            Some(t) => {
                let r = self.fresh();
                writeln!(self.body, "  {r} = call {} @{symbol}({arglist})", llvm_ty(t)).unwrap();
                Ok(Some(Val {
                    ty: t,
                    operand: r,
                }))
            }
        }
    }

    /// Lower a call to a foreign function: resolve its symbol through the
    /// runtime loader (cached, so the resolution happens once) and make an
    /// indirect call with the declared C signature.
    ///
    /// This is a plain C call, not the slot ABI — a `dll` names an ordinary C
    /// export, and the whole point is to reach it exactly as C would. Each
    /// argument is already the C representation of its type: an `int` is an
    /// `i32`, a `text` is the `char*` backing it, a `ptr` is the pointer.
    fn eval_dll_call(
        &mut self,
        name: &str,
        dll: &DllSig,
        args: &[Expr],
    ) -> Result<Option<Val>, LowerError> {
        let sig = &dll.sig;
        if args.len() != sig.params.len() {
            return err(format!(
                "foreign function `{name}` expects {} argument(s), got {}",
                sig.params.len(),
                args.len()
            ));
        }
        let mut ops: Vec<String> = Vec::new();
        for (i, a) in args.iter().enumerate() {
            let want = sig.params[i];
            let v = self.eval_hinted(a, Some(want))?;
            if v.ty != want {
                return err(format!(
                    "foreign function `{name}` argument {} expects {}, got {}",
                    i + 1,
                    want.as_str(),
                    v.ty.as_str()
                ));
            }
            ops.push(self.marshal_c_arg(want, &v));
        }

        // Resolve once, cache in a per-declaration global: a call in a loop
        // pays the load-and-lookup cost a single time. `oe_dll_get` reads the
        // cache, resolves and stores it if empty, and returns the address —
        // aborting through `oe_runtime_error` if the library or symbol is
        // missing, so a bad call is a named failure, never a silent 0.
        self.needs_dll_get = true;
        self.dll_cached.insert(name.to_string());
        let cache = dll_cache_symbol(name);
        let lib = self.cstr(&dll.library);
        let sym = self.cstr(&dll.symbol);
        let fp = self.fresh();
        writeln!(
            self.body,
            "  {fp} = call ptr @oe_dll_get(ptr @{cache}, ptr {lib}, ptr {sym})"
        )
        .unwrap();

        // `dll.conv` (stdcall/cdecl/system) is intentionally NOT emitted onto
        // the `call`: every target OpenEPL builds is 64-bit with a single C
        // convention, so the three markers name the same one and a textual
        // callconv here would only risk perturbing the proven `--os windows`
        // path for no behavioural gain. The marker is carried on the `DllSig`
        // for a future 32-bit backend, which WOULD read it at this point.
        self.emit_c_call(&fp, &ops, sig.ret)
    }

    /// One argument, in the C representation the boundary wants.
    ///
    /// A `text` marshals as the `char*` backing it. That pointer is NULL for a
    /// runtime-produced empty text (an allocation that failed, a
    /// `last_error_text` with nothing to say), and a C function running
    /// `strlen` on NULL would fault — so a null text is handed a pointer to
    /// `""` instead. A literal `""` is already a real pointer, so the common
    /// case takes the fast side of the `select`. Everything else is already its
    /// own C representation: an `int` is an `i32`, a `ptr` is the pointer, and a
    /// c-record `Val` is the address of its flat storage.
    fn marshal_c_arg(&mut self, want: Ty, v: &Val) -> String {
        let operand = if want == Ty::Text {
            let empty = self.cstr("");
            let isnull = self.fresh();
            writeln!(self.body, "  {isnull} = icmp eq ptr {}, null", v.operand).unwrap();
            let sel = self.fresh();
            writeln!(
                self.body,
                "  {sel} = select i1 {isnull}, ptr {empty}, ptr {}",
                v.operand
            )
            .unwrap();
            sel
        } else {
            v.operand.clone()
        };
        format!("{} {}", llvm_ty(want), operand)
    }

    /// Emit the `call` itself, given a callee operand and marshalled arguments,
    /// and bring the C result back into an OpenEPL value.
    ///
    /// `fp` is a `ptr`-typed operand however it was obtained — the address
    /// `oe_dll_get` resolved for a `dll`, or the run-time pointer a
    /// `call through` was handed. Under opaque pointers those are the same
    /// thing to LLVM, which is why one emitter serves both and a returned
    /// `char*` or C truth is converted in exactly one place.
    fn emit_c_call(
        &mut self,
        fp: &str,
        ops: &[String],
        ret: Option<Ty>,
    ) -> Result<Option<Val>, LowerError> {
        let arglist = ops.join(", ");
        match ret {
            None => {
                writeln!(self.body, "  call void {fp}({arglist})").unwrap();
                Ok(None)
            }
            Some(Ty::Text) => {
                // The C side returns a `char*` it still owns; copy it into a
                // runtime-owned text so the result lives and is freed like every
                // other text, and a NULL return becomes `""`.
                self.needs_dll_text = true;
                let raw = self.fresh();
                writeln!(self.body, "  {raw} = call ptr {fp}({arglist})").unwrap();
                let out = self.fresh();
                writeln!(self.body, "  {out} = call ptr @oe_dll_text(ptr {raw})").unwrap();
                Ok(Some(Val {
                    ty: Ty::Text,
                    operand: out,
                }))
            }
            Some(Ty::Bool) => {
                // C truth is any non-zero int; normalise to 0/1 so a returned
                // `bool` compares equal to `true` when it should.
                let raw = self.fresh();
                writeln!(self.body, "  {raw} = call i32 {fp}({arglist})").unwrap();
                let nz = self.fresh();
                writeln!(self.body, "  {nz} = icmp ne i32 {raw}, 0").unwrap();
                let out = self.fresh();
                writeln!(self.body, "  {out} = zext i1 {nz} to i32").unwrap();
                Ok(Some(Val {
                    ty: Ty::Bool,
                    operand: out,
                }))
            }
            Some(t) => {
                let r = self.fresh();
                writeln!(self.body, "  {r} = call {} {fp}({arglist})", llvm_ty(t)).unwrap();
                Ok(Some(Val { ty: t, operand: r }))
            }
        }
    }

    /// Lower `call through EXPR(args...): T` — a C call whose callee is a value
    /// rather than a symbol.
    ///
    /// The only difference from a `dll` call is where the address comes from.
    /// There is no `oe_dll_get`, no cache and no library to open: the program
    /// already holds the pointer. Under opaque pointers the `ptr` IS the callee
    /// operand — there is no bitcast to a function type to emit — so the same
    /// `call <ret> %fp(args)` a `dll` produces is what comes out here, which is
    /// why the two share `marshal_c_arg` and `emit_c_call` rather than each
    /// having their own idea of how a `text` crosses.
    ///
    /// Each argument's own type is the parameter type: the call site declared
    /// the signature by writing the expressions, so there is nothing to check
    /// an argument against — the checker has already proven each has a C shape.
    /// `conv` is not emitted, for the reason a `dll`'s is not.
    fn eval_call_through(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        ret: Option<Ty>,
    ) -> Result<Option<Val>, LowerError> {
        let target = self.eval(callee)?;
        if target.ty != Ty::Ptr {
            return err(format!(
                "`call through` needs a ptr to call, got {}",
                target.ty.as_str()
            ));
        }
        // The address is evaluated FIRST, before the arguments, so a callee
        // expression with a side effect happens where it is written.
        let fp = target.operand.clone();
        let mut ops: Vec<String> = Vec::new();
        for a in args {
            let v = self.eval(a)?;
            ops.push(self.marshal_c_arg(v.ty, &v));
        }
        self.emit_c_call(&fp, &ops, ret)
    }

    /// Lower a command call via the slot ABI; returns the result if non-void.
    fn eval_call(&mut self, cmd: &str, args: &[Expr]) -> Result<Option<Val>, LowerError> {
        // Commands win the name; the validator has already rejected any user
        // sub that tried to take one.
        if self.reg.get(cmd).is_none() {
            if let Some(sig) = self.reg.sub(cmd).cloned() {
                return self.eval_user_call(cmd, &sig, args);
            }
            if let Some(dll) = self.reg.dll(cmd).cloned() {
                return self.eval_dll_call(cmd, &dll, args);
            }
        }
        let command = self.reg.get(cmd).ok_or_else(|| LowerError {
            msg: format!("unknown command `{cmd}`"),
        })?;
        let sig = command.sig.clone();
        let symbol = command.symbol.clone();

        if args.len() != sig.params.len() {
            return err(format!(
                "command `{cmd}` expects {} argument(s), got {}",
                sig.params.len(),
                args.len()
            ));
        }

        // Lower and type-check each argument first (may emit arithmetic).
        // `AnyArray`/`AnyElem` parameters take their meaning from the array
        // argument this call was given; `resolve_ret` reads the same thing back
        // out for the result, so the two cannot drift.
        let mut arg_vals: Vec<Val> = Vec::new();
        let mut elem: Option<Elem> = None;
        for (i, a) in args.iter().enumerate() {
            let want = match sig.params[i] {
                Ty::AnyArray | Ty::AnyDict => None,
                Ty::AnyElem => elem.map(Elem::ty),
                t => Some(t),
            };
            let v = self.eval_hinted(a, want)?;
            match sig.params[i] {
                Ty::AnyArray => match v.ty.elem() {
                    Some(e) => elem = Some(e),
                    None => {
                        return err(format!(
                            "command `{cmd}` argument {} expects an array, got {}",
                            i + 1,
                            v.ty.as_str()
                        ))
                    }
                },
                Ty::AnyDict => match v.ty.value() {
                    Some(e) => elem = Some(e),
                    None => {
                        return err(format!(
                            "command `{cmd}` argument {} expects a dictionary, got {}",
                            i + 1,
                            v.ty.as_str()
                        ))
                    }
                },
                Ty::AnyElem => {
                    if Some(v.ty) != elem.map(Elem::ty) {
                        return err(format!(
                            "command `{cmd}` argument {} does not match what the collection holds",
                            i + 1
                        ));
                    }
                }
                t if v.ty != t => {
                    return err(format!(
                        "command `{cmd}` argument {} expects {}, got {}",
                        i + 1,
                        t.as_str(),
                        v.ty.as_str()
                    ))
                }
                _ => {}
            }
            arg_vals.push(v);
        }
        let arg_tys: Vec<Ty> = arg_vals.iter().map(|v| v.ty).collect();
        let ret_ty = resolve_ret(&sig, &arg_tys);

        let argc = arg_vals.len();
        // Return slot (always allocated; ignored for void commands). Reserved
        // in the prologue, not here: this code runs wherever the call appears,
        // and an `alloca` in a loop body reserves fresh space on every turn
        // that nothing reclaims until the function returns. A loop calling a
        // command used to exhaust the stack at around a quarter of a million
        // iterations. The size is a constant per call site, so each site gets
        // its own slot and no two calls share one.
        let ret_slot = self.alloca_temp("%Slot");

        // argv array + per-argument stores.
        let argv_base = if argc > 0 {
            let argv = self.alloca_temp(&format!("[{argc} x %Slot]"));
            for (i, v) in arg_vals.iter().enumerate() {
                let raw = self.emit_arg_i64(v);
                let slot = self.fresh();
                writeln!(
                    self.body,
                    "  {slot} = getelementptr [{argc} x %Slot], ptr {argv}, i64 0, i64 {i}"
                )
                .unwrap();
                let tagp = self.fresh();
                writeln!(
                    self.body,
                    "  {tagp} = getelementptr %Slot, ptr {slot}, i32 0, i32 0"
                )
                .unwrap();
                writeln!(self.body, "  store i32 {}, ptr {tagp}", v.ty.sdt_tag()).unwrap();
                let valp = self.fresh();
                writeln!(
                    self.body,
                    "  {valp} = getelementptr %Slot, ptr {slot}, i32 0, i32 2"
                )
                .unwrap();
                writeln!(self.body, "  store i64 {raw}, ptr {valp}").unwrap();
            }
            let base = self.fresh();
            writeln!(
                self.body,
                "  {base} = getelementptr [{argc} x %Slot], ptr {argv}, i64 0, i64 0"
            )
            .unwrap();
            base
        } else {
            "null".to_string()
        };

        self.used.insert(symbol.clone());
        writeln!(
            self.body,
            "  call void @{symbol}(ptr {ret_slot}, i32 {argc}, ptr {argv_base})"
        )
        .unwrap();

        match ret_ty {
            None => Ok(None),
            Some(rt) => {
                let valp = self.fresh();
                writeln!(
                    self.body,
                    "  {valp} = getelementptr %Slot, ptr {ret_slot}, i32 0, i32 2"
                )
                .unwrap();
                let raw = self.fresh();
                writeln!(self.body, "  {raw} = load i64, ptr {valp}").unwrap();
                let res = self.emit_ret_from_i64(rt, &raw);
                Ok(Some(Val {
                    ty: rt,
                    operand: res,
                }))
            }
        }
    }

    /// Everything except the entry point. A library stops here.
    fn finish_library(self, module_name: &str, functions: &str) -> String {
        self.finish_with(module_name, functions, false)
    }

    fn finish(self, module_name: &str, functions: &str) -> String {
        self.finish_with(module_name, functions, true)
    }

    fn finish_with(self, module_name: &str, functions: &str, entry: bool) -> String {
        let mut out = String::new();
        writeln!(
            out,
            "; OpenEPL-generated LLVM IR — module `{module_name}` (Phase 2, slot ABI)"
        )
        .unwrap();
        writeln!(out, "; Do not edit; regenerate from the .oir source.\n").unwrap();

        // The slot type mirrors OpenEPL_Slot (abi/openepl_abi.h): {tag, pad, value}.
        writeln!(out, "%Slot = type {{ i32, i32, i64 }}\n").unwrap();

        // Module variables. Zero-initialised here; their declared initializers
        // run at entry, so a `var` may call a command (`var t: int64 = now()`).
        let mut gnames: Vec<(&String, &Ty)> = self.globals.iter().collect();
        gnames.sort_by(|a, b| a.0.cmp(b.0));
        for (name, ty) in gnames {
            let zero = match ty {
                Ty::Double => "0.0",
                t if t.is_pointer() => "null",
                _ => "0",
            };
            writeln!(
                out,
                "@{} = internal global {} {zero}",
                global_symbol(name),
                llvm_ty(*ty)
            )
            .unwrap();
        }
        if !self.globals.is_empty() {
            out.push('\n');
        }

        // One address cache per foreign function called. `null` means "not yet
        // resolved"; `oe_dll_get` fills it on the first call.
        for name in &self.dll_cached {
            writeln!(out, "@{} = internal global ptr null", dll_cache_symbol(name)).unwrap();
        }
        if !self.dll_cached.is_empty() {
            out.push('\n');
        }

        for (id, s) in self.strings.iter().enumerate() {
            let encoded = encode_llvm_string(s);
            let bytes = s.len() + 1;
            writeln!(
                out,
                "@.str{id} = private unnamed_addr constant [{bytes} x i8] c\"{encoded}\\00\""
            )
            .unwrap();
        }
        if !self.strings.is_empty() {
            out.push('\n');
        }

        // Every command shares the one slot-ABI signature.
        for sym in &self.used {
            writeln!(out, "declare void @{sym}(ptr, i32, ptr)").unwrap();
        }
        if !self.used.is_empty() {
            out.push('\n');
        }

        // UI interface declarations (abi/openepl_ui.h), only when referenced.
        for sym in &self.ui_used {
            let decl = match *sym {
                "oe_ui_init" => "declare i32 @oe_ui_init(ptr, i32, i32)",
                "oe_ui_shutdown" => "declare void @oe_ui_shutdown()",
                "oe_ui_root" => "declare i64 @oe_ui_root()",
                "oe_ui_create" => "declare i64 @oe_ui_create(i64, ptr)",
                "oe_ui_set" => "declare i32 @oe_ui_set(i64, ptr, ptr)",
                "oe_ui_get" => "declare ptr @oe_ui_get(i64, ptr)",
                "oe_ui_get_int" => "declare i32 @oe_ui_get_int(i64, ptr)",
                "oe_ui_on" => "declare i32 @oe_ui_on(i64, ptr, ptr)",
                "oe_ui_set_a11y" => "declare i32 @oe_ui_set_a11y(i64, i32, ptr)",
                "oe_ui_run" => "declare i32 @oe_ui_run()",
                other => panic!("undeclared UI symbol {other}"),
            };
            writeln!(out, "{decl}").unwrap();
        }
        if !self.ui_used.is_empty() {
            out.push('\n');
        }

        // A library's own component entry points (abi/openepl_abi.h). All five
        // are declared together rather than tracked one at a time: a `declare`
        // that nothing calls costs nothing, and the five are the whole of what
        // addressing a component means.
        for lib in &self.component_libs {
            writeln!(out, "declare i64 @oe_{lib}_component_create(ptr)").unwrap();
            writeln!(out, "declare i32 @oe_{lib}_component_set(i64, ptr, ptr)").unwrap();
            writeln!(out, "declare ptr @oe_{lib}_component_get(i64, ptr)").unwrap();
            writeln!(out, "declare i32 @oe_{lib}_component_get_int(i64, ptr)").unwrap();
            writeln!(out, "declare i32 @oe_{lib}_component_on(i64, ptr, ptr)").unwrap();
        }
        if !self.component_libs.is_empty() {
            out.push('\n');
        }

        if self.loop_used {
            writeln!(out, "declare i32 @oe_loop_run()\n").unwrap();
        }

        for sym in &self.aggr_used {
            let decl = match *sym {
                "oe_ary_new" => "declare ptr @oe_ary_new(i32, i32)",
                "oe_ary_get" => "declare i64 @oe_ary_get(ptr, i32)",
                "oe_ary_set" => "declare void @oe_ary_set(ptr, i32, i64)",
                "oe_bin_at" => "declare i32 @oe_bin_at(ptr, i32)",
                "oe_bin_set" => "declare void @oe_bin_set(ptr, i32, i32)",
                "oe_rec_new" => "declare ptr @oe_rec_new(i32)",
                "oe_rec_get" => "declare i64 @oe_rec_get(ptr, i32)",
                "oe_rec_set" => "declare void @oe_rec_set(ptr, i32, i64)",
                "oe_dict_new" => "declare ptr @oe_dict_new(i32)",
                "oe_dict_at" => "declare i64 @oe_dict_at(ptr, ptr)",
                "oe_dict_put" => "declare void @oe_dict_put(ptr, ptr, i64)",
                other => panic!("undeclared aggregate symbol {other}"),
            };
            writeln!(out, "{decl}").unwrap();
        }
        if !self.aggr_used.is_empty() {
            out.push('\n');
        }

        // The runtime notification channel (abi/openepl_abi.h), used to abort
        // with a message. Declared only when something actually aborts.
        if self.needs_notify {
            writeln!(out, "declare ptr @oe_notify(i32, ptr, ptr)\n").unwrap();
        }

        // The error slot's reset (runtime/oe_error.c). An optional's initializer
        // clears it before the call it is reading, so that what it reads back is
        // that call's verdict and not an older failure's.
        if self.needs_error_clear {
            writeln!(out, "declare void @oe_error_clear()\n").unwrap();
        }

        // The foreign-function loader (runtime/oe_dll.c): resolve-and-cache a
        // symbol, and copy a returned C string into a runtime-owned text.
        if self.needs_dll_get {
            writeln!(out, "declare ptr @oe_dll_get(ptr, ptr, ptr)").unwrap();
        }
        if self.needs_dll_text {
            writeln!(out, "declare ptr @oe_dll_text(ptr)").unwrap();
        }
        if self.needs_dll_get || self.needs_dll_text {
            out.push('\n');
        }

        out.push_str(functions);
        for thunk in self.thunks.values() {
            out.push_str(thunk);
        }

        if entry {
            writeln!(out, "define i32 @ECodeStart() {{").unwrap();
            writeln!(out, "entry:").unwrap();
            out.push_str(&self.allocas.join(""));
            out.push_str(self.body.as_str());
            match &self.exit_code {
                Some(rc) => writeln!(out, "  ret i32 {rc}").unwrap(),
                None => writeln!(out, "  ret i32 0").unwrap(),
            }
            writeln!(out, "}}").unwrap();
        }
        if let Some(d) = &self.debug {
            if !d.is_empty() {
                out.push_str("\nattributes #0 = { \"frame-pointer\"=\"all\" }\n");
                out.push_str(&d.render());
            }
        }
        out
    }
}

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
    use openepl_ir::parse;

    fn lower(src: &str) -> Result<String, LowerError> {
        let m = parse(src).unwrap();
        lower_module(&m, &Registry::core())
    }

    /// Lower as a build with debug information does.
    fn lower_dbg(src: &str) -> String {
        let m = parse(src).unwrap();
        lower_module_from(&m, &Registry::core(), Some("examples/demo.oir")).unwrap()
    }

    /// `Registry::core()` plus a non-visual component whose `beep` event hands
    /// its handler an int — the shape the `timer`'s `tick` has, without putting
    /// an invented component into the hard-coded core set.
    fn lower_with_buzzer(src: &str) -> String {
        use openepl_ir::registry::{ComponentDesc, ComponentKind};
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

    const BUZZER: &str = "module m\n\nbuzzer b\n  on beep: h\nend\n\nsub main\n  call print_int(1)\nend\n\n";

    /// The library is handed a pointer with the EVENT's signature, never the
    /// handler's: that is what makes the cast on the C side type-correct.
    #[test]
    fn a_parameterised_event_binds_through_a_thunk() {
        let ll = lower_with_buzzer(&format!("{BUZZER}sub h(n: int)\n  call print_int(n)\nend\n"));
        assert!(ll.contains("define internal void @oe_evt_h_i32(i32 %a0)"), "{ll}");
        assert!(ll.contains("call void @oe_user_h(i32 %a0)"), "{ll}");
        assert!(
            ll.contains("@oe_core_component_on(") && ll.contains("ptr @oe_evt_h_i32)"),
            "{ll}"
        );
    }

    /// A handler that ignores the argument is bound through a thunk that drops
    /// it, rather than through a pointer the library would have to call with
    /// the wrong type.
    #[test]
    fn a_handler_that_ignores_the_argument_still_gets_the_event_signature() {
        let ll = lower_with_buzzer(&format!("{BUZZER}sub h\n  call print_int(1)\nend\n"));
        assert!(ll.contains("define internal void @oe_evt_h_i32(i32 %a0)"), "{ll}");
        assert!(ll.contains("call void @oe_user_h()"), "{ll}");
    }

    /// Two components binding one subroutine to one event share a thunk. Two
    /// `define`s of one name is not a diagnostic anywhere in this compiler —
    /// it is invalid IR that `llc` rejects at the end of the build.
    #[test]
    fn two_bindings_of_one_handler_share_a_thunk() {
        let ll = lower_with_buzzer(
            "module m\n\nbuzzer b1\n  on beep: h\nend\n\nbuzzer b2\n  on beep: h\nend\n\n             sub main\n  call print_int(1)\nend\n\nsub h(n: int)\n  call print_int(n)\nend\n",
        );
        assert_eq!(ll.matches("define internal void @oe_evt_h_i32").count(), 1, "{ll}");
        assert_eq!(ll.matches("ptr @oe_evt_h_i32)").count(), 2, "{ll}");
    }

    /// An event that hands nothing over binds the subroutine itself, so every
    /// program written before events could carry anything lowers to what it
    /// lowered to before.
    #[test]
    fn an_event_with_no_parameters_binds_the_subroutine_directly() {
        let ll = lower_with_buzzer(&format!(
            "module m\n\nbuzzer b\nend\n\nsub main\n  call print_int(1)\nend\n"
        ));
        assert!(!ll.contains("oe_evt_"), "{ll}");
    }

    #[test]
    fn emits_slot_type_and_abi_call() {
        let ll = lower("module m\nsub main\n  call print_int(42)\nend\n").unwrap();
        assert!(ll.contains("%Slot = type { i32, i32, i64 }"));
        assert!(ll.contains("declare void @oe_print_int(ptr, i32, ptr)"));
        assert!(ll.contains("call void @oe_print_int(ptr %"));
        assert!(ll.contains("store i32 3, ptr")); // SDT_INT tag
    }

    #[test]
    fn call_expr_reads_return_slot() {
        let ll =
            lower("module m\nsub main\n  let n: int = length(\"hi\")\n  call print_int(n)\nend\n")
                .unwrap();
        assert!(ll.contains("call void @oe_length(ptr"));
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
        assert!(ll.contains("define i32 @oe_user_fib(i32 %p0)"), "{ll}");
        assert!(ll.contains("call i32 @oe_user_fib(i32 "), "{ll}");
        assert!(ll.contains("ret i32 "), "{ll}");
        // The fall-through past the last `return` is proven dead.
        assert!(ll.contains("unreachable"), "{ll}");
        // A user sub is defined, never declared: a declare + define of the same
        // symbol is not a valid module.
        assert!(!ll.contains("declare void @oe_user_fib"), "{ll}");
    }

    /// Entry points and event handlers must lower to exactly the shape they did
    /// before parameters existed, or a handler bound by pointer would be called
    /// through a mismatched signature.
    #[test]
    fn a_plain_sub_lowers_unchanged() {
        let ll = lower("module m\nsub main\n  call print_int(1)\nend\n").unwrap();
        assert!(ll.contains("define void @oe_user_main() {"), "{ll}");
        assert!(ll.contains("call void @oe_user_main()"), "{ll}");
    }

    #[test]
    fn a_library_export_forwards_its_arguments() {
        let ll = lower(
            "module m\ntarget sharedlib\nsub twice(n: int): int\n  return n + n\nend\n",
        )
        .unwrap();
        assert!(ll.contains("define i32 @twice(i32 %a0)"), "{ll}");
        assert!(ll.contains("call i32 @oe_user_twice(i32 %a0)"), "{ll}");
    }

    #[test]
    fn text_plus_lowers_to_the_concat_command() {
        let ll = lower("module m\nsub main\n  call print_text(\"a\" + \"b\")\nend\n").unwrap();
        assert!(ll.contains("declare void @oe_concat(ptr, i32, ptr)"));
        assert!(ll.contains("call void @oe_concat(ptr %"));
    }

    /// Each side of a text `+` must be evaluated exactly once. Forwarding the
    /// original expressions to `concat` instead of the values would make
    /// `read_line() + \"!\"` read two lines.
    #[test]
    fn text_plus_evaluates_each_side_once() {
        let ll = lower("module m\nsub main\n  call print_text(read_line() + \"!\")\nend\n")
            .unwrap();
        assert_eq!(ll.matches("call void @oe_read_line(").count(), 1);
    }

    #[test]
    fn a_literal_divisor_needs_no_guard() {
        let ll = lower("module m\nsub main\n  var n: int = 9\n  call print_int(n / 3)\nend\n")
            .unwrap();
        assert!(ll.contains("sdiv"));
        assert!(!ll.contains("oe_notify"), "a constant 3 cannot be zero");
    }

    /// A divisor that is not a literal is checked for the two values the
    /// hardware faults on: zero, and -1 against the most negative dividend.
    #[test]
    fn a_variable_divisor_is_guarded_both_ways() {
        let ll = lower(
            "module m\nsub main\n  var d: int = 0\n  call print_int(10 / d)\nend\n",
        )
        .unwrap();
        assert!(ll.contains("declare ptr @oe_notify(i32, ptr, ptr)"));
        assert!(ll.contains("call ptr @oe_notify(i32 5,"));
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
        let ll = lower(
            "module m\nsub main\n  for i = 1 to 3\n    continue\n  end\nend\n",
        )
        .unwrap();
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
        let ll = lower(
            "module m\nsub main\n  var xs: int[] = [7, 8]\n  xs[0] = xs[1]\nend\n",
        )
        .unwrap();
        assert!(ll.contains("call ptr @oe_ary_new(i32 3, i32 2)"), "{ll}");
        assert!(ll.contains("call i64 @oe_ary_get(ptr"), "{ll}");
        assert!(ll.contains("call void @oe_ary_set(ptr"), "{ll}");
        assert!(ll.contains("declare i64 @oe_ary_get(ptr, i32)"), "{ll}");
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
        assert!(ll.contains("call ptr @oe_rec_new(i32 2)"), "{ll}");
        // `x` is field 1 and `y` is field 2, in declaration order.
        assert!(ll.contains("@oe_rec_set(ptr %t0, i32 1,"), "{ll}");
        assert!(ll.contains("@oe_rec_set(ptr %t0, i32 2,"), "{ll}");
        // `p.y = p.x` reads field 1 and writes field 2.
        assert!(ll.contains(", i32 1)\n"), "{ll}");
        assert!(!ll.contains("\"x\\00\""), "a field name reached the output:\n{ll}");
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
        assert!(ll.contains("declare i64 @oe_rec_get(ptr, i32)"), "{ll}");
        assert!(ll.contains("declare ptr @oe_rec_new(i32)"), "{ll}");
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
        assert!(ll.contains("define ptr @oe_user_bump(ptr %p0)"), "{ll}");
    }

    /// `d["k"]` and `d["k"] = v` are `dict_get`/`dict_set` spelled as a
    /// subscript, and reach the same direct helpers indexing does.
    #[test]
    fn a_dictionary_subscript_calls_the_helper_directly() {
        let ll = lower(
            "module m\nsub main\n  var d: int{} = {\"a\": 1}\n               d[\"b\"] = d[\"a\"]\nend\n",
        )
        .unwrap();
        // OE_SDT_DICT_OF(OE_SDT_INT) is not a tag: the value tag alone is what
        // the dictionary is told to hold.
        assert!(ll.contains("call ptr @oe_dict_new(i32 3)"), "{ll}");
        assert!(ll.contains("call i64 @oe_dict_at(ptr"), "{ll}");
        assert!(ll.contains("call void @oe_dict_put(ptr"), "{ll}");
        assert!(ll.contains("declare i64 @oe_dict_at(ptr, ptr)"), "{ll}");
    }

    /// A dictionary reaches a command as a pointer with the dictionary flag
    /// above its value tag — OE_SDT_DICT_FLAG | OE_SDT_INT.
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
        let ll = lower(
            "module m\nsub main\n  var xs: int[] = [1]\n  call print_int(count(xs))\nend\n",
        )
        .unwrap();
        assert!(ll.contains("ptrtoint ptr"), "{ll}");
        // OE_SDT_ARRAY_FLAG | OE_SDT_INT
        assert!(ll.contains("store i32 259,"), "{ll}");
    }

    #[test]
    fn a_byte_set_indexes_through_its_own_helper() {
        let ll = lower(
            "module m\nsub main\n  var b: bytes = bytes_new(1)\n  b[0] = 65\n  \
             call print_int(b[0])\nend\n",
        )
        .unwrap();
        assert!(ll.contains("call void @oe_bin_set(ptr"), "{ll}");
        assert!(ll.contains("call i32 @oe_bin_at(ptr"), "{ll}");
    }

    /// A module-level array starts as no array at all, and a pointer's zero is
    /// `null` — `0` would not even assemble.
    #[test]
    fn a_module_level_array_is_null_initialised() {
        let ll = lower(
            "module m\nvar xs: int[] = [1]\nsub main\n  call print_int(count(xs))\nend\n",
        )
        .unwrap();
        assert!(ll.contains("= internal global ptr null"), "{ll}");
    }

    #[test]
    fn only_used_commands_declared() {
        let ll = lower("module m\nsub main\n  call print_int(1)\nend\n").unwrap();
        assert!(ll.contains("declare void @oe_print_int(ptr, i32, ptr)"));
        assert!(!ll.contains("oe_sqrt"));
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
            ll.contains(r#"!DIFile(filename: "demo.oir", directory: "examples")"#),
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
            ll.contains(r#"!DISubprogram(name: "greet", linkageName: "oe_user_greet""#),
            "{ll}"
        );
        assert!(ll.contains("scopeLine: 3,"), "{ll}");
        assert!(ll.contains("define void @oe_user_greet() #0 !dbg !6 {"), "{ll}");
        // and `main`, three lines lower, gets its own subprogram
        assert!(
            ll.contains(r#"!DISubprogram(name: "main", linkageName: "oe_user_main""#),
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
        let ll = lower_dbg(
            "module m\nsub main\n  if 1 = 1\n    call print_int(1)\n  end\nend\n",
        );
        for line in ll.lines() {
            if line.ends_with(':') && !line.starts_with(' ') {
                assert!(!line.contains("!dbg"), "label carried a location: {line}");
            }
        }
        // and the instructions inside the branch did get one
        assert!(ll.contains("call void @oe_print_int"), "{ll}");
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
        assert!(!body.contains("!dbg"), "entry point carried locations: {body}");
    }

    /// A debug record carries its location as an argument, not as trailing
    /// metadata. Appending `!dbg` to one makes the whole module fail to parse,
    /// and the instruction stream cannot tell the difference by the `!dbg`
    /// test alone, because a record's spelling is `#dbg`.
    #[test]
    fn a_debug_record_is_not_given_trailing_metadata() {
        use std::fmt::Write as _;
        let mut body = Body { loc: Some(7), ..Body::default() };
        writeln!(body, "  store i32 1, ptr %v0").unwrap();
        writeln!(body, "  #dbg_declare(ptr %v0, !8, !DIExpression(), !7)").unwrap();
        let text = body.as_str();
        assert!(text.contains("store i32 1, ptr %v0, !dbg !7"), "{text}");
        for line in text.lines().filter(|l| l.contains("#dbg_")) {
            assert!(!line.ends_with(", !dbg !7"), "record carried metadata: {line}");
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
        let ll = lower(
            "module m\nsub main\n  for i in 1..10\n    call print_int(i)\n  end\nend\n",
        )
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
        let ll = lower(
            "module m\nsub main\n  if 1 = 1\n    call print_int(1)\n  end\nend\n",
        )
        .unwrap();
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
