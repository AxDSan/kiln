//! Command registry — the Phase 1 stand-in for the support-library ABI.
//!
//! Maps a surface command name to its `Signature` and the runtime symbol the
//! backend emits a call to.  In Phase 2 this table is replaced by signatures
//! loaded from `kiln_get_lib_info`; keeping it in one place now
//! means the validator and the backend agree on exactly one source of truth.

use std::collections::HashMap;

use crate::{CallConv, ConstDef, Module, RecordDef, Signature, Ty};

/// A foreign function the module declared with `dll`.  Held beside subs and
/// commands so a call resolves it the same way, but it carries the two extra
/// facts the backend needs that a sub does not: which library the symbol lives
/// in, and what that symbol is called there.
#[derive(Debug, Clone, PartialEq)]
pub struct DllSig {
    pub sig: Signature,
    /// The `from "..."` library, spelled exactly as written.
    pub library: String,
    /// The exported symbol to resolve (the `as` override, or the name itself).
    pub symbol: String,
    /// The declared calling convention, or `None`. Carried from the declaration
    /// for a future 32-bit backend; the backend emits the same code either way,
    /// because every target it builds today is 64-bit with one C convention.
    pub conv: Option<CallConv>,
}

/// A component property, as declared in a library's `ComponentDesc`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PropertyDesc {
    pub name: String,
    pub ty: Ty,
    /// Which editor an inspector should offer (`"color"`, `"file"`, `"font"`,
    /// `"multiline"`); empty asks for the plain one the type implies. Carried
    /// through from the descriptor for tools that want it — the compiler has no
    /// use for a hint.
    pub editor: String,
}

/// Whether a component occupies a rectangle. A timer has properties, events and
/// an inspector row exactly as a button does; what it lacks is a parent to be
/// drawn inside, which is why this is one field rather than a second model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentKind {
    Visual,
    NonVisual,
}

/// A component type contributed by a support library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentDesc {
    pub name: String,
    /// Accessibility role (`KN_ROLE_*`) — carried from the descriptor so a11y
    /// data exists from the start.
    pub a11y_role: i32,
    pub kind: ComponentKind,
    /// The library that declared it. A non-visual component is created through
    /// that library's own entry points, so the name is what the backend needs
    /// to emit a call at all.
    pub library: String,
    pub properties: Vec<PropertyDesc>,
    pub events: Vec<String>,
}

impl ComponentDesc {
    pub fn property(&self, name: &str) -> Option<&PropertyDesc> {
        self.properties.iter().find(|p| p.name == name)
    }
    pub fn has_event(&self, name: &str) -> bool {
        self.events.iter().any(|e| e == name)
    }
}

/// What a command's documentation says, when it has any.
///
/// Kept beside the registry rather than inside `Command` on purpose: a command
/// with no documentation is not a different kind of command, and every one of
/// the places that builds a `Command` — the hard-coded core set most of all —
/// would otherwise have to name two more fields it has nothing to put in.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CommandDoc {
    /// One sentence: what it does, and what it returns when it fails.
    pub summary: String,
    /// One to three lines of Kiln calling it, with no surrounding
    /// `module`/`sub` — the documentation generator supplies those.
    pub example: String,
}

/// One registered command.
#[derive(Debug, Clone, PartialEq)]
pub struct Command {
    pub sig: Signature,
    /// The runtime function symbol the backend emits a call to (see `runtime/`).
    pub symbol: String,
}

/// The set of commands the compiler knows how to lower.  Built either from the
/// hard-coded `core()` set (used by unit tests) or from `LibInfo` metadata read
/// out of a support library at build time (the CLI's authoritative path).
#[derive(Debug, Clone, Default)]
pub struct Registry {
    map: HashMap<String, Command>,
    /// Documentation, for the commands that have it. Sparse by design.
    docs: HashMap<String, CommandDoc>,
    components: HashMap<String, ComponentDesc>,
    /// Subroutines defined in the module being compiled, by name.
    ///
    /// They live beside the commands rather than among them so a user sub can
    /// never quietly take a library command's name: `register_subs` reports the
    /// collision instead of overwriting. Lookup order is commands first, then
    /// these, and `get` still means "library command" for every existing caller.
    subs: HashMap<String, Signature>,
    /// Foreign functions declared in the module being compiled, by name.
    ///
    /// They share the one callable namespace with commands, subs and records —
    /// `register_dlls` reports a collision rather than overwriting — but are
    /// kept apart because the backend lowers a `dll` call differently: an
    /// indirect call through a symbol resolved from a library at run time,
    /// rather than a direct call to a name the linker already knows.
    dlls: HashMap<String, DllSig>,
    /// Record types declared in the module being compiled, by name.
    ///
    /// They live here rather than in a parameter threaded through the checker
    /// because a record name is resolved in exactly the places a command name
    /// is — an expression, an argument, a declared type — and one lookup table
    /// for all of them is what keeps the validator and the backend agreeing
    /// about what `point` means.
    records: HashMap<String, RecordDef>,
    /// Named constants (`const MB_OK = 0`), by name.
    ///
    /// A constant shares the one flat name space with commands, subs, dlls and
    /// records — `register_consts` reports a collision rather than overwriting
    /// — because a reference to it is written where any of those could be. It
    /// is read-only and stores its literal value, which the checker types and
    /// the backend folds. A kit's declaration bundle contributes to this map
    /// the same way a program's own `const` lines do.
    consts: HashMap<String, ConstDef>,
    /// What each component event hands its handler, keyed by
    /// `(component type, event)`.
    ///
    /// Beside `components` rather than inside `ComponentDesc` because an event
    /// with parameters is the exception: every descriptor written so far
    /// declares none, and a `Vec<Ty>` on each of them would be a field that is
    /// empty everywhere and read almost nowhere. An absent entry means "hands
    /// nothing", which is what makes a library that never mentions parameters
    /// behave exactly as it did.
    event_params: HashMap<String, HashMap<String, Vec<Ty>>>,
}

impl Registry {
    pub fn new() -> Registry {
        Registry {
            map: HashMap::new(),
            docs: HashMap::new(),
            components: HashMap::new(),
            subs: HashMap::new(),
            dlls: HashMap::new(),
            records: HashMap::new(),
            consts: HashMap::new(),
            event_params: HashMap::new(),
        }
    }

    /// Record every subroutine in `m` as callable, returning the names that
    /// collide with a library command (the caller reports them — the validator
    /// has the line numbers).
    pub fn register_subs(&mut self, m: &Module) -> Vec<String> {
        let mut collisions = Vec::new();
        for sub in m.subs() {
            if self.map.contains_key(&sub.name) {
                collisions.push(sub.name.clone());
                continue;
            }
            self.subs.insert(sub.name.clone(), sub.signature());
        }
        collisions
    }

    /// Record every foreign-function declaration in `m` as callable, returning
    /// the names that collide with a library command, a subroutine or another
    /// `dll` (the caller reports them — the validator has the line numbers).
    ///
    /// A `dll` name is written in the same call position a command or a sub is,
    /// so it shares the one flat callable namespace with them.
    pub fn register_dlls(&mut self, m: &Module) -> Vec<String> {
        let mut collisions = Vec::new();
        for d in m.dlls() {
            if self.map.contains_key(&d.name)
                || self.subs.contains_key(&d.name)
                || self.dlls.contains_key(&d.name)
                || self.consts.contains_key(&d.name)
                || self.records.contains_key(&d.name)
            {
                collisions.push(d.name.clone());
                continue;
            }
            self.dlls.insert(
                d.name.clone(),
                DllSig {
                    sig: d.signature(),
                    library: d.library.clone(),
                    symbol: d.symbol_name().to_string(),
                    conv: d.conv,
                },
            );
        }
        collisions
    }

    /// The declaration of `name`, if it names a foreign function.
    pub fn dll(&self, name: &str) -> Option<&DllSig> {
        self.dlls.get(name)
    }

    /// Whether `name` names a foreign function.
    pub fn is_dll(&self, name: &str) -> bool {
        self.dlls.contains_key(name)
    }

    /// Foreign-function names, for diagnostics.
    pub fn dll_names(&self) -> impl Iterator<Item = &str> {
        self.dlls.keys().map(|s| s.as_str())
    }

    /// Record every record declaration in `m`, returning the names that collide
    /// with a library command or a subroutine (the caller reports them — the
    /// validator has the line numbers).
    ///
    /// A record name is written in expression position (`point(x: 1)`), so it
    /// shares the one flat callable namespace with commands and subroutines.
    pub fn register_records(&mut self, m: &Module) -> Vec<String> {
        let mut collisions = Vec::new();
        for rec in m.records() {
            if self.map.contains_key(&rec.name)
                || self.subs.contains_key(&rec.name)
                || self.dlls.contains_key(&rec.name)
                || self.records.contains_key(&rec.name)
                || self.consts.contains_key(&rec.name)
            {
                collisions.push(rec.name.clone());
                continue;
            }
            self.records.insert(rec.name.clone(), rec.clone());
        }
        collisions
    }

    /// The declaration of `name`, if it names a record type.
    pub fn record(&self, name: &str) -> Option<&RecordDef> {
        self.records.get(name)
    }

    /// Known record type names, for diagnostics.
    pub fn record_names(&self) -> impl Iterator<Item = &str> {
        self.records.keys().map(|s| s.as_str())
    }

    /// Insert one record declaration directly (a kit's declaration bundle),
    /// collision-checked across the whole callable name space; `false` if the
    /// name was already taken. The module-level `register_records` reports the
    /// name to the validator instead — this is the loader's door, where a
    /// collision is a kit-authoring error and the caller says which kit.
    pub fn insert_record(&mut self, rec: RecordDef) -> bool {
        if self.map.contains_key(&rec.name)
            || self.subs.contains_key(&rec.name)
            || self.dlls.contains_key(&rec.name)
            || self.records.contains_key(&rec.name)
            || self.consts.contains_key(&rec.name)
        {
            return false;
        }
        self.records.insert(rec.name.clone(), rec);
        true
    }

    /// Insert one foreign-function declaration directly (a kit's bundle),
    /// collision-checked; `false` if the name was already taken.
    pub fn insert_dll(&mut self, name: impl Into<String>, sig: DllSig) -> bool {
        let name = name.into();
        if self.map.contains_key(&name)
            || self.subs.contains_key(&name)
            || self.dlls.contains_key(&name)
            || self.records.contains_key(&name)
            || self.consts.contains_key(&name)
        {
            return false;
        }
        self.dlls.insert(name, sig);
        true
    }

    /// Record every named constant in `m`, returning the names that collide
    /// with a command, a subroutine, a `dll`, a record or another constant
    /// (the caller reports them — the validator has the line numbers).
    ///
    /// A constant is written in expression position, so it shares the one flat
    /// name space with everything else callable-or-nameable.
    pub fn register_consts(&mut self, m: &Module) -> Vec<String> {
        let mut collisions = Vec::new();
        for c in m.consts() {
            if !self.insert_const(c.clone()) {
                collisions.push(c.name.clone());
            }
        }
        collisions
    }

    /// Insert one constant directly (a program's `const` or a kit's bundle),
    /// collision-checked across the whole name space; `false` if taken.
    pub fn insert_const(&mut self, c: ConstDef) -> bool {
        if self.map.contains_key(&c.name)
            || self.subs.contains_key(&c.name)
            || self.dlls.contains_key(&c.name)
            || self.records.contains_key(&c.name)
            || self.consts.contains_key(&c.name)
        {
            return false;
        }
        self.consts.insert(c.name.clone(), c);
        true
    }

    /// The constant named `name`, if there is one.
    pub fn const_(&self, name: &str) -> Option<&ConstDef> {
        self.consts.get(name)
    }

    /// The type of the constant named `name`, if there is one — the fold the
    /// checker performs when it meets a bare name that is a constant.
    pub fn const_ty(&self, name: &str) -> Option<Ty> {
        self.consts.get(name).map(|c| c.ty)
    }

    /// Whether `name` names a constant.
    pub fn is_const(&self, name: &str) -> bool {
        self.consts.contains_key(name)
    }

    /// Constant names, for diagnostics and listing.
    pub fn const_names(&self) -> impl Iterator<Item = &str> {
        self.consts.keys().map(|s| s.as_str())
    }

    /// The signature of a user subroutine, if `name` is one.
    pub fn sub(&self, name: &str) -> Option<&Signature> {
        self.subs.get(name)
    }

    /// Whether `name` names a user subroutine.
    pub fn is_sub(&self, name: &str) -> bool {
        self.subs.contains_key(name)
    }

    /// User subroutine names, for diagnostics.
    pub fn sub_names(&self) -> impl Iterator<Item = &str> {
        self.subs.keys().map(|s| s.as_str())
    }

    /// Look up a component type by its surface name.
    pub fn component(&self, name: &str) -> Option<&ComponentDesc> {
        self.components.get(name)
    }

    /// Register a component type; `false` if the name was already taken.
    pub fn insert_component(&mut self, desc: ComponentDesc) -> bool {
        if self.components.contains_key(&desc.name) {
            return false;
        }
        self.components.insert(desc.name.clone(), desc);
        true
    }

    /// Declare what an event hands its handler. Called once per event that has
    /// parameters, after the component itself is registered.
    pub fn set_event_params(&mut self, component: &str, event: &str, params: Vec<Ty>) {
        self.event_params
            .entry(component.to_string())
            .or_default()
            .insert(event.to_string(), params);
    }

    /// What `component`'s `event` hands a handler; empty when it hands nothing.
    pub fn event_params(&self, component: &str, event: &str) -> &[Ty] {
        self.event_params
            .get(component)
            .and_then(|evs| evs.get(event))
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Known component type names, for diagnostics.
    pub fn component_names(&self) -> impl Iterator<Item = &str> {
        self.components.keys().map(|s| s.as_str())
    }

    /// Record what a command's documentation says. Both halves may be empty,
    /// in which case nothing is stored — an undocumented command is absent
    /// from this map rather than present with nothing in it.
    pub fn set_doc(&mut self, name: impl Into<String>, doc: CommandDoc) {
        if doc.summary.is_empty() && doc.example.is_empty() {
            return;
        }
        self.docs.insert(name.into(), doc);
    }

    /// What a command's documentation says, or `None` when it has none.
    pub fn doc(&self, name: &str) -> Option<&CommandDoc> {
        self.docs.get(name)
    }

    pub fn get(&self, name: &str) -> Option<&Command> {
        self.map.get(name)
    }

    /// Number of registered commands.
    pub fn len(&self) -> usize {
        self.map.len()
    }
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Command names, for diagnostics / drift checks.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.map.keys().map(|s| s.as_str())
    }

    /// Iterate `(name, command)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Command)> {
        self.map.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Register a command; returns `false` if the name was already taken (a
    /// duplicate across libraries — the caller reports it).
    pub fn insert(
        &mut self,
        name: impl Into<String>,
        sig: Signature,
        symbol: impl Into<String>,
    ) -> bool {
        let name = name.into();
        if self.map.contains_key(&name) {
            return false;
        }
        self.map.insert(
            name,
            Command {
                sig,
                symbol: symbol.into(),
            },
        );
        true
    }

    /// The built-in core command set (math / conversions / text / datetime / io).
    pub fn core() -> Registry {
        use Ty::*;
        let mut r = Registry::new();
        {
            let mut cmd =
                |name: &'static str, symbol: &'static str, params: &[Ty], ret: Option<Ty>| {
                    r.insert(
                        name,
                        Signature::simple(params.to_vec(), ret),
                        symbol,
                    );
                };

            // --- I/O (void) --------------------------------------------------
            cmd("print_int", "kn_print_int", &[Int], None);
            cmd("print_int64", "kn_print_int64", &[Int64], None);
            cmd("print_double", "kn_print_double", &[Double], None);
            cmd("print_text", "kn_print_text", &[Text], None);
            cmd("read_line", "kn_read_line", &[], Some(Text));
            cmd("input_ended", "kn_input_ended", &[], Some(Bool));
            cmd("ask", "kn_ask", &[Text], Some(Text));
            // What a failed `assert` runs: print the message and stop, failing.
            cmd("assert_failed", "kn_assert_failed", &[Text], None);

            // --- Errors ------------------------------------------------------
            cmd("last_error_code", "kn_last_error_code", &[], Some(Int));
            cmd("last_error_text", "kn_last_error_text", &[], Some(Text));

            // --- Integer math ------------------------------------------------
            cmd("abs_int", "kn_abs_int", &[Int], Some(Int));
            cmd("min_int", "kn_min_int", &[Int, Int], Some(Int));
            cmd("max_int", "kn_max_int", &[Int, Int], Some(Int));
            cmd("mod_int", "kn_mod_int", &[Int, Int], Some(Int));
            cmd("pow_int", "kn_pow_int", &[Int, Int], Some(Int));

            // --- Floating-point math ----------------------------------------
            cmd("sqrt", "kn_sqrt", &[Double], Some(Double));
            cmd("sin", "kn_sin", &[Double], Some(Double));
            cmd("cos", "kn_cos", &[Double], Some(Double));
            cmd("tan", "kn_tan", &[Double], Some(Double));
            cmd("pow", "kn_pow", &[Double, Double], Some(Double));
            cmd("exp", "kn_exp", &[Double], Some(Double));
            cmd("ln", "kn_ln", &[Double], Some(Double));
            cmd("log10", "kn_log10", &[Double], Some(Double));
            cmd("floor", "kn_floor", &[Double], Some(Double));
            cmd("ceil", "kn_ceil", &[Double], Some(Double));
            cmd("round", "kn_round", &[Double], Some(Double));
            cmd("abs_double", "kn_abs_double", &[Double], Some(Double));
            cmd(
                "min_double",
                "kn_min_double",
                &[Double, Double],
                Some(Double),
            );
            cmd(
                "max_double",
                "kn_max_double",
                &[Double, Double],
                Some(Double),
            );

            // --- Conversions -------------------------------------------------
            cmd("int_to_double", "kn_int_to_double", &[Int], Some(Double));
            cmd("double_to_int", "kn_double_to_int", &[Double], Some(Int));
            cmd("int_to_int64", "kn_int_to_int64", &[Int], Some(Int64));
            cmd("int64_to_int", "kn_int64_to_int", &[Int64], Some(Int));
            cmd("int_to_text", "kn_int_to_text", &[Int], Some(Text));
            cmd("int64_to_text", "kn_int64_to_text", &[Int64], Some(Text));
            cmd("double_to_text", "kn_double_to_text", &[Double], Some(Text));
            cmd("text_to_int", "kn_text_to_int", &[Text], Some(Int));
            cmd("text_to_double", "kn_text_to_double", &[Text], Some(Double));

            // --- Text --------------------------------------------------------
            cmd("text_eq", "kn_text_eq", &[Text, Text], Some(Bool));
            cmd("length", "kn_length", &[Text], Some(Int));
            cmd("uppercase", "kn_uppercase", &[Text], Some(Text));
            cmd("lowercase", "kn_lowercase", &[Text], Some(Text));
            cmd("trim", "kn_trim", &[Text], Some(Text));
            cmd("substr", "kn_substr", &[Text, Int, Int], Some(Text));
            cmd("find", "kn_find", &[Text, Text], Some(Int));
            cmd("replace", "kn_replace", &[Text, Text, Text], Some(Text));
            cmd("concat", "kn_concat", &[Text, Text], Some(Text));
            cmd("repeat", "kn_repeat", &[Text, Int], Some(Text));
            cmd("reverse", "kn_reverse", &[Text], Some(Text));

            // --- Date / time -------------------------------------------------
            cmd("now", "kn_now", &[], Some(Int64));
            cmd("year", "kn_year", &[Int64], Some(Int));
            cmd("format_time", "kn_format_time", &[Int64, Text], Some(Text));

            // --- Arrays ------------------------------------------------------
            // Declared over `AnyArray`/`AnyElem` rather than once per element
            // type: the array carries its element tag at run time, and the
            // checker pairs the two so `append(ints, "x")` is still an error.
            cmd("count", "kn_ary_count", &[AnyArray], Some(Int));
            cmd("append", "kn_ary_append", &[AnyArray, AnyElem], Some(AnyArray));
            cmd("remove", "kn_ary_remove", &[AnyArray, Int], None);
            cmd("sort", "kn_ary_sort", &[AnyArray], None);
            cmd("contains", "kn_ary_contains", &[AnyArray, AnyElem], Some(Bool));
            cmd("index_of", "kn_ary_index_of", &[AnyArray, AnyElem], Some(Int));
            cmd("join", "kn_ary_join", &[AnyArray, Text], Some(Text));
            cmd(
                "split",
                "kn_ary_split",
                &[Text, Text],
                Some(Array(crate::Elem::Text)),
            );
            // `slice(xs, start, count)` is what `xs[a..b]` becomes. It is a
            // command in its own right so the shorthand adds no semantics the
            // language did not already have, and so a computed run can be taken
            // without one.
            cmd("slice", "kn_ary_slice", &[AnyArray, Int, Int], Some(AnyArray));

            // --- Byte-sets ---------------------------------------------------
            cmd("bytes_new", "kn_bin_make", &[Int], Some(Bytes));
            cmd("bytes_count", "kn_bin_size", &[Bytes], Some(Int));
            cmd("bytes_at", "kn_bin_byte", &[Bytes, Int], Some(Int));
            cmd("bytes_set", "kn_bin_put", &[Bytes, Int, Int], None);
            cmd("bytes_from_text", "kn_bin_from_text", &[Text], Some(Bytes));
            cmd("text_from_bytes", "kn_bin_to_text", &[Bytes], Some(Text));
            cmd("bytes_slice", "kn_bin_slice", &[Bytes, Int, Int], Some(Bytes));

            // --- Dictionaries ------------------------------------------------
            // Declared over `AnyDict`/`AnyElem` for the reason the array
            // commands are: the dictionary carries its value tag at run time,
            // and the checker pairs the two so `dict_set(ages, "k", "x")` on an
            // `int{}` is still an error.
            //
            // `dict_get` on a key that is not there answers the sentinel for
            // its value type (0, "", false) and sets the error slot; `dict_has`
            // is the predicate that tells that apart from a stored 0.
            cmd("dict_count", "kn_dict_count", &[AnyDict], Some(Int));
            cmd("dict_has", "kn_dict_has", &[AnyDict, Text], Some(Bool));
            cmd("dict_get", "kn_dict_lookup", &[AnyDict, Text], Some(AnyElem));
            cmd("dict_set", "kn_dict_store", &[AnyDict, Text, AnyElem], None);
            cmd("dict_remove", "kn_dict_erase", &[AnyDict, Text], Some(Bool));
            cmd("dict_keys", "kn_dict_keys", &[AnyDict], Some(Array(crate::Elem::Text)));

            // --- Pointers and raw memory -------------------------------------
            // A `ptr` is an opaque 64-bit address; these are the escape hatch to
            // C. Offsets and sizes are `int64` so a buffer may exceed 2 GiB and
            // a 64-bit address round-trips whole. Reads and writes are raw: a
            // bad address faults exactly as it would in C — there is no bounds
            // check a C programmer would not have. `ptr_read_text` copies, so it
            // is the one that can answer "" for a null pointer instead of
            // faulting; every other read at null is undefined, as in C.
            cmd("ptr_null", "kn_ptr_null", &[], Some(Ptr));
            cmd("ptr_is_null", "kn_ptr_is_null", &[Ptr], Some(Bool));
            cmd("ptr_offset", "kn_ptr_offset", &[Ptr, Int64], Some(Ptr));
            cmd("ptr_from_int", "kn_ptr_from_int", &[Int64], Some(Ptr));
            cmd("ptr_to_int", "kn_ptr_to_int", &[Ptr], Some(Int64));
            cmd("ptr_read_int", "kn_ptr_read_int", &[Ptr, Int64], Some(Int));
            cmd("ptr_write_int", "kn_ptr_write_int", &[Ptr, Int64, Int], None);
            cmd("ptr_read_int64", "kn_ptr_read_int64", &[Ptr, Int64], Some(Int64));
            cmd("ptr_write_int64", "kn_ptr_write_int64", &[Ptr, Int64, Int64], None);
            cmd("ptr_read_byte", "kn_ptr_read_byte", &[Ptr, Int64], Some(Int));
            cmd("ptr_write_byte", "kn_ptr_write_byte", &[Ptr, Int64, Int], None);
            cmd("ptr_read_double", "kn_ptr_read_double", &[Ptr, Int64], Some(Double));
            cmd("ptr_write_double", "kn_ptr_write_double", &[Ptr, Int64, Double], None);
            cmd("ptr_read_ptr", "kn_ptr_read_ptr", &[Ptr, Int64], Some(Ptr));
            cmd("ptr_write_ptr", "kn_ptr_write_ptr", &[Ptr, Int64, Ptr], None);
            cmd("ptr_read_text", "kn_ptr_read_text", &[Ptr], Some(Text));
            cmd("ptr_write_text", "kn_ptr_write_text", &[Ptr, Int64, Text], None);
            cmd("ptr_of_text", "kn_ptr_of_text", &[Text], Some(Ptr));
            cmd("mem_alloc", "kn_mem_alloc", &[Int64], Some(Ptr));
            cmd("mem_free", "kn_mem_free", &[Ptr], None);
            cmd("mem_zero", "kn_mem_zero", &[Ptr, Int64], None);
            cmd("mem_copy", "kn_mem_copy", &[Ptr, Ptr, Int64], None);

            // --- Event loop --------------------------------------------------
            cmd("quit", "kn_quit", &[], None);
        }
        r
    }
}
