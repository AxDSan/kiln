//! Kiln Studio cross-built for Windows: `designer/build-windows.sh`
//! produces a PE32+ image for the GUI subsystem with the DLLs it imports
//! beside it, and — where wine is installed — that image gets through
//! Windows' loader and as far as this machine's display lets it, which with
//! the display drivers turned off is SDL asking for a window.
//!
//! The layer under Studio's build, run, stop and language-server code —
//! `designer/portable.h`, a child process with pipes read without blocking —
//! is what a windowless run CAN exercise, so `designer/test_portable.cpp`
//! is built as a console program and run both on the host and under wine.
//! That is the evidence for CreateProcess and PeekNamedPipe; "it compiled"
//! is not.
//!
//! Every test skips itself with a line saying why when what it needs is not
//! here: the cross compiler, the Windows build of RmlUi
//! (`tools/build-rmlui-windows.sh`), the mingw SDL2 packages, or wine. A
//! green run on a machine without them is a machine that cannot build Studio
//! for Windows, not proof that it can.
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

fn windows_ui_present() -> bool {
    if !on_path("x86_64-w64-mingw32-g++") {
        eprintln!("x86_64-w64-mingw32-g++ is not installed; skipping the Windows Studio test");
        return false;
    }
    let rmlui = repo().join("vendor/RmlUi/build-windows/librmlui.a");
    if !rmlui.is_file() {
        eprintln!(
            "{} is not there (run tools/build-rmlui-windows.sh); skipping the Windows Studio test",
            rmlui.display()
        );
        return false;
    }
    let sdl = Command::new("x86_64-w64-mingw32-pkg-config")
        .args(["--exists", "sdl2", "SDL2_image", "freetype2"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !sdl {
        eprintln!("the mingw-w64 SDL2, SDL2_image and freetype packages are not installed; skipping the Windows Studio test");
        return false;
    }
    true
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kiln_studio_windows_{tag}_test"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// PE32+ for x86-64, and the optional header's `Subsystem`: 2 is the Windows
/// GUI subsystem, 3 the console. Studio linked for the console would open a
/// black window behind itself on Windows.
fn pe_subsystem(image: &Path) -> u16 {
    let bytes = std::fs::read(image).expect("read the built image");
    assert!(bytes.len() > 0x40, "image is too short to be a PE file");
    assert_eq!(&bytes[0..2], b"MZ", "no DOS stub signature");
    let pe = u32::from_le_bytes([bytes[0x3C], bytes[0x3D], bytes[0x3E], bytes[0x3F]]) as usize;
    assert!(pe + 24 + 70 <= bytes.len(), "e_lfanew points outside the file");
    assert_eq!(&bytes[pe..pe + 4], b"PE\0\0", "no PE signature at e_lfanew");
    let machine = u16::from_le_bytes([bytes[pe + 4], bytes[pe + 5]]);
    assert_eq!(machine, 0x8664, "machine is not x86-64");
    let opt = pe + 24;
    let magic = u16::from_le_bytes([bytes[opt], bytes[opt + 1]]);
    assert_eq!(magic, 0x20B, "optional header is not PE32+");
    u16::from_le_bytes([bytes[opt + 68], bytes[opt + 69]])
}

fn dlls_beside(image: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(image.parent().unwrap())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.to_ascii_lowercase().ends_with(".dll"))
        .collect();
    out.sort();
    out
}

/// Run under wine with its display drivers turned OFF, so no test here can
/// put a window on the screen of whoever is working on this machine. `None`
/// when wine is not here.
fn wine(image: &Path, args: &[&str], cwd: &Path, env: &[(&str, &str)]) -> Option<std::process::Output> {
    if !on_path("wine") {
        eprintln!("wine is not installed; the Windows image was built but not run");
        return None;
    }
    let mut cmd = if on_path("timeout") {
        let mut c = Command::new("timeout");
        c.arg("120").arg("wine");
        c
    } else {
        Command::new("wine")
    };
    cmd.arg(image)
        .args(args)
        .current_dir(cwd)
        .env("WINEDEBUG", "-all")
        .env("WINEDLLOVERRIDES", "winex11.drv,winewayland.drv=d")
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY");
    for (k, v) in env {
        cmd.env(k, v);
    }
    Some(cmd.output().expect("run wine"))
}

fn probe_source() -> PathBuf {
    repo().join("designer/test_portable.cpp")
}

/// The portable layer's POSIX half, on the machine the suite runs on: the
/// same probe, so the two halves are held to the same checks.
#[test]
fn portable_layer_probe_passes_on_the_host() {
    if !on_path("clang++") {
        eprintln!("clang++ is not installed; skipping the portable-layer probe");
        return;
    }
    let dir = scratch("host");
    let exe = dir.join("test_portable");
    let status = Command::new("clang++")
        .args(["-std=c++17", "-I"])
        .arg(repo().join("designer"))
        .arg(probe_source())
        .arg("-o")
        .arg(&exe)
        .status()
        .expect("run clang++");
    assert!(status.success(), "designer/test_portable.cpp does not compile on the host");
    let out = Command::new(&exe).env("TMPDIR", &dir).output().expect("run the probe");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("0 failure(s)"),
        "the portable-layer probe failed on the host:\n{stdout}"
    );
}

/// The Windows half — CreateProcess with pipes, PeekNamedPipe, TerminateProcess,
/// a conversation over a child's stdin — run under wine with no display, as
/// a console program. This is the check Studio's own image cannot give with
/// the drivers off, since Studio stops at its window.
#[test]
fn portable_layer_probe_passes_under_wine() {
    if !on_path("x86_64-w64-mingw32-g++") {
        eprintln!("x86_64-w64-mingw32-g++ is not installed; skipping the Windows portable-layer probe");
        return;
    }
    let dir = scratch("wine");
    let exe = dir.join("test_portable.exe");
    // -static: the probe is a console program with no DLL to ship beside it,
    // and libwinpthread would otherwise be one.
    let status = Command::new("x86_64-w64-mingw32-g++")
        .args(["-std=gnu++17", "-static", "-I"])
        .arg(repo().join("designer"))
        .arg(probe_source())
        .arg("-o")
        .arg(&exe)
        .status()
        .expect("run x86_64-w64-mingw32-g++");
    assert!(status.success(), "designer/test_portable.cpp does not cross-compile for Windows");
    let Some(out) = wine(&exe, &[], &dir, &[]) else { return };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("0 failure(s)"),
        "the portable-layer probe failed under wine (status {:?}):\n{stdout}\n{stderr}",
        out.status.code()
    );
}

/// Studio itself: a GUI-subsystem PE with SDL2, SDL2_image, freetype and
/// SDL3 beside it, that loads under wine — every DLL resolved, the runtime
/// entered — and stops where SDL asks the (disabled) display for a window.
#[test]
fn studio_cross_builds_to_a_gui_pe_with_its_dlls_and_loads_under_wine() {
    if !windows_ui_present() {
        return;
    }
    let dir = scratch("studio");
    let status = Command::new(repo().join("designer/build-windows.sh"))
        .env("OUT_DIR", &dir)
        .current_dir(repo())
        .stdout(std::process::Stdio::null())
        .status()
        .expect("run designer/build-windows.sh");
    assert!(status.success(), "designer/build-windows.sh failed");
    let image = dir.join("kiln-studio.exe");
    assert!(image.is_file(), "no kiln-studio.exe at {}", image.display());

    assert_eq!(pe_subsystem(&image), 2, "Studio must link for the GUI subsystem");
    let dlls = dlls_beside(&image);
    for want in ["SDL2.dll", "SDL2_image.dll", "libfreetype-6.dll", "SDL3.dll"] {
        assert!(dlls.contains(&want.to_string()), "{want} is not beside Studio; found {dlls:?}");
    }

    // A scripted session on a copy of an example, as the Linux Studio tests
    // run one; the recent list and the cache go to the scratch directory,
    // never to the person's %APPDATA%.
    let form = dir.join("form.kiln");
    std::fs::copy(repo().join("examples/form.kiln"), &form).unwrap();
    let dump = dir.join("studio.ppm");
    let xdg = dir.join("xdg");
    let Some(out) = wine(
        &image,
        &[form.to_str().unwrap()],
        &dir,
        &[
            ("KILN_DESIGNER_SCRIPT", "select:ok_button"),
            ("KILN_DESIGNER_DUMP", dump.to_str().unwrap()),
            ("XDG_DATA_HOME", xdg.to_str().unwrap()),
            ("XDG_CACHE_HOME", xdg.to_str().unwrap()),
        ],
    ) else {
        return;
    };
    let stderr = String::from_utf8_lossy(&out.stderr);
    // 53 is wine's loader answering STATUS_DLL_NOT_FOUND — the failure this
    // test exists to catch — and 124 is `timeout`'s.
    let code = out.status.code();
    assert!(
        code != Some(53) && code != Some(124),
        "Studio did not get past the loader under wine (status {code:?}):\n{stderr}"
    );
    match code {
        // Not reached with the drivers off, but the shape a run with a
        // display takes: the session ran, and the frame was written.
        Some(0) => assert!(dump.is_file(), "Studio exited 0 but wrote no dump"),
        // No display driver: SDL could not open a window and said so, from
        // inside a program that had loaded every DLL and reached Studio's
        // own start-up. Anything else is a real failure.
        Some(1) => assert!(
            stderr.contains("SDL error on create window"),
            "exit 1 without SDL's window error — something else failed:\n{stderr}"
        ),
        other => panic!("unexpected exit status {other:?} under wine:\n{stderr}"),
    }
}
