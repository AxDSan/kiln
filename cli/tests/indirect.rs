//! End-to-end tests for `call through` — calling a function whose address is
//! only known at run time.
//!
//! A `dll` line names a symbol the linker resolves. This is the other half: the
//! program is holding a `ptr` that a loader handed back, and the call site
//! supplies the C signature the declaration would otherwise have carried. It is
//! what makes `GetProcAddress` and `dlsym` useful — before it, both returned an
//! address nothing in the language could call — and it is the single operation
//! a COM vtable is made of.
//!
//! The library under test is `examples/dll/plug.c`, built as `libplug.so` here
//! and as `plug.dll` for the Windows case. Neither program links against it and
//! neither declares a single one of its functions.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn on_path(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// A scratch directory per test: the plug-in is opened by a relative path from
/// the working directory, so the library and the program share one place and
/// two tests must not.
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kiln_indirect_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the scratch directory");
    dir
}

/// Build the Kiln source at `src` into `dir/<name>`, returning the output.
/// The working directory is the repository, which is how `use win` finds
/// `kits/win`.
fn build(src: &Path, out: &Path, extra: &[&str]) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_kiln"));
    cmd.args(["build", src.to_str().unwrap()])
        .args(extra)
        .args(["-o", out.to_str().unwrap()])
        .current_dir(repo())
        .env("KILN_RUNTIME_DIR", repo().join("runtime"));
    cmd.output().expect("run kiln build")
}

/// Build a one-off program written into the scratch directory, and answer
/// whatever the compiler said. Used by the diagnostic tests, which care about
/// the message rather than the binary.
fn build_source(dir: &Path, body: &str) -> (bool, String) {
    let src = dir.join("case.kiln");
    std::fs::write(&src, body).expect("write the case");
    let out = build(&src, &dir.join("case.bin"), &[]);
    let mut said = String::from_utf8_lossy(&out.stdout).into_owned();
    said.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), said)
}

// --- the POSIX path: dlopen + dlsym + call through --------------------------

/// The whole happy path on Linux. `examples/dll/plug.kiln` opens `libplug.so`
/// with `dlopen`, fetches four addresses with `dlsym`, and calls each one:
///
/// * a value in and a value out (`add`),
/// * a C string out, copied into a managed text (`name`),
/// * a `void` call in statement position, writing through a `ptr` (`bump`),
/// * an address READ OUT OF A TABLE rather than held in a variable — the shape
///   a COM method call has, both through `ptr_read_ptr` and through a slot of
///   an inline `ptr[N]` in a c-record.
#[test]
fn dlopen_dlsym_and_call_through() {
    let dir = scratch("posix");
    let status = Command::new("clang")
        .args(["-shared", "-fPIC", "-o"])
        .arg(dir.join("libplug.so"))
        .arg(repo().join("examples/dll/plug.c"))
        .status()
        .expect("run clang for libplug.so");
    assert!(status.success(), "clang failed to build libplug.so");

    let bin = dir.join("plug");
    let built = build(&repo().join("examples/dll/plug.kiln"), &bin, &[]);
    assert!(
        built.status.success(),
        "plug.kiln did not build:\n{}",
        String::from_utf8_lossy(&built.stderr)
    );

    // From `dir`, because the program opens `./libplug.so`.
    let out = Command::new(&bin).current_dir(&dir).output().expect("run the program");
    assert!(
        out.status.success(),
        "the program exited non-zero:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lines: Vec<String> = String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect();
    assert_eq!(
        lines,
        vec![
            "15",   // add(5, 10) through its address
            "plug", // name(): a char* copied out of C
            "42",   // bump(cell): a void call that wrote 41 -> 42
            "42",   // add(20, 22) through a pointer read out of a table
            "5",    // add(2, 3) through a slot of an inline `ptr[4]`
            "0",    // dlclose
        ],
        "unexpected output from the indirect calls"
    );
}

// --- what the checker refuses ----------------------------------------------

/// The callee must be a `ptr`. Anything else is a build error that says what it
/// got and where an address comes from — an `int` holding an address is exactly
/// the mistake `ptr_from_int` exists to make visible.
#[test]
fn a_non_ptr_callee_is_a_build_error() {
    let dir = scratch("callee");
    for (ty, init) in [("int", "5"), ("text", "\"add\""), ("double", "1.0")] {
        let (ok, said) = build_source(
            &dir,
            &format!(
                "module m\nsub main\n  let f: {ty} = {init}\n  \
                 call print_int(call through f(1, 2): int)\nend\n"
            ),
        );
        assert!(!ok, "a {ty} callee must not build");
        assert!(
            said.contains("calls an address") && said.contains(ty),
            "the diagnostic must name the type it got, said:\n{said}"
        );
    }
}

/// Used for its value, a `call through` needs the `: type` — without one it is
/// a C `void` call and there is nothing to bind.
#[test]
fn a_value_use_needs_a_return_type() {
    let dir = scratch("noret");
    let (ok, said) = build_source(
        &dir,
        "module m\nsub main\n  let p: ptr = ptr_null()\n  let n: int = call through p(1)\nend\n",
    );
    assert!(!ok, "a value use with no return type must not build");
    assert!(
        said.contains("no return type"),
        "the diagnostic must say the return type is missing, said:\n{said}"
    );
}

/// Every argument must have a C shape. The list is the `dll` list — the scalars
/// plus a c-record by pointer — so an array, a dictionary or a heap record is
/// refused with the reason, not lowered as the runtime handle it happens to be.
#[test]
fn an_argument_without_a_c_shape_is_refused() {
    let dir = scratch("args");
    for (decl, needle) in [
        ("var xs: int[] = [1]\n  call through p(xs)", "cannot cross the C boundary"),
        ("var d: int{} = {}\n  call through p(d)", "cannot cross the C boundary"),
    ] {
        let (ok, said) = build_source(
            &dir,
            &format!("module m\nsub main\n  let p: ptr = ptr_null()\n  {decl}\nend\n"),
        );
        assert!(!ok, "`{decl}` must not build");
        assert!(said.contains(needle), "expected `{needle}`, said:\n{said}");
    }

    // A heap record is the one worth its own message: it has fields, so the
    // mistake is plausible, and `is c` is the fix.
    let (ok, said) = build_source(
        &dir,
        "module m\nrecord R\n  x: int\nend\nsub main\n  let p: ptr = ptr_null()\n  \
         let r: R = R(x: 1)\n  call through p(r)\nend\n",
    );
    assert!(!ok, "a heap record argument must not build");
    assert!(
        said.contains("heap record") && said.contains("is c"),
        "the diagnostic must point at `is c`, said:\n{said}"
    );
}

/// A c-record argument crosses as a pointer to its flat storage — the same
/// reading a `dll` parameter typed with the record has — so a C function that
/// takes a `struct *` is reached without spelling `address of` at the call.
#[test]
fn a_c_record_argument_crosses_by_pointer() {
    let dir = scratch("record");
    let src = dir.join("geo.c");
    std::fs::write(
        &src,
        "typedef struct { int x; int y; } Point;\n\
         void move_point(Point *p, int dx) { p->x += dx; p->y += dx * 2; }\n",
    )
    .expect("write geo.c");
    let status = Command::new("clang")
        .args(["-shared", "-fPIC", "-o"])
        .arg(dir.join("libgeo.so"))
        .arg(&src)
        .status()
        .expect("run clang for libgeo.so");
    assert!(status.success(), "clang failed to build libgeo.so");

    let prog = dir.join("geo.kiln");
    std::fs::write(
        &prog,
        r#"module geo
record Point is c
  x: int
  y: int
end
dll dlopen(path: text, mode: int): ptr from "libdl.so.2"
dll dlsym(handle: ptr, symbol: text): ptr from "libdl.so.2"
sub main
  let lib: ptr = dlopen("./libgeo.so", 2)
  var here: Point
  here.x = 3
  here.y = 4
  call through (dlsym(lib, "move_point"))(here, 10)
  call print_int(here.x)
  call print_int(here.y)
end
"#,
    )
    .expect("write geo.kiln");

    let bin = dir.join("geo");
    let built = build(&prog, &bin, &[]);
    assert!(
        built.status.success(),
        "geo.kiln did not build:\n{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let out = Command::new(&bin).current_dir(&dir).output().expect("run geo");
    assert!(out.status.success(), "geo exited non-zero:\n{}", String::from_utf8_lossy(&out.stderr));
    let lines: Vec<String> = String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect();
    assert_eq!(lines, vec!["13", "24"], "C did not write through the struct pointer");
}

/// `through` is a soft keyword: it means something only straight after `call`,
/// so a variable of that name still reads, writes and prints.
#[test]
fn through_is_only_a_keyword_after_call() {
    let dir = scratch("soft");
    let (ok, said) = build_source(
        &dir,
        "module m\nsub main\n  var through: int = 1\n  through = through + 1\n  \
         call print_int(through)\nend\n",
    );
    assert!(ok, "`through` must stay an ordinary name:\n{said}");
}

/// A callee that is itself a call needs parentheses, because the parentheses
/// after the callee are the argument list. The diagnostic says so rather than
/// leaving the second `(` to trip the newline check.
#[test]
fn a_call_shaped_callee_is_told_to_use_parentheses() {
    let dir = scratch("parens");
    let (ok, said) = build_source(
        &dir,
        "module m\ndll dlsym(h: ptr, s: text): ptr from \"libdl.so.2\"\n\
         sub main\n  call through dlsym(ptr_null(), \"add\")(1)\nend\n",
    );
    assert!(!ok, "the two-parenthesis form must not build");
    assert!(
        said.contains("goes in parentheses"),
        "the diagnostic must name the fix, said:\n{said}"
    );
}

// --- the Windows path: LoadLibraryA + GetProcAddress, under wine ------------

fn mingw_present() -> bool {
    if on_path("x86_64-w64-mingw32-gcc") {
        return true;
    }
    eprintln!("x86_64-w64-mingw32-gcc is not installed; skipping the Windows indirect-call test");
    false
}

/// The gap this stage closed, proved where it was found: `use win` gives a
/// program `LoadLibraryA` and `GetProcAddress`, and until now the `ptr` they
/// answered with was a dead end. `examples/dll/plugwin.kiln` loads a DLL it does
/// not link against, fetches three exports by name, and calls all three —
/// cross-built with mingw and run under wine, because a Win32 call cannot be
/// proved by reading it.
#[test]
fn loadlibrary_getprocaddress_and_call_through_under_wine() {
    if !mingw_present() {
        return;
    }
    let dir = scratch("windows");
    let status = Command::new("x86_64-w64-mingw32-gcc")
        .args(["-shared", "-o"])
        .arg(dir.join("plug.dll"))
        .arg(repo().join("examples/dll/plug.c"))
        .status()
        .expect("run mingw for plug.dll");
    assert!(status.success(), "mingw failed to build plug.dll");

    let out = dir.join("plugwin");
    let built = build(&repo().join("examples/dll/plugwin.kiln"), &out, &["--os", "windows"]);
    assert!(
        built.status.success(),
        "plugwin.kiln did not cross-build:\n{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let image = dir.join("plugwin.exe");
    assert!(image.is_file(), "expected {} to be written", image.display());

    if !on_path("wine") {
        eprintln!("wine is not installed; the PE was built but not run");
        return;
    }
    // `DISPLAY` and `WAYLAND_DISPLAY` are removed so wine takes its null
    // display driver: nothing reaches the screen of whoever is working here.
    // `timeout` bounds it, so a program that wedges fails this one test rather
    // than hanging the whole suite.
    let mut cmd = if on_path("timeout") {
        let mut c = Command::new("timeout");
        c.arg("30").arg("wine").arg(&image);
        c
    } else {
        let mut c = Command::new("wine");
        c.arg(&image);
        c
    };
    let run = cmd
        .current_dir(&dir)
        .env("WINEDEBUG", "-all")
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .output()
        .expect("run the image under wine");
    let stdout = String::from_utf8_lossy(&run.stdout).into_owned();
    assert!(
        run.status.success(),
        "the Windows program exited {:?} under wine\nstdout:\n{stdout}\nstderr:\n{}",
        run.status.code(),
        String::from_utf8_lossy(&run.stderr)
    );
    let lines: Vec<String> = stdout.lines().map(|l| l.trim_end_matches('\r').to_string()).collect();
    assert_eq!(
        lines,
        vec!["15", "plug", "42", "42", "done"],
        "unexpected output from the Windows indirect calls"
    );
}
