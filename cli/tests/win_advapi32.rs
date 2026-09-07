//! The ADVAPI32 portion of the `win` kit: `kits/win/advapi32.kdecl`. It declares
//! the registry calls (RegOpenKeyExA and friends) and the access-token /
//! privilege calls (OpenProcessToken, LookupPrivilegeValueA,
//! AdjustTokenPrivileges) a program uses to raise SeDebugPrivilege, plus the
//! HKEY/KEY/REG/TOKEN/SE constant families and the LUID / LUID_AND_ATTRIBUTES /
//! TOKEN_PRIVILEGES structs those calls read and write.
//!
//! These tests copy *only* advapi32.kdecl into a throwaway project of their own,
//! under `kits/win/`, so they exercise this one file regardless of the other
//! `.kdecl` files a sibling stage drops into the real `kits/win/`. The kit is
//! marked windows-only, so the program is cross-built for Windows through mingw
//! and run under wine; where either is missing the test says so and stops.
//!
//! Two advapi32 calls actually run under wine and have an effect this test can
//! see without a GUI or a second process: RegOpenKeyExA opening HKCU\Software
//! returns ERROR_SUCCESS, and LookupPrivilegeValueA(NULL, "SeDebugPrivilege")
//! returns TRUE and fills a LUID. The rest of the surface is proved by building
//! for Windows and by `kiln commands --use win` listing it.

use std::path::PathBuf;
use std::process::Command;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
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

/// A throwaway project whose `kits/win/` holds *only* advapi32.kdecl and a
/// windows-only lib.json, so what these tests see is this stage's file alone.
fn isolated_project(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kiln_win_advapi32_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    let kit = dir.join("kits").join("win");
    std::fs::create_dir_all(&kit).expect("create kit dir");
    std::fs::copy(repo().join("kits/win/advapi32.kdecl"), kit.join("advapi32.kdecl"))
        .expect("copy advapi32.kdecl into the isolated kit");
    std::fs::write(
        kit.join("lib.json"),
        "{ \"display\": \"Windows API\", \"section\": \"System\", \"version\": \"0.1.0\", \"platforms\": [\"windows\"] }\n",
    )
    .expect("write the isolated kit's lib.json");
    dir
}

const PROGRAM: &str = "\
module winadv
use win

sub main
  var cell: ptr = mem_alloc(8)
  call mem_zero(cell, 8)
  let rc: int = RegOpenKeyExA(ptr_from_int(HKEY_CURRENT_USER), \"Software\", 0, KEY_READ, cell)
  call print_text(\"regopen\")
  call print_int(rc)
  if rc = ERROR_SUCCESS
    let h: ptr = ptr_from_int(ptr_read_int64(cell, 0))
    let rc2: int = RegCloseKey(h)
    call print_text(\"regclose\")
    call print_int(rc2)
  end
  call mem_free(cell)

  var luid: LUID
  let ok: bool = LookupPrivilegeValueA(ptr_null(), SE_DEBUG_NAME, luid)
  call print_text(\"privlookup\")
  if ok
    call print_text(\"yes\")
  else
    call print_text(\"no\")
  end
end
";

/// `kiln commands --use win` lists advapi32's dlls, its c-records and its
/// constants — the listing works on Linux even though a build cannot, so
/// Studio completion and the generated reference see the Win32 surface. This
/// runs everywhere; it needs no cross toolchain.
#[test]
fn commands_lists_the_advapi32_surface() {
    let dir = isolated_project("list");
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["commands", "--use", "win"])
        .current_dir(&dir) // so kits/win resolves as the project kit
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln commands");
    assert!(out.status.success(), "commands --use win failed: {}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    let has = |needle: &str| text.lines().any(|l| l.contains(needle));

    // A registry call, a handle-in / status-out shape.
    assert!(has("dll: RegOpenKeyExA(ptr, text, int, int, ptr) -> int"), "RegOpenKeyExA missing:\n{text}");
    assert!(has("dll: RegCloseKey(ptr) -> int"), "RegCloseKey missing:\n{text}");
    assert!(has("dll: RegSetValueExA("), "RegSetValueExA missing:\n{text}");
    assert!(has("dll: RegQueryValueExA("), "RegQueryValueExA missing:\n{text}");
    // A privilege call taking a LUID c-record by pointer.
    assert!(has("dll: LookupPrivilegeValueA(ptr, text, LUID) -> bool"), "LookupPrivilegeValueA missing:\n{text}");
    assert!(has("dll: AdjustTokenPrivileges(ptr, bool, TOKEN_PRIVILEGES, int, ptr, ptr) -> bool"), "AdjustTokenPrivileges missing:\n{text}");
    assert!(has("dll: OpenProcessToken(ptr, int, ptr) -> bool"), "OpenProcessToken missing:\n{text}");
    assert!(has("dll: GetTokenInformation("), "GetTokenInformation missing:\n{text}");

    // The three structs, including the nested array of nested records.
    assert!(has("crecord: LUID low_part: int, high_part: int"), "LUID missing:\n{text}");
    assert!(has("crecord: LUID_AND_ATTRIBUTES luid: LUID, attributes: int"), "LUID_AND_ATTRIBUTES missing:\n{text}");
    assert!(has("crecord: TOKEN_PRIVILEGES privilege_count: int, privileges: LUID_AND_ATTRIBUTES[1]"), "TOKEN_PRIVILEGES missing:\n{text}");

    // The constant families. An HKEY root is past a signed int, so it types int64.
    assert!(has("const: HKEY_CURRENT_USER int64"), "HKEY_CURRENT_USER missing/typed wrong:\n{text}");
    assert!(has("const: HKEY_LOCAL_MACHINE int64"), "HKEY_LOCAL_MACHINE missing:\n{text}");
    assert!(has("const: KEY_READ int"), "KEY_READ missing:\n{text}");
    assert!(has("const: KEY_ALL_ACCESS int"), "KEY_ALL_ACCESS missing:\n{text}");
    assert!(has("const: REG_SZ int"), "REG_SZ missing:\n{text}");
    assert!(has("const: REG_DWORD int"), "REG_DWORD missing:\n{text}");
    assert!(has("const: ERROR_SUCCESS int"), "ERROR_SUCCESS missing:\n{text}");
    assert!(has("const: TOKEN_ADJUST_PRIVILEGES int"), "TOKEN_ADJUST_PRIVILEGES missing:\n{text}");
    assert!(has("const: SE_PRIVILEGE_ENABLED int"), "SE_PRIVILEGE_ENABLED missing:\n{text}");
    assert!(has("const: SE_DEBUG_NAME text"), "SE_DEBUG_NAME text const missing:\n{text}");
}

/// A windows-only kit is refused for another OS with a message naming the kit
/// and the OS, rather than a wall of linker errors — the gate advapi32 sits
/// behind. Runs everywhere.
#[test]
fn advapi32_is_refused_on_linux() {
    let dir = isolated_project("gate");
    std::fs::write(
        dir.join("app.kiln"),
        "module app\nuse win\nsub main\n  call print_int(REG_SZ)\nend\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", dir.join("app.kiln").to_str().unwrap(), "--os", "linux", "-o", dir.join("app").to_str().unwrap()])
        .current_dir(&dir)
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln build");
    assert!(!out.status.success(), "a windows-only kit must not build for linux");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("win") && err.contains("linux"), "the error must name the kit and the OS, got:\n{err}");
}

/// The heart of the stage: the program cross-builds for Windows, and — under
/// wine — RegOpenKeyExA on HKCU\Software returns ERROR_SUCCESS, RegCloseKey
/// returns it too, and LookupPrivilegeValueA(NULL, SE_DEBUG_NAME) returns TRUE
/// and fills the LUID it is handed. Skips, out loud, without mingw or wine.
#[test]
fn advapi32_builds_and_runs_on_windows() {
    if !on_path("x86_64-w64-mingw32-gcc") {
        eprintln!("x86_64-w64-mingw32-gcc is not installed; skipping the advapi32 Windows cross-build");
        return;
    }
    let dir = isolated_project("run");
    let src = dir.join("prog.kiln");
    std::fs::write(&src, PROGRAM).expect("write program source");
    let exe = dir.join("prog.exe");
    let build = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", src.to_str().unwrap(), "--os", "windows", "-o", exe.to_str().unwrap()])
        .current_dir(&dir)
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln build");
    assert!(
        build.status.success(),
        "cross build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    assert!(exe.exists(), "the Windows build produced no .exe");

    if !on_path("wine") {
        eprintln!("wine is not installed; the advapi32 PE was built but not run");
        return;
    }
    let out = Command::new("wine")
        .arg(&exe)
        .current_dir(&dir)
        .env("WINEDEBUG", "-all")
        .output()
        .expect("run under wine");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().map(|l| l.trim_end_matches('\r')).collect();
    assert_eq!(
        lines,
        vec!["regopen", "0", "regclose", "0", "privlookup", "yes"],
        "advapi32 under wine did not report the effects expected:\n{stdout}"
    );
}
