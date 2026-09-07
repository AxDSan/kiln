//! The second wave of C-struct records: the shapes a real Win32 header is full
//! of. A struct nested by value (a `MSG` holds a `POINT`), 16-bit `WORD` fields
//! (`WNDCLASSEX`), a fixed inline array (`PAINTSTRUCT`'s `rgbReserved`), and a
//! `float`.
//!
//! `examples/dll/structs.c` declares the same four structs in C and reports its
//! own `sizeof` and `offsetof` for each, so every layout number the program
//! prints is checked against the C compiler's rather than against a table
//! written by hand — the whole point of a c-record is that clang and Kiln
//! agree about the bytes.
//!
//! Also here: a module variable seeded from a `const`, and a kit whose
//! declarations are split across two `.kdecl` files.
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

/// A scratch directory of its own per test: the program resolves its library
/// beside its own executable, so the two must share one directory, and two
/// tests must not race on it.
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kiln_cstruct2_{tag}_test"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn build_structs_lib(dir: &Path, cc: &str, soname: &str, shared_flags: &[&str]) {
    let src = repo().join("examples/dll/structs.c");
    let status = Command::new(cc)
        .args(shared_flags)
        .arg("-o")
        .arg(dir.join(soname))
        .arg(&src)
        .status()
        .unwrap_or_else(|e| panic!("run {cc}: {e}"));
    assert!(status.success(), "{cc} failed to build {soname}");
}

/// Every line `cstruct2.kiln` must print — the same on Linux and, below, under
/// wine. The pairs of equal numbers are Kiln's `size of` beside the same
/// `sizeof` computed by the C compiler that built `structs.c`; the `*-ok` lines
/// are Kiln's own field offsets held to clang's `offsetof`.
const EXPECT: &[&str] = &[
    "11",             // m.pt.x — a nested field round-trips
    "22",             // m.pt.y
    "48",             // size of Msg
    "48",             // clang's sizeof(SMsg): the same number
    "8",              // size of Point
    "8",              // clang's sizeof(SPoint)
    "300",            // C filled the nested POINT through the struct pointer
    "400",
    "15",             // ...and the scalar fields around it
    "1000000000000",
    "pt-offset-ok",   // `address of m.pt` sits at clang's offsetof(SMsg, pt)
    "777",            // a write through that pointer lands in m.pt.x
    "780",            // a `dll` taking the nested record moves the POINT inside
    "404",            // ...the MSG, so C got the nested struct's own address
    "65535",          // a `int16` field reads back unsigned
    "258",            // a `word` field, the same width by its other name
    "16",             // size of WC
    "16",             // clang's sizeof(SWndClass)
    "65535",          // C wrote both WORDs as uint16_t
    "258",
    "window",         // the text field after them is where it should be
    "wc-name-offset-ok",
    "3",              // bytes[1] — positions count from 1
    "48",             // bytes[16]
    "3",              // C reads the same byte at index 0
    "48",             // ...and at index 15
    "20",             // size of Blob: 4 + 16, padded to 4
    "20",             // clang's sizeof(SBlob)
    "bytes-offset-ok", // `address of b.bytes` is the first element
    "77",             // C memset the array member alone
    "77",
    "9",              // ...and left `n` untouched
    "200",            // a computed index reaches the same element
    "55",             // ...and a `const` index folds like a literal one
    "1.5",            // a float field round-trips (exact in binary)
    "4",              // size of FloatBox
    "4",              // clang's sizeof(SFloatBox)
    "2.25",           // C stored 2.25 as a float; Kiln reads it as a double
    "float-agrees",   // and C reads back exactly what Kiln wrote
    "5",              // a module variable seeded from a `const`
];

fn run_and_check(bin: &Path, dir: &Path, how: &str) {
    let out = Command::new(bin)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("run {how}: {e}"));
    assert!(
        out.status.success(),
        "{how}: program exited non-zero:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(lines, EXPECT, "{how}: unexpected output");
}

#[test]
fn nested_records_arrays_words_and_floats_match_clang() {
    if !on_path("clang") {
        eprintln!("clang is not installed; skipping the nested c-struct test");
        return;
    }
    let dir = scratch("run");
    build_structs_lib(&dir, "clang", "libstructs.so", &["-shared", "-fPIC"]);

    let bin = dir.join("cstruct2");
    let status = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args([
            "build",
            repo().join("examples/dll/cstruct2.kiln").to_str().unwrap(),
            "-o",
        ])
        .arg(&bin)
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .status()
        .expect("run kiln");
    assert!(status.success(), "kiln build cstruct2 failed");
    run_and_check(&bin, &dir, "linux");
}

/// The same program and library cross-built for Windows. A nested struct, a
/// `WORD` and an inline array lay out identically on the x64 Windows ABI, so
/// the numbers under wine are the numbers on Linux — including the ones mingw's
/// own `sizeof` reports.
#[test]
fn nested_c_structs_cross_build_for_windows_and_run_under_wine() {
    if !on_path("x86_64-w64-mingw32-gcc") {
        eprintln!("x86_64-w64-mingw32-gcc is not installed; skipping the Windows test");
        return;
    }
    let dir = scratch("win");
    build_structs_lib(&dir, "x86_64-w64-mingw32-gcc", "structs.dll", &["-shared"]);

    let bin = dir.join("cstruct2.exe");
    let status = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args([
            "build",
            repo().join("examples/dll/cstruct2.kiln").to_str().unwrap(),
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
    assert_eq!(lines, EXPECT, "unexpected output under wine");
}

/// A kit whose declarations are split across two `.kdecl` files: `use` sees
/// everything from both, and a `dll` in one file resolves a record declared in
/// the other — order across files matters no more than order within one.
#[test]
fn a_kit_may_split_its_declarations_across_files() {
    if !on_path("clang") {
        eprintln!("clang is not installed; skipping the split-kit test");
        return;
    }
    let dir = scratch("split");
    // The kit's `dll` lines name `demoffi`, the portable C library that kit
    // ships — the split here is in the DECLARATIONS, not the implementation.
    let status = Command::new("clang")
        .args(["-shared", "-fPIC", "-o"])
        .arg(dir.join("libdemoffi.so"))
        .arg(repo().join("kits/demoffi/demoffi.c"))
        .status()
        .expect("run clang for libdemoffi.so");
    assert!(status.success(), "clang failed to build libdemoffi.so");

    // `SplitPoint`, `SPLIT_ANSWER` and `SPLIT_TAG` come from `shapes.kdecl`;
    // `split_add`, `split_greeting` and `split_move` from `calls.kdecl`.
    let src = "module split\nuse split_demo\n\n\
               var seeded: int = SPLIT_ANSWER\n\n\
               sub main\n\
               \x20 call print_int(split_add(1, 2))\n\
               \x20 call print_text(split_greeting())\n\
               \x20 var p: SplitPoint\n\
               \x20 p.x = 1\n\
               \x20 p.y = 2\n\
               \x20 call split_move(p, 10, 20)\n\
               \x20 call print_int(p.x)\n\
               \x20 call print_text(SPLIT_TAG)\n\
               \x20 call print_int(seeded)\n\
               end\n";
    let srcpath = dir.join("prog.kiln");
    std::fs::write(&srcpath, src).expect("write program source");
    let bin = dir.join("prog");
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", srcpath.to_str().unwrap(), "-o"])
        .arg(&bin)
        // The kit resolves as a project kit from `kits/` beside the project,
        // which is the repo's own `kits/` — the same way `use demoffi` does.
        .current_dir(repo())
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln");
    assert!(
        out.status.success(),
        "the split-kit program failed to build:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = Command::new(&bin)
        .current_dir(&dir)
        .output()
        .expect("run the split-kit program");
    assert!(
        out.status.success(),
        "the split-kit program exited non-zero:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(
        lines,
        vec!["3", "demoffi says hello", "11", "split_demo", "42"],
        "the split kit did not deliver both files' declarations"
    );
}

/// `kiln kits` lists what a split kit declares, from every one of its files
/// — the listing and the build read the bundle through the same reader, so they
/// cannot disagree about what a kit carries.
#[test]
fn kiln_kits_lists_a_split_kits_whole_bundle() {
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .arg("kits")
        .current_dir(repo())
        .output()
        .expect("run kiln kits");
    let text = String::from_utf8_lossy(&out.stdout);
    for line in [
        "dll: split_demo split_add demoffi",     // calls.kdecl
        "crecord: split_demo SplitPoint",        // shapes.kdecl
        "const: split_demo SPLIT_ANSWER int",    // shapes.kdecl
    ] {
        assert!(text.contains(line), "expected `{line}` in:\n{text}");
    }
}

/// Two files of one kit declaring the same name is a kit-authoring fault: the
/// files share one namespace, so the error names both rather than letting one
/// silently win.
#[test]
fn a_name_declared_in_two_files_of_one_kit_is_an_error() {
    let dir = scratch("dupkit");
    let kit = dir.join("kits/dupkit");
    std::fs::create_dir_all(&kit).expect("create the kit dir");
    std::fs::write(kit.join("one.kdecl"), "const SHARED = 1\n").unwrap();
    std::fs::write(kit.join("two.kdecl"), "const SHARED = 2\n").unwrap();
    std::fs::write(
        dir.join("prog.kiln"),
        "module d\nuse dupkit\nsub main\nend\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", "prog.kiln", "-o", "prog"])
        // `kits/` beside the project is the first tier `use` looks in.
        .current_dir(&dir)
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln");
    assert!(!out.status.success(), "a duplicated name should not build");
    let msg = String::from_utf8_lossy(&out.stderr);
    assert!(
        msg.contains("one.kdecl") && msg.contains("two.kdecl") && msg.contains("SHARED"),
        "expected both files and the name in:\n{msg}"
    );
}

/// The rules that keep the new field shapes honest, each proven by a program
/// that must fail to build with a message naming the rule.
#[test]
fn the_new_field_shapes_have_build_time_rules() {
    let dir = scratch("reject");
    let cases: &[(&str, &str)] = &[
        // A nested field must be `is c`: a heap record is a pointer to an
        // object, not bytes a struct can hold by value.
        (
            "module m\nrecord H\n  x: int\nend\nrecord R is c\n  h: H\nend\nsub main\nend\n",
            "heap record",
        ),
        // A c-record that nests itself has no size.
        (
            "module m\nrecord R is c\n  x: int\n  self: R\nend\nsub main\nend\n",
            "contains itself",
        ),
        // ...including through an inline array of itself.
        (
            "module m\nrecord R is c\n  x: int\n  kids: R[2]\nend\nsub main\nend\n",
            "contains itself",
        ),
        // A literal index past the end is a bug the program need not run to
        // reveal: the count is part of the type.
        (
            "module m\nrecord R is c\n  b: byte[4]\nend\nsub main\n  var r: R\n  \
             call print_int(r.b[5])\nend\n",
            "past the end",
        ),
        // A `const` index past the end is as visible a mistake as a literal
        // one: the count is part of the type either way.
        (
            "module m\nconst SLOT = 40\nrecord R is c\n  b: byte[4]\nend\nsub main\n  \
             var r: R\n  call print_int(r.b[SLOT])\nend\n",
            "past the end",
        ),
        // An array field holds at least one element.
        (
            "module m\nrecord R is c\n  b: byte[0]\nend\nsub main\nend\n",
            "at least one element",
        ),
        // A whole nested struct is not a value that can be assigned.
        (
            "module m\nrecord P is c\n  x: int\nend\nrecord R is c\n  p: P\nend\n\
             sub main\n  var r: R\n  var q: P\n  r.p = q\nend\n",
            "nested c-record",
        ),
        // Nor is a whole inline array.
        (
            "module m\nrecord R is c\n  b: byte[4]\nend\nsub main\n  var r: R\n  \
             r.b = 1\nend\n",
            "inline array",
        ),
        // A path assignment reaches into a c-record and nothing else.
        (
            "module m\nrecord H\n  x: int\nend\nsub main\n  var xs: H[] = []\n  \
             xs[1].x = 2\nend\n",
            "not a c-record",
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
        assert!(!out.status.success(), "case {i} should not build:\n{src}");
        let msg = String::from_utf8_lossy(&out.stderr);
        assert!(
            msg.contains(needle),
            "case {i}: expected a message containing {needle:?}, got:\n{msg}"
        );
    }
}
