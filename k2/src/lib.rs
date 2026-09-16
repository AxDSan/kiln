//! Kiln 2 front end: K2 source → KIR.
//!
//! `k2-syntax` (lexer, parser, AST) and a first `k2-lower` live here as modules
//! while the language is stood up; the plan splits them into separate crates as
//! they mature. The lowerer targets [`kiln_kir`], the IR the backend consumes.
//!
//! This is the subset that makes real K2 programs compile and run today:
//! namespaces, static classes and records, methods, the primitive types and
//! control flow, arithmetic with C# signedness, string interpolation through
//! `Console.WriteLine`/`Write`. Generics, lambdas, `Result`, attributes,
//! interfaces and forms grow in later phases.

pub mod ast;
pub mod edit;
pub mod lexer;
pub mod lower;
pub mod migrate;
pub mod parser;
pub mod print;

pub use kiln_kir::ModuleKind;

pub use edit::Edit;

/// Apply a tree-level edit to a form and return the rewritten source.
pub fn edit(src: &str, e: &Edit) -> Result<String, String> {
    edit::apply(src, e)
}

/// Convert a Kiln 1.x program to Kiln 2 source — `kiln migrate`.
/// As `migrate`, with the program's libraries loaded for exact types.
pub fn migrate_with(src: &str, registry: Option<&kiln_ir::Registry>) -> Result<String, String> {
    migrate::migrate_with_units(src, registry, &|_| false)
}

/// `migrate_with`, told which `use`d names are unit files beside the program:
/// those become `using Accounts;` rather than a library's `using Kiln.Accounts;`.
pub fn migrate_with_units(
    src: &str,
    registry: Option<&kiln_ir::Registry>,
    is_unit: &dyn Fn(&str) -> bool,
) -> Result<String, String> {
    migrate::migrate_with_units(src, registry, is_unit)
}

pub fn migrate(src: &str) -> Result<String, String> {
    migrate::migrate(src)
}

/// Parse and re-print K2 source in its canonical spelling — `kiln fmt`.
pub fn format(src: &str) -> Result<String, String> {
    let toks = lexer::lex(src).map_err(|e| format!("{}:{}: {}", e.line, e.col, e.msg))?;
    let program =
        parser::parse(toks).map_err(|e| format!("{}:{}: {}", e.span.line, e.span.col, e.msg))?;
    Ok(print::program(&program))
}
pub use lower::Runtime;

/// Parse and lower K2 source to a KIR module, choosing what it links against.
pub fn compile_with(src: &str, runtime: Runtime) -> Result<kiln_kir::Module, String> {
    compile_full(src, runtime, None)
}

/// Parse and lower, with the standard library's commands available.
pub fn compile_full(
    src: &str,
    runtime: Runtime,
    registry: Option<&kiln_ir::Registry>,
) -> Result<kiln_kir::Module, String> {
    compile_named(src, runtime, registry, None)
}

/// Parse and lower, naming the file it came from so the binary carries debug
/// information a debugger can step through. `None` emits none.
pub fn compile_named(
    src: &str,
    runtime: Runtime,
    registry: Option<&kiln_ir::Registry>,
    source: Option<&str>,
) -> Result<kiln_kir::Module, String> {
    let toks = lexer::lex(src).map_err(|e| format!("{}:{}: {}", e.line, e.col, e.msg))?;
    let program =
        parser::parse(toks).map_err(|e| format!("{}:{}: {}", e.span.line, e.span.col, e.msg))?;
    let mut m = lower::lower_full(&program, runtime, registry)?;
    m.source = source.map(|s| s.to_string());
    Ok(m)
}

/// Parse and lower for a chosen artifact and machine — a library, or a program
/// for another operating system or architecture.
pub fn compile_opts(
    src: &str,
    runtime: Runtime,
    registry: Option<&kiln_ir::Registry>,
    source: Option<&str>,
    opts: &lower::Options,
) -> Result<kiln_kir::Module, String> {
    let toks = lexer::lex(src).map_err(|e| format!("{}:{}: {}", e.line, e.col, e.msg))?;
    let program =
        parser::parse(toks).map_err(|e| format!("{}:{}: {}", e.span.line, e.span.col, e.msg))?;
    let mut m = lower::lower_opts(&program, runtime, registry, opts)?;
    m.source = source.map(|s| s.to_string());
    Ok(m)
}

/// A program assembled from its entry file and the unit files it names.
///
/// `using Accounts;` names `accounts.kiln` beside the entry file, the way 1.x's
/// `use accounts` did: when such a file exists it is parsed and its types join
/// the program, and the units it names in turn are followed. A `using` that
/// names no file beside — `Kiln.Db`, or a namespace declared in the same file —
/// is left to the lowerer as it always was. Each file is read once, however many
/// others name it, and a unit may not carry top-level statements: only the
/// entry file is a program.
pub struct Units {
    pub program: ast::Program,
    /// Every file read, entry first, with its text — so a caller can collect
    /// the libraries each one asks for.
    pub files: Vec<(std::path::PathBuf, String)>,
}

/// The file a `using` names beside `dir`, if there is one.
pub fn unit_file(dir: &std::path::Path, using: &str) -> Option<std::path::PathBuf> {
    if using.starts_with("Kiln.") || using.starts_with("System") {
        return None;
    }
    let last = using.rsplit('.').next().unwrap_or(using);
    let mut snake = String::new();
    for (i, c) in last.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            snake.push('_');
        }
        snake.extend(c.to_lowercase());
    }
    [snake, last.to_lowercase(), last.to_string()]
        .into_iter()
        .map(|stem| dir.join(format!("{stem}.kiln")))
        .find(|p| p.is_file())
}

pub fn parse_units(entry: &std::path::Path, entry_src: &str) -> Result<Units, String> {
    let parse = |path: &std::path::Path, src: &str| -> Result<ast::Program, String> {
        let toks = lexer::lex(src)
            .map_err(|e| format!("{}:{}:{}: {}", path.display(), e.line, e.col, e.msg))?;
        parser::parse(toks).map_err(|e| {
            format!("{}:{}:{}: {}", path.display(), e.span.line, e.span.col, e.msg)
        })
    };
    let dir = entry
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."))
        .to_path_buf();
    let mut program = parse(entry, entry_src)?;
    let canon = |p: &std::path::Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let mut seen = vec![canon(entry)];
    let mut files = vec![(entry.to_path_buf(), entry_src.to_string())];
    let mut queue: Vec<String> = program.usings.iter().map(|u| u.path.clone()).collect();
    while let Some(using) = queue.pop() {
        let Some(path) = unit_file(&dir, &using) else {
            continue;
        };
        if seen.contains(&canon(&path)) {
            continue;
        }
        seen.push(canon(&path));
        let src = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read unit {}: {e}", path.display()))?;
        let unit = parse(&path, &src)?;
        if !unit.top_level.is_empty() {
            return Err(format!(
                "{}: a unit holds declarations only; its top-level statements belong in the program",
                path.display()
            ));
        }
        for u in &unit.usings {
            queue.push(u.path.clone());
            if !program.usings.iter().any(|have| have.path == u.path) {
                program.usings.push(u.clone());
            }
        }
        program.items.extend(unit.items);
        files.push((path, src));
    }
    Ok(Units { program, files })
}

/// A kit's `.kdecl` bundle as Kiln 2 declarations — see `migrate::declarations`.
pub fn declarations(m: &kiln_ir::Module) -> ast::Program {
    migrate::declarations(m)
}

/// Lower an already-parsed program — `parse_units`' — as `compile_opts` does.
pub fn compile_program_opts(
    program: &ast::Program,
    runtime: Runtime,
    registry: Option<&kiln_ir::Registry>,
    source: Option<&str>,
    opts: &lower::Options,
) -> Result<kiln_kir::Module, String> {
    let mut m = lower::lower_opts(program, runtime, registry, opts)?;
    m.source = source.map(|s| s.to_string());
    Ok(m)
}

/// Parse and lower to textual LLVM IR for a chosen runtime.
pub fn compile_to_llvm_with(src: &str, runtime: Runtime) -> Result<String, String> {
    Ok(kiln_kir::emit::emit(&compile_with(src, runtime)?))
}

/// Parse and lower K2 source to a KIR module.
pub fn compile(src: &str) -> Result<kiln_kir::Module, String> {
    let toks = lexer::lex(src).map_err(|e| format!("{}:{}: {}", e.line, e.col, e.msg))?;
    let program =
        parser::parse(toks).map_err(|e| format!("{}:{}: {}", e.span.line, e.span.col, e.msg))?;
    lower::lower(&program)
}

/// Parse and lower to textual LLVM IR.
pub fn compile_to_llvm(src: &str) -> Result<String, String> {
    Ok(kiln_kir::emit::emit(&compile(src)?))
}
