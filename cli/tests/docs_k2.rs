//! Every Kiln 2 sample in the documentation must compile.
//!
//! `CLAUDE.md`'s rule for 1.x is that a wrong sample fails the build rather
//! than the reader, and `tools/check-docs.sh` enforces it. The same rule holds
//! for Kiln 2: each ```k2 block is built by the same `kiln k2` a reader would
//! type, with the standard library loaded when the sample asks for it.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

/// The ```k2 blocks of a page, with the line each one starts on.
fn samples(md: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut lines = md.lines().enumerate();
    while let Some((i, line)) = lines.next() {
        if line.trim() != "```k2" {
            continue;
        }
        let mut body = String::new();
        for (_, l) in lines.by_ref() {
            if l.trim() == "```" {
                break;
            }
            body.push_str(l);
            body.push('\n');
        }
        out.push((i + 2, body));
    }
    out
}

#[test]
fn every_documented_k2_sample_compiles() {
    let root = repo_root();
    // Every page, not one: a second page with ```k2 in it would otherwise be
    // unchecked, which is exactly how a wrong sample gets published.
    let mut found: Vec<(String, usize, String)> = Vec::new();
    for entry in std::fs::read_dir(root.join("docs-site/src")).expect("the book") {
        let path = entry.expect("an entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let md = std::fs::read_to_string(&path).expect("a page");
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        for (line, src) in samples(&md) {
            found.push((name.clone(), line, src));
        }
    }
    assert!(
        found.len() >= 14,
        "expected the book's K2 samples, found {}",
        found.len()
    );

    let work = std::env::temp_dir().join(format!("k2-docs-{}", std::process::id()));
    std::fs::create_dir_all(&work).unwrap();

    let mut failures = Vec::new();
    for (page, line, src) in &found {
        let path = work.join(format!("sample-{page}-{line}.kiln"));
        std::fs::write(&path, src).unwrap();
        // A sample that reaches the standard library needs the runtime linked;
        // one that does not is self-contained against libc.
        let mut args = vec!["k2".to_string(), path.to_string_lossy().to_string()];
        if src.contains("using Kiln.") {
            args.push("--runtime".to_string());
        }
        args.push("-o".to_string());
        args.push(
            work.join(format!("sample-{page}-{line}"))
                .to_string_lossy()
                .to_string(),
        );

        let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
            .args(&args)
            .current_dir(&root)
            .output()
            .expect("kiln k2 runs");
        if !out.status.success() {
            failures.push(format!(
                "docs-site/src/{page}:{line}\n{}\n--- the sample ---\n{src}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} Kiln 2 samples in the book do not compile:\n\n{}",
        failures.len(),
        found.len(),
        failures.join("\n\n")
    );
}

#[test]
fn the_kiln_2_page_is_in_the_book() {
    let summary = std::fs::read_to_string(repo_root().join("docs-site/src/SUMMARY.md")).unwrap();
    for page in ["kiln-2.md", "moving-to-kiln-2.md"] {
        assert!(
            summary.contains(page),
            "{page} is not in SUMMARY.md, so the book will not show it"
        );
    }
}

/// The fenced blocks of one page, by language tag.
fn blocks(md: &str, tag: &str) -> Vec<String> {
    let open = format!("```{tag}");
    let mut out = Vec::new();
    let mut lines = md.lines();
    while let Some(line) = lines.next() {
        if line.trim() != open {
            continue;
        }
        let mut body = String::new();
        for l in lines.by_ref() {
            if l.trim() == "```" {
                break;
            }
            body.push_str(l);
            body.push('\n');
        }
        out.push(body);
    }
    out
}

#[test]
fn the_migration_guide_shows_what_the_tool_actually_produces() {
    // The guide prints a 1.x program and the Kiln 2 it becomes. If those two
    // ever disagree with `kiln migrate`, the page is teaching a conversion
    // nobody will get — so the tool is run and its output compared, rather
    // than the page being trusted.
    let root = repo_root();
    let md = std::fs::read_to_string(root.join("docs-site/src/moving-to-kiln-2.md"))
        .expect("the migration guide");
    let before = blocks(&md, "kiln");
    let after = blocks(&md, "k2");
    assert!(
        !before.is_empty() && !after.is_empty(),
        "the guide has lost its worked example"
    );

    let work = std::env::temp_dir().join(format!("k2-migrate-doc-{}", std::process::id()));
    std::fs::create_dir_all(&work).unwrap();
    let src = work.join("before.kiln");
    std::fs::write(&src, &before[0]).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["migrate", src.to_str().unwrap()])
        .current_dir(&root)
        .output()
        .expect("kiln migrate");
    assert!(
        out.status.success(),
        "the guide's 1.x program does not migrate:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let produced = String::from_utf8_lossy(&out.stdout).to_string();
    assert_eq!(
        produced.trim_end(),
        after[0].trim_end(),
        "the guide and `kiln migrate` disagree"
    );
}
