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
pub mod lexer;
pub mod lower;
pub mod parser;

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
