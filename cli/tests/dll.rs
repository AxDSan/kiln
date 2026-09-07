//! End-to-end tests for the `dll` foreign-function interface: build a tiny C
//! library, build an Kiln program that calls into it, run the two together,
//! and prove the value, string and pointer paths across the boundary — plus
//! that a missing symbol is a named failure, an `as` rename maps a symbol, the
//! validator rejects a bad call at build time, and a declaration compiles with
//! the library absent (loading is lazy).
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn on_path(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// A scratch directory of its own per test. The program resolves its library
/// beside its own executable, so the built `.so` and the built program must
/// share one directory — and two tests must not share it.
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kiln_dll_{tag}_test"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Compile `examples/dll/mathdll.c` to `libmathdll.so` inside `dir`.
fn build_mathdll_so(dir: &Path) {
    let src = repo().join("examples/dll/mathdll.c");
    let status = Command::new("clang")
        .args(["-shared", "-fPIC", "-o"])
        .arg(dir.join("libmathdll.so"))
        .arg(&src)
        .status()
        .expect("run clang for libmathdll.so");
    assert!(status.success(), "clang failed to build libmathdll.so");
}

/// Build `examples/dll/<name>.kiln` to `dir/<name>`; return the binary path.
fn build_oir(name: &str, dir: &Path) -> PathBuf {
    let repo = repo();
    let src = repo.join("examples/dll").join(format!("{name}.kiln"));
    let out = dir.join(name);
    let status = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .env("KILN_RUNTIME_DIR", repo.join("runtime"))
        .status()
        .expect("run kiln");
    assert!(status.success(), "kiln build {name} failed");
    out
}

/// The whole happy path: a value returned (`add_ints`), a C string returned and
/// copied into a managed text (`banner`), a value written through a `ptr` by the
/// callee (`bump`), and a symbol reached under a different name (`as`).
#[test]
fn mathdll_value_string_and_pointer_paths() {
    let dir = scratch("math");
    build_mathdll_so(&dir);
    let bin = build_oir("mathdll", &dir);

    let out = Command::new(&bin).output().expect("run mathdll program");
    assert!(
        out.status.success(),
        "program exited non-zero:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(
        lines,
        vec![
            "42",            // add_ints(40, 2)
            "Kiln <-> C", // banner() copied out of C
            "42",            // bump() wrote through the ptr: 41 -> 42
            "50",            // tentimes(5) via `as "times_ten"`
            "9000000001",    // add_bignums: int64 past the 32-bit range
            "3.5",           // halve(7.0): a double in and out
            "positive",      // is_positive(3): a C int read back as a bool
        ],
        "unexpected FFI output"
    );
}

/// A declaration whose library is absent still BUILDS and RUNS, because the load
/// is deferred to the first call and this program never makes one — proof that
/// `dll` binding is lazy, not eager at start-up.
#[test]
fn a_declaration_with_the_library_absent_builds_and_runs() {
    let dir = scratch("lazy");
    // Deliberately do NOT build any library into `dir`.
    let bin = build_oir("lazy", &dir);
    let out = Command::new(&bin).output().expect("run lazy program");
    assert!(out.status.success(), "lazy program should exit 0");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "lazy ok");
}

/// A symbol the library does not export aborts at the call, exit 1, with a
/// stderr line naming BOTH the symbol and the library.
#[test]
fn a_missing_symbol_is_a_named_runtime_error() {
    let dir = scratch("badsym");
    build_mathdll_so(&dir);
    let bin = build_oir("badsym", &dir);
    let out = Command::new(&bin).output().expect("run badsym program");
    assert!(!out.status.success(), "a missing symbol must fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("does_not_exist") && stderr.contains("mathdll"),
        "the error must name the symbol and the library, got:\n{stderr}"
    );
}

/// A missing library — one that cannot be opened at all — is the same kind of
/// named failure, and names the library and the symbol that wanted it.
#[test]
fn a_missing_library_is_a_named_runtime_error() {
    let dir = scratch("badlib");
    // No library at all in `dir`, and — unlike `lazy.kiln` — this program DOES
    // call its foreign function, so the absent library must surface at the call.
    let src = dir.join("badlib.kiln");
    std::fs::write(
        &src,
        "module badlib\n\
         dll f(x: int): int from \"an_absent_library\"\n\
         sub main\n  call print_int(f(1))\nend\n",
    )
    .expect("write badlib.kiln");
    let out_bin = dir.join("badlib");
    let status = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", src.to_str().unwrap(), "-o", out_bin.to_str().unwrap()])
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .status()
        .expect("run kiln");
    assert!(status.success(), "the program should BUILD (load is lazy)");

    let out = Command::new(&out_bin).output().expect("run badlib program");
    assert!(!out.status.success(), "a missing library must fail at the call");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("an_absent_library") && stderr.contains("`f`"),
        "the error must name the library and the symbol, got:\n{stderr}"
    );
}

/// The validator rejects a wrong-arity and a wrong-type call at BUILD time, so a
/// foreign call is checked exactly like a subroutine call.
#[test]
fn the_validator_rejects_a_bad_call() {
    let dir = scratch("validate");
    for (body, needle) in [
        ("call print_int(add_ints(1))", "expects 2 argument"),
        ("call print_int(add_ints(1, \"two\"))", "expects int, got text"),
    ] {
        let src = dir.join("bad.kiln");
        std::fs::write(
            &src,
            format!(
                "module m\n\
                 dll add_ints(a: int, b: int): int from \"mathdll\"\n\
                 sub main\n  {body}\nend\n"
            ),
        )
        .expect("write bad.kiln");
        let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
            .args(["build", src.to_str().unwrap(), "-o", dir.join("x").to_str().unwrap()])
            .env("KILN_RUNTIME_DIR", repo().join("runtime"))
            .output()
            .expect("run kiln");
        assert!(!out.status.success(), "a bad call must not build: {body}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains(needle),
            "expected `{needle}` in the diagnostic for `{body}`, got:\n{stderr}"
        );
    }
}

// --- the Windows cross build, proven under wine when both are present -------

fn mingw_present() -> bool {
    if on_path("x86_64-w64-mingw32-gcc") {
        return true;
    }
    eprintln!("x86_64-w64-mingw32-gcc is not installed; skipping the Windows FFI test");
    false
}

/// The same C library and the same program, cross-built for Windows: the `.c`
/// becomes `mathdll.dll` through mingw and the program a PE32+ image, and — when
/// wine is here — the two run together with the identical output.
#[test]
fn mathdll_cross_builds_for_windows_and_runs_under_wine() {
    if !mingw_present() {
        return;
    }
    let dir = scratch("win");

    // The library as a Windows DLL, beside where the image will be built.
    let status = Command::new("x86_64-w64-mingw32-gcc")
        .args(["-shared", "-o"])
        .arg(dir.join("mathdll.dll"))
        .arg(repo().join("examples/dll/mathdll.c"))
        .status()
        .expect("run mingw for mathdll.dll");
    assert!(status.success(), "mingw failed to build mathdll.dll");

    // The program as a PE32+ image.
    let image = dir.join("mathdll.exe");
    let status = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", repo().join("examples/dll/mathdll.kiln").to_str().unwrap(), "--os", "windows", "-o"])
        .arg(&image)
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .status()
        .expect("run kiln --os windows");
    assert!(status.success(), "kiln build --os windows failed");
    assert!(image.exists(), "no Windows image was produced");

    if !on_path("wine") {
        eprintln!("wine is not installed; the Windows image was built but not run");
        return;
    }
    let out = Command::new("wine")
        .arg(&image)
        .current_dir(&dir)
        .env("WINEDEBUG", "-all")
        .output()
        .expect("run wine");
    assert!(
        out.status.success(),
        "the Windows program exited non-zero under wine:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(
        lines,
        vec!["42", "Kiln <-> C", "42", "50", "9000000001", "3.5", "positive"],
        "unexpected FFI output under wine"
    );
}
