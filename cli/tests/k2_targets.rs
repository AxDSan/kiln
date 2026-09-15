//! Kiln 2 beyond a program for this machine: shared and static libraries a C
//! host links, and programs cross-built for Windows.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("k2-targets-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
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

fn kiln(dir: &Path, args: &[&str]) {
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("kiln");
    assert!(
        out.status.success(),
        "kiln {args:?} failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn run(cmd: &mut Command) -> String {
    let out = cmd.output().expect("run");
    assert!(
        out.status.success(),
        "{cmd:?} failed:\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).replace('\r', "")
}

const LIB: &str = concat!(
    "namespace Mathlib;\n",
    "\n",
    "public static class Api\n",
    "{\n",
    "    static int calls = 10;\n",
    "\n",
    "    public static int Add(int a, int b)\n",
    "    {\n",
    "        calls += 1;\n",
    "        return a + b;\n",
    "    }\n",
    "\n",
    "    public static bool IsEven(int n) => n % 2 == 0;\n",
    "\n",
    "    public static bool Not(bool b) => !b;\n",
    "\n",
    "    [Export(\"mathlib_shout\")]\n",
    "    public static string Shout(string s) => $\"{s}!\";\n",
    "\n",
    "    public static long Calls() => calls;\n",
    "\n",
    "    public static void DllAttach()\n",
    "    {\n",
    "        Console.WriteLine(\"attached\");\n",
    "    }\n",
    "\n",
    "    static int Hidden() => 1;\n",
    "}\n",
);

const HOST: &str = concat!(
    "#include <stdio.h>\n",
    "#include \"Mathlib.h\"\n",
    "int main(void) {\n",
    "    Mathlib_init();\n",
    "    printf(\"%d %d %d %d\\n\", (int)Add(2, 3), (int)IsEven(4), (int)IsEven(5), (int)Not(0));\n",
    "    printf(\"%s %lld\\n\", mathlib_shout(\"hey\"), (long long)Calls());\n",
    "    return 0;\n",
    "}\n",
);

#[test]
fn a_kiln_2_shared_library_exports_its_public_static_methods() {
    let dir = scratch("shared");
    std::fs::write(dir.join("lib.kiln"), LIB).unwrap();
    std::fs::write(dir.join("host.c"), HOST).unwrap();
    kiln(&dir, &["build", "lib.kiln", "--target", "sharedlib", "-o", "libMathlib.so"]);
    let header = std::fs::read_to_string(dir.join("Mathlib.h")).unwrap();
    assert!(header.contains("MATHLIB_API int32_t Add(int32_t a, int32_t b);"), "{header}");
    assert!(header.contains("MATHLIB_API int32_t IsEven(int32_t n);"), "{header}");
    assert!(header.contains("MATHLIB_API const char *mathlib_shout(const char *s);"), "{header}");
    assert!(header.contains("MATHLIB_API int64_t Calls(void);"), "{header}");
    assert!(header.contains("MATHLIB_API void Mathlib_init(void);"), "{header}");
    assert!(!header.contains("Hidden"), "a private method is not exported: {header}");
    let exe = dir.join("host");
    run(Command::new("clang")
        .arg("host.c")
        .args(["-I.", "-L.", "-lMathlib", "-Wl,-rpath,.", "-o"])
        .arg(&exe)
        .current_dir(&dir));
    // `DllAttach` runs when the library loads, before `main`.
    assert_eq!(
        run(Command::new(&exe).current_dir(&dir)),
        "attached\n5 1 0 1\nhey! 11\n"
    );
}

#[test]
fn a_kiln_2_static_library_links_into_a_c_host() {
    let dir = scratch("static");
    let lib = LIB.replace("    public static void DllAttach()\n    {\n        Console.WriteLine(\"attached\");\n    }\n\n", "");
    std::fs::write(dir.join("lib.kiln"), lib).unwrap();
    std::fs::write(dir.join("host.c"), HOST).unwrap();
    kiln(&dir, &["build", "lib.kiln", "--target", "staticlib", "-o", "libMathlib.a"]);
    let header = std::fs::read_to_string(dir.join("Mathlib.h")).unwrap();
    assert!(header.contains("#  define MATHLIB_STATIC"), "{header}");
    let exe = dir.join("host");
    run(Command::new("clang")
        .args(["host.c", "-I.", "libMathlib.a", "-lm", "-o"])
        .arg(&exe)
        .current_dir(&dir));
    assert_eq!(run(Command::new(&exe).current_dir(&dir)), "5 1 0 1\nhey! 11\n");
}

#[test]
fn a_library_refuses_a_form_and_top_level_statements() {
    let dir = scratch("refuse");
    std::fs::write(dir.join("top.kiln"), "namespace T;\nConsole.WriteLine(\"x\");\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", "top.kiln", "--target", "sharedlib"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no entry point"));
}

/// A console program, cross-built for 64- and 32-bit Windows, prints under wine
/// exactly what it prints here. Pointer width reaches everything a program
/// calls in C — `snprintf`'s size among them — so the 32-bit build is the one
/// that proves the lowering asks the target rather than assuming 64 bits.
#[test]
fn a_kiln_2_program_cross_builds_for_windows() {
    if !on_path("x86_64-w64-mingw32-gcc") || !on_path("i686-w64-mingw32-gcc") {
        eprintln!("mingw is not installed; skipping the Kiln 2 Windows build");
        return;
    }
    let dir = scratch("windows");
    let src = repo().join("examples/sugar09_tour.kiln");
    let src = src.to_str().unwrap();
    kiln(&dir, &["build", src, "-o", "here"]);
    let expected = run(Command::new(dir.join("here")).current_dir(&dir));
    for arch in ["x86_64", "x86"] {
        let exe = format!("tour-{arch}.exe");
        kiln(&dir, &["build", src, "--os", "windows", "--arch", arch, "-o", &exe]);
        assert!(dir.join(&exe).is_file());
        if !on_path("wine") {
            eprintln!("wine is not installed; the Windows image was built but not run");
            continue;
        }
        let prefix = scratch("wineprefix");
        let got = run(Command::new("wine")
            .arg(&exe)
            .current_dir(&dir)
            .env("WINEDEBUG", "-all")
            .env("WINEPREFIX", &prefix));
        assert_eq!(got, expected, "the {arch} Windows build printed something else");
    }
}
