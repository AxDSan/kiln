//! IR validator: rejects malformed / ill-typed IR before lowering.
//!
//! Runs the shared type checker (`sema`) over every subroutine and adds the
//! structural rules it doesn't cover — entry-point presence, redefinition, and
//! (new in Phase 2) the component model: component types, property names and
//! types, and event bindings are all checked against the descriptors
//! introspected from support libraries, so a typo in a form is a compile error
//! rather than a silently missing widget.
//!
//! Collects *all* errors rather than stopping at the first.
//!
//! Every diagnostic carries a position — the line, and where it can be known
//! the columns of the name it is about — so an editor can put the squiggle
//! under the mistake rather than under the line it sits on. And where the
//! registry can tell what the author meant, the diagnostic says so: a command
//! that lives in a library the module has not used, a name one typo away from
//! a real one. A diagnostic that ends the confusion is the point; one that
//! starts it is a stack trace with better manners.

use std::collections::{HashMap, HashSet};

use crate::sema::{
    callee, check_args_labeled, check_call_through, field_type, property_desc, type_of_expr_hinted,
    type_of_expr_in, Components,
};
use crate::registry::{ComponentKind, PropertyDesc};
use crate::{
    foreach_elem_types, CmpOp, Component, Elem, Expr, Item, Module, RecordDef, Registry, Span,
    Stmt, StmtKind, Sub, Target, Ty,
};

#[derive(Debug, Clone, PartialEq)]
pub struct ValidateError {
    pub msg: String,
    /// 1-based source line; 0 when the position is not known. An editor needs
    /// this to put the squiggle in the right place.
    pub line: usize,
    /// 1-based byte columns of the offending name, `end_col` one past its last
    /// byte; both 0 when only the line is known. Bytes, not characters — the
    /// language server converts at its edge.
    pub col: usize,
    pub end_col: usize,
}
impl ValidateError {
    pub fn span(&self) -> Span {
        Span::new(self.line, self.col, self.end_col)
    }
}
impl std::fmt::Display for ValidateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Prefix the line when we know it, so a plain `{e}` in the CLI reads
        // the way a compiler diagnostic should. The column stays in the fields:
        // `line N:` is what the tests and Studio parse, and a consumer that
        // wants the range (the language server) reads it there.
        if self.line > 0 {
            write!(f, "line {}: {}", self.line, self.msg)
        } else {
            write!(f, "{}", self.msg)
        }
    }
}

/// What the validator can be told beyond the registry.
///
/// The registry holds the commands of the libraries the module `use`s — which
/// is exactly what it cannot say anything about when the author calls one from
/// a library they forgot to `use`. The caller that can see every library (the
/// language server, the CLI) fills this in; `validate` without it still names
/// every fix that needs no more than the registry.
#[derive(Debug, Clone, Default)]
pub struct Hints {
    /// Command name -> the library that declares it, for commands the module
    /// cannot see.
    pub elsewhere: HashMap<String, String>,
}

/// Validate a whole module.  `Ok(())` means the backend may assume well-formed,
/// well-typed IR.
pub fn validate(m: &Module, reg: &Registry) -> Result<(), Vec<ValidateError>> {
    validate_with(m, reg, &Hints::default())
}

/// `validate`, with what the caller knows about libraries the module has not
/// used — see [`Hints`].
pub fn validate_with(m: &Module, reg: &Registry, hints: &Hints) -> Result<(), Vec<ValidateError>> {
    validate_impl(m, reg, hints, false)
}

/// Validate a kit's declaration bundle (`.kdecl`): the same record, `dll` and
/// `const` checks a program gets, but without the entry-point and export rules
/// — a bundle declares, it does not run or export, so "a library needs a
/// subroutine" is not its contract to meet. This is what lets the loader catch
/// a malformed kit (a `dll` taking a non-`is c` record, a field of an unknown
/// type, a name that collides with itself) with the kit's own name.
pub fn validate_decls(m: &Module, reg: &Registry) -> Result<(), Vec<ValidateError>> {
    validate_impl(m, reg, &Hints::default(), true)
}

/// Validate a unit on its own, with the units it `use`s already merged in:
/// everything a program gets except the entry-point rule, which is the
/// program's to meet. This is what an editor runs on an open `unit` file.
pub fn validate_unit(m: &Module, reg: &Registry, hints: &Hints) -> Result<(), Vec<ValidateError>> {
    validate_impl(m, reg, hints, true)
}

fn validate_impl(
    m: &Module,
    reg: &Registry,
    hints: &Hints,
    decls_only: bool,
) -> Result<(), Vec<ValidateError>> {
    let mut errs: Vec<ValidateError> = Vec::new();
    // Every diagnostic carries a position (all zero when it is not known), so
    // an editor can put the squiggle where the problem is.
    let mut push = |msg: String, at: Span| {
        errs.push(ValidateError {
            msg,
            line: at.line,
            col: at.col,
            end_col: at.end_col,
        })
    };

    // The declarations are read from the module as parsed; the *bodies* are
    // read from the desugared copy below, because a named argument or an
    // inferred `let` is not something the rules further down know how to check.
    let declared: Vec<_> = m.subs().collect();
    let by_name: HashMap<&str, &Sub> = declared.iter().map(|s| (s.name.as_str(), *s)).collect();

    // Subroutines become callable names in the same pass that resolves
    // commands, so `add(1, 2)` and `length("hi")` are looked up the same way.
    // A user sub may not take a command's name: silently shadowing `length`
    // would change what every existing call in the file means.
    let mut with_subs = reg.clone();
    for name in with_subs.register_subs(m) {
        let line = by_name.get(name.as_str()).map_or(0, |s| s.line);
        push(
            format!(
                "subroutine `{name}` has the same name as a library command — \
                 rename the subroutine"
            ),
            Span::line(line),
        );
    }
    // Foreign functions are registered after subs so a `dll` that takes a sub's
    // name is the one reported (the sub is already in), and before records so a
    // record cannot silently shadow one either.
    for name in with_subs.register_dlls(m) {
        let line = m.dlls().find(|d| d.name == name).map_or(0, |d| d.line);
        push(
            format!(
                "foreign function `{name}` has the same name as a library command, a \
                 subroutine, or another `dll` — rename it"
            ),
            Span::line(line),
        );
    }
    for name in with_subs.register_records(m) {
        let line = m
            .records()
            .find(|r| r.name == name)
            .map_or(0, |r| r.line);
        push(
            format!(
                "record `{name}` has the same name as a library command, a subroutine, \
                 or another record — rename the record"
            ),
            Span::line(line),
        );
    }
    // Constants join the same flat name space last, so a `const` that clashes
    // with anything already registered — a command, a sub, a `dll`, a record or
    // a constant a kit contributed — is the one reported.
    for name in with_subs.register_consts(m) {
        let line = m.consts().find(|c| c.name == name).map_or(0, |c| c.line);
        push(
            format!(
                "constant `{name}` has the same name as a library command, a subroutine, \
                 a `dll`, a record or another constant — rename the constant"
            ),
            Span::line(line),
        );
    }
    // Named arguments, defaults, inferred `let`s and record updates are all
    // rewritten here, once, into the forms the rest of this function checks.
    let (desugared, sugar_errs) = crate::desugar::desugar(m, &with_subs);
    for e in sugar_errs {
        push(e.msg, e.span);
    }
    let m = &desugared;
    let subs: Vec<_> = m.subs().collect();
    let forms: Vec<_> = m.forms().collect();
    let sub_names: HashSet<&str> = subs.iter().map(|s| s.name.as_str()).collect();

    // Every parameter default is an expression evaluated where the *call* is
    // written, so it may not read a name: the caller's scope is not the
    // subroutine's, and a default that silently bound whatever the caller
    // happened to have in hand would be the worst kind of working.
    for sub in &subs {
        for (i, default) in sub.defaults.iter().enumerate() {
            let Some(default) = default else { continue };
            let (pname, pty) = &sub.params[i];
            if let Err(e) = check_default(default, &with_subs) {
                push(
                    format!(
                        "in `{}`: the default for `{pname}` is worked out where the call is \
                         written, so it cannot use anything from here — {e}",
                        sub.name
                    ),
                    sub.name_span,
                );
                continue;
            }
            match type_of_expr_hinted(default, Some(*pty), &HashMap::new(), &with_subs, &Components::new()) {
                Ok(got) if got == *pty => {}
                Ok(got) => push(
                    format!(
                        "in `{}`: parameter `{pname}` is {} and its default is {}",
                        sub.name,
                        pty.as_str(),
                        got.as_str()
                    ),
                    sub.name_span,
                ),
                Err(e) => push(
                    format!("in `{}`: the default for `{pname}` is not a value ({e})", sub.name),
                    sub.name_span,
                ),
            }
        }
    }


    // A module variable and a constant are both module-level bindings of one
    // name, and `register_consts` cannot see the variables (they are not in the
    // registry). Catch the clash here, where both lists are in hand, so a `var`
    // never silently shadows a kit's constant.
    {
        let const_names: HashSet<&str> = with_subs.const_names().collect();
        for g in m.globals() {
            if const_names.contains(g.name.as_str()) {
                push(
                    format!(
                        "module variable `{}` has the same name as a constant — rename one",
                        g.name
                    ),
                    Span::default(),
                );
            }
        }
    }
    let reg = &with_subs;

    // --- record declarations ---------------------------------------------
    let records: Vec<&RecordDef> = m.records().collect();
    for rec in &records {
        for (fname, fty) in &rec.fields {
            if let Some(bad) = undeclared_type(*fty, reg) {
                push(
                    format!("record `{}` field `{fname}`: unknown type `{bad}`", rec.name),
                    Span::line(rec.line),
                );
            }
        }
    }
    // A c-record's field may nest another record by value — inline, with the
    // nested struct's own alignment — but only an `is c` one: a plain record is
    // a pointer to a heap object, and a struct cannot hold one by value and
    // still be the block of bytes a C API is handed.
    for rec in records.iter().filter(|r| r.is_c) {
        for (fname, fty) in &rec.fields {
            let nested = match fty {
                Ty::Record(n) => Some(*n),
                Ty::CArray(a) => match a.elem {
                    Ty::Record(n) => Some(n),
                    _ => None,
                },
                _ => None,
            };
            let Some(n) = nested else { continue };
            match reg.record(n) {
                Some(def) if def.is_c => {}
                Some(_) => push(
                    format!(
                        "c-record `{}` field `{fname}`: `{n}` is a heap record — a c-record \
                         nests another by value only if that one is `is c` too",
                        rec.name
                    ),
                    Span::line(rec.line),
                ),
                // The unknown-type walk above has already said so.
                None => {}
            }
        }
    }
    check_record_cycles(&records, &mut push);

    // --- c-record placement ----------------------------------------------
    //
    // A c-record is a flat value with a fixed layout; it lives as a `var`
    // local, a `dll` pointer parameter, or the operand of `address of` / `size
    // of`, and nowhere the language treats a record as a heap reference. Reject
    // the heap positions here so a c-record can never reach a path that would
    // call `kn_rec_*` on a struct that is not one.
    //
    // A `dll` parameter typed as a record is the one place a record name is
    // *wanted*: it says the C prototype takes a pointer to that struct. Require
    // it to be `is c` — a heap record has no C layout to point at.
    for d in m.dlls() {
        for (pname, pty) in &d.params {
            if let Ty::Record(n) = pty {
                match reg.record(n) {
                    Some(def) if def.is_c => {}
                    Some(_) => push(
                        format!(
                            "foreign function `{}`: parameter `{pname}` is the heap record \
                             `{n}` — a `dll` takes a pointer to a C struct, so declare `{n}` \
                             `is c`",
                            d.name
                        ),
                        Span::line(d.line),
                    ),
                    None => push(
                        format!(
                            "foreign function `{}`: parameter `{pname}` has unknown type `{n}`",
                            d.name
                        ),
                        Span::line(d.line),
                    ),
                }
            }
        }
    }
    // The heap-only positions: a c-record is barred from each, with a message
    // that names the position. `find_c_record` looks through a list or a
    // dictionary element too, since `RECT[]` and `RECT{}` are heap containers.
    let mut bar = |ty: Ty, where_: &str, span: Span| {
        if let Some(n) = find_c_record(ty, reg) {
            push(
                format!(
                    "c-record `{n}` cannot be {where_} — a c-record is a flat value, not a \
                     heap object; pass `address of` it, or a `ptr`, instead"
                ),
                span,
            );
        }
    };
    for g in m.globals() {
        bar(g.ty, "a module variable", Span::default());
    }
    for sub in m.subs() {
        if let Some(t) = sub.ret {
            bar(t, "a subroutine's return", sub.name_span);
        }
        for (_, pty) in &sub.params {
            bar(*pty, "a subroutine parameter", sub.name_span);
        }
    }
    for rec in &records {
        for (fname, fty) in &rec.fields {
            // A heap record holding a c-record field, or a list/dict of them:
            // the outer record is a heap object, so its field is a reference,
            // and a c-record is not one. (An `is c` record's own fields are the
            // scalar set the parser already enforced, so this only fires on a
            // plain record.)
            if !rec.is_c {
                bar(
                    *fty,
                    &format!("the field `{fname}` of a heap record"),
                    Span::line(rec.line),
                );
            }
        }
    }

    // --- entry point -----------------------------------------------------
    // What counts as a valid entry depends on the target: a GUI module is
    // entered through its form, a console module needs `main`, and a library
    // has no entry at all — it is called by a host. A declaration bundle has no
    // entry contract at all, so the whole block is skipped for one.
    let target = m.target();
    if !decls_only && target.is_executable() {
        if forms.is_empty() && !sub_names.contains("main") {
            push("module has no `main` subroutine and no `form` (nothing to run)".into(), Span::default());
        }
        if target == Target::Console && !forms.is_empty() {
            push(
                "`target console` but the module declares a form — use `target gui`".into(),
                Span::default(),
            );
        }
        if target == Target::Gui && forms.is_empty() {
            push("`target gui` but the module declares no form".into(), Span::default());
        }
    } else if !decls_only {
        if subs.is_empty() {
            push(
                format!(
                    "`target {}` exports nothing — a library needs at least one subroutine",
                    target.as_str()
                ),
                Span::default(),
            );
        }
        if !forms.is_empty() {
            push(
                format!(
                    "`target {}` cannot declare a form — build a GUI module as `target gui`",
                    target.as_str()
                ),
                Span::default(),
            );
        }
        // A `sharedlib` may name a loader hook: `dll_attach` runs when the OS
        // maps the library (DllMain / an ELF constructor), `dll_detach` when it
        // is unloaded. The loader calls them with no arguments and ignores any
        // result, so the generated entry declares them `void(void)` — a hook
        // with a parameter or a return would be a call through a mismatched
        // signature, caught here instead.
        if target == Target::SharedLib {
            for hook in ["dll_attach", "dll_detach"] {
                if let Some(s) = by_name.get(hook) {
                    if !s.is_plain() {
                        push(
                            format!(
                                "`{hook}` is a loader hook: it takes no parameters and returns \
                                 nothing"
                            ),
                            s.name_span,
                        );
                    }
                }
            }
        }
    }
    if forms.len() > 1 {
        push(format!(
            "v0.2 supports one form per module, found {}",
            forms.len()
        ), Span::default());
    }

    // --- duplicate names -------------------------------------------------
    let mut seen: HashMap<&str, usize> = HashMap::new();
    for s in &subs {
        *seen.entry(s.name.as_str()).or_insert(0) += 1;
    }
    for (name, n) in seen {
        if n > 1 {
            push(format!("subroutine `{name}` is defined {n} times"), Span::default());
        }
    }

    // --- the no-parameter contract ---------------------------------------
    // An entry point and an event handler are both invoked by generated code
    // that has no arguments to pass and nowhere to put a result. Saying so here
    // is the difference between a compile error and a call through a mismatched
    // function pointer at run time.
    if let Some(main) = by_name.get("main") {
        if !main.is_plain() && target.is_executable() {
            push(
                "`main` is the program entry: it takes no parameters and returns nothing"
                    .into(),
                main.name_span,
            );
        }
    }

    // --- forms and components -------------------------------------------
    for form in &forms {
        let mut ids: HashSet<&str> = HashSet::new();
        check_component_like(
            reg,
            "form",
            &form.name,
            &form.properties,
            &form.property_spans,
            &form.handlers,
            &form.handler_spans,
            &by_name,
            &mut push,
        );
        for child in &form.children {
            if !ids.insert(child.id.as_str()) {
                push(format!(
                    "form `{}`: duplicate component id `{}`",
                    form.name, child.id
                ), Span::default());
            }
            check_component(reg, form.name.as_str(), child, &by_name, &mut push);
            // A form is a place to draw things. A timer inside one would have
            // to be rewritten by the designer along with the widgets it does
            // not resemble, so it belongs at module level and is told so.
            if let Some(desc) = reg.component(&child.type_name) {
                if desc.kind == ComponentKind::NonVisual {
                    push(format!(
                        "form `{}`: `{}` is not a visual component — declare it at \
                         module level, outside the form",
                        form.name, child.type_name
                    ), Span::default());
                }
            }
        }
    }

    // Module-level components: the same check, plus the one thing that differs.
    let module_components: Vec<_> = m.components().collect();
    for c in &module_components {
        check_component_like(
            reg,
            &c.type_name,
            &c.id,
            &c.properties,
            &c.property_spans,
            &c.handlers,
            &c.handler_spans,
            &by_name,
            &mut push,
        );
        if let Some(desc) = reg.component(&c.type_name) {
            if desc.kind == ComponentKind::Visual {
                push(format!(
                    "`{}`: `{}` is a visual component — it has to live inside a form",
                    c.id, c.type_name
                ), Span::default());
            }
        }
    }

    // Component ids are module-scoped: every subroutine can address them.
    let mut components: Components = Components::new();
    for form in &forms {
        for child in &form.children {
            components.insert(child.id.clone(), child.type_name.clone());
        }
    }
    for c in &module_components {
        if components.insert(c.id.clone(), c.type_name.clone()).is_some() {
            push(format!("duplicate component id `{}`", c.id), Span::default());
        }
    }

    // Module-level variables, and their types.
    let globals: Vec<_> = m.globals().collect();
    let mut global_types: HashMap<String, Ty> = HashMap::new();
    for g in &globals {
        if let Some(bad) = undeclared_type(g.ty, reg) {
            push(format!("`var {}`: unknown type `{bad}`", g.name), Span::default());
        }
        // A global's initializer may call commands but must not read another
        // global: order-dependent global initialisation is a swamp, and a clear
        // error now beats a subtle one later.
        if let Err(e) = check_initializer(&g.value, &global_types, reg) {
            push(format!("in initializer of `{}`: {e}", g.name), Span::default());
        }
        match type_of_expr_hinted(&g.value, Some(g.ty), &HashMap::new(), reg, &components) {
            Ok(got) if got == g.ty => {}
            Ok(got) => push(format!(
                "`var {}` declared {} but its initializer is {}",
                g.name,
                g.ty.as_str(),
                got.as_str()
            ), Span::default()),
            Err(e) => push(format!("in initializer of `{}`: {e}", g.name), Span::default()),
        }
        if global_types.insert(g.name.clone(), g.ty).is_some() {
            push(format!(
                "module variable `{}` is declared more than once",
                g.name
            ), Span::default());
        }
    }

    // Module variables, component ids and subroutine names share ONE namespace.
    // `count = 5` and `count.text = "x"` naming the same thing would be
    // incoherent, so a collision is an error while it is still cheap to say so.
    for name in global_types.keys() {
        if components.contains_key(name) {
            push(format!(
                "`{name}` is both a module variable and a component id"
            ), Span::default());
        }
        if sub_names.contains(name.as_str()) {
            push(format!(
                "`{name}` is both a module variable and a subroutine"
            ), Span::default());
        }
    }
    for id in components.keys() {
        if sub_names.contains(id.as_str()) {
            push(format!("`{id}` is both a component id and a subroutine"), Span::default());
        }
    }
    // A record is written in expression position, so its name is in the same
    // namespace every other callable name is in.
    for rec in &records {
        if components.contains_key(&rec.name) {
            push(
                format!("`{}` is both a record and a component id", rec.name),
                Span::line(rec.line),
            );
        }
        if global_types.contains_key(&rec.name) {
            push(
                format!("`{}` is both a record and a module variable", rec.name),
                Span::line(rec.line),
            );
        }
    }

    // --- subroutine bodies -----------------------------------------------
    for item in &m.items {
        let Item::Sub(sub) = item else { continue };
        // Module variables are in scope in every subroutine.
        let mut vars: HashMap<String, Ty> = global_types.clone();
        let mut mutable_locals: HashSet<String> = HashSet::new();
        // Locals must be tracked apart from module variables: `vars` is seeded
        // with the globals so they resolve, but they are not locals and the
        // immutability rule differs.
        let mut local_names: HashSet<String> = HashSet::new();
        // Parameters are in scope for the whole body and are immutable:
        // reassigning one would make the call site's argument a lie about what
        // the subroutine is working with.
        let mut param_names: HashSet<String> = HashSet::new();
        // Loop variables are immutable for the same reason parameters are —
        // the loop, not the body, decides what comes next — but they earn
        // their own diagnostic, because "declare it with `var`" is not advice
        // an author can act on here.
        let mut loop_vars: HashSet<String> = HashSet::new();
        if let Some(bad) = sub.ret.and_then(|t| undeclared_type(t, reg)) {
            push(
                format!("`{}` returns unknown type `{bad}`", sub.name),
                sub.name_span,
            );
        }
        for (pname, pty) in &sub.params {
            if let Some(bad) = undeclared_type(*pty, reg) {
                push(
                    format!(
                        "in `{}`: parameter `{pname}` has unknown type `{bad}`",
                        sub.name
                    ),
                    sub.name_span,
                );
            }
            if vars.insert(pname.clone(), *pty).is_some() {
                push(
                    format!(
                        "in `{}`: parameter `{pname}` has the same name as a module variable",
                        sub.name
                    ),
                    sub.name_span,
                );
            }
            local_names.insert(pname.clone());
            param_names.insert(pname.clone());
        }
        // Walk nested blocks. Locals are **function-scoped** in v0.3: a `let`
        // inside an `if` is visible after it, matching the alloca-at-top
        // lowering. Block scoping is a later refinement.
        let mut stack: Vec<&[Stmt]> = vec![&sub.body];
        while let Some(block) = stack.pop() {
            for stmt in block {
                match &stmt.kind {
                    // The desugar has already turned every `LetInfer` into a
                    // `Let` with the type it read off the initializer, or
                    // reported why it could not. Reaching one here means the
                    // module was checked without that pass, which is a wiring
                    // mistake rather than a program's.
                    StmtKind::LetInfer { name, .. } => push(
                        format!("in `{}`: the type of `{name}` was never worked out", sub.name),
                        stmt.span,
                    ),
                    StmtKind::Let {
                        name,
                        ty,
                        value,
                        mutable,
                    } => {
                        if let Some(bad) = undeclared_type(*ty, reg) {
                            push(
                                format!("in `{}`: `{name}` has unknown type `{bad}`", sub.name),
                                stmt.span,
                            );
                        }
                        // A c-record is flat storage, not a value: a local of one
                        // IS its bytes, so there is no value to keep beside a
                        // truth saying whether it is there.
                        if let Ty::Optional(Elem::Record(n)) = ty {
                            if reg.record(n).map(|d| d.is_c).unwrap_or(false) {
                                push(format!(
                                    "in `{}`: `{n}` is a c-record — flat storage, not a value — so \
                                     `{n}?` is not a type. Keep a truth value of your own beside it",
                                    sub.name
                                ), stmt.span);
                            }
                        }
                        // A c-record local is only ever zero-initialised: it has
                        // no constructor, and copying one struct into another is
                        // not modelled, so `var r: RECT = anything` is refused
                        // in favour of the bare `var r: RECT` the layout allows.
                        if find_c_record(*ty, reg).is_some()
                            && !matches!(value, Expr::ZeroInit)
                        {
                            push(format!(
                                "in `{}`: a c-record `{name}` is declared `var {name}: {}` with \
                                 no initializer (it starts zeroed); set its fields afterwards",
                                sub.name,
                                ty.as_str()
                            ), stmt.span);
                        }
                        match type_of_expr_hinted(value, Some(*ty), &vars, reg, &components) {
                            Ok(got) if got == *ty => {}
                            Ok(got) => push(format!(
                                "in `{}`: `let {name}` declared {} but expression is {}",
                                sub.name,
                                ty.as_str(),
                                got.as_str()
                            ), stmt.span),
                            Err(e) => report(&sub.name, stmt, e.to_string(), reg, hints, &vars, &mut push),
                        }
                        if vars.insert(name.clone(), *ty).is_some() {
                            push(format!(
                                "in `{}`: variable `{name}` is defined more than once",
                                sub.name
                            ), stmt.span);
                        }
                        local_names.insert(name.clone());
                        if *mutable {
                            mutable_locals.insert(name.clone());
                        }
                    }
                    StmtKind::Assign { name, value } => {
                        // Resolve against locals first, then module variables.
                        let target = vars
                            .get(name)
                            .copied()
                            .or_else(|| global_types.get(name).copied());
                        match target {
                            None => report(
                                &sub.name,
                                stmt,
                                format!("assignment to undefined variable `{name}`"),
                                reg,
                                hints,
                                &vars,
                                &mut push,
                            ),
                            Some(expected) if find_c_record(expected, reg).is_some() => {
                                // A c-record name cannot be reassigned: there is
                                // no value to give it (no constructor, and a
                                // copy of another struct is not modelled). Write
                                // its fields, or the bytes through a `ptr`.
                                push(format!(
                                    "in `{}`: `{name}` is a c-record and cannot be reassigned — \
                                     set its fields (`{name}.field = ...`) or write through \
                                     `address of {name}`",
                                    sub.name
                                ), stmt.span);
                            }
                            Some(expected) => {
                                let is_local = local_names.contains(name);
                                let is_mutable = if is_local {
                                    mutable_locals.contains(name)
                                } else {
                                    true // module variables are always `var`
                                };
                                if !is_mutable {
                                    if loop_vars.contains(name) {
                                        push(format!(
                                            "in `{}`: `{name}` is the loop variable of a `for` and cannot be assigned to",
                                            sub.name
                                        ), stmt.span);
                                    } else if param_names.contains(name) {
                                        push(format!(
                                            "in `{}`: `{name}` is a parameter and cannot be assigned to",
                                            sub.name
                                        ), stmt.span);
                                    } else {
                                        push(format!(
                                        "in `{}`: `{name}` is immutable — declare it with `var` instead of `let` to allow assignment",
                                        sub.name
                                    ), stmt.span);
                                    }
                                }
                                match type_of_expr_hinted(
                                    value, Some(expected), &vars, reg, &components,
                                ) {
                                    Ok(got) if got == expected => {}
                                    Ok(got) => push(format!(
                                        "in `{}`: `{name}` is {}, cannot assign {}",
                                        sub.name,
                                        expected.as_str(),
                                        got.as_str()
                                    ), stmt.span),
                                    Err(e) => report(&sub.name, stmt, e.to_string(), reg, hints, &vars, &mut push),
                                }
                            }
                        }
                    }
                    // `call` reaches commands and user subroutines alike; the
                    // two are resolved in one namespace, commands first.
                    // A window is drawn by the event loop, and `sys_sleep_ms`
                    // holds the loop's thread: the program is "waiting" and the
                    // window is a frozen rectangle. Refusing it here, with the
                    // alternative named, teaches the loop at the moment it has
                    // to be learned — a run-time freeze teaches nothing.
                    StmtKind::Call { cmd, .. } if cmd == "sys_sleep_ms" && !forms.is_empty() => {
                        push(
                            format!("in `{}`: {}", sub.name, SLEEP_IN_A_WINDOW),
                            stmt.ident_span(cmd).unwrap_or(stmt.span),
                        );
                    }
                    StmtKind::Call { cmd, args } => match callee(cmd, reg) {
                        None => report(
                            &sub.name,
                            stmt,
                            format!("unknown command `{cmd}`"),
                            reg,
                            hints,
                            &vars,
                            &mut push,
                        ),
                        Some((what, sig)) => {
                            if let Err(e) = check_args_labeled(
                                what, cmd, &sig.params, args, &vars, reg, &components,
                            ) {
                                report(&sub.name, stmt, e.to_string(), reg, hints, &vars, &mut push);
                            }
                        }
                    },
                    // `call through <ptr>(...)` — an indirect call for its
                    // effect. There is no name to resolve and no signature to
                    // check against: the site declares the signature, so what
                    // is checked is that the callee is an address and every
                    // argument has a C shape.
                    StmtKind::CallThrough { callee: target, args, .. } => {
                        if let Err(e) = check_call_through(target, args, &vars, reg, &components) {
                            report(&sub.name, stmt, e.to_string(), reg, hints, &vars, &mut push);
                        }
                    }
                    StmtKind::If { arms, otherwise } => {
                        for (cond, body) in arms {
                            check_condition(cond, &vars, reg, hints, &components, &sub.name, stmt, &mut push);
                            stack.push(body);
                        }
                        if let Some(body) = otherwise {
                            stack.push(body);
                        }
                    }
                    StmtKind::While { cond, body } => {
                        check_condition(cond, &vars, reg, hints, &components, &sub.name, stmt, &mut push);
                        stack.push(body);
                    }
                    // Every `when` value is checked as the comparison it will
                    // become, so the rule about what may be compared with what
                    // is stated once, in `check_condition`, and a `when "red"`
                    // against an `int` is reported in those words rather than
                    // as something about a synthesized branch. Typing the
                    // tested value once per arm costs nothing: the checker
                    // reads it, the program evaluates it once.
                    StmtKind::Match { scrutinee, arms, otherwise } => {
                        for (values, body) in arms {
                            for v in values {
                                let test = Expr::Cmp(
                                    CmpOp::Eq,
                                    Box::new(scrutinee.clone()),
                                    Box::new(v.clone()),
                                );
                                check_condition(&test, &vars, reg, hints, &components, &sub.name, stmt, &mut push);
                            }
                            stack.push(body);
                        }
                        if let Some(body) = otherwise {
                            stack.push(body);
                        }
                    }
                    StmtKind::For {
                        var,
                        start,
                        limit,
                        step: _,
                        body,
                    } => {
                        for (what, e) in [("start", start), ("limit", limit)] {
                            match type_of_expr_in(e, &vars, reg, &components) {
                                Ok(Ty::Int) => {}
                                Ok(other) => push(format!(
                                    "in `{}`: a `for` counts with `int` values — its {what} value is {}",
                                    sub.name,
                                    other.as_str()
                                ), stmt.span),
                                Err(e) => report(&sub.name, stmt, e.to_string(), reg, hints, &vars, &mut push),
                            }
                        }
                        if vars.insert(var.clone(), Ty::Int).is_some() {
                            push(format!(
                                "in `{}`: variable `{var}` is defined more than once",
                                sub.name
                            ), stmt.span);
                        }
                        local_names.insert(var.clone());
                        loop_vars.insert(var.clone());
                        stack.push(body);
                    }
                    StmtKind::ForEach { elem, value, index, coll, body } => {
                        // What the collection is decides what every binding is,
                        // so type it first. A collection that cannot be iterated
                        // is reported here, not left to the synthetic loop the
                        // backend builds out of it.
                        match type_of_expr_in(coll, &vars, reg, &components) {
                            Err(e) => report(&sub.name, stmt, e.to_string(), reg, hints, &vars, &mut push),
                            Ok(cty) => match foreach_elem_types(cty) {
                                None => push(format!(
                                    "in `{}`: `for each` iterates an array, a byte-set, a text or a \
                                     dictionary — {} is none of those",
                                    sub.name,
                                    cty.as_str()
                                ), stmt.span),
                                Some((elem_ty, value_ty)) => {
                                    // A `, VALUE` second binding reads a
                                    // dictionary's value, so it means nothing
                                    // over the one-element-at-a-time collections.
                                    if value.is_some() && value_ty.is_none() {
                                        push(format!(
                                            "in `{}`: two bindings (`k, v`) read a dictionary's key and \
                                             value — {} yields one element at a time, so drop the `, {}`",
                                            sub.name,
                                            cty.as_str(),
                                            value.as_ref().unwrap()
                                        ), stmt.span);
                                    }
                                    // The element, the optional value and the
                                    // optional index are each a fresh, immutable
                                    // local for the loop body — exactly a `for`
                                    // counter's status: in `loop_vars`, absent
                                    // from `mutable_locals`. (`value_ty.unwrap_or`
                                    // gives a misused value binding a type so the
                                    // body does not also complain it is unknown.)
                                    let mut binds: Vec<(&String, Ty)> = vec![(elem, elem_ty)];
                                    if let Some(v) = value {
                                        binds.push((v, value_ty.unwrap_or(elem_ty)));
                                    }
                                    if let Some(i) = index {
                                        binds.push((i, Ty::Int));
                                    }
                                    for (name, ty) in binds {
                                        if vars.insert(name.clone(), ty).is_some() {
                                            push(format!(
                                                "in `{}`: variable `{name}` is defined more than once",
                                                sub.name
                                            ), stmt.span);
                                        }
                                        local_names.insert(name.clone());
                                        loop_vars.insert(name.clone());
                                    }
                                    stack.push(body);
                                }
                            },
                        }
                    }
                    // Placement is checked separately: this worklist flattens
                    // the body, so it cannot tell what is inside a loop.
                    // The deferred statement is checked once, where it is
                    // written — not once per exit it is copied to, which is
                    // what the backend's expansion would have produced.
                    StmtKind::Defer(inner) => stack.push(std::slice::from_ref(inner)),
                    // The desugar turns every `if some` into an `if`; reaching
                    // one here means the module was checked without that pass.
                    StmtKind::IfSome { bind, .. } => push(
                        format!(
                            "in `{}`: the type of `{bind}` was never worked out",
                            sub.name
                        ),
                        stmt.span,
                    ),
                    StmtKind::Break | StmtKind::Continue => {}
                    StmtKind::Return { value } => match (value, sub.ret) {
                        (None, None) => {}
                        (None, Some(want)) => push(format!(
                            "in `{}`: `return` needs a value — `{}` returns {}",
                            sub.name,
                            sub.name,
                            want.as_str()
                        ), stmt.span),
                        (Some(_), None) => push(format!(
                            "in `{}`: `{}` declares no return type, so `return` cannot carry a value",
                            sub.name, sub.name
                        ), stmt.span),
                        (Some(e), Some(want)) => {
                            match type_of_expr_hinted(e, Some(want), &vars, reg, &components) {
                                Ok(got) if got == want => {}
                                Ok(got) => push(format!(
                                    "in `{}`: `{}` returns {}, but this `return` gives {}",
                                    sub.name,
                                    sub.name,
                                    want.as_str(),
                                    got.as_str()
                                ), stmt.span),
                                Err(e) => report(&sub.name, stmt, e.to_string(), reg, hints, &vars, &mut push),
                            }
                        }
                    },
                    // `xs[i] = v` changes the array, not the binding, so it is
                    // allowed on a `let` — see StmtKind::SetIndex.
                    StmtKind::SetIndex { name, index, value } => {
                        let target = vars.get(name).copied();
                        match target {
                            None => report(
                                &sub.name,
                                stmt,
                                format!("assignment to undefined variable `{name}`"),
                                reg,
                                hints,
                                &vars,
                                &mut push,
                            ),
                            // `d["k"] = v` is `dict_set` spelled as a
                            // subscript. It stores under a key rather than at a
                            // position, so it is the one subscript assignment
                            // that never fails: a key that is not there is
                            // created.
                            Some(Ty::Dict(v)) => {
                                match type_of_expr_in(index, &vars, reg, &components) {
                                    Ok(Ty::Text) => {}
                                    Ok(other) => push(format!(
                                        "in `{}`: a dictionary is keyed by text, got {}",
                                        sub.name,
                                        other.as_str()
                                    ), stmt.span),
                                    Err(e) => report(&sub.name, stmt, e.to_string(), reg, hints, &vars, &mut push),
                                }
                                match type_of_expr_hinted(
                                    value, Some(v.ty()), &vars, reg, &components,
                                ) {
                                    Ok(got) if got == v.ty() => {}
                                    Ok(got) => push(format!(
                                        "in `{}`: `{name}` holds {} values, cannot store {}",
                                        sub.name,
                                        v.as_str(),
                                        got.as_str()
                                    ), stmt.span),
                                    Err(e) => report(&sub.name, stmt, e.to_string(), reg, hints, &vars, &mut push),
                                }
                            }
                            Some(Ty::Array(_)) | Some(Ty::Bytes) => {
                                let expected = match target {
                                    Some(Ty::Array(e)) => e.ty(),
                                    // A byte is written as a number, the same
                                    // way it is read.
                                    _ => Ty::Int,
                                };
                                match type_of_expr_in(index, &vars, reg, &components) {
                                    Ok(Ty::Int) => {
                                        // A bit pattern is a number here like
                                        // any other, so `xs[0x0]` is caught too.
                                        let written = match index {
                                            Expr::IntLit(v) => Some(*v),
                                            Expr::BitsLit(v) => Some(crate::sema::bits_value(*v)),
                                            _ => None,
                                        };
                                        if let Some(v) = written.as_ref() {
                                            // Indexing counts from 1. Catching a
                                            // literal 0 here is worth the special
                                            // case: it is the one mistake every
                                            // person arriving from a 0-based
                                            // language makes, and at run time it
                                            // would only say "out of range".
                                            if *v < 1 {
                                                push(format!(
                                                    "in `{}`: index {v} is before the start of `{name}` — \
                                                     positions count from 1",
                                                    sub.name
                                                ), stmt.span);
                                            }
                                        }
                                    }
                                    Ok(other) => push(format!(
                                        "in `{}`: an index counts with `int` values, got {}",
                                        sub.name,
                                        other.as_str()
                                    ), stmt.span),
                                    Err(e) => report(&sub.name, stmt, e.to_string(), reg, hints, &vars, &mut push),
                                }
                                match type_of_expr_hinted(
                                    value, Some(expected), &vars, reg, &components,
                                ) {
                                    Ok(got) if got == expected => {}
                                    Ok(got) => push(format!(
                                        "in `{}`: `{name}` holds {} values, cannot store {}",
                                        sub.name,
                                        expected.as_str(),
                                        got.as_str()
                                    ), stmt.span),
                                    Err(e) => report(&sub.name, stmt, e.to_string(), reg, hints, &vars, &mut push),
                                }
                            }
                            Some(other) => push(format!(
                                "in `{}`: `{name}` is {} — only an array, a byte-set or a \
                                 dictionary has elements",
                                sub.name,
                                other.as_str()
                            ), stmt.span),
                        }
                    }
                    // `p.x = 5` writes a record field when `p` is a record and
                    // a component property otherwise — resolved exactly as the
                    // matching read is, or the two would disagree.
                    StmtKind::SetProperty {
                        component,
                        property,
                        value,
                    } if matches!(vars.get(component), Some(Ty::Record(_))) => {
                        let Some(Ty::Record(rec)) = vars.get(component).copied() else {
                            unreachable!("guarded above")
                        };
                        match field_type(rec, property, reg) {
                            Err(e) => report(&sub.name, stmt, e.to_string(), reg, hints, &vars, &mut push),
                            // A nested c-record and an inline array are whole
                            // blocks of storage, not values: there is nothing
                            // to evaluate on the right that could fill one.
                            // Assign their parts, or copy the bytes through
                            // `address of`.
                            Ok(Ty::Record(inner)) if reg.record(inner).map(|d| d.is_c).unwrap_or(false) => {
                                push(format!(
                                    "in `{}`: `{component}.{property}` is the nested \
                                     c-record `{inner}` — assign its own fields \
                                     (`{component}.{property}.field = ...`) or copy bytes \
                                     through `address of {component}.{property}`",
                                    sub.name
                                ), stmt.span);
                            }
                            Ok(Ty::CArray(a)) => {
                                push(format!(
                                    "in `{}`: `{component}.{property}` is an inline array \
                                     of {} element(s) — assign one at a time \
                                     (`{component}.{property}[1] = ...`) or copy bytes \
                                     through `address of {component}.{property}`",
                                    sub.name,
                                    a.count
                                ), stmt.span);
                            }
                            Ok(expected) => match type_of_expr_hinted(
                                value, Some(expected), &vars, reg, &components,
                            ) {
                                Ok(got) if got == expected => {}
                                Ok(got) => push(format!(
                                    "in `{}`: `{component}.{property}` is {}, cannot store {}",
                                    sub.name,
                                    expected.as_str(),
                                    got.as_str()
                                ), stmt.span),
                                Err(e) => report(&sub.name, stmt, e.to_string(), reg, hints, &vars, &mut push),
                            },
                        }
                    }
                    StmtKind::SetProperty {
                        component,
                        property,
                        value,
                    } => match property_desc(component, property, reg, &components) {
                        Err(e) => report(&sub.name, stmt, e.to_string(), reg, hints, &vars, &mut push),
                        Ok(prop) => match type_of_expr_in(value, &vars, reg, &components) {
                            Ok(got) if got == prop.ty => {
                                if let Some(why) = colour_literal_fault(prop, value) {
                                    push(format!(
                                        "in `{}`: `{component}.{property}` {why}",
                                        sub.name
                                    ), stmt.span);
                                }
                            }
                            Ok(got) => push(format!(
                                "in `{}`: `{component}.{property}` expects {}, got {}",
                                sub.name,
                                prop.ty.as_str(),
                                got.as_str()
                            ), stmt.span),
                            Err(e) => report(&sub.name, stmt, e.to_string(), reg, hints, &vars, &mut push),
                        },
                    },
                    // `r.pt.x = v`, `r.rgb[3] = v` — a write through a path into
                    // a c-record's flat storage. The place is typed with exactly
                    // the rules a read of it uses; what is checked *here* is
                    // that it really is a c-record path (the only storage the
                    // backend can walk) and that what lands in it is one value,
                    // not a whole nested block.
                    StmtKind::SetPlace { place, value } => {
                        let root = place_root(place);
                        let rooted_in_c_record = root
                            .and_then(|r| vars.get(r).copied())
                            .and_then(|t| match t {
                                Ty::Record(n) => reg.record(n),
                                _ => None,
                            })
                            .map(|d| d.is_c)
                            .unwrap_or(false);
                        if !rooted_in_c_record {
                            push(format!(
                                "in `{}`: `{}` is not a c-record — a multi-step \
                                 assignment target reaches into a c-record's own storage \
                                 and nothing else",
                                sub.name,
                                root.unwrap_or("this")
                            ), stmt.span);
                        } else {
                            match type_of_expr_in(place, &vars, reg, &components) {
                                Err(e) => report(&sub.name, stmt, e.to_string(), reg, hints, &vars, &mut push),
                                Ok(Ty::Record(_)) | Ok(Ty::CArray(_)) => push(format!(
                                    "in `{}`: a whole nested c-record or inline array \
                                     cannot be assigned — set its parts, or copy bytes \
                                     through `address of`",
                                    sub.name
                                ), stmt.span),
                                Ok(expected) => match type_of_expr_hinted(
                                    value, Some(expected), &vars, reg, &components,
                                ) {
                                    Ok(got) if got == expected => {}
                                    Ok(got) => push(format!(
                                        "in `{}`: that field is {}, cannot store {}",
                                        sub.name,
                                        expected.as_str(),
                                        got.as_str()
                                    ), stmt.span),
                                    Err(e) => report(&sub.name, stmt, e.to_string(), reg, hints, &vars, &mut push),
                                },
                            }
                        }
                    }
                }
            }
        }

        check_loop_control(&sub.body, false, &sub.name, &mut push);

        // A value-returning subroutine must return on every path. Falling off
        // the end would hand the caller whatever happened to be in the return
        // register — the class of bug that is hardest to see and worst to hit.
        if let Some(want) = sub.ret {
            if !always_returns(&sub.body) {
                push(
                    format!(
                        "`{}` returns {} but can reach its `end` without a `return`",
                        sub.name,
                        want.as_str()
                    ),
                    sub.name_span,
                );
            }
        }
    }

    if errs.is_empty() {
        Ok(())
    } else {
        Err(errs)
    }
}

/// Whether every path through `body` reaches a `return`.
///
/// An `if` counts only when it has an `else` **and** every branch returns: a
/// missing `else` is exactly the path that falls through. A `while` never
/// counts — its condition may be false the first time it is tested.
fn always_returns(body: &[Stmt]) -> bool {
    body.iter().any(|s| match &s.kind {
        StmtKind::Return { .. } => true,
        StmtKind::If {
            arms,
            otherwise: Some(else_body),
        } => arms.iter().all(|(_, b)| always_returns(b)) && always_returns(else_body),
        // The same rule, for the same reason: without an `else` there is a path
        // that matches nothing and falls straight through.
        StmtKind::Match {
            arms,
            otherwise: Some(else_body),
            ..
        } => arms.iter().all(|(_, b)| always_returns(b)) && always_returns(else_body),
        _ => false,
    })
}

/// `break` and `continue` mean nothing outside a loop, and a jump to nowhere
/// is worth catching here rather than in the backend. The body walk in
/// `validate` is a flat worklist and cannot carry nesting depth, so this is a
/// separate recursion.
fn check_loop_control(
    body: &[Stmt],
    in_loop: bool,
    sub: &str,
    push: &mut impl FnMut(String, Span),
) {
    for stmt in body {
        match &stmt.kind {
            StmtKind::Break | StmtKind::Continue if !in_loop => {
                let word = if matches!(stmt.kind, StmtKind::Break) {
                    "break"
                } else {
                    "continue"
                };
                push(
                    format!("in `{sub}`: `{word}` is only meaningful inside a `while` or `for`"),
                    stmt.span,
                );
            }
            StmtKind::While { body, .. }
            | StmtKind::For { body, .. }
            | StmtKind::ForEach { body, .. } => check_loop_control(body, true, sub, push),
            StmtKind::If { arms, otherwise } => {
                for (_, b) in arms {
                    check_loop_control(b, in_loop, sub, push);
                }
                if let Some(b) = otherwise {
                    check_loop_control(b, in_loop, sub, push);
                }
            }
            // A `match` is a branch, not a loop: a `break` in one of its arms
            // belongs to whatever loop encloses the `match`, exactly as it
            // would inside an `if`.
            StmtKind::Match { arms, otherwise, .. } => {
                for (_, b) in arms {
                    check_loop_control(b, in_loop, sub, push);
                }
                if let Some(b) = otherwise {
                    check_loop_control(b, in_loop, sub, push);
                }
            }
            _ => {}
        }
    }
}

/// A loop or branch condition must be a truth value.
fn check_condition(
    cond: &Expr,
    vars: &HashMap<String, Ty>,
    reg: &Registry,
    hints: &Hints,
    components: &Components,
    sub: &str,
    stmt: &Stmt,
    push: &mut impl FnMut(String, Span),
) {
    match type_of_expr_in(cond, vars, reg, components) {
        Ok(Ty::Bool) => {}
        Ok(other) => push(
            format!("in `{sub}`: condition must be a truth value, found {}", other.as_str()),
            stmt.span,
        ),
        Err(e) => report(sub, stmt, e.to_string(), reg, hints, vars, push),
    }
}

/// The refusal a windowed program meets when it reaches for `sys_sleep_ms`.
const SLEEP_IN_A_WINDOW: &str = "`sys_sleep_ms` would freeze the window — a windowed program \
does not wait, it declares a `timer` and does the work in its `on tick` handler";

/// A diagnostic about a statement, placed on the name it is about and finished
/// with the fix when one can be named.
///
/// The type checker's errors are plain text with no position, so the name is
/// read back out of the message: the first backticked word is, by convention
/// throughout `sema`, the thing the sentence is about. When the statement's
/// header does not contain it the whole header is underlined, which is what
/// every diagnostic used to get.
fn report(
    sub: &str,
    stmt: &Stmt,
    msg: String,
    reg: &Registry,
    hints: &Hints,
    vars: &HashMap<String, Ty>,
    push: &mut impl FnMut(String, Span),
) {
    let at = backticked(&msg)
        .and_then(|name| stmt.ident_span(name))
        .unwrap_or(stmt.span);
    let msg = name_the_fix(msg, reg, hints, vars);
    push(format!("in `{sub}`: {msg}"), at);
}

/// The first `` `word` `` in a message.
fn backticked(msg: &str) -> Option<&str> {
    let rest = &msg[msg.find('`')? + 1..];
    let end = rest.find('`')?;
    Some(&rest[..end])
}

/// Append the fix to a diagnostic whose fix the registry can name.
///
/// Two cases are worth the special handling. A command the module cannot see
/// because it never `use`d the library is the mistake every newcomer makes
/// once per library, and the registry of every library knows the answer. A
/// name one typo away from a real one is the other, and the only thing the
/// message can do about it is say which real one.
fn name_the_fix(msg: String, reg: &Registry, hints: &Hints, vars: &HashMap<String, Ty>) -> String {
    if let Some(name) = msg.strip_prefix("unknown command `").and_then(backtick_end) {
        if let Some(lib) = hints.elsewhere.get(name) {
            return format!("{msg} — it is in the `{lib}` library: add `use {lib}` to the module");
        }
        let callables = reg
            .names()
            .chain(reg.sub_names())
            .chain(reg.dll_names())
            .chain(reg.record_names());
        return match closest(name, callables) {
            Some(c) => format!("{msg} — did you mean `{c}`?"),
            None => msg,
        };
    }
    for prefix in [
        "use of undefined variable `",
        "assignment to undefined variable `",
    ] {
        if let Some(name) = msg.strip_prefix(prefix).and_then(backtick_end) {
            return match closest(name, vars.keys().map(String::as_str)) {
                Some(c) => format!("{msg} — did you mean `{c}`?"),
                None => msg,
            };
        }
    }
    msg
}

/// `name` of `` name`... ``: the rest of a message after a name's opening
/// backtick, cut at the closing one.
fn backtick_end(rest: &str) -> Option<&str> {
    rest.find('`').map(|end| &rest[..end])
}

/// The candidate closest to `name` by edit distance, if any is close enough
/// to be a plausible typo rather than a different word.
///
/// Compared case-insensitively, so `Print_text` finds `print_text` at distance
/// zero. The allowance grows with the length: one edit in a short name is a
/// different name, two in a long one is still a slip.
fn closest<'a>(name: &str, candidates: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    let allowance = if name.len() <= 4 { 1 } else { 2 };
    let lower = name.to_ascii_lowercase();
    let mut best: Option<(usize, &str)> = None;
    for c in candidates {
        if c == name {
            continue;
        }
        let d = edit_distance(&lower, &c.to_ascii_lowercase());
        if d > allowance {
            continue;
        }
        // Ties fall to the alphabetically first, so the answer does not depend
        // on hash-map order.
        let better = match best {
            None => true,
            Some((bd, bc)) => d < bd || (d == bd && c < bc),
        };
        if better {
            best = Some((d, c));
        }
    }
    best.map(|(_, c)| c)
}

/// Levenshtein distance over bytes, with an adjacent transposition counted as
/// one edit — `teh` is one slip away from `the`, not two.
fn edit_distance(a: &str, b: &str) -> usize {
    let a = a.as_bytes();
    let b = b.as_bytes();
    let mut prev2: Vec<usize> = Vec::new();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                cur[j] = cur[j].min(prev2[j - 2] + 1);
            }
        }
        prev2 = prev;
        prev = cur;
    }
    prev[b.len()]
}

/// A module variable's initializer may call commands but must not read another
/// module variable (see the note where this is called).
/// Whether `e` may stand as a parameter default.
///
/// A default is copied to the call and worked out *there*, so it may not read a
/// name: every name at the call belongs to the caller — a local of theirs would
/// shadow the module variable this declaration meant — and a parameter of this
/// subroutine does not exist there at all. A constant is fine, because it is a
/// literal with a name and folds to the literal. A *call* is fine too: a call
/// written in a default means at the call site exactly what it would mean typed
/// there, which is the whole bargain of the desugar.
fn check_default(e: &Expr, reg: &Registry) -> Result<(), String> {
    let go = |e: &Expr| check_default(e, reg);
    match e {
        Expr::Var(n) if reg.is_const(n) => Ok(()),
        Expr::Var(n) => Err(format!(
            "`{n}` is a name, and the names where the call is written are the caller's"
        )),
        Expr::GetProperty {
            component,
            property,
        } => Err(format!(
            "`{component}.{property}` is read where the call is written, not here"
        )),
        Expr::AddressOf(n) => Err(format!("`address of {n}` cannot be a default")),
        Expr::Bin(_, l, r)
        | Expr::Cmp(_, l, r)
        | Expr::Logical(_, l, r)
        | Expr::Bit(_, l, r) => go(l).and_then(|_| go(r)),
        Expr::Not(x) | Expr::BitNot(x) | Expr::Neg(x) | Expr::ToText { value: x, .. } => go(x),
        Expr::IfElse { cond, then, els } => go(cond).and_then(|_| go(then)).and_then(|_| go(els)),
        Expr::Otherwise { value, fallback } => go(value).and_then(|_| go(fallback)),
        // A comprehension reads its own bindings, and those are names the call
        // site does not have.
        // An optional is made by a declaration, and a default is not one.
        Expr::NoneLit => Err("`none` is the absence of a value a declaration named".to_string()),
        Expr::HasValue(x) | Expr::Unwrap(x) => go(x),
        Expr::Comprehension { .. } => Err(
            "a list built by a loop cannot be a default: the loop's bindings are names, and the \
             names where the call is written are the caller's"
                .to_string(),
        ),
        Expr::In { needle, haystack, .. } => go(needle).and_then(|_| go(haystack)),
        Expr::Chain { lo, mid, hi, .. } => go(lo).and_then(|_| go(mid)).and_then(|_| go(hi)),
        Expr::Index { base, index } => go(base).and_then(|_| go(index)),
        Expr::Field { base, .. } => go(base),
        Expr::Slice { base, from, to } => {
            go(base)?;
            for part in [from, to].into_iter().flatten() {
                check_default(part, reg)?;
            }
            Ok(())
        }
        Expr::Call { args, .. } | Expr::CallThrough { args, .. } => {
            for a in args {
                check_default(a, reg)?;
            }
            Ok(())
        }
        Expr::ArrayLit(items) => {
            for i in items {
                check_default(i, reg)?;
            }
            Ok(())
        }
        Expr::DictLit(pairs) => {
            for (k, v) in pairs {
                check_default(k, reg)?;
                check_default(v, reg)?;
            }
            Ok(())
        }
        Expr::RecordLit { fields, .. } => {
            for (_, v) in fields {
                check_default(v, reg)?;
            }
            Ok(())
        }
        Expr::RecordUpdate { base, .. } => Err(format!(
            "a record update copies from a name, and `{}` is not one here",
            match &**base {
                Expr::Var(n) => n.clone(),
                _ => "it".to_string(),
            }
        )),
        Expr::Labeled { value, .. } => go(value),
        Expr::IntLit(_)
        | Expr::BitsLit(_)
        | Expr::DoubleLit(_)
        | Expr::TextLit(_)
        | Expr::BoolLit(_)
        | Expr::SizeOf(_)
        | Expr::ZeroInit => Ok(()),
    }
}

fn check_initializer(
    e: &Expr,
    globals: &HashMap<String, Ty>,
    reg: &Registry,
) -> Result<(), String> {
    match e {
        Expr::Var(name) if globals.contains_key(name) => Err(format!(
            "cannot read module variable `{name}` here; module variable initializers may use literals and command calls only"
        )),
        // A constant is a literal with a name: it has no storage, no
        // initialisation order, and folds where it is written — so it is
        // allowed here for exactly the reason a bare number is. This is what
        // lets a module variable be seeded from a kit's `const`
        // (`var flags: int = MB_OK`).
        Expr::Var(name) if reg.const_(name).is_some() => Ok(()),
        Expr::Var(name) => Err(format!("unknown variable `{name}`")),
        Expr::Bin(_, l, r) => {
            check_initializer(l, globals, reg)?;
            check_initializer(r, globals, reg)
        }
        Expr::Neg(e) | Expr::Not(e) | Expr::BitNot(e) => check_initializer(e, globals, reg),
        Expr::Cmp(_, l, r) | Expr::Logical(_, l, r) | Expr::Bit(_, l, r) => {
            check_initializer(l, globals, reg)?;
            check_initializer(r, globals, reg)
        }
        // A subroutine body may read module variables, so calling one here
        // would reintroduce exactly the ordering hazard this rule exists to
        // prevent — just through one more level of indirection.
        Expr::Call { cmd, .. } if reg.is_sub(cmd) => Err(format!(
            "cannot call subroutine `{cmd}` here; module variable initializers may use literals and command calls only"
        )),
        Expr::Call { args, .. } => {
            for a in args {
                check_initializer(a, globals, reg)?;
            }
            Ok(())
        }
        Expr::ArrayLit(items) => {
            for a in items {
                check_initializer(a, globals, reg)?;
            }
            Ok(())
        }
        Expr::RecordLit { fields, .. } => {
            for (_, v) in fields {
                check_initializer(v, globals, reg)?;
            }
            Ok(())
        }
        Expr::DictLit(pairs) => {
            for (k, v) in pairs {
                check_initializer(k, globals, reg)?;
                check_initializer(v, globals, reg)?;
            }
            Ok(())
        }
        Expr::Field { base, .. } => check_initializer(base, globals, reg),
        Expr::Index { base, index } => {
            check_initializer(base, globals, reg)?;
            check_initializer(index, globals, reg)
        }
        Expr::Slice { base, from, to } => {
            check_initializer(base, globals, reg)?;
            for b in [from, to].into_iter().flatten() {
                check_initializer(b, globals, reg)?;
            }
            Ok(())
        }
        Expr::GetProperty { .. } => {
            Err("cannot read a component property before the form exists".to_string())
        }
        // The address to call comes from somewhere — a `GetProcAddress`, a
        // `dlsym`, a vtable — and nothing has run yet at module-initialiser
        // time, so the pointer can only be null here. Refusing it names that,
        // rather than leaving a call through address zero to fault at start-up.
        Expr::CallThrough { .. } => Err(
            "cannot make an indirect call here; module variable initializers may use literals \
             and command calls only"
                .to_string(),
        ),
        // An interpolation hole in a module-variable initializer: its value
        // faces the same restriction the rest of the initializer does, since the
        // hole desugars to a `concat` of `*_to_text` calls run at start-up.
        Expr::ToText { value, .. } => check_initializer(value, globals, reg),
        // A conditional and a fallback are ordinary expressions here: each side
        // faces exactly the restriction the rest of the initializer does.
        Expr::IfElse { cond, then, els } => {
            check_initializer(cond, globals, reg)?;
            check_initializer(then, globals, reg)?;
            check_initializer(els, globals, reg)
        }
        Expr::Otherwise { value, fallback } => {
            check_initializer(value, globals, reg)?;
            check_initializer(fallback, globals, reg)
        }
        _ => Ok(()),
    }
}

/// The record name in `ty` that no `record` declaration defines, if any.
///
/// The parser reads any unrecognised word in type position as a record name,
/// because a record may be declared after the subroutine that uses it. This is
/// where a word that names nothing is caught — and it has to run at every
/// declaration site, since a misspelt type reaching the backend is a crash
/// rather than a diagnostic.
/// The c-record name reachable in `ty` — the type itself, or the element of a
/// list or dictionary of them — or `None` if none is. Used to bar a c-record
/// from the heap positions where a record is a reference.
fn find_c_record(ty: Ty, reg: &Registry) -> Option<&'static str> {
    let name = match ty {
        Ty::Record(n) => n,
        Ty::Array(Elem::Record(n)) | Ty::Dict(Elem::Record(n)) => n,
        Ty::CArray(a) => return find_c_record(a.elem, reg),
        _ => return None,
    };
    match reg.record(name) {
        Some(def) if def.is_c => Some(name),
        _ => None,
    }
}

/// The variable a place expression is rooted at: `r` in `r.pt.x` and in
/// `r.rgb[3]`. `None` when the chain does not start at a plain name.
fn place_root(place: &Expr) -> Option<&str> {
    match place {
        Expr::Var(n) => Some(n),
        Expr::GetProperty { component, .. } => Some(component),
        Expr::Field { base, .. } | Expr::Index { base, .. } => place_root(base),
        _ => None,
    }
}

fn undeclared_type(ty: Ty, reg: &Registry) -> Option<&'static str> {
    let name = match ty {
        Ty::Record(n) => n,
        Ty::Array(Elem::Record(n)) | Ty::Dict(Elem::Record(n)) | Ty::Optional(Elem::Record(n)) => n,
        // An inline array's element is a type like any other: `Point[4]` is
        // unknown exactly when `Point` is.
        Ty::CArray(a) => return undeclared_type(a.elem, reg),
        _ => return None,
    };
    if reg.record(name).is_some() {
        None
    } else {
        Some(name)
    }
}

/// Reject a record that contains itself.
///
/// Only a *direct* field counts: a record is built with every field given at
/// once and there is no empty value a field could hold in the meantime, so a
/// record whose field is itself can never be constructed. A field that is a
/// LIST of the same record is fine — `[]` is a real value — and a tree is
/// exactly what that spelling is for.
///
/// The walk is an explicit stack, not recursion: the graph being checked is
/// the one that might contain a cycle, so the checker must not be the thing
/// that follows it forever.
fn check_record_cycles(records: &[&RecordDef], push: &mut impl FnMut(String, Span)) {
    let by_name: HashMap<&str, &RecordDef> =
        records.iter().map(|r| (r.name.as_str(), *r)).collect();
    for rec in records {
        // Depth-first from this record, following direct record fields only.
        // `seen` bounds the walk to each record once, so a cycle anywhere in
        // the graph ends the search instead of extending it.
        let mut seen: HashSet<&str> = HashSet::new();
        let mut stack: Vec<&str> = vec![rec.name.as_str()];
        let mut cycles = false;
        while let Some(cur) = stack.pop() {
            let Some(def) = by_name.get(cur) else { continue };
            for (_, fty) in &def.fields {
                // An inline array of records nests by value just as a bare
                // field does — `a: A[2]` inside `A` is as impossible as `a: A`.
                let next: &'static str = match fty {
                    Ty::Record(n) => n,
                    Ty::CArray(a) => match a.elem {
                        Ty::Record(n) => n,
                        _ => continue,
                    },
                    _ => continue,
                };
                let next = &next;
                if *next == rec.name.as_str() {
                    cycles = true;
                    break;
                }
                if seen.insert(next) {
                    stack.push(next);
                }
            }
            if cycles {
                break;
            }
        }
        if cycles {
            push(
                format!(
                    "record `{}` contains itself — every field is given a value when the \
                     record is made, so this one could never be built. A list of them \
                     (`{}[]`) can.",
                    rec.name, rec.name
                ),
                Span::line(rec.line),
            );
        }
    }
}

/// `int, text` — a parameter list for a diagnostic that shows two of them.
fn type_list(types: &[Ty]) -> String {
    types
        .iter()
        .map(|t| t.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn check_component(
    reg: &Registry,
    form: &str,
    c: &Component,
    subs: &HashMap<&str, &Sub>,
    push: &mut impl FnMut(String, Span),
) {
    let where_ = format!("{}.{}", form, c.id);
    check_component_like(reg, &c.type_name, &where_, c.properties.as_slice(), &c.property_spans, &c.handlers, &c.handler_spans, subs, push);
}

/// ` — did you mean `x`?`, or nothing.
fn did_you_mean<'a>(name: &str, candidates: impl Iterator<Item = &'a str>) -> String {
    match closest(name, candidates) {
        Some(c) => format!(" — did you mean `{c}`?"),
        None => String::new(),
    }
}

/// Why a literal written into a colour property is not a colour, or `None`.
///
/// A colour property is text as far as the type system goes, so `"#44444"` —
/// five digits, one short — type-checks and then fails at run time in whatever
/// way the renderer fails, usually by painting nothing and saying nothing.
/// Only a literal is checked: a variable or a call is whatever it turns out to
/// be, and guessing about it would either miss it or cry wolf. The descriptor's
/// editor hint, not the property name, decides what is a colour — that is the
/// one place a library says so, and the inspector reads the same word.
fn colour_literal_fault(prop: &PropertyDesc, value: &Expr) -> Option<String> {
    if prop.editor != "color" {
        return None;
    }
    let Expr::TextLit(text) = value else {
        return None;
    };
    let is_colour = text.strip_prefix('#').is_some_and(|hex| {
        matches!(hex.len(), 3 | 6 | 8) && hex.bytes().all(|b| b.is_ascii_hexdigit())
    });
    if is_colour {
        None
    } else {
        Some(format!(
            "value \"{text}\" is not a colour (use #rgb, #rrggbb or #rrggbbaa)"
        ))
    }
}

/// Shared checking for a form root or a child component: the type must exist,
/// every property must be declared by that type with a matching value type, and
/// every event must exist and bind to a real subroutine.
///
/// `property_spans` and `handler_spans` run parallel to `properties` and
/// `handlers`; a missing entry (a component built by hand rather than parsed)
/// falls back to no position.
#[allow(clippy::too_many_arguments)]
fn check_component_like(
    reg: &Registry,
    type_name: &str,
    where_: &str,
    properties: &[(String, Expr)],
    property_spans: &[Span],
    handlers: &[(String, String)],
    handler_spans: &[Span],
    subs: &HashMap<&str, &Sub>,
    push: &mut impl FnMut(String, Span),
) {
    let Some(desc) = reg.component(type_name) else {
        let mut known: Vec<&str> = reg.component_names().collect();
        known.sort_unstable();
        push(format!(
            "`{where_}`: unknown component type `{type_name}`{}",
            if known.is_empty() {
                " (no component library is in scope — add `use ui`)".to_string()
            } else {
                let hint = did_you_mean(type_name, known.iter().copied());
                format!("{hint} (known: {})", known.join(", "))
            }
        ), Span::default());
        return;
    };

    let empty = HashMap::new();
    for (i, (name, value)) in properties.iter().enumerate() {
        let at = property_spans.get(i).copied().unwrap_or_default();
        let Some(prop) = desc.property(name) else {
            let mut known: Vec<&str> = desc.properties.iter().map(|p| p.name.as_str()).collect();
            known.sort_unstable();
            push(format!(
                "`{where_}`: component `{type_name}` has no property `{name}`{} (has: {})",
                did_you_mean(name, known.iter().copied()),
                known.join(", ")
            ), at);
            continue;
        };
        match type_of_expr_in(value, &empty, reg, &Components::new()) {
            Ok(got) if got == prop.ty => {
                if let Some(why) = colour_literal_fault(prop, value) {
                    push(format!("`{where_}`: property `{name}` {why}"), at);
                }
            }
            Ok(got) => push(format!(
                "`{where_}`: property `{name}` expects {}, got {}",
                prop.ty.as_str(),
                got.as_str()
            ), at),
            Err(e) => push(format!("`{where_}`: property `{name}`: {e}"), at),
        }
    }

    for (i, (event, handler)) in handlers.iter().enumerate() {
        let at = handler_spans.get(i).copied().unwrap_or_default();
        if !desc.has_event(event) {
            let known = desc.events.join(", ");
            push(format!(
                "`{where_}`: component `{type_name}` has no event `{event}`{}{}",
                did_you_mean(event, desc.events.iter().map(String::as_str)),
                if known.is_empty() {
                    String::new()
                } else {
                    format!(" (has: {known})")
                }
            ), at);
        }
        let wants = reg.event_params(type_name, event);
        match subs.get(handler.as_str()) {
            None => push(format!(
                "`{where_}`: event `{event}` is bound to `{handler}`, which is not a subroutine in this module{}",
                did_you_mean(handler, subs.keys().copied()),
            ), at),
            // Nothing calls a handler for its answer, so a return type is
            // wrong whatever the event hands over.
            Some(sub) if sub.ret.is_some() => push(format!(
                "`{where_}`: event `{event}` is bound to `{handler}`, which returns a value — an event handler returns nothing"
            ), sub.name_span),
            // Either the handler asks for what the event hands it, or it asks
            // for nothing. Taking nothing is not a concession to old code: an
            // event that reports something a handler does not need should not
            // force the handler to name it.
            Some(sub) if sub.params.is_empty() => {}
            Some(sub)
                if sub.params.len() == wants.len()
                    && sub.params.iter().zip(wants).all(|((_, got), w)| got == w) => {}
            // Both signatures, in the shape each is written: the event's as
            // the parameter list a handler would declare, the handler's as it
            // was declared. The fix is in the subroutine, so that is what the
            // position points at.
            Some(sub) => push(format!(
                "`{where_}`: event `{event}` hands a handler ({}), but `{handler}` takes ({}) — take exactly those, or none: `sub {handler}({})`",
                type_list(wants),
                type_list(&sub.params.iter().map(|(_, t)| *t).collect::<Vec<_>>()),
                param_list(wants),
            ), sub.name_span),
        }
    }
}

/// `n: int, s: text` — a parameter list a handler could paste. Events carry
/// types only, so the names are made up from the types; the author renames
/// them in the same keystroke they would have spent typing them.
pub fn param_list(types: &[Ty]) -> String {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    let mut out = Vec::new();
    for t in types {
        let stem = match t {
            Ty::Int | Ty::Int64 => "n",
            Ty::Double => "x",
            Ty::Text => "s",
            Ty::Bool => "flag",
            Ty::Bytes => "data",
            _ => "value",
        };
        let seen = counts.entry(stem).or_insert(0);
        *seen += 1;
        let name = if *seen == 1 { stem.to_string() } else { format!("{stem}{seen}") };
        out.push(format!("{name}: {}", t.as_str()));
    }
    out.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;

    fn reg() -> Registry {
        Registry::core()
    }

    #[test]
    fn accepts_valid() {
        let m = parse(
            "module m\nsub main\n  let s: text = uppercase(\"hi\")\n  let n: int = length(s)\n  call print_int(n)\nend\n",
        )
        .unwrap();
        assert!(validate(&m, &reg()).is_ok());
    }

    #[test]
    fn rejects_missing_entry() {
        let m = parse("module m\nsub other\n  call print_int(1)\nend\n").unwrap();
        let e = validate(&m, &reg()).unwrap_err();
        assert!(e.iter().any(|e| e.msg.contains("no `main`")));
    }

    #[test]
    fn allows_multiple_subs() {
        // Handlers are subroutines, so multi-sub modules must validate.
        let m = parse(
            "module m\nsub main\n  call print_int(1)\nend\nsub helper\n  call print_int(2)\nend\n",
        )
        .unwrap();
        assert!(validate(&m, &reg()).is_ok(), "{:?}", validate(&m, &reg()));
    }

    #[test]
    fn rejects_type_mismatch_and_unknown_cmd() {
        let m =
            parse("module m\nsub main\n  let x: int = \"nope\"\n  call frob(1)\nend\n").unwrap();
        assert!(validate(&m, &reg()).unwrap_err().len() >= 2);
    }

    #[test]
    fn rejects_void_in_expression() {
        let m = parse("module m\nsub main\n  let x: int = print_int(1)\nend\n").unwrap();
        assert!(validate(&m, &reg()).is_err());
    }

    /// `address of` a subroutine with a C-representable signature yields a `ptr`.
    #[test]
    fn accepts_address_of_a_c_representable_sub() {
        let m = parse(
            "module m\nsub cb(a: int, b: int): int\n  return a + b\nend\n\
             sub main\n  var p: ptr = address of cb\n  call print_int(1)\nend\n",
        )
        .unwrap();
        assert!(validate(&m, &reg()).is_ok(), "{:?}", validate(&m, &reg()));
    }

    /// A sub the C ABI cannot represent is rejected, and the diagnostic names
    /// the sub and the offending type — you learn it before you run anything.
    #[test]
    fn rejects_address_of_a_non_c_representable_sub() {
        let m = parse(
            "module m\nsub cb(xs: int[])\n  call print_int(1)\nend\n\
             sub main\n  var p: ptr = address of cb\n  call print_int(1)\nend\n",
        )
        .unwrap();
        let e = validate(&m, &reg()).unwrap_err();
        assert!(
            e.iter().any(|e| e.msg.contains("`cb`") && e.msg.contains("int[]")),
            "the error must name the sub and its bad type: {e:?}"
        );
    }

    /// Taking the address of a name that is not a subroutine is rejected.
    #[test]
    fn rejects_address_of_a_non_sub() {
        let m = parse(
            "module m\nsub main\n  var p: ptr = address of print_int\n  call print_int(1)\nend\n",
        )
        .unwrap();
        let e = validate(&m, &reg()).unwrap_err();
        assert!(
            e.iter().any(|e| e.msg.contains("print_int")),
            "the error must name what `print_int` actually is: {e:?}"
        );
    }

    #[test]
    fn rejects_reassigning_a_let() {
        let m = parse("module m\nsub main\n  let x: int = 1\n  x = 2\nend\n").unwrap();
        let e = validate(&m, &reg()).unwrap_err();
        assert!(
            e.iter()
                .any(|e| e.msg.contains("immutable") && e.msg.contains("var")),
            "the error should name the fix: {e:?}"
        );
    }

    /// A diagnostic without a position is useless to an editor: it either has
    /// no squiggle or squiggles the wrong line. Pin the exact lines so the
    /// language server can trust them.
    #[test]
    fn reports_the_line_of_the_offending_statement() {
        //            1         2          3                4        5             6
        let src = "module m\nsub main\n  let x: int = 1\n  x = 2\n  call nope()\nend\n";
        let m = parse(src).unwrap();
        let e = validate(&m, &reg()).unwrap_err();

        let immutable = e.iter().find(|e| e.msg.contains("immutable")).unwrap();
        assert_eq!(immutable.line, 4, "the assignment is on line 4: {e:?}");

        let unknown = e.iter().find(|e| e.msg.contains("unknown command")).unwrap();
        assert_eq!(unknown.line, 5, "the call is on line 5: {e:?}");

        // And the position must survive into the rendered message.
        assert!(
            immutable.to_string().starts_with("line 4:"),
            "Display should carry the position: {immutable}"
        );
    }

    #[test]
    fn accepts_assigning_a_var() {
        let m = parse("module m\nsub main\n  var x: int = 1\n  x = 2\nend\n").unwrap();
        assert!(validate(&m, &reg()).is_ok());
    }

    #[test]
    fn rejects_global_reading_another_global() {
        let m =
            parse("module m\nvar a: int = 1\nvar b: int = a\nsub main\n  call print_int(b)\nend\n")
                .unwrap();
        assert!(validate(&m, &reg()).is_err());
    }

    #[test]
    fn rejects_assigning_undefined() {
        let m = parse("module m\nsub main\n  nope = 1\nend\n").unwrap();
        assert!(validate(&m, &reg()).is_err());
    }

    #[test]
    fn globals_are_visible_in_subs() {
        let m =
            parse("module m\nvar n: int = 5\nsub main\n  n = n + 1\n  call print_int(n)\nend\n")
                .unwrap();
        assert!(validate(&m, &reg()).is_ok(), "{:?}", validate(&m, &reg()));
    }

    #[test]
    fn rejects_non_bool_condition() {
        let m = parse("module m\nsub main\n  if 5\n    call print_int(1)\n  end\nend\n").unwrap();
        let e = validate(&m, &reg()).unwrap_err();
        assert!(e.iter().any(|e| e.msg.contains("truth value")), "{e:?}");
    }

    #[test]
    fn rejects_ordering_on_text() {
        let m = parse(
            "module m\nsub main\n  let t: text = \"a\"\n  if t < \"b\"\n    call print_int(1)\n  end\nend\n",
        )
        .unwrap();
        assert!(validate(&m, &reg()).is_err());
    }

    #[test]
    fn accepts_text_equality_and_while() {
        let m = parse(
            "module m\nsub main\n  var n: int = 0\n  while n < 3\n    n = n + 1\n  end\n  if \"a\" = \"a\"\n    call print_int(n)\n  end\nend\n",
        )
        .unwrap();
        assert!(validate(&m, &reg()).is_ok(), "{:?}", validate(&m, &reg()));
    }

    // --- subroutine parameters, returns and calls -------------------------

    fn errors(src: &str) -> Vec<ValidateError> {
        validate(&parse(src).unwrap(), &reg()).unwrap_err()
    }
    fn accepts(src: &str) {
        let m = parse(src).unwrap();
        assert!(validate(&m, &reg()).is_ok(), "{:?}", validate(&m, &reg()));
    }

    #[test]
    fn accepts_a_sub_with_parameters_and_a_return() {
        accepts(
            "module m\nsub add(a: int, b: int): int\n  return a + b\nend\nsub main\n  call print_int(add(1, 2))\nend\n",
        );
    }

    #[test]
    fn accepts_recursion() {
        accepts(
            "module m\nsub fib(n: int): int\n  if n < 2\n    return n\n  end\n  return fib(n - 1) + fib(n - 2)\nend\nsub main\n  call print_int(fib(10))\nend\n",
        );
    }

    #[test]
    fn rejects_wrong_argument_count_with_a_line() {
        //            1         2                      3                4        5
        let src = "module m\nsub add(a: int, b: int): int\n  return a + b\nend\nsub main\n  call print_int(add(1))\nend\n";
        let e = errors(src);
        let bad = e
            .iter()
            .find(|e| e.msg.contains("expects 2 argument(s), got 1"))
            .unwrap_or_else(|| panic!("{e:?}"));
        assert!(bad.msg.contains("subroutine `add`"), "{bad:?}");
        assert_eq!(bad.line, 6, "the call is on line 6: {e:?}");
    }

    #[test]
    fn rejects_wrong_argument_type() {
        let e = errors(
            "module m\nsub add(a: int, b: int): int\n  return a + b\nend\nsub main\n  call print_int(add(1, \"two\"))\nend\n",
        );
        assert!(
            e.iter()
                .any(|e| e.msg.contains("argument 2 expects int, got text")),
            "{e:?}"
        );
    }

    #[test]
    fn rejects_a_missing_return() {
        let e = errors(
            "module m\nsub pick(n: int): int\n  if n > 0\n    return 1\n  end\nend\nsub main\n  call print_int(pick(1))\nend\n",
        );
        let bad = e
            .iter()
            .find(|e| e.msg.contains("without a `return`"))
            .unwrap_or_else(|| panic!("{e:?}"));
        assert_eq!(bad.line, 2, "the subroutine header is on line 2");
    }

    /// An `if`/`else` where both sides return is a complete path — refusing it
    /// would force a pointless trailing `return`.
    #[test]
    fn an_if_else_that_both_return_is_enough() {
        accepts(
            "module m\nsub sign(n: int): int\n  if n > 0\n    return 1\n  else\n    return 0\n  end\nend\nsub main\n  call print_int(sign(3))\nend\n",
        );
    }

    /// A `while` may never run, so it cannot be the thing that returns.
    #[test]
    fn a_while_does_not_count_as_returning() {
        let e = errors(
            "module m\nsub f(n: int): int\n  while n > 0\n    return 1\n  end\nend\nsub main\n  call print_int(f(1))\nend\n",
        );
        assert!(e.iter().any(|e| e.msg.contains("without a `return`")), "{e:?}");
    }

    #[test]
    fn rejects_a_return_of_the_wrong_type() {
        let e = errors(
            "module m\nsub f(): int\n  return \"nope\"\nend\nsub main\n  call print_int(f())\nend\n",
        );
        assert!(
            e.iter().any(|e| e.msg.contains("returns int, but this `return` gives text")),
            "{e:?}"
        );
    }

    #[test]
    fn rejects_a_value_from_a_sub_that_returns_nothing() {
        let e = errors("module m\nsub main\n  return 1\nend\n");
        assert!(
            e.iter().any(|e| e.msg.contains("declares no return type")),
            "{e:?}"
        );
    }

    #[test]
    fn rejects_a_void_sub_used_as_a_value() {
        let e = errors("module m\nsub greet(who: text)\n  call print_text(who)\nend\nsub main\n  let x: int = greet(\"a\")\nend\n");
        assert!(
            e.iter()
                .any(|e| e.msg.contains("subroutine `greet` returns nothing")),
            "{e:?}"
        );
    }

    /// A user sub must not silently take a library command's name: every
    /// existing call to `length` would quietly mean something else.
    #[test]
    fn rejects_a_sub_that_shadows_a_command() {
        let e = errors("module m\nsub length(s: text): int\n  return 0\nend\nsub main\n  call print_int(length(\"a\"))\nend\n");
        let bad = e
            .iter()
            .find(|e| e.msg.contains("same name as a library command"))
            .unwrap_or_else(|| panic!("{e:?}"));
        assert_eq!(bad.line, 2);
    }

    #[test]
    fn a_constant_is_usable_where_a_literal_is() {
        // A const folds to its literal, so it types like one in a comparison,
        // an argument and arithmetic.
        accepts(
            "module m\nconst ANSWER = 42\nsub main\n  if ANSWER = 42\n    call print_int(ANSWER + 1)\n  end\nend\n",
        );
    }

    #[test]
    fn rejects_a_constant_that_shadows_a_command() {
        let e = errors("module m\nconst length = 3\nsub main\nend\n");
        assert!(
            e.iter().any(|e| e.msg.contains("constant `length`") && e.msg.contains("library command")),
            "a const may not take a command's name: {e:?}"
        );
    }

    #[test]
    fn rejects_a_module_variable_and_a_constant_of_one_name() {
        let e = errors("module m\nconst N = 1\nvar N: int = 2\nsub main\nend\n");
        assert!(
            e.iter().any(|e| e.msg.contains("same name as a constant")),
            "a var and a const of one name must clash: {e:?}"
        );
    }

    #[test]
    fn rejects_assigning_to_a_parameter() {
        let e = errors("module m\nsub f(a: int): int\n  a = 2\n  return a\nend\nsub main\n  call print_int(f(1))\nend\n");
        assert!(
            e.iter().any(|e| e.msg.contains("is a parameter and cannot be assigned to")),
            "{e:?}"
        );
    }

    #[test]
    fn rejects_an_entry_point_with_parameters() {
        let e = errors("module m\nsub main(a: int)\n  call print_int(a)\nend\n");
        assert!(
            e.iter().any(|e| e.msg.contains("`main` is the program entry")),
            "{e:?}"
        );
    }

    #[test]
    fn rejects_calling_a_sub_from_a_global_initializer() {
        let e = errors("module m\nvar n: int = f()\nsub f(): int\n  return 1\nend\nsub main\n  call print_int(n)\nend\n");
        assert!(
            e.iter().any(|e| e.msg.contains("cannot call subroutine `f` here")),
            "{e:?}"
        );
    }

    // --- loops, negation and the text `+` -------------------------------

    #[test]
    fn accepts_for_break_and_continue() {
        accepts("module m\nsub main\n  for i = 1 to 10\n    if i = 3\n      continue\n    end\n    if i = 8\n      break\n    end\n    call print_int(i)\n  end\nend\n");
    }

    #[test]
    fn rejects_break_outside_a_loop() {
        let e = errors("module m\nsub main\n  break\nend\n");
        assert!(
            e.iter()
                .any(|e| e.msg.contains("`break` is only meaningful inside") && e.line == 3),
            "{e:?}"
        );
    }

    /// A `break` in an `if` that is itself inside a loop is fine; the same
    /// `if` outside one is not. The placement check must follow branches
    /// without treating them as loops.
    #[test]
    fn a_break_inside_an_if_inherits_the_enclosing_loop() {
        accepts("module m\nsub main\n  while true\n    if true\n      break\n    end\n  end\nend\n");
        let e = errors("module m\nsub main\n  if true\n    continue\n  end\nend\n");
        assert!(
            e.iter().any(|e| e.msg.contains("`continue` is only meaningful")),
            "{e:?}"
        );
    }

    #[test]
    fn rejects_assigning_to_a_loop_variable() {
        let e = errors("module m\nsub main\n  for i = 1 to 3\n    i = 9\n  end\nend\n");
        assert!(
            e.iter()
                .any(|e| e.msg.contains("is the loop variable of a `for`")),
            "{e:?}"
        );
    }

    #[test]
    fn rejects_a_non_integer_loop_bound() {
        let e = errors("module m\nsub main\n  for i = 1 to 2.5\n  end\nend\n");
        assert!(
            e.iter()
                .any(|e| e.msg.contains("counts with `int` values") && e.msg.contains("limit")),
            "{e:?}"
        );
    }

    /// Locals are function-scoped, so two loops in one subroutine cannot share
    /// a variable name. Pinned deliberately: it is the existing scoping rule
    /// applied to loop variables, not an accident of `for`.
    #[test]
    fn two_loops_cannot_share_a_variable_name() {
        let e = errors("module m\nsub main\n  for i = 1 to 3\n  end\n  for i = 1 to 3\n  end\nend\n");
        assert!(
            e.iter().any(|e| e.msg.contains("`i` is defined more than once")),
            "{e:?}"
        );
    }

    #[test]
    fn a_loop_variable_is_readable_and_typed_int() {
        accepts("module m\nsub main\n  for i = 1 to 3\n    call print_int(i + 1)\n  end\nend\n");
        let e = errors("module m\nsub main\n  for i = 1 to 3\n    call print_text(i)\n  end\nend\n");
        assert!(!e.is_empty());
    }

    #[test]
    fn text_joins_with_plus_but_only_with_plus() {
        accepts("module m\nsub main\n  let s: text = \"a\" + \"b\"\n  call print_text(s)\nend\n");
        let e = errors("module m\nsub main\n  let s: text = \"a\" - \"b\"\n  call print_text(s)\nend\n");
        assert!(
            e.iter().any(|e| e.msg.contains("text supports `+` (joining)")),
            "{e:?}"
        );
    }

    #[test]
    fn plus_does_not_mix_text_with_numbers() {
        let e = errors("module m\nsub main\n  let s: text = \"a\" + 1\n  call print_text(s)\nend\n");
        assert!(!e.is_empty(), "text + int must not compile");
    }

    #[test]
    fn negation_needs_a_number() {
        accepts("module m\nsub main\n  var n: int = 3\n  call print_int(-n)\nend\n");
        let e = errors("module m\nsub main\n  let s: text = \"a\"\n  call print_text(-s)\nend\n");
        assert!(
            e.iter().any(|e| e.msg.contains("`-` negates numbers")),
            "{e:?}"
        );
    }

    /// A module variable's initializer may not read another one — including
    /// through a negation, which the recursion has to look inside.
    #[test]
    fn a_global_initializer_cannot_negate_another_global() {
        let e = errors("module m\nvar a: int = 1\nvar b: int = -a\nsub main\n  call print_int(b)\nend\n");
        assert!(
            e.iter().any(|e| e.msg.contains("cannot read module variable `a`")),
            "{e:?}"
        );
    }

    /// The aggregate rules, each with the line of the statement that broke it.
    #[test]
    fn array_diagnostics() {
        for (src, want) in [
            ("let xs: int[] = [1, \"two\"]", "element 2 is text"),
            ("let xs: int[] = [1]\n  let a: int = xs[-1]", "before the start"),
            ("let a: int = [1, 2][5]", "past the end"),
            // The boundary either side of the new base: [2] is the last
            // element, [3] is one past it, and [0] no longer exists at all.
            ("let a: int = [1, 2][3]", "past the end"),
            ("let a: int = [1, 2][0]", "before the start"),
            ("let xs: int[] = [1]\n  call print_int(xs)", "expects int, got int[]"),
            ("let xs: int[] = [1]\n  let n: int = count(xs, 1)", "expects 1 argument"),
            ("let n: int = count(5)", "expects an array"),
            ("let xs: text[] = []\n  let n: int = index_of(xs, 1)", "must match what the array holds"),
            ("let xs: int[] = []\n  let a: int = xs[\"one\"]", "an index counts with `int`"),
            ("let a: int = 1\n  a[0] = 2", "only an array, a byte-set or a dictionary has elements"),
            ("let xs: int[] = []\n  xs[0] = \"no\"", "holds int values, cannot store text"),
        ] {
            let m = parse(&format!("module m\nsub main\n  {src}\nend\n")).unwrap();
            let errs = validate(&m, &reg()).unwrap_err();
            assert!(
                errs.iter().any(|e| e.msg.contains(want) && e.line > 0),
                "expected {want:?} with a line, got {errs:?}"
            );
        }
    }

    /// The record rules, each with the line of the statement that broke it.
    /// Construction is where all three of the interesting mistakes live.
    #[test]
    fn record_diagnostics() {
        const DECL: &str = "module m\nrecord point\n  x: int\n  y: int\nend\n";
        for (src, want) in [
            ("let p: point = point(x: 1, y: 2, z: 3)", "has no field `z`"),
            ("let p: point = point(x: 1, y: \"two\")", "field `y` is int, got text"),
            ("let p: point = point(x: 1)", "is missing field `y`"),
            ("let p: point = point(x: 1, x: 2, y: 3)", "gives field `x` twice"),
            ("let p: point = point(x: 1, y: 2)\n  let n: int = p.z", "has no field `z`"),
            ("let n: int = 1\n  let m: int = n.x", "only a record has fields"),
            ("let p: circle = 1", "unknown type `circle`"),
            ("let p: point = point(x: 1, y: 2)\n  p.x = \"no\"", "cannot store text"),
        ] {
            let m = parse(&format!("{DECL}sub main\n  {src}\nend\n")).unwrap();
            let errs = validate(&m, &reg()).unwrap_err();
            assert!(
                errs.iter().any(|e| e.msg.contains(want) && e.line > 0),
                "expected {want:?} with a line, got {errs:?}"
            );
        }
    }

    /// A record whose field is its own type can never be built — and the check
    /// that says so must not be the thing that follows the cycle forever.
    /// Mutual recursion is the case a one-record `seen` set would miss.
    #[test]
    fn a_record_cannot_contain_itself() {
        for src in [
            "module m\nrecord node\n  next: node\nend\nsub main\nend\n",
            "module m\nrecord a\n  b: b\nend\nrecord b\n  a: a\nend\nsub main\nend\n",
        ] {
            let errs = errors(src);
            assert!(
                errs.iter().any(|e| e.msg.contains("contains itself") && e.line > 0),
                "{errs:?}"
            );
        }
        // A LIST of the same record is a different thing: `[]` is a real value,
        // so a tree is constructible and must stay legal.
        accepts("module m\nrecord node\n  kids: node[]\nend\nsub main\n  let n: node = node(kids: [])\nend\n");
    }

    /// A record name is written where a call is written, so it lives in the
    /// same namespace and a clash has to be reported rather than resolved.
    #[test]
    fn a_record_cannot_take_a_command_or_subroutine_name() {
        let errs = errors("module m\nrecord length\n  n: int\nend\nsub main\nend\n");
        assert!(errs.iter().any(|e| e.msg.contains("same name as a library command")), "{errs:?}");
    }

    /// The dictionary rules. `get` on a missing key is a RUN-time answer, not a
    /// compile error, so what is checked here is only the shape of the access.
    #[test]
    fn dict_diagnostics() {
        for (src, want) in [
            ("let d: int{} = {\"a\": 1, \"b\": \"two\"}", "value 2 is text"),
            ("let d: int{} = {1: 1}", "keyed by text"),
            ("let d: int{} = {}\n  let n: int = d[1]", "keyed by text"),
            ("let d: int{} = {}\n  d[1] = 2", "keyed by text"),
            ("let d: int{} = {}\n  d[\"a\"] = \"no\"", "cannot store text"),
            ("let d: int{} = {}\n  call dict_set(d, \"a\", \"no\")", "must match what the dictionary holds"),
            ("let n: int = dict_count(5)", "expects a dictionary"),
            ("let d: int{} = {\"a\": 1}\n  let n: text = d[\"a\"]", "declared text but expression is int"),
        ] {
            let m = parse(&format!("module m\nsub main\n  {src}\nend\n")).unwrap();
            let errs = validate(&m, &reg()).unwrap_err();
            assert!(
                errs.iter().any(|e| e.msg.contains(want) && e.line > 0),
                "expected {want:?} with a line, got {errs:?}"
            );
        }
    }

    /// An empty dictionary has no value to infer from, exactly as `[]` has no
    /// element — so the declaration is the only thing that can say.
    #[test]
    fn an_empty_dictionary_needs_a_declared_type() {
        let errs = errors("module m\nsub main\n  call print_int(count({}))\nend\n");
        assert!(
            errs.iter().any(|e| e.msg.contains("does not say what it holds")),
            "{errs:?}"
        );
        accepts("module m\nsub main\n  var d: text{} = {}\n  d[\"k\"] = \"v\"\nend\n");
    }

    /// `sort` and `join` read element VALUES, and a record's value is an
    /// address. Ordering by address is a confident wrong answer, which is worse
    /// than a refusal.
    #[test]
    fn a_list_of_records_cannot_be_sorted_or_joined() {
        const DECL: &str = "module m\nrecord point\n  x: int\nend\n";
        for cmd in ["call sort(ps)", "let s: text = join(ps, \",\")"] {
            let src = format!(
                "{DECL}sub main\n  var ps: point[] = [point(x: 1)]\n  {cmd}\nend\n"
            );
            let errs = errors(&src);
            assert!(
                errs.iter().any(|e| e.msg.contains("has no order and no spelling")),
                "{errs:?}"
            );
        }
        // The commands that only move elements around stay available.
        accepts(&format!(
            "{DECL}sub main\n  var ps: point[] = [point(x: 1)]\n               ps = append(ps, point(x: 2))\n  call print_int(count(ps))\nend\n"
        ));
    }

    /// An empty list has no element to infer from, so the declaration is the
    /// only thing that can say what it holds — and without one it must be an
    /// error rather than a guess.
    #[test]
    fn an_empty_list_needs_a_declared_type() {
        let ok = parse("module m\nsub main\n  var xs: text[] = []\n  call print_int(count(xs))\nend\n")
            .unwrap();
        assert!(validate(&ok, &reg()).is_ok(), "{:?}", validate(&ok, &reg()));

        let bad = parse("module m\nsub main\n  call print_int(count([]))\nend\n").unwrap();
        let errs = validate(&bad, &reg()).unwrap_err();
        assert!(
            errs.iter().any(|e| e.msg.contains("does not say what it holds")),
            "{errs:?}"
        );
    }

    /// `append` is declared over "any array" and "whatever it holds", so the
    /// checker has to give the call back the concrete type it was handed.
    #[test]
    fn a_generic_command_yields_a_concrete_type() {
        let m = parse(
            "module m\nsub main\n  var xs: text[] = []\n  xs = append(xs, \"a\")\n  \
             call print_text(join(xs, \",\"))\nend\n",
        )
        .unwrap();
        assert!(validate(&m, &reg()).is_ok(), "{:?}", validate(&m, &reg()));

        let bad = parse(
            "module m\nsub main\n  var xs: text[] = []\n  var ns: int[] = append(xs, \"a\")\nend\n",
        )
        .unwrap();
        assert!(validate(&bad, &reg()).is_err());
    }

    /// Elements change; the binding does not. `let` promises the name keeps
    /// meaning the same array, which is why writing one element is allowed.
    #[test]
    fn an_element_of_a_let_array_may_be_assigned() {
        let m = parse("module m\nsub main\n  let xs: int[] = [1]\n  xs[1] = 2\nend\n").unwrap();
        assert!(validate(&m, &reg()).is_ok(), "{:?}", validate(&m, &reg()));
    }

    #[test]
    fn a_byte_set_is_indexed_as_numbers() {
        let m = parse(
            "module m\nsub main\n  var b: bytes = bytes_new(2)\n  b[1] = 65\n  \
             call print_int(b[1])\nend\n",
        )
        .unwrap();
        assert!(validate(&m, &reg()).is_ok(), "{:?}", validate(&m, &reg()));
    }

    #[test]
    fn rejects_mixed_numeric() {
        let m = parse("module m\nsub main\n  let d: double = 1.5\n  let x: double = d + 1\nend\n")
            .unwrap();
        assert!(validate(&m, &reg()).is_err());
    }

    /// A registry with one non-visual component whose `beep` event hands over
    /// an int. Built here rather than taken from `Registry::core()` because the
    /// hard-coded set is checked against the real `core` metadata, and a
    /// component invented for a test has no business in that comparison.
    fn reg_with_event_param() -> Registry {
        use crate::registry::{ComponentDesc, ComponentKind};
        let mut r = Registry::core();
        r.insert_component(ComponentDesc {
            name: "buzzer".into(),
            a11y_role: 0,
            kind: ComponentKind::NonVisual,
            library: "core".into(),
            properties: Vec::new(),
            events: vec!["beep".into()],
        });
        r.set_event_params("buzzer", "beep", vec![Ty::Int]);
        r
    }

    fn buzzer_module(handler: &str) -> Module {
        parse(&format!(
            "module m\n\nbuzzer b\n  on beep: h\nend\n\n             sub main\n  call print_int(1)\nend\n\n{handler}"
        ))
        .unwrap()
    }

    #[test]
    fn a_handler_may_take_what_the_event_hands_it() {
        let m = buzzer_module("sub h(n: int)\n  call print_int(n)\nend\n");
        let r = reg_with_event_param();
        assert!(validate(&m, &r).is_ok(), "{:?}", validate(&m, &r));
    }

    /// The compatibility that matters most: every program written before an
    /// event carried anything binds a parameterless handler, and must go on
    /// compiling unchanged.
    #[test]
    fn a_handler_may_ignore_what_the_event_hands_it() {
        let m = buzzer_module("sub h\n  call print_int(1)\nend\n");
        let r = reg_with_event_param();
        assert!(validate(&m, &r).is_ok(), "{:?}", validate(&m, &r));
    }

    #[test]
    fn rejects_a_handler_that_takes_the_wrong_thing() {
        let m = buzzer_module("sub h(s: text)\n  call print_text(s)\nend\n");
        let e = validate(&m, &reg_with_event_param()).unwrap_err();
        assert!(
            e.iter().any(|e| e.msg.contains("hands a handler (int)")
                && e.msg.contains("takes (text)")),
            "{e:?}"
        );
        // The line of the offending subroutine, so an editor can point at it.
        assert!(e.iter().any(|e| e.line > 0), "{e:?}");
    }

    /// An event that hands nothing over still rejects a handler that asks for
    /// something: there is no argument to invent.
    #[test]
    fn rejects_a_parameter_on_an_event_that_hands_nothing() {
        use crate::registry::{ComponentDesc, ComponentKind};
        let mut r = Registry::core();
        r.insert_component(ComponentDesc {
            name: "buzzer".into(),
            a11y_role: 0,
            kind: ComponentKind::NonVisual,
            library: "core".into(),
            properties: Vec::new(),
            events: vec!["beep".into()],
        });
        let m = buzzer_module("sub h(n: int)\n  call print_int(n)\nend\n");
        assert!(validate(&m, &r).is_err());
    }

    // --- diagnostics that name the fix, and where -------------------------

    /// A diagnostic underlines the name it is about, not the line it sits on:
    /// with two calls on one line, "something here is wrong" is not a
    /// diagnostic.
    #[test]
    fn a_diagnostic_carries_the_columns_of_the_name_it_is_about() {
        //                     1         2
        //            123456789012345678901234567890
        let src = "module m\nsub main\n  call print_int(nope(1))\nend\n";
        let e = errors(src);
        let bad = e.iter().find(|e| e.msg.contains("unknown command `nope`")).unwrap();
        assert_eq!((bad.line, bad.col, bad.end_col), (3, 18, 22), "{bad:?}");
        // The rendered form is unchanged: the CLI and Studio parse `line N:`.
        assert!(bad.to_string().starts_with("line 3: "), "{bad}");

        // A message about a name the header does not contain still gets the
        // header, which is what every diagnostic used to get.
        let e = errors("module m\nsub main\n  if 5\n    call print_int(1)\n  end\nend\n");
        let bad = e.iter().find(|e| e.msg.contains("truth value")).unwrap();
        assert_eq!((bad.line, bad.col, bad.end_col), (3, 3, 7), "{bad:?}");
    }

    #[test]
    fn an_unknown_command_suggests_the_nearest_real_one() {
        let e = errors("module m\nsub main\n  call prnt_text(\"hi\")\nend\n");
        let bad = e.iter().find(|e| e.msg.contains("unknown command")).unwrap();
        assert_eq!(
            bad.msg,
            "in `main`: unknown command `prnt_text` — did you mean `print_text`?"
        );
        // Subroutines are candidates too, and a wholly different word is not.
        let e = errors("module m\nsub greet\nend\nsub main\n  call gret()\n  call frobnicate()\nend\n");
        assert!(e.iter().any(|e| e.msg.ends_with("did you mean `greet`?")), "{e:?}");
        let far = e.iter().find(|e| e.msg.contains("frobnicate")).unwrap();
        assert!(!far.msg.contains("did you mean"), "{far:?}");
    }

    #[test]
    fn an_undefined_variable_suggests_the_nearest_one_in_scope() {
        let e = errors("module m\nsub main\n  var total: int = 1\n  totl = 2\n  call print_int(totla)\nend\n");
        assert!(
            e.iter().any(|e| e.msg == "in `main`: assignment to undefined variable `totl` — did you mean `total`?"),
            "{e:?}"
        );
        assert!(
            e.iter().any(|e| e.msg == "in `main`: use of undefined variable `totla` — did you mean `total`?"),
            "{e:?}"
        );
    }

    /// The registry cannot see a library the module never `use`d; the caller
    /// can, and says so through `Hints`. The message names the exact line to
    /// add.
    #[test]
    fn a_command_from_an_unused_library_says_which_use_to_add() {
        let m = parse("module m\nsub main\n  let t: text = file_read_text(\"a\")\nend\n").unwrap();
        let mut hints = Hints::default();
        hints.elsewhere.insert("file_read_text".into(), "file".into());
        let e = validate_with(&m, &reg(), &hints).unwrap_err();
        assert_eq!(
            e[0].msg,
            "in `main`: unknown command `file_read_text` — it is in the `file` library: add `use file` to the module"
        );
        assert_eq!((e[0].line, e[0].col), (3, 17), "{e:?}");
    }

    #[test]
    fn closest_allows_a_slip_but_not_a_different_word() {
        let names = ["print_text", "print_int", "length", "concat"];
        let it = || names.iter().copied();
        assert_eq!(closest("prnt_text", it()), Some("print_text"));
        assert_eq!(closest("lenght", it()), Some("length"));
        assert_eq!(closest("Length", it()), Some("length"));
        assert_eq!(closest("print", it()), None);
        assert_eq!(closest("xyz", it()), None);
        assert_eq!(closest("length", it()), None, "an exact match is not a suggestion");
    }

    /// `sys_sleep_ms` holds the thread the window is drawn from. A module with
    /// a form is refused it at build time, and told what to use instead.
    #[test]
    fn a_windowed_program_may_not_sleep() {
        use crate::registry::{ComponentDesc, ComponentKind};
        use crate::Signature;
        let mut r = Registry::core();
        r.insert("sys_sleep_ms", Signature::simple(vec![Ty::Int], None), "x");
        r.insert_component(ComponentDesc {
            name: "form".into(),
            a11y_role: 0,
            kind: ComponentKind::Visual,
            library: "ui".into(),
            properties: Vec::new(),
            events: Vec::new(),
        });
        let m = parse("module m\nform win\nend\nsub main\n  call sys_sleep_ms(500)\nend\n").unwrap();
        let e = validate(&m, &r).unwrap_err();
        assert_eq!(
            e[0].msg,
            "in `main`: `sys_sleep_ms` would freeze the window — a windowed program \
             does not wait, it declares a `timer` and does the work in its `on tick` handler"
        );
        assert_eq!((e[0].line, e[0].col, e[0].end_col), (5, 8, 20), "{e:?}");
        // Without a form there is no window to freeze.
        let m = parse("module m\nsub main\n  call sys_sleep_ms(500)\nend\n").unwrap();
        assert!(validate(&m, &r).is_ok());
    }

    /// The binding line is where a bad event or handler name is written, and
    /// both are a typo away from a real one often enough to say which.
    #[test]
    fn a_bad_binding_is_placed_on_its_line_and_suggests_the_fix() {
        let src = "module m\n\nbuzzer b\n  on beeb: hh\nend\n\nsub main\nend\n\nsub h\nend\n";
        let e = validate(&parse(src).unwrap(), &reg_with_event_param()).unwrap_err();
        let ev = e.iter().find(|e| e.msg.contains("has no event")).unwrap();
        assert_eq!(
            ev.msg,
            "`b`: component `buzzer` has no event `beeb` — did you mean `beep`? (has: beep)"
        );
        assert_eq!((ev.line, ev.col, ev.end_col), (4, 6, 10), "{ev:?}");
        let hd = e.iter().find(|e| e.msg.contains("not a subroutine")).unwrap();
        assert!(hd.msg.ends_with("— did you mean `h`?"), "{hd:?}");
        assert_eq!(hd.line, 4);
    }

    #[test]
    fn a_wrong_handler_signature_shows_both_and_the_line_to_paste() {
        let m = buzzer_module("sub h(s: text)\n  call print_text(s)\nend\n");
        let e = validate(&m, &reg_with_event_param()).unwrap_err();
        assert_eq!(
            e[0].msg,
            "`b`: event `beep` hands a handler (int), but `h` takes (text) — take exactly those, or none: `sub h(n: int)`"
        );
        assert_eq!((e[0].line, e[0].col), (11, 5), "points at the handler's name: {e:?}");
    }

    #[test]
    fn param_lists_name_parameters_from_their_types() {
        assert_eq!(param_list(&[Ty::Int, Ty::Text, Ty::Int]), "n: int, s: text, n2: int");
        assert_eq!(param_list(&[]), "");
    }

    #[test]
    fn rejects_a_handler_that_returns_a_value() {
        let m = buzzer_module("sub h(n: int): int\n  return n\nend\n");
        let e = validate(&m, &reg_with_event_param()).unwrap_err();
        assert!(e.iter().any(|e| e.msg.contains("returns nothing")), "{e:?}");
    }
}
