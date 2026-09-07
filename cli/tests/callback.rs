//! End-to-end tests for `address of`: passing an Kiln subroutine to C as a
//! function pointer. Build a tiny C library that calls back through a pointer,
//! build an Kiln program that hands it the address of a sub, run the two
//! together, and prove that C reaches Kiln code — for a returned value
//! (`apply` -> `summer`), for a side effect repeated in C's own loop
//! (`each` -> `announce`), and for a C string handed to a `text` parameter
//! (`greet` -> `say`). Also prove that a non-C-representable sub is a named
//! build-time error, that a `--release` build (which dead-strips with
//! `--gc-sections`) keeps the address-taken sub, and — under wine — that the
//! same thing works cross-built for Windows.
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
    let dir = std::env::temp_dir().join(format!("kiln_cb_{tag}_test"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Compile `examples/dll/cb.c` to `libcb.so` inside `dir`.
fn build_cb_so(dir: &Path) {
    let src = repo().join("examples/dll/cb.c");
    let status = Command::new("clang")
        .args(["-shared", "-fPIC", "-o"])
        .arg(dir.join("libcb.so"))
        .arg(&src)
        .status()
        .expect("run clang for libcb.so");
    assert!(status.success(), "clang failed to build libcb.so");
}

/// Build `examples/dll/cb.kiln` to `dir/cb`, optionally with `--release`; return
/// the binary path.
fn build_cb(dir: &Path, release: bool) -> PathBuf {
    let repo = repo();
    let src = repo.join("examples/dll/cb.kiln");
    let out = dir.join("cb");
    let mut args = vec![
        "build".to_string(),
        src.to_str().unwrap().to_string(),
        "-o".to_string(),
        out.to_str().unwrap().to_string(),
    ];
    if release {
        args.push("--release".to_string());
    }
    let status = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(&args)
        .env("KILN_RUNTIME_DIR", repo.join("runtime"))
        .status()
        .expect("run kiln");
    assert!(status.success(), "kiln build cb failed (release={release})");
    out
}

fn run_lines(bin: &Path) -> Vec<String> {
    let out = Command::new(bin).output().expect("run cb program");
    assert!(
        out.status.success(),
        "program exited non-zero:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

/// The whole happy path: C calls `summer` through a function pointer for a
/// returned sum (`apply(address of summer, 40, 2)` -> 42), calls `announce`
/// once per number in its own loop for the side effect (`each(address of
/// announce, 3)` -> the sequence 1, 2, 3 printed from inside C), and hands `say`
/// a C string it prints (`greet(address of say)` -> "from C"), proving a `text`
/// parameter arrives intact.
#[test]
fn c_calls_back_into_kiln_for_a_value_and_for_effect() {
    let dir = scratch("run");
    build_cb_so(&dir);
    let bin = build_cb(&dir, false);
    assert_eq!(
        run_lines(&bin),
        vec!["42", "1", "2", "3", "from C"],
        "unexpected callback output"
    );
}

/// A `--release` build hardens and strips, and its link runs `--gc-sections`.
/// A subroutine whose address is only *taken* (never called by name) must
/// survive that pass, or `summer` would be gone and `apply` would jump into
/// nothing. The build running with the identical output is the proof it stayed.
#[test]
fn a_release_build_keeps_the_address_taken_sub() {
    let dir = scratch("release");
    build_cb_so(&dir);
    let bin = build_cb(&dir, true);
    assert_eq!(
        run_lines(&bin),
        vec!["42", "1", "2", "3", "from C"],
        "the address-taken sub was dropped by --gc-sections in a release build"
    );
}

/// A sub whose signature cannot cross the C boundary is a BUILD-time error that
/// names the sub and the offending parameter, exactly as a bad `dll` signature
/// is — you learn it cannot be a callback before you run anything.
#[test]
fn a_non_c_representable_sub_is_a_named_build_error() {
    let dir = scratch("badsig");
    let src = dir.join("bad.kiln");
    std::fs::write(
        &src,
        "module m\n\
         dll each(fn: ptr, n: int) from \"cb\"\n\
         sub bad(xs: int[])\n  call print_int(1)\nend\n\
         sub main\n  call each(address of bad, 1)\nend\n",
    )
    .expect("write bad.kiln");
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", src.to_str().unwrap(), "-o", dir.join("x").to_str().unwrap()])
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln");
    assert!(!out.status.success(), "a non-C-representable callback must not build");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("`bad`") && stderr.contains("int[]"),
        "the error must name the sub and the offending type, got:\n{stderr}"
    );
}

/// Taking the address of something that is not a subroutine is a build error
/// that says what the name actually is.
#[test]
fn address_of_a_non_sub_is_a_named_build_error() {
    let dir = scratch("nonsub");
    let src = dir.join("bad.kiln");
    // `print_int` is a built-in command, not a sub.
    std::fs::write(
        &src,
        "module m\nsub main\n  var p: ptr = address of print_int\n  call print_int(1)\nend\n",
    )
    .expect("write bad.kiln");
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", src.to_str().unwrap(), "-o", dir.join("x").to_str().unwrap()])
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln");
    assert!(!out.status.success(), "address of a command must not build");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("print_int") && stderr.contains("command"),
        "the error must say the name is a command, got:\n{stderr}"
    );
}

// --- the Windows cross build, proven under wine when both are present -------

fn mingw_present() -> bool {
    if on_path("x86_64-w64-mingw32-gcc") {
        return true;
    }
    eprintln!("x86_64-w64-mingw32-gcc is not installed; skipping the Windows callback test");
    false
}

/// The same C library and the same program, cross-built for Windows: the `.c`
/// becomes `cb.dll` through mingw and the program a PE32+ image, and — when wine
/// is here — the two run together with the identical output. `LoadLibrary` +
/// `GetProcAddress` reach `cb.dll`, and the function pointer Kiln handed it
/// is one the Windows C ABI calls the same way it does on Linux.
#[test]
fn cb_cross_builds_for_windows_and_runs_under_wine() {
    if !mingw_present() {
        return;
    }
    let dir = scratch("win");

    let status = Command::new("x86_64-w64-mingw32-gcc")
        .args(["-shared", "-o"])
        .arg(dir.join("cb.dll"))
        .arg(repo().join("examples/dll/cb.c"))
        .status()
        .expect("run mingw for cb.dll");
    assert!(status.success(), "mingw failed to build cb.dll");

    let image = dir.join("cb.exe");
    let status = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", repo().join("examples/dll/cb.kiln").to_str().unwrap(), "--os", "windows", "-o"])
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
        vec!["42", "1", "2", "3", "from C"],
        "unexpected callback output under wine"
    );
}
