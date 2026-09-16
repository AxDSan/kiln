//! An ergonomic builder for constructing KIR by hand — used by the Phase 1
//! exit fixtures, and the shape the front-end lowerers will target.
//!
//! It keeps the index bookkeeping (`FuncId`, `LocalId`, `RecordId`, `GlobalId`)
//! honest: each `add_*` returns the id it just assigned, so nothing has to
//! guess a vector position.

use crate::*;

pub struct ModuleBuilder {
    pub m: Module,
}

impl ModuleBuilder {
    pub fn new(name: &str, kind: ModuleKind, target: Target) -> ModuleBuilder {
        ModuleBuilder {
            m: Module::new(name, kind, target),
        }
    }

    pub fn intern(&mut self, k: TyKind) -> TyId {
        self.m.types.intern(k)
    }

    pub fn array(&mut self, elem: TyId) -> TyId {
        self.intern(TyKind::Array(elem))
    }
    pub fn optional(&mut self, inner: TyId) -> TyId {
        self.intern(TyKind::Optional(inner))
    }
    pub fn func_ty(&mut self, params: Vec<TyId>, ret: TyId) -> TyId {
        self.intern(TyKind::Func { params, ret })
    }
    pub fn tuple(&mut self, elems: Vec<TyId>) -> TyId {
        self.intern(TyKind::Tuple(elems))
    }

    /// A C-layout record. `offsets`/`size`/`align` are the front end's to compute;
    /// this helper lays fields out with natural alignment for the common case.
    pub fn c_record(
        &mut self,
        name: &str,
        fields: Vec<(&str, TyId)>,
        equality: Equality,
    ) -> RecordId {
        let id = RecordId(self.m.records.len() as u32);
        let mut offset = 0i64;
        let mut align = 1i64;
        let mut offsets = Vec::new();
        let defs: Vec<FieldDef> = fields
            .iter()
            .map(|(n, t)| {
                let sz = self.scalar_size(*t);
                offset = round_up(offset, sz);
                offsets.push(offset);
                offset += sz;
                align = align.max(sz);
                FieldDef {
                    name: (*n).to_string(),
                    ty: *t,
                }
            })
            .collect();
        let size = round_up(offset, align.max(1));
        self.m.records.push(RecordDef {
            id,
            name: name.to_string(),
            fields: defs,
            layout: Layout::C {
                size,
                align,
                offsets,
            },
            equality,
        });
        let _ = self.intern(TyKind::Record(id));
        id
    }

    fn scalar_size(&self, ty: TyId) -> i64 {
        match self.m.types.kind(ty) {
            TyKind::Bool | TyKind::I8 | TyKind::U8 => 1,
            TyKind::I16 | TyKind::U16 => 2,
            TyKind::I32 | TyKind::U32 | TyKind::Char | TyKind::F32 => 4,
            TyKind::I64 | TyKind::U64 | TyKind::F64 => 8,
            TyKind::Nint | TyKind::Nuint => (self.m.target.ptr_bits / 8) as i64,
            _ => (self.m.target.ptr_bits / 8) as i64, // pointers
        }
    }

    pub fn record_ty(&mut self, id: RecordId) -> TyId {
        self.intern(TyKind::Record(id))
    }

    pub fn add_global(&mut self, name: &str, ty: TyId, is_gc_root: bool) -> GlobalId {
        let id = GlobalId(self.m.globals.len() as u32);
        self.m.globals.push(GlobalDef {
            id,
            name: name.to_string(),
            ty,
            is_gc_root,
        });
        id
    }

    /// Reserve a function id so bodies can refer to functions defined later
    /// (recursion, mutual reference, a closure over a not-yet-added body).
    pub fn declare_func(&mut self, symbol: &str, params: Vec<(&str, TyId)>, ret: TyId) -> FuncId {
        self.func_full(
            symbol,
            params,
            ret,
            CallConv::Kiln,
            Linkage::Internal,
            false,
        )
    }

    pub fn func_full(
        &mut self,
        symbol: &str,
        params: Vec<(&str, TyId)>,
        ret: TyId,
        conv: CallConv,
        linkage: Linkage,
        synthetic: bool,
    ) -> FuncId {
        let id = FuncId(self.m.funcs.len() as u32);
        // Each parameter is also a local, spilled to a slot (is_arg).
        let mut locals = Vec::new();
        let param_defs: Vec<Param> = params
            .iter()
            .enumerate()
            .map(|(i, (n, t))| {
                locals.push(Local {
                    name: (*n).to_string(),
                    ty: *t,
                    span: Span::default(),
                    is_arg: Some(i),
                });
                Param {
                    name: (*n).to_string(),
                    ty: *t,
                    span: Span::default(),
                }
            })
            .collect();
        self.m.funcs.push(Func {
            id,
            symbol: symbol.to_string(),
            line: 0,
            file: None,
            params: param_defs,
            ret,
            conv,
            locals,
            body: Vec::new(),
            linkage,
            synthetic,
        });
        id
    }

    /// Add a plain local (not a parameter) to a function, returning its id.
    pub fn add_local(&mut self, func: FuncId, name: &str, ty: TyId) -> LocalId {
        let f = &mut self.m.funcs[func.0 as usize];
        let id = LocalId(f.locals.len() as u32);
        f.locals.push(Local {
            name: name.to_string(),
            ty,
            span: Span::default(),
            is_arg: None,
        });
        id
    }

    /// The local id of parameter `i` (params are the first locals).
    pub fn param_local(&self, i: usize) -> LocalId {
        LocalId(i as u32)
    }

    pub fn set_body(&mut self, func: FuncId, body: Vec<Stmt>) {
        self.m.funcs[func.0 as usize].body = body;
    }

    pub fn set_entry(&mut self, func: FuncId) {
        self.m.entry = Some(func);
    }

    pub fn build(self) -> Module {
        self.m
    }
}

fn round_up(x: i64, align: i64) -> i64 {
    if align <= 1 {
        x
    } else {
        (x + align - 1) / align * align
    }
}
