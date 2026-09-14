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
    migrate::migrate_with(src, registry)
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
