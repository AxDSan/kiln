//! A windowed program cross-built for Windows: `kiln build --os windows`
//! on a module with a form produces a PE32+ image for the GUI subsystem, with
//! the DLLs it imports beside it, and — where wine is installed — that image
//! gets through Windows' loader and as far as this machine's display lets it.
//!
//! Under wine the display drivers are turned off, so no test here can put a
//! window on the screen of whoever is working on the machine — and so no
//! test here sees a drawn frame: the check stops at SDL asking for a window.
//!
//! Every test skips itself with a line saying why when what it needs is not
//! here: the cross compiler, the Windows build of RmlUi
//! (`tools/build-rmlui-windows.sh`), or the mingw SDL2 packages. A green run
//! on a machine without them is a machine that cannot build for Windows, not
//! proof that it can.
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

/// The cross compiler, the Windows RmlUi, and the mingw SDL2 headers: all
/// three, or the test says which is missing and stops.
fn windows_ui_present() -> bool {
    if !on_path("x86_64-w64-mingw32-gcc") {
        eprintln!("x86_64-w64-mingw32-gcc is not installed; skipping the Windows GUI cross-build test");
        return false;
    }
    let rmlui = repo().join("vendor/RmlUi/build-windows/librmlui.a");
    if !rmlui.is_file() {
        eprintln!(
            "{} is not there (run tools/build-rmlui-windows.sh); skipping the Windows GUI cross-build test",
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
        eprintln!("the mingw-w64 SDL2, SDL2_image and freetype packages are not installed; skipping the Windows GUI cross-build test");
        return false;
    }
    true
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kiln_windows_gui_{tag}_test"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

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

/// The image's own headers: PE32+ for x86-64, and the optional header's
/// `Subsystem` field — `2` is the Windows GUI subsystem, `3` the console.
/// A form linked for the console subsystem would open a black console window
/// behind itself on Windows, which is what this field guards against.
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

/// Run the image under wine with its display drivers turned OFF, so the test
/// can never put a window on the screen of whoever is working on this
/// machine. That also means no OpenGL: what remains provable is that the
/// program loads — every DLL resolved — and runs up to the point a window is
/// needed. `None` when wine is not here.
fn wine(image: &Path, cwd: &Path, env: &[(&str, &str)]) -> Option<std::process::Output> {
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

/// A binary PPM's pixel at (x, y), top-left origin — what `KILN_UI_DUMP`
/// writes.
fn ppm_pixel(path: &Path, x: usize, y: usize) -> (u8, u8, u8) {
    let bytes = std::fs::read(path).expect("read the dump");
    let mut fields: Vec<String> = Vec::new();
    let mut i = 0;
    while fields.len() < 4 {
        while bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let start = i;
        while !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        fields.push(String::from_utf8_lossy(&bytes[start..i]).to_string());
    }
    i += 1; // the single whitespace after the maxval
    assert_eq!(fields[0], "P6", "the dump is not a binary PPM");
    let w: usize = fields[1].parse().unwrap();
    let at = i + (y * w + x) * 3;
    (bytes[at], bytes[at + 1], bytes[at + 2])
}

#[test]
fn form_cross_builds_to_a_gui_subsystem_pe_with_its_dlls_beside_it() {
    if !windows_ui_present() {
        return;
    }
    let dir = scratch("form");
    let image = dir.join("form.exe");
    build_windows(&repo().join("examples/form.kiln"), &image, &[]);

    assert_eq!(pe_subsystem(&image), 2, "a form must link for the GUI subsystem");

    // The three the ui library links directly. Their own dependencies are
    // copied too, and the run below is what proves that list complete: a
    // DLL missing anywhere in the chain and the loader refuses the whole
    // program before `main`.
    let dlls = dlls_beside(&image);
    for want in ["SDL2.dll", "SDL2_image.dll", "libfreetype-6.dll"] {
        assert!(dlls.contains(&want.to_string()), "{want} is not beside the program; found {dlls:?}");
    }
    // sdl2-compat's SDL2.dll loads SDL3.dll by hand, not through its import
    // table; the manifest names it so it ships too.
    assert!(dlls.contains(&"SDL3.dll".to_string()), "SDL3.dll is not beside the program; found {dlls:?}");

    let dump = dir.join("w.ppm");
    let Some(out) = wine(
        &image,
        &dir,
        &[
            ("KILN_UI_EXIT_AFTER_FRAMES", "3"),
            ("KILN_UI_DUMP", dump.to_str().unwrap()),
        ],
    ) else {
        return;
    };
    let stderr = String::from_utf8_lossy(&out.stderr);
    // 53 is what wine's loader answers for STATUS_DLL_NOT_FOUND (0xc0000135)
    // — the failure this test exists to catch — and 124 is `timeout`'s.
    let code = out.status.code();
    assert!(
        code != Some(53) && code != Some(124),
        "the Windows program did not get past the loader under wine (status {code:?}):\n{stderr}"
    );
    match code {
        // Not a branch this suite reaches — the display drivers are off
        // above, always — but the shape a run WITH a display would take, so
        // that someone who removes the override to look at the window gets
        // the frame checked rather than an "unexpected status": the form was
        // drawn and the dump has its background where nothing else is
        // painted.
        Some(0) => {
            assert!(dump.is_file(), "the program exited 0 but wrote no dump");
            let (r, g, b) = ppm_pixel(&dump, 10, 10);
            let close = |a: u8, b: u8| (a as i32 - b as i32).abs() <= 2;
            assert!(
                close(r, 0x1e) && close(g, 0x22) && close(b, 0x33),
                "pixel (10,10) is #{r:02x}{g:02x}{b:02x}, not the form's #1e2233"
            );
        }
        // No display driver — the case on a build machine — so SDL could not
        // open a window and said so in its own words, from inside a program
        // that had loaded every DLL, run the runtime's entry, and reached
        // the ui library's initialisation. Anything else is a real failure.
        Some(1) => assert!(
            stderr.contains("SDL error on create window"),
            "exit 1 without SDL's window error — something else failed:\n{stderr}"
        ),
        other => panic!("unexpected exit status {other:?} under wine:\n{stderr}"),
    }
}

#[test]
fn console_program_that_uses_ui_runs_under_wine_with_the_dlls_beside_it() {
    if !windows_ui_present() {
        return;
    }
    let dir = scratch("conui");
    let source = dir.join("conui.kiln");
    std::fs::write(
        &source,
        "module conui\nuse ui\n\nsub main\n  call print_text(\"loaded with ui beside it\")\nend\n",
    )
    .unwrap();
    let image = dir.join("conui.exe");
    build_windows(&source, &image, &[]);

    // A console program keeps its console even when it links the UI stack.
    assert_eq!(pe_subsystem(&image), 3, "a console program must stay in the console subsystem");
    assert!(dlls_beside(&image).contains(&"SDL2.dll".to_string()));

    // No window is asked for, so this runs to completion with no display
    // at all — and the DLL chain is proven complete by the loader, which
    // resolves every import before `main`.
    let Some(out) = wine(&image, &dir, &[]) else { return };
    assert!(
        out.status.success(),
        "the program exited {:?} under wine:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.lines().any(|l| l == "loaded with ui beside it"),
        "unexpected output under wine: {stdout:?}"
    );
}

#[test]
fn console_cross_build_ships_no_dlls() {
    if !on_path("x86_64-w64-mingw32-gcc") {
        eprintln!("x86_64-w64-mingw32-gcc is not installed; skipping the Windows cross-build test");
        return;
    }
    let dir = scratch("console");
    let image = dir.join("hello.exe");
    build_windows(&repo().join("examples/hello.kiln"), &image, &[]);
    assert_eq!(pe_subsystem(&image), 3, "a console program must link for the console subsystem");
    assert!(
        dlls_beside(&image).is_empty(),
        "a console program imports nothing from the mingw sysroot, yet DLLs were copied: {:?}",
        dlls_beside(&image)
    );
}
