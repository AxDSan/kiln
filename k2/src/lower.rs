//! K2 AST → KIR. A first lowerer for the runnable subset.
//!
//! Resolution and desugaring happen here so the KIR the backend sees is plain:
//! names become ids, `Console.WriteLine` becomes `printf`, ternaries and
//! short-circuit `&&`/`||` become `If` into a temp, interpolation becomes a run
//! of prints. Types are resolved to `kiln_kir::TyId`.

use crate::ast;
use crate::lexer::Span as LSpan;
use kiln_kir::build::ModuleBuilder;
use kiln_kir::*;
use std::collections::HashMap;

/// Field indices of a synthesised `Result` record.
const RESULT_OK: usize = 0;
const RESULT_VALUE: usize = 1;
const RESULT_ERR: usize = 2;

/// Field indices of a synthesised `Dictionary` record.
const DICT_LEN: usize = 0;
const DICT_CAP: usize = 1;
const DICT_KEYS: usize = 2;
const DICT_VALUES: usize = 3;
/// Open-addressed index: slot → entry number plus one, `0` meaning empty.
const DICT_INDEX: usize = 4;

/// Field index of a set's open-addressed index buffer.
const SET_INDEX: usize = 3;

/// Field indices of a synthesised `List` record.
const LIST_LEN: usize = 0;
const LIST_CAP: usize = 1;
const LIST_DATA: usize = 2;

/// How a K2 program reaches the outside world.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Runtime {
    /// libc only: printing is `printf`. Self-contained, links nothing else.
    #[default]
    Libc,
    /// The Kiln runtime: printing is the `print_text` command over the slot
    /// ABI, exactly as a 1.x program reaches it.
    Kiln,
}

pub fn lower(p: &ast::Program) -> Result<Module, String> {
    lower_with(p, Runtime::Libc)
}

pub fn lower_with(p: &ast::Program, runtime: Runtime) -> Result<Module, String> {
    lower_full(p, runtime, None)
}

/// Lower, with the command registry the standard library is described by. With
/// one, `File.ReadText(p)` resolves to the `file_read_text` command; without
/// one, only the built-in surface is available.
pub fn lower_full(
    p: &ast::Program,
    runtime: Runtime,
    registry: Option<&kiln_ir::Registry>,
) -> Result<Module, String> {
    lower_opts(p, runtime, registry, &Options::default())
}

/// What a build asks of lowering beyond the source: the kind of artifact, and
/// the machine it is for.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// `SharedLib` or `StaticLib` to build a library; anything else builds a
    /// program, whose kind the source decides (a form makes it a GUI program).
    pub kind: ModuleKind,
    pub target: Target,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            kind: ModuleKind::Console,
            target: Target::X86_64_LINUX,
        }
    }
}

pub fn lower_opts(
    p: &ast::Program,
    runtime: Runtime,
    registry: Option<&kiln_ir::Registry>,
    opts: &Options,
) -> Result<Module, String> {
    let library = matches!(opts.kind, ModuleKind::SharedLib | ModuleKind::StaticLib);
    let mut b = ModuleBuilder::new(
        p.namespace.as_deref().unwrap_or("program"),
        if library { opts.kind } else { ModuleKind::Console },
        opts.target,
    );

    // Pass 1: reserve a RecordId + interned type for every declared type.
    let mut type_ids: HashMap<String, RecordId> = HashMap::new();
    let mut type_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    for item in &p.items {
        match item {
            ast::Item::Type(td) => {
                type_names.insert(td.name.clone());
            }
            ast::Item::Enum(ed) => {
                type_names.insert(ed.name.clone());
            }
            ast::Item::Interface(id) => {
                type_names.insert(id.name.clone());
            }
            ast::Item::Form(f) => {
                type_names.insert(f.name.clone());
            }
            ast::Item::Component(_) => {}
        }
    }
    for item in &p.items {
        if let ast::Item::Type(td) = item {
            if !td.type_params.is_empty() {
                continue; // a template: instantiated on use
            }
            if td.kind != ast::TypeKind::StaticClass {
                let rid = RecordId(b.m.records.len() as u32);
                b.m.records.push(RecordDef {
                    id: rid,
                    name: td.name.clone(),
                    fields: Vec::new(),
                    layout: Layout::Managed,
                    equality: match td.kind {
                        ast::TypeKind::Record => Equality::ByValue,
                        _ => Equality::ByRef,
                    },
                });
                let _ = b.m.types.intern(TyKind::Record(rid));
                type_ids.insert(td.name.clone(), rid);
            }
        }
    }

    // Collect enum members as integer constants.
    let mut enums: HashMap<String, HashMap<String, i128>> = HashMap::new();
    for item in &p.items {
        if let ast::Item::Enum(ed) = item {
            let mut members = HashMap::new();
            let mut next = 0i128;
            for (name, val) in &ed.members {
                let v = match val {
                    Some(e) => const_int(e)?,
                    None => next,
                };
                members.insert(name.clone(), v);
                next = v + 1;
            }
            enums.insert(ed.name.clone(), members);
        }
    }

    // With the runtime linked, the collector owns the heap.
    if runtime == Runtime::Kiln {
        b.m.allocator = Allocator::Runtime;
    }
    // `Bytes` is addressed as a buffer of u8; intern that once up front.
    let _ = b.m.types.intern(TyKind::Array(TyTable::U8));
    let mut cx = Cx {
        registry: registry.cloned(),
        runtime,
        b,
        type_ids,
        enums,
        enum_backing: HashMap::new(),
        consts: HashMap::new(),
        methods: HashMap::new(),
        type_names,
        generics: HashMap::new(),
        mono: HashMap::new(),
        tvars: HashMap::new(),
        results: HashMap::new(),
        lists: HashMap::new(),
        dicts: HashMap::new(),
        sets: HashMap::new(),
        dlls: HashMap::new(),
        packed: std::collections::HashSet::new(),
        tables: HashMap::new(),
        form_state: HashMap::new(),
        static_inits: Vec::new(),
        nonvisual: HashMap::new(),
        c_bools: Default::default(),
        namespace_components: Vec::new(),
        components: HashMap::new(),
        generic_types: HashMap::new(),
        type_mono: HashMap::new(),
        interfaces: HashMap::new(),
        iface_records: HashMap::new(),
        impls: HashMap::new(),
        pending: Vec::new(),
    };

    // Interfaces: a value of interface type is a record holding the object and
    // one function pointer per method — dispatch without a separate vtable
    // global to initialise.
    for item in &p.items {
        if let ast::Item::Interface(idecl) = item {
            let names: Vec<String> = idecl.methods.iter().map(|m| m.name.clone()).collect();
            let mut fields: Vec<(&str, TyId)> = vec![("obj", TyTable::PTR)];
            for _ in &names {
                fields.push(("fn", TyTable::PTR));
            }
            let n = cx.b.m.records.len();
            let rid =
                cx.b.c_record(&format!("$IFace{n}"), fields, Equality::ByRef);
            cx.interfaces.insert(idecl.name.clone(), names);
            cx.iface_records.insert(idecl.name.clone(), rid);
        }
    }
    // Which methods each type supplies for each interface it implements.
    for item in &p.items {
        if let ast::Item::Type(td) = item {
            for iname in &td.implements {
                let Some(want) = cx.interfaces.get(iname).cloned() else {
                    return Err(format!(
                        "`{}` implements unknown interface `{iname}`",
                        td.name
                    ));
                };
                for m in &want {
                    if !td.methods.iter().any(|x| &x.name == m) {
                        return Err(format!("`{}` does not implement `{iname}.{m}`", td.name));
                    }
                }
                cx.impls.insert((td.name.clone(), iname.clone()), want);
            }
        }
    }

    // `[Packed]` records lay out with no padding; `[Table]` records map to a
    // database table, with column names defaulting to snake_case.
    for item in &p.items {
        if let ast::Item::Type(td) = item {
            if td.attrs.iter().any(|a| a.name == "Packed") {
                cx.packed.insert(td.name.clone());
            }
            if let Some(t) = td.attrs.iter().find(|a| a.name == "Table") {
                let table = match t.args.first().map(|e| &e.kind) {
                    Some(ast::ExprKind::Str(s)) => s.clone(),
                    _ => snake_case(&td.name),
                };
                let mut columns = Vec::new();
                let mut types = Vec::new();
                for f in td.record_params.iter().chain(td.fields.iter()) {
                    let col = f
                        .attrs
                        .iter()
                        .find(|a| a.name == "Column")
                        .and_then(|a| match a.args.first().map(|e| &e.kind) {
                            Some(ast::ExprKind::Str(s)) => Some(s.clone()),
                            _ => None,
                        })
                        .unwrap_or_else(|| snake_case(&f.name));
                    let auto = f.attrs.iter().any(|a| a.name == "Auto");
                    columns.push((f.name.clone(), col, auto));
                    types.push(f.ty.clone());
                }
                cx.tables.insert(
                    td.name.clone(),
                    TableInfo {
                        table,
                        columns,
                        types,
                    },
                );
            }
        }
    }

    // An enum's backing type, so it can be written as a parameter or field.
    for item in &p.items {
        if let ast::Item::Enum(ed) = item {
            let ty = match &ed.backing {
                Some(t) => cx.resolve(t)?,
                None => TyTable::I32,
            };
            cx.enum_backing.insert(ed.name.clone(), ty);
        }
    }

    // Pass 2: fill record fields and compute C layout. A `[CLayout]` record
    // nested by value in another needs its own size first, so the pass repeats
    // until every nesting depth has settled.
    let clayout_names: std::collections::HashSet<String> = p
        .items
        .iter()
        .filter_map(|i| match i {
            ast::Item::Type(td) if td.attrs.iter().any(|a| a.name == "CLayout") => Some(td.name.clone()),
            _ => None,
        })
        .collect();
    for _round in 0..=clayout_names.len() {
    for item in &p.items {
        if let ast::Item::Type(td) = item {
            if td.kind == ast::TypeKind::StaticClass || !td.type_params.is_empty() {
                continue;
            }
            let rid = cx.type_ids[&td.name];
            let clayout = td.attrs.iter().any(|a| a.name == "CLayout");
            let mut fields = Vec::new();
            for (i, f) in td.record_params.iter().chain(td.fields.iter()).enumerate() {
                let mut ty = match &f.ty {
                    // `byte[16] Bytes` — a C `unsigned char Bytes[16]`.
                    ast::TypeRef::Fixed(inner, n) if clayout => {
                        let elem = cx.resolve(inner)?;
                        if elem == TyTable::BOOL {
                            return Err(format!(
                                "field `{}`: an inline array of `bool` has no one C width — use `int[{n}]` or `byte[{n}]`",
                                f.name
                            ));
                        }
                        if *n == 0 {
                            return Err(format!("field `{}`: an inline array holds at least one value", f.name));
                        }
                        cx.b.m.types.intern(TyKind::Inline { elem, count: *n })
                    }
                    _ => cx.resolve(&f.ty)?,
                };
                // A `[CLayout]` record inside another is held by value, as a C
                // struct member is; a pointer to one is a `Ptr`.
                if clayout {
                    if let ast::TypeRef::Named(n) = &f.ty {
                        if clayout_names.contains(n) {
                            ty = cx.b.m.types.intern(TyKind::Inline { elem: ty, count: 0 });
                        }
                    }
                }
                // A `bool` in a C struct is a C int — a Win32 `BOOL` — and a C
                // API writes truth as any non-zero value. It is stored as an int
                // and read back as `!= 0`, so a 7 is true.
                if clayout && ty == TyTable::BOOL {
                    ty = TyTable::I32;
                    cx.c_bools.insert((rid, i));
                }
                fields.push(FieldDef {
                    name: f.name.clone(),
                    ty,
                });
            }
            let (size, align, offsets) = if cx.packed.contains(&td.name) {
                let mut off = 0i64;
                let mut offs = Vec::new();
                for f in &fields {
                    offs.push(off);
                    off += cx.scalar_size(f.ty);
                }
                (off, 1, offs)
            } else {
                cx.c_layout(&fields)
            };
            cx.b.m.records[rid.0 as usize].fields = fields;
            cx.b.m.records[rid.0 as usize].layout = Layout::C {
                size,
                align,
                offsets,
            };
        }
    }
    }

    // Collect constants (literal-valued) for reference resolution.
    for item in &p.items {
        if let ast::Item::Type(td) = item {
            for c in &td.consts {
                let ty = cx.resolve(&c.ty)?;
                cx.consts.insert(c.name.clone(), (ty, c.value.clone()));
                cx.consts
                    .insert(format!("{}.{}", td.name, c.name), (ty, c.value.clone()));
            }
        }
    }

    // Generic type templates are instantiated from type references.
    for item in &p.items {
        if let ast::Item::Type(td) = item {
            if !td.type_params.is_empty() {
                cx.generic_types.insert(td.name.clone(), td.clone());
            }
        }
    }

    // A static class's fields are module globals: there is one of each, for
    // the life of the program, which is what `static` means. Their initial
    // values run before the entry point does.
    for item in &p.items {
        if let ast::Item::Type(td) = item {
            if td.kind != ast::TypeKind::StaticClass {
                continue;
            }
            for fld in &td.fields {
                let ty = cx.resolve(&fld.ty)?;
                let g = cx.b.add_global(
                    &format!("{}__static_{}", td.name, fld.name),
                    ty,
                    cx.b.m.types.is_pointer(ty),
                );
                cx.form_state.insert(fld.name.clone(), (g, ty));
                cx.form_state
                    .insert(format!("{}.{}", td.name, fld.name), (g, ty));
                if let Some(init) = &fld.default {
                    cx.static_inits.push((g, ty, init.clone()));
                }
            }
        }
    }

    // Components declared at namespace level have no rectangle: a timer, a
    // server. Each is a global holding the handle its library hands back.
    for item in &p.items {
        if let ast::Item::Component(c) = item {
            let lib = cx.nonvisual_library(&c.type_name).ok_or_else(|| {
                format!(
                    "`{}` is not a component without a rectangle; a visual one belongs in a form",
                    c.type_name
                )
            })?;
            let g = cx.b.add_global(&format!("$component__{}", c.id), TyTable::I64, false);
            cx.components.insert(c.id.clone(), (g, c.type_name.clone()));
            cx.nonvisual.insert(c.id.clone(), lib);
            cx.namespace_components.push(c.clone());
        }
    }

    // Pass 3: declare all methods (symbol + signature) before lowering bodies.
    for item in &p.items {
        if let ast::Item::Type(td) = item {
            if !td.type_params.is_empty() {
                continue;
            }
            for m in &td.methods {
                let this = if m.is_static || td.kind == ast::TypeKind::StaticClass {
                    None
                } else {
                    Some(cx.record_ty(&td.name))
                };
                // `[Dll]` extern: a foreign function, not a body to lower.
                if m.is_extern {
                    register_dll(&mut cx, &td.name, m)?;
                    continue;
                }
                // A generic method is a template: nothing is emitted until a
                // call site fixes its type arguments.
                if !m.type_params.is_empty() {
                    let t = Template {
                        owner: td.name.clone(),
                        method: m.clone(),
                        this: this.is_some(),
                    };
                    cx.generics
                        .insert(format!("{}.{}", td.name, m.name), t.clone());
                    cx.generics.entry(m.name.clone()).or_insert(t);
                    continue;
                }
                let mut params = Vec::new();
                if let Some(t) = this {
                    params.push(("this", t));
                }
                let owned: Vec<(String, TyId)> = m
                    .params
                    .iter()
                    .map(|p| Ok((p.name.clone(), cx.resolve(&p.ty)?)))
                    .collect::<Result<_, String>>()?;
                for (n, t) in &owned {
                    params.push((n.as_str(), *t));
                }
                let ret = cx.resolve(&m.ret)?;
                let sym = format!("{}_{}", td.name, m.name);
                let fid = cx.b.declare_func(&sym, params, ret);
                cx.b.m.funcs[fid.0 as usize].line = m.span.line;
                let sig = Sig {
                    fid,
                    this: this.is_some(),
                    params: owned.iter().map(|(_, t)| *t).collect(),
                    defaults: m.params.iter().map(|p| p.default.clone()).collect(),
                    ret,
                };
                cx.methods
                    .insert(format!("{}.{}", td.name, m.name), sig.clone());
                // also reachable unqualified from within the same type
                cx.methods.entry(m.name.clone()).or_insert(sig);
            }
        }
    }

    // Pass 4: lower method bodies (generic templates are skipped — their
    // instances are lowered from the pending queue below).
    for item in &p.items {
        if let ast::Item::Type(td) = item {
            if !td.type_params.is_empty() {
                continue;
            }
            for m in &td.methods {
                if !m.type_params.is_empty() || m.is_extern {
                    continue;
                }
                let sig = cx.methods[&format!("{}.{}", td.name, m.name)].clone();
                cx.lower_method(td, m, &sig)?;
            }
        }
    }
    // A form: its components become globals holding runtime handles, its
    // methods are plain functions, and its build sequence is the entry point.
    // `partial form` lets the designer's half and the code half be separate
    // blocks — in one file or several. They are merged here, in source order.
    let form = {
        let parts: Vec<&ast::FormDecl> = p
            .items
            .iter()
            .filter_map(|i| match i {
                ast::Item::Form(f) => Some(f),
                _ => None,
            })
            .collect();
        match parts.split_first() {
            None => None,
            Some((first, rest)) => {
                if let Some(other) = rest.iter().find(|f| f.name != first.name) {
                    return Err(format!(
                        "a program declares one form, but found `{}` and `{}`",
                        first.name, other.name
                    ));
                }
                let mut merged = (*first).clone();
                for part in rest {
                    merged.properties.extend(part.properties.clone());
                    merged.components.extend(part.components.clone());
                    merged.fields.extend(part.fields.clone());
                    merged.methods.extend(part.methods.clone());
                }
                Some(merged)
            }
        }
    };
    if form.is_some() && library {
        return Err("a library has no window — a form is built into a program, not a shared or static library".into());
    }
    if let Some(f) = &form {
        cx.b.m.kind = ModuleKind::Gui;
        for c in &f.components {
            let g =
                cx.b.add_global(&format!("{}__{}", f.name, c.id), TyTable::I64, false);
            cx.components.insert(c.id.clone(), (g, c.type_name.clone()));
            if let Some(lib) = cx.nonvisual_library(&c.type_name) {
                cx.nonvisual.insert(c.id.clone(), lib);
            }
        }
        for fld in &f.fields {
            let ty = cx.resolve(&fld.ty)?;
            let g = cx.b.add_global(
                &format!("{}__state_{}", f.name, fld.name),
                ty,
                cx.b.m.types.is_pointer(ty),
            );
            cx.form_state.insert(fld.name.clone(), (g, ty));
            // `string label = "points";` — the default used to be dropped, so
            // the field started as null and the first handler to read it did
            // not get what the source said.
            if let Some(init) = &fld.default {
                cx.static_inits.push((g, ty, init.clone()));
            }
        }
        // Declare the handlers first so the build sequence can bind them.
        for m in &f.methods {
            // An extern in a form is a foreign function exactly as it is in a
            // class. Declared and lowered like a method instead, it compiled to
            // an empty body returning 0 — a call to C that silently never
            // happened.
            if m.is_extern {
                register_dll(&mut cx, &f.name, m)?;
                continue;
            }
            let owned: Vec<(String, TyId)> = m
                .params
                .iter()
                .map(|p| Ok((p.name.clone(), cx.resolve(&p.ty)?)))
                .collect::<Result<_, String>>()?;
            let params: Vec<(&str, TyId)> = owned.iter().map(|(n, t)| (n.as_str(), *t)).collect();
            let ret = cx.resolve(&m.ret)?;
            let sym = format!("{}_{}", f.name, m.name);
            let fid = cx.b.declare_func(&sym, params, ret);
            let sig = Sig {
                fid,
                this: false,
                params: owned.iter().map(|(_, t)| *t).collect(),
                defaults: m.params.iter().map(|p| p.default.clone()).collect(),
                ret,
            };
            cx.methods
                .insert(format!("{}.{}", f.name, m.name), sig.clone());
            cx.methods.entry(m.name.clone()).or_insert(sig);
        }
        for m in f.methods.iter().filter(|m| !m.is_extern) {
            let sig = cx.methods[&format!("{}.{}", f.name, m.name)].clone();
            cx.lower_into(sig.fid, m, false, None, None)?;
        }
        let build =
            cx.b.declare_func(&format!("{}_Build", f.name), vec![], TyTable::I32);
        cx.lower_form(build, f)?;
        // The form built its components itself; the wrapper only sets globals.
        let entry = cx.wrap_entry(build, true)?;
        cx.b.set_entry(entry);
        cx.drain_pending()?;
        return Ok(cx.b.build());
    }

    if library {
        if !p.top_level.is_empty() {
            return Err("a library has no entry point — top-level statements belong in a program".into());
        }
        library_exports(&mut cx, p)?;
        cx.drain_pending()?;
        return Ok(cx.b.build());
    }

    // Top-level statements become Main.
    if !p.top_level.is_empty() {
        let main = cx.b.declare_func("kmain", vec![], TyTable::VOID);
        let mut fl = FnLower::new(&mut cx, main, TyTable::VOID);
        fl.captured = captured_names(&p.top_level, None);
        for s in &p.top_level {
            fl.stmt(s)?;
        }
        let body = fl.finish();
        cx.b.set_body(main, body);
        let entry = cx.wrap_entry_with_inits(main)?;
        cx.b.set_entry(entry);
    } else if let Some(sig) = cx.methods.get("Main").cloned() {
        let entry = cx.wrap_entry_with_inits(sig.fid)?;
        cx.b.set_entry(entry);
    }

    // Every generic instance and lifted lambda queued by *any* of the above —
    // method bodies and top-level code alike — gets its body here. This must
    // run after the last direct lowering, or an instance would be declared and
    // left empty.
    cx.drain_pending()?;

    Ok(cx.b.build())
}

/// A library's face to a C host: each `public static` method of a `public static
/// class`, exported under its own name (or the one `[Export("name")]` gives),
/// and `<Namespace>_init` for the static fields' initial values.
///
/// The export is a thin wrapper around the method, as 1.x's is around a sub:
/// internal calls keep the method's own symbol, and the wrapper carries the C
/// shape — a `bool` crosses as a 32-bit int, which is what a C `_Bool`-free
/// header can promise. `DllAttach` and `DllDetach` are exported as the loader
/// hooks `dll_attach` and `dll_detach`.
fn library_exports(cx: &mut Cx, p: &ast::Program) -> Result<(), String> {
    let module = cx.b.m.name.replace('.', "_");
    let mut seen: HashMap<String, String> = HashMap::new();
    for item in &p.items {
        let ast::Item::Type(td) = item else { continue };
        if td.kind != ast::TypeKind::StaticClass || td.vis != ast::Vis::Public {
            continue;
        }
        for m in &td.methods {
            if !m.is_static || m.vis != ast::Vis::Public || m.is_extern || !m.type_params.is_empty() {
                continue;
            }
            let export = m.attrs.iter().find(|a| a.name == "Export");
            let symbol = match (m.name.as_str(), export) {
                (_, Some(a)) => match a.args.first().map(|e| &e.kind) {
                    Some(ast::ExprKind::Str(s)) => s.clone(),
                    None => m.name.clone(),
                    _ => return Err(format!("`[Export]` on `{}` takes the C name as a string", m.name)),
                },
                ("DllAttach", None) => "dll_attach".into(),
                ("DllDetach", None) => "dll_detach".into(),
                _ => m.name.clone(),
            };
            let what = format!("{}.{}", td.name, m.name);
            if let Some(prev) = seen.insert(symbol.clone(), what.clone()) {
                return Err(format!("`{what}` and `{prev}` would both be exported as `{symbol}` — rename one with `[Export(\"name\")]`"));
            }
            let sig = cx.methods[&what].clone();
            let c_ty = |t: TyId| if t == TyTable::BOOL { TyTable::I32 } else { t };
            let names: Vec<String> = m.params.iter().map(|p| p.name.clone()).collect();
            let params: Vec<(&str, TyId)> =
                names.iter().map(|n| n.as_str()).zip(sig.params.iter().map(|t| c_ty(*t))).collect();
            let ret = c_ty(sig.ret);
            let fid = cx.b.func_full(&symbol, params.clone(), ret, CallConv::Cdecl, Linkage::Exported, true);
            let mut fl = FnLower::new(cx, fid, ret);
            let args: Vec<Expr> = sig
                .params
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    let a = Expr::Local(LocalId(i as u32));
                    if *t == TyTable::BOOL {
                        Expr::Bin(BinOp::Ne, Box::new(a), Box::new(Expr::Int(0, TyTable::I32)), TyTable::I32)
                    } else {
                        a
                    }
                })
                .collect();
            let call = Expr::Call(Box::new(Call::Direct { func: sig.fid, args }));
            if sig.ret == TyTable::VOID {
                fl.push(Stmt::Expr(call));
                fl.push(Stmt::Return(None));
            } else if sig.ret == TyTable::BOOL {
                fl.push(Stmt::Return(Some(Expr::Cast { value: Box::new(call), to: TyTable::I32 })));
            } else {
                fl.push(Stmt::Return(Some(call)));
            }
            let body = fl.finish();
            cx.b.set_body(fid, body);
            cx.b.m.exports.push(ExportDef {
                symbol,
                params: params.iter().map(|(n, t)| (n.to_string(), *t)).collect(),
                ret,
            });
        }
    }
    // Static fields still need their initial values, and a library has no
    // moment that is obviously start-up: the host says when, as with 1.x.
    let init_sym = format!("{module}_init");
    let init = cx.b.func_full(&init_sym, vec![], TyTable::VOID, CallConv::Cdecl, Linkage::Exported, true);
    let inits = std::mem::take(&mut cx.static_inits);
    let components = std::mem::take(&mut cx.namespace_components);
    let mut fl = FnLower::new(cx, init, TyTable::VOID);
    for (g, ty, value) in &inits {
        let (v, _) = fl.expr(value, Some(*ty))?;
        fl.push(Stmt::Assign { place: Place::Global(*g), value: v });
    }
    for c in &components {
        fl.build_nonvisual(c, None)?;
    }
    fl.push(Stmt::Return(None));
    let body = fl.finish();
    cx.b.set_body(init, body);
    cx.b.m.exports.insert(0, ExportDef { symbol: init_sym, params: Vec::new(), ret: TyTable::VOID });
    Ok(())
}

#[derive(Clone)]
struct Sig {
    fid: FuncId,
    this: bool,
    params: Vec<TyId>,
    /// Each parameter's default value, parallel to `params`.
    defaults: Vec<Option<ast::Expr>>,
    ret: TyId,
}

/// Record a `[Dll]` extern method as the foreign function it declares, under
/// both its qualified and its bare name.
fn register_dll(cx: &mut Cx, owner: &str, m: &ast::Method) -> Result<(), String> {
    let Some(dll) = m.attrs.iter().find(|a| a.name == "Dll") else {
        return Err(format!(
            "`{}` is extern but has no [Dll(\"library\")] attribute",
            m.name
        ));
    };
    let library = match dll.args.first().map(|e| &e.kind) {
        Some(ast::ExprKind::Str(s)) => s.clone(),
        _ => return Err("[Dll] needs a library name".into()),
    };
    let symbol = dll
        .named
        .iter()
        .find(|(k, _)| k == "Entry")
        .and_then(|(_, v)| match &v.kind {
            ast::ExprKind::Str(s) => Some(s.clone()),
            _ => None,
        })
        .unwrap_or_else(|| m.name.clone());
    let conv = dll
        .named
        .iter()
        .find(|(k, _)| k == "Convention")
        .map(|(_, v)| match &v.kind {
            ast::ExprKind::Member(_, n) if n == "StdCall" => CallConv::Stdcall,
            ast::ExprKind::Member(_, n) if n == "System" => CallConv::System,
            _ => CallConv::Cdecl,
        })
        .unwrap_or(CallConv::Cdecl);
    let params: Vec<TyId> = m
        .params
        .iter()
        .map(|p| cx.resolve(&p.ty))
        .collect::<Result<_, String>>()?;
    let ret = cx.resolve(&m.ret)?;
    let sig = DllSig {
        library,
        symbol,
        conv,
        params,
        ret,
    };
    // `runtime` and libc are linked into every program; any other library a
    // `[Dll]` names is found and bound when it is first called.
    if !matches!(sig.library.as_str(), "runtime" | "c" | "libc") {
        cx.b.m.foreign_libraries.insert(sig.library.clone());
    }
    cx.dlls.insert(format!("{owner}.{}", m.name), sig.clone());
    cx.dlls.entry(m.name.clone()).or_insert(sig);
    Ok(())
}

/// An operator as it is written, for an error.
fn ast_op_text(op: ast::BinOp) -> &'static str {
    use ast::BinOp as B;
    match op {
        B::Add => "+",
        B::Sub => "-",
        B::Mul => "*",
        B::Div => "/",
        B::Rem => "%",
        B::BitAnd => "&",
        B::BitOr => "|",
        B::BitXor => "^",
        B::Shl => "<<",
        B::Shr => ">>",
        B::UShr => ">>>",
        _ => "this operator",
    }
}

/// The runtime byte-set's header: `int32 dims; int32 len`.
const BIN_HEADER: i32 = 8;

/// A slot tag in words, for an error a reader can act on.
fn describe_slot(tag: i32) -> String {
    const ARRAY: i32 = 0x100;
    const DICT: i32 = 0x200;
    if tag & ARRAY != 0 {
        return format!("a list of {}", describe_slot(tag & !ARRAY));
    }
    if tag & DICT != 0 {
        return format!("a dictionary of {}", describe_slot(tag & !DICT));
    }
    match tag {
        3 => "a whole number".into(),
        4 => "a 64-bit whole number".into(),
        6 => "a decimal number".into(),
        8 => "true or false".into(),
        9 => "text".into(),
        10 => "bytes".into(),
        13 => "a record".into(),
        14 => "a pointer".into(),
        255 => "anything".into(),
        _ => format!("slot type {tag}"),
    }
}

/// A record mapped to a database table by `[Table]`.
#[derive(Clone)]
struct TableInfo {
    table: String,
    /// `(field name, column name, is_auto)` in declaration order.
    columns: Vec<(String, String, bool)>,
    /// Each column's declared type, so a row can be read back into a record
    /// with the right `db_*` reader rather than everything as text.
    types: Vec<ast::TypeRef>,
}

/// A foreign function declared with `[Dll]`.
#[derive(Clone)]
struct DllSig {
    library: String,
    symbol: String,
    conv: CallConv,
    params: Vec<TyId>,
    ret: TyId,
}

/// A generic method awaiting instantiation.
#[derive(Clone)]
struct Template {
    owner: String,
    method: ast::Method,
    this: bool,
}

/// A monomorphic instance whose body still has to be lowered. Body lowering is
/// deferred to the driver so it never runs inside another function's lowering
/// (which already holds the one mutable borrow of `Cx`).
struct Pending {
    fid: FuncId,
    method: ast::Method,
    tvars: HashMap<String, TyId>,
    this: bool,
    /// For a lifted lambda: the environment record reached through parameter 0,
    /// and the captured variables it holds, in field order.
    env: Option<EnvPlan>,
    /// For a constructor: the record it builds and returns.
    ctor: Option<RecordId>,
}

/// How a lifted lambda reaches the variables it captured.
#[derive(Clone)]
struct EnvPlan {
    /// Record of `Ptr` fields, one per captured variable.
    rec: RecordId,
    /// `(name, cell record, value type)` per field, in order.
    captures: Vec<(String, RecordId, TyId)>,
}

/// A captured variable: its value lives in a one-field heap record (a "cell"),
/// so the declaring function and every closure over it read and write the same
/// memory — capture by reference, as the spec requires.
#[derive(Clone)]
struct Cell {
    /// An expression yielding the cell pointer in the current function.
    ptr: Expr,
    rec: RecordId,
    ty: TyId,
}

struct Cx {
    /// The standard library's commands, when the caller supplied them.
    registry: Option<kiln_ir::Registry>,
    runtime: Runtime,
    b: ModuleBuilder,
    type_ids: HashMap<String, RecordId>,
    /// Every declared type name — records, classes, static classes and enums —
    /// so `Type.Member` can be told from `value.Member`.
    type_names: std::collections::HashSet<String>,
    enums: HashMap<String, HashMap<String, i128>>,
    /// An enum used as a type is its backing integer (`int` unless declared).
    enum_backing: HashMap<String, TyId>,
    consts: HashMap<String, (TyId, ast::Expr)>,
    methods: HashMap<String, Sig>,
    /// Generic method templates, by `Type.Name` and by bare `Name`.
    generics: HashMap<String, Template>,
    /// Instantiation cache: (template key, concrete type arguments) → instance.
    mono: HashMap<(String, Vec<TyId>), Sig>,
    /// Type parameters bound while lowering one instance.
    tvars: HashMap<String, TyId>,
    /// `Result<T>` is synthesised per value type: a record `{ok, value, err}`.
    results: HashMap<TyId, RecordId>,
    /// `List<T>` is synthesised per element type: `{len, cap, data}`.
    lists: HashMap<TyId, RecordId>,
    /// `Dictionary<K,V>` per key/value pair: `{len, cap, keys, values}`.
    dicts: HashMap<(TyId, TyId), RecordId>,
    /// `HashSet<T>` per element type: a list that refuses duplicates.
    sets: HashMap<TyId, RecordId>,
    /// `[Dll]` externs, by `Type.Name` and bare `Name`.
    dlls: HashMap<String, DllSig>,
    /// Form-level state: name → its global.
    form_state: HashMap<String, (GlobalId, TyId)>,
    /// Globals with an initial value — static fields and form fields — set by
    /// a wrapper around the entry point before it runs.
    static_inits: Vec<(GlobalId, TyId, ast::Expr)>,
    /// Components with no rectangle, by id: the library whose own entry
    /// points create and address them (`core` for a timer, `net` for a server).
    nonvisual: HashMap<String, String>,
    /// `bool` fields of `[CLayout]` records, stored as a C int.
    c_bools: std::collections::HashSet<(RecordId, usize)>,
    /// Namespace-level non-visual components, built before the entry point.
    namespace_components: Vec<ast::ComponentDecl>,
    /// Component id → (global holding its runtime handle, component type).
    components: HashMap<String, (GlobalId, String)>,
    /// Generic type declarations, awaiting type arguments.
    generic_types: HashMap<String, ast::TypeDecl>,
    /// Instantiated generic types: (name, type args) → the record it became.
    type_mono: HashMap<(String, Vec<TyId>), RecordId>,
    /// Interfaces: name → the method names it declares, in order.
    interfaces: HashMap<String, Vec<String>>,
    /// An interface's value record: `{obj, fn per method}`.
    iface_records: HashMap<String, RecordId>,
    /// `(type, interface)` → the type's methods in the interface's order.
    impls: HashMap<(String, String), Vec<String>>,
    /// `[Table]` records: type name → (table name, columns).
    tables: HashMap<String, TableInfo>,
    /// Records declared `[Packed]`: laid out with no padding, and given
    /// generated Read/Write over a byte buffer.
    packed: std::collections::HashSet<String>,
    pending: Vec<Pending>,
}

impl Cx {
    /// The entry point, preceded by every global's initial value. With nothing
    /// to initialise the entry is returned unchanged, so a program without
    /// static state compiles exactly as it did.
    fn wrap_entry_with_inits(&mut self, entry: FuncId) -> Result<FuncId, String> {
        self.wrap_entry(entry, false)
    }

    /// The entry point wrapped with what has to happen around it: globals'
    /// initial values first; and for a console program with components, those
    /// components built before `Main` and the runtime's loop run after it —
    /// which is what keeps a program with a live timer or server running once
    /// `Main` returns, exactly as 1.x does.
    fn wrap_entry(&mut self, entry: FuncId, is_form: bool) -> Result<FuncId, String> {
        let components = if is_form {
            Vec::new()
        } else {
            std::mem::take(&mut self.namespace_components)
        };
        if self.static_inits.is_empty() && components.is_empty() {
            return Ok(entry);
        }
        let entry_ret = self.b.m.func(entry).ret;
        let ret = if components.is_empty() { entry_ret } else { TyTable::I32 };
        let wrapper = self.b.declare_func("$init_then_entry", vec![], ret);
        let inits = std::mem::take(&mut self.static_inits);
        let mut fl = FnLower::new(self, wrapper, ret);
        for (g, ty, init) in &inits {
            let (v, _) = fl.expr(init, Some(*ty))?;
            fl.push(Stmt::Assign {
                place: Place::Global(*g),
                value: v,
            });
        }
        for c in &components {
            fl.build_nonvisual(c, None)?;
        }
        let call = Expr::Call(Box::new(Call::Direct {
            func: entry,
            args: Vec::new(),
        }));
        if !components.is_empty() {
            fl.push(Stmt::Expr(call));
            let run = Expr::Call(Box::new(Call::Dll {
                library: "runtime".into(),
                symbol: "kn_loop_run".into(),
                conv: CallConv::Cdecl,
                args: Vec::new(),
                arg_tys: Vec::new(),
                ret: TyTable::I32,
                varargs: false,
            }));
            fl.push(Stmt::Return(Some(run)));
        } else if ret == TyTable::VOID {
            fl.push(Stmt::Expr(call));
            fl.push(Stmt::Return(None));
        } else {
            fl.push(Stmt::Return(Some(call)));
        }
        let body = fl.finish();
        self.b.set_body(wrapper, body);
        Ok(wrapper)
    }

    /// The library that owns a component type with no rectangle, or `None`
    /// for a visual one (or one the registry does not know).
    fn nonvisual_library(&self, type_name: &str) -> Option<String> {
        let d = self.registry.as_ref()?.component(&snake_case(type_name))?;
        match d.kind {
            kiln_ir::registry::ComponentKind::NonVisual => Some(d.library.clone()),
            kiln_ir::registry::ComponentKind::Visual => None,
        }
    }

    fn record_ty(&mut self, name: &str) -> TyId {
        let rid = self.type_ids[name];
        self.b.m.types.intern(TyKind::Record(rid))
    }

    fn resolve(&mut self, t: &ast::TypeRef) -> Result<TyId, String> {
        // A bound type parameter wins: inside a monomorphic instance, `T` is
        // whatever this instantiation bound it to.
        if let ast::TypeRef::Named(n) = t {
            if let Some(&bound) = self.tvars.get(n.as_str()) {
                return Ok(bound);
            }
        }
        Ok(match t {
            ast::TypeRef::Void => TyTable::VOID,
            ast::TypeRef::Named(n) => match n.as_str() {
                "int" => TyTable::I32,
                "uint" => TyTable::U32,
                "long" => TyTable::I64,
                "ulong" => TyTable::U64,
                "short" => TyTable::I16,
                "ushort" => TyTable::U16,
                "byte" => TyTable::U8,
                "sbyte" => TyTable::I8,
                "nint" => TyTable::NINT,
                "nuint" => TyTable::NUINT,
                "float" => TyTable::F32,
                "double" => TyTable::F64,
                "bool" => TyTable::BOOL,
                "char" => TyTable::CHAR,
                "string" => TyTable::STR,
                "Bytes" => TyTable::BYTES,
                "Ptr" => TyTable::PTR,
                // `Action` with no type arguments: takes nothing, returns
                // nothing. It is the shape an event handler has, so it is the
                // one a program reaches for first.
                "Action" => self.b.m.types.intern(TyKind::Func {
                    params: Vec::new(),
                    ret: TyTable::VOID,
                }),
                other => {
                    if self.type_ids.contains_key(other) {
                        self.record_ty(other)
                    } else if let Some(rid) = self.iface_records.get(other).copied() {
                        self.b.m.types.intern(TyKind::Record(rid))
                    } else if let Some(t) = self.enum_backing.get(other) {
                        *t
                    } else {
                        return Err(format!("unknown type `{other}`"));
                    }
                }
            },
            ast::TypeRef::Optional(inner) => {
                let t = self.resolve(inner)?;
                self.b.m.types.intern(TyKind::Optional(t))
            }
            ast::TypeRef::Array(elem) => {
                let e = self.resolve(elem)?;
                self.b.m.types.intern(TyKind::Array(e))
            }
            ast::TypeRef::Fixed(..) => {
                return Err(
                    "`T[N]` holds N values in place, which only a `[CLayout]` record's field can do — use `List<T>` elsewhere"
                        .into(),
                )
            }
            ast::TypeRef::FnPtr { conv, params, ret } => {
                let conv = match conv.as_deref() {
                    None | Some("Cdecl") => CallConv::Cdecl,
                    Some("Stdcall") => CallConv::Stdcall,
                    Some(other) => {
                        return Err(format!(
                            "`delegate* unmanaged[{other}]` — Kiln calls C functions as `Cdecl` or `Stdcall`"
                        ))
                    }
                };
                let mut ps = Vec::new();
                for p in params {
                    ps.push(self.resolve(p)?);
                }
                let ret = self.resolve(ret)?;
                self.b.m.types.intern(TyKind::CFunc { params: ps, ret, conv })
            }
            ast::TypeRef::Generic(n, args) => match n.as_str() {
                "List" => {
                    let e = self.resolve(&args[0])?;
                    let rid = self.list_record(e);
                    self.b.m.types.intern(TyKind::Record(rid))
                }
                // `Func<A, B, R>`: the last argument is the return type.
                "Func" => {
                    let mut tys = Vec::new();
                    for a in args {
                        tys.push(self.resolve(a)?);
                    }
                    let ret = tys.pop().unwrap_or(TyTable::VOID);
                    self.b.m.types.intern(TyKind::Func { params: tys, ret })
                }
                // `Result<T>`: a record {ok, value, err}, synthesised per T.
                "HashSet" => {
                    let e = self.resolve(&args[0])?;
                    let rid = self.set_record(e);
                    self.b.m.types.intern(TyKind::Record(rid))
                }
                "Dictionary" => {
                    if args.len() != 2 {
                        return Err("Dictionary takes a key and a value type".into());
                    }
                    let k = self.resolve(&args[0])?;
                    let v = self.resolve(&args[1])?;
                    let rid = self.dict_record(k, v);
                    self.b.m.types.intern(TyKind::Record(rid))
                }
                "Result" => {
                    let vt = match args.first() {
                        Some(a) => self.resolve(a)?,
                        None => TyTable::I32,
                    };
                    let rid = self.result_record(vt);
                    self.b.m.types.intern(TyKind::Record(rid))
                }
                // `Action<A, B>`: no return.
                "Action" => {
                    let mut tys = Vec::new();
                    for a in args {
                        tys.push(self.resolve(a)?);
                    }
                    self.b.m.types.intern(TyKind::Func {
                        params: tys,
                        ret: TyTable::VOID,
                    })
                }
                _ => {
                    if self.generic_types.contains_key(n.as_str()) {
                        let mut targs = Vec::new();
                        for a in args {
                            targs.push(self.resolve(a)?);
                        }
                        let rid = self.instantiate_type(n, &targs)?;
                        self.b.m.types.intern(TyKind::Record(rid))
                    } else {
                        return Err(format!("generic type `{n}` not yet supported"));
                    }
                }
            },
        })
    }

    /// The record standing for `Result<T>`: `{ok: bool, value: T, err: string}`.
    /// Field indices are fixed so lowering can reach them positionally.
    fn result_record(&mut self, value_ty: TyId) -> RecordId {
        if let Some(r) = self.results.get(&value_ty) {
            return *r;
        }
        let n = self.b.m.records.len();
        let rid = self.b.c_record(
            &format!("$Result{n}"),
            vec![
                ("ok", TyTable::BOOL),
                ("value", value_ty),
                ("err", TyTable::STR),
            ],
            Equality::ByValue,
        );
        self.results.insert(value_ty, rid);
        rid
    }

    /// Instantiate a generic type for concrete type arguments: a record with
    /// the fields substituted, and its methods declared and queued.
    fn instantiate_type(&mut self, name: &str, targs: &[TyId]) -> Result<RecordId, String> {
        let key = (name.to_string(), targs.to_vec());
        if let Some(r) = self.type_mono.get(&key) {
            return Ok(*r);
        }
        let td = self.generic_types[name].clone();
        if td.type_params.len() != targs.len() {
            return Err(format!(
                "`{name}` takes {} type argument(s), got {}",
                td.type_params.len(),
                targs.len()
            ));
        }
        let suffix: Vec<String> = targs.iter().map(|t| t.0.to_string()).collect();
        let inst_name = format!("{name}${}", suffix.join("_"));

        // Reserve the record so a field may refer back to the instance.
        let rid = RecordId(self.b.m.records.len() as u32);
        self.b.m.records.push(RecordDef {
            id: rid,
            name: inst_name.clone(),
            fields: Vec::new(),
            layout: Layout::Managed,
            equality: match td.kind {
                ast::TypeKind::Record => Equality::ByValue,
                _ => Equality::ByRef,
            },
        });
        let _ = self.b.m.types.intern(TyKind::Record(rid));
        self.type_mono.insert(key, rid);

        // Bind the type parameters — and the template's own name, so that a
        // member written in terms of `Box` means this instance of it.
        let mut tvars = HashMap::new();
        for (p, t) in td.type_params.iter().zip(targs.iter()) {
            tvars.insert(p.clone(), *t);
        }
        let inst_ty_early = self.b.m.types.intern(TyKind::Record(rid));
        tvars.insert(name.to_string(), inst_ty_early);
        let saved = std::mem::replace(&mut self.tvars, tvars.clone());

        let result = (|| -> Result<(), String> {
            let mut fields = Vec::new();
            for f in td.record_params.iter().chain(td.fields.iter()) {
                fields.push(FieldDef {
                    name: f.name.clone(),
                    ty: self.resolve(&f.ty)?,
                });
            }
            let (size, align, offsets) = self.c_layout(&fields);
            self.b.m.records[rid.0 as usize].fields = fields;
            self.b.m.records[rid.0 as usize].layout = Layout::C {
                size,
                align,
                offsets,
            };

            // Declare and queue every method of this instance.
            let inst_ty = self.b.m.types.intern(TyKind::Record(rid));
            for m in &td.methods {
                if !m.type_params.is_empty() || m.is_extern {
                    continue;
                }
                let this = !m.is_static && td.kind != ast::TypeKind::StaticClass;
                let mut params: Vec<(&str, TyId)> = Vec::new();
                if this && m.name != "$ctor" {
                    params.push(("this", inst_ty));
                }
                let owned: Vec<(String, TyId)> = m
                    .params
                    .iter()
                    .map(|p| Ok((p.name.clone(), self.resolve(&p.ty)?)))
                    .collect::<Result<_, String>>()?;
                for (n, t) in &owned {
                    params.push((n.as_str(), *t));
                }
                let ret = self.resolve(&m.ret)?;
                let sym = format!("{inst_name}_{}", m.name);
                let fid = self.b.declare_func(&sym, params, ret);
                let sig = Sig {
                    fid,
                    this,
                    params: owned.iter().map(|(_, t)| *t).collect(),
                    defaults: m.params.iter().map(|p| p.default.clone()).collect(),
                    ret,
                };
                self.methods
                    .insert(format!("{inst_name}.{}", m.name), sig.clone());
                let ctor = if m.name == "$ctor" { Some(rid) } else { None };
                self.pending.push(Pending {
                    fid,
                    method: m.clone(),
                    tvars: tvars.clone(),
                    this: this && ctor.is_none(),
                    env: None,
                    ctor,
                });
            }
            Ok(())
        })();
        self.tvars = saved;
        result?;
        Ok(rid)
    }

    /// The record standing for `List<T>`: `{len, cap, data}` where `data` is a
    /// contiguous buffer of `T`.
    fn list_record(&mut self, elem: TyId) -> RecordId {
        if let Some(r) = self.lists.get(&elem) {
            return *r;
        }
        let data_ty = self.b.m.types.intern(TyKind::Array(elem));
        let n = self.b.m.records.len();
        let rid = self.b.c_record(
            &format!("$List{n}"),
            vec![
                ("len", TyTable::I32),
                ("cap", TyTable::I32),
                ("data", data_ty),
            ],
            Equality::ByRef,
        );
        self.lists.insert(elem, rid);
        rid
    }

    /// The record standing for `Dictionary<K,V>`: parallel key and value
    /// buffers. Lookup is a linear scan — correct, and honest about being a
    /// placeholder until the runtime's hashed dictionary is wired in.
    fn dict_record(&mut self, k: TyId, v: TyId) -> RecordId {
        if let Some(r) = self.dicts.get(&(k, v)) {
            return *r;
        }
        let keys_ty = self.b.m.types.intern(TyKind::Array(k));
        let vals_ty = self.b.m.types.intern(TyKind::Array(v));
        let index_ty = self.b.m.types.intern(TyKind::Array(TyTable::I32));
        let n = self.b.m.records.len();
        let rid = self.b.c_record(
            &format!("$Dict{n}"),
            vec![
                ("len", TyTable::I32),
                ("cap", TyTable::I32),
                ("keys", keys_ty),
                ("values", vals_ty),
                ("index", index_ty),
            ],
            Equality::ByRef,
        );
        self.dicts.insert((k, v), rid);
        rid
    }

    /// The interface a value type stands for, if it is an interface record.
    fn as_interface(&self, ty: TyId) -> Option<String> {
        if let TyKind::Record(rid) = *self.b.m.types.kind(ty) {
            return self
                .iface_records
                .iter()
                .find(|(_, r)| **r == rid)
                .map(|(n, _)| n.clone());
        }
        None
    }

    /// The declared name of a record type, if it has one.
    fn record_name(&self, ty: TyId) -> Option<String> {
        if let TyKind::Record(rid) = *self.b.m.types.kind(ty) {
            return Some(self.b.m.record(rid).name.clone());
        }
        None
    }

    /// The record standing for `HashSet<T>`: the same shape as a list, with
    /// `Add` checking for the element first.
    fn set_record(&mut self, elem: TyId) -> RecordId {
        if let Some(r) = self.sets.get(&elem) {
            return *r;
        }
        let data_ty = self.b.m.types.intern(TyKind::Array(elem));
        let index_ty = self.b.m.types.intern(TyKind::Array(TyTable::I32));
        let n = self.b.m.records.len();
        let rid = self.b.c_record(
            &format!("$Set{n}"),
            vec![
                ("len", TyTable::I32),
                ("cap", TyTable::I32),
                ("data", data_ty),
                ("index", index_ty),
            ],
            Equality::ByRef,
        );
        self.sets.insert(elem, rid);
        rid
    }

    /// If `ty` is a synthesised `HashSet`, its record and element type.
    fn as_set(&self, ty: TyId) -> Option<(RecordId, TyId)> {
        if let TyKind::Record(rid) = *self.b.m.types.kind(ty) {
            if self.b.m.record(rid).name.starts_with("$Set") {
                let data = self.b.m.record(rid).fields[LIST_DATA].ty;
                if let TyKind::Array(e) = *self.b.m.types.kind(data) {
                    return Some((rid, e));
                }
            }
        }
        None
    }

    /// If `ty` is a synthesised `Dictionary`, its record, key and value types.
    fn as_dict(&self, ty: TyId) -> Option<(RecordId, TyId, TyId)> {
        if let TyKind::Record(rid) = *self.b.m.types.kind(ty) {
            if self.b.m.record(rid).name.starts_with("$Dict") {
                let kt = self.b.m.record(rid).fields[DICT_KEYS].ty;
                let vt = self.b.m.record(rid).fields[DICT_VALUES].ty;
                if let (TyKind::Array(k), TyKind::Array(v)) = (
                    self.b.m.types.kind(kt).clone(),
                    self.b.m.types.kind(vt).clone(),
                ) {
                    return Some((rid, k, v));
                }
            }
        }
        None
    }

    /// If `ty` is a synthesised `List`, its record and element type.
    fn as_list(&self, ty: TyId) -> Option<(RecordId, TyId)> {
        if let TyKind::Record(rid) = *self.b.m.types.kind(ty) {
            if self.b.m.record(rid).name.starts_with("$List") {
                let data = self.b.m.record(rid).fields[LIST_DATA].ty;
                if let TyKind::Array(e) = *self.b.m.types.kind(data) {
                    return Some((rid, e));
                }
            }
        }
        None
    }

    /// If `ty` is a synthesised `Result`, its record and value type.
    fn as_result(&self, ty: TyId) -> Option<(RecordId, TyId)> {
        if let TyKind::Record(rid) = *self.b.m.types.kind(ty) {
            if self.b.m.record(rid).name.starts_with("$Result") {
                let vt = self.b.m.record(rid).fields[RESULT_VALUE].ty;
                return Some((rid, vt));
            }
        }
        None
    }

    /// Whether `ty` is a reference — what `where T : class` admits: a class,
    /// record or interface value, text, a byte buffer, an array or a
    /// collection. Numbers, flags, characters, pointers and `T?` are not.
    fn is_reference_type(&self, ty: TyId) -> bool {
        matches!(
            self.b.m.types.kind(ty),
            TyKind::Record(_)
                | TyKind::Str
                | TyKind::Bytes
                | TyKind::Array(_)
                | TyKind::Dict(..)
                | TyKind::Set(_)
                | TyKind::Func { .. }
        )
    }

    /// Whether `new T()` would find something to run — what `where T : new()`
    /// requires. A collection starts empty; a record with no declared
    /// constructor is zeroed; a declared constructor must be callable with no
    /// arguments.
    fn can_new_without_arguments(&self, ty: TyId) -> bool {
        if self.as_list(ty).is_some() || self.as_dict(ty).is_some() || self.as_set(ty).is_some() {
            return true;
        }
        let TyKind::Record(rid) = *self.b.m.types.kind(ty) else {
            return false;
        };
        let name = self.b.m.record(rid).name.clone();
        match self.methods.get(&format!("{name}.$ctor")) {
            Some(sig) => sig.defaults.iter().all(|d| d.is_some()),
            None => true,
        }
    }

    fn c_layout(&self, fields: &[FieldDef]) -> (i64, i64, Vec<i64>) {
        let mut offset = 0i64;
        let mut align = 1i64;
        let mut offsets = Vec::new();
        for f in fields {
            let (sz, al) = self.size_align(f.ty);
            offset = round_up(offset, al);
            offsets.push(offset);
            offset += sz;
            align = align.max(al);
        }
        (round_up(offset, align.max(1)), align, offsets)
    }

    /// Size and alignment of a C field: storage held in place is its element's
    /// size times the count, aligned as one element is.
    fn size_align(&self, ty: TyId) -> (i64, i64) {
        match *self.b.m.types.kind(ty) {
            TyKind::Inline { elem, count } => {
                let (esz, eal) = match *self.b.m.types.kind(elem) {
                    TyKind::Record(rid) => match &self.b.m.record(rid).layout {
                        Layout::C { size, align, .. } => (*size, *align),
                        Layout::Managed => (0, 1),
                    },
                    _ => {
                        let s = self.scalar_size(elem);
                        (s, s)
                    }
                };
                (esz * (count.max(1) as i64), eal.max(1))
            }
            _ => self.value_size_align(ty),
        }
    }

    /// Size and alignment of a value held in a slot, a field or a buffer cell,
    /// as LLVM lays out the type the emitter spells for it. A `T?` is
    /// `{ T, i1 }`, a function value is the `{fn, env}` pair and a tuple is a
    /// struct: none of them is one pointer wide, and measuring them as one gave
    /// a record with two optional fields half the allocation it writes to.
    fn value_size_align(&self, ty: TyId) -> (i64, i64) {
        let ptr = (self.b.m.target.ptr_bits / 8) as i64;
        match self.b.m.types.kind(ty) {
            TyKind::Optional(inner) => {
                let (sz, al) = self.value_size_align(*inner);
                (round_up(sz + 1, al.max(1)), al.max(1))
            }
            TyKind::Func { .. } => (ptr * 2, ptr),
            TyKind::Tuple(elems) => {
                let mut offset = 0i64;
                let mut align = 1i64;
                for e in elems.clone() {
                    let (sz, al) = self.value_size_align(e);
                    offset = round_up(offset, al.max(1)) + sz;
                    align = align.max(al);
                }
                (round_up(offset, align), align)
            }
            TyKind::Void => (0, 1),
            _ => {
                let s = self.scalar_size(ty);
                (s, s)
            }
        }
    }

    pub(crate) fn scalar_size(&self, ty: TyId) -> i64 {
        match self.b.m.types.kind(ty) {
            TyKind::Optional(_) | TyKind::Func { .. } | TyKind::Tuple(_) => {
                self.value_size_align(ty).0
            }
            TyKind::Bool | TyKind::I8 | TyKind::U8 => 1,
            TyKind::I16 | TyKind::U16 => 2,
            TyKind::I32 | TyKind::U32 | TyKind::Char | TyKind::F32 => 4,
            TyKind::I64 | TyKind::U64 | TyKind::F64 => 8,
            TyKind::Nint | TyKind::Nuint => (self.b.m.target.ptr_bits / 8) as i64,
            _ => (self.b.m.target.ptr_bits / 8) as i64,
        }
    }

    fn lower_method(
        &mut self,
        td: &ast::TypeDecl,
        m: &ast::Method,
        sig: &Sig,
    ) -> Result<(), String> {
        let _ = td;
        let ctor = if m.name == "$ctor" {
            match *self.b.m.types.kind(sig.ret) {
                TyKind::Record(rid) => Some(rid),
                _ => None,
            }
        } else {
            None
        };
        self.lower_into(sig.fid, m, sig.this, None, ctor)
    }

    /// Lower a method body into an already-declared function. Parameter types
    /// come from the declared function, so this serves plain methods and
    /// monomorphic instances alike.
    fn lower_into(
        &mut self,
        fid: FuncId,
        m: &ast::Method,
        this: bool,
        env: Option<EnvPlan>,
        ctor: Option<RecordId>,
    ) -> Result<(), String> {
        let ret = self.b.m.func(fid).ret;
        let ptys: Vec<TyId> = self.b.m.func(fid).params.iter().map(|p| p.ty).collect();
        let mut fl = FnLower::new(self, fid, ret);
        // Names this body's lambdas close over; such locals live in cells.
        fl.captured = captured_names(&m.body, m.expr_body.as_ref());
        if this {
            fl.scope.insert("this".into(), (LocalId(0), ptys[0]));
        }
        let base = if this { 1 } else { 0 };
        for (i, p) in m.params.iter().enumerate() {
            fl.scope
                .insert(p.name.clone(), (LocalId((base + i) as u32), ptys[base + i]));
        }
        // A lifted lambda reaches its captures through the environment record
        // in parameter 0: `env.field[i]` is the cell pointer for capture `i`.
        if let Some(plan) = env {
            let env_ty = fl.cx.b.m.types.intern(TyKind::Record(plan.rec));
            let env_val = Expr::Cast {
                value: Box::new(Expr::Local(LocalId(0))),
                to: env_ty,
            };
            for (i, (name, cell_rec, vty)) in plan.captures.iter().enumerate() {
                let cell_ty = fl.cx.b.m.types.intern(TyKind::Record(*cell_rec));
                let ptr = Expr::Cast {
                    value: Box::new(Expr::Field(Box::new(env_val.clone()), i)),
                    to: cell_ty,
                };
                fl.cells.insert(
                    name.clone(),
                    Cell {
                        ptr,
                        rec: *cell_rec,
                        ty: *vty,
                    },
                );
            }
        }
        // A parameter a lambda closes over has to move into a cell, so the
        // closure and the body share one location rather than two copies.
        let captured_params: Vec<(String, LocalId, TyId)> = m
            .params
            .iter()
            .enumerate()
            .filter(|(_, p)| fl.captured.contains(&p.name))
            .map(|(i, p)| (p.name.clone(), LocalId((base + i) as u32), ptys[base + i]))
            .collect();
        for (name, lid, ty) in captured_params {
            fl.scope.remove(&name);
            fl.make_cell(&name, ty, Expr::Local(lid))?;
        }
        // A constructor starts with a zeroed instance bound to `this`, runs the
        // body (whose bare field names resolve through it), and returns it.
        let this_local = if let Some(rid) = ctor {
            let rty = fl.cx.b.m.types.intern(TyKind::Record(rid));
            let zeros: Vec<Expr> = fl
                .cx
                .b
                .m
                .record(rid)
                .fields
                .iter()
                .map(|f| f.ty)
                .collect::<Vec<_>>()
                .into_iter()
                .map(|t| zero_of(&fl.cx.b.m.types, t))
                .collect();
            let l = fl.cx.b.add_local(fid, "this", rty);
            fl.push(Stmt::Let {
                local: l,
                value: Expr::MakeRecord(rid, zeros),
            });
            fl.scope.insert("this".into(), (l, rty));
            Some(l)
        } else {
            None
        };
        if let Some(e) = &m.expr_body {
            // `=> e` is `{ return e; }` — checked and converted as that is.
            if ret == TyTable::VOID {
                let (val, _) = fl.expr(e, None)?;
                fl.push(Stmt::Expr(val));
            } else {
                fl.stmt(&ast::Stmt {
                    kind: ast::StmtKind::Return(Some(e.clone())),
                    leading: Vec::new(),
                    span: e.span,
                })?;
            }
        } else {
            for s in &m.body {
                fl.stmt(s)?;
            }
        }
        if let Some(l) = this_local {
            fl.push(Stmt::Return(Some(Expr::Local(l))));
        }
        let body = fl.finish();
        self.b.set_body(fid, body);
        Ok(())
    }

    /// Emit a form's build sequence: start the UI, create each component, set
    /// its properties, bind its handlers, then run the event loop. This mirrors
    /// what the 1.x backend emits, so it meets the same `kn_ui_*` interface.
    fn lower_form(&mut self, fid: FuncId, f: &ast::FormDecl) -> Result<(), String> {
        let mut title = "Kiln Application".to_string();
        let (mut width, mut height) = (800i64, 600i64);
        for (n, v) in &f.properties {
            match n.as_str() {
                "Title" => title = literal_text(v)?,
                "Width" => width = literal_text(v)?.parse().unwrap_or(800),
                "Height" => height = literal_text(v)?.parse().unwrap_or(600),
                _ => {}
            }
        }
        let mut fl = FnLower::new(self, fid, TyTable::I32);
        fl.push(Stmt::Expr(ui_call(
            "kn_ui_init",
            vec![
                Expr::Str(title.clone()),
                Expr::Int(width as i128, TyTable::I32),
                Expr::Int(height as i128, TyTable::I32),
            ],
            vec![TyTable::STR, TyTable::I32, TyTable::I32],
            TyTable::I32,
        )));
        // The root window is handle 1, as the runtime assigns it.
        let root = Expr::Int(1, TyTable::I64);
        // The accessibility tree needs every element announced; the window is
        // named by its title, never by the identifier in the source.
        fl.push(Stmt::Expr(ui_a11y(
            root.clone(),
            1, // KN_ROLE_WINDOW
            Expr::Str(title.clone()),
        )));
        for (n, v) in &f.properties {
            if matches!(n.as_str(), "Title" | "Width" | "Height") {
                continue;
            }
            let text = literal_text(v)?;
            fl.push(Stmt::Expr(ui_set(
                root.clone(),
                &snake_case(n),
                Expr::Str(text),
            )));
        }

        for c in &f.components {
            if fl.cx.nonvisual.contains_key(&c.id) {
                continue;
            }
            let (g, _) = fl.cx.components[&c.id];
            let handle = ui_call(
                "kn_ui_create",
                vec![root.clone(), Expr::Str(snake_case(&c.type_name))],
                vec![TyTable::I64, TyTable::STR],
                TyTable::I64,
            );
            fl.push(Stmt::Assign {
                place: Place::Global(g),
                value: handle,
            });
            for (n, v) in &c.properties {
                let text = literal_text(v)?;
                fl.push(Stmt::Expr(ui_set(
                    Expr::Global(g),
                    &snake_case(n),
                    Expr::Str(text),
                )));
            }
            let role = match snake_case(&c.type_name).as_str() {
                "button" => 2, // KN_ROLE_BUTTON
                "label" => 3,  // KN_ROLE_LABEL
                _ => 0,
            };
            let name = c
                .properties
                .iter()
                .find(|(n, _)| n == "Text")
                .and_then(|(_, v)| literal_text(v).ok());
            fl.push(Stmt::Expr(ui_a11y(
                Expr::Global(g),
                role,
                match name {
                    Some(t) => Expr::Str(t),
                    None => Expr::Null(TyTable::STR),
                },
            )));
            for (event, handler) in &c.handlers {
                let fid = match handler {
                    ast::HandlerRef::Method(nm) => {
                        fl.cx
                            .methods
                            .get(&format!("{}.{}", f.name, nm))
                            .ok_or_else(|| format!("`{nm}` is not a method of `{}`", f.name))?
                            .fid
                    }
                    // A lambda written at the wiring site becomes a handler of
                    // its own. A form's state lives in globals, so one that
                    // touches form state needs no environment pointer and binds
                    // on the ABI as it stands.
                    ast::HandlerRef::Lambda(lam) => {
                        if !lam.params.is_empty() {
                            return Err("an event handler lambda takes no parameters yet".into());
                        }
                        let n = fl.cx.b.m.funcs.len();
                        let sym = format!("{}_{}_{}", f.name, c.id, snake_case(event));
                        let _ = n;
                        let hid = fl.cx.b.declare_func(&sym, vec![], TyTable::VOID);
                        let method = ast::Method {
                            leading: Vec::new(),
                            attrs: Vec::new(),
                            is_extern: false,
                            vis: ast::Vis::Private,
                            is_static: true,
                            name: sym.clone(),
                            type_params: Vec::new(),
                            constraints: Vec::new(),
                            params: Vec::new(),
                            ret: ast::TypeRef::Void,
                            body: match &lam.body {
                                ast::LambdaBody::Block(b) => b.clone(),
                                ast::LambdaBody::Expr(e) => vec![ast::Stmt {
                                    leading: Vec::new(),
                                    kind: ast::StmtKind::Expr((**e).clone()),
                                    span: Default::default(),
                                }],
                            },
                            expr_body: None,
                            doc: None,
                            span: Default::default(),
                        };
                        fl.cx.pending.push(Pending {
                            fid: hid,
                            method,
                            tvars: HashMap::new(),
                            this: false,
                            env: None,
                            ctor: None,
                        });
                        hid
                    }
                };
                // Bound as a (function, null environment) pair, the same as a
                // handler wired in code — so `-= OnGo` finds one wired here.
                // An event that hands its handler arguments (a grid's row) is
                // refused by the environment binding with 2, and bound the
                // older way instead.
                let on_env = ui_call(
                    "kn_ui_on_env",
                    vec![
                        Expr::Global(g),
                        Expr::Str(snake_case(event)),
                        Expr::FuncPtr(fid),
                        Expr::Null(TyTable::PTR),
                    ],
                    vec![TyTable::I64, TyTable::STR, TyTable::PTR, TyTable::PTR],
                    TyTable::I32,
                );
                fl.push(Stmt::If {
                    cond: Expr::Bin(
                        BinOp::Eq,
                        Box::new(on_env),
                        Box::new(Expr::Int(2, TyTable::I32)),
                        TyTable::I32,
                    ),
                    then: vec![Stmt::Expr(ui_call(
                        "kn_ui_on",
                        vec![Expr::Global(g), Expr::Str(snake_case(event)), Expr::FuncPtr(fid)],
                        vec![TyTable::I64, TyTable::STR, TyTable::PTR],
                        TyTable::I32,
                    ))],
                    els: vec![],
                });
            }
        }

        // Components with no rectangle — the form's own and any declared at
        // namespace level — are built once the window exists, so a handler
        // they fire can already touch it.
        let nv: Vec<ast::ComponentDecl> = f
            .components
            .iter()
            .filter(|c| fl.cx.nonvisual.contains_key(&c.id))
            .cloned()
            .chain(fl.cx.namespace_components.clone())
            .collect();
        for c in &nv {
            fl.build_nonvisual(c, Some(&f.name))?;
        }

        // A form's `Main`, if it has one, runs once the window and every
        // component exist and before the first event — so it can set a
        // property, wire a handler or load data the form shows. This is the
        // point 1.x ran `sub main` at, and `kiln migrate` relies on it.
        if let Some(main) = fl.cx.methods.get(&format!("{}.Main", f.name)).cloned() {
            fl.push(Stmt::Expr(Expr::Call(Box::new(Call::Direct {
                func: main.fid,
                args: Vec::new(),
            }))));
        }

        let rc = fl.new_local("$rc", TyTable::I32);
        fl.push(Stmt::Let {
            local: rc,
            value: ui_call("kn_ui_run", vec![], vec![], TyTable::I32),
        });
        fl.push(Stmt::Expr(ui_call(
            "kn_ui_shutdown",
            vec![],
            vec![],
            TyTable::VOID,
        )));
        fl.push(Stmt::Return(Some(Expr::Local(rc))));
        let body = fl.finish();
        self.b.set_body(fid, body);
        Ok(())
    }

    /// Lower every queued monomorphic instance. Lowering one may queue more
    /// (a generic calling another generic), so this runs to a fixed point.
    fn drain_pending(&mut self) -> Result<(), String> {
        while let Some(p) = self.pending.pop() {
            let saved = std::mem::replace(&mut self.tvars, p.tvars);
            let r = self.lower_into(p.fid, &p.method, p.this, p.env, p.ctor);
            self.tvars = saved;
            r?;
        }
        Ok(())
    }
}

fn round_up(x: i64, align: i64) -> i64 {
    if align <= 1 {
        x
    } else {
        (x + align - 1) / align * align
    }
}

fn const_int(e: &ast::Expr) -> Result<i128, String> {
    match &e.kind {
        ast::ExprKind::Int(v) => Ok(*v),
        ast::ExprKind::Unary(ast::UnOp::Neg, inner) => Ok(-const_int(inner)?),
        _ => Err("enum member must be a constant integer".into()),
    }
}

// ─── function lowering ──────────────────────────────────────────────────────

struct FnLower<'a> {
    cx: &'a mut Cx,
    fid: FuncId,
    ret: TyId,
    scope: HashMap<String, (LocalId, TyId)>,
    /// Captured variables, by name: their value lives in a heap cell.
    cells: HashMap<String, Cell>,
    /// Names the lambdas in this body close over (an over-approximation: a
    /// local with one of these names is celled whether or not it is really
    /// captured, which is safe and costs one indirection).
    captured: std::collections::HashSet<String>,
    blocks: Vec<Vec<Stmt>>,
    /// Deferred work, one frame per lexical block; each frame holds one group
    /// per `defer` (a single `defer` can lower to several statements). KIR has
    /// no `defer`, so a frame is copied to every edge that leaves its block:
    /// the groups unwind in reverse, the statements inside a group do not.
    defers: Vec<Vec<Vec<Stmt>>>,
    /// `defers.len()` on entry to each enclosing loop body, so `break` and
    /// `continue` know which frames they are leaving.
    loop_frames: Vec<usize>,
    /// The step of each enclosing loop (a `for`'s increment, a `foreach`'s
    /// counter bump). `continue` must run it, or the loop never advances.
    loop_steps: Vec<Vec<Stmt>>,
    /// Each enclosing `switch` statement: the `loop_frames` depth it pushed,
    /// and the flag a `continue` inside it sets.
    switch_frames: Vec<(usize, LocalId)>,
    /// `T?` locals proven non-null by an enclosing `if`, so reading one yields
    /// the `T` rather than the optional.
    narrowed: std::collections::HashSet<String>,
}

impl<'a> FnLower<'a> {
    fn new(cx: &'a mut Cx, fid: FuncId, ret: TyId) -> FnLower<'a> {
        FnLower {
            cx,
            fid,
            ret,
            scope: HashMap::new(),
            cells: HashMap::new(),
            captured: std::collections::HashSet::new(),
            blocks: vec![Vec::new()],
            defers: vec![Vec::new()],
            loop_frames: Vec::new(),
            loop_steps: Vec::new(),
            switch_frames: Vec::new(),
            narrowed: std::collections::HashSet::new(),
        }
    }

    /// Copy deferred statements from the innermost frame down to `floor`,
    /// innermost first and in reverse declaration order — the unwinding order.
    fn emit_defers_to(&mut self, floor: usize) {
        let mut out = Vec::new();
        for frame in self.defers[floor..].iter().rev() {
            for group in frame.iter().rev() {
                out.extend(group.iter().cloned());
            }
        }
        for s in out {
            self.push(s);
        }
    }

    fn push(&mut self, s: Stmt) {
        self.blocks.last_mut().unwrap().push(s);
    }

    fn finish(mut self) -> Vec<Stmt> {
        self.emit_defers_to(0);
        self.blocks.pop().unwrap()
    }

    /// Move a value into a fresh heap cell and register `name` as captured.
    /// The cell is a one-field record, so the declaring function and every
    /// closure over it read and write the same memory.
    fn make_cell(&mut self, name: &str, ty: TyId, init: Expr) -> Result<(), String> {
        let n = self.cx.b.m.records.len();
        let rec = self
            .cx
            .b
            .c_record(&format!("$cell{n}"), vec![("v", ty)], Equality::ByRef);
        let cell_ty = self.cx.b.m.types.intern(TyKind::Record(rec));
        let lid = self
            .cx
            .b
            .add_local(self.fid, &format!("${name}$cell"), cell_ty);
        self.push(Stmt::Let {
            local: lid,
            value: Expr::MakeRecord(rec, vec![init]),
        });
        self.cells.insert(
            name.to_string(),
            Cell {
                ptr: Expr::Local(lid),
                rec,
                ty,
            },
        );
        Ok(())
    }

    /// Declare a variable, in a cell when a lambda closes over its name.
    fn declare_var(&mut self, name: &str, ty: TyId, init: Expr) -> Result<(), String> {
        if self.captured.contains(name) {
            return self.make_cell(name, ty, init);
        }
        let lid = self.new_local(name, ty);
        self.scope.insert(name.to_string(), (lid, ty));
        self.push(Stmt::Let {
            local: lid,
            value: init,
        });
        Ok(())
    }

    fn new_local(&mut self, name: &str, ty: TyId) -> LocalId {
        let id = self.cx.b.add_local(self.fid, name, ty);
        id
    }

    fn tt(&self) -> &TyTable {
        &self.cx.b.m.types
    }

    // ─── statements ──────────────────────────────────────────────────────

    fn lower_body(&mut self, stmts: &[ast::Stmt]) -> Result<Vec<Stmt>, String> {
        self.blocks.push(Vec::new());
        self.defers.push(Vec::new());
        for s in stmts {
            self.stmt(s)?;
        }
        // Leaving the block by falling off its end also runs its defers.
        let floor = self.defers.len() - 1;
        self.emit_defers_to(floor);
        self.defers.pop();
        Ok(self.blocks.pop().unwrap())
    }

    fn stmt(&mut self, s: &ast::Stmt) -> Result<(), String> {
        // A marker per source statement: this is the line table.
        if s.span.line > 0 {
            self.push(Stmt::Line(s.span.line));
        }
        match &s.kind {
            ast::StmtKind::Local {
                name, ty, value, ..
            } => {
                let hint = match ty {
                    Some(t) => Some(self.cx.resolve(t)?),
                    None => None,
                };
                let (mut val, mut vty) = self.expr(value, hint)?;
                if let Some(h) = hint {
                    (val, vty) = self.settle(val, vty, h, &format!("`{name}`"))?;
                }
                let _ = vty;
                let lty = hint.unwrap_or(vty);
                self.declare_var(name, lty, val)?;
            }
            ast::StmtKind::Assign { target, op, value } => {
                // `count.Text = "..."` sets a component property at run time.
                if let ast::ExprKind::Member(recv, prop) = &target.kind {
                    if let ast::ExprKind::Ident(id) = &recv.kind {
                        if let Some(g) = self.cx.components.get(id).map(|(g, _)| *g) {
                            let type_name = self.cx.components.get(id).map(|(_, t)| t.clone());
                            let is_property = type_name.is_some_and(|t| {
                                self.cx.registry.as_ref().is_some_and(|r| {
                                    r.component(&snake_case(&t)).is_some_and(|d| {
                                        d.properties.iter().any(|p| p.name == snake_case(prop))
                                    })
                                })
                            });
                            // `title.Text += "!"` on a property is `title.Text =
                            // title.Text + "!"`; on an event it wires a handler.
                            if *op != ast::AssignOp::Eq && is_property {
                                let bop = match op {
                                    ast::AssignOp::Add => ast::BinOp::Add,
                                    ast::AssignOp::Sub => ast::BinOp::Sub,
                                    ast::AssignOp::Mul => ast::BinOp::Mul,
                                    ast::AssignOp::Div => ast::BinOp::Div,
                                    ast::AssignOp::Rem => ast::BinOp::Rem,
                                    ast::AssignOp::Eq | ast::AssignOp::NullCoalesce => {
                                        return Err("`??=` on a component property is not supported".into())
                                    }
                                };
                                let expanded = ast::Stmt {
                                    kind: ast::StmtKind::Assign {
                                        target: target.clone(),
                                        op: ast::AssignOp::Eq,
                                        value: ast::Expr {
                                            kind: ast::ExprKind::Binary(
                                                bop,
                                                Box::new(target.clone()),
                                                Box::new(value.clone()),
                                            ),
                                            span: value.span,
                                        },
                                    },
                                    leading: Vec::new(),
                                    span: s.span,
                                };
                                return self.stmt(&expanded);
                            }
                            if *op != ast::AssignOp::Eq {
                                // `go.Click += handler` in code wires an event
                                // at run time; `-=` unwires it.
                                if matches!(op, ast::AssignOp::Add | ast::AssignOp::Sub) {
                                    return self.wire_event(
                                        g,
                                        prop,
                                        value,
                                        *op == ast::AssignOp::Add,
                                    );
                                }
                                return Err(
                                    "compound assignment to a component property is not supported"
                                        .into(),
                                );
                            }
                            let (v, vty) = self.expr(value, None)?;
                            // Properties cross as text, whatever the value is.
                            let text = if vty == TyTable::STR {
                                v
                            } else {
                                self.build_string(vec![(String::new(), Some((v, vty)))]).0
                            };
                            if let Some(lib) = self.cx.nonvisual.get(id).cloned() {
                                self.push(Stmt::Expr(component_call(
                                    &lib,
                                    "set",
                                    vec![Expr::Global(g), Expr::Str(snake_case(prop)), text],
                                    vec![TyTable::I64, TyTable::STR, TyTable::STR],
                                    TyTable::I32,
                                )));
                                return Ok(());
                            }
                            self.push(Stmt::Expr(ui_set(Expr::Global(g), &snake_case(prop), text)));
                            return Ok(());
                        }
                    }
                }
                // `d[k] = v` updates in place, or appends when the key is new.
                if let ast::ExprKind::Index(base, key) = &target.kind {
                    let (dv, dty) = self.expr_raw(base, None)?;
                    if self.cx.as_dict(dty).is_some() {
                        if *op != ast::AssignOp::Eq {
                            return Err(
                                "compound assignment to a Dictionary entry is not supported".into(),
                            );
                        }
                        let holder = self.new_local("$dict", dty);
                        self.push(Stmt::Let {
                            local: holder,
                            value: dv,
                        });
                        self.dict_set(holder, dty, key, value)?;
                        return Ok(());
                    }
                }
                let place = self.place(target)?;
                // The type comes from the place already built, not from lowering
                // the target again: that ran any call in it a second time, so
                // `xs[Next()] = v` advanced twice.
                let pty = match self.type_of_place(&place) {
                    Some(t) => t,
                    None => self.place_ty(target)?,
                };
                let (rhs, rty) = self.expr(value, Some(pty))?;
                let (rhs, rty) = self.settle(rhs, rty, pty, "this assignment")?;
                // A number stored where a number of another width lives is
                // converted — a `bool` into a C `BOOL` field, an `int` into a
                // `long`.
                let rhs = if rty != pty
                    && !self.tt().is_pointer(rty)
                    && !self.tt().is_pointer(pty)
                    && !matches!(self.tt().kind(pty), TyKind::Optional(_))
                    && !matches!(self.tt().kind(rty), TyKind::Optional(_) | TyKind::Record(_) | TyKind::Func { .. })
                {
                    Expr::Cast { value: Box::new(rhs), to: pty }
                } else {
                    rhs
                };
                let value = if *op == ast::AssignOp::Eq {
                    rhs
                } else if *op == ast::AssignOp::Add && pty == TyTable::STR {
                    // `s += t` appends: a new string, not pointer arithmetic.
                    let cur = self.read_place(&place);
                    self.build_string(vec![
                        (String::new(), Some((cur, TyTable::STR))),
                        (String::new(), Some((rhs, TyTable::STR))),
                    ])
                    .0
                } else {
                    let cur = self.read_place(&place);
                    let bop = match op {
                        ast::AssignOp::Add => BinOp::Add,
                        ast::AssignOp::Sub => BinOp::Sub,
                        ast::AssignOp::Mul => BinOp::Mul,
                        ast::AssignOp::Div => BinOp::Div,
                        ast::AssignOp::Rem => BinOp::Rem,
                        _ => return Err("unsupported compound assignment".into()),
                    };
                    Expr::Bin(bop, Box::new(cur), Box::new(rhs), pty)
                };
                self.push(Stmt::Assign { place, value });
            }
            ast::StmtKind::Expr(e) => {
                if let Some(()) = self.try_console(e)? {
                    return Ok(());
                }
                // A statement has to do something. A literal or a name on its
                // own does nothing, and was silently accepted — which is how a
                // migrated `match` compiled to a string that did nothing.
                if matches!(
                    e.kind,
                    ast::ExprKind::Str(_)
                        | ast::ExprKind::Int(_)
                        | ast::ExprKind::Float(..)
                        | ast::ExprKind::Bool(_)
                        | ast::ExprKind::Null
                        | ast::ExprKind::Ident(_)
                        | ast::ExprKind::Interp(_)
                ) {
                    return Err("this value on its own does nothing — a statement is a call, an assignment or a declaration".into());
                }
                let (val, _) = self.expr(e, None)?;
                self.push(Stmt::Expr(val));
            }
            ast::StmtKind::Return(v) => match v {
                None => {
                    self.emit_defers_to(0);
                    self.push(Stmt::Return(None))
                }
                Some(e) => {
                    let (val, vty) = self.expr(e, Some(self.ret))?;
                    let want = self.ret;
                    let (val, vty) = self.settle(val, vty, want, "`return`")?;
                    // A number returned where a number of another width is
                    // declared is converted: `long Calls() => count;`.
                    let is_num = |t: TyId| {
                        matches!(
                            self.tt().kind(t),
                            TyKind::I8 | TyKind::I16 | TyKind::I32 | TyKind::I64
                                | TyKind::U8 | TyKind::U16 | TyKind::U32 | TyKind::U64
                                | TyKind::Nint | TyKind::Nuint | TyKind::F32 | TyKind::F64
                                | TyKind::Char
                        )
                    };
                    let val = if vty != want && is_num(vty) && is_num(want) {
                        Expr::Cast { value: Box::new(val), to: want }
                    } else {
                        val
                    };
                    // `return x;` from a Result-returning method is `Ok(x)`
                    // unless the value already is a Result.
                    let val = match self.cx.as_result(self.ret) {
                        Some((rid, _)) if vty != self.ret => Expr::MakeRecord(
                            rid,
                            vec![Expr::Bool(true), val, Expr::Str(String::new())],
                        ),
                        _ => val,
                    };
                    self.emit_defers_to(0);
                    self.push(Stmt::Return(Some(val)));
                }
            },
            ast::StmtKind::If { cond, then, els } => {
                let c = self.condition(cond)?;
                // `if (x != null)` proves `x` present in the then-branch;
                // `if (x == null)` proves it in the else-branch.
                let (narrow_then, narrow_else) = null_test_target(cond);
                let saved = self.narrowed.clone();
                if let Some(n) = &narrow_then {
                    self.narrowed.insert(n.clone());
                }
                let then_b = self.lower_body(then);
                self.narrowed = saved.clone();
                if let Some(n) = &narrow_else {
                    self.narrowed.insert(n.clone());
                }
                let els_b = self.lower_body(els);
                self.narrowed = saved;
                let (then_b, els_b) = (then_b?, els_b?);
                self.push(Stmt::If {
                    cond: c,
                    then: then_b,
                    els: els_b,
                });
            }
            ast::StmtKind::While { cond, body } => {
                // loop { if (!cond) break; body }
                let c = self.condition(cond)?;
                let mut inner = vec![Stmt::If {
                    cond: Expr::Not(Box::new(c)),
                    then: vec![Stmt::Break],
                    els: vec![],
                }];
                self.loop_frames.push(self.defers.len());
                self.loop_steps.push(Vec::new());
                let body_b = self.lower_body(body);
                self.loop_steps.pop();
                self.loop_frames.pop();
                inner.extend(body_b?);
                self.push(Stmt::Loop { body: inner });
            }
            ast::StmtKind::For {
                init,
                cond,
                step,
                body,
            } => {
                if let Some(init) = init.as_ref() {
                    self.stmt(init)?;
                }
                let cond_expr = match cond {
                    Some(c) => self.condition(c)?,
                    None => Expr::Bool(true),
                };
                let mut inner = vec![Stmt::If {
                    cond: Expr::Not(Box::new(cond_expr)),
                    then: vec![Stmt::Break],
                    els: vec![],
                }];
                let step_b = match step.as_ref() {
                    Some(step) => {
                        self.blocks.push(Vec::new());
                        self.stmt(step)?;
                        self.blocks.pop().unwrap()
                    }
                    None => Vec::new(),
                };
                self.loop_frames.push(self.defers.len());
                self.loop_steps.push(step_b.clone());
                let body_b = self.lower_body(body);
                self.loop_steps.pop();
                self.loop_frames.pop();
                inner.extend(body_b?);
                inner.extend(step_b);
                self.push(Stmt::Loop { body: inner });
            }
            ast::StmtKind::ForEach {
                var,
                value,
                coll,
                body,
            } => match value {
                Some(v) => self.lower_foreach_dict(var, v, coll, body)?,
                None => self.lower_foreach(var, coll, body)?,
            },
            ast::StmtKind::Switch { subject, sections } => {
                self.switch_stmt(subject, sections)?;
            }
            ast::StmtKind::Break => {
                let floor = self.loop_frames.last().copied().unwrap_or(0);
                self.emit_defers_to(floor);
                self.push(Stmt::Break)
            }
            ast::StmtKind::Continue => {
                let floor = self.loop_frames.last().copied().unwrap_or(0);
                self.emit_defers_to(floor);
                // Inside a `switch`, which is lowered as a loop that runs once,
                // `continue` means the enclosing loop: leave the switch with a
                // flag set, and the code after it continues for real.
                if let Some(&(depth, flag)) = self.switch_frames.last() {
                    if self.loop_frames.len() == self.switch_frames.len() {
                        return Err("`continue` is only allowed inside a loop".into());
                    }
                    if depth == self.loop_frames.len() {
                        self.push(Stmt::Assign {
                            place: Place::Local(flag),
                            value: Expr::Bool(true),
                        });
                        self.push(Stmt::Break);
                        return Ok(());
                    }
                }
                // The step runs on `continue` too — otherwise a counted loop
                // that continues never advances.
                if let Some(step) = self.loop_steps.last().cloned() {
                    for st in step {
                        self.push(st);
                    }
                }
                self.push(Stmt::Continue)
            }
            ast::StmtKind::Block(b) => {
                let body = self.lower_body(b)?;
                for s in body {
                    self.push(s);
                }
            }
            ast::StmtKind::Defer(inner) => {
                self.blocks.push(Vec::new());
                self.stmt(inner)?;
                let lowered = self.blocks.pop().unwrap();
                self.defers.last_mut().unwrap().push(lowered);
            }
        }
        Ok(())
    }

    /// Bind a loop variable for the body, in a fresh cell per iteration when a
    /// lambda closes over it.
    ///
    /// C# learned this the hard way: one cell shared by every iteration means
    /// all the closures see the last value, which is never what was meant. So
    /// the loop keeps its counter in a hidden local and each turn copies it
    /// into a cell of its own — the statements that do it are returned, to be
    /// run at the top of the body, after the bounds check.
    fn bind_loop_var(&mut self, var: &str, ty: TyId, from: LocalId) -> Result<Vec<Stmt>, String> {
        if !self.captured.contains(var) {
            self.scope.insert(var.to_string(), (from, ty));
            return Ok(Vec::new());
        }
        self.blocks.push(Vec::new());
        let r = self.make_cell(var, ty, Expr::Local(from));
        let prologue = self.blocks.pop().unwrap();
        r?;
        Ok(prologue)
    }

    /// Drop a loop variable's binding once its body has been lowered.
    fn unbind_loop_var(&mut self, var: &str) {
        self.scope.remove(var);
        self.cells.remove(var);
    }

    /// `foreach (var (key, value) in dict)` — the entries in the order their
    /// keys were added, which is the order the dictionary keeps.
    fn lower_foreach_dict(
        &mut self,
        kvar: &str,
        vvar: &str,
        coll: &ast::Expr,
        body: &[ast::Stmt],
    ) -> Result<(), String> {
        let (dv, dty) = self.expr(coll, None)?;
        let Some((_, kty, vty)) = self.cx.as_dict(dty) else {
            return Err("`foreach (var (key, value) in …)` walks a Dictionary".into());
        };
        let holder = self.new_local("$each", dty);
        self.push(Stmt::Let {
            local: holder,
            value: dv,
        });
        let idx = self.new_local("$i", TyTable::I32);
        self.push(Stmt::Let {
            local: idx,
            value: Expr::Int(0, TyTable::I32),
        });
        let k = self.new_local(kvar, kty);
        let v = self.new_local(vvar, vty);
        let bind_k = self.bind_loop_var(kvar, kty, k)?;
        let bind_v = self.bind_loop_var(vvar, vty, v)?;
        let field = |f: usize| Expr::Field(Box::new(Expr::Local(holder)), f);
        let mut inner = vec![
            Stmt::If {
                cond: Expr::Not(Box::new(Expr::Bin(
                    BinOp::Lt,
                    Box::new(Expr::Local(idx)),
                    Box::new(field(DICT_LEN)),
                    TyTable::I32,
                ))),
                then: vec![Stmt::Break],
                els: vec![],
            },
            Stmt::Assign {
                place: Place::Local(k),
                value: Expr::Index(Box::new(field(DICT_KEYS)), Box::new(Expr::Local(idx))),
            },
            Stmt::Assign {
                place: Place::Local(v),
                value: Expr::Index(Box::new(field(DICT_VALUES)), Box::new(Expr::Local(idx))),
            },
        ];
        let step = vec![Stmt::Assign {
            place: Place::Local(idx),
            value: Expr::Bin(
                BinOp::Add,
                Box::new(Expr::Local(idx)),
                Box::new(Expr::Int(1, TyTable::I32)),
                TyTable::I32,
            ),
        }];
        self.loop_frames.push(self.defers.len());
        self.loop_steps.push(step.clone());
        let body_b = self.lower_body(body);
        self.loop_steps.pop();
        self.loop_frames.pop();
        self.unbind_loop_var(kvar);
        self.unbind_loop_var(vvar);
        inner.extend(bind_k);
        inner.extend(bind_v);
        inner.extend(body_b?);
        inner.extend(step);
        self.push(Stmt::Loop { body: inner });
        Ok(())
    }

    /// `foreach (i in a..b)` — the only collection form supported yet.
    fn lower_foreach(
        &mut self,
        var: &str,
        coll: &ast::Expr,
        body: &[ast::Stmt],
    ) -> Result<(), String> {
        // `foreach (x in list)` walks positions 1..Count.
        if !matches!(coll.kind, ast::ExprKind::Range(..)) {
            let (lv, lty) = self.expr(coll, None)?;
            if let Some((_, elem)) = self.cx.as_list(lty).or_else(|| self.cx.as_set(lty)) {
                let holder = self.new_local("$each", lty);
                self.push(Stmt::Let {
                    local: holder,
                    value: lv,
                });
                let idx = self.new_local("$i", TyTable::I32);
                self.push(Stmt::Let {
                    local: idx,
                    value: Expr::Int(0, TyTable::I32),
                });
                let item = self.new_local(var, elem);
                let bind = self.bind_loop_var(var, elem, item)?;
                let len = Expr::Field(Box::new(Expr::Local(holder)), LIST_LEN);
                let mut inner = vec![
                    Stmt::If {
                        cond: Expr::Not(Box::new(Expr::Bin(
                            BinOp::Lt,
                            Box::new(Expr::Local(idx)),
                            Box::new(len),
                            TyTable::I32,
                        ))),
                        then: vec![Stmt::Break],
                        els: vec![],
                    },
                    Stmt::Assign {
                        place: Place::Local(item),
                        value: Expr::Index(
                            Box::new(Expr::Field(Box::new(Expr::Local(holder)), LIST_DATA)),
                            Box::new(Expr::Local(idx)),
                        ),
                    },
                ];
                let step = vec![Stmt::Assign {
                    place: Place::Local(idx),
                    value: Expr::Bin(
                        BinOp::Add,
                        Box::new(Expr::Local(idx)),
                        Box::new(Expr::Int(1, TyTable::I32)),
                        TyTable::I32,
                    ),
                }];
                self.loop_frames.push(self.defers.len());
                self.loop_steps.push(step.clone());
                let body_b = self.lower_body(body);
                self.loop_steps.pop();
                self.loop_frames.pop();
                self.unbind_loop_var(var);
                // The cell is filled after the item is read and before the
                // body runs, so each turn closes over its own copy.
                inner.extend(bind);
                inner.extend(body_b?);
                inner.extend(step);
                self.push(Stmt::Loop { body: inner });
                return Ok(());
            }
            return Err("foreach supports integer ranges and List<T>".into());
        }
        let ast::ExprKind::Range(lo, hi, inclusive) = &coll.kind else {
            return Err("foreach supports only integer ranges `a..b` for now".into());
        };
        let (lo_v, ity) = self.expr(lo, Some(TyTable::I32))?;
        let (hi_v, _) = self.expr(hi, Some(ity))?;
        let iv = self.new_local(var, ity);
        let bind = self.bind_loop_var(var, ity, iv)?;
        let hi_l = self.new_local("$end", ity);
        self.push(Stmt::Let {
            local: iv,
            value: lo_v,
        });
        self.push(Stmt::Let {
            local: hi_l,
            value: hi_v,
        });
        let cmp = if *inclusive { BinOp::Le } else { BinOp::Lt };
        let mut inner = vec![Stmt::If {
            cond: Expr::Not(Box::new(Expr::Bin(
                cmp,
                Box::new(Expr::Local(iv)),
                Box::new(Expr::Local(hi_l)),
                ity,
            ))),
            then: vec![Stmt::Break],
            els: vec![],
        }];
        let step = vec![Stmt::Assign {
            place: Place::Local(iv),
            value: Expr::Bin(
                BinOp::Add,
                Box::new(Expr::Local(iv)),
                Box::new(Expr::Int(1, ity)),
                ity,
            ),
        }];
        self.loop_frames.push(self.defers.len());
        self.loop_steps.push(step.clone());
        let body_b = self.lower_body(body);
        self.loop_steps.pop();
        self.loop_frames.pop();
        self.unbind_loop_var(var);
        inner.extend(bind);
        inner.extend(body_b?);
        inner.extend(step);
        self.push(Stmt::Loop { body: inner });
        Ok(())
    }

    // ─── Console.WriteLine / Write ─────────────────────────────────────────

    fn try_console(&mut self, e: &ast::Expr) -> Result<Option<()>, String> {
        let ast::ExprKind::Call(callee, args) = &e.kind else {
            return Ok(None);
        };
        let ast::ExprKind::Member(recv, method) = &callee.kind else {
            return Ok(None);
        };
        let ast::ExprKind::Ident(obj) = &recv.kind else {
            return Ok(None);
        };
        if obj != "Console" || (method != "WriteLine" && method != "Write") {
            return Ok(None);
        }
        let newline = method == "WriteLine";

        // Gather the line as literal chunks and values, whichever form the
        // argument took.
        let mut parts: Vec<(String, Option<(Expr, TyId)>)> = Vec::new();
        if let Some(arg) = args.first() {
            match &arg.kind {
                ast::ExprKind::Interp(segs) => {
                    for seg in segs {
                        match seg {
                            ast::InterpSeg::Lit(l) => parts.push((l.clone(), None)),
                            ast::InterpSeg::Expr(x) => {
                                let (v, t) = self.expr(x, None)?;
                                parts.push((String::new(), Some((v, t))));
                            }
                        }
                    }
                }
                _ => {
                    let (v, t) = self.expr(arg, None)?;
                    parts.push((String::new(), Some((v, t))));
                }
            }
        }

        if self.cx.runtime == Runtime::Kiln {
            // `print_text` writes a whole line, so the line is built first.
            // `Console.Write` has no line to write: the runtime's own printing
            // goes through libc's `printf` on the same stream, so writing the
            // built string with `%s` and no newline interleaves correctly with
            // it — the two cannot get out of order.
            if !newline {
                let s = match parts.len() {
                    0 => return Ok(Some(())),
                    1 if parts[0].1.is_none() => Expr::Str(parts[0].0.clone()),
                    _ => self.build_string(parts).0,
                };
                self.emit_printf("%s", vec![(s, TyTable::STR)]);
                return Ok(Some(()));
            }
            if parts.is_empty() {
                self.emit_command_print(Expr::Str(String::new()));
                return Ok(Some(()));
            }
            let only_literal = parts.len() == 1 && parts[0].1.is_none();
            let s = if only_literal {
                Expr::Str(parts[0].0.clone())
            } else {
                self.build_string(parts).0
            };
            self.emit_command_print(s);
            return Ok(Some(()));
        }

        // libc: one printf per chunk, then the newline.
        for (lit, val) in parts {
            if !lit.is_empty() {
                self.emit_print_str(&lit);
            }
            if let Some((v, t)) = val {
                if t == TyTable::STR {
                    self.emit_printf("%s", vec![(v, TyTable::STR)]);
                } else {
                    self.emit_print_value(v, t)?;
                }
            }
        }
        if newline {
            self.emit_print_str("\n");
        }
        Ok(Some(()))
    }

    /// The printf/snprintf conversion for a value, with any promotion applied.
    fn fmt_arg(&mut self, v: Expr, ty: TyId) -> (&'static str, Expr, TyId) {
        match self.tt().kind(ty) {
            // A string that is not there interpolates as nothing, as 1.x's does.
            // libc prints a null `%s` as "(null)", which is a C implementation
            // detail leaking into a program's output.
            TyKind::Str if !matches!(v, Expr::Str(_)) => {
                let t = self.new_local("$text", TyTable::STR);
                self.push(Stmt::Let { local: t, value: v });
                self.push(Stmt::If {
                    cond: Expr::Bin(
                        BinOp::Eq,
                        Box::new(Expr::Local(t)),
                        Box::new(Expr::Null(TyTable::STR)),
                        TyTable::STR,
                    ),
                    then: vec![Stmt::Assign {
                        place: Place::Local(t),
                        value: Expr::Str(String::new()),
                    }],
                    els: vec![],
                });
                ("%s", Expr::Local(t), TyTable::STR)
            }
            TyKind::Str => ("%s", v, TyTable::STR),
            // A `T?` interpolates as its value, or as nothing when it has none —
            // as C# prints a null. It fell through to the integer case and
            // panicked the emitter on anything that was not an int.
            TyKind::Optional(inner) => {
                let inner = *inner;
                let held = self.new_local("$opt", ty);
                self.push(Stmt::Let { local: held, value: v });
                let t = self.new_local("$opttext", TyTable::STR);
                self.push(Stmt::Let {
                    local: t,
                    value: Expr::Str(String::new()),
                });
                self.blocks.push(Vec::new());
                let (text, _) = self.build_string(vec![(
                    String::new(),
                    Some((Expr::OptionalGet(Box::new(Expr::Local(held))), inner)),
                )]);
                let mut then = self.blocks.pop().unwrap();
                then.push(Stmt::Assign {
                    place: Place::Local(t),
                    value: text,
                });
                self.push(Stmt::If {
                    cond: Expr::OptionalHasValue(Box::new(Expr::Local(held))),
                    then,
                    els: vec![],
                });
                ("%s", Expr::Local(t), TyTable::STR)
            }
            TyKind::F32 => (
                "%g",
                Expr::Cast {
                    value: Box::new(v),
                    to: TyTable::F64,
                },
                TyTable::F64,
            ),
            TyKind::F64 => ("%g", v, TyTable::F64),
            TyKind::I64 | TyKind::U64 | TyKind::Nint | TyKind::Nuint => ("%lld", v, TyTable::I64),
            // `true` and `false`, as 1.x prints them and as they are written.
            // Printing `1` made every migrated program that shows a bool print
            // something different from the program it came from.
            TyKind::Bool => {
                let t = self.new_local("$booltext", TyTable::STR);
                self.push(Stmt::Let {
                    local: t,
                    value: Expr::Str("false".into()),
                });
                self.push(Stmt::If {
                    cond: v,
                    then: vec![Stmt::Assign {
                        place: Place::Local(t),
                        value: Expr::Str("true".into()),
                    }],
                    els: vec![],
                });
                ("%s", Expr::Local(t), TyTable::STR)
            }
            TyKind::U8 | TyKind::U16 | TyKind::U32 => {
                ("%u", cast_to(v, ty, TyTable::U32), TyTable::U32)
            }
            _ => ("%d", cast_to(v, ty, TyTable::I32), TyTable::I32),
        }
    }

    /// Build a `string` from literal chunks and values, with libc: `snprintf`
    /// measures it, `malloc` allocates, `snprintf` fills. This is what makes
    /// `$"..."` usable as a value and `string + string` work; it is replaced by
    /// the Kiln text runtime when the standard library lands.
    fn build_string(&mut self, parts: Vec<(String, Option<(Expr, TyId)>)>) -> (Expr, TyId) {
        let mut fmt = String::new();
        let mut args: Vec<(Expr, TyId)> = Vec::new();
        for (lit, val) in parts {
            for ch in lit.chars() {
                if ch == '%' {
                    fmt.push_str("%%");
                } else {
                    fmt.push(ch);
                }
            }
            if let Some((v, ty)) = val {
                let (spec, arg, promoted) = self.fmt_arg(v, ty);
                fmt.push_str(spec);
                args.push((arg, promoted));
            }
        }
        // Values are evaluated once, into locals, then used by both snprintf calls.
        let mut held = Vec::new();
        for (a, ty) in args {
            let l = self.new_local("$fmtarg", ty);
            self.push(Stmt::Let { local: l, value: a });
            held.push(Expr::Local(l));
        }
        let snprintf = |args: Vec<Expr>| {
            Expr::Call(Box::new(Call::Dll {
                library: "c".into(),
                symbol: "snprintf".into(),
                conv: CallConv::Cdecl,
                args,
                arg_tys: vec![TyTable::STR, TyTable::NUINT, TyTable::STR],
                ret: TyTable::I32,
                varargs: true,
            }))
        };
        // n = snprintf(null, 0, fmt, ...)
        let mut measure = vec![
            Expr::Null(TyTable::STR),
            Expr::Int(0, TyTable::NUINT),
            Expr::Str(fmt.clone()),
        ];
        measure.extend(held.iter().cloned());
        let n = self.new_local("$len", TyTable::I32);
        self.push(Stmt::Let {
            local: n,
            value: snprintf(measure),
        });
        // buf = malloc(n + 1)
        let size = Expr::Cast {
            value: Box::new(Expr::Bin(
                BinOp::Add,
                Box::new(Expr::Local(n)),
                Box::new(Expr::Int(1, TyTable::I32)),
                TyTable::I32,
            )),
            to: TyTable::NUINT,
        };
        let buf = self.new_local("$buf", TyTable::STR);
        let alloc = self.alloc(size, TyTable::STR);
        self.push(Stmt::Let {
            local: buf,
            value: alloc,
        });
        // snprintf(buf, n + 1, fmt, ...)
        let mut fill = vec![
            Expr::Local(buf),
            Expr::Cast {
                value: Box::new(Expr::Bin(
                    BinOp::Add,
                    Box::new(Expr::Local(n)),
                    Box::new(Expr::Int(1, TyTable::I32)),
                    TyTable::I32,
                )),
                to: TyTable::NUINT,
            },
            Expr::Str(fmt),
        ];
        fill.extend(held);
        self.push(Stmt::Expr(snprintf(fill)));
        (Expr::Local(buf), TyTable::STR)
    }

    fn emit_print_str(&mut self, s: &str) {
        self.emit_printf("%s", vec![(Expr::Str(s.to_string()), TyTable::STR)]);
    }

    fn emit_print_value(&mut self, v: Expr, ty: TyId) -> Result<(), String> {
        let (fmt, arg, arg_ty) = self.fmt_arg(v, ty);
        self.emit_printf(fmt, vec![(arg, arg_ty)]);
        Ok(())
    }

    /// Print a string through the Kiln runtime's `print_text` command.
    fn emit_command_print(&mut self, s: Expr) {
        self.push(Stmt::Expr(Expr::Call(Box::new(Call::Command {
            symbol: "kn_print_text".into(),
            args: vec![s],
            arg_slots: vec![SlotTy {
                tag: 9, // KN_SDT_TEXT
                ty: TyTable::STR,
            }],
            ret: TyTable::VOID,
        }))));
    }

    fn emit_printf(&mut self, fmt: &str, extra: Vec<(Expr, TyId)>) {
        let mut args = vec![Expr::Str(fmt.to_string())];
        for (e, _) in extra {
            args.push(e);
        }
        self.push(Stmt::Expr(Expr::Call(Box::new(Call::Dll {
            library: "c".into(),
            symbol: "printf".into(),
            conv: CallConv::Cdecl,
            args,
            arg_tys: vec![TyTable::STR],
            ret: TyTable::I32,
            varargs: true,
        }))));
    }

    // ─── expressions ───────────────────────────────────────────────────────

    /// Lower an expression, wrapping a plain `T` when a `T?` is expected.
    fn expr(&mut self, e: &ast::Expr, hint: Option<TyId>) -> Result<(Expr, TyId), String> {
        // A number literal takes its width from the value type a `T?` holds, so
        // `long? n = 7;` is a `long` wrapped rather than an `int` left unwrapped.
        let raw_hint = match (&e.kind, hint.map(|h| self.tt().kind(h).clone())) {
            (ast::ExprKind::Int(_) | ast::ExprKind::Float(..), Some(TyKind::Optional(inner))) => {
                Some(inner)
            }
            _ => hint,
        };
        let (v, ty) = self.expr_raw(e, raw_hint)?;
        if let Some(want) = hint {
            if ty != want {
                if let TyKind::Optional(inner) = *self.tt().kind(want) {
                    if ty == inner {
                        return Ok((Expr::MakeOptional(inner, Some(Box::new(v))), want));
                    }
                }
                // A library command reports failure through the error slot. Where
                // a `Result<T>` is expected, the call is wrapped: the value is
                // held, the slot is read, and the two become Ok or Err. This is
                // the spec's §9 lowering — only a Result that is stored, passed
                // or returned is materialised.
                if let Some((rid, vt)) = self.cx.as_result(want) {
                    if vt == ty
                        && matches!(&v, Expr::Call(c) if matches!(**c, Call::Command { .. }))
                    {
                        return self.wrap_error_slot(v, ty, rid, vt);
                    }
                }
                // A concrete type becomes an interface value: the object plus
                // one function pointer per interface method.
                if let Some(iface) = self.cx.as_interface(want) {
                    if let Some(concrete) = self.cx.record_name(ty) {
                        return self.to_interface(v, &concrete, &iface, want);
                    }
                }
            }
        }
        Ok((v, ty))
    }

    fn expr_raw(&mut self, e: &ast::Expr, hint: Option<TyId>) -> Result<(Expr, TyId), String> {
        match &e.kind {
            // `[a, b, c]` — an array, which is the shape a command taking a
            // list of values expects. The element type comes from the hint, or
            // from the first element when there is none.
            // `[a, b, c]` — target-typed, as the spec says. Where a command
            // wants an array it is one; everywhere else it is a `List<T>`,
            // which is the only list a program sees.
            ast::ExprKind::Collection(items) => {
                if let Some(TyKind::Array(elem)) = hint.map(|h| self.tt().kind(h).clone()) {
                    let mut vals = Vec::new();
                    for it in items {
                        let (v, _) = self.expr(it, Some(elem))?;
                        vals.push(v);
                    }
                    let ty = self.cx.b.m.types.intern(TyKind::Array(elem));
                    return Ok((Expr::MakeArray(elem, vals), ty));
                }
                let elem = match hint.and_then(|h| self.cx.as_list(h)) {
                    Some((_, e)) => e,
                    None => match items.first() {
                        Some(first) => {
                            // Lowered only for its type; the value is lowered
                            // again below, against that type.
                            let probe = first.clone();
                            self.blocks.push(Vec::new());
                            let t = self.expr(&probe, None).map(|(_, t)| t);
                            self.blocks.pop();
                            t?
                        }
                        None => {
                            return Err(
                                "an empty `[]` needs a type to be — write `new List<T>()`, \
                                 or give the variable one"
                                    .into(),
                            )
                        }
                    },
                };
                let rid = self.cx.list_record(elem);
                let lty = self.cx.b.m.types.intern(TyKind::Record(rid));
                let data_ty = self.cx.b.m.record(rid).fields[LIST_DATA].ty;
                let holder = self.new_local("$lit", lty);
                self.push(Stmt::Let {
                    local: holder,
                    value: Expr::MakeRecord(
                        rid,
                        vec![
                            Expr::Int(0, TyTable::I32),
                            Expr::Int(0, TyTable::I32),
                            Expr::Null(data_ty),
                        ],
                    ),
                });
                for it in items {
                    let (v, _) = self.expr(it, Some(elem))?;
                    self.list_add(holder, lty, v);
                }
                Ok((Expr::Local(holder), lty))
            }
            ast::ExprKind::Int(v) => {
                let ty = hint
                    .filter(|t| is_int_ty(self.tt(), *t))
                    .unwrap_or(TyTable::I32);
                Ok((Expr::Int(*v, ty), ty))
            }
            ast::ExprKind::Float(v, f32) => {
                let ty = if *f32 {
                    TyTable::F32
                } else {
                    hint.filter(|t| self.tt().is_float(*t))
                        .unwrap_or(TyTable::F64)
                };
                Ok((Expr::Float(*v, ty), ty))
            }
            ast::ExprKind::Bool(b) => Ok((Expr::Bool(*b), TyTable::BOOL)),
            ast::ExprKind::Str(s) => Ok((Expr::Str(s.clone()), TyTable::STR)),
            ast::ExprKind::Char(c) => Ok((Expr::Int(*c as i128, TyTable::CHAR), TyTable::CHAR)),
            ast::ExprKind::Null => {
                let ty = hint.unwrap_or(TyTable::PTR);
                if let TyKind::Optional(inner) = *self.tt().kind(ty) {
                    return Ok((Expr::MakeOptional(inner, None), ty));
                }
                Ok((Expr::Null(ty), ty))
            }
            // A method named where a pointer is wanted is its address — a
            // callback handed to C. Only a static method: an instance method
            // has a `this` no C caller would supply.
            ast::ExprKind::Ident(name)
                if (hint == Some(TyTable::PTR)
                    || hint.is_some_and(|h| matches!(self.tt().kind(h), TyKind::CFunc { .. })))
                    && !self.scope.contains_key(name.as_str())
                    && !self.cells.contains_key(name.as_str()) =>
            {
                let want = hint.unwrap();
                let found = self
                    .cx
                    .methods
                    .iter()
                    .find(|(k, sig)| {
                        !sig.this && (k.as_str() == name || k.ends_with(&format!(".{name}")))
                    })
                    .map(|(_, sig)| (sig.fid, sig.params.clone(), sig.ret));
                match found {
                    Some((fid, params, ret)) => {
                        // Typed as a function address, the method's signature
                        // must be the one the address promises.
                        if let TyKind::CFunc { params: wp, ret: wr, .. } = self.tt().kind(want).clone() {
                            if wp != params || wr != ret {
                                return Err(format!(
                                    "`{name}` does not have the signature this function pointer names"
                                ));
                            }
                            return Ok((Expr::FuncPtr(fid), want));
                        }
                        Ok((Expr::FuncPtr(fid), TyTable::PTR))
                    }
                    None => self.ident(name, e.span),
                }
            }
            ast::ExprKind::Ident(name) => self.ident(name, e.span),
            ast::ExprKind::Member(recv, member) => self.member(recv, member),
            ast::ExprKind::Call(callee, args) => {
                // `Error("...")` builds a failed Result of the expected type.
                if let ast::ExprKind::Ident(n) = &callee.kind {
                    if n == "Error" && !self.cx.methods.contains_key("Error") {
                        let want = hint
                            .or(Some(self.ret))
                            .and_then(|t| self.cx.as_result(t))
                            .ok_or_else(|| {
                                "`Error(...)` needs a `Result` target — use it in a `return` \
                                 from a Result-returning method, or assign it to a Result"
                                    .to_string()
                            })?;
                        let (rid, vt) = want;
                        let msg = match args.first() {
                            Some(a) => self.expr(a, Some(TyTable::STR))?.0,
                            None => Expr::Str(String::new()),
                        };
                        let ty = self.cx.b.m.types.intern(TyKind::Record(rid));
                        return Ok((
                            Expr::MakeRecord(
                                rid,
                                vec![Expr::Bool(false), zero_of(self.tt(), vt), msg],
                            ),
                            ty,
                        ));
                    }
                }
                self.call(callee, args)
            }
            ast::ExprKind::Index(base, idx) => {
                let (b, bty) = self.expr(base, None)?;
                if bty == TyTable::BYTES {
                    let (i, _) = self.expr(idx, Some(TyTable::I32))?;
                    let u8arr = self.cx.b.m.types.intern(TyKind::Array(TyTable::U8));
                    return Ok((
                        Expr::Index(
                            Box::new(Expr::Cast {
                                value: Box::new(b),
                                to: u8arr,
                            }),
                            Box::new(Expr::Bin(
                                BinOp::Add,
                                Box::new(i),
                                Box::new(Expr::Int(BIN_HEADER as i128, TyTable::I32)),
                                TyTable::I32,
                            )),
                        ),
                        TyTable::U8,
                    ));
                }
                if self.cx.as_dict(bty).is_some() {
                    return Err(
                        "read a Dictionary with `.Get(k)`, which yields a `V?` — there is no \
                         exception to throw for a missing key"
                            .into(),
                    );
                }
                // `blob.Bytes[i]` — an inline array, positions from 1.
                if let TyKind::Inline { elem, count } = *self.tt().kind(bty) {
                    let (i, _) = self.expr(idx, Some(TyTable::I32))?;
                    let (b, i0) = self.checked_inline_index(b, count, i);
                    return Ok((Expr::Index(Box::new(b), Box::new(i0)), elem));
                }
                // `list[i]` reads the buffer; positions are 1-based.
                if let Some((_, elem)) = self.cx.as_list(bty) {
                    let (i, _) = self.expr(idx, Some(TyTable::I32))?;
                    let (data, i0) = self.checked_list_index(b, bty, i);
                    return Ok((Expr::Index(Box::new(data), Box::new(i0)), elem));
                }
                let elem = match self.tt().kind(bty) {
                    TyKind::Array(e) => *e,
                    _ => return Err("indexing is only supported on List<T> for now".into()),
                };
                let (i, _) = self.expr(idx, Some(TyTable::I32))?;
                // 1-based → the runtime helper expects 0-based; subtract 1.
                let i0 = Expr::Bin(
                    BinOp::Sub,
                    Box::new(i),
                    Box::new(Expr::Int(1, TyTable::I32)),
                    TyTable::I32,
                );
                Ok((Expr::Index(Box::new(b), Box::new(i0)), elem))
            }
            ast::ExprKind::Unary(op, inner) => {
                let uhint = hint.map(|h| match *self.tt().kind(h) {
                    TyKind::Optional(i) => i,
                    _ => h,
                });
                let (v, ty) = self.expr(inner, uhint)?;
                let out = match op {
                    ast::UnOp::Neg => Expr::Neg(Box::new(v), ty),
                    ast::UnOp::Not => Expr::Not(Box::new(v)),
                    ast::UnOp::BitNot => {
                        // ~x == x ^ -1
                        Expr::Bin(BinOp::Xor, Box::new(v), Box::new(Expr::Int(-1, ty)), ty)
                    }
                };
                let rty = if *op == ast::UnOp::Not {
                    TyTable::BOOL
                } else {
                    ty
                };
                Ok((out, rty))
            }
            ast::ExprKind::Binary(op, a, b) => {
                // A null comparison on an optional is a presence test.
                if matches!(op, ast::BinOp::Eq | ast::BinOp::Ne) {
                    let (opt, _other) = match (&a.kind, &b.kind) {
                        (ast::ExprKind::Null, _) => (Some(b), true),
                        (_, ast::ExprKind::Null) => (Some(a), true),
                        _ => (None, false),
                    };
                    if let Some(x) = opt {
                        let (v, vty) = self.expr_raw(x, None)?;
                        if let TyKind::Optional(_) = *self.tt().kind(vty) {
                            let has = Expr::OptionalHasValue(Box::new(v));
                            let out = if *op == ast::BinOp::Ne {
                                has
                            } else {
                                Expr::Not(Box::new(has))
                            };
                            return Ok((out, TyTable::BOOL));
                        }
                    }
                }
                self.binary(*op, a, b, hint)
            }
            ast::ExprKind::Cast(t, inner) => {
                let to = self.cx.resolve(t)?;
                let (v, _) = self.expr(inner, None)?;
                Ok((
                    Expr::Cast {
                        value: Box::new(v),
                        to,
                    },
                    to,
                ))
            }
            ast::ExprKind::New(t, args, inits) => self.new_record(t, args, inits),
            ast::ExprKind::DictInit(t, entries) => {
                let (d, dty) = self.new_record(t, &[], &[])?;
                if self.cx.as_dict(dty).is_none() {
                    return Err("`{ [key] = value }` initialises a Dictionary".into());
                }
                let holder = self.new_local("$dictinit", dty);
                self.push(Stmt::Let {
                    local: holder,
                    value: d,
                });
                for (k, v) in entries {
                    self.dict_set(holder, dty, k, v)?;
                }
                Ok((Expr::Local(holder), dty))
            }
            ast::ExprKind::Ternary(c, a, b) => self.ternary(c, a, b, hint),
            ast::ExprKind::NullCoalesce(a, b) => {
                let (v, vty) = self.expr_raw(a, None)?;
                // `command(...) ?? fallback` — the call reports failure through
                // the error slot, so it becomes a Result first. This is 1.x's
                // `otherwise`, spelled the way C# spells a fallback.
                let (v, vty) = if self.cx.as_result(vty).is_none()
                    && !matches!(self.tt().kind(vty), TyKind::Optional(_))
                    && matches!(&v, Expr::Call(c) if matches!(**c, Call::Command { .. }))
                {
                    let rid = self.cx.result_record(vty);
                    self.wrap_error_slot(v, vty, rid, vty)?
                } else {
                    (v, vty)
                };
                // `T? ?? fallback`
                if let TyKind::Optional(inner) = *self.tt().kind(vty) {
                    let held = self.new_local("$coalesce", vty);
                    self.push(Stmt::Let {
                        local: held,
                        value: v,
                    });
                    let out = self.new_local("$value", inner);
                    let (fb, fbty) = self.expr(b, Some(inner))?;
                    self.check_fits(fbty, inner, "the fallback after `??`")?;
                    self.push(Stmt::If {
                        cond: Expr::OptionalHasValue(Box::new(Expr::Local(held))),
                        then: vec![Stmt::Assign {
                            place: Place::Local(out),
                            value: Expr::OptionalGet(Box::new(Expr::Local(held))),
                        }],
                        els: vec![Stmt::Assign {
                            place: Place::Local(out),
                            value: fb,
                        }],
                    });
                    return Ok((Expr::Local(out), inner));
                }
                let (_, val_ty) = self
                    .cx
                    .as_result(vty)
                    .ok_or_else(|| "`??` applies to a `Result` or a `T?`".to_string())?;
                let held = self.new_local("$coalesce", vty);
                self.push(Stmt::Let {
                    local: held,
                    value: v,
                });
                let out = self.new_local("$value", val_ty);
                let (fb, fbty) = self.expr(b, Some(val_ty))?;
                self.check_fits(fbty, val_ty, "the fallback after `??`")?;
                self.push(Stmt::If {
                    cond: Expr::Field(Box::new(Expr::Local(held)), RESULT_OK),
                    then: vec![Stmt::Assign {
                        place: Place::Local(out),
                        value: Expr::Field(Box::new(Expr::Local(held)), RESULT_VALUE),
                    }],
                    els: vec![Stmt::Assign {
                        place: Place::Local(out),
                        value: fb,
                    }],
                });
                Ok((Expr::Local(out), val_ty))
            }
            ast::ExprKind::Try(inner) => {
                let (v, vty) = self.expr(inner, None)?;
                // `File.ReadText(p)?` — a command reports failure through the
                // error slot, so it becomes a Result before being propagated.
                let (v, vty) = if self.cx.as_result(vty).is_none()
                    && matches!(&v, Expr::Call(c) if matches!(**c, Call::Command { .. }))
                {
                    let rid = self.cx.result_record(vty);
                    self.wrap_error_slot(v, vty, rid, vty)?
                } else {
                    (v, vty)
                };
                let (_, val_ty) = self.cx.as_result(vty).ok_or_else(|| {
                    "`?` applies to a `Result`, or to a command call that can fail".to_string()
                })?;
                let (out_rid, out_vt) = self.cx.as_result(self.ret).ok_or_else(|| {
                    "`?` needs the enclosing method to return a `Result`".to_string()
                })?;
                // Hold the result once, then test it.
                let t = self.new_local("$try", vty);
                self.push(Stmt::Let { local: t, value: v });
                let failed = Expr::Not(Box::new(Expr::Field(Box::new(Expr::Local(t)), RESULT_OK)));
                let propagated = Expr::MakeRecord(
                    out_rid,
                    vec![
                        Expr::Bool(false),
                        zero_of(self.tt(), out_vt),
                        Expr::Field(Box::new(Expr::Local(t)), RESULT_ERR),
                    ],
                );
                self.push(Stmt::If {
                    cond: failed,
                    then: vec![Stmt::Return(Some(propagated))],
                    els: vec![],
                });
                Ok((Expr::Field(Box::new(Expr::Local(t)), RESULT_VALUE), val_ty))
            }
            ast::ExprKind::NullForgiving(inner) => self.null_forgiving(inner),
            ast::ExprKind::TypeArgs(..) => {
                Err("type arguments are written on a call to a generic method: `Make<int>()`".into())
            }
            ast::ExprKind::NullConditional(recv, steps) => self.null_conditional(recv, steps),
            ast::ExprKind::Range(_, _, _) => Err("a range is only valid in `foreach`".into()),
            ast::ExprKind::Interp(segs) => {
                let mut parts: Vec<(String, Option<(Expr, TyId)>)> = Vec::new();
                for seg in segs {
                    match seg {
                        ast::InterpSeg::Lit(l) => parts.push((l.clone(), None)),
                        ast::InterpSeg::Expr(x) => {
                            let (v, t) = self.expr(x, None)?;
                            parts.push((String::new(), Some((v, t))));
                        }
                    }
                }
                Ok(self.build_string(parts))
            }
            ast::ExprKind::Switch(subject, arms) => self.switch(subject, arms, hint),
            ast::ExprKind::Lambda(l) => {
                let want = hint.ok_or_else(|| {
                    "a lambda needs a target type — assign it to a `Func<...>`/`Action<...>` \
                     local or pass it to a parameter of that type"
                        .to_string()
                })?;
                self.lambda(l, want)
            }
        }
    }

    fn ident(&mut self, name: &str, _span: LSpan) -> Result<(Expr, TyId), String> {
        if let Some(c) = self.cells.get(name).cloned() {
            return Ok((Expr::Field(Box::new(c.ptr), 0), c.ty));
        }
        if let Some((lid, ty)) = self.scope.get(name).copied() {
            if let TyKind::Optional(inner) = *self.tt().kind(ty) {
                if self.narrowed.contains(name) {
                    return Ok((Expr::OptionalGet(Box::new(Expr::Local(lid))), inner));
                }
            }
            return Ok((Expr::Local(lid), ty));
        }
        if let Some((g, ty)) = self.cx.form_state.get(name).copied() {
            return Ok((Expr::Global(g), ty));
        }
        if let Some((ty, value)) = self.cx.consts.get(name).cloned() {
            return self.expr(&value, Some(ty));
        }
        // An unqualified name inside an instance method may be a field of `this`.
        if let Some((this_lid, this_ty)) = self.scope.get("this").copied() {
            if let TyKind::Record(rid) = *self.tt().kind(this_ty) {
                let rec = self.cx.b.m.record(rid);
                if let Some(idx) = rec.fields.iter().position(|f| f.name == name) {
                    let fty = rec.fields[idx].ty;
                    return Ok((Expr::Field(Box::new(Expr::Local(this_lid)), idx), fty));
                }
            }
        }
        Err(format!("unknown name `{name}`"))
    }

    fn member(&mut self, recv: &ast::Expr, member: &str) -> Result<(Expr, TyId), String> {
        // `T.Size` on a [Packed] record.
        if let ast::ExprKind::Ident(obj) = &recv.kind {
            // Every record is laid out as C lays out a struct, so every record
            // has a size known while compiling — not only a `[Packed]` one.
            if member == "Size" && self.cx.type_ids.contains_key(obj.as_str()) {
                let rty = self.cx.record_ty(obj);
                if let TyKind::Record(rid) = *self.tt().kind(rty) {
                    if let Layout::C { size, .. } = self.cx.b.m.record(rid).layout {
                        return Ok((Expr::Int(size as i128, TyTable::I32), TyTable::I32));
                    }
                }
            }
        }
        // Enum member: Type.Member
        if let ast::ExprKind::Ident(obj) = &recv.kind {
            if let Some(members) = self.cx.enums.get(obj) {
                if let Some(v) = members.get(member) {
                    return Ok((Expr::Int(*v, TyTable::I32), TyTable::I32));
                }
            }
            if let Some((ty, value)) = self.cx.consts.get(&format!("{obj}.{member}")).cloned() {
                return self.expr(&value, Some(ty));
            }
            // `count.Text` — reading a component's property off the live window.
            // Typed by the component's descriptor: `Left` is an int, `Enabled`
            // a bool, `Text` a string — so `count.Left + 10` is arithmetic, not
            // text pasted to a number.
            if !self.scope.contains_key(obj.as_str()) && !self.cells.contains_key(obj.as_str()) {
                if let Some((g, type_name)) = self.cx.components.get(obj).cloned() {
                    let prop = snake_case(member);
                    let pty = self.cx.registry.as_ref().and_then(|r| {
                        r.component(&snake_case(&type_name))
                            .and_then(|d| d.properties.iter().find(|p| p.name == prop))
                            .map(|p| p.ty)
                    });
                    if let Some(lib) = self.cx.nonvisual.get(obj).cloned() {
                        let text = component_call(
                            &lib,
                            "get",
                            vec![Expr::Global(g), Expr::Str(prop)],
                            vec![TyTable::I64, TyTable::STR],
                            TyTable::STR,
                        );
                        return Ok(match pty {
                            Some(kiln_ir::Ty::Int) | Some(kiln_ir::Ty::Int64) => (
                                Expr::Call(Box::new(Call::Dll {
                                    library: "c".into(),
                                    symbol: "atoi".into(),
                                    conv: CallConv::Cdecl,
                                    args: vec![text],
                                    arg_tys: vec![TyTable::STR],
                                    ret: TyTable::I32,
                                    varargs: false,
                                })),
                                TyTable::I32,
                            ),
                            Some(kiln_ir::Ty::Bool) => {
                                let t = self.new_local("$prop", TyTable::STR);
                                self.push(Stmt::Let { local: t, value: text });
                                (
                                    self.key_equal(
                                        Expr::Local(t),
                                        Expr::Str("true".into()),
                                        TyTable::STR,
                                    ),
                                    TyTable::BOOL,
                                )
                            }
                            _ => (text, TyTable::STR),
                        });
                    }
                    return Ok(match pty {
                        Some(kiln_ir::Ty::Int) | Some(kiln_ir::Ty::Int64) => (
                            ui_call(
                                "kn_ui_get_int",
                                vec![Expr::Global(g), Expr::Str(prop)],
                                vec![TyTable::I64, TyTable::STR],
                                TyTable::I32,
                            ),
                            TyTable::I32,
                        ),
                        Some(kiln_ir::Ty::Bool) => (
                            Expr::Bin(
                                BinOp::Ne,
                                Box::new(ui_call(
                                    "kn_ui_get_int",
                                    vec![Expr::Global(g), Expr::Str(prop)],
                                    vec![TyTable::I64, TyTable::STR],
                                    TyTable::I32,
                                )),
                                Box::new(Expr::Int(0, TyTable::I32)),
                                TyTable::I32,
                            ),
                            TyTable::BOOL,
                        ),
                        // Text, and anything the descriptor does not name: the
                        // library answers with text either way.
                        _ => (
                            ui_call(
                                "kn_ui_get",
                                vec![Expr::Global(g), Expr::Str(prop)],
                                vec![TyTable::I64, TyTable::STR],
                                TyTable::STR,
                            ),
                            TyTable::STR,
                        ),
                    });
                }
            }
            // `P.count` — a static field, when `P` is not a value in scope.
            if !self.scope.contains_key(obj.as_str()) && !self.cells.contains_key(obj.as_str()) {
                if let Some((g, ty)) = self.cx.form_state.get(&format!("{obj}.{member}")).copied() {
                    return Ok((Expr::Global(g), ty));
                }
            }
        }
        // Field access on a record value, or `.Length` etc. (later).
        let (base, bty) = self.expr(recv, None)?;
        // A set exposes its size.
        if self.cx.as_set(bty).is_some() {
            return match member {
                "Count" => Ok((Expr::Field(Box::new(base), LIST_LEN), TyTable::I32)),
                other => Err(format!(
                    "no member `{other}` on a HashSet — use .Add(x) or .Contains(x)"
                )),
            };
        }
        // A dictionary exposes its size.
        if let Some((_, kty, vty)) = self.cx.as_dict(bty) {
            // `.Keys` / `.Values` — a new list, in insertion order.
            if member == "Keys" || member == "Values" {
                let (field, elem) = if member == "Keys" { (DICT_KEYS, kty) } else { (DICT_VALUES, vty) };
                let holder = self.new_local("$dictview", bty);
                self.push(Stmt::Let { local: holder, value: base });
                let lrid = self.cx.list_record(elem);
                let lty = self.cx.b.m.types.intern(TyKind::Record(lrid));
                let data_ty = self.cx.b.m.record(lrid).fields[LIST_DATA].ty;
                let out = self.new_local("$view", lty);
                self.push(Stmt::Let {
                    local: out,
                    value: Expr::MakeRecord(
                        lrid,
                        vec![Expr::Int(0, TyTable::I32), Expr::Int(0, TyTable::I32), Expr::Null(data_ty)],
                    ),
                });
                let i = self.new_local("$vi", TyTable::I32);
                self.push(Stmt::Let { local: i, value: Expr::Int(0, TyTable::I32) });
                let item = Expr::Index(
                    Box::new(Expr::Field(Box::new(Expr::Local(holder)), field)),
                    Box::new(Expr::Local(i)),
                );
                self.blocks.push(Vec::new());
                self.list_add(out, lty, item);
                let add = self.blocks.pop().unwrap();
                let mut body = vec![Stmt::If {
                    cond: Expr::Not(Box::new(Expr::Bin(
                        BinOp::Lt,
                        Box::new(Expr::Local(i)),
                        Box::new(Expr::Field(Box::new(Expr::Local(holder)), DICT_LEN)),
                        TyTable::I32,
                    ))),
                    then: vec![Stmt::Break],
                    els: vec![],
                }];
                body.extend(add);
                body.push(Stmt::Assign {
                    place: Place::Local(i),
                    value: Expr::Bin(BinOp::Add, Box::new(Expr::Local(i)), Box::new(Expr::Int(1, TyTable::I32)), TyTable::I32),
                });
                self.push(Stmt::Loop { body });
                return Ok((Expr::Local(out), lty));
            }
            return match member {
                "Count" => Ok((Expr::Field(Box::new(base), DICT_LEN), TyTable::I32)),
                other => Err(format!(
                    "no member `{other}` on a Dictionary — use .ContainsKey(k) or .Get(k)"
                )),
            };
        }
        // A list exposes its length.
        if self.cx.as_list(bty).is_some() {
            return match member {
                "Count" => Ok((Expr::Field(Box::new(base), LIST_LEN), TyTable::I32)),
                other => Err(format!("no member `{other}` on a List")),
            };
        }
        // An optional exposes presence and value explicitly.
        if let TyKind::Optional(inner) = *self.tt().kind(bty) {
            // `found.Name` on a `Person?` reads the field of the person it
            // holds, and stops the program if it holds none.
            if let TyKind::Record(rid) = *self.tt().kind(inner) {
                let idx = self.cx.b.m.record(rid).fields.iter().position(|f| f.name == member);
                if let Some(idx) = idx {
                    let fty = self.cx.b.m.record(rid).fields[idx].ty;
                    let v = self.present_or_stop(base, bty, member)?;
                    return Ok((Expr::Field(Box::new(v), idx), fty));
                }
            }
            return match member {
                "HasValue" => Ok((Expr::OptionalHasValue(Box::new(base)), TyTable::BOOL)),
                "Value" => Ok((Expr::OptionalGet(Box::new(base)), inner)),
                other => Err(format!(
                    "no member `{other}` on a `T?` — test it against null, or use .Value"
                )),
            };
        }
        // A synthesised Result exposes named members rather than raw fields.
        if let Some((_, vt)) = self.cx.as_result(bty) {
            return match member {
                "IsOk" => Ok((Expr::Field(Box::new(base), RESULT_OK), TyTable::BOOL)),
                "IsErr" => Ok((
                    Expr::Not(Box::new(Expr::Field(Box::new(base), RESULT_OK))),
                    TyTable::BOOL,
                )),
                "Value" => Ok((Expr::Field(Box::new(base), RESULT_VALUE), vt)),
                "Error" => Ok((Expr::Field(Box::new(base), RESULT_ERR), TyTable::STR)),
                other => Err(format!("no member `{other}` on a Result")),
            };
        }
        if let TyKind::Record(rid) = *self.tt().kind(bty) {
            let rec = self.cx.b.m.record(rid);
            if let Some(idx) = rec.fields.iter().position(|f| f.name == member) {
                let fty = rec.fields[idx].ty;
                if self.cx.c_bools.contains(&(rid, idx)) {
                    return Ok((
                        Expr::Bin(
                            BinOp::Ne,
                            Box::new(Expr::Field(Box::new(base), idx)),
                            Box::new(Expr::Int(0, TyTable::I32)),
                            TyTable::I32,
                        ),
                        TyTable::BOOL,
                    ));
                }
                // A nested record reads as that record, in place.
                if let TyKind::Inline { elem, count: 0 } = *self.tt().kind(fty) {
                    return Ok((Expr::Field(Box::new(base), idx), elem));
                }
                return Ok((Expr::Field(Box::new(base), idx), fty));
            }
        }
        // `s.Length` — a command taking just the receiver.
        if let Some((sym, params, ret, tags)) = self.lookup_instance_command(bty, member, 0) {
            let slots = vec![SlotTy {
                tag: tags[0],
                ty: params[0],
            }];
            return Ok((
                Expr::Call(Box::new(Call::Command {
                    symbol: sym,
                    args: vec![base],
                    arg_slots: slots,
                    ret,
                })),
                ret,
            ));
        }
        Err(format!("no member `{member}` on this value"))
    }

    fn call(&mut self, callee: &ast::Expr, args: &[ast::Expr]) -> Result<(Expr, TyId), String> {
        // `Make<int>()`: the written type arguments are resolved here and the
        // call goes on as the plain one would, with them handed to
        // `instantiate` in place of inference.
        let (callee, explicit) = match &callee.kind {
            ast::ExprKind::TypeArgs(inner, trefs) => {
                let mut tys = Vec::new();
                for t in trefs {
                    tys.push(self.cx.resolve(t)?);
                }
                (inner.as_ref(), Some(tys))
            }
            _ => (callee, None),
        };
        // `T.OffsetOf("Field")` — where a field of a record starts, in bytes,
        // known while compiling as `T.Size` is.
        if let ast::ExprKind::Member(recv, m) = &callee.kind {
            if let ast::ExprKind::Ident(obj) = &recv.kind {
                if m == "OffsetOf" && self.cx.type_ids.contains_key(obj.as_str()) {
                    let [ast::Expr { kind: ast::ExprKind::Str(field), .. }] = args else {
                        return Err(format!("`{obj}.OffsetOf` takes a field name as a string: `{obj}.OffsetOf(\"X\")`"));
                    };
                    let rty = self.cx.record_ty(obj);
                    if let TyKind::Record(rid) = *self.tt().kind(rty) {
                        let rec = self.cx.b.m.record(rid);
                        let idx = rec
                            .fields
                            .iter()
                            .position(|f| f.name == *field)
                            .ok_or_else(|| format!("`{obj}` has no field `{field}`"))?;
                        if let Layout::C { offsets, .. } = &rec.layout {
                            return Ok((Expr::Int(offsets[idx] as i128, TyTable::I32), TyTable::I32));
                        }
                    }
                }
            }
        }
        // Calling a C function address — `add(5, 10)`, `table[3](2, 3)`,
        // `((delegate* unmanaged<Ptr, void>)p)(cell)`. The callee is lowered
        // as a value in a throwaway block first: only if it turns out to be a
        // function address is that lowering kept.
        let value_callee = match &callee.kind {
            ast::ExprKind::Ident(n) => self
                .scope
                .get(n.as_str())
                .is_some_and(|(_, t)| matches!(self.tt().kind(*t), TyKind::CFunc { .. })),
            ast::ExprKind::Member(..) | ast::ExprKind::Index(..) | ast::ExprKind::Cast(..) | ast::ExprKind::Call(..) => true,
            _ => false,
        };
        if value_callee {
            self.blocks.push(Vec::new());
            let probe = self.expr_raw(callee, None);
            let pushed = self.blocks.pop().unwrap();
            if let Ok((fv, fty)) = probe {
                if let TyKind::CFunc { params, ret, .. } = self.tt().kind(fty).clone() {
                    for st in pushed {
                        self.push(st);
                    }
                    if args.len() != params.len() {
                        return Err(format!(
                            "this function pointer takes {} argument(s), got {}",
                            params.len(),
                            args.len()
                        ));
                    }
                    let held = self.new_local("$fnptr", fty);
                    self.push(Stmt::Let { local: held, value: fv });
                    let mut kargs = Vec::new();
                    for (i, (a, pty)) in args.iter().zip(params.iter()).enumerate() {
                        let (v, vty) = self.expr(a, Some(*pty))?;
                        let (v, _) = self.settle(v, vty, *pty, &format!("argument {}", i + 1))?;
                        kargs.push(v);
                    }
                    return Ok((
                        Expr::Call(Box::new(Call::Indirect {
                            callee: Box::new(Expr::Local(held)),
                            args: kargs,
                            sig: fty,
                        })),
                        ret,
                    ));
                }
            }
        }
        // Calling a value of function type: an indirect call through its
        // `{fn, env}` pair.
        if let ast::ExprKind::Ident(name) = &callee.kind {
            if let Some((lid, ty)) = self.scope.get(name).copied() {
                if let TyKind::Func { params, ret } = self.tt().kind(ty).clone() {
                    if args.len() != params.len() {
                        return Err(format!(
                            "`{name}` takes {} argument(s), got {}",
                            params.len(),
                            args.len()
                        ));
                    }
                    let mut kargs = Vec::new();
                    for (a, pty) in args.iter().zip(params.iter()) {
                        kargs.push(self.expr(a, Some(*pty))?.0);
                    }
                    return Ok((
                        Expr::Call(Box::new(Call::Indirect {
                            callee: Box::new(Expr::Local(lid)),
                            args: kargs,
                            sig: ty,
                        })),
                        ret,
                    ));
                }
            }
        }
        // `set.Add(x)` / `set.Contains(x)` — a set is a list that refuses
        // duplicates, so Add looks before it appends.
        if let ast::ExprKind::Member(recv, name) = &callee.kind {
            if name == "Add" || name == "Contains" {
                let (sv, sty) = self.expr_raw(recv, None)?;
                if let Some((_, elem)) = self.cx.as_set(sty) {
                    if args.len() != 1 {
                        return Err(format!("HashSet.{name} takes one argument"));
                    }
                    let holder = self.new_local("$set", sty);
                    self.push(Stmt::Let {
                        local: holder,
                        value: sv,
                    });
                    let (v, _) = self.expr(&args[0], Some(elem))?;
                    let vh = self.new_local("$item", elem);
                    self.push(Stmt::Let {
                        local: vh,
                        value: v,
                    });
                    let found = self.set_find(holder, vh, elem)?;
                    let present = Expr::Bin(
                        BinOp::Ge,
                        Box::new(Expr::Local(found)),
                        Box::new(Expr::Int(0, TyTable::I32)),
                        TyTable::I32,
                    );
                    if name == "Contains" {
                        return Ok((present, TyTable::BOOL));
                    }
                    self.blocks.push(Vec::new());
                    self.set_append(holder, sty, vh);
                    let append = self.blocks.pop().unwrap();
                    self.push(Stmt::If {
                        cond: present,
                        then: vec![],
                        els: append,
                    });
                    return Ok((Expr::Int(0, TyTable::VOID), TyTable::VOID));
                }
            }
        }
        // `list.Add(x)` — grow the buffer if it is full, then store.
        if let ast::ExprKind::Member(recv, name) = &callee.kind {
            if name == "Add" {
                let (lv, lty) = self.expr_raw(recv, None)?;
                if self.cx.as_list(lty).is_some() {
                    if args.len() != 1 {
                        return Err("List.Add takes one argument".into());
                    }
                    let (_, elem) = self.cx.as_list(lty).unwrap();
                    let (val, _) = self.expr(&args[0], Some(elem))?;
                    let holder = self.new_local("$list", lty);
                    self.push(Stmt::Let {
                        local: holder,
                        value: lv,
                    });
                    self.list_add(holder, lty, val);
                    return Ok((Expr::Int(0, TyTable::VOID), TyTable::VOID));
                }
            }
            // `T.InsertSql()` / `T.SelectSql(x => ...)` on a [Table] record.
            // The statement is built at compile time: there is no reflection in
            // the binary, only the string these produce.
            if let ast::ExprKind::Ident(obj) = &recv.kind {
                if let Some(info) = self.cx.tables.get(obj).cloned() {
                    // The `*Sql` pair answer with the statement text; these two
                    // run it. Both build the same SQL at compile time — the
                    // difference is only whether the database is touched.
                    if name == "Insert" {
                        return self.table_insert(&info, args);
                    }
                    if name == "Select" {
                        return self.table_select(obj, &info, args);
                    }
                    if name == "InsertSql" {
                        let cols: Vec<&str> = info
                            .columns
                            .iter()
                            .filter(|(_, _, auto)| !auto)
                            .map(|(_, c, _)| c.as_str())
                            .collect();
                        let holes = vec!["?"; cols.len()].join(", ");
                        let sql = format!(
                            "insert into {} ({}) values ({})",
                            info.table,
                            cols.join(", "),
                            holes
                        );
                        return Ok((Expr::Str(sql), TyTable::STR));
                    }
                    if name == "SelectSql" {
                        let cols: Vec<&str> =
                            info.columns.iter().map(|(_, c, _)| c.as_str()).collect();
                        let head = format!("select {} from {}", cols.join(", "), info.table);
                        if args.is_empty() {
                            return Ok((Expr::Str(head), TyTable::STR));
                        }
                        let ast::ExprKind::Lambda(l) = &args[0].kind else {
                            return Err("SelectSql takes a predicate lambda".into());
                        };
                        let ast::LambdaBody::Expr(body) = &l.body else {
                            return Err("a query predicate must be an expression".into());
                        };
                        let param = l.params.first().map(|(n, _)| n.clone()).unwrap_or_default();
                        let mut where_sql = String::new();
                        let mut binds = Vec::new();
                        self.query_sql(body, &param, &info, &mut where_sql, &mut binds)?;
                        return Ok((Expr::Str(format!("{head} where {where_sql}")), TyTable::STR));
                    }
                }
            }
            // `T.Read(bytes, offset)` on a [Packed] record, and `Bytes.Alloc(n)`.
            if let ast::ExprKind::Ident(obj) = &recv.kind {
                if obj == "Bytes" && name == "Alloc" {
                    if args.len() != 1 {
                        return Err("Bytes.Alloc takes a length".into());
                    }
                    let (n, _) = self.expr(&args[0], Some(TyTable::I32))?;
                    return Ok((self.bytes_alloc(n), TyTable::BYTES));
                }
                if name == "Read" && self.cx.packed.contains(obj) {
                    if args.len() != 2 {
                        return Err(format!("{obj}.Read takes a buffer and an offset"));
                    }
                    let rty = self.cx.record_ty(obj);
                    let TyKind::Record(rid) = *self.tt().kind(rty) else {
                        unreachable!()
                    };
                    let size = match &self.cx.b.m.record(rid).layout {
                        Layout::C { size, .. } => *size,
                        _ => return Err("a [Packed] record needs a C layout".into()),
                    };
                    let (buf, _) = self.expr(&args[0], Some(TyTable::BYTES))?;
                    let (off, _) = self.expr(&args[1], Some(TyTable::I32))?;
                    let dst = self.new_local("$read", rty);
                    self.push(Stmt::Let {
                        local: dst,
                        value: self.alloc(Expr::Int(size as i128, TyTable::I64), rty),
                    });
                    self.push(Stmt::Expr(self.memcpy(
                        Expr::Cast {
                            value: Box::new(Expr::Local(dst)),
                            to: TyTable::PTR,
                        },
                        self.byte_ptr(buf, off),
                        size,
                    )));
                    return Ok((Expr::Local(dst), rty));
                }
            }
            // `record.Write(bytes, offset)` on a [Packed] record.
            if name == "Write" {
                let (rv, rty) = self.expr_raw(recv, None)?;
                if let TyKind::Record(rid) = *self.tt().kind(rty) {
                    let rname = self.cx.b.m.record(rid).name.clone();
                    if self.cx.packed.contains(&rname) {
                        if args.len() != 2 {
                            return Err("Write takes a buffer and an offset".into());
                        }
                        let size = match &self.cx.b.m.record(rid).layout {
                            Layout::C { size, .. } => *size,
                            _ => return Err("a [Packed] record needs a C layout".into()),
                        };
                        let (buf, _) = self.expr(&args[0], Some(TyTable::BYTES))?;
                        let (off, _) = self.expr(&args[1], Some(TyTable::I32))?;
                        self.push(Stmt::Expr(self.memcpy(
                            self.byte_ptr(buf, off),
                            Expr::Cast {
                                value: Box::new(rv),
                                to: TyTable::PTR,
                            },
                            size,
                        )));
                        return Ok((Expr::Int(0, TyTable::VOID), TyTable::VOID));
                    }
                }
            }
            // A call through an interface value dispatches on its table.
            {
                let probe = self.expr_raw(recv, None);
                if let Ok((rv, rty)) = probe {
                    if let Some(iface) = self.cx.as_interface(rty) {
                        return self.interface_call(rv, &iface, name, args);
                    }
                }
            }
            // `dict.ContainsKey(k)` / `dict.Get(k)`
            // `x.ToString()` — the same text interpolating it gives.
            if name == "ToString" && args.is_empty() {
                let (v, vty) = self.expr_raw(recv, None)?;
                if vty == TyTable::STR {
                    return Ok((v, TyTable::STR));
                }
                if matches!(self.tt().kind(vty), TyKind::Record(_) | TyKind::Func { .. }) {
                    return Err("ToString is for numbers, bools and text".into());
                }
                return Ok(self.build_string(vec![(String::new(), Some((v, vty)))]));
            }
            // `xs.RemoveAt(i)` — by position, from 1, checked like a read.
            if name == "RemoveAt" {
                let (lv, lty) = self.expr_raw(recv, None)?;
                if self.cx.as_list(lty).is_some() {
                    if args.len() != 1 {
                        return Err("List.RemoveAt takes a position".into());
                    }
                    let (pos, _) = self.expr(&args[0], Some(TyTable::I32))?;
                    let (data, i0) = self.checked_list_index(lv, lty, pos);
                    let Expr::Field(holder_e, _) = &data else { unreachable!() };
                    let Expr::Local(holder) = **holder_e else { unreachable!() };
                    let at = self.new_local("$at", TyTable::I32);
                    self.push(Stmt::Let { local: at, value: i0 });
                    let d = || Expr::Field(Box::new(Expr::Local(holder)), LIST_DATA);
                    let len = || Expr::Field(Box::new(Expr::Local(holder)), LIST_LEN);
                    let plus1 = |e: Expr| {
                        Expr::Bin(BinOp::Add, Box::new(e), Box::new(Expr::Int(1, TyTable::I32)), TyTable::I32)
                    };
                    self.push(Stmt::Loop {
                        body: vec![
                            Stmt::If {
                                cond: Expr::Not(Box::new(Expr::Bin(
                                    BinOp::Lt,
                                    Box::new(plus1(Expr::Local(at))),
                                    Box::new(len()),
                                    TyTable::I32,
                                ))),
                                then: vec![Stmt::Break],
                                els: vec![],
                            },
                            Stmt::Assign {
                                place: Place::Index(Box::new(d()), Box::new(Expr::Local(at))),
                                value: Expr::Index(Box::new(d()), Box::new(plus1(Expr::Local(at)))),
                            },
                            Stmt::Assign {
                                place: Place::Local(at),
                                value: plus1(Expr::Local(at)),
                            },
                        ],
                    });
                    self.push(Stmt::Assign {
                        place: Place::Field(Box::new(Expr::Local(holder)), LIST_LEN),
                        value: Expr::Bin(
                            BinOp::Sub,
                            Box::new(len()),
                            Box::new(Expr::Int(1, TyTable::I32)),
                            TyTable::I32,
                        ),
                    });
                    return Ok((Expr::Int(0, TyTable::VOID), TyTable::VOID));
                }
            }
            // `d.Remove(k)` — true if the key was there. In place: the dictionary
            // is shared by reference, so everyone holding it sees it go.
            if name == "Remove" {
                let (dv, dty) = self.expr_raw(recv, None)?;
                if let Some((_, kty, _)) = self.cx.as_dict(dty) {
                    if args.len() != 1 {
                        return Err("Dictionary.Remove takes a key".into());
                    }
                    let holder = self.new_local("$dict", dty);
                    self.push(Stmt::Let {
                        local: holder,
                        value: dv,
                    });
                    let (k, _) = self.expr(&args[0], Some(kty))?;
                    let removed = self.dict_remove(holder, dty, k)?;
                    return Ok((Expr::Local(removed), TyTable::BOOL));
                }
                if let Some((_, elem)) = self.cx.as_list(dty) {
                    if args.len() != 1 {
                        return Err("List.Remove takes the item to remove".into());
                    }
                    let holder = self.new_local("$list", dty);
                    self.push(Stmt::Let {
                        local: holder,
                        value: dv,
                    });
                    let (x, _) = self.expr(&args[0], Some(elem))?;
                    let removed = self.list_remove(holder, dty, x)?;
                    return Ok((Expr::Local(removed), TyTable::BOOL));
                }
            }
            // `xs.Sort()` — in place, ascending; text by content. An insertion
            // sort: stable, and a list a program sorts is rarely large.
            if name == "Sort" && args.is_empty() {
                let (lv, lty) = self.expr_raw(recv, None)?;
                if let Some((_, elem)) = self.cx.as_list(lty) {
                    let holder = self.new_local("$list", lty);
                    self.push(Stmt::Let { local: holder, value: lv });
                    let data = || Expr::Field(Box::new(Expr::Local(holder)), LIST_DATA);
                    let len = || Expr::Field(Box::new(Expr::Local(holder)), LIST_LEN);
                    let i = self.new_local("$si", TyTable::I32);
                    let j = self.new_local("$sj", TyTable::I32);
                    let key = self.new_local("$skey", elem);
                    let one = |e: Expr, op: BinOp| Expr::Bin(op, Box::new(e), Box::new(Expr::Int(1, TyTable::I32)), TyTable::I32);
                    let at = |loc: LocalId| Expr::Index(Box::new(data()), Box::new(Expr::Local(loc)));
                    // greater(data[j], key)
                    let greater = if elem == TyTable::STR {
                        Expr::Bin(
                            BinOp::Gt,
                            Box::new(Expr::Call(Box::new(Call::Dll {
                                library: "c".into(),
                                symbol: "strcmp".into(),
                                conv: CallConv::Cdecl,
                                args: vec![at(j), Expr::Local(key)],
                                arg_tys: vec![TyTable::STR, TyTable::STR],
                                ret: TyTable::I32,
                                varargs: false,
                            }))),
                            Box::new(Expr::Int(0, TyTable::I32)),
                            TyTable::I32,
                        )
                    } else {
                        Expr::Bin(BinOp::Gt, Box::new(at(j)), Box::new(Expr::Local(key)), elem)
                    };
                    self.push(Stmt::Let { local: i, value: Expr::Int(1, TyTable::I32) });
                    let inner = vec![
                        Stmt::If {
                            cond: Expr::Bin(BinOp::Lt, Box::new(Expr::Local(j)), Box::new(Expr::Int(0, TyTable::I32)), TyTable::I32),
                            then: vec![Stmt::Break],
                            els: vec![],
                        },
                        Stmt::If {
                            cond: Expr::Not(Box::new(greater)),
                            then: vec![Stmt::Break],
                            els: vec![],
                        },
                        Stmt::Assign {
                            place: Place::Index(Box::new(data()), Box::new(one(Expr::Local(j), BinOp::Add))),
                            value: at(j),
                        },
                        Stmt::Assign { place: Place::Local(j), value: one(Expr::Local(j), BinOp::Sub) },
                    ];
                    let outer = vec![
                        Stmt::If {
                            cond: Expr::Not(Box::new(Expr::Bin(BinOp::Lt, Box::new(Expr::Local(i)), Box::new(len()), TyTable::I32))),
                            then: vec![Stmt::Break],
                            els: vec![],
                        },
                        Stmt::Assign { place: Place::Local(key), value: at(i) },
                        Stmt::Assign { place: Place::Local(j), value: one(Expr::Local(i), BinOp::Sub) },
                        Stmt::Loop { body: inner },
                        Stmt::Assign {
                            place: Place::Index(Box::new(data()), Box::new(one(Expr::Local(j), BinOp::Add))),
                            value: Expr::Local(key),
                        },
                        Stmt::Assign { place: Place::Local(i), value: one(Expr::Local(i), BinOp::Add) },
                    ];
                    self.push(Stmt::Loop { body: outer });
                    return Ok((Expr::Int(0, TyTable::VOID), TyTable::VOID));
                }
            }
            // `xs.IndexOf(x)` — the position of the first `x`, from 1; 0 when
            // it is not there, as every position in Kiln counts.
            if name == "IndexOf" {
                let (lv, lty) = self.expr_raw(recv, None)?;
                if let Some((_, elem)) = self.cx.as_list(lty) {
                    if args.len() != 1 {
                        return Err("List.IndexOf takes the item to look for".into());
                    }
                    let holder = self.new_local("$list", lty);
                    self.push(Stmt::Let { local: holder, value: lv });
                    let (x, _) = self.expr(&args[0], Some(elem))?;
                    let at = self.list_find(holder, elem, x)?;
                    return Ok((
                        Expr::Bin(BinOp::Add, Box::new(Expr::Local(at)), Box::new(Expr::Int(1, TyTable::I32)), TyTable::I32),
                        TyTable::I32,
                    ));
                }
            }
            // `xs.Slice(from, to)` — positions `from` to `to`, both included, as a
            // new list. Clamped rather than refused, as 1.x's slice is: asking
            // for more than is there gives what is there.
            if name == "Slice" {
                let (lv, lty) = self.expr_raw(recv, None)?;
                if let Some((lrid, elem)) = self.cx.as_list(lty) {
                    if args.len() != 2 {
                        return Err("List.Slice takes a first and a last position".into());
                    }
                    let holder = self.new_local("$list", lty);
                    self.push(Stmt::Let { local: holder, value: lv });
                    let (from, _) = self.expr(&args[0], Some(TyTable::I32))?;
                    let (to, _) = self.expr(&args[1], Some(TyTable::I32))?;
                    let lo = self.new_local("$lo", TyTable::I32);
                    let hi = self.new_local("$hi", TyTable::I32);
                    self.push(Stmt::Let { local: lo, value: from });
                    self.push(Stmt::Let { local: hi, value: to });
                    let len = || Expr::Field(Box::new(Expr::Local(holder)), LIST_LEN);
                    let clamp = |loc: LocalId, cmp: BinOp, bound: Expr| Stmt::If {
                        cond: Expr::Bin(cmp, Box::new(Expr::Local(loc)), Box::new(bound.clone()), TyTable::I32),
                        then: vec![Stmt::Assign { place: Place::Local(loc), value: bound }],
                        els: vec![],
                    };
                    self.push(clamp(lo, BinOp::Lt, Expr::Int(1, TyTable::I32)));
                    self.push(clamp(hi, BinOp::Gt, len()));
                    let data_ty = self.cx.b.m.record(lrid).fields[LIST_DATA].ty;
                    let out = self.new_local("$slice", lty);
                    self.push(Stmt::Let {
                        local: out,
                        value: Expr::MakeRecord(lrid, vec![Expr::Int(0, TyTable::I32), Expr::Int(0, TyTable::I32), Expr::Null(data_ty)]),
                    });
                    let item = Expr::Index(
                        Box::new(Expr::Field(Box::new(Expr::Local(holder)), LIST_DATA)),
                        Box::new(Expr::Bin(BinOp::Sub, Box::new(Expr::Local(lo)), Box::new(Expr::Int(1, TyTable::I32)), TyTable::I32)),
                    );
                    self.blocks.push(Vec::new());
                    self.list_add(out, lty, item);
                    let add = self.blocks.pop().unwrap();
                    let _ = elem;
                    let mut body = vec![Stmt::If {
                        cond: Expr::Bin(BinOp::Gt, Box::new(Expr::Local(lo)), Box::new(Expr::Local(hi)), TyTable::I32),
                        then: vec![Stmt::Break],
                        els: vec![],
                    }];
                    body.extend(add);
                    body.push(Stmt::Assign {
                        place: Place::Local(lo),
                        value: Expr::Bin(BinOp::Add, Box::new(Expr::Local(lo)), Box::new(Expr::Int(1, TyTable::I32)), TyTable::I32),
                    });
                    self.push(Stmt::Loop { body });
                    return Ok((Expr::Local(out), lty));
                }
            }
            // `xs.Contains(x)` on a list: a scan, since a list has no index.
            if name == "Contains" {
                let (lv, lty) = self.expr_raw(recv, None)?;
                if let Some((_, elem)) = self.cx.as_list(lty) {
                    if args.len() != 1 {
                        return Err("List.Contains takes the item to look for".into());
                    }
                    let holder = self.new_local("$list", lty);
                    self.push(Stmt::Let {
                        local: holder,
                        value: lv,
                    });
                    let (x, _) = self.expr(&args[0], Some(elem))?;
                    let at = self.list_find(holder, elem, x)?;
                    return Ok((
                        Expr::Bin(
                            BinOp::Ge,
                            Box::new(Expr::Local(at)),
                            Box::new(Expr::Int(0, TyTable::I32)),
                            TyTable::I32,
                        ),
                        TyTable::BOOL,
                    ));
                }
            }
            if name == "ContainsKey" || name == "Get" {
                let (dv, dty) = self.expr_raw(recv, None)?;
                if let Some((_, kty, vty)) = self.cx.as_dict(dty) {
                    if args.len() != 1 {
                        return Err(format!("Dictionary.{name} takes one argument"));
                    }
                    let holder = self.new_local("$dict", dty);
                    self.push(Stmt::Let {
                        local: holder,
                        value: dv,
                    });
                    let (k, _) = self.expr(&args[0], Some(kty))?;
                    let found = self.dict_find(holder, dty, k)?;
                    let present = Expr::Bin(
                        BinOp::Ge,
                        Box::new(Expr::Local(found)),
                        Box::new(Expr::Int(0, TyTable::I32)),
                        TyTable::I32,
                    );
                    if name == "ContainsKey" {
                        return Ok((present, TyTable::BOOL));
                    }
                    // `.Get(k)` yields `V?` — absence is a value, not a throw.
                    let opt_ty = self.cx.b.m.types.intern(TyKind::Optional(vty));
                    let out = self.new_local("$got", opt_ty);
                    self.push(Stmt::If {
                        cond: present,
                        then: vec![Stmt::Assign {
                            place: Place::Local(out),
                            value: Expr::MakeOptional(
                                vty,
                                Some(Box::new(Expr::Index(
                                    Box::new(Expr::Field(
                                        Box::new(Expr::Local(holder)),
                                        DICT_VALUES,
                                    )),
                                    Box::new(Expr::Local(found)),
                                ))),
                            ),
                        }],
                        els: vec![Stmt::Assign {
                            place: Place::Local(out),
                            value: Expr::MakeOptional(vty, None),
                        }],
                    });
                    return Ok((Expr::Local(out), opt_ty));
                }
            }
            // `list.Where(pred)` / `list.Select(f)` / `list.Any(pred)` /
            // `list.First(pred)` / `list.OrderBy(key)` — written here rather
            // than in K2 until the standard library exists.
            if matches!(name.as_str(), "Where" | "Select" | "Any" | "First" | "OrderBy") {
                let (lv, lty) = self.expr_raw(recv, None)?;
                if self.cx.as_list(lty).is_some() {
                    if name == "OrderBy" {
                        return self.list_order_by(lv, lty, args);
                    }
                    return self.list_query(name, lv, lty, args);
                }
            }
        }
        // Resolve a method: `Type.Method(..)` (static) or `recv.Method(..)`.
        // `Owner.Member(...)` may name a standard-library command: the spec's
        // rule is that `file_read_text` is written `File.ReadText`, so the
        // name is reversed and looked up.
        if let ast::ExprKind::Member(recv, member) = &callee.kind {
            if let ast::ExprKind::Ident(owner) = &recv.kind {
                // `Owner` must name a library, not a value in scope: `s.Uppercase()`
                // is an instance call on `s`, not the static command `uppercase`.
                let is_value = self.scope.contains_key(owner.as_str())
                    || self.cells.contains_key(owner.as_str())
                    || self.cx.form_state.contains_key(owner.as_str())
                    || self.cx.components.contains_key(owner.as_str());
                if !is_value && (!self.is_type_name(owner) || owner == "Console") {
                    if let Some((sym, params, ret, tags)) = self.lookup_command(owner, member) {
                        if args.len() != params.len() {
                            return Err(format!(
                                "`{owner}.{member}` expects {} argument(s), got {}",
                                params.len(),
                                args.len()
                            ));
                        }
                        let mut kargs = Vec::new();
                        let who = format!("{owner}.{member}");
                        for (i, (a, pty)) in args.iter().zip(params.iter()).enumerate() {
                            kargs.push(self.command_arg(&who, i + 1, a, *pty, tags[i])?);
                        }
                        let slots = params
                            .iter()
                            .zip(tags.iter())
                            .map(|(ty, tag)| SlotTy { tag: *tag, ty: *ty })
                            .collect();
                        let call = Expr::Call(Box::new(Call::Command {
                            symbol: sym,
                            args: kargs,
                            arg_slots: slots,
                            ret,
                        }));
                        // A command that answers with a list hands back a
                        // runtime array; the language has `List<T>`.
                        if let TyKind::Array(e) = *self.tt().kind(ret) {
                            return self.array_to_list(call, e);
                        }
                        return Ok((call, ret));
                    }
                }
            }
        }
        let (key, this_arg, this_ty): (String, Option<Expr>, TyId) = match &callee.kind {
            ast::ExprKind::Ident(name) => (name.clone(), None, TyTable::VOID),
            ast::ExprKind::Member(recv, name) => {
                if let ast::ExprKind::Ident(obj) = &recv.kind {
                    let qualified = format!("{obj}.{name}");
                    if (self.cx.methods.contains_key(&qualified)
                        || self.cx.generics.contains_key(&qualified)
                        || self.cx.dlls.contains_key(&qualified))
                        && self.is_type_name(obj)
                    {
                        (qualified, None, TyTable::VOID)
                    } else {
                        // instance call: prefer the receiver's own method.
                        let (recv_v, rty) = self.expr(recv, None)?;
                        let key = self
                            .cx
                            .record_name(rty)
                            .map(|r| format!("{r}.{name}"))
                            .filter(|k| self.cx.methods.contains_key(k))
                            .unwrap_or_else(|| name.clone());
                        (key, Some(recv_v), rty)
                    }
                } else {
                    let (recv_v, rty) = self.expr(recv, None)?;
                    let key = self
                        .cx
                        .record_name(rty)
                        .map(|r| format!("{r}.{name}"))
                        .filter(|k| self.cx.methods.contains_key(k))
                        .unwrap_or_else(|| name.clone());
                    (key, Some(recv_v), rty)
                }
            }
            _ => return Err("unsupported call target".into()),
        };
        // A `[Dll]` extern call.
        if let Some(d) = self.cx.dlls.get(&key).cloned() {
            if args.len() != d.params.len() {
                return Err(format!(
                    "`{key}` expects {} argument(s), got {}",
                    d.params.len(),
                    args.len()
                ));
            }
            let mut kargs = Vec::new();
            let mut arg_tys = Vec::new();
            for (a, pty) in args.iter().zip(d.params.iter()) {
                let v = self.expr(a, Some(*pty))?.0;
                // A C function taking a buffer wants the bytes, not the header
                // in front of them.
                if *pty == TyTable::BYTES {
                    kargs.push(self.byte_ptr(v, Expr::Int(0, TyTable::I32)));
                    arg_tys.push(TyTable::PTR);
                } else {
                    kargs.push(v);
                    arg_tys.push(*pty);
                }
            }
            return Ok((
                Expr::Call(Box::new(Call::Dll {
                    library: d.library,
                    symbol: d.symbol,
                    conv: d.conv,
                    args: kargs,
                    arg_tys,
                    ret: d.ret,
                    varargs: false,
                })),
                d.ret,
            ));
        }
        // A generic method: lower the arguments first (their types are what the
        // type parameters are inferred from), then instantiate.
        if self.cx.generics.contains_key(&key) {
            // With the type arguments written, each parameter's type is known
            // before its argument is lowered, so `Pick<long>(5, 6)` widens the
            // literals rather than inferring `int` and conflicting.
            let mut hints: Vec<Option<TyId>> = Vec::new();
            if let Some(ex) = &explicit {
                let method = self.cx.generics[&key].method.clone();
                if ex.len() == method.type_params.len() {
                    let mut tv = self.cx.tvars.clone();
                    for (tp, t) in method.type_params.iter().zip(ex) {
                        tv.insert(tp.clone(), *t);
                    }
                    let saved = std::mem::replace(&mut self.cx.tvars, tv);
                    for p in &method.params {
                        hints.push(self.cx.resolve(&p.ty).ok());
                    }
                    self.cx.tvars = saved;
                }
            }
            let mut lowered = Vec::new();
            for (i, a) in args.iter().enumerate() {
                lowered.push(self.expr(a, hints.get(i).copied().flatten())?);
            }
            let sig = self.instantiate(&key, &lowered, explicit.as_deref())?;
            let mut kargs: Vec<Expr> = Vec::new();
            if let Some(this) = this_arg {
                kargs.push(this);
            }
            kargs.extend(lowered.into_iter().map(|(e, _)| e));
            return Ok((
                Expr::Call(Box::new(Call::Direct {
                    func: sig.fid,
                    args: kargs,
                })),
                sig.ret,
            ));
        }

        // `value.Member(args)` may be a command taking the receiver first.
        if let (Some(this), ast::ExprKind::Member(_, member)) = (&this_arg, &callee.kind) {
            if let Some((sym, params, ret, tags)) =
                self.lookup_instance_command(this_ty, member, args.len())
            {
                let mut kargs = vec![this.clone()];
                for (i, (a, pty)) in args.iter().zip(params.iter().skip(1)).enumerate() {
                    // The receiver is argument 1, so the rest start at 2.
                    kargs.push(self.command_arg(member, i + 2, a, *pty, tags[i + 1])?);
                }
                let slots = params
                    .iter()
                    .zip(tags.iter())
                    .map(|(ty, tag)| SlotTy { tag: *tag, ty: *ty })
                    .collect();
                let call = Expr::Call(Box::new(Call::Command {
                    symbol: sym,
                    args: kargs,
                    arg_slots: slots,
                    ret,
                }));
                if let TyKind::Array(e) = *self.tt().kind(ret) {
                    return self.array_to_list(call, e);
                }
                return Ok((call, ret));
            }
        }
        // A bare call to a standard-library command: `IntToText(n)` is
        // `int_to_text(n)`. Core's commands have no owner to put in front of
        // them — they are not `Text.` anything — so the free call is how they
        // are written, and a user method of the same name still wins.
        if this_arg.is_none() && !key.contains('.') && !self.cx.methods.contains_key(&key) {
            if let Some(found) = self.bare_command(&key, args)? {
                return Ok(found);
            }
        }
        let sig = self
            .cx
            .methods
            .get(&key)
            .cloned()
            .ok_or_else(|| format!("unknown method `{key}`"))?;
        let mut kargs = Vec::new();
        if let Some(this) = this_arg {
            kargs.push(this);
        }
        kargs.extend(self.call_args(&key, args, &sig)?);
        Ok((
            Expr::Call(Box::new(Call::Direct {
                func: sig.fid,
                args: kargs,
            })),
            sig.ret,
        ))
    }

    /// Append to a list held in `holder`: grow the buffer when it is full, then
    /// store and bump the length.
    fn list_add(&mut self, holder: LocalId, lty: TyId, val: Expr) {
        let (lrid, elem) = self.cx.as_list(lty).expect("a list");
        let l = || Expr::Local(holder);
        let len = || Expr::Field(Box::new(l()), LIST_LEN);
        let cap = || Expr::Field(Box::new(l()), LIST_CAP);
        let esize = self.cx.scalar_size(elem);
        let data_ty = self.cx.b.m.record(lrid).fields[LIST_DATA].ty;
        let newcap = self.new_local("$newcap", TyTable::I32);
        let grow = vec![
            Stmt::If {
                cond: Expr::Bin(
                    BinOp::Eq,
                    Box::new(cap()),
                    Box::new(Expr::Int(0, TyTable::I32)),
                    TyTable::I32,
                ),
                then: vec![Stmt::Assign {
                    place: Place::Local(newcap),
                    value: Expr::Int(4, TyTable::I32),
                }],
                els: vec![Stmt::Assign {
                    place: Place::Local(newcap),
                    value: Expr::Bin(
                        BinOp::Mul,
                        Box::new(cap()),
                        Box::new(Expr::Int(2, TyTable::I32)),
                        TyTable::I32,
                    ),
                }],
            },
            Stmt::Assign {
                place: Place::Field(Box::new(l()), LIST_DATA),
                value: self.realloc(
                    Expr::Field(Box::new(l()), LIST_DATA),
                    Expr::Bin(
                        BinOp::Mul,
                        Box::new(Expr::Local(newcap)),
                        Box::new(Expr::Int(esize as i128, TyTable::I32)),
                        TyTable::I32,
                    ),
                    data_ty,
                ),
            },
            Stmt::Assign {
                place: Place::Field(Box::new(l()), LIST_CAP),
                value: Expr::Local(newcap),
            },
        ];
        self.push(Stmt::If {
            cond: Expr::Bin(BinOp::Eq, Box::new(len()), Box::new(cap()), TyTable::I32),
            then: grow,
            els: vec![],
        });
        self.push(Stmt::Assign {
            place: Place::Index(
                Box::new(Expr::Field(Box::new(l()), LIST_DATA)),
                Box::new(len()),
            ),
            value: val,
        });
        self.push(Stmt::Assign {
            place: Place::Field(Box::new(l()), LIST_LEN),
            value: Expr::Bin(
                BinOp::Add,
                Box::new(len()),
                Box::new(Expr::Int(1, TyTable::I32)),
                TyTable::I32,
            ),
        });
    }

    /// Translate a predicate lambda into a SQL `where` clause at compile time.
    ///
    /// Only the shapes a database can evaluate are accepted — comparisons,
    /// `&&`/`||`/`!`, a column of the row, and constants. Anything else is a
    /// compile error naming the unsupported piece, rather than something that
    /// silently runs in the wrong place.
    /// Translate a query predicate to SQL, collecting into `binds` every value
    /// that becomes a `?`. A captured value is never pasted into the statement:
    /// it is bound, so a query cannot be built wrong by its own data.
    fn query_sql(
        &mut self,
        e: &ast::Expr,
        param: &str,
        info: &TableInfo,
        out: &mut String,
        binds: &mut Vec<ast::Expr>,
    ) -> Result<(), String> {
        use ast::ExprKind as E;
        match &e.kind {
            E::Binary(op, a, b) => {
                let sym = match op {
                    ast::BinOp::Eq => "=",
                    ast::BinOp::Ne => "<>",
                    ast::BinOp::Lt => "<",
                    ast::BinOp::Le => "<=",
                    ast::BinOp::Gt => ">",
                    ast::BinOp::Ge => ">=",
                    ast::BinOp::And => "and",
                    ast::BinOp::Or => "or",
                    other => {
                        return Err(format!("`{other:?}` cannot be translated to SQL"));
                    }
                };
                out.push('(');
                self.query_sql(a, param, info, out, binds)?;
                out.push_str(&format!(" {sym} "));
                self.query_sql(b, param, info, out, binds)?;
                out.push(')');
                Ok(())
            }
            E::Unary(ast::UnOp::Not, inner) => {
                out.push_str("not ");
                self.query_sql(inner, param, info, out, binds)
            }
            // `x.Column` — the row's column.
            E::Member(recv, field) => {
                if let E::Ident(n) = &recv.kind {
                    if n == param {
                        let col = info
                            .columns
                            .iter()
                            .find(|(f, _, _)| f == field)
                            .map(|(_, c, _)| c.clone())
                            .ok_or_else(|| {
                                format!("`{field}` is not a column of `{}`", info.table)
                            })?;
                        out.push_str(&col);
                        return Ok(());
                    }
                }
                Err("a query may only reach the row's own columns".into())
            }
            E::Int(v) => {
                out.push_str(&v.to_string());
                Ok(())
            }
            E::Bool(b) => {
                out.push_str(if *b { "1" } else { "0" });
                Ok(())
            }
            E::Str(_) => {
                // A literal string is still parameterised, never interpolated.
                out.push('?');
                binds.push(e.clone());
                Ok(())
            }
            // Anything captured from outside becomes a bound parameter.
            E::Ident(_) => {
                out.push('?');
                binds.push(e.clone());
                Ok(())
            }
            other => Err(format!("this cannot be translated to SQL: {other:?}")),
        }
    }

    /// Build an interface value from a concrete one.
    fn to_interface(
        &mut self,
        v: Expr,
        concrete: &str,
        iface: &str,
        want: TyId,
    ) -> Result<(Expr, TyId), String> {
        let methods = self
            .cx
            .impls
            .get(&(concrete.to_string(), iface.to_string()))
            .cloned()
            .ok_or_else(|| format!("`{concrete}` does not implement `{iface}`"))?;
        let rid = self.cx.iface_records[iface];
        let mut fields = vec![Expr::Cast {
            value: Box::new(v),
            to: TyTable::PTR,
        }];
        for m in &methods {
            let sig = self
                .cx
                .methods
                .get(&format!("{concrete}.{m}"))
                .ok_or_else(|| format!("`{concrete}.{m}` is not defined"))?;
            fields.push(Expr::FuncPtr(sig.fid));
        }
        Ok((Expr::MakeRecord(rid, fields), want))
    }

    /// Call a method through an interface value: the stored function pointer,
    /// with the stored object as its receiver.
    fn interface_call(
        &mut self,
        recv: Expr,
        iface: &str,
        method: &str,
        args: &[ast::Expr],
    ) -> Result<(Expr, TyId), String> {
        let names = self.cx.interfaces[iface].clone();
        let idx = names
            .iter()
            .position(|n| n == method)
            .ok_or_else(|| format!("`{iface}` has no method `{method}`"))?;
        // Any implementation has the interface's signature; take it from one.
        let sig = self
            .cx
            .impls
            .iter()
            .find(|((_, i), _)| i == iface)
            .and_then(|((t, _), _)| self.cx.methods.get(&format!("{t}.{method}")).cloned())
            .ok_or_else(|| format!("nothing implements `{iface}`"))?;

        let holder = self.new_local(
            "$iface",
            self.tt()
                .find(&TyKind::Record(self.cx.iface_records[iface]))
                .unwrap(),
        );
        self.push(Stmt::Let {
            local: holder,
            value: recv,
        });
        let fn_ty = self.cx.b.m.types.intern(TyKind::Func {
            params: sig.params.clone(),
            ret: sig.ret,
        });
        let callee = Expr::FuncValue {
            fn_ptr: Box::new(Expr::Field(Box::new(Expr::Local(holder)), idx + 1)),
            env: Box::new(Expr::Field(Box::new(Expr::Local(holder)), 0)),
        };
        let kargs = self.call_args(&format!("{iface}.{method}"), args, &sig)?;
        Ok((
            Expr::Call(Box::new(Call::Indirect {
                callee: Box::new(callee),
                args: kargs,
                sig: fn_ty,
            })),
            sig.ret,
        ))
    }

    /// Turn a command call into a `Result<T>` by reading the error slot it
    /// writes. The value is evaluated once, whatever the verdict.
    fn wrap_error_slot(
        &mut self,
        call: Expr,
        vty: TyId,
        rid: RecordId,
        result_vt: TyId,
    ) -> Result<(Expr, TyId), String> {
        let held = self.new_local("$called", vty);
        self.push(Stmt::Let {
            local: held,
            value: call,
        });
        let code = self.error_slot("last_error_code", TyTable::I32)?;
        let failed = Expr::Bin(
            BinOp::Ne,
            Box::new(code),
            Box::new(Expr::Int(0, TyTable::I32)),
            TyTable::I32,
        );
        let rty = self.cx.b.m.types.intern(TyKind::Record(rid));
        let out = self.new_local("$result", rty);
        let text = self.error_slot("last_error_text", TyTable::STR)?;
        self.push(Stmt::If {
            cond: failed,
            then: vec![Stmt::Assign {
                place: Place::Local(out),
                value: Expr::MakeRecord(
                    rid,
                    vec![Expr::Bool(false), zero_of(self.tt(), result_vt), text],
                ),
            }],
            els: vec![Stmt::Assign {
                place: Place::Local(out),
                value: Expr::MakeRecord(
                    rid,
                    vec![
                        Expr::Bool(true),
                        Expr::Local(held),
                        Expr::Str(String::new()),
                    ],
                ),
            }],
        });
        Ok((Expr::Local(out), rty))
    }

    /// Call a `libs/db` command by its Kiln name, with the slot tags its
    /// declared signature asks for.
    fn db_call(
        &mut self,
        name: &str,
        args: Vec<(Expr, TyId)>,
        ret: TyId,
    ) -> Result<Expr, String> {
        let reg = self.cx.registry.as_ref().ok_or_else(|| {
            format!("`{name}` needs the `db` library — add `using Kiln.Db;`")
        })?;
        let cmd = reg
            .get(name)
            .ok_or_else(|| format!("the `db` library does not provide `{name}`"))?;
        let symbol = cmd.symbol.clone();
        let mut slots = Vec::new();
        let mut vals = Vec::new();
        for (v, t) in args {
            slots.push(SlotTy {
                tag: self.cx.b.m.types.sdt_tag(t),
                ty: t,
            });
            vals.push(v);
        }
        Ok(Expr::Call(Box::new(Call::Command {
            symbol,
            args: vals,
            arg_slots: slots,
            ret,
        })))
    }

    /// A value as text, which is how `db_exec`/`db_query` bind parameters:
    /// their parameter list is an array of text.
    fn as_text(&mut self, v: Expr, ty: TyId) -> Expr {
        if ty == TyTable::STR {
            return v;
        }
        self.build_string(vec![(String::new(), Some((v, ty)))]).0
    }

    /// `T.Insert(handle, row)` — the statement built at compile time, the
    /// values bound at run time. An `[Auto]` column is left to the database.
    fn table_insert(
        &mut self,
        info: &TableInfo,
        args: &[ast::Expr],
    ) -> Result<(Expr, TyId), String> {
        if args.len() != 2 {
            return Err("Insert takes a database handle and a row".into());
        }
        let (h, _) = self.expr(&args[0], Some(TyTable::I32))?;
        let (row, row_ty) = self.expr(&args[1], None)?;
        let held = self.new_local("$row", row_ty);
        self.push(Stmt::Let {
            local: held,
            value: row,
        });

        let cols: Vec<&str> = info
            .columns
            .iter()
            .filter(|(_, _, auto)| !auto)
            .map(|(_, c, _)| c.as_str())
            .collect();
        let sql = format!(
            "insert into {} ({}) values ({})",
            info.table,
            cols.join(", "),
            vec!["?"; cols.len()].join(", ")
        );

        // Each non-auto field, in declaration order, as text.
        let mut binds = Vec::new();
        for (i, (_, _, auto)) in info.columns.iter().enumerate() {
            if *auto {
                continue;
            }
            let fty = self.cx.resolve(&info.types[i])?;
            let v = Expr::Field(Box::new(Expr::Local(held)), i);
            binds.push(self.as_text(v, fty));
        }
        let arr_ty = self.cx.b.m.types.intern(TyKind::Array(TyTable::STR));
        let params = Expr::MakeArray(TyTable::STR, binds);
        let call = self.db_call(
            "db_exec",
            vec![
                (h, TyTable::I32),
                (Expr::Str(sql), TyTable::STR),
                (params, arr_ty),
            ],
            TyTable::I32,
        )?;
        Ok((call, TyTable::I32))
    }

    /// `T.Select(handle, x => predicate)` — the query built at compile time,
    /// run, and every row read back into a `List<T>`.
    ///
    /// Nothing about the row's shape reaches the binary: which `db_*` reader
    /// each column uses is decided here, from the record's declared types.
    fn table_select(
        &mut self,
        type_name: &str,
        info: &TableInfo,
        args: &[ast::Expr],
    ) -> Result<(Expr, TyId), String> {
        if args.is_empty() {
            return Err("Select takes a database handle".into());
        }
        let (h, _) = self.expr(&args[0], Some(TyTable::I32))?;

        let cols: Vec<&str> = info.columns.iter().map(|(_, c, _)| c.as_str()).collect();
        let mut sql = format!("select {} from {}", cols.join(", "), info.table);
        let mut bind_exprs: Vec<ast::Expr> = Vec::new();
        if let Some(pred) = args.get(1) {
            let ast::ExprKind::Lambda(l) = &pred.kind else {
                return Err("Select takes a predicate lambda".into());
            };
            let ast::LambdaBody::Expr(body) = &l.body else {
                return Err("a query predicate must be an expression".into());
            };
            let param = l.params.first().map(|(n, _)| n.clone()).unwrap_or_default();
            let mut where_sql = String::new();
            self.query_sql(body, &param, info, &mut where_sql, &mut bind_exprs)?;
            sql = format!("{sql} where {where_sql}");
        }
        let mut binds = Vec::new();
        for b in &bind_exprs {
            let (v, t) = self.expr(b, None)?;
            binds.push(self.as_text(v, t));
        }
        let arr_ty = self.cx.b.m.types.intern(TyKind::Array(TyTable::STR));
        let query = self.db_call(
            "db_query",
            vec![
                (h, TyTable::I32),
                (Expr::Str(sql), TyTable::STR),
                (Expr::MakeArray(TyTable::STR, binds), arr_ty),
            ],
            TyTable::I32,
        )?;
        let rows = self.new_local("$rows", TyTable::I32);
        self.push(Stmt::Let {
            local: rows,
            value: query,
        });

        // The list this answers with.
        let rid = *self
            .cx
            .type_ids
            .get(type_name)
            .ok_or_else(|| format!("`{type_name}` is not a record"))?;
        let elem = self.cx.b.m.types.intern(TyKind::Record(rid));
        let lrid = self.cx.list_record(elem);
        let lty = self.cx.b.m.types.intern(TyKind::Record(lrid));
        let data_ty = self.cx.b.m.record(lrid).fields[LIST_DATA].ty;
        let out = self.new_local("$rowsout", lty);
        self.push(Stmt::Let {
            local: out,
            value: Expr::MakeRecord(
                lrid,
                vec![
                    Expr::Int(0, TyTable::I32),
                    Expr::Int(0, TyTable::I32),
                    Expr::Null(data_ty),
                ],
            ),
        });

        // while db_next(rows) { out.Add(new T(col 1, col 2, …)) }
        let next = self.db_call("db_next", vec![(Expr::Local(rows), TyTable::I32)], TyTable::BOOL)?;
        let mut body = vec![Stmt::If {
            cond: Expr::Not(Box::new(next)),
            then: vec![Stmt::Break],
            els: vec![],
        }];
        let mut fields = Vec::new();
        for (i, ty_ref) in info.types.iter().enumerate() {
            let fty = self.cx.resolve(ty_ref)?;
            // Columns are counted from 1, as every position in Kiln is.
            let reader = match self.cx.b.m.types.kind(fty) {
                TyKind::Str => "db_text",
                TyKind::F32 | TyKind::F64 => "db_double",
                TyKind::Bool => "db_bool",
                TyKind::I64 | TyKind::U64 | TyKind::Nint | TyKind::Nuint => "db_int64",
                _ => "db_int",
            };
            let raw_ty = match reader {
                "db_text" => TyTable::STR,
                "db_double" => TyTable::F64,
                "db_bool" => TyTable::BOOL,
                "db_int64" => TyTable::I64,
                _ => TyTable::I32,
            };
            let read = self.db_call(
                reader,
                vec![
                    (Expr::Local(rows), TyTable::I32),
                    (Expr::Int(i as i128 + 1, TyTable::I32), TyTable::I32),
                ],
                raw_ty,
            )?;
            fields.push(if raw_ty == fty {
                read
            } else {
                Expr::Cast {
                    value: Box::new(read),
                    to: fty,
                }
            });
        }
        let item = self.new_local("$row", elem);
        body.push(Stmt::Let {
            local: item,
            value: Expr::MakeRecord(rid, fields),
        });
        self.blocks.push(Vec::new());
        self.list_add(out, lty, Expr::Local(item));
        let add = self.blocks.pop().unwrap();
        body.extend(add);
        self.push(Stmt::Loop { body });

        let close = self.db_call(
            "db_result_close",
            vec![(Expr::Local(rows), TyTable::I32)],
            TyTable::BOOL,
        )?;
        self.push(Stmt::Expr(close));
        Ok((Expr::Local(out), lty))
    }

    /// A no-argument command that reads the error slot.
    fn error_slot(&mut self, name: &str, ret: TyId) -> Result<Expr, String> {
        let reg = self
            .cx
            .registry
            .as_ref()
            .ok_or_else(|| format!("`{name}` needs the standard library"))?;
        let cmd = reg
            .get(name)
            .ok_or_else(|| format!("the runtime does not provide `{name}`"))?;
        Ok(Expr::Call(Box::new(Call::Command {
            symbol: cmd.symbol.clone(),
            args: Vec::new(),
            arg_slots: Vec::new(),
            ret,
        })))
    }

    /// The type an already-lowered expression carries, for the few places that
    /// need it after the fact.
    fn expr_ty_of(&self, e: &Expr) -> TyId {
        match e {
            Expr::Local(l) => self.cx.b.m.func(self.fid).locals[l.0 as usize].ty,
            Expr::Global(g) => self.cx.b.m.global(*g).ty,
            Expr::Str(_) => TyTable::STR,
            Expr::Int(_, t) | Expr::Float(_, t) | Expr::Null(t) | Expr::Neg(_, t) => *t,
            Expr::Bool(_) => TyTable::BOOL,
            Expr::Cast { to, .. } => *to,
            Expr::Bin(_, _, _, t) => *t,
            Expr::MakeRecord(rid, _) => self
                .cx
                .b
                .m
                .types
                .find(&TyKind::Record(*rid))
                .unwrap_or(TyTable::PTR),
            _ => TyTable::PTR,
        }
    }

    /// Allocate `size` bytes: from the collector when it is linked, from libc
    /// otherwise. Everything K2 puts on the heap goes through here.
    fn alloc(&self, size: Expr, ret: TyId) -> Expr {
        match self.cx.runtime {
            Runtime::Kiln => Expr::Call(Box::new(Call::Dll {
                library: "runtime".into(),
                symbol: "kn_notify".into(),
                conv: CallConv::Cdecl,
                args: vec![
                    Expr::Int(1, TyTable::I32), // KN_NRS_MALLOC
                    Expr::Cast {
                        value: Box::new(size),
                        to: TyTable::PTR,
                    },
                    Expr::Null(TyTable::PTR),
                ],
                arg_tys: vec![TyTable::I32, TyTable::PTR, TyTable::PTR],
                ret,
                varargs: false,
            })),
            Runtime::Libc => Expr::Call(Box::new(Call::Dll {
                library: "c".into(),
                symbol: "malloc".into(),
                conv: CallConv::Cdecl,
                args: vec![size],
                arg_tys: vec![TyTable::NUINT],
                ret,
                varargs: false,
            })),
        }
    }

    /// Grow a block, preserving what is in it.
    fn realloc(&self, ptr: Expr, size: Expr, ret: TyId) -> Expr {
        match self.cx.runtime {
            Runtime::Kiln => Expr::Call(Box::new(Call::Dll {
                library: "runtime".into(),
                symbol: "kn_notify".into(),
                conv: CallConv::Cdecl,
                args: vec![
                    Expr::Int(3, TyTable::I32), // KN_NRS_MREALLOC
                    Expr::Cast {
                        value: Box::new(ptr),
                        to: TyTable::PTR,
                    },
                    Expr::Cast {
                        value: Box::new(size),
                        to: TyTable::PTR,
                    },
                ],
                arg_tys: vec![TyTable::I32, TyTable::PTR, TyTable::PTR],
                ret,
                varargs: false,
            })),
            Runtime::Libc => Expr::Call(Box::new(Call::Dll {
                library: "c".into(),
                symbol: "realloc".into(),
                conv: CallConv::Cdecl,
                args: vec![
                    Expr::Cast {
                        value: Box::new(ptr),
                        to: TyTable::PTR,
                    },
                    Expr::Cast {
                        value: Box::new(size),
                        to: TyTable::NUINT,
                    },
                ],
                arg_tys: vec![TyTable::PTR, TyTable::NUINT],
                ret,
                varargs: false,
            })),
        }
    }

    /// `memcpy(dst, src, n)`.
    fn memcpy(&self, dst: Expr, src: Expr, n: i64) -> Expr {
        Expr::Call(Box::new(Call::Dll {
            library: "c".into(),
            symbol: "memcpy".into(),
            conv: CallConv::Cdecl,
            args: vec![dst, src, Expr::Int(n as i128, TyTable::NUINT)],
            arg_tys: vec![TyTable::PTR, TyTable::PTR, TyTable::NUINT],
            ret: TyTable::PTR,
            varargs: false,
        }))
    }

    /// The address of byte `offset` in a buffer (offsets are 0-based).
    /// The address of byte `offset` (from 0) of a `Bytes`.
    ///
    /// A `Bytes` is the runtime's byte-set — `{ int32 dims; int32 len; data }`
    /// — so the data starts eight bytes in. Treating the pointer as the data,
    /// as this once did, read a command's bytes from their header and handed a
    /// command bytes it could not read.
    fn byte_ptr(&self, buf: Expr, offset: Expr) -> Expr {
        let u8arr = self
            .cx
            .b
            .m
            .types
            .find(&TyKind::Array(TyTable::U8))
            .expect("u8 buffer type");
        Expr::ElemPtr(
            Box::new(Expr::Cast {
                value: Box::new(buf),
                to: u8arr,
            }),
            Box::new(Expr::Bin(
                BinOp::Add,
                Box::new(offset),
                Box::new(Expr::Int(BIN_HEADER as i128, TyTable::I32)),
                TyTable::I32,
            )),
        )
    }

    /// A new `Bytes` of `n` zeroed bytes, with its header written.
    fn bytes_alloc(&mut self, n: Expr) -> Expr {
        let len = self.new_local("$binlen", TyTable::I32);
        self.push(Stmt::Let { local: len, value: n });
        let size = Expr::Cast {
            value: Box::new(Expr::Bin(
                BinOp::Add,
                Box::new(Expr::Local(len)),
                Box::new(Expr::Int(BIN_HEADER as i128, TyTable::I32)),
                TyTable::I32,
            )),
            to: TyTable::I64,
        };
        let buf = self.new_local("$bin", TyTable::BYTES);
        let allocated = self.alloc(size.clone(), TyTable::BYTES);
        self.push(Stmt::Let { local: buf, value: allocated });
        let i32arr = self.cx.b.m.types.intern(TyKind::Array(TyTable::I32));
        let header = || Expr::Cast {
            value: Box::new(Expr::Local(buf)),
            to: i32arr,
        };
        self.push(Stmt::Assign {
            place: Place::Index(Box::new(header()), Box::new(Expr::Int(0, TyTable::I32))),
            value: Expr::Int(1, TyTable::I32),
        });
        self.push(Stmt::Assign {
            place: Place::Index(Box::new(header()), Box::new(Expr::Int(1, TyTable::I32))),
            value: Expr::Local(len),
        });
        let data = self.byte_ptr(Expr::Local(buf), Expr::Int(0, TyTable::I32));
        self.push(Stmt::Expr(Expr::Call(Box::new(Call::Dll {
            library: "c".into(),
            symbol: "memset".into(),
            conv: CallConv::Cdecl,
            args: vec![
                data,
                Expr::Int(0, TyTable::I32),
                Expr::Cast {
                    value: Box::new(Expr::Local(len)),
                    to: TyTable::NUINT,
                },
            ],
            arg_tys: vec![TyTable::PTR, TyTable::I32, TyTable::NUINT],
            ret: TyTable::PTR,
            varargs: false,
        }))));
        Expr::Local(buf)
    }

    /// Are two keys equal? Strings compare by content through libc `strcmp`;
    /// everything else compares by value.
    fn key_equal(&mut self, a: Expr, b: Expr, kty: TyId) -> Expr {
        if kty == TyTable::STR {
            let cmp = Expr::Call(Box::new(Call::Dll {
                library: "c".into(),
                symbol: "strcmp".into(),
                conv: CallConv::Cdecl,
                args: vec![a, b],
                arg_tys: vec![TyTable::STR, TyTable::STR],
                ret: TyTable::I32,
                varargs: false,
            }));
            Expr::Bin(
                BinOp::Eq,
                Box::new(cmp),
                Box::new(Expr::Int(0, TyTable::I32)),
                TyTable::I32,
            )
        } else {
            Expr::Bin(BinOp::Eq, Box::new(a), Box::new(b), kty)
        }
    }

    /// A hash of `key`, as a local holding a u32.
    ///
    /// A string is hashed FNV-1a over its bytes; anything else is multiplied by
    /// Knuth's constant. Both are cheap and spread well enough for probing.
    fn hash_of(&mut self, key: LocalId, kty: TyId) -> LocalId {
        let h = self.new_local("$hash", TyTable::U32);
        if kty == TyTable::STR {
            self.push(Stmt::Let {
                local: h,
                value: Expr::Int(2_166_136_261, TyTable::U32),
            });
            let i = self.new_local("$hi", TyTable::I32);
            self.push(Stmt::Let {
                local: i,
                value: Expr::Int(0, TyTable::I32),
            });
            let u8arr = self.cx.b.m.types.intern(TyKind::Array(TyTable::U8));
            let byte = Expr::Index(
                Box::new(Expr::Cast {
                    value: Box::new(Expr::Local(key)),
                    to: u8arr,
                }),
                Box::new(Expr::Local(i)),
            );
            let b = self.new_local("$hb", TyTable::U8);
            let body = vec![
                Stmt::Let {
                    local: b,
                    value: byte,
                },
                Stmt::If {
                    cond: Expr::Bin(
                        BinOp::Eq,
                        Box::new(Expr::Local(b)),
                        Box::new(Expr::Int(0, TyTable::U8)),
                        TyTable::U8,
                    ),
                    then: vec![Stmt::Break],
                    els: vec![],
                },
                Stmt::Assign {
                    place: Place::Local(h),
                    value: Expr::Bin(
                        BinOp::Xor,
                        Box::new(Expr::Local(h)),
                        Box::new(Expr::Cast {
                            value: Box::new(Expr::Local(b)),
                            to: TyTable::U32,
                        }),
                        TyTable::U32,
                    ),
                },
                Stmt::Assign {
                    place: Place::Local(h),
                    value: Expr::Bin(
                        BinOp::Mul,
                        Box::new(Expr::Local(h)),
                        Box::new(Expr::Int(16_777_619, TyTable::U32)),
                        TyTable::U32,
                    ),
                },
                Stmt::Assign {
                    place: Place::Local(i),
                    value: Expr::Bin(
                        BinOp::Add,
                        Box::new(Expr::Local(i)),
                        Box::new(Expr::Int(1, TyTable::I32)),
                        TyTable::I32,
                    ),
                },
            ];
            self.push(Stmt::Loop { body });
        } else {
            self.push(Stmt::Let {
                local: h,
                value: Expr::Bin(
                    BinOp::Mul,
                    Box::new(Expr::Cast {
                        value: Box::new(Expr::Local(key)),
                        to: TyTable::U32,
                    }),
                    Box::new(Expr::Int(2_654_435_761u32 as i128, TyTable::U32)),
                    TyTable::U32,
                ),
            });
        }
        h
    }

    /// Find a value in a set, yielding a local holding its entry number or -1.
    /// Probing is the dictionary's, over the set's own index.
    fn set_find(&mut self, holder: LocalId, vh: LocalId, elem: TyId) -> Result<LocalId, String> {
        let found = self.new_local("$sfound", TyTable::I32);
        self.push(Stmt::Let {
            local: found,
            value: Expr::Int(-1, TyTable::I32),
        });
        let cap = || Expr::Field(Box::new(Expr::Local(holder)), LIST_CAP);
        let index = || Expr::Field(Box::new(Expr::Local(holder)), SET_INDEX);
        let h = self.hash_of(vh, elem);
        let slot = self.new_local("$sslot", TyTable::I32);
        let entry = self.new_local("$sentry", TyTable::I32);
        let mask = || {
            Expr::Bin(
                BinOp::Sub,
                Box::new(cap()),
                Box::new(Expr::Int(1, TyTable::I32)),
                TyTable::I32,
            )
        };
        let cur = Expr::Index(
            Box::new(Expr::Field(Box::new(Expr::Local(holder)), LIST_DATA)),
            Box::new(Expr::Bin(
                BinOp::Sub,
                Box::new(Expr::Local(entry)),
                Box::new(Expr::Int(1, TyTable::I32)),
                TyTable::I32,
            )),
        );
        let eq = self.key_equal(cur, Expr::Local(vh), elem);
        let probe = vec![
            Stmt::Let {
                local: slot,
                value: Expr::Bin(
                    BinOp::And,
                    Box::new(Expr::Cast {
                        value: Box::new(Expr::Local(h)),
                        to: TyTable::I32,
                    }),
                    Box::new(mask()),
                    TyTable::I32,
                ),
            },
            Stmt::Loop {
                body: vec![
                    Stmt::Let {
                        local: entry,
                        value: Expr::Index(Box::new(index()), Box::new(Expr::Local(slot))),
                    },
                    Stmt::If {
                        cond: Expr::Bin(
                            BinOp::Eq,
                            Box::new(Expr::Local(entry)),
                            Box::new(Expr::Int(0, TyTable::I32)),
                            TyTable::I32,
                        ),
                        then: vec![Stmt::Break],
                        els: vec![],
                    },
                    Stmt::If {
                        cond: eq,
                        then: vec![
                            Stmt::Assign {
                                place: Place::Local(found),
                                value: Expr::Bin(
                                    BinOp::Sub,
                                    Box::new(Expr::Local(entry)),
                                    Box::new(Expr::Int(1, TyTable::I32)),
                                    TyTable::I32,
                                ),
                            },
                            Stmt::Break,
                        ],
                        els: vec![],
                    },
                    Stmt::Assign {
                        place: Place::Local(slot),
                        value: Expr::Bin(
                            BinOp::And,
                            Box::new(Expr::Bin(
                                BinOp::Add,
                                Box::new(Expr::Local(slot)),
                                Box::new(Expr::Int(1, TyTable::I32)),
                                TyTable::I32,
                            )),
                            Box::new(mask()),
                            TyTable::I32,
                        ),
                    },
                ],
            },
        ];
        self.push(Stmt::If {
            cond: Expr::Bin(
                BinOp::Gt,
                Box::new(cap()),
                Box::new(Expr::Int(0, TyTable::I32)),
                TyTable::I32,
            ),
            then: probe,
            els: vec![],
        });
        Ok(found)
    }

    /// Append to a set: grow and reindex when half full, then store and index.
    fn set_append(&mut self, holder: LocalId, sty: TyId, vh: LocalId) {
        let (srid, elem) = self.cx.as_set(sty).expect("a set");
        let d = || Expr::Local(holder);
        let len = || Expr::Field(Box::new(d()), LIST_LEN);
        let cap = || Expr::Field(Box::new(d()), LIST_CAP);
        let index = || Expr::Field(Box::new(d()), SET_INDEX);
        let esize = self.cx.scalar_size(elem);
        let data_ty = self.cx.b.m.record(srid).fields[LIST_DATA].ty;
        let index_ty = self.cx.b.m.record(srid).fields[SET_INDEX].ty;
        let newcap = self.new_local("$scap", TyTable::I32);
        let mul = |a: Expr, b: i64| {
            Expr::Bin(
                BinOp::Mul,
                Box::new(a),
                Box::new(Expr::Int(b as i128, TyTable::I32)),
                TyTable::I32,
            )
        };
        let mut grow = vec![Stmt::If {
            cond: Expr::Bin(
                BinOp::Eq,
                Box::new(cap()),
                Box::new(Expr::Int(0, TyTable::I32)),
                TyTable::I32,
            ),
            then: vec![Stmt::Assign {
                place: Place::Local(newcap),
                value: Expr::Int(8, TyTable::I32),
            }],
            els: vec![Stmt::Assign {
                place: Place::Local(newcap),
                value: mul(cap(), 2),
            }],
        }];
        grow.push(Stmt::Assign {
            place: Place::Field(Box::new(d()), LIST_DATA),
            value: self.realloc(
                Expr::Field(Box::new(d()), LIST_DATA),
                mul(Expr::Local(newcap), esize),
                data_ty,
            ),
        });
        grow.push(Stmt::Assign {
            place: Place::Field(Box::new(d()), SET_INDEX),
            value: self.alloc(
                Expr::Cast {
                    value: Box::new(mul(Expr::Local(newcap), 4)),
                    to: TyTable::I64,
                },
                index_ty,
            ),
        });
        let z = self.new_local("$szi", TyTable::I32);
        grow.push(Stmt::Let {
            local: z,
            value: Expr::Int(0, TyTable::I32),
        });
        grow.push(Stmt::Loop {
            body: vec![
                Stmt::If {
                    cond: Expr::Not(Box::new(Expr::Bin(
                        BinOp::Lt,
                        Box::new(Expr::Local(z)),
                        Box::new(Expr::Local(newcap)),
                        TyTable::I32,
                    ))),
                    then: vec![Stmt::Break],
                    els: vec![],
                },
                Stmt::Assign {
                    place: Place::Index(Box::new(index()), Box::new(Expr::Local(z))),
                    value: Expr::Int(0, TyTable::I32),
                },
                Stmt::Assign {
                    place: Place::Local(z),
                    value: Expr::Bin(
                        BinOp::Add,
                        Box::new(Expr::Local(z)),
                        Box::new(Expr::Int(1, TyTable::I32)),
                        TyTable::I32,
                    ),
                },
            ],
        });
        grow.push(Stmt::Assign {
            place: Place::Field(Box::new(d()), LIST_CAP),
            value: Expr::Local(newcap),
        });
        // Reinsert every element into the fresh index.
        let e = self.new_local("$sre", TyTable::I32);
        grow.push(Stmt::Let {
            local: e,
            value: Expr::Int(0, TyTable::I32),
        });
        self.blocks.push(Vec::new());
        let ekey = self.new_local("$srekey", elem);
        self.push(Stmt::Let {
            local: ekey,
            value: Expr::Index(
                Box::new(Expr::Field(Box::new(d()), LIST_DATA)),
                Box::new(Expr::Local(e)),
            ),
        });
        self.index_insert_at(holder, ekey, elem, Expr::Local(e), LIST_CAP, SET_INDEX);
        let reinsert = self.blocks.pop().unwrap();
        let mut re_body = vec![Stmt::If {
            cond: Expr::Not(Box::new(Expr::Bin(
                BinOp::Lt,
                Box::new(Expr::Local(e)),
                Box::new(len()),
                TyTable::I32,
            ))),
            then: vec![Stmt::Break],
            els: vec![],
        }];
        re_body.extend(reinsert);
        re_body.push(Stmt::Assign {
            place: Place::Local(e),
            value: Expr::Bin(
                BinOp::Add,
                Box::new(Expr::Local(e)),
                Box::new(Expr::Int(1, TyTable::I32)),
                TyTable::I32,
            ),
        });
        grow.push(Stmt::Loop { body: re_body });

        self.push(Stmt::If {
            cond: Expr::Bin(
                BinOp::Ge,
                Box::new(mul(len(), 2)),
                Box::new(cap()),
                TyTable::I32,
            ),
            then: grow,
            els: vec![],
        });
        self.push(Stmt::Assign {
            place: Place::Index(
                Box::new(Expr::Field(Box::new(d()), LIST_DATA)),
                Box::new(len()),
            ),
            value: Expr::Local(vh),
        });
        self.index_insert_at(holder, vh, elem, len(), LIST_CAP, SET_INDEX);
        self.push(Stmt::Assign {
            place: Place::Field(Box::new(d()), LIST_LEN),
            value: Expr::Bin(
                BinOp::Add,
                Box::new(len()),
                Box::new(Expr::Int(1, TyTable::I32)),
                TyTable::I32,
            ),
        });
    }

    /// The 0-based position of `x` in a list, or -1.
    fn list_find(&mut self, holder: LocalId, elem: TyId, x: Expr) -> Result<LocalId, String> {
        let xh = self.new_local("$want", elem);
        self.push(Stmt::Let { local: xh, value: x });
        let at = self.new_local("$at", TyTable::I32);
        self.push(Stmt::Let {
            local: at,
            value: Expr::Int(-1, TyTable::I32),
        });
        let i = self.new_local("$scan", TyTable::I32);
        self.push(Stmt::Let {
            local: i,
            value: Expr::Int(0, TyTable::I32),
        });
        let len = Expr::Field(Box::new(Expr::Local(holder)), LIST_LEN);
        let item = Expr::Index(
            Box::new(Expr::Field(Box::new(Expr::Local(holder)), LIST_DATA)),
            Box::new(Expr::Local(i)),
        );
        let same = self.key_equal(item, Expr::Local(xh), elem);
        let body = vec![
            Stmt::If {
                cond: Expr::Not(Box::new(Expr::Bin(
                    BinOp::Lt,
                    Box::new(Expr::Local(i)),
                    Box::new(len),
                    TyTable::I32,
                ))),
                then: vec![Stmt::Break],
                els: vec![],
            },
            Stmt::If {
                cond: same,
                then: vec![
                    Stmt::Assign {
                        place: Place::Local(at),
                        value: Expr::Local(i),
                    },
                    Stmt::Break,
                ],
                els: vec![],
            },
            Stmt::Assign {
                place: Place::Local(i),
                value: Expr::Bin(
                    BinOp::Add,
                    Box::new(Expr::Local(i)),
                    Box::new(Expr::Int(1, TyTable::I32)),
                    TyTable::I32,
                ),
            },
        ];
        self.push(Stmt::Loop { body });
        Ok(at)
    }

    /// Remove the first `x` from a list, keeping the order of the rest.
    fn list_remove(&mut self, holder: LocalId, lty: TyId, x: Expr) -> Result<LocalId, String> {
        let (_, elem) = self.cx.as_list(lty).expect("a list");
        let at = self.list_find(holder, elem, x)?;
        let removed = self.new_local("$removed", TyTable::BOOL);
        let present = Expr::Bin(
            BinOp::Ge,
            Box::new(Expr::Local(at)),
            Box::new(Expr::Int(0, TyTable::I32)),
            TyTable::I32,
        );
        self.push(Stmt::Let {
            local: removed,
            value: present.clone(),
        });
        let data = || Expr::Field(Box::new(Expr::Local(holder)), LIST_DATA);
        let len = || Expr::Field(Box::new(Expr::Local(holder)), LIST_LEN);
        let plus1 = |e: Expr| {
            Expr::Bin(
                BinOp::Add,
                Box::new(e),
                Box::new(Expr::Int(1, TyTable::I32)),
                TyTable::I32,
            )
        };
        // Shift everything after it down by one.
        let shift = vec![
            Stmt::If {
                cond: Expr::Not(Box::new(Expr::Bin(
                    BinOp::Lt,
                    Box::new(plus1(Expr::Local(at))),
                    Box::new(len()),
                    TyTable::I32,
                ))),
                then: vec![Stmt::Break],
                els: vec![],
            },
            Stmt::Assign {
                place: Place::Index(Box::new(data()), Box::new(Expr::Local(at))),
                value: Expr::Index(Box::new(data()), Box::new(plus1(Expr::Local(at)))),
            },
            Stmt::Assign {
                place: Place::Local(at),
                value: plus1(Expr::Local(at)),
            },
        ];
        self.push(Stmt::If {
            cond: present,
            then: vec![
                Stmt::Loop { body: shift },
                Stmt::Assign {
                    place: Place::Field(Box::new(Expr::Local(holder)), LIST_LEN),
                    value: Expr::Bin(
                        BinOp::Sub,
                        Box::new(len()),
                        Box::new(Expr::Int(1, TyTable::I32)),
                        TyTable::I32,
                    ),
                },
            ],
            els: vec![],
        });
        Ok(removed)
    }

    /// `d[key] = value`: update the entry in place, or append it when the key
    /// is new. Shared by assignment and by `new Dictionary<K, V> { [k] = v }`.
    fn dict_set(
        &mut self,
        holder: LocalId,
        dty: TyId,
        key: &ast::Expr,
        value: &ast::Expr,
    ) -> Result<(), String> {
        let (_, kty, vty) = self.cx.as_dict(dty).expect("a dictionary");
        let (kv, _) = self.expr(key, Some(kty))?;
        let khold = self.new_local("$setkey", kty);
        self.push(Stmt::Let {
            local: khold,
            value: kv,
        });
        let (vv, _) = self.expr(value, Some(vty))?;
        let vhold = self.new_local("$setval", vty);
        self.push(Stmt::Let {
            local: vhold,
            value: vv,
        });
        let found = self.dict_find(holder, dty, Expr::Local(khold))?;
        self.blocks.push(Vec::new());
        self.dict_append(holder, dty, Expr::Local(khold), Expr::Local(vhold));
        let append = self.blocks.pop().unwrap();
        self.push(Stmt::If {
            cond: Expr::Bin(
                BinOp::Ge,
                Box::new(Expr::Local(found)),
                Box::new(Expr::Int(0, TyTable::I32)),
                TyTable::I32,
            ),
            then: vec![Stmt::Assign {
                place: Place::Index(
                    Box::new(Expr::Field(
                        Box::new(Expr::Local(holder)),
                        DICT_VALUES,
                    )),
                    Box::new(Expr::Local(found)),
                ),
                value: Expr::Local(vhold),
            }],
            els: append,
        });
        Ok(())
    }

    /// Remove `key` from a dictionary in place.
    ///
    /// The later entries shift down to close the gap, keeping insertion order,
    /// and the index is rebuilt from the entries that are left. Rebuilding rather than leaving a
    /// tombstone keeps the rule `dict_find` relies on: nothing in the index was
    /// ever removed, so a probe may stop at the first empty slot.
    fn dict_remove(&mut self, holder: LocalId, dty: TyId, key: Expr) -> Result<LocalId, String> {
        let (_, kty, _) = self.cx.as_dict(dty).expect("a dictionary");
        let found = self.dict_find(holder, dty, key)?;
        let removed = self.new_local("$removed", TyTable::BOOL);
        let present = Expr::Bin(
            BinOp::Ge,
            Box::new(Expr::Local(found)),
            Box::new(Expr::Int(0, TyTable::I32)),
            TyTable::I32,
        );
        self.push(Stmt::Let {
            local: removed,
            value: present.clone(),
        });
        let d = || Expr::Local(holder);
        let field = |f: usize| Expr::Field(Box::new(Expr::Local(holder)), f);
        let one = |e: Expr, op: BinOp| {
            Expr::Bin(op, Box::new(e), Box::new(Expr::Int(1, TyTable::I32)), TyTable::I32)
        };
        let _ = d;

        // Shift the later entries down one, so the dictionary keeps the order
        // its keys were added in — which is the order iterating it gives, and
        // what 1.x promised. Moving the last entry into the gap would be no
        // cheaper here, since the index is rebuilt either way.
        let mut then = Vec::new();
        let at = self.new_local("$at", TyTable::I32);
        then.push(Stmt::Assign {
            place: Place::Local(at),
            value: Expr::Local(found),
        });
        let mut shift = vec![Stmt::If {
            cond: Expr::Not(Box::new(Expr::Bin(
                BinOp::Lt,
                Box::new(one(Expr::Local(at), BinOp::Add)),
                Box::new(field(DICT_LEN)),
                TyTable::I32,
            ))),
            then: vec![Stmt::Break],
            els: vec![],
        }];
        for f in [DICT_KEYS, DICT_VALUES] {
            shift.push(Stmt::Assign {
                place: Place::Index(Box::new(field(f)), Box::new(Expr::Local(at))),
                value: Expr::Index(Box::new(field(f)), Box::new(one(Expr::Local(at), BinOp::Add))),
            });
        }
        shift.push(Stmt::Assign {
            place: Place::Local(at),
            value: one(Expr::Local(at), BinOp::Add),
        });
        then.push(Stmt::Loop { body: shift });
        then.push(Stmt::Assign {
            place: Place::Field(Box::new(Expr::Local(holder)), DICT_LEN),
            value: one(field(DICT_LEN), BinOp::Sub),
        });
        // Clear the index…
        let z = self.new_local("$z", TyTable::I32);
        then.push(Stmt::Assign {
            place: Place::Local(z),
            value: Expr::Int(0, TyTable::I32),
        });
        then.push(Stmt::Loop {
            body: vec![
                Stmt::If {
                    cond: Expr::Not(Box::new(Expr::Bin(
                        BinOp::Lt,
                        Box::new(Expr::Local(z)),
                        Box::new(field(DICT_CAP)),
                        TyTable::I32,
                    ))),
                    then: vec![Stmt::Break],
                    els: vec![],
                },
                Stmt::Assign {
                    place: Place::Index(Box::new(field(DICT_INDEX)), Box::new(Expr::Local(z))),
                    value: Expr::Int(0, TyTable::I32),
                },
                Stmt::Assign {
                    place: Place::Local(z),
                    value: one(Expr::Local(z), BinOp::Add),
                },
            ],
        });
        // …and put every remaining entry back into it.
        let e = self.new_local("$e", TyTable::I32);
        let kh = self.new_local("$rekey", kty);
        then.push(Stmt::Assign {
            place: Place::Local(e),
            value: Expr::Int(0, TyTable::I32),
        });
        self.blocks.push(Vec::new());
        self.index_insert_at(holder, kh, kty, Expr::Local(e), DICT_CAP, DICT_INDEX);
        let insert = self.blocks.pop().unwrap();
        let mut body = vec![
            Stmt::If {
                cond: Expr::Not(Box::new(Expr::Bin(
                    BinOp::Lt,
                    Box::new(Expr::Local(e)),
                    Box::new(field(DICT_LEN)),
                    TyTable::I32,
                ))),
                then: vec![Stmt::Break],
                els: vec![],
            },
            Stmt::Assign {
                place: Place::Local(kh),
                value: Expr::Index(Box::new(field(DICT_KEYS)), Box::new(Expr::Local(e))),
            },
        ];
        body.extend(insert);
        body.push(Stmt::Assign {
            place: Place::Local(e),
            value: one(Expr::Local(e), BinOp::Add),
        });
        then.push(Stmt::Loop { body });

        self.push(Stmt::If {
            cond: present,
            then,
            els: vec![],
        });
        Ok(removed)
    }

    /// Find `key` in a dictionary, yielding a local holding its entry number
    /// or -1.
    ///
    /// The index is open-addressed over a power-of-two capacity, holding entry
    /// numbers plus one so that zero means empty. Nothing is ever removed, so
    /// no tombstones are needed and a probe stops at the first empty slot.
    fn dict_find(&mut self, holder: LocalId, dty: TyId, key: Expr) -> Result<LocalId, String> {
        let (_, kty, _) = self.cx.as_dict(dty).expect("a dictionary");
        let k = self.new_local("$key", kty);
        self.push(Stmt::Let {
            local: k,
            value: key,
        });
        let found = self.new_local("$found", TyTable::I32);
        self.push(Stmt::Let {
            local: found,
            value: Expr::Int(-1, TyTable::I32),
        });
        // An empty dictionary has no index to probe.
        let cap = || Expr::Field(Box::new(Expr::Local(holder)), DICT_CAP);
        let h = self.hash_of(k, kty);
        let slot = self.new_local("$slot", TyTable::I32);
        let probe = {
            let mask = Expr::Bin(
                BinOp::Sub,
                Box::new(cap()),
                Box::new(Expr::Int(1, TyTable::I32)),
                TyTable::I32,
            );
            let start = Expr::Bin(
                BinOp::And,
                Box::new(Expr::Cast {
                    value: Box::new(Expr::Local(h)),
                    to: TyTable::I32,
                }),
                Box::new(mask),
                TyTable::I32,
            );
            let entry = self.new_local("$entry", TyTable::I32);
            let index = || Expr::Field(Box::new(Expr::Local(holder)), DICT_INDEX);
            let cur_key = Expr::Index(
                Box::new(Expr::Field(Box::new(Expr::Local(holder)), DICT_KEYS)),
                Box::new(Expr::Bin(
                    BinOp::Sub,
                    Box::new(Expr::Local(entry)),
                    Box::new(Expr::Int(1, TyTable::I32)),
                    TyTable::I32,
                )),
            );
            let eq = self.key_equal(cur_key, Expr::Local(k), kty);
            vec![
                Stmt::Let {
                    local: slot,
                    value: start,
                },
                Stmt::Loop {
                    body: vec![
                        Stmt::Let {
                            local: entry,
                            value: Expr::Index(Box::new(index()), Box::new(Expr::Local(slot))),
                        },
                        // An empty slot means the key is not here.
                        Stmt::If {
                            cond: Expr::Bin(
                                BinOp::Eq,
                                Box::new(Expr::Local(entry)),
                                Box::new(Expr::Int(0, TyTable::I32)),
                                TyTable::I32,
                            ),
                            then: vec![Stmt::Break],
                            els: vec![],
                        },
                        Stmt::If {
                            cond: eq,
                            then: vec![
                                Stmt::Assign {
                                    place: Place::Local(found),
                                    value: Expr::Bin(
                                        BinOp::Sub,
                                        Box::new(Expr::Local(entry)),
                                        Box::new(Expr::Int(1, TyTable::I32)),
                                        TyTable::I32,
                                    ),
                                },
                                Stmt::Break,
                            ],
                            els: vec![],
                        },
                        Stmt::Assign {
                            place: Place::Local(slot),
                            value: Expr::Bin(
                                BinOp::And,
                                Box::new(Expr::Bin(
                                    BinOp::Add,
                                    Box::new(Expr::Local(slot)),
                                    Box::new(Expr::Int(1, TyTable::I32)),
                                    TyTable::I32,
                                )),
                                Box::new(Expr::Bin(
                                    BinOp::Sub,
                                    Box::new(cap()),
                                    Box::new(Expr::Int(1, TyTable::I32)),
                                    TyTable::I32,
                                )),
                                TyTable::I32,
                            ),
                        },
                    ],
                },
            ]
        };
        self.push(Stmt::If {
            cond: Expr::Bin(
                BinOp::Gt,
                Box::new(cap()),
                Box::new(Expr::Int(0, TyTable::I32)),
                TyTable::I32,
            ),
            then: probe,
            els: vec![],
        });
        Ok(found)
    }

    /// Append a key and value, growing and reindexing when the table is full.
    ///
    /// Capacity is a power of two, so a probe can mask rather than divide. On
    /// growth every entry is reinserted into the fresh index — there is no
    /// removal, so that is the only time the index is rebuilt.
    fn dict_append(&mut self, holder: LocalId, dty: TyId, key: Expr, val: Expr) {
        let (drid, kty, vty) = self.cx.as_dict(dty).expect("a dictionary");
        let d = || Expr::Local(holder);
        let len = || Expr::Field(Box::new(d()), DICT_LEN);
        let cap = || Expr::Field(Box::new(d()), DICT_CAP);
        let index = || Expr::Field(Box::new(d()), DICT_INDEX);
        let ksize = self.cx.scalar_size(kty);
        let vsize = self.cx.scalar_size(vty);
        let keys_ty = self.cx.b.m.record(drid).fields[DICT_KEYS].ty;
        let vals_ty = self.cx.b.m.record(drid).fields[DICT_VALUES].ty;
        let index_ty = self.cx.b.m.record(drid).fields[DICT_INDEX].ty;
        let newcap = self.new_local("$dcap", TyTable::I32);

        // ── grow ────────────────────────────────────────────────────────────
        let mut grow = vec![Stmt::If {
            cond: Expr::Bin(
                BinOp::Eq,
                Box::new(cap()),
                Box::new(Expr::Int(0, TyTable::I32)),
                TyTable::I32,
            ),
            then: vec![Stmt::Assign {
                place: Place::Local(newcap),
                value: Expr::Int(8, TyTable::I32),
            }],
            els: vec![Stmt::Assign {
                place: Place::Local(newcap),
                value: Expr::Bin(
                    BinOp::Mul,
                    Box::new(cap()),
                    Box::new(Expr::Int(2, TyTable::I32)),
                    TyTable::I32,
                ),
            }],
        }];
        let mul = |a: Expr, b: i64| {
            Expr::Bin(
                BinOp::Mul,
                Box::new(a),
                Box::new(Expr::Int(b as i128, TyTable::I32)),
                TyTable::I32,
            )
        };
        grow.push(Stmt::Assign {
            place: Place::Field(Box::new(d()), DICT_KEYS),
            value: self.realloc(
                Expr::Field(Box::new(d()), DICT_KEYS),
                mul(Expr::Local(newcap), ksize),
                keys_ty,
            ),
        });
        grow.push(Stmt::Assign {
            place: Place::Field(Box::new(d()), DICT_VALUES),
            value: self.realloc(
                Expr::Field(Box::new(d()), DICT_VALUES),
                mul(Expr::Local(newcap), vsize),
                vals_ty,
            ),
        });
        // A fresh, zeroed index of the new capacity.
        grow.push(Stmt::Assign {
            place: Place::Field(Box::new(d()), DICT_INDEX),
            value: self.alloc(
                Expr::Cast {
                    value: Box::new(mul(Expr::Local(newcap), 4)),
                    to: TyTable::I64,
                },
                index_ty,
            ),
        });
        let z = self.new_local("$zi", TyTable::I32);
        grow.push(Stmt::Let {
            local: z,
            value: Expr::Int(0, TyTable::I32),
        });
        grow.push(Stmt::Loop {
            body: vec![
                Stmt::If {
                    cond: Expr::Not(Box::new(Expr::Bin(
                        BinOp::Lt,
                        Box::new(Expr::Local(z)),
                        Box::new(Expr::Local(newcap)),
                        TyTable::I32,
                    ))),
                    then: vec![Stmt::Break],
                    els: vec![],
                },
                Stmt::Assign {
                    place: Place::Index(Box::new(index()), Box::new(Expr::Local(z))),
                    value: Expr::Int(0, TyTable::I32),
                },
                Stmt::Assign {
                    place: Place::Local(z),
                    value: Expr::Bin(
                        BinOp::Add,
                        Box::new(Expr::Local(z)),
                        Box::new(Expr::Int(1, TyTable::I32)),
                        TyTable::I32,
                    ),
                },
            ],
        });
        grow.push(Stmt::Assign {
            place: Place::Field(Box::new(d()), DICT_CAP),
            value: Expr::Local(newcap),
        });
        // Reinsert every existing entry into the new index.
        let e = self.new_local("$re", TyTable::I32);
        grow.push(Stmt::Let {
            local: e,
            value: Expr::Int(0, TyTable::I32),
        });
        self.blocks.push(Vec::new());
        let ekey = self.new_local("$rekey", kty);
        self.push(Stmt::Let {
            local: ekey,
            value: Expr::Index(
                Box::new(Expr::Field(Box::new(d()), DICT_KEYS)),
                Box::new(Expr::Local(e)),
            ),
        });
        self.index_insert(holder, ekey, kty, Expr::Local(e));
        let reinsert = self.blocks.pop().unwrap();
        let mut re_body = vec![Stmt::If {
            cond: Expr::Not(Box::new(Expr::Bin(
                BinOp::Lt,
                Box::new(Expr::Local(e)),
                Box::new(len()),
                TyTable::I32,
            ))),
            then: vec![Stmt::Break],
            els: vec![],
        }];
        re_body.extend(reinsert);
        re_body.push(Stmt::Assign {
            place: Place::Local(e),
            value: Expr::Bin(
                BinOp::Add,
                Box::new(Expr::Local(e)),
                Box::new(Expr::Int(1, TyTable::I32)),
                TyTable::I32,
            ),
        });
        grow.push(Stmt::Loop { body: re_body });

        // Grow when the table is full, keeping the load factor at a half.
        self.push(Stmt::If {
            cond: Expr::Bin(
                BinOp::Ge,
                Box::new(Expr::Bin(
                    BinOp::Mul,
                    Box::new(len()),
                    Box::new(Expr::Int(2, TyTable::I32)),
                    TyTable::I32,
                )),
                Box::new(cap()),
                TyTable::I32,
            ),
            then: grow,
            els: vec![],
        });

        // ── append ──────────────────────────────────────────────────────────
        let kh = self.new_local("$akey", kty);
        self.push(Stmt::Let {
            local: kh,
            value: key,
        });
        self.push(Stmt::Assign {
            place: Place::Index(
                Box::new(Expr::Field(Box::new(d()), DICT_KEYS)),
                Box::new(len()),
            ),
            value: Expr::Local(kh),
        });
        self.push(Stmt::Assign {
            place: Place::Index(
                Box::new(Expr::Field(Box::new(d()), DICT_VALUES)),
                Box::new(len()),
            ),
            value: val,
        });
        self.index_insert(holder, kh, kty, len());
        self.push(Stmt::Assign {
            place: Place::Field(Box::new(d()), DICT_LEN),
            value: Expr::Bin(
                BinOp::Add,
                Box::new(len()),
                Box::new(Expr::Int(1, TyTable::I32)),
                TyTable::I32,
            ),
        });
    }

    /// Put entry number `entry` into the index under `key`'s hash, at the first
    /// empty slot from there.
    fn index_insert(&mut self, holder: LocalId, key: LocalId, kty: TyId, entry: Expr) {
        self.index_insert_at(holder, key, kty, entry, DICT_CAP, DICT_INDEX)
    }

    /// The same, for a record whose capacity and index sit at other fields — a
    /// set has the shape of a list plus an index.
    fn index_insert_at(
        &mut self,
        holder: LocalId,
        key: LocalId,
        kty: TyId,
        entry: Expr,
        cap_field: usize,
        index_field: usize,
    ) {
        let h = self.hash_of(key, kty);
        let cap = || Expr::Field(Box::new(Expr::Local(holder)), cap_field);
        let index = || Expr::Field(Box::new(Expr::Local(holder)), index_field);
        let mask = || {
            Expr::Bin(
                BinOp::Sub,
                Box::new(cap()),
                Box::new(Expr::Int(1, TyTable::I32)),
                TyTable::I32,
            )
        };
        let slot = self.new_local("$islot", TyTable::I32);
        self.push(Stmt::Let {
            local: slot,
            value: Expr::Bin(
                BinOp::And,
                Box::new(Expr::Cast {
                    value: Box::new(Expr::Local(h)),
                    to: TyTable::I32,
                }),
                Box::new(mask()),
                TyTable::I32,
            ),
        });
        self.push(Stmt::Loop {
            body: vec![
                Stmt::If {
                    cond: Expr::Bin(
                        BinOp::Eq,
                        Box::new(Expr::Index(Box::new(index()), Box::new(Expr::Local(slot)))),
                        Box::new(Expr::Int(0, TyTable::I32)),
                        TyTable::I32,
                    ),
                    then: vec![Stmt::Break],
                    els: vec![],
                },
                Stmt::Assign {
                    place: Place::Local(slot),
                    value: Expr::Bin(
                        BinOp::And,
                        Box::new(Expr::Bin(
                            BinOp::Add,
                            Box::new(Expr::Local(slot)),
                            Box::new(Expr::Int(1, TyTable::I32)),
                            TyTable::I32,
                        )),
                        Box::new(mask()),
                        TyTable::I32,
                    ),
                },
            ],
        });
        self.push(Stmt::Assign {
            place: Place::Index(Box::new(index()), Box::new(Expr::Local(slot))),
            value: Expr::Bin(
                BinOp::Add,
                Box::new(entry),
                Box::new(Expr::Int(1, TyTable::I32)),
                TyTable::I32,
            ),
        });
    }

    /// `Where` keeps the elements a predicate accepts; `Select` maps each one;
    /// `Any` asks whether one exists; `First` answers with the first one. The
    /// result type of a `Select` comes from probing the lambda body.
    fn list_query(
        &mut self,
        which: &str,
        src_val: Expr,
        src_ty: TyId,
        args: &[ast::Expr],
    ) -> Result<(Expr, TyId), String> {
        let (_, elem) = self.cx.as_list(src_ty).expect("a list");
        if args.len() != 1 {
            return Err(format!("List.{which} takes one argument"));
        }
        // `Any` and `First` answer with one value rather than a new list, so
        // they stop at the first element the predicate accepts.
        if which == "Any" || which == "First" {
            return self.list_find_by(which, src_val, src_ty, &args[0], elem);
        }
        // The lambda's type: Where is T -> bool; Select is T -> R, and R is
        // found by lowering the body into a scratch block that is discarded.
        let out_elem = if which == "Where" {
            TyTable::BOOL
        } else {
            self.probe_lambda_result(&args[0], elem)?
        };
        let fn_ty = self.cx.b.m.types.intern(TyKind::Func {
            params: vec![elem],
            ret: out_elem,
        });
        let (f, _) = self.expr(&args[0], Some(fn_ty))?;
        let fl = self.new_local("$fn", fn_ty);
        self.push(Stmt::Let {
            local: fl,
            value: f,
        });

        let src = self.new_local("$src", src_ty);
        self.push(Stmt::Let {
            local: src,
            value: src_val,
        });
        let result_elem = if which == "Where" { elem } else { out_elem };
        let out_rid = self.cx.list_record(result_elem);
        let out_ty = self.cx.b.m.types.intern(TyKind::Record(out_rid));
        let out_data_ty = self.cx.b.m.record(out_rid).fields[LIST_DATA].ty;
        let out = self.new_local("$out", out_ty);
        self.push(Stmt::Let {
            local: out,
            value: Expr::MakeRecord(
                out_rid,
                vec![
                    Expr::Int(0, TyTable::I32),
                    Expr::Int(0, TyTable::I32),
                    Expr::Null(out_data_ty),
                ],
            ),
        });

        let i = self.new_local("$qi", TyTable::I32);
        self.push(Stmt::Let {
            local: i,
            value: Expr::Int(0, TyTable::I32),
        });
        let item = self.new_local("$qitem", elem);
        let call = |item_local: LocalId| {
            Expr::Call(Box::new(Call::Indirect {
                callee: Box::new(Expr::Local(fl)),
                args: vec![Expr::Local(item_local)],
                sig: fn_ty,
            }))
        };
        let mut inner = vec![
            Stmt::If {
                cond: Expr::Not(Box::new(Expr::Bin(
                    BinOp::Lt,
                    Box::new(Expr::Local(i)),
                    Box::new(Expr::Field(Box::new(Expr::Local(src)), LIST_LEN)),
                    TyTable::I32,
                ))),
                then: vec![Stmt::Break],
                els: vec![],
            },
            Stmt::Assign {
                place: Place::Local(item),
                value: Expr::Index(
                    Box::new(Expr::Field(Box::new(Expr::Local(src)), LIST_DATA)),
                    Box::new(Expr::Local(i)),
                ),
            },
        ];
        // The append is built in a scratch block so it can be nested.
        self.blocks.push(Vec::new());
        if which == "Where" {
            self.list_add(out, out_ty, Expr::Local(item));
            let add = self.blocks.pop().unwrap();
            inner.push(Stmt::If {
                cond: call(item),
                then: add,
                els: vec![],
            });
        } else {
            let mapped = self.new_local("$mapped", out_elem);
            self.push(Stmt::Let {
                local: mapped,
                value: call(item),
            });
            self.list_add(out, out_ty, Expr::Local(mapped));
            let add = self.blocks.pop().unwrap();
            inner.extend(add);
        }
        inner.push(Stmt::Assign {
            place: Place::Local(i),
            value: Expr::Bin(
                BinOp::Add,
                Box::new(Expr::Local(i)),
                Box::new(Expr::Int(1, TyTable::I32)),
                TyTable::I32,
            ),
        });
        self.push(Stmt::Loop { body: inner });
        Ok((Expr::Local(out), out_ty))
    }

    /// `OrderBy(key)` — a new list holding the same elements, ordered ascending
    /// by the key the lambda selects. `Sort` orders a list by the element
    /// itself and does it in place; this reaches a field, leaves the source
    /// alone, and is stable, so elements with equal keys keep their order.
    ///
    /// The sort is the same insertion sort `Sort` is: a list a program orders is
    /// rarely large, and being stable is worth more than being quick. The key
    /// must be something the language can compare — a number, a bool, a char or
    /// a string — because there is no user comparator to call.
    fn list_order_by(
        &mut self,
        src_val: Expr,
        src_ty: TyId,
        args: &[ast::Expr],
    ) -> Result<(Expr, TyId), String> {
        let (_, elem) = self.cx.as_list(src_ty).expect("a list");
        if args.len() != 1 {
            return Err("List.OrderBy takes one key selector".into());
        }
        let kty = self.probe_lambda_result(&args[0], elem)?;
        let orderable = matches!(
            self.tt().kind(kty),
            TyKind::Bool
                | TyKind::I8
                | TyKind::U8
                | TyKind::I16
                | TyKind::U16
                | TyKind::I32
                | TyKind::U32
                | TyKind::I64
                | TyKind::U64
                | TyKind::Nint
                | TyKind::Nuint
                | TyKind::Char
                | TyKind::F32
                | TyKind::F64
                | TyKind::Str
        );
        if !orderable {
            return Err(format!(
                "`OrderBy` needs a key the language can order — a number, a bool, a char \
                 or a string — not {}",
                self.describe_ty(kty)
            ));
        }
        let fn_ty = self.cx.b.m.types.intern(TyKind::Func {
            params: vec![elem],
            ret: kty,
        });
        let (f, fty) = self.expr(&args[0], Some(fn_ty))?;
        self.check_fits(fty, fn_ty, "the `List.OrderBy` key")?;
        let fl = self.new_local("$fn", fn_ty);
        self.push(Stmt::Let {
            local: fl,
            value: f,
        });
        let src = self.new_local("$src", src_ty);
        self.push(Stmt::Let {
            local: src,
            value: src_val,
        });

        // A fresh list, filled with a copy of the source: the language's own
        // `Sort` is in place, and an ordering operation that quietly reordered
        // the caller's list would be a different thing wearing the name.
        let out_rid = self.cx.list_record(elem);
        let out_data_ty = self.cx.b.m.record(out_rid).fields[LIST_DATA].ty;
        let out = self.new_local("$out", src_ty);
        self.push(Stmt::Let {
            local: out,
            value: Expr::MakeRecord(
                out_rid,
                vec![
                    Expr::Int(0, TyTable::I32),
                    Expr::Int(0, TyTable::I32),
                    Expr::Null(out_data_ty),
                ],
            ),
        });
        let copy_i = self.new_local("$ci", TyTable::I32);
        self.push(Stmt::Let {
            local: copy_i,
            value: Expr::Int(0, TyTable::I32),
        });
        // The append is built in a scratch block so it nests inside the loop.
        self.blocks.push(Vec::new());
        self.list_add(
            out,
            src_ty,
            Expr::Index(
                Box::new(Expr::Field(Box::new(Expr::Local(src)), LIST_DATA)),
                Box::new(Expr::Local(copy_i)),
            ),
        );
        let append = self.blocks.pop().unwrap();
        let mut copy = vec![Stmt::If {
            cond: Expr::Not(Box::new(Expr::Bin(
                BinOp::Lt,
                Box::new(Expr::Local(copy_i)),
                Box::new(Expr::Field(Box::new(Expr::Local(src)), LIST_LEN)),
                TyTable::I32,
            ))),
            then: vec![Stmt::Break],
            els: vec![],
        }];
        copy.extend(append);
        copy.push(Stmt::Assign {
            place: Place::Local(copy_i),
            value: Expr::Bin(
                BinOp::Add,
                Box::new(Expr::Local(copy_i)),
                Box::new(Expr::Int(1, TyTable::I32)),
                TyTable::I32,
            ),
        });
        self.push(Stmt::Loop { body: copy });

        // Insertion sort by the selected key, exactly as `Sort` does it.
        let data = || Expr::Field(Box::new(Expr::Local(out)), LIST_DATA);
        let len = || Expr::Field(Box::new(Expr::Local(out)), LIST_LEN);
        let one = |e: Expr, op: BinOp| {
            Expr::Bin(
                op,
                Box::new(e),
                Box::new(Expr::Int(1, TyTable::I32)),
                TyTable::I32,
            )
        };
        let at = |loc: LocalId| Expr::Index(Box::new(data()), Box::new(Expr::Local(loc)));
        let call_on = |e: Expr| {
            Expr::Call(Box::new(Call::Indirect {
                callee: Box::new(Expr::Local(fl)),
                args: vec![e],
                sig: fn_ty,
            }))
        };
        let i = self.new_local("$oi", TyTable::I32);
        let j = self.new_local("$oj", TyTable::I32);
        let item = self.new_local("$oitem", elem);
        let key = self.new_local("$okey", kty);
        let here = self.new_local("$ohere", kty);
        // `here > key`, which for a string is `strcmp(here, key) > 0`.
        let greater = if kty == TyTable::STR {
            Expr::Bin(
                BinOp::Gt,
                Box::new(Expr::Call(Box::new(Call::Dll {
                    library: "c".into(),
                    symbol: "strcmp".into(),
                    conv: CallConv::Cdecl,
                    args: vec![Expr::Local(here), Expr::Local(key)],
                    arg_tys: vec![TyTable::STR, TyTable::STR],
                    ret: TyTable::I32,
                    varargs: false,
                }))),
                Box::new(Expr::Int(0, TyTable::I32)),
                TyTable::I32,
            )
        } else if kty == TyTable::BOOL {
            // `here && !key`: an `i1` compared signed reads `true` as -1, which
            // would put `true` before `false`.
            Expr::Bin(
                BinOp::And,
                Box::new(Expr::Local(here)),
                Box::new(Expr::Not(Box::new(Expr::Local(key)))),
                kty,
            )
        } else {
            Expr::Bin(
                BinOp::Gt,
                Box::new(Expr::Local(here)),
                Box::new(Expr::Local(key)),
                kty,
            )
        };
        self.push(Stmt::Let {
            local: i,
            value: Expr::Int(1, TyTable::I32),
        });
        let inner = vec![
            Stmt::If {
                cond: Expr::Bin(
                    BinOp::Lt,
                    Box::new(Expr::Local(j)),
                    Box::new(Expr::Int(0, TyTable::I32)),
                    TyTable::I32,
                ),
                then: vec![Stmt::Break],
                els: vec![],
            },
            Stmt::Assign {
                place: Place::Local(here),
                value: call_on(at(j)),
            },
            Stmt::If {
                cond: Expr::Not(Box::new(greater)),
                then: vec![Stmt::Break],
                els: vec![],
            },
            Stmt::Assign {
                place: Place::Index(Box::new(data()), Box::new(one(Expr::Local(j), BinOp::Add))),
                value: at(j),
            },
            Stmt::Assign {
                place: Place::Local(j),
                value: one(Expr::Local(j), BinOp::Sub),
            },
        ];
        let outer = vec![
            Stmt::If {
                cond: Expr::Not(Box::new(Expr::Bin(
                    BinOp::Lt,
                    Box::new(Expr::Local(i)),
                    Box::new(len()),
                    TyTable::I32,
                ))),
                then: vec![Stmt::Break],
                els: vec![],
            },
            Stmt::Assign {
                place: Place::Local(item),
                value: at(i),
            },
            Stmt::Assign {
                place: Place::Local(key),
                value: call_on(Expr::Local(item)),
            },
            Stmt::Assign {
                place: Place::Local(j),
                value: one(Expr::Local(i), BinOp::Sub),
            },
            Stmt::Loop { body: inner },
            Stmt::Assign {
                place: Place::Index(Box::new(data()), Box::new(one(Expr::Local(j), BinOp::Add))),
                value: Expr::Local(item),
            },
            Stmt::Assign {
                place: Place::Local(i),
                value: one(Expr::Local(i), BinOp::Add),
            },
        ];
        self.push(Stmt::Loop { body: outer });
        Ok((Expr::Local(out), src_ty))
    }

    /// `Any(pred)` — true when the predicate accepts some element; `First(pred)`
    /// — that element. Both stop at the first one that matches. `First` with no
    /// match has no value to answer with, so it stops the program by name
    /// rather than answering with a zero that would read like a result.
    fn list_find_by(
        &mut self,
        which: &str,
        src_val: Expr,
        src_ty: TyId,
        arg: &ast::Expr,
        elem: TyId,
    ) -> Result<(Expr, TyId), String> {
        let fn_ty = self.cx.b.m.types.intern(TyKind::Func {
            params: vec![elem],
            ret: TyTable::BOOL,
        });
        let (f, fty) = self.expr(arg, Some(fn_ty))?;
        self.check_fits(fty, fn_ty, &format!("the `List.{which}` predicate"))?;
        let fl = self.new_local("$fn", fn_ty);
        self.push(Stmt::Let {
            local: fl,
            value: f,
        });
        let src = self.new_local("$src", src_ty);
        self.push(Stmt::Let {
            local: src,
            value: src_val,
        });

        let out_ty = if which == "Any" { TyTable::BOOL } else { elem };
        let out = self.new_local("$qout", out_ty);
        let seed = if which == "Any" {
            Expr::Bool(false)
        } else {
            zero_of(self.tt(), elem)
        };
        self.push(Stmt::Let {
            local: out,
            value: seed,
        });
        let found = (which == "First").then(|| {
            let l = self.new_local("$qfound", TyTable::BOOL);
            self.push(Stmt::Let {
                local: l,
                value: Expr::Bool(false),
            });
            l
        });

        let i = self.new_local("$qi", TyTable::I32);
        self.push(Stmt::Let {
            local: i,
            value: Expr::Int(0, TyTable::I32),
        });
        let item = self.new_local("$qitem", elem);
        let call = Expr::Call(Box::new(Call::Indirect {
            callee: Box::new(Expr::Local(fl)),
            args: vec![Expr::Local(item)],
            sig: fn_ty,
        }));
        let mut hit = Vec::new();
        if which == "Any" {
            hit.push(Stmt::Assign {
                place: Place::Local(out),
                value: Expr::Bool(true),
            });
        } else {
            hit.push(Stmt::Assign {
                place: Place::Local(out),
                value: Expr::Local(item),
            });
            hit.push(Stmt::Assign {
                place: Place::Local(found.unwrap()),
                value: Expr::Bool(true),
            });
        }
        hit.push(Stmt::Break);
        let inner = vec![
            Stmt::If {
                cond: Expr::Not(Box::new(Expr::Bin(
                    BinOp::Lt,
                    Box::new(Expr::Local(i)),
                    Box::new(Expr::Field(Box::new(Expr::Local(src)), LIST_LEN)),
                    TyTable::I32,
                ))),
                then: vec![Stmt::Break],
                els: vec![],
            },
            Stmt::Assign {
                place: Place::Local(item),
                value: Expr::Index(
                    Box::new(Expr::Field(Box::new(Expr::Local(src)), LIST_DATA)),
                    Box::new(Expr::Local(i)),
                ),
            },
            Stmt::If {
                cond: call,
                then: hit,
                els: vec![],
            },
            Stmt::Assign {
                place: Place::Local(i),
                value: Expr::Bin(
                    BinOp::Add,
                    Box::new(Expr::Local(i)),
                    Box::new(Expr::Int(1, TyTable::I32)),
                    TyTable::I32,
                ),
            },
        ];
        self.push(Stmt::Loop { body: inner });

        if let Some(found) = found {
            let report = self.stop_message(vec![Expr::Str(
                "kiln: First found no element the predicate accepts\n".into(),
            )]);
            self.push(Stmt::If {
                cond: Expr::Not(Box::new(Expr::Local(found))),
                then: vec![Stmt::Expr(report), self.exit_one()],
                els: vec![],
            });
        }
        Ok((Expr::Local(out), out_ty))
    }

    /// Lower a lambda body once into a discarded block just to learn its result
    /// type, so `Select` knows the element type of the list it produces.
    fn probe_lambda_result(&mut self, e: &ast::Expr, param_ty: TyId) -> Result<TyId, String> {
        let ast::ExprKind::Lambda(l) = &e.kind else {
            return Err("List.Select needs a lambda".into());
        };
        let ast::LambdaBody::Expr(body) = &l.body else {
            return Err("List.Select needs an expression lambda".into());
        };
        let saved_scope = self.scope.clone();
        let name = l.params.first().map(|(n, _)| n.clone()).unwrap_or_default();
        let tmp = self.new_local("$probe", param_ty);
        self.scope.insert(name, (tmp, param_ty));
        self.blocks.push(Vec::new());
        let r = self.expr(body, None);
        self.blocks.pop();
        self.scope = saved_scope;
        Ok(r?.1)
    }

    /// A `switch` statement: the subject is held once and the sections become an
    /// if-chain inside a loop that runs once, so `break` leaves the switch.
    ///
    /// As in C#, control may not fall from one section into the next: a
    /// section with statements ends in `break`, `return` or `continue`.
    fn switch_stmt(
        &mut self,
        subject: &ast::Expr,
        sections: &[ast::SwitchSection],
    ) -> Result<(), String> {
        for (i, sec) in sections.iter().enumerate() {
            let ends = matches!(
                sec.body.last().map(|s| &s.kind),
                Some(ast::StmtKind::Break | ast::StmtKind::Return(_) | ast::StmtKind::Continue)
            );
            if !ends {
                let what = if sec.body.is_empty() && i + 1 == sections.len() {
                    "the last section of a `switch` has no statements"
                } else {
                    "control cannot fall through from one `case` to the next — end the section with `break`, `return` or `continue`"
                };
                return Err(what.into());
            }
        }
        if sections.iter().flat_map(|s| &s.labels).filter(|l| matches!(l, ast::SwitchPat::Discard)).count() > 1 {
            return Err("a `switch` has more than one `default`".into());
        }
        let (subj, sty) = self.expr(subject, None)?;
        let held = self.new_local("$switch", sty);
        self.push(Stmt::Let { local: held, value: subj });
        let flag = self.new_local("$switchcontinue", TyTable::BOOL);
        self.push(Stmt::Let { local: flag, value: Expr::Bool(false) });

        self.loop_frames.push(self.defers.len());
        self.switch_frames.push((self.loop_frames.len(), flag));
        let result = (|| -> Result<Vec<Stmt>, String> {
            // Conditions and bodies, in source order; `default` last.
            let mut arms: Vec<(Option<Vec<Stmt>>, Option<Expr>, Vec<Stmt>)> = Vec::new();
            let mut default_body: Option<Vec<Stmt>> = None;
            for sec in sections {
                let body = self.lower_body(&sec.body)?;
                if sec.labels.iter().any(|l| matches!(l, ast::SwitchPat::Discard)) {
                    default_body = Some(body);
                    continue;
                }
                self.blocks.push(Vec::new());
                let mut cond: Option<Expr> = None;
                let mut err = None;
                for l in &sec.labels {
                    let (c, op) = match l {
                        ast::SwitchPat::Const(c) => (c, BinOp::Eq),
                        ast::SwitchPat::Relational(op, c) => (
                            c,
                            match op {
                                ast::BinOp::Lt => BinOp::Lt,
                                ast::BinOp::Le => BinOp::Le,
                                ast::BinOp::Gt => BinOp::Gt,
                                _ => BinOp::Ge,
                            },
                        ),
                        ast::SwitchPat::Discard => unreachable!(),
                    };
                    match self.expr(c, Some(sty)) {
                        Ok((cv, cty)) => {
                            let test = if sty == TyTable::STR && cty == TyTable::STR {
                                let order = Expr::Call(Box::new(Call::Dll {
                                    library: "c".into(),
                                    symbol: "strcmp".into(),
                                    conv: CallConv::Cdecl,
                                    args: vec![Expr::Local(held), cv],
                                    arg_tys: vec![TyTable::STR, TyTable::STR],
                                    ret: TyTable::I32,
                                    varargs: false,
                                }));
                                Expr::Bin(op, Box::new(order), Box::new(Expr::Int(0, TyTable::I32)), TyTable::I32)
                            } else {
                                Expr::Bin(op, Box::new(Expr::Local(held)), Box::new(cv), sty)
                            };
                            cond = Some(match cond.take() {
                                None => test,
                                Some(prev) => Expr::Bin(BinOp::Or, Box::new(prev), Box::new(test), TyTable::BOOL),
                            });
                        }
                        Err(e) => {
                            err = Some(e);
                            break;
                        }
                    }
                }
                let pre = self.blocks.pop().unwrap();
                if let Some(e) = err {
                    return Err(e);
                }
                arms.push((Some(pre), cond, body));
            }
            // Build the chain from the back. A label's setup statements (a
            // constant that is a call, say) run before its test.
            let mut chain = default_body.unwrap_or_default();
            for (pre, cond, body) in arms.into_iter().rev() {
                let mut stmts = pre.unwrap_or_default();
                stmts.push(Stmt::If {
                    cond: cond.expect("a case has a label"),
                    then: body,
                    els: std::mem::take(&mut chain),
                });
                chain = stmts;
            }
            chain.push(Stmt::Break);
            Ok(chain)
        })();
        self.switch_frames.pop();
        self.loop_frames.pop();
        let body = result?;
        self.push(Stmt::Loop { body });
        // A `continue` inside the switch continues the enclosing loop now.
        self.blocks.push(Vec::new());
        let cont = self.stmt(&ast::Stmt {
            kind: ast::StmtKind::Continue,
            leading: Vec::new(),
            span: subject.span,
        });
        let then = self.blocks.pop().unwrap();
        if cont.is_ok() && !self.loop_frames.is_empty() {
            self.push(Stmt::If { cond: Expr::Local(flag), then, els: vec![] });
        }
        Ok(())
    }

    /// A `switch` expression: the subject is held once, then the arms become an
    /// if-chain assigning into one temporary.
    fn switch(
        &mut self,
        subject: &ast::Expr,
        arms: &[ast::SwitchArm],
        hint: Option<TyId>,
    ) -> Result<(Expr, TyId), String> {
        if arms.is_empty() {
            return Err("a `switch` expression needs at least one arm".into());
        }
        let (subj, sty) = self.expr(subject, None)?;
        let held = self.new_local("$switch", sty);
        self.push(Stmt::Let {
            local: held,
            value: subj,
        });
        // The result type comes from the first arm (or the caller's hint).
        let (first_val, rty) = {
            let saved = self.blocks.len();
            self.blocks.push(Vec::new());
            let r = self.expr(&arms[0].value, hint);
            self.blocks.truncate(saved);
            r?
        };
        let _ = first_val;
        let out = self.new_local("$case", rty);

        // Build the chain from the last arm backwards.
        let mut chain: Vec<Stmt> = Vec::new();
        let mut have_default = false;
        for arm in arms.iter().rev() {
            self.blocks.push(Vec::new());
            let val = self.expr(&arm.value, Some(rty));
            let mut body = self.blocks.pop().unwrap();
            let val = val?.0;
            body.push(Stmt::Assign {
                place: Place::Local(out),
                value: val,
            });
            match &arm.pat {
                ast::SwitchPat::Discard => {
                    have_default = true;
                    chain = body;
                }
                ast::SwitchPat::Const(c) => {
                    self.blocks.push(Vec::new());
                    let cv = self.expr(c, Some(sty));
                    let pre = self.blocks.pop().unwrap();
                    let cv = cv?.0;
                    let mut stmts = pre;
                    stmts.push(Stmt::If {
                        cond: Expr::Bin(BinOp::Eq, Box::new(Expr::Local(held)), Box::new(cv), sty),
                        then: body,
                        els: std::mem::take(&mut chain),
                    });
                    chain = stmts;
                }
                ast::SwitchPat::Relational(op, c) => {
                    self.blocks.push(Vec::new());
                    let cv = self.expr(c, Some(sty));
                    let pre = self.blocks.pop().unwrap();
                    let cv = cv?.0;
                    let kop = match op {
                        ast::BinOp::Lt => BinOp::Lt,
                        ast::BinOp::Le => BinOp::Le,
                        ast::BinOp::Gt => BinOp::Gt,
                        ast::BinOp::Ge => BinOp::Ge,
                        _ => return Err("unsupported relational pattern".into()),
                    };
                    let mut stmts = pre;
                    stmts.push(Stmt::If {
                        cond: Expr::Bin(kop, Box::new(Expr::Local(held)), Box::new(cv), sty),
                        then: body,
                        els: std::mem::take(&mut chain),
                    });
                    chain = stmts;
                }
            }
        }
        if !have_default {
            return Err("a `switch` expression needs a `_` arm".into());
        }
        for st in chain {
            self.push(st);
        }
        Ok((Expr::Local(out), rty))
    }

    /// Lift a lambda to its own function and build a closure value.
    ///
    /// The lifted function takes the environment pointer as parameter 0, which
    /// is the KIR closure convention; a non-capturing lambda passes `null` for
    /// it. Capturing lambdas need their captured locals hoisted into an env
    /// record at the point of declaration, which is a later step — until then a
    /// capture is reported rather than silently mis-compiled.
    fn lambda(&mut self, l: &ast::Lambda, want: TyId) -> Result<(Expr, TyId), String> {
        let (want_params, want_ret) = match self.tt().kind(want) {
            TyKind::Func { params, ret } => (params.clone(), *ret),
            _ => {
                return Err("a lambda's target type must be a `Func<...>` or `Action<...>`".into())
            }
        };
        if want_params.len() != l.params.len() {
            return Err(format!(
                "lambda takes {} parameter(s) but its target type expects {}",
                l.params.len(),
                want_params.len()
            ));
        }
        // Which enclosing variables does this lambda close over? Captured
        // locals already live in cells; a plain local here means a form of
        // capture the cell pass does not cover (a loop variable).
        let mut free = Vec::new();
        collect_idents_lambda(l, &mut free);
        let bound: Vec<&str> = l.params.iter().map(|(n, _)| n.as_str()).collect();
        let mut captures: Vec<(String, RecordId, TyId)> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        for name in &free {
            if bound.contains(&name.as_str()) || seen.contains(name) {
                continue;
            }
            if let Some(c) = self.cells.get(name).cloned() {
                seen.push(name.clone());
                captures.push((name.clone(), c.rec, c.ty));
            } else if self.scope.contains_key(name) {
                // A captured name should have been put in a cell when it was
                // declared. Reaching here means the pass that decides that
                // missed this binding form — a miscompile if it were allowed
                // through, since the closure would read a copy.
                return Err(format!(
                    "this lambda captures `{name}`, which is bound in a form the \
                     compiler cannot yet close over"
                ));
            }
        }

        // Declare the lifted function: env pointer first, then the parameters.
        let n = self.cx.b.m.funcs.len();
        let sym = format!("{}$lambda{n}", self.cx.b.m.name.replace('.', "_"));
        let mut params: Vec<(&str, TyId)> = vec![("$env", TyTable::PTR)];
        for ((name, _), ty) in l.params.iter().zip(want_params.iter()) {
            params.push((name.as_str(), *ty));
        }
        let fid = self.cx.b.declare_func(&sym, params, want_ret);

        // Queue the body; `this` binds parameter 0, the (unused) env pointer.
        let method = ast::Method {
            leading: Vec::new(),
            attrs: Vec::new(),
            is_extern: false,
            vis: ast::Vis::Private,
            is_static: true,
            name: sym.clone(),
            type_params: Vec::new(),
            constraints: Vec::new(),
            params: l
                .params
                .iter()
                .map(|(nm, t)| ast::Param {
                    name: nm.clone(),
                    ty: t.clone().unwrap_or(ast::TypeRef::Void),
                    default: None,
                    span: Default::default(),
                })
                .collect(),
            ret: ast::TypeRef::Void,
            body: match &l.body {
                ast::LambdaBody::Block(b) => b.clone(),
                ast::LambdaBody::Expr(_) => Vec::new(),
            },
            expr_body: match &l.body {
                ast::LambdaBody::Expr(e) => Some((**e).clone()),
                ast::LambdaBody::Block(_) => None,
            },
            doc: None,
            span: Default::default(),
        };
        // The environment: a record of pointers, one per captured cell. Both
        // sides reach the same cells, so a capture is by reference.
        let (env_expr, env_plan) = if captures.is_empty() {
            (Expr::Null(TyTable::PTR), None)
        } else {
            let rn = self.cx.b.m.records.len();
            let fields: Vec<(&str, TyId)> =
                captures.iter().map(|_| ("cell", TyTable::PTR)).collect();
            let env_rec = self
                .cx
                .b
                .c_record(&format!("$env{rn}"), fields, Equality::ByRef);
            let mut vals = Vec::new();
            for (name, _, _) in &captures {
                let c = self.cells[name].clone();
                vals.push(Expr::Cast {
                    value: Box::new(c.ptr),
                    to: TyTable::PTR,
                });
            }
            (
                Expr::MakeRecord(env_rec, vals),
                Some(EnvPlan {
                    rec: env_rec,
                    captures: captures.clone(),
                }),
            )
        };

        self.cx.pending.push(Pending {
            fid,
            method,
            tvars: self.cx.tvars.clone(),
            this: true,
            env: env_plan,
            ctor: None,
        });

        Ok((
            Expr::MakeClosure {
                func: fid,
                env: Box::new(env_expr),
            },
            want,
        ))
    }

    /// Create a component with no rectangle, set its properties and bind its
    /// events, through its own library's entry points. `owner` is the form or
    /// class whose methods its handlers name.
    fn build_nonvisual(&mut self, c: &ast::ComponentDecl, owner: Option<&str>) -> Result<(), String> {
        let lib = self.cx.nonvisual[&c.id].clone();
        let (g, _) = self.cx.components[&c.id];
        self.push(Stmt::Assign {
            place: Place::Global(g),
            value: component_call(
                &lib,
                "create",
                vec![Expr::Str(snake_case(&c.type_name))],
                vec![TyTable::STR],
                TyTable::I64,
            ),
        });
        for (n, v) in &c.properties {
            let text = literal_text(v)?;
            self.push(Stmt::Expr(component_call(
                &lib,
                "set",
                vec![Expr::Global(g), Expr::Str(snake_case(n)), Expr::Str(text)],
                vec![TyTable::I64, TyTable::STR, TyTable::STR],
                TyTable::I32,
            )));
        }
        for (event, handler) in &c.handlers {
            let fid = match handler {
                ast::HandlerRef::Method(nm) => {
                    let qualified = owner.map(|o| format!("{o}.{nm}"));
                    qualified
                        .as_ref()
                        .and_then(|q| self.cx.methods.get(q))
                        .or_else(|| self.cx.methods.get(nm))
                        .ok_or_else(|| format!("`{nm}` is not a method this program declares"))?
                        .fid
                }
                ast::HandlerRef::Lambda(_) => {
                    return Err(format!(
                        "`{}.{event}` takes a method by name; a lambda here is not supported yet",
                        c.id
                    ))
                }
            };
            // The library calls the handler with the event's arguments, so a
            // method declaring those parameters binds directly — the same
            // signature on both sides.
            self.push(Stmt::Expr(component_call(
                &lib,
                "on",
                vec![Expr::Global(g), Expr::Str(snake_case(event)), Expr::FuncPtr(fid)],
                vec![TyTable::I64, TyTable::STR, TyTable::PTR],
                TyTable::I32,
            )));
        }
        Ok(())
    }

    /// A call's arguments, lowered against the parameters: a missing trailing
    /// argument takes its default, and any other mismatch is an error naming
    /// the method. Pairing them with `zip` silently dropped what did not line
    /// up, so a call short an argument read garbage where it should have been.
    fn call_args(&mut self, name: &str, args: &[ast::Expr], sig: &Sig) -> Result<Vec<Expr>, String> {
        let want = sig.params.len();
        let required = sig
            .defaults
            .iter()
            .rposition(|d| d.is_none())
            .map(|i| i + 1)
            .unwrap_or(0)
            .min(want);
        if args.len() > want || args.len() < required {
            let expected = if required == want {
                format!("{want}")
            } else {
                format!("{required} to {want}")
            };
            return Err(format!(
                "`{name}` takes {expected} argument(s), but this call passes {}",
                args.len()
            ));
        }
        let mut out = Vec::new();
        for (i, pty) in sig.params.iter().enumerate() {
            let v = match args.get(i) {
                Some(a) => {
                    let (v, vty) = self.expr(a, Some(*pty))?;
                    self.check_fits(vty, *pty, &format!("argument {} of `{name}`", i + 1))?;
                    // A `T?` handed where a `T` is wanted is its value — and a
                    // stop, not a null the callee trips over, when it has none.
                    match *self.tt().kind(vty) {
                        TyKind::Optional(inner) if inner == *pty => {
                            self.present_or_stop(v, vty, &format!("argument {} of {name}", i + 1))?
                        }
                        _ => v,
                    }
                }
                None => {
                    let d = sig.defaults.get(i).cloned().flatten().expect("checked above");
                    self.expr(&d, Some(*pty))?.0
                }
            };
            out.push(v);
        }
        Ok(out)
    }

    /// Does a value of type `from` fit where `to` is wanted?
    ///
    /// Deliberately a check of *kind*, not of every width: a number fits a
    /// number, a `T` fits a `T?`, `null` fits anything that can be absent.
    /// What it refuses is a value of another kind entirely — text where a
    /// dictionary belongs, a record where a number does — which used to reach
    /// the emitter and either panicked it or produced LLVM clang rejected.
    fn fits(&self, from: TyId, to: TyId) -> bool {
        if from == to {
            return true;
        }
        let numeric = |t: TyId| {
            matches!(
                self.tt().kind(t),
                TyKind::I8 | TyKind::I16 | TyKind::I32 | TyKind::I64
                    | TyKind::U8 | TyKind::U16 | TyKind::U32 | TyKind::U64
                    | TyKind::Nint | TyKind::Nuint | TyKind::F32 | TyKind::F64
                    | TyKind::Char | TyKind::Bool
            )
        };
        if numeric(from) && numeric(to) {
            return true;
        }
        match (self.tt().kind(from), self.tt().kind(to)) {
            (_, TyKind::Optional(inner)) => *inner == from || self.fits(from, *inner),
            (TyKind::Optional(inner), _) => self.fits(*inner, to),
            // `null`, and the untyped pointer an extern answers with.
            // An untyped pointer — `null`, or what an extern answers with — may
            // become a typed one. The other way round is not a conversion: text
            // where a pointer is wanted is usually a mistake, and a record or a
            // buffer has its own way across (`Bytes`, `[CLayout]`).
            (TyKind::Ptr, TyKind::Str | TyKind::Record(_) | TyKind::Bytes | TyKind::Array(_)) => true,
            (TyKind::Record(_) | TyKind::Bytes, TyKind::Ptr) => true,
            // A value becomes a `Result<T>` by being its success.
            (_, TyKind::Record(rid)) if self.cx.b.m.record(*rid).name.starts_with("$Result") => true,
            // An interface value is built from any implementation.
            (TyKind::Record(_), TyKind::Record(rid))
                if self.cx.iface_records.values().any(|r| r == rid) =>
            {
                true
            }
            (TyKind::Func { .. }, TyKind::Func { .. }) => true,
            // A function address is an address: it goes where a `Ptr` does.
            (TyKind::CFunc { .. }, TyKind::Ptr) => true,
            // Storage held in place converts to the address of its first byte.
            (TyKind::Inline { .. }, TyKind::Ptr) => true,
            _ => false,
        }
    }

    /// `fits`, as an error naming what was wanted and what was given.
    /// Check a value against the type a place wants, and settle a `T?` into a
    /// `T` the way an argument is: its value, or a stop naming `what` when it
    /// has none. Every place a value is stored — a local, an assignment, a
    /// `return`, a field — follows the one rule; before, only arguments did, and
    /// the others stored the `{value, present}` pair into a plain slot, which
    /// clang refused.
    fn settle(&mut self, v: Expr, vty: TyId, want: TyId, what: &str) -> Result<(Expr, TyId), String> {
        self.check_fits(vty, want, what)?;
        let want_optional = matches!(self.tt().kind(want), TyKind::Optional(_));
        match *self.tt().kind(vty) {
            TyKind::Optional(inner) if !want_optional => {
                let v = self.present_or_stop(v, vty, what)?;
                Ok((v, inner))
            }
            _ => Ok((v, vty)),
        }
    }

    fn check_fits(&self, from: TyId, to: TyId, what: &str) -> Result<(), String> {
        if self.fits(from, to) {
            return Ok(());
        }
        Err(format!(
            "{what} wants {}, but this is {}",
            self.describe_ty(to),
            self.describe_ty(from)
        ))
    }

    /// The value inside a `T?`, or a stop with a message naming what was being
    /// reached through it — the same shape an out-of-range index takes.
    fn present_or_stop(&mut self, v: Expr, oty: TyId, member: &str) -> Result<Expr, String> {
        let held = self.new_local("$present", oty);
        self.push(Stmt::Let { local: held, value: v });
        let report = self.stop_message(vec![Expr::Str(format!("kiln: `.{member}` was read through a value that is not there\n"))]);
        let stop = Expr::Call(Box::new(Call::Dll {
            library: "c".into(),
            symbol: "exit".into(),
            conv: CallConv::Cdecl,
            args: vec![Expr::Int(1, TyTable::I32)],
            arg_tys: vec![TyTable::I32],
            ret: TyTable::VOID,
            varargs: false,
        }));
        self.push(Stmt::If {
            cond: Expr::Not(Box::new(Expr::OptionalHasValue(Box::new(Expr::Local(held))))),
            then: vec![Stmt::Expr(report), Stmt::Expr(stop)],
            els: vec![],
        });
        Ok(Expr::OptionalGet(Box::new(Expr::Local(held))))
    }

    /// `x!` — assert that an optional holds a value. The result is the value
    /// without its `T?`; one that is not there stops the program with a
    /// message rather than being read as though it were. A value that is
    /// already a `T` comes back unchanged, as C#'s `!` is a no-op there.
    fn null_forgiving(&mut self, inner: &ast::Expr) -> Result<(Expr, TyId), String> {
        let (v, ty) = self.expr(inner, None)?;
        if let TyKind::Optional(val_ty) = *self.tt().kind(ty) {
            let held = self.new_local("$assert", ty);
            self.push(Stmt::Let {
                local: held,
                value: v,
            });
            let report = self.stop_message(vec![Expr::Str(
                "kiln: `!` was used on a value that is not there\n".into(),
            )]);
            self.push(Stmt::If {
                cond: Expr::Not(Box::new(Expr::OptionalHasValue(Box::new(Expr::Local(held))))),
                then: vec![Stmt::Expr(report), self.exit_one()],
                els: vec![],
            });
            return Ok((Expr::OptionalGet(Box::new(Expr::Local(held))), val_ty));
        }
        if self.cx.as_result(ty).is_some() {
            return Err("`!` applies to a `T?`; a `Result` is unwrapped with `??` or `?`".into());
        }
        Ok((v, ty))
    }

    /// `x?.M`, `x?[i]`, `x?.M(…)` — the receiver is evaluated once and the
    /// accesses run only when it holds a value. The result is the access's
    /// type made optional, so a missing receiver yields no value rather than a
    /// read through nothing.
    fn null_conditional(
        &mut self,
        recv: &ast::Expr,
        steps: &[ast::NullStep],
    ) -> Result<(Expr, TyId), String> {
        let (rv, rty) = self.expr(recv, None)?;
        let TyKind::Optional(inner) = *self.tt().kind(rty) else {
            return Err(format!(
                "`?.` reads through a value that may be null, but this is {} — write `.` \
                 instead, or make it a `T?`",
                self.describe_ty(rty)
            ));
        };
        if steps.is_empty() {
            return Err("`?.` needs a member or an index after it".into());
        }
        let held = self.new_local("$nc", rty);
        self.push(Stmt::Let {
            local: held,
            value: rv,
        });
        let val = self.new_local("$ncval", inner);
        // The access is lowered exactly as the written form would be, against a
        // hidden name bound to the value the optional holds: the same member,
        // call and index rules, inside one presence guard.
        let hidden = format!("$nc{}", val.0);
        let mut access = ast::Expr {
            kind: ast::ExprKind::Ident(hidden.clone()),
            span: recv.span,
        };
        for step in steps {
            access = match step {
                ast::NullStep::Member(name) => ast::Expr {
                    kind: ast::ExprKind::Member(Box::new(access), name.clone()),
                    span: recv.span,
                },
                ast::NullStep::Call(args) => ast::Expr {
                    kind: ast::ExprKind::Call(Box::new(access), args.clone()),
                    span: recv.span,
                },
                ast::NullStep::Index(idx) => ast::Expr {
                    kind: ast::ExprKind::Index(Box::new(access), idx.clone()),
                    span: recv.span,
                },
            };
        }
        self.scope.insert(hidden.clone(), (val, inner));
        self.blocks.push(Vec::new());
        self.push(Stmt::Let {
            local: val,
            value: Expr::OptionalGet(Box::new(Expr::Local(held))),
        });
        let lowered = self.expr(&access, None);
        self.scope.remove(&hidden);
        let (av, aty) = lowered?;
        // An access that is already a `T?` stays one, as in C#: `a?.Name` where
        // `Name` is a `string?` is a `string?`, not an optional of an optional.
        let (out_ty, val_ty, wrapped) = match *self.tt().kind(aty) {
            TyKind::Optional(i) => (aty, i, av),
            _ => (
                self.cx.b.m.types.intern(TyKind::Optional(aty)),
                aty,
                Expr::MakeOptional(aty, Some(Box::new(av))),
            ),
        };
        let out = self.new_local("$ncopt", out_ty);
        let mut then_body = self.blocks.pop().unwrap();
        then_body.push(Stmt::Assign {
            place: Place::Local(out),
            value: wrapped,
        });
        self.push(Stmt::If {
            cond: Expr::OptionalHasValue(Box::new(Expr::Local(held))),
            then: then_body,
            els: vec![Stmt::Assign {
                place: Place::Local(out),
                value: Expr::MakeOptional(val_ty, None),
            }],
        });
        Ok((Expr::Local(out), out_ty))
    }

    /// The statement that ends a program with a message already printed.
    fn exit_one(&self) -> Stmt {
        Stmt::Expr(Expr::Call(Box::new(Call::Dll {
            library: "c".into(),
            symbol: "exit".into(),
            conv: CallConv::Cdecl,
            args: vec![Expr::Int(1, TyTable::I32)],
            arg_tys: vec![TyTable::I32],
            ret: TyTable::VOID,
            varargs: false,
        })))
    }

    /// A condition: something that is true or false, and nothing else.
    ///
    /// A string or a number used as one was accepted and emitted as a branch on
    /// a pointer, which is invalid LLVM — the build failed in clang, far from
    /// the line that caused it, with nothing to say what was wrong.
    fn condition(&mut self, e: &ast::Expr) -> Result<Expr, String> {
        let (c, ty) = self.expr(e, Some(TyTable::BOOL))?;
        if ty != TyTable::BOOL {
            return Err(format!(
                "a condition must be true or false, but this is {}",
                self.describe_ty(ty)
            ));
        }
        Ok(c)
    }

    /// A type as a reader would name it, for an error.
    fn describe_ty(&self, ty: TyId) -> String {
        match self.tt().kind(ty) {
            TyKind::Str => "text".into(),
            TyKind::Bool => "true or false".into(),
            TyKind::F32 | TyKind::F64 => "a decimal number".into(),
            TyKind::Void => "nothing".into(),
            TyKind::Record(_) if self.cx.as_list(ty).is_some() => {
                let (_, e) = self.cx.as_list(ty).unwrap();
                format!("a list of {}", self.describe_ty(e))
            }
            TyKind::Record(_) if self.cx.as_dict(ty).is_some() => "a dictionary".into(),
            TyKind::Record(_) if self.cx.as_set(ty).is_some() => "a set".into(),
            TyKind::Record(_) if self.cx.as_result(ty).is_some() => "a Result".into(),
            TyKind::Record(rid) => format!("a `{}`", self.cx.b.m.record(*rid).name),
            TyKind::Optional(_) => "an optional value — test it with `!= null`".into(),
            TyKind::Func { .. } => "a function".into(),
            TyKind::CFunc { .. } => "a C function address".into(),
            k if matches!(k, TyKind::I8 | TyKind::I16 | TyKind::I32 | TyKind::I64 | TyKind::U8 | TyKind::U16 | TyKind::U32 | TyKind::U64 | TyKind::Nint | TyKind::Nuint) => "a whole number".into(),
            _ => "a value".into(),
        }
    }

    /// `button.Click += handler` / `-= handler` in code: bind or unbind an
    /// event at run time (ABI v5).
    ///
    /// The handler becomes a (function, environment) pair. A lambda's lifted
    /// function already takes its environment as its first parameter, which is
    /// exactly the shape the UI library calls; the library holds the
    /// environment for as long as the handler is bound, so a click can never
    /// reach a captured variable the collector has freed.
    fn wire_event(
        &mut self,
        g: GlobalId,
        event: &str,
        value: &ast::Expr,
        add: bool,
    ) -> Result<(), String> {
        let action = self.cx.b.m.types.intern(TyKind::Func {
            params: Vec::new(),
            ret: TyTable::VOID,
        });
        let (fn_ptr, env) = match &value.kind {
            ast::ExprKind::Ident(nm) => {
                // The component's global is `{Form}__{id}`, which names the form
                // whose method this is.
                let form = self
                    .cx
                    .b
                    .m
                    .global(g)
                    .name
                    .split("__")
                    .next()
                    .unwrap_or("")
                    .to_string();
                let sig = self
                    .cx
                    .methods
                    .get(&format!("{form}.{nm}"))
                    .ok_or_else(|| format!("`{nm}` is not a method of `{form}`"))?;
                // A form method takes nothing, and an environment handler takes
                // one pointer. A callee ignoring an extra argument is sound under
                // the C convention the library calls through, where the caller
                // cleans the stack — and binding the method itself, with a null
                // environment, gives the same pair every time, which is what
                // lets a later `-=` find it.
                (Expr::FuncPtr(sig.fid), Expr::Null(TyTable::PTR))
            }
            ast::ExprKind::Lambda(l) => {
                if !l.params.is_empty() {
                    return Err("an event handler lambda takes no parameters yet".into());
                }
                let (v, _) = self.expr(value, Some(action))?;
                match v {
                    Expr::MakeClosure { func, env } => (
                        Expr::FuncPtr(func),
                        Expr::Cast {
                            value: env,
                            to: TyTable::PTR,
                        },
                    ),
                    _ => return Err("this lambda could not be made into a handler".into()),
                }
            }
            _ => {
                return Err(
                    "an event takes a method of the form or a lambda written where it is wired"
                        .into(),
                )
            }
        };
        let symbol = if add { "kn_ui_on_env" } else { "kn_ui_off_env" };
        self.push(Stmt::Expr(ui_call(
            symbol,
            vec![Expr::Global(g), Expr::Str(snake_case(event)), fn_ptr, env],
            vec![TyTable::I64, TyTable::STR, TyTable::PTR, TyTable::PTR],
            TyTable::I32,
        )));
        Ok(())
    }

    /// Monomorphise a generic method for the argument types at this call site.
    ///
    /// Type parameters are inferred by matching each declared parameter type
    /// against the lowered argument's type; the instance is cached, so the same
    /// type arguments produce one function with one mangled symbol.
    fn instantiate(
        &mut self,
        key: &str,
        args: &[(Expr, TyId)],
        explicit: Option<&[TyId]>,
    ) -> Result<Sig, String> {
        let t = self.cx.generics[key].clone();
        let m = &t.method;
        if args.len() != m.params.len() {
            return Err(format!(
                "`{key}` expects {} argument(s), got {}",
                m.params.len(),
                args.len()
            ));
        }
        // Written type arguments first, then inference for the rest — which
        // also checks the arguments against what was written.
        let mut tvars: HashMap<String, TyId> = HashMap::new();
        if let Some(explicit) = explicit {
            if explicit.len() != m.type_params.len() {
                return Err(format!(
                    "`{key}` takes {} type argument(s), and {} were written",
                    m.type_params.len(),
                    explicit.len()
                ));
            }
            for (tp, t) in m.type_params.iter().zip(explicit) {
                tvars.insert(tp.clone(), *t);
            }
        }
        for (p, (_, aty)) in m.params.iter().zip(args.iter()) {
            unify(&p.ty, *aty, &m.type_params, &self.cx.b.m.types, &mut tvars)?;
        }
        let mut targs = Vec::new();
        for tp in &m.type_params {
            match tvars.get(tp) {
                Some(t) => targs.push(*t),
                None => {
                    return Err(format!(
                        "cannot infer type parameter `{tp}` of `{key}` from the arguments"
                    ))
                }
            }
        }
        // `where T : I`, `where T : class`, `where T : new()` — checked when
        // the type argument is chosen, so the instance's body may rely on them.
        for (tp, constraint) in &m.constraints {
            let Some(bound) = tvars.get(tp).copied() else { continue };
            match constraint.as_str() {
                "class" => {
                    if !self.cx.is_reference_type(bound) {
                        return Err(format!(
                            "`{key}` needs `{tp}` to be a reference type (`where {tp} : class`), \
                             and {} is not",
                            self.describe_ty(bound)
                        ));
                    }
                }
                "new()" => {
                    if !self.cx.can_new_without_arguments(bound) {
                        return Err(format!(
                            "`{key}` needs `{tp}` to have a parameterless constructor \
                             (`where {tp} : new()`), and {} does not",
                            self.describe_ty(bound)
                        ));
                    }
                }
                iface if self.cx.interfaces.contains_key(iface) => {
                    let ok = self
                        .cx
                        .record_name(bound)
                        .map(|n| self.cx.impls.contains_key(&(n, iface.to_string())))
                        .unwrap_or(false);
                    if !ok {
                        let shown = self
                            .cx
                            .record_name(bound)
                            .unwrap_or_else(|| self.describe_ty(bound));
                        return Err(format!(
                            "`{key}` needs `{tp}` to implement `{iface}`, and `{shown}` does not"
                        ));
                    }
                }
                // A name the compiler does not know as a constraint is left
                // alone, as before: the parser accepts any type name here.
                _ => {}
            }
        }

        let cache_key = (key.to_string(), targs.clone());
        if let Some(sig) = self.cx.mono.get(&cache_key) {
            return Ok(sig.clone());
        }

        // Declare the instance with the type parameters bound.
        let saved = std::mem::replace(&mut self.cx.tvars, tvars.clone());
        let result = (|| -> Result<Sig, String> {
            let ptys: Vec<TyId> = m
                .params
                .iter()
                .map(|p| self.cx.resolve(&p.ty))
                .collect::<Result<_, String>>()?;
            let ret = self.cx.resolve(&m.ret)?;
            let suffix: Vec<String> = targs.iter().map(|t| format!("{}", t.0)).collect();
            let sym = format!("{}_{}${}", t.owner, m.name, suffix.join("_"));
            let mut params: Vec<(&str, TyId)> = Vec::new();
            if t.this {
                let this_ty = self.cx.record_ty(&t.owner);
                params.push(("this", this_ty));
            }
            params.extend(
                m.params
                    .iter()
                    .zip(ptys.iter())
                    .map(|(p, t)| (p.name.as_str(), *t)),
            );
            let fid = self.cx.b.declare_func(&sym, params, ret);
            Ok(Sig {
                fid,
                this: t.this,
                params: ptys,
                defaults: t.method.params.iter().map(|p| p.default.clone()).collect(),
                ret,
            })
        })();
        self.cx.tvars = saved;
        let sig = result?;

        self.cx.mono.insert(cache_key, sig.clone());
        // The body is lowered by the driver, not here: this call is already
        // inside one function's lowering.
        self.cx.pending.push(Pending {
            fid: sig.fid,
            method: t.method.clone(),
            tvars,
            this: t.this,
            env: None,
            ctor: None,
        });
        Ok(sig)
    }

    /// Lower one argument of a command against the slot its signature declares.
    ///
    /// A command's parameters are described by ABI tags, not by Kiln types, and
    /// until now nothing compared the two. A `List<T>` passed where an array
    /// was declared reached the runtime as a record pointer and was read as an
    /// array — a segfault in the library, with nothing in the message to say
    /// which argument was wrong. A mismatch is a compile error instead.
    fn command_arg(
        &mut self,
        name: &str,
        pos: usize,
        a: &ast::Expr,
        pty: TyId,
        tag: i32,
    ) -> Result<Expr, String> {
        let (v, actual) = self.expr(a, Some(pty))?;
        self.check_command_arg(name, pos, v, actual, pty, tag)
    }

    /// The check itself, over an argument that is already lowered.
    fn check_command_arg(
        &mut self,
        name: &str,
        pos: usize,
        v: Expr,
        actual: TyId,
        pty: TyId,
        tag: i32,
    ) -> Result<Expr, String> {
        if actual == pty {
            return Ok(v);
        }
        let got = self.cx.b.m.types.sdt_tag(actual);
        if got == tag {
            // The same shape on the ABI — an int width, say. The emitter
            // marshals it.
            return Ok(v);
        }
        // A number where a number of another width is wanted converts, as it
        // does for any call: an `int` handed to a command taking an `int64`.
        let is_num = |t: i32| matches!(t, 3 | 4 | 6);
        if is_num(got) && is_num(tag) {
            return Ok(Expr::Cast {
                value: Box::new(v),
                to: pty,
            });
        }
        // A `List<T>` where a list of T is declared: build the runtime array
        // the library expects. Crossing an ABI costs a copy, which is what any
        // marshalling layer pays; the alternative is making the caller write
        // the loop, or refusing something perfectly reasonable.
        const ARRAY: i32 = 0x100;
        const ANY_ELEM: i32 = 255;
        if tag & ARRAY != 0 {
            if let Some((_, elem)) = self.cx.as_list(actual) {
                // A command declared over "a list of anything" (`join`, `count`)
                // takes a list of any element type.
                let want = tag & !ARRAY;
                if want == ANY_ELEM || self.cx.b.m.types.sdt_tag(elem) == want {
                    return self.list_to_array(v, actual, elem);
                }
            }
        }
        // Describe what was passed as the Kiln type it is, not as the slot it
        // marshals to: someone who wrote `List<int>` is not helped by being
        // told they passed a record.
        let got_text = match self.cx.as_list(actual) {
            Some((_, e)) => format!("a list of {}", describe_slot(self.cx.b.m.types.sdt_tag(e))),
            None => describe_slot(got),
        };
        Err(format!(
            "`{name}` expects {} for argument {pos}, but this is {got_text}",
            describe_slot(tag)
        ))
    }

    /// Copy a runtime array into a `List<T>`, which is what the language has.
    ///
    /// The mirror of `list_to_array`: a command that answers with a list —
    /// `Text.Split`, `Db.ColumnNames` — hands back a runtime array, and
    /// without this its result can only be handed to another command. One
    /// conversion each way keeps `List<T>` the only list a K2 program sees.
    fn array_to_list(&mut self, v: Expr, elem: TyId) -> Result<(Expr, TyId), String> {
        let arr_ty = self.cx.b.m.types.intern(TyKind::Array(elem));
        let held = self.new_local("$fromary", arr_ty);
        self.push(Stmt::Let {
            local: held,
            value: v,
        });
        // `count(xs)` is the core command that reads an array's length.
        let tag = self.cx.b.m.types.sdt_tag(arr_ty);
        let n = self.new_local("$fromn", TyTable::I32);
        self.push(Stmt::Let {
            local: n,
            value: Expr::Call(Box::new(Call::Command {
                symbol: "kn_ary_count".into(),
                args: vec![Expr::Local(held)],
                arg_slots: vec![SlotTy { tag, ty: arr_ty }],
                ret: TyTable::I32,
            })),
        });

        let lrid = self.cx.list_record(elem);
        let lty = self.cx.b.m.types.intern(TyKind::Record(lrid));
        let data_ty = self.cx.b.m.record(lrid).fields[LIST_DATA].ty;
        let out = self.new_local("$fromlist", lty);
        self.push(Stmt::Let {
            local: out,
            value: Expr::MakeRecord(
                lrid,
                vec![
                    Expr::Int(0, TyTable::I32),
                    Expr::Int(0, TyTable::I32),
                    Expr::Null(data_ty),
                ],
            ),
        });
        let i = self.new_local("$fromi", TyTable::I32);
        self.push(Stmt::Let {
            local: i,
            value: Expr::Int(0, TyTable::I32),
        });
        let get = Expr::Call(Box::new(Call::Dll {
            library: "runtime".into(),
            symbol: "kn_ary_get".into(),
            conv: CallConv::Cdecl,
            args: vec![
                Expr::Local(held),
                // Positions count from 1.
                Expr::Bin(
                    BinOp::Add,
                    Box::new(Expr::Local(i)),
                    Box::new(Expr::Int(1, TyTable::I32)),
                    TyTable::I32,
                ),
            ],
            arg_tys: vec![TyTable::PTR, TyTable::I32],
            ret: TyTable::I64,
            varargs: false,
        }));
        let mut body = vec![Stmt::If {
            cond: Expr::Not(Box::new(Expr::Bin(
                BinOp::Lt,
                Box::new(Expr::Local(i)),
                Box::new(Expr::Local(n)),
                TyTable::I32,
            ))),
            then: vec![Stmt::Break],
            els: vec![],
        }];
        self.blocks.push(Vec::new());
        let val = Expr::Cast {
            value: Box::new(get),
            to: elem,
        };
        self.list_add(out, lty, val);
        let add = self.blocks.pop().unwrap();
        body.extend(add);
        body.push(Stmt::Assign {
            place: Place::Local(i),
            value: Expr::Bin(
                BinOp::Add,
                Box::new(Expr::Local(i)),
                Box::new(Expr::Int(1, TyTable::I32)),
                TyTable::I32,
            ),
        });
        self.push(Stmt::Loop { body });
        Ok((Expr::Local(out), lty))
    }

    /// Copy a `List<T>` into a runtime array, which is what a command's
    /// list-shaped parameter is.
    fn list_to_array(&mut self, v: Expr, lty: TyId, elem: TyId) -> Result<Expr, String> {
        let held = self.new_local("$tolist", lty);
        self.push(Stmt::Let {
            local: held,
            value: v,
        });
        let len = Expr::Field(Box::new(Expr::Local(held)), LIST_LEN);
        let tag = self.cx.b.m.types.sdt_tag(elem);
        let arr_ty = self.cx.b.m.types.intern(TyKind::Array(elem));
        let arr = self.new_local("$toary", arr_ty);
        self.push(Stmt::Let {
            local: arr,
            value: Expr::Call(Box::new(Call::Dll {
                library: "runtime".into(),
                symbol: "kn_ary_new".into(),
                conv: CallConv::Cdecl,
                args: vec![Expr::Int(tag as i128, TyTable::I32), len.clone()],
                arg_tys: vec![TyTable::I32, TyTable::I32],
                ret: arr_ty,
                varargs: false,
            })),
        });
        // for i in 0 .. len-1: kn_ary_set(arr, i + 1, data[i])
        let i = self.new_local("$toi", TyTable::I32);
        self.push(Stmt::Let {
            local: i,
            value: Expr::Int(0, TyTable::I32),
        });
        let data = Expr::Field(Box::new(Expr::Local(held)), LIST_DATA);
        let item = Expr::Index(Box::new(data), Box::new(Expr::Local(i)));
        let body = vec![
            Stmt::If {
                cond: Expr::Not(Box::new(Expr::Bin(
                    BinOp::Lt,
                    Box::new(Expr::Local(i)),
                    Box::new(len),
                    TyTable::I32,
                ))),
                then: vec![Stmt::Break],
                els: vec![],
            },
            Stmt::Expr(Expr::Call(Box::new(Call::Dll {
                library: "runtime".into(),
                symbol: "kn_ary_set".into(),
                conv: CallConv::Cdecl,
                args: vec![
                    Expr::Local(arr),
                    // Positions count from 1.
                    Expr::Bin(
                        BinOp::Add,
                        Box::new(Expr::Local(i)),
                        Box::new(Expr::Int(1, TyTable::I32)),
                        TyTable::I32,
                    ),
                    Expr::Cast {
                        value: Box::new(item),
                        to: TyTable::I64,
                    },
                ],
                arg_tys: vec![TyTable::PTR, TyTable::I32, TyTable::I64],
                ret: TyTable::VOID,
                varargs: false,
            }))),
            Stmt::Assign {
                place: Place::Local(i),
                value: Expr::Bin(
                    BinOp::Add,
                    Box::new(Expr::Local(i)),
                    Box::new(Expr::Int(1, TyTable::I32)),
                    TyTable::I32,
                ),
            },
        ];
        self.push(Stmt::Loop { body });
        Ok(Expr::Local(arr))
    }

    /// A list position checked against the list's length, as a 0-based index.
    ///
    /// Reading or writing outside a list stops the program with a message
    /// naming the position and the length, as 1.x does. Without it a read
    /// returned whatever lay past the buffer and a write corrupted it — the
    /// one outcome an index must never have, and silent.
    /// The call that reports why a program stops: `kn_stop` in the runtime,
    /// which every target links, or `dprintf` for a libc-only build. The
    /// format's trailing newline is the runtime's to add.
    fn stop_message(&self, mut args: Vec<Expr>) -> Expr {
        match self.cx.runtime {
            Runtime::Kiln => {
                if let Some(Expr::Str(s)) = args.first_mut() {
                    if s.ends_with('\n') {
                        s.pop();
                    }
                    if let Some(rest) = s.strip_prefix("kiln: ") {
                        *s = rest.to_string();
                    }
                }
                Expr::Call(Box::new(Call::Dll {
                    library: "runtime".into(),
                    symbol: "kn_stop".into(),
                    conv: CallConv::Cdecl,
                    args,
                    arg_tys: vec![TyTable::STR],
                    ret: TyTable::VOID,
                    varargs: true,
                }))
            }
            Runtime::Libc => {
                args.insert(0, Expr::Int(2, TyTable::I32));
                Expr::Call(Box::new(Call::Dll {
                    library: "c".into(),
                    symbol: "dprintf".into(),
                    conv: CallConv::Cdecl,
                    args,
                    arg_tys: vec![TyTable::I32, TyTable::STR],
                    ret: TyTable::I32,
                    varargs: true,
                }))
            }
        }
    }

    /// Bounds-check a 1-based position into an inline array of `count`, as a
    /// list's is checked, and give back the array and the 0-based index.
    fn checked_inline_index(&mut self, arr: Expr, count: u32, pos: Expr) -> (Expr, Expr) {
        let p = self.new_local("$ixpos", TyTable::I32);
        self.push(Stmt::Let { local: p, value: pos });
        let outside = Expr::Bin(
            BinOp::Or,
            Box::new(Expr::Bin(BinOp::Lt, Box::new(Expr::Local(p)), Box::new(Expr::Int(1, TyTable::I32)), TyTable::I32)),
            Box::new(Expr::Bin(BinOp::Gt, Box::new(Expr::Local(p)), Box::new(Expr::Int(count as i128, TyTable::I32)), TyTable::I32)),
            TyTable::BOOL,
        );
        let report = self.stop_message(vec![Expr::Str("kiln: index %d is outside an inline array of %d element(s)\n".into()), Expr::Local(p),
                Expr::Int(count as i128, TyTable::I32)]);
        let stop = Expr::Call(Box::new(Call::Dll {
            library: "c".into(),
            symbol: "exit".into(),
            conv: CallConv::Cdecl,
            args: vec![Expr::Int(1, TyTable::I32)],
            arg_tys: vec![TyTable::I32],
            ret: TyTable::VOID,
            varargs: false,
        }));
        self.push(Stmt::If { cond: outside, then: vec![Stmt::Expr(report), Stmt::Expr(stop)], els: vec![] });
        let i0 = Expr::Bin(BinOp::Sub, Box::new(Expr::Local(p)), Box::new(Expr::Int(1, TyTable::I32)), TyTable::I32);
        (arr, i0)
    }

    fn checked_list_index(&mut self, list: Expr, lty: TyId, pos: Expr) -> (Expr, Expr) {
        let held = self.new_local("$ixlist", lty);
        self.push(Stmt::Let {
            local: held,
            value: list,
        });
        let p = self.new_local("$ixpos", TyTable::I32);
        self.push(Stmt::Let {
            local: p,
            value: pos,
        });
        let len = Expr::Field(Box::new(Expr::Local(held)), LIST_LEN);
        let outside = Expr::Bin(
            BinOp::Or,
            Box::new(Expr::Bin(
                BinOp::Lt,
                Box::new(Expr::Local(p)),
                Box::new(Expr::Int(1, TyTable::I32)),
                TyTable::I32,
            )),
            Box::new(Expr::Bin(
                BinOp::Gt,
                Box::new(Expr::Local(p)),
                Box::new(len.clone()),
                TyTable::I32,
            )),
            TyTable::BOOL,
        );
        let report = self.stop_message(vec![Expr::Str("kiln: index %d is outside a list of %d element(s)\n".into()), Expr::Local(p),
                len]);
        let stop = Expr::Call(Box::new(Call::Dll {
            library: "c".into(),
            symbol: "exit".into(),
            conv: CallConv::Cdecl,
            args: vec![Expr::Int(1, TyTable::I32)],
            arg_tys: vec![TyTable::I32],
            ret: TyTable::VOID,
            varargs: false,
        }));
        self.push(Stmt::If {
            cond: outside,
            then: vec![Stmt::Expr(report), Stmt::Expr(stop)],
            els: vec![],
        });
        let data = Expr::Field(Box::new(Expr::Local(held)), LIST_DATA);
        let i0 = Expr::Bin(
            BinOp::Sub,
            Box::new(Expr::Local(p)),
            Box::new(Expr::Int(1, TyTable::I32)),
            TyTable::I32,
        );
        (data, i0)
    }

    /// `Name(args)` as the command `name(args)`, when the registry has one
    /// taking that many arguments.
    fn bare_command(
        &mut self,
        name: &str,
        args: &[ast::Expr],
    ) -> Result<Option<(Expr, TyId)>, String> {
        let bare = snake_case(name);
        let Some(reg) = self.cx.registry.as_ref() else {
            return Ok(None);
        };
        let Some(cmd) = reg.get(&bare) else {
            return Ok(None);
        };
        let sig = cmd.sig.clone();
        let symbol = cmd.symbol.clone();
        if sig.params.len() != args.len() {
            return Err(format!(
                "`{name}` expects {} argument(s), got {}",
                sig.params.len(),
                args.len()
            ));
        }
        let params: Vec<TyId> = sig.params.iter().map(|t| ir_ty(*t, &mut self.cx)).collect();
        let tags: Vec<i32> = sig.params.iter().map(|t| t.sdt_tag()).collect();
        let ret = match sig.ret {
            Some(t) => ir_ty(t, &mut self.cx),
            None => TyTable::VOID,
        };
        let mut kargs = Vec::new();
        for (i, (a, pty)) in args.iter().zip(params.iter()).enumerate() {
            kargs.push(self.command_arg(name, i + 1, a, *pty, tags[i])?);
        }
        let slots = params
            .iter()
            .zip(tags.iter())
            .map(|(ty, tag)| SlotTy { tag: *tag, ty: *ty })
            .collect();
        let call = Expr::Call(Box::new(Call::Command {
            symbol,
            args: kargs,
            arg_slots: slots,
            ret,
        }));
        if let TyKind::Array(e) = *self.tt().kind(ret) {
            return self.array_to_list(call, e).map(Some);
        }
        Ok(Some((call, ret)))
    }

    /// Resolve `value.Member(...)` to a command whose first parameter is the
    /// receiver: `length(s)` is written `s.Length`, `db_exec(db, …)` is written
    /// `db.Exec(…)`. The receiver's type must match that first parameter, which
    /// is what keeps `x.Count` on a list from finding some unrelated command.
    fn lookup_instance_command(
        &mut self,
        recv_ty: TyId,
        member: &str,
        extra_args: usize,
    ) -> Option<(String, Vec<TyId>, TyId, Vec<i32>)> {
        let bare = snake_case(member);
        let reg = self.cx.registry.as_ref()?;
        let cmd = reg.get(&bare)?;
        let sig = cmd.sig.clone();
        if sig.params.len() != extra_args + 1 {
            return None;
        }
        let symbol = cmd.symbol.clone();
        let params: Vec<TyId> = sig.params.iter().map(|t| ir_ty(*t, &mut self.cx)).collect();
        if params[0] != recv_ty {
            return None;
        }
        let tags: Vec<i32> = sig.params.iter().map(|t| t.sdt_tag()).collect();
        let ret = match sig.ret {
            Some(t) => ir_ty(t, &mut self.cx),
            None => TyTable::VOID,
        };
        Some((symbol, params, ret, tags))
    }

    /// Resolve `Owner.Member` to a standard-library command: its symbol, its
    /// parameter types, its return type, and each parameter's slot tag.
    fn lookup_command(
        &mut self,
        owner: &str,
        member: &str,
    ) -> Option<(String, Vec<TyId>, TyId, Vec<i32>)> {
        // The spec's rule is that `file_read_text` is written `File.ReadText`.
        // Some libraries do not prefix their commands (`uppercase`, not
        // `text_uppercase`), so the bare member name is tried as well.
        let qualified = format!("{}_{}", snake_case(owner), snake_case(member));
        let bare = snake_case(member);
        let reg = self.cx.registry.as_ref()?;
        let cmd = reg.get(&qualified).or_else(|| reg.get(&bare))?;
        let sig = cmd.sig.clone();
        let symbol = cmd.symbol.clone();
        let params: Vec<TyId> = sig.params.iter().map(|t| ir_ty(*t, &mut self.cx)).collect();
        let tags: Vec<i32> = sig.params.iter().map(|t| t.sdt_tag()).collect();
        let ret = match sig.ret {
            Some(t) => ir_ty(t, &mut self.cx),
            None => TyTable::VOID,
        };
        Some((symbol, params, ret, tags))
    }

    fn is_type_name(&self, name: &str) -> bool {
        self.cx.type_names.contains(name)
            || self.cx.type_ids.contains_key(name)
            || matches!(
                name,
                "Console" | "Math" | "int" | "long" | "string" | "double"
            )
    }

    fn new_record(
        &mut self,
        t: &ast::TypeRef,
        args: &[ast::Expr],
        inits: &[(String, ast::Expr)],
    ) -> Result<(Expr, TyId), String> {
        let ty = self.cx.resolve(t)?;
        // `new HashSet<T>()` starts empty; it grows like a list.
        if let Some((srid, _)) = self.cx.as_set(ty) {
            let data_ty = self.cx.b.m.record(srid).fields[LIST_DATA].ty;
            let index_ty = self.cx.b.m.record(srid).fields[SET_INDEX].ty;
            return Ok((
                Expr::MakeRecord(
                    srid,
                    vec![
                        Expr::Int(0, TyTable::I32),
                        Expr::Int(0, TyTable::I32),
                        Expr::Null(data_ty),
                        Expr::Null(index_ty),
                    ],
                ),
                ty,
            ));
        }
        // `new Dictionary<K,V>()` starts empty; buffers grow on first set.
        if let Some((drid, _, _)) = self.cx.as_dict(ty) {
            let keys_ty = self.cx.b.m.record(drid).fields[DICT_KEYS].ty;
            let vals_ty = self.cx.b.m.record(drid).fields[DICT_VALUES].ty;
            let index_ty = self.cx.b.m.record(drid).fields[DICT_INDEX].ty;
            return Ok((
                Expr::MakeRecord(
                    drid,
                    vec![
                        Expr::Int(0, TyTable::I32),
                        Expr::Int(0, TyTable::I32),
                        Expr::Null(keys_ty),
                        Expr::Null(vals_ty),
                        Expr::Null(index_ty),
                    ],
                ),
                ty,
            ));
        }
        // `new List<T>()` starts empty; the buffer is allocated on first Add.
        if self.cx.as_list(ty).is_some() {
            let TyKind::Record(lrid) = *self.tt().kind(ty) else {
                unreachable!()
            };
            let data_ty = self.cx.b.m.record(lrid).fields[LIST_DATA].ty;
            return Ok((
                Expr::MakeRecord(
                    lrid,
                    vec![
                        Expr::Int(0, TyTable::I32),
                        Expr::Int(0, TyTable::I32),
                        Expr::Null(data_ty),
                    ],
                ),
                ty,
            ));
        }
        let TyKind::Record(rid) = *self.tt().kind(ty) else {
            return Err("`new` is only supported for records/classes for now".into());
        };
        // A declared constructor wins over positional field initialisation.
        let rname = self.cx.b.m.record(rid).name.clone();
        if let Some(sig) = self.cx.methods.get(&format!("{rname}.$ctor")).cloned() {
            let kargs = self.call_args(&format!("new {rname}"), args, &sig)?;
            return Ok((
                Expr::Call(Box::new(Call::Direct {
                    func: sig.fid,
                    args: kargs,
                })),
                sig.ret,
            ));
        }
        let field_tys: Vec<(String, TyId)> = self
            .cx
            .b
            .m
            .record(rid)
            .fields
            .iter()
            .map(|f| (f.name.clone(), f.ty))
            .collect();
        let mut values: Vec<Expr> = Vec::with_capacity(field_tys.len());
        if field_tys.iter().any(|(_, t)| matches!(self.tt().kind(*t), TyKind::Inline { .. })) {
            if !args.is_empty() {
                return Err(format!(
                    "`{rname}` holds storage in place, so it is made zeroed with `new {rname}()` and filled field by field"
                ));
            }
            if let Some((n, _)) = inits
                .iter()
                .find(|(n, _)| field_tys.iter().any(|(f, t)| f == n && matches!(self.tt().kind(*t), TyKind::Inline { .. })))
            {
                return Err(format!("`{n}` is held in place — fill it element by element after `new`"));
            }
        }
        if !args.is_empty() {
            // Positional: one argument per field, in order. `zip` dropped what
            // did not line up, so a short list left fields holding garbage.
            if args.len() != field_tys.len() {
                return Err(format!(
                    "`new {}` takes {} value(s), one per field, but this passes {}",
                    self.cx.b.m.record(rid).name,
                    field_tys.len(),
                    args.len()
                ));
            }
            for (i, (a, (fname, fty))) in args.iter().zip(field_tys.iter()).enumerate() {
                let (v, vty) = self.expr(a, Some(*fty))?;
                let _ = i;
                let (v, _) = self.settle(v, vty, *fty, &format!("field `{fname}`"))?;
                values.push(v);
            }
        } else {
            // An object initialiser, or `new T()`: a field not given is zero —
            // 0, false, or no value for anything held by reference.
            for (fname, fty) in &field_tys {
                if let Some((_, e)) = inits.iter().find(|(n, _)| n == fname) {
                    values.push(self.expr(e, Some(*fty))?.0);
                } else if self.tt().is_pointer(*fty) {
                    values.push(Expr::Null(*fty));
                } else if self.tt().is_float(*fty) {
                    values.push(Expr::Float(0.0, *fty));
                } else if *fty == TyTable::BOOL {
                    values.push(Expr::Bool(false));
                } else {
                    values.push(Expr::Int(0, *fty));
                }
            }
        }
        Ok((Expr::MakeRecord(rid, values), ty))
    }

    fn ternary(
        &mut self,
        c: &ast::Expr,
        a: &ast::Expr,
        b: &ast::Expr,
        hint: Option<TyId>,
    ) -> Result<(Expr, TyId), String> {
        let cond = self.condition(c)?;
        // `s == null ? "none" : s` proves `s` present in the arm that runs when
        // it is not null, exactly as an `if`/`else` does. Without the proof the
        // arm read `s` as the `T?` it is and stored the pair into a slot typed
        // from the other arm — valid Kiln that clang then refused.
        let (narrow_then, narrow_else) = null_test_target(c);
        let saved = self.narrowed.clone();
        // Each arm is lowered into its own branch, with whatever statements it
        // emits to compute itself: `ok ? xs[99] : 0` must not check the index
        // when `ok` is false.
        self.blocks.push(Vec::new());
        if let Some(n) = &narrow_then {
            self.narrowed.insert(n.clone());
        }
        let a_res = self.expr(a, hint);
        let mut then = self.blocks.pop().unwrap();
        self.narrowed = saved.clone();
        if let Some(n) = &narrow_else {
            self.narrowed.insert(n.clone());
        }
        let (av, aty) = a_res?;
        self.blocks.push(Vec::new());
        let b_res = self.expr(b, hint.or(Some(aty)));
        let mut els = self.blocks.pop().unwrap();
        self.narrowed = saved;
        let (bv, bty) = b_res?;
        // The two arms may differ only in optionality — `n == 1 ? "one" : null`
        // is the natural way to write it — and then the result is the optional:
        // the plain arm is wrapped. Taking the plain arm's type instead put the
        // other arm's `{value, present}` pair into a plain slot, which is valid
        // Kiln that clang refuses at the IR.
        let (av, aty, bv) = match (
            self.tt().kind(aty).clone(),
            self.tt().kind(bty).clone(),
        ) {
            (TyKind::Optional(i), _) if bty == i => {
                (av, aty, Expr::MakeOptional(i, Some(Box::new(bv))))
            }
            (_, TyKind::Optional(i)) if aty == i => (
                Expr::MakeOptional(i, Some(Box::new(av))),
                bty,
                bv,
            ),
            _ => (av, aty, bv),
        };
        let t = self.new_local("$tern", aty);
        then.push(Stmt::Assign {
            place: Place::Local(t),
            value: av,
        });
        els.push(Stmt::Assign {
            place: Place::Local(t),
            value: bv,
        });
        self.push(Stmt::If { cond, then, els });
        Ok((Expr::Local(t), aty))
    }

    fn binary(
        &mut self,
        op: ast::BinOp,
        a: &ast::Expr,
        b: &ast::Expr,
        hint: Option<TyId>,
    ) -> Result<(Expr, TyId), String> {
        // Arithmetic happens on the value type; a `T?` target wraps the result
        // afterwards, so never let an optional become the operand type.
        let hint = hint.map(|h| match *self.tt().kind(h) {
            TyKind::Optional(inner) => inner,
            _ => h,
        });
        // short-circuit && / ||
        if op == ast::BinOp::And || op == ast::BinOp::Or {
            let (av, _) = self.expr(a, Some(TyTable::BOOL))?;
            let t = self.new_local("$sc", TyTable::BOOL);
            self.push(Stmt::Let {
                local: t,
                value: av,
            });
            // The right side is lowered into the branch that only runs when it
            // is needed — including any statements it emits to compute itself.
            // Lowering it first put those before the `if`, so a bounds check
            // or a call in `ready && xs[9] > 0` ran even when `ready` was false.
            self.blocks.push(Vec::new());
            let rhs = self.expr(b, Some(TyTable::BOOL));
            let mut then = self.blocks.pop().unwrap();
            let (bv, _) = rhs?;
            then.push(Stmt::Assign {
                place: Place::Local(t),
                value: bv,
            });
            if op == ast::BinOp::And {
                self.push(Stmt::If {
                    cond: Expr::Local(t),
                    then,
                    els: vec![],
                });
            } else {
                self.push(Stmt::If {
                    cond: Expr::Not(Box::new(Expr::Local(t))),
                    then,
                    els: vec![],
                });
            }
            return Ok((Expr::Local(t), TyTable::BOOL));
        }

        // `a >>> n` — a logical shift on the operand's own width: reinterpret as
        // the unsigned twin, shift (which KIR emits as `lshr` for unsigned),
        // and reinterpret back.
        if op == ast::BinOp::UShr {
            let (av, aty) = self.expr(a, hint)?;
            let (bv, _) = self.expr(b, Some(aty))?;
            let uns = match self.tt().kind(aty) {
                TyKind::I8 | TyKind::U8 => TyTable::U8,
                TyKind::I16 | TyKind::U16 => TyTable::U16,
                TyKind::I32 | TyKind::U32 => TyTable::U32,
                TyKind::I64 | TyKind::U64 => TyTable::U64,
                TyKind::Nint | TyKind::Nuint => TyTable::NUINT,
                _ => return Err("`>>>` shifts a whole number".into()),
            };
            let shifted = Expr::Bin(
                BinOp::Shr,
                Box::new(Expr::Cast { value: Box::new(av), to: uns }),
                Box::new(Expr::Cast { value: Box::new(bv), to: uns }),
                uns,
            );
            return Ok((
                Expr::Cast {
                    value: Box::new(shifted),
                    to: aty,
                },
                aty,
            ));
        }
        let kop = match op {
            ast::BinOp::UShr => unreachable!("handled above"),
            ast::BinOp::Add => BinOp::Add,
            ast::BinOp::Sub => BinOp::Sub,
            ast::BinOp::Mul => BinOp::Mul,
            ast::BinOp::Div => BinOp::Div,
            ast::BinOp::Rem => BinOp::Rem,
            ast::BinOp::Eq => BinOp::Eq,
            ast::BinOp::Ne => BinOp::Ne,
            ast::BinOp::Lt => BinOp::Lt,
            ast::BinOp::Le => BinOp::Le,
            ast::BinOp::Gt => BinOp::Gt,
            ast::BinOp::Ge => BinOp::Ge,
            ast::BinOp::BitAnd => BinOp::And,
            ast::BinOp::BitOr => BinOp::Or,
            ast::BinOp::BitXor => BinOp::Xor,
            ast::BinOp::Shl => BinOp::Shl,
            ast::BinOp::Shr => BinOp::Shr,
            ast::BinOp::And | ast::BinOp::Or => unreachable!(),
        };
        // `string + string` builds a new string rather than adding pointers.
        // The probe is lowered into a block that is thrown away when `a` turns
        // out not to be a string, so `a` is emitted once either way.
        if op == ast::BinOp::Add {
            self.blocks.push(Vec::new());
            let probe = self.expr_raw(a, None);
            let pushed = self.blocks.pop().unwrap();
            let probe = probe?;
            if probe.1 == TyTable::STR {
                for st in pushed {
                    self.push(st);
                }
                let rhs = self.expr(b, Some(TyTable::STR))?;
                return Ok(self.build_string(vec![
                    (String::new(), Some(probe)),
                    (String::new(), Some(rhs)),
                ]));
            }
            // not a string: lower `a` for real below, with the real hint
        }
        let is_cmp = matches!(
            op,
            ast::BinOp::Eq
                | ast::BinOp::Ne
                | ast::BinOp::Lt
                | ast::BinOp::Le
                | ast::BinOp::Gt
                | ast::BinOp::Ge
        );
        // Operand type: prefer the hint for arithmetic; infer from operands for
        // comparisons.
        let (av, aty) = self.expr(a, if is_cmp { None } else { hint })?;
        let (bv, bty) = self.expr(b, Some(aty))?;
        // Text compares by content. Comparing the pointers made
        // `"fizz" + "buzz" == "fizzbuzz"` false, which no reader expects and
        // which neither C# nor Kiln 1.x does.
        if is_cmp && aty == TyTable::STR && bty == TyTable::STR {
            let order = Expr::Call(Box::new(Call::Dll {
                library: "c".into(),
                symbol: "strcmp".into(),
                conv: CallConv::Cdecl,
                args: vec![av, bv],
                arg_tys: vec![TyTable::STR, TyTable::STR],
                ret: TyTable::I32,
                varargs: false,
            }));
            return Ok((
                Expr::Bin(kop, Box::new(order), Box::new(Expr::Int(0, TyTable::I32)), TyTable::I32),
                TyTable::BOOL,
            ));
        }
        // Two numbers of different widths meet at the wider one — a float
        // over any whole number — so `intValue == longValue` compares 64 bits
        // rather than emitting an i32 beside an i64.
        let rank = |t: TyId| -> Option<i64> {
            match self.tt().kind(t) {
                TyKind::I8 | TyKind::U8 => Some(1),
                TyKind::I16 | TyKind::U16 => Some(2),
                TyKind::I32 | TyKind::U32 | TyKind::Char => Some(4),
                TyKind::I64 | TyKind::U64 | TyKind::Nint | TyKind::Nuint => Some(8),
                TyKind::F32 => Some(16),
                TyKind::F64 => Some(32),
                _ => None,
            }
        };
        let (av, aty, bv, bty) = match (rank(aty), rank(bty)) {
            (Some(ra), Some(rb)) if ra < rb => (Expr::Cast { value: Box::new(av), to: bty }, bty, bv, bty),
            (Some(ra), Some(rb)) if rb < ra => (av, aty, Expr::Cast { value: Box::new(bv), to: aty }, aty),
            _ => (av, aty, bv, bty),
        };
        let operand_ty = if aty != TyTable::BOOL { aty } else { bty };
        // Arithmetic and bit operations are on numbers. Text in one was
        // emitted as a multiply of a pointer — invalid LLVM, rejected by clang
        // with nothing to say which line did it.
        if !is_cmp {
            for (t, side) in [(aty, "left"), (bty, "right")] {
                let numeric = matches!(
                    self.tt().kind(t),
                    TyKind::I8 | TyKind::I16 | TyKind::I32 | TyKind::I64
                        | TyKind::U8 | TyKind::U16 | TyKind::U32 | TyKind::U64
                        | TyKind::Nint | TyKind::Nuint | TyKind::F32 | TyKind::F64
                        | TyKind::Char | TyKind::Bool
                );
                if !numeric {
                    return Err(format!(
                        "`{}` works on numbers, but its {side} side is {}{}",
                        ast_op_text(op),
                        self.describe_ty(t),
                        if t == TyTable::STR && op == ast::BinOp::Mul {
                            " — repeat text with `Repeat(text, count)`"
                        } else {
                            ""
                        }
                    ));
                }
            }
        }
        // Whole-number division by zero stops with a message, as 1.x does,
        // rather than dying on SIGFPE with nothing said.
        let int_div = matches!(kop, BinOp::Div | BinOp::Rem)
            && !self.tt().is_float(operand_ty)
            && !matches!(bv, Expr::Int(n, _) if n != 0);
        let (av, bv) = if int_div {
            let l = self.new_local("$dividend", aty);
            self.push(Stmt::Let { local: l, value: av });
            let r = self.new_local("$divisor", bty);
            self.push(Stmt::Let { local: r, value: bv });
            let report = self.stop_message(vec![Expr::Str("kiln: division by zero\n".into())]);
            let stop = Expr::Call(Box::new(Call::Dll {
                library: "c".into(),
                symbol: "exit".into(),
                conv: CallConv::Cdecl,
                args: vec![Expr::Int(1, TyTable::I32)],
                arg_tys: vec![TyTable::I32],
                ret: TyTable::VOID,
                varargs: false,
            }));
            let zero = Expr::Bin(
                BinOp::Eq,
                Box::new(Expr::Local(r)),
                Box::new(Expr::Int(0, bty)),
                bty,
            );
            self.push(Stmt::If {
                cond: zero,
                then: vec![Stmt::Expr(report), Stmt::Expr(stop)],
                els: vec![],
            });
            (Expr::Local(l), Expr::Local(r))
        } else {
            (av, bv)
        };
        let out = Expr::Bin(kop, Box::new(av), Box::new(bv), operand_ty);
        let rty = if is_cmp { TyTable::BOOL } else { operand_ty };
        Ok((out, rty))
    }

    // ─── places ─────────────────────────────────────────────────────────────

    fn place(&mut self, e: &ast::Expr) -> Result<Place, String> {
        match &e.kind {
            ast::ExprKind::Ident(name) => {
                if let Some(c) = self.cells.get(name).cloned() {
                    return Ok(Place::Field(Box::new(c.ptr), 0));
                }
                if let Some((lid, _)) = self.scope.get(name).copied() {
                    return Ok(Place::Local(lid));
                }
                if let Some((g, _)) = self.cx.form_state.get(name).copied() {
                    return Ok(Place::Global(g));
                }
                // A bare name inside an instance method may be a field of `this`.
                if let Some((this_lid, this_ty)) = self.scope.get("this").copied() {
                    if let TyKind::Record(rid) = *self.tt().kind(this_ty) {
                        let rec = self.cx.b.m.record(rid);
                        if let Some(idx) = rec.fields.iter().position(|f| f.name == *name) {
                            return Ok(Place::Field(Box::new(Expr::Local(this_lid)), idx));
                        }
                    }
                }
                Err(format!("cannot assign to unknown `{name}`"))
            }
            ast::ExprKind::Member(recv, member) => {
                if let ast::ExprKind::Ident(obj) = &recv.kind {
                    if !self.scope.contains_key(obj.as_str()) && !self.cells.contains_key(obj.as_str()) {
                        if let Some((g, _)) =
                            self.cx.form_state.get(&format!("{obj}.{member}")).copied()
                        {
                            return Ok(Place::Global(g));
                        }
                    }
                }
                let (mut base, mut bty) = self.expr(recv, None)?;
                // A field of a `T?` is a field of the value it holds; a missing
                // one stops the program rather than writing through null.
                if let TyKind::Optional(inner) = *self.tt().kind(bty) {
                    base = self.present_or_stop(base, bty, member)?;
                    bty = inner;
                }
                if let TyKind::Record(rid) = *self.tt().kind(bty) {
                    let rec = self.cx.b.m.record(rid);
                    if let Some(idx) = rec.fields.iter().position(|f| f.name == *member) {
                        if matches!(self.tt().kind(rec.fields[idx].ty), TyKind::Inline { .. }) {
                            return Err(format!(
                                "`{member}` is held in place — assign its elements or fields one by one"
                            ));
                        }
                        return Ok(Place::Field(Box::new(base), idx));
                    }
                }
                Err(format!("cannot assign to member `{member}`"))
            }
            // A store mirrors the read of the same shape exactly. It used to
            // hand the emitter the list record or the Bytes pointer as if it
            // were an array, and `xs[2] = 20` crashed the compiler.
            ast::ExprKind::Index(base, idx) => {
                let (b, bty) = self.expr(base, None)?;
                let (i, _) = self.expr(idx, Some(TyTable::I32))?;
                // Bytes are addressed by offset, from 0, as the read is.
                if bty == TyTable::BYTES {
                    let u8arr = self.cx.b.m.types.intern(TyKind::Array(TyTable::U8));
                    return Ok(Place::Index(
                        Box::new(Expr::Cast {
                            value: Box::new(b),
                            to: u8arr,
                        }),
                        Box::new(Expr::Bin(
                            BinOp::Add,
                            Box::new(i),
                            Box::new(Expr::Int(BIN_HEADER as i128, TyTable::I32)),
                            TyTable::I32,
                        )),
                    ));
                }
                if self.cx.as_dict(bty).is_some() {
                    return Err("store into a Dictionary with `d[key] = value`".into());
                }
                if self.cx.as_list(bty).is_some() {
                    let (data, i0) = self.checked_list_index(b, bty, i);
                    return Ok(Place::Index(Box::new(data), Box::new(i0)));
                }
                if let TyKind::Inline { elem, count } = *self.tt().kind(bty) {
                    if matches!(self.tt().kind(elem), TyKind::Record(_)) {
                        return Err("assign the fields of a nested record one by one".into());
                    }
                    let (b, i0) = self.checked_inline_index(b, count, i);
                    return Ok(Place::Index(Box::new(b), Box::new(i0)));
                }
                let i0 = Expr::Bin(
                    BinOp::Sub,
                    Box::new(i),
                    Box::new(Expr::Int(1, TyTable::I32)),
                    TyTable::I32,
                );
                if matches!(self.tt().kind(bty), TyKind::Array(_)) {
                    return Ok(Place::Index(Box::new(b), Box::new(i0)));
                }
                Err("only a List, an array or Bytes can be stored into by position".into())
            }
            _ => Err("invalid assignment target".into()),
        }
    }

    /// The type a place holds, when it can be read off the place itself.
    fn type_of_place(&self, p: &Place) -> Option<TyId> {
        match p {
            Place::Local(l) => Some(self.cx.b.m.func(self.fid).locals[l.0 as usize].ty),
            Place::Global(g) => Some(self.cx.b.m.global(*g).ty),
            Place::Field(base, i) => match self.tt().kind(self.expr_ty_of(base)) {
                TyKind::Record(rid) => self.cx.b.m.record(*rid).fields.get(*i).map(|f| f.ty),
                _ => None,
            },
            Place::Index(base, _) => {
                let bty = match base.as_ref() {
                    Expr::Field(inner, i) => match self.tt().kind(self.expr_ty_of(inner)) {
                        TyKind::Record(rid) => self.cx.b.m.record(*rid).fields.get(*i).map(|f| f.ty)?,
                        _ => return None,
                    },
                    other => self.expr_ty_of(other),
                };
                match self.tt().kind(bty) {
                    TyKind::Array(e) => Some(*e),
                    _ => None,
                }
            }
        }
    }

    /// The current value of a place, for a compound assignment.
    fn read_place(&self, p: &Place) -> Expr {
        match p {
            Place::Local(l) => Expr::Local(*l),
            Place::Global(g) => Expr::Global(*g),
            Place::Field(base, i) => Expr::Field(base.clone(), *i),
            Place::Index(base, i) => Expr::Index(base.clone(), i.clone()),
        }
    }

    fn place_ty(&mut self, e: &ast::Expr) -> Result<TyId, String> {
        Ok(self.expr(e, None)?.1)
    }
}

/// Names the lambdas inside a function body close over. An over-approximation:
/// it is every identifier any lambda mentions, so a local sharing a name is
/// celled needlessly — safe, and costing one indirection.
fn captured_names(
    body: &[ast::Stmt],
    expr_body: Option<&ast::Expr>,
) -> std::collections::HashSet<String> {
    let mut lambdas = Vec::new();
    for s in body {
        collect_lambdas_stmt(s, &mut lambdas);
    }
    if let Some(e) = expr_body {
        collect_lambdas_expr(e, &mut lambdas);
    }
    let mut out = std::collections::HashSet::new();
    for l in lambdas {
        let mut free = Vec::new();
        collect_idents_lambda(&l, &mut free);
        let bound: Vec<&str> = l.params.iter().map(|(n, _)| n.as_str()).collect();
        for n in free {
            if !bound.contains(&n.as_str()) {
                out.insert(n);
            }
        }
    }
    out
}

fn collect_lambdas_expr(e: &ast::Expr, out: &mut Vec<ast::Lambda>) {
    use ast::ExprKind as E;
    match &e.kind {
        E::DictInit(_, entries) => {
            for (k, v) in entries {
                collect_lambdas_expr(k, out);
                collect_lambdas_expr(v, out);
            }
        }
        E::Collection(items) => {
            for i in items {
                collect_lambdas_expr(i, out);
            }
        }
        E::Lambda(l) => {
            out.push(l.clone());
            // a nested lambda's captures matter to this function too
            match &l.body {
                ast::LambdaBody::Expr(x) => collect_lambdas_expr(x, out),
                ast::LambdaBody::Block(b) => {
                    for s in b {
                        collect_lambdas_stmt(s, out);
                    }
                }
            }
        }
        E::Member(b, _) => collect_lambdas_expr(b, out),
        E::Call(c, args) => {
            collect_lambdas_expr(c, out);
            for a in args {
                collect_lambdas_expr(a, out);
            }
        }
        E::Index(a, b) | E::Binary(_, a, b) | E::NullCoalesce(a, b) | E::Range(a, b, _) => {
            collect_lambdas_expr(a, out);
            collect_lambdas_expr(b, out);
        }
        E::Unary(_, a) | E::Cast(_, a) | E::Try(a) | E::NullForgiving(a) | E::TypeArgs(a, _) => {
            collect_lambdas_expr(a, out)
        }
        E::NullConditional(recv, steps) => {
            collect_lambdas_expr(recv, out);
            for st in steps {
                match st {
                    ast::NullStep::Member(_) => {}
                    ast::NullStep::Call(args) => {
                        for a in args {
                            collect_lambdas_expr(a, out);
                        }
                    }
                    ast::NullStep::Index(i) => collect_lambdas_expr(i, out),
                }
            }
        }
        E::Ternary(a, b, c) => {
            collect_lambdas_expr(a, out);
            collect_lambdas_expr(b, out);
            collect_lambdas_expr(c, out);
        }
        E::New(_, args, inits) => {
            for a in args {
                collect_lambdas_expr(a, out);
            }
            for (_, v) in inits {
                collect_lambdas_expr(v, out);
            }
        }
        E::Interp(segs) => {
            for sg in segs {
                if let ast::InterpSeg::Expr(x) = sg {
                    collect_lambdas_expr(x, out);
                }
            }
        }
        E::Switch(subj, arms) => {
            collect_lambdas_expr(subj, out);
            for a in arms {
                collect_lambdas_expr(&a.value, out);
            }
        }
        E::Int(_)
        | E::Float(_, _)
        | E::Bool(_)
        | E::Str(_)
        | E::Char(_)
        | E::Null
        | E::Ident(_) => {}
    }
}

fn collect_lambdas_stmt(s: &ast::Stmt, out: &mut Vec<ast::Lambda>) {
    use ast::StmtKind as S;
    match &s.kind {
        S::Local { value, .. } => collect_lambdas_expr(value, out),
        S::Assign { target, value, .. } => {
            collect_lambdas_expr(target, out);
            collect_lambdas_expr(value, out);
        }
        S::Expr(e) => collect_lambdas_expr(e, out),
        S::Return(Some(e)) => collect_lambdas_expr(e, out),
        S::Return(None) | S::Break | S::Continue => {}
        S::If { cond, then, els } => {
            collect_lambdas_expr(cond, out);
            for x in then.iter().chain(els) {
                collect_lambdas_stmt(x, out);
            }
        }
        S::While { cond, body } => {
            collect_lambdas_expr(cond, out);
            for x in body {
                collect_lambdas_stmt(x, out);
            }
        }
        S::For {
            init,
            cond,
            step,
            body,
        } => {
            if let Some(i) = init.as_ref() {
                collect_lambdas_stmt(i, out);
            }
            if let Some(c) = cond {
                collect_lambdas_expr(c, out);
            }
            if let Some(st) = step.as_ref() {
                collect_lambdas_stmt(st, out);
            }
            for x in body {
                collect_lambdas_stmt(x, out);
            }
        }
        S::ForEach { coll, body, .. } => {
            collect_lambdas_expr(coll, out);
            for x in body {
                collect_lambdas_stmt(x, out);
            }
        }
        S::Switch { subject, sections } => {
            collect_lambdas_expr(subject, out);
            for sec in sections {
                for l in &sec.labels {
                    if let ast::SwitchPat::Const(c) | ast::SwitchPat::Relational(_, c) = l {
                        collect_lambdas_expr(c, out);
                    }
                }
                for x in &sec.body {
                    collect_lambdas_stmt(x, out);
                }
            }
        }
        S::Defer(d) => collect_lambdas_stmt(d, out),
        S::Block(b) => {
            for x in b {
                collect_lambdas_stmt(x, out);
            }
        }
    }
}

/// Every identifier mentioned in a lambda body, so captures can be detected.
fn collect_idents_lambda(l: &ast::Lambda, out: &mut Vec<String>) {
    match &l.body {
        ast::LambdaBody::Expr(e) => collect_idents_expr(e, out),
        ast::LambdaBody::Block(b) => {
            for s in b {
                collect_idents_stmt(s, out);
            }
        }
    }
}

fn collect_idents_expr(e: &ast::Expr, out: &mut Vec<String>) {
    use ast::ExprKind as E;
    match &e.kind {
        E::DictInit(_, entries) => {
            for (k, v) in entries {
                collect_idents_expr(k, out);
                collect_idents_expr(v, out);
            }
        }
        E::Collection(items) => {
            for i in items {
                collect_idents_expr(i, out);
            }
        }
        E::Ident(n) => out.push(n.clone()),
        E::Member(b, _) => collect_idents_expr(b, out),
        E::Call(c, args) => {
            collect_idents_expr(c, out);
            for a in args {
                collect_idents_expr(a, out);
            }
        }
        E::Index(a, b) => {
            collect_idents_expr(a, out);
            collect_idents_expr(b, out);
        }
        E::Unary(_, a) | E::Cast(_, a) | E::Try(a) | E::NullForgiving(a) | E::TypeArgs(a, _) => {
            collect_idents_expr(a, out)
        }
        E::NullConditional(recv, steps) => {
            collect_idents_expr(recv, out);
            for st in steps {
                match st {
                    ast::NullStep::Member(_) => {}
                    ast::NullStep::Call(args) => {
                        for a in args {
                            collect_idents_expr(a, out);
                        }
                    }
                    ast::NullStep::Index(i) => collect_idents_expr(i, out),
                }
            }
        }
        E::Binary(_, a, b) | E::NullCoalesce(a, b) => {
            collect_idents_expr(a, out);
            collect_idents_expr(b, out);
        }
        E::Range(a, b, _) => {
            collect_idents_expr(a, out);
            collect_idents_expr(b, out);
        }
        E::Ternary(a, b, c) => {
            collect_idents_expr(a, out);
            collect_idents_expr(b, out);
            collect_idents_expr(c, out);
        }
        E::New(_, args, inits) => {
            for a in args {
                collect_idents_expr(a, out);
            }
            for (_, v) in inits {
                collect_idents_expr(v, out);
            }
        }
        E::Interp(segs) => {
            for s in segs {
                if let ast::InterpSeg::Expr(x) = s {
                    collect_idents_expr(x, out);
                }
            }
        }
        E::Lambda(inner) => collect_idents_lambda(inner, out),
        E::Switch(subj, arms) => {
            collect_idents_expr(subj, out);
            for a in arms {
                if let ast::SwitchPat::Const(c) | ast::SwitchPat::Relational(_, c) = &a.pat {
                    collect_idents_expr(c, out);
                }
                collect_idents_expr(&a.value, out);
            }
        }
        E::Int(_) | E::Float(_, _) | E::Bool(_) | E::Str(_) | E::Char(_) | E::Null => {}
    }
}

fn collect_idents_stmt(s: &ast::Stmt, out: &mut Vec<String>) {
    use ast::StmtKind as S;
    match &s.kind {
        S::Local { value, .. } => collect_idents_expr(value, out),
        S::Assign { target, value, .. } => {
            collect_idents_expr(target, out);
            collect_idents_expr(value, out);
        }
        S::Expr(e) => collect_idents_expr(e, out),
        S::Return(Some(e)) => collect_idents_expr(e, out),
        S::Return(None) | S::Break | S::Continue => {}
        S::If { cond, then, els } => {
            collect_idents_expr(cond, out);
            for x in then.iter().chain(els) {
                collect_idents_stmt(x, out);
            }
        }
        S::While { cond, body } => {
            collect_idents_expr(cond, out);
            for x in body {
                collect_idents_stmt(x, out);
            }
        }
        S::For {
            init,
            cond,
            step,
            body,
        } => {
            if let Some(i) = init.as_ref() {
                collect_idents_stmt(i, out);
            }
            if let Some(c) = cond {
                collect_idents_expr(c, out);
            }
            if let Some(st) = step.as_ref() {
                collect_idents_stmt(st, out);
            }
            for x in body {
                collect_idents_stmt(x, out);
            }
        }
        S::ForEach { coll, body, .. } => {
            collect_idents_expr(coll, out);
            for x in body {
                collect_idents_stmt(x, out);
            }
        }
        S::Switch { subject, sections } => {
            collect_idents_expr(subject, out);
            for sec in sections {
                for l in &sec.labels {
                    if let ast::SwitchPat::Const(c) | ast::SwitchPat::Relational(_, c) = l {
                        collect_idents_expr(c, out);
                    }
                }
                for x in &sec.body {
                    collect_idents_stmt(x, out);
                }
            }
        }
        S::Defer(d) => collect_idents_stmt(d, out),
        S::Block(b) => {
            for x in b {
                collect_idents_stmt(x, out);
            }
        }
    }
}

/// Match a declared parameter type against a concrete argument type, binding
/// any type parameter it names. Concrete positions are not checked here — the
/// instance body is type-checked when it is lowered.
fn unify(
    decl: &ast::TypeRef,
    actual: TyId,
    type_params: &[String],
    tt: &TyTable,
    out: &mut HashMap<String, TyId>,
) -> Result<(), String> {
    match decl {
        ast::TypeRef::Named(n) if type_params.iter().any(|p| p == n) => {
            match out.get(n) {
                Some(prev) if *prev != actual => {
                    return Err(format!("conflicting types inferred for `{n}`"))
                }
                _ => {
                    out.insert(n.clone(), actual);
                }
            }
            Ok(())
        }
        ast::TypeRef::Array(_) | ast::TypeRef::Generic(_, _)
            if matches!(tt.kind(actual), TyKind::Array(_)) =>
        {
            let elem = match tt.kind(actual) {
                TyKind::Array(e) => *e,
                _ => unreachable!(),
            };
            let decl_elem = match decl {
                ast::TypeRef::Array(inner) => inner.as_ref(),
                ast::TypeRef::Generic(_, args) if !args.is_empty() => &args[0],
                _ => return Ok(()),
            };
            unify(decl_elem, elem, type_params, tt, out)
        }
        ast::TypeRef::Optional(inner) => {
            if let TyKind::Optional(t) = tt.kind(actual) {
                return unify(inner, *t, type_params, tt, out);
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// For `x != null` returns `(Some(x), None)`; for `x == null`, `(None, Some(x))`
/// — the branch in which `x` is known to hold a value.
fn null_test_target(cond: &ast::Expr) -> (Option<String>, Option<String>) {
    if let ast::ExprKind::Binary(op, a, b) = &cond.kind {
        if matches!(op, ast::BinOp::Eq | ast::BinOp::Ne) {
            let name = match (&a.kind, &b.kind) {
                (ast::ExprKind::Ident(n), ast::ExprKind::Null) => Some(n.clone()),
                (ast::ExprKind::Null, ast::ExprKind::Ident(n)) => Some(n.clone()),
                _ => None,
            };
            if let Some(n) = name {
                return if *op == ast::BinOp::Ne {
                    (Some(n), None)
                } else {
                    (None, Some(n))
                };
            }
        }
    }
    (None, None)
}

/// A call into the UI interface (`abi/kiln_ui.h`) — plain C, not the slot ABI.
/// `kn_{lib}_component_{op}(…)` — a non-visual component's own entry point.
/// `core` is the runtime itself, which is how the linker knows it.
fn component_call(lib: &str, op: &str, args: Vec<Expr>, arg_tys: Vec<TyId>, ret: TyId) -> Expr {
    Expr::Call(Box::new(Call::Dll {
        library: if lib == "core" { "runtime".into() } else { lib.to_string() },
        symbol: format!("kn_{lib}_component_{op}"),
        conv: CallConv::Cdecl,
        args,
        arg_tys,
        ret,
        varargs: false,
    }))
}

fn ui_call(symbol: &str, args: Vec<Expr>, arg_tys: Vec<TyId>, ret: TyId) -> Expr {
    Expr::Call(Box::new(Call::Dll {
        library: "ui".into(),
        symbol: symbol.to_string(),
        conv: CallConv::Cdecl,
        args,
        arg_tys,
        ret,
        varargs: false,
    }))
}

/// `kn_ui_set_a11y(handle, role, name)` — what the accessibility tree reads.
fn ui_a11y(handle: Expr, role: i32, name: Expr) -> Expr {
    ui_call(
        "kn_ui_set_a11y",
        vec![handle, Expr::Int(role as i128, TyTable::I32), name],
        vec![TyTable::I64, TyTable::I32, TyTable::STR],
        TyTable::I32,
    )
}

/// `kn_ui_set(handle, name, value)` — every property crosses as text.
fn ui_set(handle: Expr, name: &str, value: Expr) -> Expr {
    ui_call(
        "kn_ui_set",
        vec![handle, Expr::Str(name.to_string()), value],
        vec![TyTable::I64, TyTable::STR, TyTable::STR],
        TyTable::I32,
    )
}

/// A designer property must be a literal; it is written into the binary as the
/// text the runtime sets.
fn literal_text(e: &ast::Expr) -> Result<String, String> {
    Ok(match &e.kind {
        ast::ExprKind::Str(s) => s.clone(),
        ast::ExprKind::Int(v) => v.to_string(),
        ast::ExprKind::Bool(b) => b.to_string(),
        ast::ExprKind::Float(v, _) => v.to_string(),
        _ => return Err("a designer property must be a literal".into()),
    })
}

/// A 1.x command signature's type, as a KIR type.
fn ir_ty(t: kiln_ir::Ty, cx: &mut Cx) -> TyId {
    use kiln_ir::Ty as T;
    match t {
        T::Int => TyTable::I32,
        T::Int64 => TyTable::I64,
        T::Double => TyTable::F64,
        T::Text => TyTable::STR,
        T::Bool => TyTable::BOOL,
        T::Bytes => TyTable::BYTES,
        T::Ptr => TyTable::PTR,
        T::Byte => TyTable::U8,
        T::Int16 => TyTable::I16,
        T::Float => TyTable::F32,
        T::Array(e) | T::Dict(e) | T::Optional(e) => {
            let inner = ir_ty(e.ty(), cx);
            cx.b.m.types.intern(TyKind::Array(inner))
        }
        // Signature-only and interop shapes cross as a pointer.
        _ => TyTable::PTR,
    }
}

/// `CharacterId` → `character_id`. The default column name for a field.
fn snake_case(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                out.push('_');
            }
            for l in c.to_lowercase() {
                out.push(l);
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// A zero/null value of a type — the unused `value` slot of a failed `Result`.
fn zero_of(tt: &TyTable, ty: TyId) -> Expr {
    match tt.kind(ty) {
        TyKind::F32 | TyKind::F64 => Expr::Float(0.0, ty),
        TyKind::Bool => Expr::Bool(false),
        TyKind::Str => Expr::Str(String::new()),
        k if matches!(
            k,
            TyKind::Ptr
                | TyKind::Bytes
                | TyKind::Record(_)
                | TyKind::Array(_)
                | TyKind::Dict(..)
                | TyKind::Set(_)
        ) =>
        {
            Expr::Null(ty)
        }
        _ => Expr::Int(0, ty),
    }
}

fn is_int_ty(tt: &TyTable, t: TyId) -> bool {
    matches!(
        tt.kind(t),
        TyKind::I8
            | TyKind::U8
            | TyKind::I16
            | TyKind::U16
            | TyKind::I32
            | TyKind::U32
            | TyKind::I64
            | TyKind::U64
            | TyKind::Nint
            | TyKind::Nuint
            | TyKind::Char
    )
}

fn cast_to(v: Expr, from: TyId, to: TyId) -> Expr {
    if from == to {
        v
    } else {
        Expr::Cast {
            value: Box::new(v),
            to,
        }
    }
}
