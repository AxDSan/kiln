#![allow(unused_imports)]
//! Part of the backend lowering, split out of `lib.rs` (Phase 1). This is the
//! same `impl Lowerer<'_>` methods as before, moved verbatim; no behaviour
//! change.
use crate::*;
use kiln_ir::registry::{ComponentKind, DllSig};
use kiln_ir::sema::resolve_ret;
use kiln_ir::{
    BinOp, BitOp, CallConv, CmpOp, Component, Elem, Expr, LogicalOp, Module, Registry, Signature,
    TargetInfo, Ty,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;

impl Lowerer<'_> {
    /// Everything except the entry point. A library stops here.
    pub(crate) fn finish_library(self, module_name: &str, functions: &str) -> String {
        self.finish_with(module_name, functions, false)
    }

    pub(crate) fn finish(self, module_name: &str, functions: &str) -> String {
        self.finish_with(module_name, functions, true)
    }

    pub(crate) fn finish_with(self, module_name: &str, functions: &str, entry: bool) -> String {
        let mut out = String::new();
        writeln!(
            out,
            "; Kiln-generated LLVM IR — module `{module_name}` (Phase 2, slot ABI)"
        )
        .unwrap();
        writeln!(out, "; Do not edit; regenerate from the .kiln source.\n").unwrap();

        // The slot type mirrors Kiln_Slot (abi/kiln_abi.h): {tag, pad, value}.
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

        // The collector's root table: the addresses of the pointer-typed
        // module variables. Only in an executable — a library target has no
        // `main`, so nothing there records where the stack starts and the
        // collector stays off.
        let roots = self.gc_root_symbols();
        if entry && !roots.is_empty() {
            let items: Vec<String> = roots.iter().map(|r| format!("ptr @{r}")).collect();
            writeln!(
                out,
                "@kn_gc_roots = internal global [{} x ptr] [{}]\n",
                roots.len(),
                items.join(", ")
            )
            .unwrap();
        }
        if entry {
            writeln!(out, "declare void @kn_gc_set_roots(ptr, i32)\n").unwrap();
        }

        // One address cache per foreign function called. `null` means "not yet
        // resolved"; `kn_dll_get` fills it on the first call.
        for name in &self.dll_cached {
            writeln!(
                out,
                "@{} = internal global ptr null",
                dll_cache_symbol(name)
            )
            .unwrap();
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

        // UI interface declarations (abi/kiln_ui.h), only when referenced.
        for sym in &self.ui_used {
            let decl = match *sym {
                "kn_ui_init" => "declare i32 @kn_ui_init(ptr, i32, i32)",
                "kn_ui_shutdown" => "declare void @kn_ui_shutdown()",
                "kn_ui_root" => "declare i64 @kn_ui_root()",
                "kn_ui_create" => "declare i64 @kn_ui_create(i64, ptr)",
                "kn_ui_set" => "declare i32 @kn_ui_set(i64, ptr, ptr)",
                "kn_ui_get" => "declare ptr @kn_ui_get(i64, ptr)",
                "kn_ui_get_int" => "declare i32 @kn_ui_get_int(i64, ptr)",
                "kn_ui_on" => "declare i32 @kn_ui_on(i64, ptr, ptr)",
                "kn_ui_set_a11y" => "declare i32 @kn_ui_set_a11y(i64, i32, ptr)",
                "kn_ui_run" => "declare i32 @kn_ui_run()",
                other => panic!("undeclared UI symbol {other}"),
            };
            writeln!(out, "{decl}").unwrap();
        }
        if !self.ui_used.is_empty() {
            out.push('\n');
        }

        // A library's own component entry points (abi/kiln_abi.h). All five
        // are declared together rather than tracked one at a time: a `declare`
        // that nothing calls costs nothing, and the five are the whole of what
        // addressing a component means.
        for lib in &self.component_libs {
            writeln!(out, "declare i64 @kn_{lib}_component_create(ptr)").unwrap();
            writeln!(out, "declare i32 @kn_{lib}_component_set(i64, ptr, ptr)").unwrap();
            writeln!(out, "declare ptr @kn_{lib}_component_get(i64, ptr)").unwrap();
            writeln!(out, "declare i32 @kn_{lib}_component_get_int(i64, ptr)").unwrap();
            writeln!(out, "declare i32 @kn_{lib}_component_on(i64, ptr, ptr)").unwrap();
        }
        if !self.component_libs.is_empty() {
            out.push('\n');
        }

        if self.loop_used {
            writeln!(out, "declare i32 @kn_loop_run()\n").unwrap();
        }

        for sym in &self.aggr_used {
            let decl = match *sym {
                "kn_ary_new" => "declare ptr @kn_ary_new(i32, i32)",
                "kn_ary_get" => "declare i64 @kn_ary_get(ptr, i32)",
                "kn_ary_set" => "declare void @kn_ary_set(ptr, i32, i64)",
                "kn_bin_at" => "declare i32 @kn_bin_at(ptr, i32)",
                "kn_bin_set" => "declare void @kn_bin_set(ptr, i32, i32)",
                "kn_rec_new" => "declare ptr @kn_rec_new(i32)",
                "kn_rec_get" => "declare i64 @kn_rec_get(ptr, i32)",
                "kn_rec_set" => "declare void @kn_rec_set(ptr, i32, i64)",
                "kn_dict_new" => "declare ptr @kn_dict_new(i32)",
                "kn_dict_at" => "declare i64 @kn_dict_at(ptr, ptr)",
                "kn_dict_put" => "declare void @kn_dict_put(ptr, ptr, i64)",
                other => panic!("undeclared aggregate symbol {other}"),
            };
            writeln!(out, "{decl}").unwrap();
        }
        if !self.aggr_used.is_empty() {
            out.push('\n');
        }

        // The runtime notification channel (abi/kiln_abi.h), used to abort
        // with a message. Declared only when something actually aborts.
        if self.needs_notify {
            writeln!(out, "declare ptr @kn_notify(i32, ptr, ptr)\n").unwrap();
        }

        // The error slot's reset (runtime/kn_error.c). An optional's initializer
        // clears it before the call it is reading, so that what it reads back is
        // that call's verdict and not an older failure's.
        if self.needs_error_clear {
            writeln!(out, "declare void @kn_error_clear()\n").unwrap();
        }

        // The foreign-function loader (runtime/kn_dll.c): resolve-and-cache a
        // symbol, and copy a returned C string into a runtime-owned text.
        if self.needs_dll_get {
            writeln!(out, "declare ptr @kn_dll_get(ptr, ptr, ptr)").unwrap();
        }
        if self.needs_dll_text {
            writeln!(out, "declare ptr @kn_dll_text(ptr)").unwrap();
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
