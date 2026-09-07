//! The two halves of 0.7.0, proved together.
//!
//! `call through` calls an address; `band`, `bor`, `bxor`, `shl` and `ushr`
//! work on the bits of a word, and a hex literal writes one down. Each has its
//! own tests — `indirect.rs` and `bitwise.rs`. This file is the join, because
//! the reason both arrived at once is that neither is much use alone: an
//! address is chosen out of a table by testing a bit, and the flag word handed
//! to the function that address belongs to is built by combining constants.
//!
//! Two programs, one per platform, and both are self-checking transcripts:
//!
//! * `examples/dll/dispatch.kiln` opens `libops.so` (from `examples/dll/ops.c`),
//!   which is neither linked against nor declared, fetches five same-shaped
//!   exports into a `ptr[5]`, and lets a request word decide which to call —
//!   then checks C's `&`, `|`, `^`, `<<` and `>>` against Kiln's own.
//! * `examples/win/flags.kiln` asks `GetProcAddress` for `GetCurrentProcessId`
//!   and calls it, checking the answer against the same function reached as a
//!   declared import, and combines and tests the `win` kit's real constants —
//!   including handing `VirtualAlloc` and `OpenProcess` words built with `bor`.

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

/// A scratch directory per test. The Linux program opens its plug-in by a
/// relative path, so the library and the binary share one place.
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kiln_interop07_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the scratch directory");
    dir
}

/// Build an example into `dir/<name>`. The working directory is the
/// repository, which is how `use win` finds `kits/win`.
fn build(src: &Path, out: &Path, extra: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", src.to_str().unwrap()])
        .args(extra)
        .args(["-o", out.to_str().unwrap()])
        .current_dir(repo())
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln build")
}

// --- Linux: a bit picks the slot, and the slot is called --------------------

/// The whole combined path on Linux, end to end.
///
/// `0xDEAD_BEEF` is a 32-bit pattern with its top bit set, so as an `int` it is
/// negative — which is what makes `ushr` and `shr` disagree, and what makes
/// checking Kiln's shifts against C's worth doing. The request word leaves
/// the left shift out, so the transcript says `skipped shl` before a second
/// `bor` turns that slot back on.
#[test]
fn a_bit_chooses_the_slot_and_the_address_is_called() {
    let dir = scratch("posix");
    let status = Command::new("clang")
        .args(["-shared", "-fPIC", "-o"])
        .arg(dir.join("libops.so"))
        .arg(repo().join("examples/dll/ops.c"))
        .status()
        .expect("run clang for libops.so");
    assert!(status.success(), "clang failed to build libops.so");

    let bin = dir.join("dispatch");
    let built = build(&repo().join("examples/dll/dispatch.kiln"), &bin, &[]);
    assert!(
        built.status.success(),
        "dispatch.kiln did not build:\n{}",
        String::from_utf8_lossy(&built.stderr)
    );

    // From `dir`, because the program opens `./libops.so`.
    let out = Command::new(&bin).current_dir(&dir).output().expect("run the program");
    assert!(
        out.status.success(),
        "the program exited non-zero:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lines: Vec<String> =
        String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect();
    assert_eq!(
        lines,
        vec![
            "ok and = 4",                                    // 0xDEADBEEF & 4
            "ok or = -559038737",                            // 0xDEADBEEF | 4
            "ok xor = -559038741",                           // 0xDEADBEEF ^ 4
            "skipped shl",                                   // its bit was clear
            "ok ushr = 233495534",                           // zero-filled
            "ok shr keeps the sign where ushr does not",
            "ok shl = -354685200",                           // the bit, turned back on
            "0",                                             // dlclose
        ],
        "unexpected transcript from the dispatch table"
    );
}

// --- Windows: the win kit's own constants, under wine -----------------------

fn mingw_present() -> bool {
    if on_path("x86_64-w64-mingw32-gcc") {
        return true;
    }
    eprintln!("x86_64-w64-mingw32-gcc is not installed; skipping the Windows combined test");
    false
}

/// The same two features against the API they were added for.
///
/// Every line is a check the win kit could not make when it was written: an
/// address from `GetProcAddress` that is actually called, and a constant asked
/// which of its bits are set. The identity check is the load-bearing one —
/// `GetCurrentProcessId` reached as a declared import and reached as an address
/// must answer the same number, and only one process id exists to answer with.
#[test]
fn the_win_kit_calls_an_address_and_tests_its_own_flags_under_wine() {
    if !mingw_present() {
        return;
    }
    let dir = scratch("windows");
    let out = dir.join("flags");
    let built = build(&repo().join("examples/win/flags.kiln"), &out, &["--os", "windows"]);
    assert!(
        built.status.success(),
        "flags.kiln did not cross-build:\n{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let image = dir.join("flags.exe");
    assert!(image.is_file(), "expected {} to be written", image.display());

    if !on_path("wine") {
        eprintln!("wine is not installed; the PE was built but not run");
        return;
    }
    // `DISPLAY` and `WAYLAND_DISPLAY` are removed so wine takes its null
    // display driver and nothing reaches the screen of whoever is working
    // here; `timeout` bounds a program that wedges to this one test.
    let mut cmd = if on_path("timeout") {
        let mut c = Command::new("timeout");
        c.arg("60").arg("wine").arg(&image);
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
    let lines: Vec<String> =
        stdout.lines().map(|l| l.trim_end_matches('\r').to_string()).collect();
    assert_eq!(
        lines,
        vec![
            "ok GetProcAddress found GetCurrentProcessId",
            "ok the address answers what the import does",
            "ok a missing export is null",
            "ok MEM_COMMIT bor MEM_RESERVE is the pair",
            "ok bor agrees with + on disjoint flags",
            "ok WS_OVERLAPPEDWINDOW has a border",
            "ok WS_OVERLAPPEDWINDOW is not visible",
            "ok WS_POPUP is the top bit",
            "ok a flag can be cleared",
            "ok LOWORD",
            "ok HIWORD",
            "ok VirtualAlloc took a combined type word",
            "ok the page holds what was written",
            "ok VirtualFree",
            "ok OpenProcess took a combined access mask",
            "ok CloseHandle",
            "ok FreeLibrary",
        ],
        "unexpected transcript from the Windows program"
    );
}
