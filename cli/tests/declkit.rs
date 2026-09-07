//! Declaration kits: `use <name>` brings in a bundle of foreign declarations —
//! `dll` calls, `is c` records and named constants — that live in the kit, not
//! the program. The proof kit is `kits/demoffi`, which ships a tiny portable C
//! library (`demoffi.c`) its `dll` lines load at run time, an `is c` record and
//! three constants. A program that `use demoffi` calls the dll, uses the record
//! and a const with no local `dll`/`record`/`const` line of its own.
//!
//! These build the real binary against the real kit, exactly as a person would.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
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

/// A scratch directory of its own per test. The program resolves its runtime
/// library beside its own executable, so the built lib and the built program
/// must share one directory — and two tests must not share it.
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kiln_declkit_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Compile the kit's shipped `demoffi.c` into a loadable library in `dir`,
/// under the name the `dll` declarations look for.
fn build_demoffi_lib(dir: &Path, soname: &str, cc: &str) {
    let src = repo().join("kits/demoffi/demoffi.c");
    let status = Command::new(cc)
        .args(["-shared", "-fPIC", "-o"])
        .arg(dir.join(soname))
        .arg(&src)
        .status()
        .unwrap_or_else(|e| panic!("run {cc}: {e}"));
    assert!(status.success(), "{cc} failed to build {soname}");
}

/// Build `src_text` (written into `dir/prog.kiln`) to `dir/prog`, with the
/// working directory pinned to the repo so `kits/demoffi` resolves as a project
/// kit. `extra` carries `--os windows` and the like.
fn build_program(dir: &Path, src_text: &str, out: &str, extra: &[&str]) -> Result<PathBuf, String> {
    let srcpath = dir.join("prog.kiln");
    std::fs::write(&srcpath, src_text).expect("write program source");
    let outpath = dir.join(out);
    let output = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", srcpath.to_str().unwrap(), "-o", outpath.to_str().unwrap()])
        .args(extra)
        .current_dir(repo())
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln build");
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    Ok(outpath)
}

const PROGRAM: &str = "\
module demo
use demoffi

sub main
  call print_int(demoffi_add(DEMO_ANSWER, DEMO_SCALE))
  call print_text(demoffi_greeting())
  var p: DemoPoint
  p.x = 3
  p.y = 4
  call demoffi_move(p, DEMO_SCALE, 20)
  call print_int(p.x)
  call print_int(p.y)
  call print_int64(size of DemoPoint)
  if demoffi_add(1, 1) = DEMO_ANSWER
    call print_text(\"unexpected\")
  else
    call print_text(\"const compare works\")
  end
  call print_text(DEMO_TAG)
end
";

const EXPECTED: &[&str] = &[
    "52",                   // demoffi_add(DEMO_ANSWER=42, DEMO_SCALE=10) — dll + two consts
    "demoffi says hello",   // demoffi_greeting() — a C string copied out
    "13",                   // p.x = 3 + DEMO_SCALE   — c-record mutated through the dll pointer
    "24",                   // p.y = 4 + 20
    "8",                    // size of DemoPoint      — the kit's c-record layout
    "const compare works",  // DEMO_ANSWER in a comparison
    "demoffi",              // DEMO_TAG, a text constant
];

/// The whole point of the stage: `use demoffi` supplies the dll, the c-record
/// and the constants, and a program that writes none of them builds and runs.
#[test]
fn use_demoffi_supplies_dll_record_and_const() {
    if !on_path("clang") {
        eprintln!("clang is not installed; skipping");
        return;
    }
    let dir = scratch("run");
    build_demoffi_lib(&dir, "libdemoffi.so", "clang");
    let bin = build_program(&dir, PROGRAM, "prog", &[]).expect("build should succeed");

    let out = Command::new(&bin).output().expect("run program");
    assert!(
        out.status.success(),
        "program exited non-zero:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(lines, EXPECTED, "unexpected output from the demoffi program");
}

/// `kiln commands --use demoffi` lists the kit's dlls, its c-record and its
/// constants, so Studio completion and the generated docs can see them.
#[test]
fn commands_lists_the_bundle() {
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["commands", "--use", "demoffi"])
        .current_dir(repo())
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln commands");
    assert!(out.status.success(), "commands --use demoffi failed: {}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    let has = |needle: &str| text.lines().any(|l| l.contains(needle));
    assert!(has("dll: demoffi_add(int, int) -> int"), "dll listing missing:\n{text}");
    assert!(has("dll: demoffi_greeting() -> text"), "dll listing missing:\n{text}");
    assert!(has("dll: demoffi_move(DemoPoint, int, int)"), "dll with a c-record param missing:\n{text}");
    assert!(has("crecord: DemoPoint"), "c-record listing missing:\n{text}");
    assert!(has("const: DEMO_ANSWER int"), "const listing missing:\n{text}");
    assert!(has("const: DEMO_TAG text"), "text const listing missing:\n{text}");
}

/// A windows-only declaration kit used with `--os linux` is a clear compile
/// error naming the kit and the OS it needs — not a wall of linker errors.
#[test]
fn a_windows_only_kit_is_refused_on_linux() {
    let dir = scratch("gate");
    // A project of its own with a windows-only decl kit under kits/.
    let kit = dir.join("kits").join("winonly");
    std::fs::create_dir_all(&kit).expect("create kit dir");
    std::fs::write(
        kit.join("winonly.kdecl"),
        "dll MessageBeep(kind: int): bool from \"user32\" system\nconst MB_OK = 0\n",
    )
    .unwrap();
    std::fs::write(
        kit.join("lib.json"),
        "{ \"display\": \"Win Only\", \"version\": \"1.0.0\", \"platforms\": [\"windows\"] }\n",
    )
    .unwrap();
    let src = dir.join("app.kiln");
    std::fs::write(&src, "module app\nuse winonly\nsub main\n  call print_int(MB_OK)\nend\n").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", src.to_str().unwrap(), "--os", "linux", "-o", dir.join("app").to_str().unwrap()])
        .current_dir(&dir) // so kits/winonly resolves as the project kit
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln build");
    assert!(!output.status.success(), "a windows-only kit must not build for linux");
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(
        err.contains("winonly") && err.contains("windows"),
        "the error must name the kit and the required OS, got:\n{err}"
    );

    // And listing its contents is still allowed on Linux — a Win32 kit must
    // document and complete on a machine that cannot build for Windows.
    let listed = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["commands", "--use", "winonly"])
        .current_dir(&dir)
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln commands");
    assert!(listed.status.success(), "listing a windows-only kit on linux must work");
    assert!(
        String::from_utf8_lossy(&listed.stdout).contains("MessageBeep"),
        "the windows-only kit's dll should still be listed on linux"
    );
}

// --- a minimal LSP client, enough to open a document and ask for completion ---

struct Lsp {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    id: i64,
}

impl Lsp {
    fn start() -> Lsp {
        let mut child = Command::new(env!("CARGO_BIN_EXE_kiln"))
            .arg("lsp")
            .current_dir(repo())
            .env("KILN_RUNTIME_DIR", repo().join("runtime"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn kiln lsp");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut c = Lsp { child, stdin, stdout, id: 1 };
        let root = format!("file://{}", repo().display());
        c.send(serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "capabilities": {}, "rootUri": root }
        }));
        c.wait_for_id(1);
        c.send(serde_json::json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }));
        c
    }

    fn send(&mut self, v: serde_json::Value) {
        let body = serde_json::to_vec(&v).unwrap();
        write!(self.stdin, "Content-Length: {}\r\n\r\n", body.len()).unwrap();
        self.stdin.write_all(&body).unwrap();
        self.stdin.flush().unwrap();
    }

    fn recv(&mut self) -> serde_json::Value {
        let mut len = 0usize;
        loop {
            let mut line = String::new();
            let n = self.stdout.read_line(&mut line).expect("read header");
            assert!(n > 0, "server closed the stream");
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some(v) = line.strip_prefix("Content-Length: ") {
                len = v.parse().expect("numeric Content-Length");
            }
        }
        let mut buf = vec![0u8; len];
        self.stdout.read_exact(&mut buf).expect("read body");
        serde_json::from_slice(&buf).expect("body is JSON")
    }

    fn wait_for_id(&mut self, id: i64) -> serde_json::Value {
        for _ in 0..20 {
            let m = self.recv();
            if m["id"] == id {
                return m["result"].clone();
            }
        }
        panic!("no response to id {id}");
    }

    fn open(&mut self, uri: &str, text: &str) {
        self.send(serde_json::json!({
            "jsonrpc": "2.0", "method": "textDocument/didOpen",
            "params": { "textDocument": { "uri": uri, "languageId": "kiln", "version": 1, "text": text } }
        }));
    }

    fn completion_labels(&mut self, uri: &str, line: u32, ch: u32) -> Vec<String> {
        self.id += 1;
        let id = self.id;
        self.send(serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": "textDocument/completion",
            "params": { "textDocument": { "uri": uri }, "position": { "line": line, "character": ch } }
        }));
        let r = self.wait_for_id(id);
        r.as_array()
            .expect("completion list")
            .iter()
            .map(|i| i["label"].as_str().unwrap().to_string())
            .collect()
    }

    fn shutdown(mut self) {
        self.send(serde_json::json!({ "jsonrpc": "2.0", "id": 999, "method": "shutdown", "params": null }));
        self.send(serde_json::json!({ "jsonrpc": "2.0", "method": "exit", "params": null }));
        let _ = self.child.wait();
    }
}

/// The language server offers a kit's declarations after `use demoffi`, so
/// Studio completes a `dll`, a record and a constant that appear nowhere in the
/// file being edited.
#[test]
fn lsp_completes_a_kit_declaration() {
    let mut c = Lsp::start();
    let uri = "file:///tmp/kiln_declkit_complete.kiln";
    // Caret on the blank line inside `main`.
    c.open(uri, "module m\nuse demoffi\nsub main\n  \nend\n");
    let labels = c.completion_labels(uri, 3, 2);
    assert!(labels.contains(&"demoffi_add".to_string()), "kit dll should complete: {labels:?}");
    assert!(labels.contains(&"DemoPoint".to_string()), "kit record should complete: {labels:?}");
    assert!(labels.contains(&"DEMO_ANSWER".to_string()), "kit const should complete: {labels:?}");
    c.shutdown();
}

/// The kit is portable: the same program, cross-built for Windows through
/// mingw and run under wine, says exactly what the Linux build says. Skips
/// itself, out loud, where the cross toolchain or wine is absent.
#[test]
fn demoffi_cross_builds_and_runs_on_windows() {
    if !on_path("x86_64-w64-mingw32-gcc") {
        eprintln!("x86_64-w64-mingw32-gcc is not installed; skipping the Windows cross-build");
        return;
    }
    let dir = scratch("win");
    build_demoffi_lib(&dir, "demoffi.dll", "x86_64-w64-mingw32-gcc");
    let bin = match build_program(&dir, PROGRAM, "prog.exe", &["--os", "windows"]) {
        Ok(b) => b,
        Err(e) => panic!("cross build failed:\n{e}"),
    };
    assert!(bin.exists(), "the Windows build produced no .exe");

    if !on_path("wine") {
        eprintln!("wine is not installed; built the PE but did not run it");
        return;
    }
    let out = Command::new("wine")
        .arg(&bin)
        .current_dir(&dir) // wine resolves demoffi.dll beside the .exe
        .env("WINEDEBUG", "-all")
        .output()
        .expect("run under wine");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().map(|l| l.trim_end_matches('\r')).collect();
    assert_eq!(lines, EXPECTED, "wine output differs from the Linux build:\n{stdout}");
}
