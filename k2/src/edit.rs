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
    /// Replace the designer block's properties, components and wiring with
    /// exactly what is described, leaving the code half and everything else
    /// alone. This is what a designer's "save" is: it holds the whole form and
    /// has no record of which single edit got it there.
    Sync(FormSpec),
}

/// A form as the designer holds it, in the spelling `kiln inspect` prints —
/// which is what makes this the inverse of that command rather than a second
/// format nobody else speaks.
#[derive(Debug, Default, Clone)]
pub struct FormSpec {
    pub properties: Vec<(String, String)>,
    pub components: Vec<ComponentSpec>,
}

#[derive(Debug, Default, Clone)]
pub struct ComponentSpec {
    pub id: String,
    pub type_name: String,
    pub properties: Vec<(String, String)>,
    pub handlers: Vec<(String, String)>,
}

/// Read a form description in `kiln inspect`'s own lines.
///
/// `form:` starts it, then `prop: <id> <name> <value>`, `component: <id>
/// <type>` and `handler: <id> <event> <method>` in any order. A line that is
/// not one of those is ignored, so the whole of an `inspect` listing can be
/// handed back without filtering it first.
pub fn parse_spec(text: &str) -> Result<FormSpec, String> {
    let mut spec = FormSpec::default();
    let mut form_id = String::new();
    for line in text.lines() {
        let line = line.trim_end();
        let Some((kind, rest)) = line.split_once(": ") else {
            continue;
        };
        match kind {
            "form" => {
                form_id = rest.split_whitespace().next().unwrap_or("").to_string();
            }
            "component" => {
                let mut w = rest.split_whitespace();
                let (Some(id), Some(ty)) = (w.next(), w.next()) else {
                    return Err(format!("a `component:` line needs an id and a type: {line}"));
                };
                spec.components.push(ComponentSpec {
                    id: id.to_string(),
                    type_name: ty.to_string(),
                    ..Default::default()
                });
            }
            "prop" => {
                let mut w = rest.splitn(3, ' ');
                let (Some(id), Some(name)) = (w.next(), w.next()) else {
                    return Err(format!("a `prop:` line needs an id and a name: {line}"));
                };
                let value = unescape(w.next().unwrap_or(""));
                if id == form_id {
                    spec.properties.push((name.to_string(), value));
                } else if let Some(c) = spec.components.iter_mut().find(|c| c.id == id) {
                    c.properties.push((name.to_string(), value));
                } else {
                    return Err(format!("`prop:` for unknown component `{id}`"));
                }
            }
            "handler" => {
                let mut w = rest.split_whitespace();
                let (Some(id), Some(ev), Some(m)) = (w.next(), w.next(), w.next()) else {
                    return Err(format!("a `handler:` line needs an id, event and method: {line}"));
                };
                if let Some(c) = spec.components.iter_mut().find(|c| c.id == id) {
                    c.handlers.push((ev.to_string(), m.to_string()));
                }
            }
            _ => {}
        }
    }
    if form_id.is_empty() {
        return Err("no `form:` line in the description".into());
    }
    Ok(spec)
}

/// The inverse of the escaping `kiln inspect` does on a value.
fn unescape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut it = v.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n') => out.push('\n'),
            Some('0') => out.push('\0'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// `background_color` to `BackgroundColor`: the designer speaks the spelling
/// the toolchain files things under, and K2 source is written in C#'s casing.
/// This is the inverse of what `kiln inspect` does on the way out.
fn to_pascal(name: &str) -> String {
    let mut out = String::new();
    for part in name.split('_') {
        let mut cs = part.chars();
        if let Some(f) = cs.next() {
            out.extend(f.to_uppercase());
            out.push_str(cs.as_str());
        }
    }
    out
}

/// The same, for a name that is a value rather than a type: `count_label`
/// becomes `countLabel`, so a round trip through the designer gives back the
/// identifier that went in.
fn to_camel(name: &str) -> String {
    let p = to_pascal(name);
    let mut cs = p.chars();
    match cs.next() {
        Some(f) => f.to_lowercase().chain(cs).collect(),
        None => String::new(),
    }
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
        Edit::Sync(spec) => {
            // A component that is still there keeps its comments: the designer
            // owns the block's *shape*, not the notes someone wrote in it.
            let kept: Vec<(String, Vec<String>)> = form
                .components
                .iter()
                .map(|c| (c.id.clone(), c.leading.clone()))
                .collect();
            // A value arrives as text, and guessing its type from the text
            // alone turns `Text = "0"` into `Text = 0`. Where the property is
            // already there and its text has not changed, its written form is
            // kept exactly.
            let old_form_props = form.properties.clone();
            form.properties = spec
                .properties
                .iter()
                .map(|(n, v)| {
                    let name = to_pascal(n);
                    (name.clone(), keep_or_parse(&old_form_props, &name, v))
                })
                .collect();
            let old_components = form.components.clone();
            form.components = spec
                .components
                .iter()
                .map(|c| {
                    let id = to_camel(&c.id);
                    let old_props = old_components
                        .iter()
                        .find(|o| o.id == id)
                        .map(|o| o.properties.clone())
                        .unwrap_or_default();
                    ComponentDecl {
                        leading: kept
                            .iter()
                            .find(|(k, _)| *k == id)
                            .map(|(_, l)| l.clone())
                            .unwrap_or_default(),
                        type_name: to_pascal(&c.type_name),
                        id,
                        properties: c
                            .properties
                            .iter()
                            .map(|(n, v)| {
                                let name = to_pascal(n);
                                (name.clone(), keep_or_parse(&old_props, &name, v))
                            })
                            .collect(),
                        handlers: c
                            .handlers
                            .iter()
                            .map(|(e, m)| {
                                (to_pascal(e), HandlerRef::Method(to_pascal(m)))
                            })
                            .collect(),
                        span: Span::default(),
                    }
                })
                .collect();
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

/// The value to write for a property that may already be there.
///
/// `kiln inspect` prints a value as text, so a string `"0"` and a number `0`
/// arrive identically. Re-guessing from the text would rewrite one as the
/// other on every save. If the property already exists and still reads the
/// same, its written form is what goes back.
fn keep_or_parse(old: &[(String, Expr)], name: &str, text: &str) -> Expr {
    if let Some((_, e)) = old.iter().find(|(n, _)| n == name) {
        if value_text(e) == text {
            return e.clone();
        }
    }
    literal(text)
}

/// A property value as `kiln inspect` prints it, for comparing against what
/// came back.
fn value_text(e: &Expr) -> String {
    match &e.kind {
        ExprKind::Str(s) => s.clone(),
        ExprKind::Int(v) => v.to_string(),
        ExprKind::Bool(b) => if *b { "true" } else { "false" }.to_string(),
        _ => crate::print::expr(e),
    }
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
