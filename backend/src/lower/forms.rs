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
    /// Render a property value literal as the text the UI layer expects.
    /// Values are textual at the D10 boundary in v0 (see abi/kiln_ui.h).
    pub(crate) fn property_text(&self, e: &Expr) -> Result<String, LowerError> {
        Ok(match e {
            Expr::TextLit(s) => s.clone(),
            Expr::IntLit(v) => v.to_string(),
            // A property written as a bit pattern (`width = 0x1E0`) is a
            // literal like any other; the UI layer wants the number.
            Expr::BitsLit(v) => kiln_ir::sema::bits_value(*v).to_string(),
            Expr::DoubleLit(v) => format!("{v}"),
            Expr::BoolLit(b) => b.to_string(),
            _ => return err("component property values must be literals in v0.2"),
        })
    }

    /// Assign each component its compile-time handle constant.
    ///
    /// Handles count from 1 in creation order, per library (abi/kiln_abi.h),
    /// which is exactly the order the create calls below are emitted in. The
    /// form root is the `ui` library's handle 1, so its children start at 2;
    /// every other library starts at 1. Two libraries' counters never meet,
    /// because a handle is only ever passed back to the entry points of the
    /// library that issued it.
    pub(crate) fn map_components(
        &mut self,
        form: Option<&kiln_ir::Form>,
        module_components: &[&Component],
    ) {
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
    pub(crate) fn owner(&self, id: &str) -> Option<String> {
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
    /// through `kn_ui_*`: that symmetry is the point of the `kind` field, and
    /// is why the inspector, the checker and the code preview need no new
    /// concepts to show a timer.
    pub(crate) fn build_component(&mut self, c: &Component) -> Result<(), LowerError> {
        let lib = self.owner(&c.id).ok_or_else(|| LowerError {
            msg: format!("unknown component type `{}`", c.type_name),
        })?;
        self.component_libs.insert(lib.clone());
        let type_op = self.cstr(&c.type_name);
        let handle = self.fresh();
        writeln!(
            self.body,
            "  {handle} = call i64 @kn_{lib}_component_create(ptr {type_op})"
        )
        .unwrap();
        for (name, value) in &c.properties {
            let text = self.property_text(value)?;
            self.set_property(Some(&lib), &handle, name, &text);
        }
        self.bind_handlers_to(Some(&lib), &c.type_name, &handle, &c.handlers);
        Ok(())
    }

    pub(crate) fn form_build(&mut self, form: &kiln_ir::Form) -> Result<(), LowerError> {
        // Window geometry/title come from the form's own properties.
        let mut title = "Kiln Application".to_string();
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
        self.ui_used.insert("kn_ui_init");
        writeln!(
            self.body,
            "  call i32 @kn_ui_init(ptr {title_op}, i32 {width}, i32 {height})"
        )
        .unwrap();

        // The root is always handle 1; children follow in creation order.
        self.ui_used.insert("kn_ui_root");
        let root_tmp = self.fresh();
        writeln!(self.body, "  {root_tmp} = call i64 @kn_ui_root()").unwrap();
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
            self.ui_used.insert("kn_ui_create");
            let handle = self.fresh();
            writeln!(
                self.body,
                "  {handle} = call i64 @kn_ui_create(i64 {root}, ptr {type_op})"
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
    pub(crate) fn loop_run(&mut self) {
        self.loop_used = true;
        let rc = self.fresh();
        writeln!(self.body, "  {rc} = call i32 @kn_loop_run()").unwrap();
        self.exit_code = Some(rc);
    }

    /// Start the event loop and tear down. Emitted after start-up code.
    pub(crate) fn form_run(&mut self) {
        self.ui_used.insert("kn_ui_run");
        let rc = self.fresh();
        writeln!(self.body, "  {rc} = call i32 @kn_ui_run()").unwrap();
        self.exit_code = Some(rc);
        self.ui_used.insert("kn_ui_shutdown");
        writeln!(self.body, "  call void @kn_ui_shutdown()").unwrap();
    }

    pub(crate) fn set_property(
        &mut self,
        lib: Option<&str>,
        handle: &str,
        name: &str,
        value: &str,
    ) {
        let n = self.cstr(name);
        let v = self.cstr(value);
        let f = self.setter(lib);
        writeln!(self.body, "  call i32 @{f}(i64 {handle}, ptr {n}, ptr {v})").unwrap();
    }

    /// The property setter for a component: the widget interface, or the
    /// declaring library's own.
    pub(crate) fn setter(&mut self, lib: Option<&str>) -> String {
        match lib {
            None => {
                self.ui_used.insert("kn_ui_set");
                "kn_ui_set".to_string()
            }
            Some(lib) => {
                self.component_libs.insert(lib.to_string());
                format!("kn_{lib}_component_set")
            }
        }
    }

    /// Record the a11y role with no accessible name (see the G8 note above).
    pub(crate) fn a11y_role_only(&mut self, handle: &str, role: i32) {
        self.ui_used.insert("kn_ui_set_a11y");
        writeln!(
            self.body,
            "  call i32 @kn_ui_set_a11y(i64 {handle}, i32 {role}, ptr null)"
        )
        .unwrap();
    }

    pub(crate) fn a11y(&mut self, handle: &str, role: i32, name: &str) {
        let n = self.cstr(name);
        self.ui_used.insert("kn_ui_set_a11y");
        writeln!(
            self.body,
            "  call i32 @kn_ui_set_a11y(i64 {handle}, i32 {role}, ptr {n})"
        )
        .unwrap();
    }

    /// Bind events to handler FUNCTION POINTERS (never names — G8).
    pub(crate) fn bind_handlers_to(
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
                    self.ui_used.insert("kn_ui_on");
                    "kn_ui_on".to_string()
                }
                Some(lib) => {
                    self.component_libs.insert(lib.to_string());
                    format!("kn_{lib}_component_on")
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
    pub(crate) fn handler_symbol(&mut self, type_name: &str, event: &str, sub: &str) -> String {
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
        let name = format!("kn_evt_{sub}_{shape}");
        if !self.thunks.contains_key(&name) {
            let decls = params
                .iter()
                .enumerate()
                .map(|(i, t)| format!("{} %a{i}", llvm_ty(*t)))
                .collect::<Vec<_>>()
                .join(", ");
            let takes = reg.sub(sub).is_some_and(|s| !s.params.is_empty());
            let args = if takes { decls.clone() } else { String::new() };
            // The thunk itself is called by the runtime, so it stays cdecl; the
            // call it makes to the handler carries the handler's convention.
            let cc = self.cc_prefix(self.sub_convs.get(sub).copied().flatten());
            let symbol = user_symbol(sub);
            self.thunks.insert(
                name.clone(),
                format!(
                    "define internal void @{name}({decls}) {{\nentry:\n  call {cc}void @{symbol}({args})\n  ret void\n}}\n\n",
                ),
            );
        }
        name
    }
}
