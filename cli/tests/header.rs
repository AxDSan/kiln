//! A library build writes a C header beside its artifact, and that header is
//! what a C or C++ host compiles against.
//!
//! The header is only worth having if a host built from it links and runs, so
//! the tests here compile a consumer — once as C, once as C++, from the same
//! source, because the `extern "C"` guard is only exercised by the second —
//! and check what it prints. A header that compiles and marshals wrong is the
//! failure these exist to catch.
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

/// A scratch directory per test: two tests must not share one artifact.
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kiln_header_{tag}_test"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// One subroutine per scalar type, a module variable for `_init` to set, and
/// one array-taking subroutine that C cannot be handed.
const LIB_SRC: &str = "module greet\n\
                       target sharedlib\n\
                       \n\
                       var calls: int = 0\n\
                       \n\
                       sub add(a: int, b: int): int\n  calls = calls + 1\n  return a + b\nend\n\
                       \n\
                       sub greeting(name: text): text\n  return \"Hello, \" + name + \"!\"\nend\n\
                       \n\
                       sub half(x: double): double\n  return x / 2.0\nend\n\
                       \n\
                       sub twice(n: int64): int64\n  return n + n\nend\n\
                       \n\
                       sub is_even(n: int): bool\n  return n % 2 = 0\nend\n\
                       \n\
                       sub count_calls: int\n  return calls\nend\n\
                       \n\
                       sub total(xs: int[]): int\n  return count(xs)\nend\n";

/// The consumer, written so the one source is both C and C++: it includes
/// the header, calls every prototype, and prints what came back.
const HOST_SRC: &str = "#include <stdio.h>\n\
                        #include \"greet.h\"\n\
                        int main(void) {\n\
                          greet_init();\n\
                          printf(\"%d\\n\", (int)add(2, 3));\n\
                          printf(\"%s\\n\", greeting(\"world\"));\n\
                          printf(\"%g\\n\", half(3.0));\n\
                          printf(\"%lld\\n\", (long long)twice(1LL << 40));\n\
                          printf(\"%d %d\\n\", (int)is_even(4), (int)is_even(5));\n\
                          printf(\"%d\\n\", (int)count_calls());\n\
                          return 0;\n\
                        }\n";

fn kiln(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(args)
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln")
}

fn build_lib(dir: &Path, extra: &[&str]) -> (PathBuf, std::process::Output) {
    let src = dir.join("greet.kiln");
    std::fs::write(&src, LIB_SRC).expect("write source");
    let mut args = vec!["build", src.to_str().unwrap()];
    args.extend_from_slice(extra);
    let out = kiln(&args);
    assert!(
        out.status.success(),
        "kiln build failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    (src, out)
}

/// Compile `HOST_SRC` with `compiler` as `lang`, link it against the shared
/// library in `dir` with an rpath so it runs from anywhere, and return its
/// stdout.
fn run_host(dir: &Path, compiler: &str, lang: &str) -> String {
    let src = dir.join("host.c");
    std::fs::write(&src, HOST_SRC).expect("write host");
    let bin = dir.join(format!("host_{lang}"));
    let out = Command::new(compiler)
        .args(["-x", lang, "-Wall", "-Werror"])
        .arg(&src)
        .arg("-I")
        .arg(dir)
        .arg("-L")
        .arg(dir)
        .arg("-lgreet")
        .arg(format!("-Wl,-rpath,{}", dir.display()))
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("run the C compiler");
    assert!(
        out.status.success(),
        "compiling the {lang} host against greet.h failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = Command::new(&bin).output().expect("run host");
    assert!(out.status.success(), "the {lang} host exited non-zero");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

const EXPECTED: &str = "5\nHello, world!\n1.5\n2199023255552\n1 0\n1\n";

/// The header lands beside the artifact, named after the module, with the
/// export macro, `<module>_init`, and one prototype per subroutine in the C
/// types the ABI uses. A subroutine C cannot be handed is listed, not declared.
#[test]
fn shared_library_writes_a_header_a_c_and_a_cxx_host_compile_against() {
    let dir = scratch("so");
    let so = dir.join("libgreet.so");
    let (_, out) = build_lib(&dir, &["-o", so.to_str().unwrap()]);
    let header = dir.join("greet.h");
    assert!(header.is_file(), "no greet.h beside libgreet.so");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("wrote") && stderr.contains("greet.h"),
        "{stderr}"
    );

    let h = std::fs::read_to_string(&header).unwrap();
    for line in [
        "#pragma once",
        "extern \"C\" {",
        "ifdef GREET_EXPORTS",
        "define GREET_API __declspec(dllexport)",
        "define GREET_API __declspec(dllimport)",
        "define GREET_API __attribute__((visibility(\"default\")))",
        "GREET_API void greet_init(void);",
        "GREET_API int32_t add(int32_t a, int32_t b);",
        "GREET_API const char *greeting(const char *name);",
        "GREET_API double half(double x);",
        "GREET_API int64_t twice(int64_t n);",
        "GREET_API int32_t is_even(int32_t n);",
        "GREET_API int32_t count_calls(void);",
        " *   total(xs: int[]): int",
    ] {
        assert!(h.contains(line), "greet.h lacks `{line}`:\n{h}");
    }
    assert!(
        !h.contains("GREET_API int32_t total"),
        "an array parameter got a prototype:\n{h}"
    );

    assert_eq!(run_host(&dir, "clang", "c"), EXPECTED);
    assert_eq!(run_host(&dir, "clang++", "c++"), EXPECTED);
}

/// `--header` puts it somewhere else; nothing lands beside the artifact then.
#[test]
fn header_flag_chooses_the_path() {
    let dir = scratch("flag");
    let so = dir.join("libgreet.so");
    let hdr = dir.join("include").join("api.h");
    std::fs::create_dir_all(hdr.parent().unwrap()).unwrap();
    build_lib(
        &dir,
        &[
            "-o",
            so.to_str().unwrap(),
            "--header",
            hdr.to_str().unwrap(),
        ],
    );
    assert!(hdr.is_file(), "--header path was not written");
    assert!(
        !dir.join("greet.h").exists(),
        "the default header was written as well"
    );
    assert!(std::fs::read_to_string(&hdr)
        .unwrap()
        .contains("GREET_API int32_t add"));
}

/// A static library gets the same header, and a C host links the archive.
#[test]
fn static_library_writes_the_same_header() {
    let dir = scratch("a");
    let archive = dir.join("libgreet.a");
    build_lib(
        &dir,
        &["--target", "staticlib", "-o", archive.to_str().unwrap()],
    );
    let h = std::fs::read_to_string(dir.join("greet.h")).expect("greet.h beside libgreet.a");
    assert!(
        h.contains("GREET_API const char *greeting(const char *name);"),
        "{h}"
    );
    assert!(
        h.contains("#  define GREET_STATIC"),
        "a static library's header must not import:\n{h}"
    );

    let src = dir.join("host.c");
    std::fs::write(&src, HOST_SRC).unwrap();
    let bin = dir.join("host_static");
    let out = Command::new("clang")
        .arg(&src)
        .arg("-I")
        .arg(&dir)
        .arg(&archive)
        .arg("-lm")
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("clang");
    assert!(
        out.status.success(),
        "linking the archive failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = Command::new(&bin).output().expect("run host");
    assert_eq!(String::from_utf8_lossy(&out.stdout), EXPECTED);
}

/// A program writes no header: it exports nothing a host could call.
#[test]
fn a_program_writes_no_header() {
    let dir = scratch("exe");
    let src = dir.join("hello.kiln");
    std::fs::write(
        &src,
        "module hello\nsub main\n  call print_text(\"hi\")\nend\n",
    )
    .unwrap();
    let bin = dir.join("hello");
    let out = kiln(&["build", src.to_str().unwrap(), "-o", bin.to_str().unwrap()]);
    assert!(out.status.success());
    assert!(!dir.join("hello.h").exists(), "a program grew a header");
}

fn mingw_present() -> bool {
    let ok = Command::new("x86_64-w64-mingw32-gcc")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        eprintln!("x86_64-w64-mingw32-gcc is not installed; skipping the import-library test");
    }
    ok
}

/// A Windows DLL comes with the import library a consumer's
/// `#pragma comment(lib, "greet.lib")` names, carrying the exports, plus
/// the same header.
#[test]
fn windows_shared_library_writes_an_import_library() {
    if !mingw_present() {
        return;
    }
    let dir = scratch("win");
    let dll = dir.join("greet.dll");
    let (_, out) = build_lib(
        &dir,
        &[
            "--os",
            "windows",
            "--target",
            "sharedlib",
            "-o",
            dll.to_str().unwrap(),
        ],
    );
    let lib = dir.join("greet.lib");
    assert!(lib.is_file(), "no greet.lib beside greet.dll");
    let bytes = std::fs::read(&lib).unwrap();
    assert!(
        bytes.starts_with(b"!<arch>\n"),
        "greet.lib is not an archive"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("greet.lib"), "{stderr}");
    assert!(dir.join("greet.h").is_file(), "no greet.h beside greet.dll");

    // The import library must name the exports, or the consumer's link fails
    // with an unresolved `__imp_add` — the symbol MSVC and mingw both look for.
    let nm = Command::new("x86_64-w64-mingw32-nm")
        .arg(&lib)
        .output()
        .expect("nm");
    let syms = String::from_utf8_lossy(&nm.stdout);
    for s in ["__imp_greet_init", "__imp_add", "__imp_greeting"] {
        assert!(syms.contains(s), "greet.lib lacks {s}:\n{syms}");
    }
}
