//! Language-server support for Kiln 2 files.
//!
//! Both languages use `.kiln`, so the server decides per file: a 1.x program
//! opens with `module` or `unit`, and anything else is K2. Diagnostics come
//! from the K2 front end itself — the same parse and lowering a build runs — so
//! the editor never disagrees with the compiler.

use lsp_types::{
    Diagnostic, DiagnosticSeverity, DocumentSymbol, Position, Range, SymbolKind, TextEdit,
};

/// Whether a source file is Kiln 2 rather than 1.x.
///
/// A 1.x program begins with `module` or `unit`; that is the one line every
/// such file has and no K2 file does.
pub fn is_k2(src: &str) -> bool {
    for line in src.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') || t.starts_with("//") || t.starts_with("/*") {
            continue;
        }
        return !(t.starts_with("module ")
            || t.starts_with("unit ")
            || t == "module"
            || t == "unit");
    }
    false
}

fn at(line: usize, col: usize, len: usize) -> Range {
    // Kiln counts lines and columns from 1; LSP counts from 0.
    let l = line.saturating_sub(1) as u32;
    let c = col.saturating_sub(1) as u32;
    Range {
        start: Position::new(l, c),
        end: Position::new(l, c + len.max(1) as u32),
    }
}

fn error(range: Range, msg: String) -> Diagnostic {
    Diagnostic {
        range,
        severity: Some(DiagnosticSeverity::ERROR),
        source: Some("kiln".into()),
        message: msg,
        ..Default::default()
    }
}

/// Diagnostics for a K2 file: a lexer or parser error carries its position; a
/// lowering error does not yet, so it is reported at the top of the file rather
/// than at a position that would be wrong.
pub fn diagnostics(src: &str) -> Vec<Diagnostic> {
    let toks = match kiln_k2::lexer::lex(src) {
        Ok(t) => t,
        Err(e) => return vec![error(at(e.line, e.col, 1), e.msg)],
    };
    let program = match kiln_k2::parser::parse(toks) {
        Ok(p) => p,
        Err(e) => {
            let len = e.span.end_col.saturating_sub(e.span.col).max(1);
            return vec![error(at(e.span.line, e.span.col, len), e.msg)];
        }
    };
    // Parsed; now see whether it lowers.
    match kiln_k2::lower::lower(&program) {
        Ok(_) => Vec::new(),
        Err(msg) => vec![error(at(1, 1, 1), msg)],
    }
}

#[allow(deprecated)] // DocumentSymbol::deprecated is required by the struct
fn symbol(name: &str, detail: Option<String>, kind: SymbolKind, line: usize) -> DocumentSymbol {
    let r = at(line, 1, name.len());
    DocumentSymbol {
        name: name.to_string(),
        detail,
        kind,
        tags: None,
        deprecated: None,
        range: r,
        selection_range: r,
        children: None,
    }
}

/// The outline of a K2 file: its types, their members, and a form's components.
pub fn symbols(src: &str) -> Vec<DocumentSymbol> {
    use kiln_k2::ast::*;
    let Ok(toks) = kiln_k2::lexer::lex(src) else {
        return Vec::new();
    };
    let Ok(program) = kiln_k2::parser::parse(toks) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in &program.items {
        match item {
            Item::Type(t) => {
                let kind = match t.kind {
                    TypeKind::Record | TypeKind::Struct => SymbolKind::STRUCT,
                    _ => SymbolKind::CLASS,
                };
                let mut children: Vec<DocumentSymbol> = t
                    .consts
                    .iter()
                    .map(|c| symbol(&c.name, None, SymbolKind::CONSTANT, c.span.line))
                    .collect();
                children.extend(
                    t.fields
                        .iter()
                        .map(|f| symbol(&f.name, None, SymbolKind::FIELD, f.span.line)),
                );
                children.extend(
                    t.methods
                        .iter()
                        .map(|m| symbol(&m.name, None, SymbolKind::METHOD, m.span.line)),
                );
                let mut s = symbol(&t.name, None, kind, t.span.line);
                s.children = Some(children);
                out.push(s);
            }
            Item::Enum(e) => {
                let mut s = symbol(&e.name, None, SymbolKind::ENUM, e.span.line);
                s.children = Some(
                    e.members
                        .iter()
                        .map(|(n, _)| symbol(n, None, SymbolKind::ENUM_MEMBER, e.span.line))
                        .collect(),
                );
                out.push(s);
            }
            Item::Interface(i) => {
                let mut s = symbol(&i.name, None, SymbolKind::INTERFACE, i.span.line);
                s.children = Some(
                    i.methods
                        .iter()
                        .map(|m| symbol(&m.name, None, SymbolKind::METHOD, m.span.line))
                        .collect(),
                );
                out.push(s);
            }
            Item::Form(f) => {
                let mut children: Vec<DocumentSymbol> = f
                    .components
                    .iter()
                    .map(|c| {
                        symbol(
                            &c.id,
                            Some(c.type_name.clone()),
                            SymbolKind::OBJECT,
                            c.span.line,
                        )
                    })
                    .collect();
                children.extend(
                    f.fields
                        .iter()
                        .map(|x| symbol(&x.name, None, SymbolKind::FIELD, x.span.line)),
                );
                children.extend(
                    f.methods
                        .iter()
                        .map(|m| symbol(&m.name, None, SymbolKind::METHOD, m.span.line)),
                );
                let mut s = symbol(&f.name, Some("form".into()), SymbolKind::CLASS, f.span.line);
                s.children = Some(children);
                out.push(s);
            }
        }
    }
    out
}

/// Whole-document formatting: the canonical spelling, as one edit.
pub fn formatting(src: &str) -> Option<Vec<TextEdit>> {
    let out = kiln_k2::format(src).ok()?;
    if out == src {
        return Some(Vec::new());
    }
    let end_line = src.lines().count() as u32 + 1;
    Some(vec![TextEdit {
        range: Range {
            start: Position::new(0, 0),
            end: Position::new(end_line, 0),
        },
        new_text: out,
    }])
}
