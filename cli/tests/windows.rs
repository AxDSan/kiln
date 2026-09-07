//! Cross-building for Windows: `kiln build --os windows` produces a PE32+
//! image through mingw-w64, and — where wine is installed — that image runs
//! and says what the Linux build says.
//!
//! Every test skips itself with a line saying why when the cross compiler is
//! not on this machine: a checkout without mingw is a machine that cannot
//! build for Windows, not a broken toolchain.
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
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Absent, and the test has nothing to build with. Said out loud rather than
/// passed in silence, so a green run on a machine without mingw is not
/// mistaken for proof.
fn mingw_present() -> bool {
    if on_path("x86_64-w64-mingw32-gcc") {
        return true;
    }
    eprintln!("x86_64-w64-mingw32-gcc is not installed; skipping the Windows cross-build test");
    false
}

/// A scratch directory of its own per test, because the Windows program's
/// working files land wherever it is run, and two tests must not share one.
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kiln_windows_{tag}_test"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Build `source` for Windows into `dir`, with `extra` on the command line.
fn build_windows(source: &Path, out: &Path, extra: &[&str]) {
    let repo = repo();
    let status = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", source.to_str().unwrap(), "--os", "windows", "-o"])
        .arg(out)
        .args(extra)
        .env("KILN_RUNTIME_DIR", repo.join("runtime"))
        .status()
        .expect("run kiln");
    assert!(status.success(), "kiln build --os windows failed for {}", source.display());
}

/// Read the image's own headers, not `file`'s opinion of them: `MZ`, the
/// `e_lfanew` pointer at 0x3C, `PE\0\0` where it points, an x86-64 machine
/// word, and the optional header's magic — `0x20B` is what "PE32+" means.
fn assert_pe32_plus(image: &Path) {
    let bytes = std::fs::read(image).expect("read the built image");
    assert!(bytes.len() > 0x40, "image is too short to be a PE file");
    assert_eq!(&bytes[0..2], b"MZ", "no DOS stub signature");
    let pe = u32::from_le_bytes([bytes[0x3C], bytes[0x3D], bytes[0x3E], bytes[0x3F]]) as usize;
    assert!(pe + 26 <= bytes.len(), "e_lfanew points outside the file");
    assert_eq!(&bytes[pe..pe + 4], b"PE\0\0", "no PE signature at e_lfanew");
    let machine = u16::from_le_bytes([bytes[pe + 4], bytes[pe + 5]]);
    assert_eq!(machine, 0x8664, "machine is not x86-64");
    let magic = u16::from_le_bytes([bytes[pe + 24], bytes[pe + 25]]);
    assert_eq!(magic, 0x20B, "optional header is not PE32+");
}

/// Run the image under wine when it is here; `None` when it is not.
///
/// CRLF is what a Windows console program writes, and `lines()` strips it, so
/// the comparison is against what was said and not how the line ended.
fn wine_lines(image: &Path, cwd: &Path) -> Option<Vec<String>> {
    if !on_path("wine") {
        eprintln!("wine is not installed; the Windows image was built but not run");
        return None;
    }
    let out = Command::new("wine")
        .arg(image)
        .current_dir(cwd)
        .env("WINEDEBUG", "-all")
        .output()
        .expect("run wine");
    assert!(
        out.status.success(),
        "the Windows program exited non-zero under wine:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect(),
    )
}

#[test]
fn hello_cross_builds_to_pe32_plus_and_runs_under_wine() {
    if !mingw_present() {
        return;
    }
    let dir = scratch("hello");
    // `-o hello` on purpose: a Windows program named without an extension is
    // a file Windows will not run, so the build adds one.
    build_windows(&repo().join("examples/hello.kiln"), &dir.join("hello"), &[]);
    let image = dir.join("hello.exe");
    assert!(image.is_file(), "expected {} to be written", image.display());
    assert_pe32_plus(&image);

    if let Some(lines) = wine_lines(&image, &dir) {
        assert_eq!(
            lines,
            vec!["Kiln — arithmetic demo", "42", "14", "42", "42"],
            "unexpected output under wine"
        );
    }
}

/// A release build is the profile someone ships, so it is the one that must
/// cross-build: the hardening flags are a different set on PE, and a flag the
/// Linux profile takes for granted would fail the mingw link outright.
#[test]
fn release_cross_build_is_a_pe_that_runs() {
    if !mingw_present() {
        return;
    }
    let dir = scratch("release");
    build_windows(
        &repo().join("examples/hello.kiln"),
        &dir.join("hello.exe"),
        &["--release"],
    );
    let image = dir.join("hello.exe");
    assert_pe32_plus(&image);
    if let Some(lines) = wine_lines(&image, &dir) {
        assert_eq!(lines[0], "Kiln — arithmetic demo");
        assert_eq!(lines.len(), 5);
    }
}

/// The support libraries a console program reaches for most — files and the
/// environment — cross-build and answer the same, with one deliberate
/// difference: the program knows which operating system it is on.
///
/// Written here rather than taken from `examples/`: those print paths and
/// sizes, and a path is spelt differently on Windows while a size depends on
/// how the C library ends a line. Everything below is a yes, a count, or a
/// value the program put there itself.
#[test]
fn file_and_system_libraries_cross_build_and_run() {
    if !mingw_present() {
        return;
    }
    let dir = scratch("libs");
    let source = dir.join("winlibs.kiln");
    std::fs::write(
        &source,
        "module winlibs\n\
         use file\n\
         use system\n\
         \n\
         sub main\n\
         \x20 let note: text = \"winlibs-note.txt\"\n\
         \x20 if file_write_text(note, \"alpha\\nbeta\\ngamma\\n\")\n\
         \x20   call print_text(\"wrote\")\n\
         \x20 end\n\
         \x20 call print_int(file_line_count(note))\n\
         \x20 if file_exists(note) and not dir_exists(note)\n\
         \x20   call print_text(\"a file\")\n\
         \x20 end\n\
         \x20 if file_delete(note) and not file_exists(note)\n\
         \x20   call print_text(\"deleted\")\n\
         \x20 end\n\
         \x20 if env_set(\"KILN_WIN\", \"hello\")\n\
         \x20   call print_text(concat(\"env: \", env_get(\"KILN_WIN\")))\n\
         \x20 end\n\
         \x20 call print_int(sys_arg_count())\n\
         \x20 call print_int64(int_to_int64(2000000000) + int_to_int64(2000000000))\n\
         \x20 call print_text(os_name())\n\
         end\n",
    )
    .expect("write the sample");
    build_windows(&source, &dir.join("winlibs.exe"), &[]);
    let image = dir.join("winlibs.exe");
    assert_pe32_plus(&image);

    if let Some(lines) = wine_lines(&image, &dir) {
        assert_eq!(
            lines,
            vec![
                "wrote",
                "3",
                "a file",
                "deleted",
                "env: hello",
                "0",
                // Past 32 bits: an int64 that came back truncated would say
                // something else here, and nothing in hello.kiln would notice.
                "4000000000",
                "windows",
            ],
            "unexpected output under wine"
        );
        assert!(
            !dir.join("winlibs-note.txt").exists(),
            "the program's scratch file was not cleaned up"
        );
    }
}

/// A library cross-builds to the names Windows expects, and the DLL carries
/// an export table — a `.dll` nobody can link against is the wrong shape with
/// the right extension.
#[test]
fn library_cross_builds_to_dll_and_archive() {
    if !mingw_present() {
        return;
    }
    let dir = scratch("lib");
    let source = repo().join("examples/hellolib.kiln");
    let dll = dir.join("hellolib.dll");
    build_windows(&source, &dll, &["--target", "sharedlib"]);
    assert_pe32_plus(&dll);
    let bytes = std::fs::read(&dll).unwrap();
    let pe = u32::from_le_bytes([bytes[0x3C], bytes[0x3D], bytes[0x3E], bytes[0x3F]]) as usize;
    // Characteristics: bit 0x2000 is IMAGE_FILE_DLL.
    let characteristics = u16::from_le_bytes([bytes[pe + 22], bytes[pe + 23]]);
    assert_ne!(characteristics & 0x2000, 0, "the .dll is not marked as a DLL");
    // The export directory is the first data directory, 112 bytes into the
    // PE32+ optional header; a size of zero means nothing is exported.
    let opt = pe + 24;
    let export_size = u32::from_le_bytes([
        bytes[opt + 116],
        bytes[opt + 117],
        bytes[opt + 118],
        bytes[opt + 119],
    ]);
    assert_ne!(export_size, 0, "the DLL exports nothing");

    let archive = dir.join("libhellolib.a");
    build_windows(&source, &archive, &["--target", "staticlib"]);
    let bytes = std::fs::read(&archive).unwrap();
    assert!(bytes.starts_with(b"!<arch>\n"), "the static library is not an ar archive");
}

/// The `ptr` type and the raw-memory commands must marshal identically on
/// Windows: the slot layout is ABI, and a pointer travels in the slot's union
/// exactly as it does on Linux. Building `examples/ptr.kiln` for Windows and
/// running it under wine proves the whole path — int64 offsets, the int<->ptr
/// escape hatch, and pointer-width reads — behaves the same on LLP64.
#[test]
fn ptr_cross_builds_and_runs_under_wine() {
    if !mingw_present() {
        return;
    }
    let dir = scratch("ptr");
    build_windows(&repo().join("examples/ptr.kiln"), &dir.join("ptr.exe"), &[]);
    let image = dir.join("ptr.exe");
    assert!(image.is_file(), "expected {} to be written", image.display());
    assert_pe32_plus(&image);

    if let Some(lines) = wine_lines(&image, &dir) {
        assert_eq!(
            lines,
            vec![
                "42", "9000000000", "255", "3.5", "hello, C", "", "7", "7",
                "same", "null-ok", "123", "Kiln",
            ],
            "unexpected ptr output under wine"
        );
    }
}
