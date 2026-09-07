//! End-to-end tests for the optional calling-convention marker on a `dll`
//! declaration (`... from "user32" system`). The marker is documentation and
//! forward-compat: every target Kiln emits is 64-bit with a single C
//! convention, so a `dll` declared WITH `system`/`stdcall`/`cdecl` must build
//! and run identically to one declared without. These tests prove exactly that
//! — the parser accepts the marker in its allowed positions and nothing
//! downstream changed — on Linux and, under wine, cross-built for Windows. They
//! do NOT prove the marker changes generated code: it deliberately does not.
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

/// A scratch directory of its own per test — the program resolves its library
/// beside its own executable, so the built `.so` and the program must share one
/// directory, and two tests must not.
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kiln_cc_{tag}_test"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// The same C library the FFI tests use, compiled to `libmathdll.so` in `dir`.
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

/// The `mathdll` program, re-declared with a convention marker on every `dll` —
/// a mix of `system`, `stdcall`, `cdecl`, and one that carries `as` *and* a
/// convention (`as "times_ten" system`). Its output must be byte-for-byte the
/// unmarked program's, which is the whole point.
const MARKED: &str = "\
module dllmath_conv

dll add_ints(a: int, b: int): int from \"mathdll\" system
dll banner(): text from \"mathdll\" cdecl
dll bump(cell: ptr) from \"mathdll\" stdcall
dll tentimes(x: int): int from \"mathdll\" as \"times_ten\" system
dll add_bignums(a: int64, b: int64): int64 from \"mathdll\" system
dll halve(x: double): double from \"mathdll\" system
dll is_positive(x: int): bool from \"mathdll\" system

sub main
  call print_int(add_ints(40, 2))
  call print_text(banner())

  let cell: ptr = mem_alloc(4)
  call ptr_write_int(cell, 0, 41)
  call bump(cell)
  call print_int(ptr_read_int(cell, 0))
  call mem_free(cell)

  call print_int(tentimes(5))
  call print_int64(add_bignums(9000000000, 1))
  call print_double(halve(7.0))
  if is_positive(3)
    call print_text(\"positive\")
  end
end
";

/// The seven lines the unmarked `mathdll` program prints (see `dll.rs`).
const EXPECTED: &[&str] = &[
    "42",
    "Kiln <-> C",
    "42",
    "50",
    "9000000001",
    "3.5",
    "positive",
];

/// A `dll` marked `system`/`stdcall`/`cdecl` builds and runs identically to the
/// unmarked one — same library, same calls, same output. Proof the marker is
/// accepted and is a no-op on this 64-bit target.
#[test]
fn a_marked_dll_builds_and_runs_identically() {
    let dir = scratch("linux");
    build_mathdll_so(&dir);

    let src = dir.join("marked.kiln");
    std::fs::write(&src, MARKED).expect("write marked.kiln");
    let bin = dir.join("marked");
    let status = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", src.to_str().unwrap(), "-o", bin.to_str().unwrap()])
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .status()
        .expect("run kiln");
    assert!(status.success(), "a marked program must build");

    let out = Command::new(&bin).output().expect("run marked program");
    assert!(
        out.status.success(),
        "program exited non-zero:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(lines, EXPECTED, "a marked dll must produce the same output");
}

/// An unknown convention (`pascal`) on a `dll`, and one (`fastcall`) on a `sub`,
/// are both build-time errors whose diagnostic names the offending word and the
/// allowed set.
#[test]
fn an_unknown_convention_is_a_build_error() {
    let dir = scratch("reject");
    for (name, src, bad) in [
        (
            "dll",
            "module m\ndll f(): int from \"x\" pascal\nsub main\nend\n",
            "pascal",
        ),
        (
            "sub",
            "module m\nsub cb(a: int): int fastcall\n  return a\nend\nsub main\nend\n",
            "fastcall",
        ),
    ] {
        let path = dir.join(format!("{name}.kiln"));
        std::fs::write(&path, src).expect("write reject source");
        let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
            .args(["build", path.to_str().unwrap(), "-o", dir.join("x").to_str().unwrap()])
            .env("KILN_RUNTIME_DIR", repo().join("runtime"))
            .output()
            .expect("run kiln");
        assert!(!out.status.success(), "`{bad}` must not build");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains(bad) && stderr.contains("stdcall"),
            "the diagnostic must name `{bad}` and the allowed set, got:\n{stderr}"
        );
    }
}

// --- the Windows cross build, proven under wine when both are present -------

/// The marked program, cross-built for Windows against `mathdll.dll` and run
/// under wine, prints the identical seven lines — the marker changes nothing on
/// the x64 Windows target either.
#[test]
fn a_marked_dll_cross_builds_for_windows_and_runs_under_wine() {
    if !on_path("x86_64-w64-mingw32-gcc") {
        eprintln!("x86_64-w64-mingw32-gcc is not installed; skipping the Windows callconv test");
        return;
    }
    let dir = scratch("win");

    let status = Command::new("x86_64-w64-mingw32-gcc")
        .args(["-shared", "-o"])
        .arg(dir.join("mathdll.dll"))
        .arg(repo().join("examples/dll/mathdll.c"))
        .status()
        .expect("run mingw for mathdll.dll");
    assert!(status.success(), "mingw failed to build mathdll.dll");

    let src = dir.join("marked.kiln");
    std::fs::write(&src, MARKED).expect("write marked.kiln");
    let image = dir.join("marked.exe");
    let status = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", src.to_str().unwrap(), "--os", "windows", "-o"])
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
    assert_eq!(lines, EXPECTED, "a marked dll must match under wine too");
}
