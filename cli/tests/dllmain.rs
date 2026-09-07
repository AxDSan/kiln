//! End-to-end tests for the loader hook: a `target sharedlib` module that
//! defines `sub dll_attach` gets a platform entry point — `DllMain` on Windows,
//! an ELF constructor on Linux — so the library runs the instant it is mapped,
//! with no host code asking it to.
//!
//! The worked example (examples/hook/) is the headline: a Kiln library that,
//! on load, installs a function-pointer detour into a C library the host is
//! already calling, so the host's next call returns a hooked value — proof that
//! `dll`, `address of` and `dll_attach` compose into a real in-process hook. A
//! second, smaller library proves the ordering the shim promises (`<module>_init`
//! runs before `dll_attach`) through an exported flag, and that a hook-less
//! sharedlib gets no entry point at all. The Windows half of the hook runs under
//! wine when it is here, and skips with a line when it is not.
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

/// A scratch directory per test: a shared library is resolved beside the program
/// that loads it, so every artifact of one case must share one directory — and
/// two cases must not share it.
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kiln_dllmain_{tag}_test"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Build a Kiln source to `out` for the host platform; assert it succeeds.
fn build_native(src: &Path, out: &Path, extra: &[&str]) {
    let status = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .args(extra)
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .status()
        .expect("run kiln");
    assert!(status.success(), "kiln build failed for {}", src.display());
}

/// Compile a C source with `cc` into `out`; assert it succeeds. Extra flags come
/// after the source, so `-l` and `-L` land where a linker wants them.
fn cc(compiler: &str, src: &Path, out: &Path, extra: &[&str]) {
    let status = Command::new(compiler)
        .arg(src)
        .args(extra)
        .arg("-o")
        .arg(out)
        .status()
        .unwrap_or_else(|e| panic!("run {compiler}: {e}"));
    assert!(status.success(), "{compiler} failed for {}", src.display());
}

/// stdout of `bin` run in `cwd`, split into lines; the program must exit 0.
fn run_lines(bin: &Path, cwd: &Path) -> Vec<String> {
    let out = Command::new(bin)
        .current_dir(cwd)
        .output()
        .unwrap_or_else(|e| panic!("run {}: {e}", bin.display()));
    assert!(
        out.status.success(),
        "{} exited non-zero:\n{}",
        bin.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

/// The worked example, natively: a C library (`hookrt`) the host calls, an
/// Kiln library (`hook`) that installs a detour from its `dll_attach`, and a
/// host that calls the C function before and after loading the Kiln library.
/// The before is the original (10*2), the after is the detour (original + 1) —
/// the loader ran the hook, in-process, with nothing patched by hand.
#[test]
fn dll_attach_installs_a_hook_the_host_sees() {
    let dir = scratch("hook");
    let ex = repo().join("examples/hook");
    cc("clang", &ex.join("hookrt.c"), &dir.join("libhookrt.so"), &["-shared", "-fPIC"]);
    build_native(&ex.join("hook.kiln"), &dir.join("libhook.so"), &["--target", "sharedlib"]);
    cc(
        "clang",
        &ex.join("host.c"),
        &dir.join("host"),
        &["-L", dir.to_str().unwrap(), "-lhookrt", &format!("-Wl,-rpath,{}", dir.display()), "-ldl"],
    );

    let lines = run_lines(&dir.join("host"), &dir);
    assert_eq!(lines, vec!["before 20", "after 21"], "the hook did not take effect");
}

/// The ordering the shim promises: `<module>_init` runs before `dll_attach`.
/// The flag starts at 7 (what `init` sets it to) and `dll_attach` adds one, so a
/// loader that reads the exported `check` back sees 8 — which is only possible if
/// `init` ran first AND `dll_attach` ran at all. A hook-less twin of the library
/// gets no loader entry, so nothing runs on load and its flag reads 0.
#[test]
fn an_exported_flag_shows_init_then_attach_ran() {
    let dir = scratch("flag");
    std::fs::write(
        dir.join("flag.kiln"),
        "module flag\n\
         target sharedlib\n\
         var attached: int = 7\n\
         sub dll_attach\n  attached = attached + 1\nend\n\
         sub check(): int\n  return attached\nend\n",
    )
    .expect("write flag.kiln");
    // The same shape without the special name: nothing wires it to the loader.
    std::fs::write(
        dir.join("plain.kiln"),
        "module plain\n\
         target sharedlib\n\
         var attached: int = 7\n\
         sub check(): int\n  return attached\nend\n",
    )
    .expect("write plain.kiln");
    std::fs::write(
        dir.join("loader.c"),
        "#include <stdio.h>\n#include <dlfcn.h>\n\
         int main(int argc, char **argv) {\n\
         \x20 void *h = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);\n\
         \x20 if (!h) { fprintf(stderr, \"dlopen: %s\\n\", dlerror()); return 2; }\n\
         \x20 int (*check)(void) = (int(*)(void))dlsym(h, \"check\");\n\
         \x20 if (!check) { fprintf(stderr, \"no check symbol\\n\"); return 3; }\n\
         \x20 printf(\"%d\\n\", check());\n\
         \x20 return 0;\n}\n",
    )
    .expect("write loader.c");

    build_native(&dir.join("flag.kiln"), &dir.join("libflag.so"), &["--target", "sharedlib"]);
    build_native(&dir.join("plain.kiln"), &dir.join("libplain.so"), &["--target", "sharedlib"]);
    cc("clang", &dir.join("loader.c"), &dir.join("loader"), &["-ldl"]);

    let hooked = Command::new(dir.join("loader"))
        .arg(dir.join("libflag.so"))
        .output()
        .expect("run loader on libflag");
    assert!(hooked.status.success(), "loader failed:\n{}", String::from_utf8_lossy(&hooked.stderr));
    assert_eq!(
        String::from_utf8_lossy(&hooked.stdout).trim(),
        "8",
        "init-then-attach did not run (7 set by init, +1 by dll_attach)"
    );

    let plain = Command::new(dir.join("loader"))
        .arg(dir.join("libplain.so"))
        .output()
        .expect("run loader on libplain");
    assert!(plain.status.success(), "loader failed:\n{}", String::from_utf8_lossy(&plain.stderr));
    assert_eq!(
        String::from_utf8_lossy(&plain.stdout).trim(),
        "0",
        "a hook-less sharedlib must get no loader entry, so nothing runs on load"
    );
}

/// A loader hook is called by the OS with no arguments and its result ignored,
/// so the entry the backend wires up is `void(void)`. A `dll_attach` that
/// declares a parameter or a return is rejected at build time, naming the hook.
#[test]
fn a_loader_hook_with_a_signature_is_rejected() {
    let dir = scratch("badsig");
    for bad in [
        "sub dll_attach(x: int)\n  call print_int(x)\nend\n",
        "sub dll_detach(): int\n  return 1\nend\n",
    ] {
        let src = dir.join("bad.kiln");
        std::fs::write(
            &src,
            format!("module bad\ntarget sharedlib\n{bad}sub go\n  call print_text(\"hi\")\nend\n"),
        )
        .expect("write bad.kiln");
        let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
            .args(["build", src.to_str().unwrap(), "-o", dir.join("libbad.so").to_str().unwrap()])
            .args(["--target", "sharedlib"])
            .env("KILN_RUNTIME_DIR", repo().join("runtime"))
            .output()
            .expect("run kiln");
        assert!(!out.status.success(), "a hook with a signature must not build");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("loader hook"),
            "the error must name the hook contract, got:\n{stderr}"
        );
    }
}

/// Both hooks fire, in order: `dll_attach` when the library is mapped,
/// `dll_detach` when it is unmapped. A loader dlopens the library — printing
/// "attached" from the constructor — reads an export, then dlcloses it, running
/// the destructor and printing "detached". The order in the captured output is
/// the proof.
#[test]
fn both_hooks_fire_at_load_and_unload() {
    let dir = scratch("bothhooks");
    std::fs::write(
        dir.join("dt.kiln"),
        "module dt\n\
         target sharedlib\n\
         sub dll_attach\n  call print_text(\"attached\")\nend\n\
         sub dll_detach\n  call print_text(\"detached\")\nend\n\
         sub check(): int\n  return 1\nend\n",
    )
    .expect("write dt.kiln");
    std::fs::write(
        dir.join("ld.c"),
        "#include <stdio.h>\n#include <dlfcn.h>\n\
         int main(int argc, char **argv) {\n\
         \x20 void *h = dlopen(argv[1], RTLD_NOW);\n\
         \x20 if (!h) { fprintf(stderr, \"dlopen: %s\\n\", dlerror()); return 2; }\n\
         \x20 int (*check)(void) = (int(*)(void))dlsym(h, \"check\");\n\
         \x20 if (!check || check() != 1) { fprintf(stderr, \"bad check\\n\"); return 3; }\n\
         \x20 dlclose(h);\n\
         \x20 return 0;\n}\n",
    )
    .expect("write ld.c");
    build_native(&dir.join("dt.kiln"), &dir.join("libdt.so"), &["--target", "sharedlib"]);
    cc("clang", &dir.join("ld.c"), &dir.join("ld"), &["-ldl"]);

    let out = Command::new(dir.join("ld"))
        .arg(dir.join("libdt.so"))
        .output()
        .expect("run loader");
    assert!(out.status.success(), "loader failed:\n{}", String::from_utf8_lossy(&out.stderr));
    let lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(
        lines,
        vec!["attached", "detached"],
        "the hooks did not fire in load/unload order"
    );
}

// --- the Windows half, under wine when it is here ------------------------

const MINGW_GCC: &str = "x86_64-w64-mingw32-gcc";

fn mingw_present() -> bool {
    if on_path(MINGW_GCC) {
        return true;
    }
    eprintln!("{MINGW_GCC} is not installed; skipping the Windows loader-hook test");
    false
}

/// The same worked hook, cross-built to a real `DllMain` and run under wine. The
/// host loads `hook.dll` with `LoadLibraryA`, which drives `DllMain`'s
/// `DLL_PROCESS_ATTACH` into `dll_attach`, installing the detour into the one
/// loaded `hookrt.dll` the host is calling — so the after value changes, in the
/// Windows process, exactly as on Linux.
#[test]
fn dll_attach_is_a_real_dllmain_under_wine() {
    if !mingw_present() {
        return;
    }
    let dir = scratch("winhook");
    let ex = repo().join("examples/hook");
    cc(
        MINGW_GCC,
        &ex.join("hookrt.c"),
        &dir.join("hookrt.dll"),
        &["-shared", &format!("-Wl,--out-implib,{}", dir.join("libhookrt.dll.a").display())],
    );
    build_native(&ex.join("hook.kiln"), &dir.join("hook.dll"), &["--os", "windows", "--target", "sharedlib"]);
    cc(
        MINGW_GCC,
        &ex.join("host.c"),
        &dir.join("host.exe"),
        &["-L", dir.to_str().unwrap(), "-lhookrt"],
    );

    if !on_path("wine") {
        eprintln!("wine is not installed; the Windows hook was built but not run");
        return;
    }
    let out = Command::new("wine")
        .arg("host.exe")
        .current_dir(&dir)
        .env("WINEDEBUG", "-all")
        .output()
        .expect("run wine");
    assert!(
        out.status.success(),
        "host.exe exited non-zero under wine:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(lines, vec!["before 20", "after 21"], "the DllMain hook did not take effect");
}
