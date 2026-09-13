//! The editor grammars must know the language the compiler implements.
//!
//! Both are hand-written data files, so nothing stops them drifting from the
//! lexer except a test that reads all three.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Every K2 keyword, as the lexer defines them (`design/k2/spec.md` §2).
const K2_KEYWORDS: &[&str] = &[
    "namespace",
    "using",
    "public",
    "private",
    "internal",
    "static",
    "const",
    "var",
    "let",
    "class",
    "record",
    "struct",
    "enum",
    "interface",
    "form",
    "new",
    "this",
    "return",
    "if",
    "else",
    "switch",
    "case",
    "default",
    "for",
    "foreach",
    "in",
    "while",
    "do",
    "break",
    "continue",
    "defer",
    "is",
    "as",
    "ref",
    "out",
];

#[test]
fn the_vscode_grammar_knows_the_k2_keywords() {
    let g =
        std::fs::read_to_string(repo_root().join("editors/vscode/syntaxes/kiln.tmLanguage.json"))
            .unwrap();
    // Valid JSON, first of all.
    let v: serde_json::Value = serde_json::from_str(&g).expect("the grammar is JSON");
    assert_eq!(v["scopeName"], "source.kiln");
    for kw in K2_KEYWORDS {
        assert!(g.contains(kw), "the VS Code grammar does not know `{kw}`");
    }
    // K2's comment and interpolation forms.
    assert!(g.contains("//.*$"), "no line-comment rule");
    assert!(g.contains("\\\\$\\\""), "no `$\"` interpolation rule");
}

#[test]
fn the_kate_grammar_knows_the_k2_keywords() {
    let g = std::fs::read_to_string(repo_root().join("editors/kate/kiln.xml")).unwrap();
    for kw in K2_KEYWORDS {
        assert!(
            g.contains(&format!("<item>{kw}</item>")),
            "the Kate grammar does not know `{kw}`"
        );
    }
    assert!(g.contains("BlockComment"), "no block-comment context");
}

#[test]
fn the_grammars_still_know_the_1x_keywords() {
    // 1.x keeps shipping, and both editors keep highlighting it.
    let vs =
        std::fs::read_to_string(repo_root().join("editors/vscode/syntaxes/kiln.tmLanguage.json"))
            .unwrap();
    let kate = std::fs::read_to_string(repo_root().join("editors/kate/kiln.xml")).unwrap();
    for kw in ["sub", "end", "module"] {
        assert!(vs.contains(kw), "VS Code forgot 1.x `{kw}`");
        assert!(kate.contains(kw), "Kate forgot 1.x `{kw}`");
    }
}
