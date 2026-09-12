//! End-to-end tests for the 32-bit Windows backend (`--os windows --arch x86`).
//!
//! The i386 backend exists to reach the machines a 64-bit build cannot: a
//! 2000s Win32 client is a PE32 image, and the language around it is unchanged.
//! So these tests are the facts that are NOT the language:
//!
//!  * the artifact really is a PE32 i386 image, not a PE32+ one;
//!  * a c-record's pointer-sized field is four bytes, not eight;
//!  * a Win32 `system` declaration is `stdcall`, which is what keeps the stack
//!    balanced — a `cdecl` call to a `stdcall` function is a crash;
//!  * the image imports no mingw runtime DLL (`libgcc_s_dw2-1.dll`), because
//!    the machine it is copied to does not have one.
//!
//! Built with mingw's i686 toolchain, run under wine, and skipped with a line
//! when either is not installed — the same bargain every other cross test makes.
use std::path::PathBuf;
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
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn x32_present() -> bool {
    if on_path("i686-w64-mingw32-gcc") {
        return true;
    }
    eprintln!("i686-w64-mingw32-gcc is not installed; skipping the x86 Windows test");
    false
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kiln_x32_{tag}_test"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// The `Machine` field of a PE image: 0x014c is i386, 0x8664 is x86-64.
fn pe_machine(bytes: &[u8]) -> u16 {
    let e_lfanew = u32::from_le_bytes(bytes[0x3c..0x40].try_into().unwrap()) as usize;
    assert_eq!(&bytes[e_lfanew..e_lfanew + 4], b"PE\0\0", "not a PE image");
    u16::from_le_bytes(bytes[e_lfanew + 4..e_lfanew + 6].try_into().unwrap())
}

/// A library with a loader hook and an exported subroutine: the exact shape an
/// injected DLL has.
const LIB: &str = "\
module probe
target sharedlib

dll MessageBoxA(handle: ptr, text: text, caption: text, kind: int): int from \"user32\" system

sub dll_attach
  call print_text(\"attached\")
end

sub add(a: int, b: int): int
  return a + b
end
";

#[test]
fn x32_sharedlib_is_a_pe32_i386_image_with_no_mingw_dll_imports() {
    if !x32_present() {
        return;
    }
    let dir = scratch("dll");
    let src = dir.join("probe.kiln");
    std::fs::write(&src, LIB).unwrap();
    let dll = dir.join("probe32.dll");
    let status = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args([
            "build",
            src.to_str().unwrap(),
            "--os",
            "windows",
            "--arch",
            "x86",
            "--target",
            "sharedlib",
            "-o",
            dll.to_str().unwrap(),
        ])
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .status()
        .expect("run kiln");
    assert!(status.success(), "kiln build --arch x86 failed");

    let bytes = std::fs::read(&dll).expect("read the built DLL");
    assert_eq!(pe_machine(&bytes), 0x014c, "the image must be i386, not x86-64");
    // The mingw runtime DLLs a `long long` divide would otherwise pull in. The
    // image is copied to a machine that has no mingw sysroot, so these must not
    // be in its import table.
    assert!(
        !bytes.windows(20).any(|w| w == b"libgcc_s_dw2-1.dll\0\0"),
        "the DLL imports libgcc_s_dw2-1.dll — -static-libgcc is not in effect"
    );
    assert!(
        !bytes.windows(19).any(|w| w == b"libwinpthread-1.dll"),
        "the DLL imports libwinpthread-1.dll"
    );
}

/// The whole round trip: build a 32-bit DLL and a 32-bit host, load one from
/// the other under wine, and see the loader hook fire and an export answer.
#[test]
fn x32_sharedlib_loads_into_a_32_bit_host_under_wine() {
    if !x32_present() {
        return;
    }
    if !on_path("wine") {
        eprintln!("wine is not installed; the 32-bit image was built but not run");
        return;
    }
    let dir = scratch("run");
    let src = dir.join("probe.kiln");
    std::fs::write(&src, LIB).unwrap();
    let dll = dir.join("probe32.dll");
    let status = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args([
            "build",
            src.to_str().unwrap(),
            "--os",
            "windows",
            "--arch",
            "x86",
            "--target",
            "sharedlib",
            "-o",
            dll.to_str().unwrap(),
        ])
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .status()
        .expect("run kiln");
    assert!(status.success(), "kiln build --arch x86 failed");

    let host_c = dir.join("host.c");
    std::fs::write(
        &host_c,
        r#"
#include <windows.h>
#include <stdio.h>
typedef int (__cdecl *addfn)(int, int);
int main(void) {
    HMODULE h = LoadLibraryA("probe32.dll");
    if (!h) { printf("load failed %lu\n", GetLastError()); return 1; }
    addfn a = (addfn)GetProcAddress(h, "add");
    if (!a) { printf("add export missing\n"); return 1; }
    printf("add(2,3)=%d\n", a(2, 3));
    return 0;
}
"#,
    )
    .unwrap();
    let host = dir.join("host32.exe");
    let status = Command::new("i686-w64-mingw32-gcc")
        .arg(&host_c)
        .arg("-o")
        .arg(&host)
        .status()
        .expect("run i686-w64-mingw32-gcc for the host");
    assert!(status.success(), "building the 32-bit host failed");

    let out = Command::new("wine")
        .arg(&host)
        .current_dir(&dir)
        .output()
        .expect("run wine");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("attached"),
        "the loader hook did not run; stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("add(2,3)=5"),
        "the exported subroutine returned the wrong answer; stdout:\n{stdout}"
    );
}
