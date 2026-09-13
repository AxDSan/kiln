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
    let mut b = ModuleBuilder::new(
        p.namespace.as_deref().unwrap_or("program"),
        ModuleKind::Console,
        Target::X86_64_LINUX,
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
                }
                cx.tables
                    .insert(td.name.clone(), TableInfo { table, columns });
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

    // Pass 2: fill record fields and compute C layout.
    for item in &p.items {
        if let ast::Item::Type(td) = item {
            if td.kind == ast::TypeKind::StaticClass || !td.type_params.is_empty() {
                continue;
            }
            let rid = cx.type_ids[&td.name];
            let mut fields = Vec::new();
            for f in td.record_params.iter().chain(td.fields.iter()) {
                let ty = cx.resolve(&f.ty)?;
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
                    let dll = m.attrs.iter().find(|a| a.name == "Dll");
                    let Some(dll) = dll else {
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
                    cx.dlls
                        .insert(format!("{}.{}", td.name, m.name), sig.clone());
                    cx.dlls.entry(m.name.clone()).or_insert(sig);
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
                let sig = Sig {
                    fid,
                    this: this.is_some(),
                    params: owned.iter().map(|(_, t)| *t).collect(),
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
    if let Some(f) = &form {
        cx.b.m.kind = ModuleKind::Gui;
        for c in &f.components {
            let g =
                cx.b.add_global(&format!("{}__{}", f.name, c.id), TyTable::I64, false);
            cx.components.insert(c.id.clone(), (g, c.type_name.clone()));
        }
        for fld in &f.fields {
            let ty = cx.resolve(&fld.ty)?;
            let g = cx.b.add_global(
                &format!("{}__state_{}", f.name, fld.name),
                ty,
                cx.b.m.types.is_pointer(ty),
            );
            cx.form_state.insert(fld.name.clone(), (g, ty));
        }
        // Declare the handlers first so the build sequence can bind them.
        for m in &f.methods {
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
                ret,
            };
            cx.methods
                .insert(format!("{}.{}", f.name, m.name), sig.clone());
            cx.methods.entry(m.name.clone()).or_insert(sig);
        }
        for m in &f.methods {
            let sig = cx.methods[&format!("{}.{}", f.name, m.name)].clone();
            cx.lower_into(sig.fid, m, false, None, None)?;
        }
        let build =
            cx.b.declare_func(&format!("{}_Build", f.name), vec![], TyTable::I32);
        cx.lower_form(build, f)?;
        cx.b.set_entry(build);
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
        cx.b.set_entry(main);
    } else if let Some(sig) = cx.methods.get("Main").cloned() {
        cx.b.set_entry(sig.fid);
    }

    // Every generic instance and lifted lambda queued by *any* of the above —
    // method bodies and top-level code alike — gets its body here. This must
    // run after the last direct lowering, or an instance would be declared and
    // left empty.
    cx.drain_pending()?;

    Ok(cx.b.build())
}

#[derive(Clone)]
struct Sig {
    fid: FuncId,
    this: bool,
    params: Vec<TyId>,
    ret: TyId,
}

/// A record mapped to a database table by `[Table]`.
#[derive(Clone)]
struct TableInfo {
    table: String,
    /// `(field name, column name, is_auto)` in declaration order.
    columns: Vec<(String, String, bool)>,
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
        let n = self.b.m.records.len();
        let rid = self.b.c_record(
            &format!("$Dict{n}"),
            vec![
                ("len", TyTable::I32),
                ("cap", TyTable::I32),
                ("keys", keys_ty),
                ("values", vals_ty),
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
        let n = self.b.m.records.len();
        let rid = self.b.c_record(
            &format!("$Set{n}"),
            vec![
                ("len", TyTable::I32),
                ("cap", TyTable::I32),
                ("data", data_ty),
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

    fn c_layout(&self, fields: &[FieldDef]) -> (i64, i64, Vec<i64>) {
        let mut offset = 0i64;
        let mut align = 1i64;
        let mut offsets = Vec::new();
        for f in fields {
            let sz = self.scalar_size(f.ty);
            offset = round_up(offset, sz);
            offsets.push(offset);
            offset += sz;
            align = align.max(sz);
        }
        (round_up(offset, align.max(1)), align, offsets)
    }

    pub(crate) fn scalar_size(&self, ty: TyId) -> i64 {
        match self.b.m.types.kind(ty) {
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
            let (val, _) = fl.expr(e, Some(ret))?;
            fl.push(Stmt::Return(Some(val)));
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
                let target = Expr::FuncPtr(fid);
                fl.push(Stmt::Expr(ui_call(
                    "kn_ui_on",
                    vec![Expr::Global(g), Expr::Str(snake_case(event)), target],
                    vec![TyTable::I64, TyTable::STR, TyTable::PTR],
                    TyTable::I32,
                )));
            }
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
        match &s.kind {
            ast::StmtKind::Local {
                name, ty, value, ..
            } => {
                let hint = match ty {
                    Some(t) => Some(self.cx.resolve(t)?),
                    None => None,
                };
                let (val, vty) = self.expr(value, hint)?;
                let lty = hint.unwrap_or(vty);
                self.declare_var(name, lty, val)?;
            }
            ast::StmtKind::Assign { target, op, value } => {
                // `count.Text = "..."` sets a component property at run time.
                if let ast::ExprKind::Member(recv, prop) = &target.kind {
                    if let ast::ExprKind::Ident(id) = &recv.kind {
                        if let Some(g) = self.cx.components.get(id).map(|(g, _)| *g) {
                            if *op != ast::AssignOp::Eq {
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
                            self.push(Stmt::Expr(ui_set(Expr::Global(g), &snake_case(prop), text)));
                            return Ok(());
                        }
                    }
                }
                // `d[k] = v` updates in place, or appends when the key is new.
                if let ast::ExprKind::Index(base, key) = &target.kind {
                    let (dv, dty) = self.expr_raw(base, None)?;
                    if let Some((_, kty, vty)) = self.cx.as_dict(dty) {
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
                        return Ok(());
                    }
                }
                let place = self.place(target)?;
                let pty = self.place_ty(target)?;
                let (rhs, _) = self.expr(value, Some(pty))?;
                let value = if *op == ast::AssignOp::Eq {
                    rhs
                } else {
                    let cur = self.expr(target, Some(pty))?.0;
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
                let (val, ty) = self.expr(e, None)?;
                // keep the call for its effect; a bare value is dropped
                if ty == TyTable::VOID {
                    self.push(Stmt::Expr(val));
                } else {
                    self.push(Stmt::Expr(val));
                }
            }
            ast::StmtKind::Return(v) => match v {
                None => {
                    self.emit_defers_to(0);
                    self.push(Stmt::Return(None))
                }
                Some(e) => {
                    let (val, vty) = self.expr(e, Some(self.ret))?;
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
                let (c, _) = self.expr(cond, Some(TyTable::BOOL))?;
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
                let (c, _) = self.expr(cond, Some(TyTable::BOOL))?;
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
                    Some(c) => self.expr(c, Some(TyTable::BOOL))?.0,
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
            ast::StmtKind::ForEach { var, coll, body } => {
                self.lower_foreach(var, coll, body)?;
            }
            ast::StmtKind::Break => {
                let floor = self.loop_frames.last().copied().unwrap_or(0);
                self.emit_defers_to(floor);
                self.push(Stmt::Break)
            }
            ast::StmtKind::Continue => {
                let floor = self.loop_frames.last().copied().unwrap_or(0);
                self.emit_defers_to(floor);
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
                self.scope.insert(var.to_string(), (item, elem));
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
        self.scope.insert(var.to_string(), (iv, ity));
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
            if !newline {
                return Err(
                    "Console.Write is not available against the Kiln runtime yet — \
                     print_text writes a whole line"
                        .into(),
                );
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
            TyKind::Str => ("%s", v, TyTable::STR),
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
            TyKind::Bool => (
                "%d",
                Expr::Cast {
                    value: Box::new(v),
                    to: TyTable::I32,
                },
                TyTable::I32,
            ),
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
                arg_tys: vec![TyTable::STR, TyTable::I64, TyTable::STR],
                ret: TyTable::I32,
                varargs: true,
            }))
        };
        // n = snprintf(null, 0, fmt, ...)
        let mut measure = vec![
            Expr::Null(TyTable::STR),
            Expr::Int(0, TyTable::I64),
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
            to: TyTable::I64,
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
                to: TyTable::I64,
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
        let (v, ty) = self.expr_raw(e, hint)?;
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
                            Box::new(i),
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
                // `list[i]` reads the buffer; positions are 1-based.
                if let Some((_, elem)) = self.cx.as_list(bty) {
                    let (i, _) = self.expr(idx, Some(TyTable::I32))?;
                    let i0 = Expr::Bin(
                        BinOp::Sub,
                        Box::new(i),
                        Box::new(Expr::Int(1, TyTable::I32)),
                        TyTable::I32,
                    );
                    return Ok((
                        Expr::Index(Box::new(Expr::Field(Box::new(b), LIST_DATA)), Box::new(i0)),
                        elem,
                    ));
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
                    let (fb, _) = self.expr(b, Some(inner))?;
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
                let (fb, _) = self.expr(b, Some(val_ty))?;
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
            if member == "Size" && self.cx.packed.contains(obj) {
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
        if self.cx.as_dict(bty).is_some() {
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
                    self.list_add(holder, sty, Expr::Local(vh));
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
                        self.query_sql(body, &param, &info, &mut where_sql)?;
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
                    let size = Expr::Cast {
                        value: Box::new(n),
                        to: TyTable::I64,
                    };
                    return Ok((self.alloc(size, TyTable::BYTES), TyTable::BYTES));
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
            // `list.Where(pred)` / `list.Select(f)` — written here rather than
            // in K2 until the standard library exists.
            if name == "Where" || name == "Select" {
                let (lv, lty) = self.expr_raw(recv, None)?;
                if self.cx.as_list(lty).is_some() {
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
                        for (a, pty) in args.iter().zip(params.iter()) {
                            kargs.push(self.expr(a, Some(*pty))?.0);
                        }
                        let slots = params
                            .iter()
                            .zip(tags.iter())
                            .map(|(ty, tag)| SlotTy { tag: *tag, ty: *ty })
                            .collect();
                        return Ok((
                            Expr::Call(Box::new(Call::Command {
                                symbol: sym,
                                args: kargs,
                                arg_slots: slots,
                                ret,
                            })),
                            ret,
                        ));
                    }
                }
            }
        }
        let (key, this_arg): (String, Option<Expr>) = match &callee.kind {
            ast::ExprKind::Ident(name) => (name.clone(), None),
            ast::ExprKind::Member(recv, name) => {
                if let ast::ExprKind::Ident(obj) = &recv.kind {
                    let qualified = format!("{obj}.{name}");
                    if (self.cx.methods.contains_key(&qualified)
                        || self.cx.generics.contains_key(&qualified)
                        || self.cx.dlls.contains_key(&qualified))
                        && self.is_type_name(obj)
                    {
                        (qualified, None)
                    } else {
                        // instance call: prefer the receiver's own method.
                        let (recv_v, rty) = self.expr(recv, None)?;
                        let key = self
                            .cx
                            .record_name(rty)
                            .map(|r| format!("{r}.{name}"))
                            .filter(|k| self.cx.methods.contains_key(k))
                            .unwrap_or_else(|| name.clone());
                        (key, Some(recv_v))
                    }
                } else {
                    let (recv_v, rty) = self.expr(recv, None)?;
                    let key = self
                        .cx
                        .record_name(rty)
                        .map(|r| format!("{r}.{name}"))
                        .filter(|k| self.cx.methods.contains_key(k))
                        .unwrap_or_else(|| name.clone());
                    (key, Some(recv_v))
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
            for (a, pty) in args.iter().zip(d.params.iter()) {
                kargs.push(self.expr(a, Some(*pty))?.0);
            }
            return Ok((
                Expr::Call(Box::new(Call::Dll {
                    library: d.library,
                    symbol: d.symbol,
                    conv: d.conv,
                    args: kargs,
                    arg_tys: d.params.clone(),
                    ret: d.ret,
                    varargs: false,
                })),
                d.ret,
            ));
        }
        // A generic method: lower the arguments first (their types are what the
        // type parameters are inferred from), then instantiate.
        if self.cx.generics.contains_key(&key) {
            let mut lowered = Vec::new();
            for a in args {
                lowered.push(self.expr(a, None)?);
            }
            let sig = self.instantiate(&key, &lowered)?;
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
            let recv_ty = self.expr_ty_of(this);
            if let Some((sym, params, ret, tags)) =
                self.lookup_instance_command(recv_ty, member, args.len())
            {
                let mut kargs = vec![this.clone()];
                for (a, pty) in args.iter().zip(params.iter().skip(1)) {
                    kargs.push(self.expr(a, Some(*pty))?.0);
                }
                let slots = params
                    .iter()
                    .zip(tags.iter())
                    .map(|(ty, tag)| SlotTy { tag: *tag, ty: *ty })
                    .collect();
                return Ok((
                    Expr::Call(Box::new(Call::Command {
                        symbol: sym,
                        args: kargs,
                        arg_slots: slots,
                        ret,
                    })),
                    ret,
                ));
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
        for (a, pty) in args.iter().zip(sig.params.iter()) {
            kargs.push(self.expr(a, Some(*pty))?.0);
        }
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
        let (lrid, elem) = self
            .cx
            .as_list(lty)
            .or_else(|| self.cx.as_set(lty))
            .expect("a list or set");
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
    fn query_sql(
        &mut self,
        e: &ast::Expr,
        param: &str,
        info: &TableInfo,
        out: &mut String,
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
                self.query_sql(a, param, info, out)?;
                out.push_str(&format!(" {sym} "));
                self.query_sql(b, param, info, out)?;
                out.push(')');
                Ok(())
            }
            E::Unary(ast::UnOp::Not, inner) => {
                out.push_str("not ");
                self.query_sql(inner, param, info, out)
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
            E::Str(s) => {
                // A literal string is still parameterised, never interpolated.
                let _ = s;
                out.push('?');
                Ok(())
            }
            // Anything captured from outside becomes a bound parameter.
            E::Ident(_) => {
                out.push('?');
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
        let mut kargs = Vec::new();
        for (a, pty) in args.iter().zip(sig.params.iter()) {
            kargs.push(self.expr(a, Some(*pty))?.0);
        }
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
                arg_tys: vec![TyTable::I64],
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
                        to: TyTable::I64,
                    },
                ],
                arg_tys: vec![TyTable::PTR, TyTable::I64],
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
            args: vec![dst, src, Expr::Int(n as i128, TyTable::I64)],
            arg_tys: vec![TyTable::PTR, TyTable::PTR, TyTable::I64],
            ret: TyTable::PTR,
            varargs: false,
        }))
    }

    /// The address of byte `offset` in a buffer (offsets are 0-based).
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
            Box::new(offset),
        )
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

    /// Scan a set for the value held in `$item`, yielding its index or -1.
    fn set_find(&mut self, holder: LocalId, vh: LocalId, elem: TyId) -> Result<LocalId, String> {
        let found = self.new_local("$sfound", TyTable::I32);
        self.push(Stmt::Let {
            local: found,
            value: Expr::Int(-1, TyTable::I32),
        });
        let i = self.new_local("$si", TyTable::I32);
        self.push(Stmt::Let {
            local: i,
            value: Expr::Int(0, TyTable::I32),
        });
        let cur = Expr::Index(
            Box::new(Expr::Field(Box::new(Expr::Local(holder)), LIST_DATA)),
            Box::new(Expr::Local(i)),
        );
        let eq = self.key_equal(cur, Expr::Local(vh), elem);
        let body = vec![
            Stmt::If {
                cond: Expr::Not(Box::new(Expr::Bin(
                    BinOp::Lt,
                    Box::new(Expr::Local(i)),
                    Box::new(Expr::Field(Box::new(Expr::Local(holder)), LIST_LEN)),
                    TyTable::I32,
                ))),
                then: vec![Stmt::Break],
                els: vec![],
            },
            Stmt::If {
                cond: eq,
                then: vec![
                    Stmt::Assign {
                        place: Place::Local(found),
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
        Ok(found)
    }

    /// Scan a dictionary for `key`, yielding a local holding its index or -1.
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
        let i = self.new_local("$di", TyTable::I32);
        self.push(Stmt::Let {
            local: i,
            value: Expr::Int(0, TyTable::I32),
        });
        let cur = Expr::Index(
            Box::new(Expr::Field(Box::new(Expr::Local(holder)), DICT_KEYS)),
            Box::new(Expr::Local(i)),
        );
        let eq = self.key_equal(cur, Expr::Local(k), kty);
        let body = vec![
            Stmt::If {
                cond: Expr::Not(Box::new(Expr::Bin(
                    BinOp::Lt,
                    Box::new(Expr::Local(i)),
                    Box::new(Expr::Field(Box::new(Expr::Local(holder)), DICT_LEN)),
                    TyTable::I32,
                ))),
                then: vec![Stmt::Break],
                els: vec![],
            },
            Stmt::If {
                cond: eq,
                then: vec![
                    Stmt::Assign {
                        place: Place::Local(found),
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
        Ok(found)
    }

    /// Grow both buffers and append a key/value pair.
    fn dict_append(&mut self, holder: LocalId, dty: TyId, key: Expr, val: Expr) {
        let (drid, kty, vty) = self.cx.as_dict(dty).expect("a dictionary");
        let d = || Expr::Local(holder);
        let len = || Expr::Field(Box::new(d()), DICT_LEN);
        let cap = || Expr::Field(Box::new(d()), DICT_CAP);
        let ksize = self.cx.scalar_size(kty);
        let vsize = self.cx.scalar_size(vty);
        let keys_ty = self.cx.b.m.record(drid).fields[DICT_KEYS].ty;
        let vals_ty = self.cx.b.m.record(drid).fields[DICT_VALUES].ty;
        let newcap = self.new_local("$dcap", TyTable::I32);
        let realloc = |s: &Self, buf: Expr, elem: i64, cap: Expr, ret: TyId| {
            s.realloc(
                buf,
                Expr::Bin(
                    BinOp::Mul,
                    Box::new(cap),
                    Box::new(Expr::Int(elem as i128, TyTable::I32)),
                    TyTable::I32,
                ),
                ret,
            )
        };
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
                place: Place::Field(Box::new(d()), DICT_KEYS),
                value: realloc(
                    self,
                    Expr::Field(Box::new(d()), DICT_KEYS),
                    ksize,
                    Expr::Local(newcap),
                    keys_ty,
                ),
            },
            Stmt::Assign {
                place: Place::Field(Box::new(d()), DICT_VALUES),
                value: realloc(
                    self,
                    Expr::Field(Box::new(d()), DICT_VALUES),
                    vsize,
                    Expr::Local(newcap),
                    vals_ty,
                ),
            },
            Stmt::Assign {
                place: Place::Field(Box::new(d()), DICT_CAP),
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
                Box::new(Expr::Field(Box::new(d()), DICT_KEYS)),
                Box::new(len()),
            ),
            value: key,
        });
        self.push(Stmt::Assign {
            place: Place::Index(
                Box::new(Expr::Field(Box::new(d()), DICT_VALUES)),
                Box::new(len()),
            ),
            value: val,
        });
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

    /// `Where` keeps the elements a predicate accepts; `Select` maps each one.
    /// The result type of a `Select` comes from probing the lambda body.
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
                return Err(format!(
                    "this lambda captures `{name}`, which is a loop or pattern variable; \
                     capturing those is not supported yet"
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

    /// Monomorphise a generic method for the argument types at this call site.
    ///
    /// Type parameters are inferred by matching each declared parameter type
    /// against the lowered argument's type; the instance is cached, so the same
    /// type arguments produce one function with one mangled symbol.
    fn instantiate(&mut self, key: &str, args: &[(Expr, TyId)]) -> Result<Sig, String> {
        let t = self.cx.generics[key].clone();
        let m = &t.method;
        if args.len() != m.params.len() {
            return Err(format!(
                "`{key}` expects {} argument(s), got {}",
                m.params.len(),
                args.len()
            ));
        }
        // Infer.
        let mut tvars: HashMap<String, TyId> = HashMap::new();
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
        // `where T : I` — the type argument must actually implement I.
        for (tp, iface) in &m.constraints {
            let Some(bound) = tvars.get(tp) else { continue };
            if !self.cx.interfaces.contains_key(iface) {
                continue; // not an interface constraint (`class`, `new()`, …)
            }
            let ok = self
                .cx
                .record_name(*bound)
                .map(|n| self.cx.impls.contains_key(&(n, iface.clone())))
                .unwrap_or(false);
            if !ok {
                let shown = self
                    .cx
                    .record_name(*bound)
                    .unwrap_or_else(|| format!("{:?}", self.cx.b.m.types.kind(*bound)));
                return Err(format!(
                    "`{key}` needs `{tp}` to implement `{iface}`, and `{shown}` does not"
                ));
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
            return Ok((
                Expr::MakeRecord(
                    srid,
                    vec![
                        Expr::Int(0, TyTable::I32),
                        Expr::Int(0, TyTable::I32),
                        Expr::Null(data_ty),
                    ],
                ),
                ty,
            ));
        }
        // `new Dictionary<K,V>()` starts empty; buffers grow on first set.
        if let Some((drid, _, _)) = self.cx.as_dict(ty) {
            let keys_ty = self.cx.b.m.record(drid).fields[DICT_KEYS].ty;
            let vals_ty = self.cx.b.m.record(drid).fields[DICT_VALUES].ty;
            return Ok((
                Expr::MakeRecord(
                    drid,
                    vec![
                        Expr::Int(0, TyTable::I32),
                        Expr::Int(0, TyTable::I32),
                        Expr::Null(keys_ty),
                        Expr::Null(vals_ty),
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
            let mut kargs = Vec::new();
            for (a, pty) in args.iter().zip(sig.params.iter()) {
                kargs.push(self.expr(a, Some(*pty))?.0);
            }
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
        if !args.is_empty() {
            // positional
            for (a, (_, fty)) in args.iter().zip(field_tys.iter()) {
                values.push(self.expr(a, Some(*fty))?.0);
            }
        } else {
            // object initialiser: default missing to zero
            for (fname, fty) in &field_tys {
                if let Some((_, e)) = inits.iter().find(|(n, _)| n == fname) {
                    values.push(self.expr(e, Some(*fty))?.0);
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
        let (cond, _) = self.expr(c, Some(TyTable::BOOL))?;
        let (av, aty) = self.expr(a, hint)?;
        let (bv, _) = self.expr(b, hint.or(Some(aty)))?;
        let t = self.new_local("$tern", aty);
        self.push(Stmt::If {
            cond,
            then: vec![Stmt::Assign {
                place: Place::Local(t),
                value: av,
            }],
            els: vec![Stmt::Assign {
                place: Place::Local(t),
                value: bv,
            }],
        });
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
            let (bv, _) = self.expr(b, Some(TyTable::BOOL))?;
            let assign = Stmt::Assign {
                place: Place::Local(t),
                value: bv,
            };
            if op == ast::BinOp::And {
                self.push(Stmt::If {
                    cond: Expr::Local(t),
                    then: vec![assign],
                    els: vec![],
                });
            } else {
                self.push(Stmt::If {
                    cond: Expr::Not(Box::new(Expr::Local(t))),
                    then: vec![assign],
                    els: vec![],
                });
            }
            return Ok((Expr::Local(t), TyTable::BOOL));
        }

        let kop = match op {
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
        if op == ast::BinOp::Add {
            let probe = self.expr_raw(a, None)?;
            if probe.1 == TyTable::STR {
                let rhs = self.expr(b, Some(TyTable::STR))?;
                return Ok(self.build_string(vec![
                    (String::new(), Some(probe)),
                    (String::new(), Some(rhs)),
                ]));
            }
            // not a string: fall through, re-lowering `a` with the real hint
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
        let operand_ty = if aty != TyTable::BOOL { aty } else { bty };
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
                let (base, bty) = self.expr(recv, None)?;
                if let TyKind::Record(rid) = *self.tt().kind(bty) {
                    let rec = self.cx.b.m.record(rid);
                    if let Some(idx) = rec.fields.iter().position(|f| f.name == *member) {
                        return Ok(Place::Field(Box::new(base), idx));
                    }
                }
                Err(format!("cannot assign to member `{member}`"))
            }
            ast::ExprKind::Index(base, idx) => {
                let (b, _) = self.expr(base, None)?;
                let (i, _) = self.expr(idx, Some(TyTable::I32))?;
                let i0 = Expr::Bin(
                    BinOp::Sub,
                    Box::new(i),
                    Box::new(Expr::Int(1, TyTable::I32)),
                    TyTable::I32,
                );
                Ok(Place::Index(Box::new(b), Box::new(i0)))
            }
            _ => Err("invalid assignment target".into()),
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
        E::Unary(_, a) | E::Cast(_, a) | E::Try(a) => collect_lambdas_expr(a, out),
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
        E::Unary(_, a) | E::Cast(_, a) | E::Try(a) => collect_idents_expr(a, out),
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
