//! End-to-end tests for the `ptr` type and the raw-memory commands: build
//! `examples/ptr.kiln` to a native binary, run it, and check that every
//! pointer operation round-trips the value it was handed.
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Build `<repo>/examples/<name>.kiln` to a temp binary; `tag` must be unique
/// per test so parallel tests do not race on the output path (see build.rs).
fn build_as(name: &str, tag: &str) -> PathBuf {
    let repo = repo();
    let example = repo.join("examples").join(format!("{name}.kiln"));
    let out_bin = std::env::temp_dir().join(format!("kiln_{name}_{tag}_test"));
    let status = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args([
            "build",
            example.to_str().unwrap(),
            "-o",
            out_bin.to_str().unwrap(),
        ])
        .env("KILN_RUNTIME_DIR", repo.join("runtime"))
        .status()
        .expect("run kiln");
    assert!(status.success(), "kiln build {name} failed");
    out_bin
}

fn run(bin: &Path) -> String {
    let out = Command::new(bin).output().expect("run built binary");
    assert!(out.status.success(), "binary exited non-zero");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The whole surface: mem_alloc + typed read/write at offsets, byte and double
/// round-trips, ptr_of_text/ptr_read_text, a two-pointer chain, the int<->ptr
/// escape hatch, pointer equality, the null pointer, and ptr_offset.
#[test]
fn ptr_example_builds_and_runs() {
    let stdout = run(&build_as("ptr", "run"));
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines,
        vec![
            "42",           // ptr_read_int at offset 0
            "9000000000",   // ptr_read_int64 at offset 8 (a value past i32)
            "255",          // ptr_read_byte reads 0..255
            "3.5",          // ptr_read_double round-trips the bytes
            "hello, C",     // ptr_of_text -> ptr_read_text
            "",             // ptr_read_text(ptr_null()) is the one safe null read
            "7",            // ptr_write_ptr / ptr_read_ptr walk a chain
            "7",            // ptr_from_int(ptr_to_int(p)) is the same address
            "same",         // two names for one address compare equal
            "null-ok",      // ptr_is_null(ptr_null())
            "123",          // a write through ptr_offset is visible from the base
            "Kiln",      // ptr_write_text copies a text (with its NUL)
        ],
        "unexpected ptr output:\n{stdout}"
    );
}
