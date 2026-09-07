//! The `win` kit, whole: `use win` and nothing else, held to what Windows
//! actually does.
//!
//! The five subsystem suites beside this one (`win_user32`, `win_gdi32`,
//! `win_kernel32_proc`, `win_kernel32_mem`, `win_advapi32`) each prove one
//! `.kdecl` file against a scratch kit holding only that file. This one proves
//! the opposite thing: that the five files merge into a single bundle with no
//! name fighting another, that `use win` resolves it, and — the part that
//! decides whether the kit is finished — that a real Windows program can be
//! written with `use win` as its only foreign declaration.
//!
//! `examples/win/` is that program, five times over. Not one of those files
//! contains a `dll`, a `record ... is c` or a `const` line: every struct,
//! every number and every entry point comes from the kit. If the kit were
//! short of anything they need, they would not build.
//!
//! They are cross-built with mingw and run under wine, because a Win32
//! binding cannot be proved by parsing it. A struct field at the wrong
//! offset, a `DWORD` declared `int64`, an entry point spelled the way the
//! documentation prints it rather than the way the DLL exports it — none of
//! those fail a build, and all of them fail the moment Windows fills a struct
//! or dispatches a message.
//!
//! ## The display
//!
//! wine is run with `DISPLAY` and `WAYLAND_DISPLAY` removed from the
//! environment, so it falls back to its null display driver. Two consequences,
//! and both matter:
//!
//! * Nothing appears on the screen of whoever is working on this machine. A
//!   test must never steal a window or a focus.
//! * A window is still *created* and its messages are still *delivered* — the
//!   null driver is a driver. `window.kiln` registers a class, creates a
//!   window, pumps messages, and its WNDPROC is called back with WM_PAINT and
//!   WM_DESTROY, all of which this suite checks. What is not checked, because
//!   there is no framebuffer to read, is that the pixels TextOutA drew are the
//!   pixels intended.
//!
//! Every test says why it is skipping when the cross compiler or wine is not
//! installed, so a green run on a machine without them is not mistaken for
//! proof.

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

fn mingw_present() -> bool {
    if on_path("x86_64-w64-mingw32-gcc") {
        return true;
    }
    eprintln!("x86_64-w64-mingw32-gcc is not installed; skipping the win kit cross-build");
    false
}

fn wine_present() -> bool {
    if on_path("wine") {
        return true;
    }
    eprintln!("wine is not installed; the PE was built but not run");
    false
}

/// A scratch directory per test. The program finds its runtime beside its own
/// executable, so the build output and anything it loads share one directory,
/// and two tests must not share it.
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kiln_win_kit_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the scratch directory");
    dir
}

/// Build `examples/win/<name>.kiln` for Windows into `dir`.
///
/// The working directory is the repository, which is how `use win` finds
/// `kits/win`: a kit is resolved from the directory the build runs in, not
/// from the directory the source sits in.
fn build_example(dir: &Path, name: &str) -> PathBuf {
    let src = repo().join("examples/win").join(format!("{name}.kiln"));
    assert!(src.is_file(), "missing example {}", src.display());
    let out = dir.join(name);
    let done = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", src.to_str().unwrap(), "--os", "windows", "-o", out.to_str().unwrap()])
        .current_dir(repo())
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln build");
    assert!(
        done.status.success(),
        "{name}.kiln did not build for windows:\n{}",
        String::from_utf8_lossy(&done.stderr)
    );
    let image = dir.join(format!("{name}.exe"));
    assert!(image.is_file(), "expected {} to be written", image.display());
    image
}

/// The image's own headers, not `file`'s opinion of them: the DOS signature,
/// the PE offset it carries, the machine word, and the optional header's
/// magic — `0x20B` is what "PE32+" means.
fn assert_pe32_plus(path: &Path) {
    let bytes = std::fs::read(path).expect("read the built image");
    assert!(bytes.len() > 0x100, "{} is too small to be a PE", path.display());
    assert_eq!(&bytes[0..2], b"MZ", "{}: no DOS stub signature", path.display());
    let pe = u32::from_le_bytes(bytes[0x3c..0x40].try_into().unwrap()) as usize;
    assert_eq!(&bytes[pe..pe + 4], b"PE\0\0", "{}: no PE signature", path.display());
    let machine = u16::from_le_bytes(bytes[pe + 4..pe + 6].try_into().unwrap());
    assert_eq!(machine, 0x8664, "{}: machine is not x86-64", path.display());
    let magic = u16::from_le_bytes(bytes[pe + 24..pe + 26].try_into().unwrap());
    assert_eq!(magic, 0x20B, "{}: optional header is not PE32+", path.display());
}

/// Run a built `.exe` under wine and answer its stdout, with the carriage
/// returns Windows adds trimmed off.
///
/// `DISPLAY` and `WAYLAND_DISPLAY` are removed so wine uses its null display
/// driver: nothing reaches the screen of whoever is working on this machine.
/// `timeout` bounds it, because a message loop that never sees WM_QUIT would
/// otherwise hang the whole suite rather than fail one test.
fn run_under_wine(dir: &Path, image: &Path, seconds: &str) -> Vec<String> {
    let mut cmd = if on_path("timeout") {
        let mut c = Command::new("timeout");
        c.arg(seconds).arg("wine").arg(image);
        c
    } else {
        let mut c = Command::new("wine");
        c.arg(image);
        c
    };
    let out = cmd
        .current_dir(dir)
        .env("WINEDEBUG", "-all")
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .output()
        .expect("run the image under wine");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "{} exited {:?} under wine\nstdout:\n{stdout}\nstderr:\n{}",
        image.display(),
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    stdout.lines().map(|l| l.trim_end_matches('\r').to_string()).collect()
}

/// The self-checking examples print `<label> ok` or `<label> FAILED`. Nothing
/// may fail, and `done` must be the last line — a program that stopped early
/// reports no failure at all.
fn assert_no_failures(name: &str, lines: &[String]) {
    let failed: Vec<&String> = lines.iter().filter(|l| l.contains("FAILED")).collect();
    assert!(failed.is_empty(), "{name} reported failures:\n{failed:?}\nfull output:\n{}", lines.join("\n"));
    assert_eq!(lines.last().map(String::as_str), Some("done"), "{name} did not run to the end:\n{}", lines.join("\n"));
}

/// The five examples all cross-build for Windows against the merged kit, and
/// what comes out is a PE32+ image for x86-64.
///
/// This is the collision check as much as the build check: five `.kdecl` files
/// written by five hands merge into one namespace, and a name declared twice
/// would stop every one of these before it reached the linker.
#[test]
fn every_example_cross_builds_for_windows() {
    if !mingw_present() {
        return;
    }
    let dir = scratch("build");
    // Every `.kiln` the directory holds, so an example added later is covered
    // without this test being edited to know about it.
    let mut names: Vec<String> = std::fs::read_dir(repo().join("examples/win"))
        .expect("read examples/win")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter_map(|n| n.strip_suffix(".kiln").map(str::to_string))
        .collect();
    names.sort();
    assert!(names.len() >= 5, "examples/win lost files: {names:?}");
    for name in &names {
        let image = build_example(&dir, name);
        assert_pe32_plus(&image);
    }
}

/// A program written against `use win` alone builds for the CONSOLE subsystem,
/// and there is no way to ask for the GUI one.
///
/// `--target gui` is refused for a module with no `form`, because the GUI
/// target is Kiln's own UI stack rather than a subsystem switch — so a raw
/// Win32 program gets a console window beside its own window on a real
/// Windows desktop. That is a gap rather than a bug, and it is pinned here so
/// that closing it is a visible change rather than a silent one.
#[test]
fn a_raw_win32_program_is_a_console_subsystem_image() {
    if !mingw_present() {
        return;
    }
    let dir = scratch("subsystem");
    let image = build_example(&dir, "window");
    let bytes = std::fs::read(&image).expect("read the built image");
    let pe = u32::from_le_bytes(bytes[0x3c..0x40].try_into().unwrap()) as usize;
    // The optional header's Subsystem field: 3 is console, 2 is GUI.
    let subsystem = u16::from_le_bytes(bytes[pe + 0x5c..pe + 0x5e].try_into().unwrap());
    assert_eq!(subsystem, 3, "expected a console-subsystem image");

    let refused = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", repo().join("examples/win/window.kiln").to_str().unwrap(),
               "--os", "windows", "--target", "gui",
               "-o", dir.join("gui").to_str().unwrap()])
        .current_dir(repo())
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln build");
    assert!(!refused.status.success(), "`--target gui` unexpectedly built a formless module");
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("declares no form"),
        "the refusal should say why, got:\n{}",
        String::from_utf8_lossy(&refused.stderr)
    );
}

/// The memory proof: a process handle from `OpenProcess`, bytes read back out
/// of that process with `ReadProcessMemory` and written into it with
/// `WriteProcessMemory`, the region `VirtualQuery` describes, and a symbol
/// found in a DLL loaded by name.
///
/// The value that comes back is the value that went in — through kernel32,
/// not off the pointer — which is what makes this a binding proof rather than
/// a memory-write-and-read.
#[test]
fn meminfo_reads_its_own_memory_under_wine() {
    if !mingw_present() || !wine_present() {
        return;
    }
    let dir = scratch("meminfo");
    let image = build_example(&dir, "meminfo");
    let lines = run_under_wine(&dir, &image, "180");
    assert_no_failures("meminfo", &lines);
    assert!(
        lines.iter().any(|l| l == "value 305419896"),
        "the sentinel did not come back through ReadProcessMemory:\n{}",
        lines.join("\n")
    );
    assert!(
        lines.iter().any(|l| l == "region 4096"),
        "VirtualQuery did not describe one page:\n{}",
        lines.join("\n")
    );
}

/// The window proof: a class registered, a window created, a message loop
/// pumped, and a Kiln subroutine called back by Windows itself with
/// WM_PAINT and WM_DESTROY.
///
/// `paint` and `destroy` are printed from *inside* the WNDPROC, so seeing them
/// is seeing Windows dispatch into `address of wndproc`. What this cannot
/// check is the drawing: wine's null display driver has no framebuffer to read
/// back, so `TextOutA` is proved to have been called and to have returned,
/// not to have put the right pixels anywhere.
#[test]
fn window_pumps_messages_under_wine() {
    if !mingw_present() || !wine_present() {
        return;
    }
    let dir = scratch("window");
    let image = build_example(&dir, "window");
    let lines = run_under_wine(&dir, &image, "180");
    let has = |needle: &str| lines.iter().any(|l| l == needle);

    assert!(has("registered"), "RegisterClassExA failed:\n{}", lines.join("\n"));
    assert!(has("created"), "CreateWindowExA failed:\n{}", lines.join("\n"));
    assert!(has("paint"), "the WNDPROC never saw WM_PAINT:\n{}", lines.join("\n"));
    assert!(has("destroy"), "the WNDPROC never saw WM_DESTROY:\n{}", lines.join("\n"));
    assert!(has("destroys 1"), "WM_DESTROY should arrive exactly once:\n{}", lines.join("\n"));
    assert!(
        lines.iter().any(|l| l.starts_with("paints ") && l != "paints 0"),
        "WM_PAINT should have been handled at least once:\n{}",
        lines.join("\n")
    );

    // The loop left through WM_QUIT rather than by running out of budget: the
    // frame count printed at the end is below the ceiling the example sets.
    let frames: i64 = lines
        .iter()
        .find_map(|l| l.strip_prefix("frames "))
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no frame count printed:\n{}", lines.join("\n")));
    assert!(frames < 400, "the pump ran out its budget instead of seeing WM_QUIT: {frames}");
}

/// The registry proof: a key created, a DWORD and a string written into it,
/// both read back with the in-out size cell the API insists on, and the key
/// deleted again — checked by reopening it and finding it gone.
#[test]
fn registry_round_trips_under_wine() {
    if !mingw_present() || !wine_present() {
        return;
    }
    let dir = scratch("registry");
    let image = build_example(&dir, "registry");
    let lines = run_under_wine(&dir, &image, "180");
    assert_no_failures("registry", &lines);
    assert!(
        lines.iter().any(|l| l == "product Kiln"),
        "the REG_SZ did not come back:\n{}",
        lines.join("\n")
    );
}

/// The process proof: a thread whose ThreadProc is a Kiln subroutine
/// Windows calls on a stack it made, and a child process started through the
/// STARTUPINFOA / PROCESS_INFORMATION pair whose exit code comes back.
#[test]
fn spawn_starts_a_thread_and_a_process_under_wine() {
    if !mingw_present() || !wine_present() {
        return;
    }
    let dir = scratch("spawn");
    let image = build_example(&dir, "spawn");
    let lines = run_under_wine(&dir, &image, "180");
    assert_no_failures("spawn", &lines);
}

/// `kiln commands --use win` lists the merged bundle, so Studio's
/// completion, the language server and the generated reference all see one
/// kit rather than five files.
///
/// The names checked are one from each `.kdecl`, plus the record and constant
/// spellings a program is written against.
#[test]
fn commands_lists_the_merged_bundle() {
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["commands", "--use", "win"])
        .current_dir(repo())
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln commands");
    assert!(
        out.status.success(),
        "commands --use win failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    let has = |needle: &str| text.lines().any(|l| l.contains(needle));

    // One entry point from each subsystem file.
    for needle in [
        "dll: MessageBoxA",        // user32
        "dll: CreateWindowExA",    // user32
        "dll: TextOutA",           // gdi32
        "dll: CreateProcessA",     // kernel32_proc
        "dll: ReadProcessMemory",  // kernel32_mem
        "dll: VirtualAlloc",       // kernel32_mem
        "dll: RegOpenKeyExA",      // advapi32
    ] {
        assert!(has(needle), "`{needle}` is missing from the merged bundle:\n{text}");
    }

    // The structs and numbers a program writes beside them.
    for needle in [
        "crecord: WNDCLASSEXA",
        "crecord: MSG",
        "crecord: PAINTSTRUCT",
        "crecord: STARTUPINFOA",
        "crecord: MEMORY_BASIC_INFORMATION",
        "const: WM_PAINT",
        "const: HKEY_CURRENT_USER",
        "const: PAGE_READWRITE",
    ] {
        assert!(has(needle), "`{needle}` is missing from the merged bundle:\n{text}");
    }

    // A bundle this size is the point of a kit; a listing that collapsed to a
    // handful would mean one file stopped merging without anything erroring.
    let dlls = text.lines().filter(|l| l.starts_with("dll: ")).count();
    assert!(dlls > 300, "only {dlls} dll declarations merged — a file is missing:\n{text}");
}

/// `use win` on a build that is not for Windows is one sentence naming the
/// kit, the platform it supports and the flag to pass — not a wall of linker
/// errors about symbols nobody wrote.
#[test]
fn the_win_kit_is_refused_for_linux() {
    let dir = scratch("gate");
    let src = repo().join("examples/win/meminfo.kiln");
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", src.to_str().unwrap(), "--os", "linux", "-o", dir.join("meminfo").to_str().unwrap()])
        .current_dir(repo())
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln build");
    assert!(!out.status.success(), "a windows-only kit must not build for linux");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("kit `win` supports windows"),
        "the refusal must name the kit and the platform it supports, got:\n{err}"
    );
    assert!(
        err.contains("--os windows"),
        "the refusal must say which flag to build with, got:\n{err}"
    );
}
