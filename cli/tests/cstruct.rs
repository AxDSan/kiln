//! End-to-end tests for C-struct records (`record NAME is c`): a record with a
//! fixed C memory layout, so a `dll` is handed a pointer to a real C struct.
//!
//! Builds `examples/dll/geo.c` into a shared library, builds
//! `examples/dll/cstruct.kiln` against it, runs the two together, and proves the
//! whole surface — field read/write through the flat layout, `size of`,
//! `address of` a c-record, a `dll` mutating one through its pointer, and the
//! padding of a mixed record held to clang's own `sizeof`/`offsetof`.
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

/// A scratch directory of its own per test: the program resolves `libgeo.so`
/// beside its own executable, so the two must share one directory, and two
/// tests must not race on it.
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kiln_cstruct_{tag}_test"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Compile `examples/dll/geo.c` to `libgeo.so` inside `dir`.
fn build_geo_so(dir: &Path) {
    let src = repo().join("examples/dll/geo.c");
    let status = Command::new("clang")
        .args(["-shared", "-fPIC", "-o"])
        .arg(dir.join("libgeo.so"))
        .arg(&src)
        .status()
        .expect("run clang for libgeo.so");
    assert!(status.success(), "clang failed to build libgeo.so");
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

/// The lines every run of `cstruct.kiln` must print — the same on Linux and,
/// below, under wine. Line 4 is Kiln's `size of Mixed` and line 5 is clang's
/// `sizeof(Mixed)` returned from `geo.c`: their being equal is the layout held
/// to the C compiler's. Lines 7 and 8 write through the struct pointer at the
/// offset clang reports and read the field Kiln placed, so a wrong offset
/// would not round-trip.
const EXPECT: &[&str] = &[
    "13",         // p.x: move_point(+10) mutated the struct through the pointer
    "24",         // p.y: move_point(+20)
    "8",          // size of Point (two ints, no padding)
    "24",         // size of Mixed (byte, int, byte, int64 — with padding)
    "24",         // clang's sizeof(Mixed): the same number
    "200",        // a byte field reads back unsigned
    "99",         // field `b` at clang's offset, written through the ptr
    "7000000000", // field `d` at offset 16, an int64 through the ptr
    "flag-set",   // a bool field C wrote as 7 normalises to true on read
    "hi",         // a text field round-trips: stored borrowed, read copied out
];

#[test]
fn cstruct_layout_size_and_dll_mutation() {
    if !on_path("clang") {
        eprintln!("clang is not installed; skipping the c-struct FFI test");
        return;
    }
    let dir = scratch("run");
    build_geo_so(&dir);
    let bin = build_oir("cstruct", &dir);

    let out = Command::new(&bin).output().expect("run cstruct program");
    assert!(
        out.status.success(),
        "program exited non-zero:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(lines, EXPECT, "unexpected c-struct output");
}

/// The validator's c-record placement rules, each proven by a program that must
/// fail to build with a message that names the rule. These need no library, so
/// they run even where clang is absent — a c-record misused as a heap object is
/// a build error, never a miscompile.
#[test]
fn c_record_rejections_are_build_errors() {
    let dir = scratch("reject");
    // Each snippet must FAIL to build, with a message that names the rule.
    let cases: &[(&str, &str)] = &[
        // A c-record local takes a record literal of its own type (0.9.0: it
        // becomes the zeroed declaration plus the field writes) and nothing
        // else — there is still no value to give it.
        (
            "module m\nrecord R is c\n  x: int\nend\nsub main\n  var r: R = 1\nend\n",
            "no initializer",
        ),
        (
            "module m\nrecord R is c\n  x: int\nend\nsub f(r: R)\nend\nsub main\nend\n",
            "cannot be a subroutine parameter",
        ),
        (
            "module m\nrecord H\n  x: int\nend\ndll d(h: H) from \"lib\"\nsub main\nend\n",
            "is c",
        ),
        (
            "module m\nrecord R is c\n  bad: text[]\nend\nsub main\nend\n",
            "not a C-layout field type",
        ),
        // A nested c-record IS laid out (see `cstruct2.rs`); a nested *heap*
        // record is what a struct cannot hold by value.
        (
            "module m\nrecord Inner\n  x: int\nend\nrecord R is c\n  n: Inner\nend\nsub main\nend\n",
            "heap record",
        ),
    ];
    for (i, (src, needle)) in cases.iter().enumerate() {
        let path = dir.join(format!("bad{i}.kiln"));
        std::fs::write(&path, src).unwrap();
        let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
            .args(["build", path.to_str().unwrap(), "-o"])
            .arg(dir.join(format!("bad{i}")))
            .env("KILN_RUNTIME_DIR", repo().join("runtime"))
            .output()
            .expect("run kiln");
        assert!(
            !out.status.success(),
            "case {i} should not build:\n{src}"
        );
        let msg = String::from_utf8_lossy(&out.stderr);
        assert!(
            msg.contains(needle),
            "case {i}: expected a message containing {needle:?}, got:\n{msg}"
        );
    }
}

// --- the Windows cross build, proven under wine when both are present -------

fn mingw_present() -> bool {
    if on_path("x86_64-w64-mingw32-gcc") {
        return true;
    }
    eprintln!("x86_64-w64-mingw32-gcc is not installed; skipping the Windows c-struct test");
    false
}

/// The same program and library cross-built for Windows: `geo.c` becomes
/// `geo.dll` through mingw and the program a PE32+ image, and — when wine is
/// here — the two run together with the identical output, proving the c-record
/// layout is the same on the x64 Windows ABI.
#[test]
fn cstruct_cross_builds_for_windows_and_runs_under_wine() {
    if !mingw_present() {
        return;
    }
    let dir = scratch("win");
    let status = Command::new("x86_64-w64-mingw32-gcc")
        .args(["-shared", "-o"])
        .arg(dir.join("geo.dll"))
        .arg(repo().join("examples/dll/geo.c"))
        .status()
        .expect("run mingw for geo.dll");
    assert!(status.success(), "mingw failed to build geo.dll");

    let bin = dir.join("cstruct.exe");
    let status = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args([
            "build",
            repo().join("examples/dll/cstruct.kiln").to_str().unwrap(),
            "--os",
            "windows",
            "-o",
        ])
        .arg(&bin)
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .status()
        .expect("run kiln --os windows");
    assert!(status.success(), "kiln build --os windows failed");

    if !on_path("wine") {
        eprintln!("wine is not installed; the Windows image was built but not run");
        return;
    }
    let out = Command::new("wine")
        .arg(&bin)
        .current_dir(&dir)
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
    assert_eq!(lines, EXPECT, "unexpected c-struct output under wine");
}
