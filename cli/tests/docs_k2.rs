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
    let page = root.join("docs-site/src/kiln-2.md");
    let md = std::fs::read_to_string(&page).expect("the Kiln 2 page");
    let found = samples(&md);
    assert!(
        found.len() >= 12,
        "expected the page's samples, found {}",
        found.len()
    );

    let work = std::env::temp_dir().join(format!("k2-docs-{}", std::process::id()));
    std::fs::create_dir_all(&work).unwrap();

    let mut failures = Vec::new();
    for (line, src) in &found {
        let path = work.join(format!("sample-{line}.kiln"));
        std::fs::write(&path, src).unwrap();
        // A sample that reaches the standard library needs the runtime linked;
        // one that does not is self-contained against libc.
        let mut args = vec!["k2".to_string(), path.to_string_lossy().to_string()];
        if src.contains("using Kiln.") {
            args.push("--runtime".to_string());
        }
        args.push("-o".to_string());
        args.push(work.join(format!("sample-{line}")).to_string_lossy().to_string());

        let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
            .args(&args)
            .current_dir(&root)
            .output()
            .expect("kiln k2 runs");
        if !out.status.success() {
            failures.push(format!(
                "docs-site/src/kiln-2.md:{line}\n{}\n--- the sample ---\n{src}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} Kiln 2 samples do not compile:\n\n{}",
        failures.len(),
        found.len(),
        failures.join("\n\n")
    );
}

#[test]
fn the_kiln_2_page_is_in_the_book() {
    let summary = std::fs::read_to_string(repo_root().join("docs-site/src/SUMMARY.md")).unwrap();
    assert!(
        summary.contains("kiln-2.md"),
        "the Kiln 2 page is not in SUMMARY.md, so the book will not show it"
    );
}
