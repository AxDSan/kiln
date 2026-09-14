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
            Item::Component(c) => out.push(symbol(
                &c.id,
                Some(c.type_name.clone()),
                SymbolKind::OBJECT,
                c.span.line,
            )),
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

// ─── Hover and completion ───────────────────────────────────────────────────
//
// Both answer from the K2 parse tree rather than a token scan, so what the
// editor says about a name is what the compiler decided about it. A file that
// does not parse still answers: the declarations above the caret are usually
// intact, and an editor that goes quiet the moment you type `(` is worse than
// one that is briefly a little stale.

/// Every name the file declares, with what to say about it.
struct Model {
    /// name → (signature, what kind of thing it is)
    entries: Vec<(String, String, &'static str)>,
    /// `owner` → its members, for completion after a dot.
    members: Vec<(String, Vec<(String, String, &'static str)>)>,
}

fn method_sig(m: &kiln_k2::ast::Method) -> String {
    use kiln_k2::print::ty;
    let ps: Vec<String> = m
        .params
        .iter()
        .map(|p| format!("{} {}", ty(&p.ty), p.name))
        .collect();
    let generics = if m.type_params.is_empty() {
        String::new()
    } else {
        format!("<{}>", m.type_params.join(", "))
    };
    format!(
        "{}{} {}{}({})",
        if m.is_static { "static " } else { "" },
        ty(&m.ret),
        m.name,
        generics,
        ps.join(", ")
    )
}

/// Parse, and if that fails, parse again with the caret's line blanked out.
///
/// This is the whole difference between a language server that helps and one
/// that goes quiet exactly when you need it: `Rect.` is a syntax error, and
/// asking what follows the dot is the one moment you are guaranteed to be
/// looking at a file that does not parse. Everything else in the file is
/// intact, so blanking the one line being typed recovers it.
fn parse_tolerantly(src: &str, caret_line: Option<usize>) -> Option<kiln_k2::ast::Program> {
    let parse = |text: &str| {
        kiln_k2::lexer::lex(text)
            .ok()
            .and_then(|t| kiln_k2::parser::parse(t).ok())
    };
    if let Some(p) = parse(src) {
        return Some(p);
    }
    let line = caret_line?;
    let blanked: String = src
        .lines()
        .enumerate()
        .map(|(i, l)| if i + 1 == line { "" } else { l })
        .collect::<Vec<_>>()
        .join("\n");
    parse(&blanked)
}

fn build_model(src: &str, caret_line: Option<usize>) -> Model {
    use kiln_k2::ast::*;
    use kiln_k2::print::ty;
    let mut model = Model {
        entries: Vec::new(),
        members: Vec::new(),
    };
    let Some(program) = parse_tolerantly(src, caret_line) else {
        return model;
    };
    for item in &program.items {
        match item {
            Item::Type(t) => {
                let what = match t.kind {
                    TypeKind::Record => "record",
                    TypeKind::Struct => "struct",
                    TypeKind::Class => "class",
                    TypeKind::StaticClass => "static class",
                };
                let generics = if t.type_params.is_empty() {
                    String::new()
                } else {
                    format!("<{}>", t.type_params.join(", "))
                };
                model
                    .entries
                    .push((t.name.clone(), format!("{what} {}{generics}", t.name), what));
                let mut ms = Vec::new();
                for f in t.record_params.iter().chain(t.fields.iter()) {
                    ms.push((f.name.clone(), format!("{} {}", ty(&f.ty), f.name), "field"));
                }
                for c in &t.consts {
                    ms.push((c.name.clone(), format!("const {}", c.name), "constant"));
                }
                for m in &t.methods {
                    ms.push((m.name.clone(), method_sig(m), "method"));
                }
                model.entries.extend(ms.iter().cloned());
                model.members.push((t.name.clone(), ms));
            }
            Item::Enum(e) => {
                model
                    .entries
                    .push((e.name.clone(), format!("enum {}", e.name), "enum"));
                let ms: Vec<_> = e
                    .members
                    .iter()
                    .map(|(n, _)| (n.clone(), format!("{}.{n}", e.name), "enum member"))
                    .collect();
                model.entries.extend(ms.iter().cloned());
                model.members.push((e.name.clone(), ms));
            }
            Item::Interface(i) => {
                model
                    .entries
                    .push((i.name.clone(), format!("interface {}", i.name), "interface"));
                let ms: Vec<_> = i.methods.iter().map(|m| (m.name.clone(), method_sig(m), "method")).collect();
                model.entries.extend(ms.iter().cloned());
                model.members.push((i.name.clone(), ms));
            }
            Item::Component(c) => {
                model.entries.push((
                    c.id.clone(),
                    format!("{} {}", c.type_name, c.id),
                    "component",
                ));
            }
            Item::Form(f) => {
                model
                    .entries
                    .push((f.name.clone(), format!("form {}", f.name), "form"));
                for c in &f.components {
                    // A component is reached by its own name, and its
                    // properties are what you can set on it.
                    model.entries.push((
                        c.id.clone(),
                        format!("{} {}", c.type_name, c.id),
                        "component",
                    ));
                    let props: Vec<_> = c
                        .properties
                        .iter()
                        .map(|(n, _)| (n.clone(), format!("{}.{n}", c.id), "property"))
                        .collect();
                    model.members.push((c.id.clone(), props));
                }
                for x in &f.fields {
                    model
                        .entries
                        .push((x.name.clone(), format!("{} {}", ty(&x.ty), x.name), "field"));
                }
                for m in &f.methods {
                    model.entries.push((m.name.clone(), method_sig(m), "method"));
                }
            }
        }
    }
    model
}

/// The identifier under the caret, and the text to its left on that line.
fn word_at(src: &str, line: usize, col: usize) -> (String, String) {
    let text = src.lines().nth(line.saturating_sub(1)).unwrap_or("");
    let bytes = text.as_bytes();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'@';
    let cut = col.saturating_sub(1).min(text.len());
    let mut start = cut;
    while start > 0 && is_word(bytes[start - 1]) {
        start -= 1;
    }
    let mut end = cut;
    while end < bytes.len() && is_word(bytes[end]) {
        end += 1;
    }
    (text[start..end].to_string(), text[..cut].to_string())
}

/// What to show when the caret rests on a name in a K2 file.
pub fn hover(src: &str, line: usize, col: usize) -> Option<String> {
    let (word, _) = word_at(src, line, col);
    if word.is_empty() {
        return None;
    }
    if let Some(k) = keyword_doc(&word) {
        return Some(format!("```\n{word}\n```\n\n{k}"));
    }
    let model = build_model(src, Some(line));
    let (_, sig, what) = model.entries.iter().find(|(n, _, _)| *n == word)?;
    Some(format!("```\n{sig}\n```\n\n{what}"))
}

/// A one-line account of a keyword or built-in type, for hover.
///
/// These are the words with no declaration to point at, so without this the
/// editor says nothing about exactly the parts of the language a newcomer is
/// most likely to be hovering.
fn keyword_doc(w: &str) -> Option<&'static str> {
    Some(match w {
        "defer" => "runs when the enclosing block is left, however it is left",
        "let" => "an immutable binding",
        "var" => "a local whose type is inferred",
        "form" => "a window: its properties and components are the source Studio edits",
        "record" => "compared by value; written positionally",
        "class" => "holds mutable state; compared by reference",
        "interface" => "a shape two types can share — there is no inheritance",
        "namespace" => "the file's namespace",
        "using" => "load a library: `using Kiln.File;` gives `File.ReadText`",
        "switch" => "a switch expression: each arm is a pattern and a value",
        "foreach" => "iterate a range or a list; each turn binds its own variable",
        "partial" => "this declaration is one half; the other half is elsewhere",
        "extern" => "declared here, defined elsewhere — see `[Dll]`",
        "string" => "text",
        "int" | "long" | "short" | "sbyte" => "a signed integer",
        "uint" | "ulong" | "ushort" | "byte" => "an unsigned integer",
        "nint" | "nuint" => "an integer the width of a pointer",
        "bool" => "true or false",
        "double" | "float" => "a floating-point number",
        "Result" => "a value that is either a result or an error — there are no exceptions",
        _ => return None,
    })
}

/// Completions for a K2 file: the members of what is before the dot, or every
/// name in the file plus the words the language itself provides.
pub fn completion(src: &str, line: usize, col: usize) -> Vec<(String, SymbolKind, String)> {
    let (_, before) = word_at(src, line, col);
    let model = build_model(src, Some(line));
    let mut out = Vec::new();

    // `id.` — the members of that one thing, and nothing else.
    let dotted = before.trim_end();
    if let Some(head) = dotted.strip_suffix('.') {
        let owner: String = head
            .chars()
            .rev()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if let Some((_, ms)) = model.members.iter().find(|(n, _)| *n == owner) {
            for (name, sig, what) in ms {
                out.push((name.clone(), kind_of(what), sig.clone()));
            }
        }
        return out;
    }

    for (name, sig, what) in &model.entries {
        out.push((name.clone(), kind_of(what), sig.clone()));
    }
    for kw in K2_WORDS {
        out.push((
            kw.to_string(),
            SymbolKind::KEY,
            keyword_doc(kw).unwrap_or("keyword").to_string(),
        ));
    }
    out
}

fn kind_of(what: &str) -> SymbolKind {
    match what {
        "method" => SymbolKind::METHOD,
        "field" | "property" => SymbolKind::FIELD,
        "constant" => SymbolKind::CONSTANT,
        "enum" => SymbolKind::ENUM,
        "enum member" => SymbolKind::ENUM_MEMBER,
        "interface" => SymbolKind::INTERFACE,
        "record" | "struct" => SymbolKind::STRUCT,
        "component" => SymbolKind::OBJECT,
        _ => SymbolKind::CLASS,
    }
}

/// The words the language provides, offered alongside what the file declares.
const K2_WORDS: &[&str] = &[
    "namespace", "using", "public", "private", "internal", "static", "const", "var", "let",
    "class", "record", "struct", "enum", "interface", "form", "partial", "new", "this", "return",
    "if", "else", "switch", "for", "foreach", "in", "while", "do", "break", "continue", "defer",
    "is", "as", "ref", "out", "extern", "int", "uint", "long", "ulong", "short", "ushort", "byte",
    "sbyte", "nint", "nuint", "float", "double", "bool", "char", "string", "void", "true", "false",
    "null", "Result", "List", "Dictionary", "HashSet", "Action", "Func", "Console",
];

/// A name as it appears in a K2 file: where, and whether it declares something.
#[derive(Clone, Debug, PartialEq)]
pub struct Occurrence {
    pub line: usize,
    /// 1-based byte column.
    pub col: usize,
    pub len: usize,
    pub is_declaration: bool,
}

/// Words that can stand before a name without making it a declaration.
const NOT_A_TYPE: &[&str] = &[
    "return", "new", "else", "in", "is", "as", "case", "default", "using", "this",
    "true", "false", "null", "ref", "out", "break", "continue", "defer", "do",
    "switch", "if", "for", "foreach", "while", "public", "private", "internal",
    "static", "partial", "extern", "const",
];

/// Every occurrence of `name` in code — outside comments and string literals,
/// inside the holes of a `$"…"` string.
///
/// A name is a declaration when a type (or a declaring keyword) stands right
/// before it: `int count`, `var x`, `class Point`, `Label title`. That is the
/// one shape C#'s grammar gives a declaration, so reading the text is enough
/// and navigation keeps working in a file that does not yet parse.
pub fn occurrences(src: &str, name: &str) -> Vec<Occurrence> {
    let mut out = Vec::new();
    // One pass over the whole text, since block comments span lines.
    let b = src.as_bytes();
    let (mut i, mut line, mut line_start) = (0usize, 1usize, 0usize);
    // The last significant token before the current position: a word, or a
    // punctuation byte.
    let mut prev: String = String::new();
    // Inside `$"…"`: how deep in `{}` we are (0 = in the literal part).
    let mut interp: Option<usize> = None;
    let is_word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    while i < b.len() {
        let c = b[i];
        if c == b'\n' {
            line += 1;
            line_start = i + 1;
            i += 1;
            continue;
        }
        if let Some(depth) = interp {
            if depth == 0 {
                match c {
                    b'\\' => i += 2,
                    b'"' => {
                        interp = None;
                        prev = "\"".into();
                        i += 1;
                    }
                    b'{' => {
                        interp = Some(1);
                        prev = "{".into();
                        i += 1;
                    }
                    _ => i += 1,
                }
                continue;
            }
            if c == b'{' {
                interp = Some(depth + 1);
            } else if c == b'}' {
                interp = Some(depth - 1);
                i += 1;
                continue;
            }
        }
        if interp.is_none() {
            if c == b'/' && b.get(i + 1) == Some(&b'/') {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            if c == b'/' && b.get(i + 1) == Some(&b'*') {
                i += 2;
                while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                    if b[i] == b'\n' {
                        line += 1;
                        line_start = i + 1;
                    }
                    i += 1;
                }
                i += 2;
                continue;
            }
            if c == b'$' && b.get(i + 1) == Some(&b'"') {
                interp = Some(0);
                i += 2;
                continue;
            }
            if c == b'"' || c == b'\'' {
                i += 1;
                while i < b.len() && b[i] != c && b[i] != b'\n' {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                i += 1;
                prev = "\"".into();
                continue;
            }
        }
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if is_word(c) {
            let start = i;
            while i < b.len() && is_word(b[i]) {
                i += 1;
            }
            let word = &src[start..i];
            if word == name && !c.is_ascii_digit() {
                let after = src[i..].trim_start();
                let prev_is_type = prev == ">" || prev == "]" || prev == "?"
                    || (prev.as_bytes().first().is_some_and(|p| is_word(*p) && !p.is_ascii_digit())
                        && !NOT_A_TYPE.contains(&prev.as_str()));
                // `a b` followed by what can end or continue a declaration.
                let ends = after.is_empty()
                    || [";", ",", ")", "=", "(", "{", "<", ":", "in "]
                        .iter()
                        .any(|e| after.starts_with(e))
                    && !after.starts_with("==");
                out.push(Occurrence {
                    line,
                    col: start - line_start + 1,
                    len: word.len(),
                    is_declaration: interp.is_none() && prev_is_type && ends,
                });
            }
            prev = word.to_string();
            continue;
        }
        // `List<int> xs` and `int? x` hug their type; `a > b` and `c ? a : b`
        // do not, and are not declarations.
        let tight = i > 0 && (is_word(b[i - 1]) || b[i - 1] == b'>' || b[i - 1] == b']');
        prev = if (c == b'>' || c == b'?') && !tight {
            " ".into()
        } else {
            (c as char).to_string()
        };
        i += 1;
    }
    // Enum members and positional record fields sit after `{`, `(` or `,`;
    // the parse tree knows them where the text cannot.
    if let Some(program) = parse_tolerantly(src, None) {
        for item in &program.items {
            if let kiln_k2::ast::Item::Enum(e) = item {
                if e.members.iter().any(|(n, _)| n == name) {
                    if let Some(o) = out.iter_mut().find(|o| o.line >= e.span.line) {
                        o.is_declaration = true;
                    }
                }
            }
        }
    }
    out
}

/// Where the name under the caret is declared: the nearest declaration above
/// it (a local shadows a field), else the first one anywhere in the file (a
/// method declared further down).
pub fn definition(src: &str, line: usize, col: usize) -> Option<Occurrence> {
    let (word, _) = word_at(src, line, col);
    if word.is_empty() {
        return None;
    }
    let decls: Vec<Occurrence> = occurrences(src, &word)
        .into_iter()
        .filter(|o| o.is_declaration)
        .collect();
    decls
        .iter()
        .rev()
        .find(|o| (o.line, o.col) <= (line, col))
        .or_else(|| decls.first())
        .cloned()
}

/// Every use of the name under the caret.
pub fn references(src: &str, line: usize, col: usize, include_declaration: bool) -> Vec<Occurrence> {
    let (word, _) = word_at(src, line, col);
    if word.is_empty() {
        return Vec::new();
    }
    occurrences(src, &word)
        .into_iter()
        .filter(|o| include_declaration || !o.is_declaration)
        .collect()
}

/// The signature of a method this file declares, for signature help.
pub fn signature(src: &str, caret_line: usize, name: &str) -> Option<String> {
    let model = build_model(src, Some(caret_line));
    model
        .entries
        .iter()
        .chain(model.members.iter().flat_map(|(_, ms)| ms.iter()))
        .find(|(n, _, what)| n == name && *what == "method")
        .map(|(_, sig, _)| sig.clone())
}
