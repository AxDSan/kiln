//! `kiln edit`: tree-level edits to a form's designer block.
//!
//! Studio does not splice text. It asks the CLI to change the tree and print it
//! back, which is why the printer has to carry comments and be a fixed point:
//! everything the edit did not touch comes out exactly as it went in.
//!
//! This keeps the "the CLI is the only reader of a project file" rule from
//! `CLAUDE.md`, and extends it — the CLI is now the only *writer* too.

use crate::ast::*;
use crate::lexer::Span;
use crate::{parser, print};

/// One change to a form.
#[derive(Clone, Debug)]
pub enum Edit {
    /// Set a property on the form itself (`target` empty) or on a component.
    SetProperty {
        target: String,
        name: String,
        value: String,
    },
    /// Add a component to the form's designer block.
    AddComponent { type_name: String, id: String },
    /// Remove a component and everything wired to it.
    RemoveComponent { id: String },
    /// Wire an event to a method of the form.
    AddHandler {
        target: String,
        event: String,
        method: String,
    },
    /// Rename a component, updating nothing else — handlers are named
    /// separately, so this is the whole of it.
    RenameComponent { from: String, to: String },
}

/// Apply an edit to K2 source, returning the rewritten source.
pub fn apply(src: &str, edit: &Edit) -> Result<String, String> {
    let toks = crate::lexer::lex(src).map_err(|e| format!("{}:{}: {}", e.line, e.col, e.msg))?;
    let mut program =
        parser::parse(toks).map_err(|e| format!("{}:{}: {}", e.span.line, e.span.col, e.msg))?;

    // The designer's half is the first `form` block that has components or
    // properties; a `partial form` holding only code is left alone.
    let form = program
        .items
        .iter_mut()
        .filter_map(|i| match i {
            Item::Form(f) => Some(f),
            _ => None,
        })
        .max_by_key(|f| f.components.len() + f.properties.len())
        .ok_or_else(|| "this file declares no form".to_string())?;

    match edit {
        Edit::SetProperty {
            target,
            name,
            value,
        } => {
            let props = if target.is_empty() {
                &mut form.properties
            } else {
                &mut form
                    .components
                    .iter_mut()
                    .find(|c| &c.id == target)
                    .ok_or_else(|| format!("no component `{target}`"))?
                    .properties
            };
            let v = literal(value);
            match props.iter_mut().find(|(n, _)| n == name) {
                Some((_, slot)) => *slot = v,
                None => props.push((name.clone(), v)),
            }
        }
        Edit::AddComponent { type_name, id } => {
            if form.components.iter().any(|c| &c.id == id) {
                return Err(format!("`{id}` already exists"));
            }
            form.components.push(ComponentDecl {
                leading: Vec::new(),
                type_name: type_name.clone(),
                id: id.clone(),
                properties: Vec::new(),
                handlers: Vec::new(),
                span: Span::default(),
            });
        }
        Edit::RemoveComponent { id } => {
            let before = form.components.len();
            form.components.retain(|c| &c.id != id);
            if form.components.len() == before {
                return Err(format!("no component `{id}`"));
            }
        }
        Edit::RenameComponent { from, to } => {
            let c = form
                .components
                .iter_mut()
                .find(|c| &c.id == from)
                .ok_or_else(|| format!("no component `{from}`"))?;
            c.id = to.clone();
        }
        Edit::AddHandler {
            target,
            event,
            method,
        } => {
            let c = form
                .components
                .iter_mut()
                .find(|c| &c.id == target)
                .ok_or_else(|| format!("no component `{target}`"))?;
            if c.handlers.iter().any(|(e, _)| e == event) {
                return Err(format!("`{target}.{event}` is already wired"));
            }
            c.handlers
                .push((event.clone(), HandlerRef::Method(method.clone())));
        }
    }

    Ok(print::program(&program))
}

/// A property value as written in the designer block. Numbers and booleans stay
/// unquoted; everything else is a string, which is what the runtime takes.
fn literal(v: &str) -> Expr {
    let kind = if let Ok(i) = v.parse::<i128>() {
        ExprKind::Int(i)
    } else if v == "true" || v == "false" {
        ExprKind::Bool(v == "true")
    } else {
        ExprKind::Str(v.to_string())
    };
    Expr {
        kind,
        span: Span::default(),
    }
}
