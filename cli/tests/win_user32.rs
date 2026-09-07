//! The `win` kit's USER32 half: `kits/win/user32.kdecl`.
//!
//! Three things have to be true of a transcribed system header, and each one
//! gets its own test.
//!
//!   1. The structs have the layout Windows has. A c-record that is one field
//!      or one padding byte out of step corrupts the stack of the first API
//!      that fills it in, and nothing about the build says so. So the layout
//!      is checked from BOTH sides against one table: the Kiln side prints
//!      `size of` and the `address of` deltas under wine, and the mingw
//!      Windows SDK headers are asked the same questions with `_Static_assert`
//!      on `sizeof`/`offsetof`. Either side alone would only prove the .kdecl
//!      self-consistent.
//!   2. The functions are really there and really callable. The ones whose
//!      effect is visible with no window, no display driver and no second
//!      process are called for real, under wine with its display drivers
//!      turned off.
//!   3. Everything else — a window class, a message loop, a hook, a menu, the
//!      clipboard — builds: parsed, type-checked, lowered and linked into a
//!      PE32+ image. Running those needs a desktop, and a test that needs a
//!      desktop is a test that is skipped on every machine that matters.
//!
//! Each test says why it is skipping when mingw or wine is not installed. A
//! green run on a machine without them proves nothing about this kit.

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

fn mingw_present() -> bool {
    if on_path("x86_64-w64-mingw32-gcc") {
        return true;
    }
    eprintln!("x86_64-w64-mingw32-gcc is not installed; skipping");
    false
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kiln_win_user32_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Build `source` for Windows with the working directory pinned to the repo,
/// so `kits/win` resolves as the project kit `use win` asks for.
fn build_windows(dir: &Path, source: &str, out: &str) -> Result<PathBuf, String> {
    let srcpath = dir.join("prog.kiln");
    std::fs::write(&srcpath, source).expect("write program source");
    let outpath = dir.join(out);
    let output = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", srcpath.to_str().unwrap(), "--os", "windows", "-o"])
        .arg(&outpath)
        .current_dir(repo())
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln build");
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    Ok(outpath)
}

/// Run a built image under wine with the display drivers turned OFF, so no
/// test here can put a window on the screen of whoever is working on this
/// machine. `None` when wine is not installed.
fn wine_lines(image: &Path, cwd: &Path) -> Option<Vec<String>> {
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
    let out = cmd
        .arg(image)
        .current_dir(cwd)
        .env("WINEDEBUG", "-all")
        .env("WINEDLLOVERRIDES", "winex11.drv,winewayland.drv=d")
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .output()
        .expect("run wine");
    assert!(
        out.status.success(),
        "the Windows program exited non-zero under wine:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect(),
    )
}

/// One table, two readers. Each row is a label, the number the `.kdecl` layout
/// implies, and the `sizeof`/`offsetof` expression the Windows SDK is asked
/// for the same number. `layout_program` prints the numbers in this order and
/// `sdk_agrees_with_the_table` static-asserts the C side of each row.
const LAYOUT: &[(&str, i64, &str)] = &[
    ("sizeof MSG", 48, "sizeof(MSG)"),
    ("offsetof MSG.pt", 36, "offsetof(MSG, pt)"),
    ("sizeof WNDCLASSEXA", 80, "sizeof(WNDCLASSEXA)"),
    ("offsetof WNDCLASSEXA.wnd_extra", 20, "offsetof(WNDCLASSEXA, cbWndExtra)"),
    ("offsetof WNDCLASSEXA.class_name", 64, "offsetof(WNDCLASSEXA, lpszClassName)"),
    ("sizeof WNDCLASSA", 72, "sizeof(WNDCLASSA)"),
    ("sizeof CREATESTRUCTA", 80, "sizeof(CREATESTRUCTA)"),
    ("offsetof CREATESTRUCTA.style", 48, "offsetof(CREATESTRUCTA, style)"),
    ("offsetof CREATESTRUCTA.name", 56, "offsetof(CREATESTRUCTA, lpszName)"),
    ("sizeof MINMAXINFO", 40, "sizeof(MINMAXINFO)"),
    ("sizeof WINDOWPLACEMENT", 44, "sizeof(WINDOWPLACEMENT)"),
    ("sizeof WINDOWPOS", 40, "sizeof(WINDOWPOS)"),
    ("sizeof KBDLLHOOKSTRUCT", 24, "sizeof(KBDLLHOOKSTRUCT)"),
    ("offsetof KBDLLHOOKSTRUCT.extra_info", 16, "offsetof(KBDLLHOOKSTRUCT, dwExtraInfo)"),
    ("sizeof MSLLHOOKSTRUCT", 32, "sizeof(MSLLHOOKSTRUCT)"),
    ("offsetof MSLLHOOKSTRUCT.extra_info", 24, "offsetof(MSLLHOOKSTRUCT, dwExtraInfo)"),
    ("sizeof CWPSTRUCT", 32, "sizeof(CWPSTRUCT)"),
    ("sizeof MOUSEINPUT", 32, "sizeof(MOUSEINPUT)"),
    ("sizeof KEYBDINPUT", 24, "sizeof(KEYBDINPUT)"),
    // The union: both arms measure the real sizeof(INPUT), which is what
    // SendInput's third argument has to be given.
    ("sizeof INPUT", 40, "sizeof(INPUT)"),
    ("sizeof INPUTMOUSE", 40, "sizeof(INPUT)"),
    ("offsetof INPUT.ki", 8, "offsetof(INPUT, ki)"),
    // PAINTSTRUCT and RECT are declared in the kit's gdi32.kdecl; the calls
    // here take them, so their layout is this file's business too.
    ("sizeof PAINTSTRUCT", 72, "sizeof(PAINTSTRUCT)"),
    ("offsetof PAINTSTRUCT.paint", 12, "offsetof(PAINTSTRUCT, rcPaint)"),
    ("offsetof PAINTSTRUCT.reserved", 36, "offsetof(PAINTSTRUCT, rgbReserved)"),
    ("sizeof RECT", 16, "sizeof(RECT)"),
];

/// The constants, checked against the SDK's own macros. Each entry is a C
/// expression that must be true where `windows.h` is the authority.
const CONSTANTS: &[&str] = &[
    "WM_CREATE == 1 && WM_DESTROY == 2 && WM_SIZE == 5 && WM_PAINT == 15",
    "WM_CLOSE == 16 && WM_QUIT == 18 && WM_KEYDOWN == 256 && WM_LBUTTONDOWN == 513",
    "WM_COMMAND == 273 && WM_TIMER == 275 && WM_HOTKEY == 786 && WM_CLIPBOARDUPDATE == 797",
    "WM_USER == 1024 && WM_APP == 32768 && WM_MOUSEWHEEL == 522 && WM_NCCREATE == 129",
    "WS_OVERLAPPEDWINDOW == 13565952u && WS_VISIBLE == 268435456u",
    "WS_CHILD == 1073741824u && WS_BORDER == 8388608u && WS_CAPTION == 12582912u",
    "(int) WS_POPUP == -2147483647 - 1 && (int) WS_POPUPWINDOW == -2138570752",
    "CW_USEDEFAULT == -2147483647 - 1",
    "MB_OK == 0 && MB_OKCANCEL == 1 && MB_YESNO == 4",
    "MB_ICONERROR == 16 && MB_ICONINFORMATION == 64 && IDYES == 6 && IDNO == 7",
    "SW_HIDE == 0 && SW_SHOWNORMAL == 1 && SW_SHOW == 5",
    "WH_KEYBOARD_LL == 13 && WH_MOUSE_LL == 14 && WH_CBT == 5 && HC_ACTION == 0",
    "(int) (intptr_t) IDC_ARROW == 32512 && (int) (intptr_t) IDI_APPLICATION == 32512",
    "COLOR_WINDOW == 5 && COLOR_BTNFACE == 15",
    "SM_CXSCREEN == 0 && SM_CYSCREEN == 1 && SM_CMONITORS == 80 && SM_CXICON == 11",
    "VK_RETURN == 13 && VK_ESCAPE == 27 && VK_SPACE == 32 && VK_F1 == 112",
    "VK_F12 == 123 && VK_LEFT == 37 && VK_DELETE == 46 && VK_RMENU == 165",
    "(int) (intptr_t) HWND_MESSAGE == -3 && (int) (intptr_t) HWND_TOPMOST == -1",
    "GWL_STYLE == -16 && GWLP_USERDATA == -21 && GWLP_WNDPROC == -4",
    "CF_TEXT == 1 && CF_UNICODETEXT == 13 && CF_HDROP == 15",
    "MOD_ALT == 1 && MOD_WIN == 8 && MOD_NOREPEAT == 16384",
    "PM_REMOVE == 1 && SWP_NOSIZE == 1 && SWP_FRAMECHANGED == 32",
    "DT_SINGLELINE == 32 && DT_CALCRECT == 1024 && MF_POPUP == 16 && MF_SEPARATOR == 2048",
    "SC_CLOSE == 61536 && SC_MINIMIZE == 61472 && CS_HREDRAW == 2 && CS_VREDRAW == 1",
    "INPUT_KEYBOARD == 1 && KEYEVENTF_KEYUP == 2 && MOUSEEVENTF_LEFTDOWN == 2",
    "MAPVK_VK_TO_VSC == 0 && LR_LOADFROMFILE == 16 && IMAGE_ICON == 1",
];

/// The Kiln half of the layout check: `size of` for a whole struct, and
/// the difference of two `address of` for a member's offset.
fn layout_program() -> String {
    let mut body = String::from("module layout\nuse win\n\nsub main\n");
    body.push_str("  var msg: MSG\n  var wc: WNDCLASSEXA\n  var cs: CREATESTRUCTA\n");
    body.push_str("  var kb: KBDLLHOOKSTRUCT\n  var ml: MSLLHOOKSTRUCT\n");
    body.push_str("  var inp: INPUT\n  var ps: PAINTSTRUCT\n");
    for (label, _, _) in LAYOUT {
        let line = match *label {
            "sizeof MSG" => "size of MSG".into(),
            "offsetof MSG.pt" => offset("msg", "msg.pt"),
            "sizeof WNDCLASSEXA" => "size of WNDCLASSEXA".into(),
            "offsetof WNDCLASSEXA.wnd_extra" => offset("wc", "wc.wnd_extra"),
            "offsetof WNDCLASSEXA.class_name" => offset("wc", "wc.class_name"),
            "sizeof WNDCLASSA" => "size of WNDCLASSA".into(),
            "sizeof CREATESTRUCTA" => "size of CREATESTRUCTA".into(),
            "offsetof CREATESTRUCTA.style" => offset("cs", "cs.style"),
            "offsetof CREATESTRUCTA.name" => offset("cs", "cs.name"),
            "sizeof MINMAXINFO" => "size of MINMAXINFO".into(),
            "sizeof WINDOWPLACEMENT" => "size of WINDOWPLACEMENT".into(),
            "sizeof WINDOWPOS" => "size of WINDOWPOS".into(),
            "sizeof KBDLLHOOKSTRUCT" => "size of KBDLLHOOKSTRUCT".into(),
            "offsetof KBDLLHOOKSTRUCT.extra_info" => offset("kb", "kb.extra_info"),
            "sizeof MSLLHOOKSTRUCT" => "size of MSLLHOOKSTRUCT".into(),
            "offsetof MSLLHOOKSTRUCT.extra_info" => offset("ml", "ml.extra_info"),
            "sizeof CWPSTRUCT" => "size of CWPSTRUCT".into(),
            "sizeof MOUSEINPUT" => "size of MOUSEINPUT".into(),
            "sizeof KEYBDINPUT" => "size of KEYBDINPUT".into(),
            "sizeof INPUT" => "size of INPUT".into(),
            "sizeof INPUTMOUSE" => "size of INPUTMOUSE".into(),
            "offsetof INPUT.ki" => offset("inp", "inp.ki"),
            "sizeof PAINTSTRUCT" => "size of PAINTSTRUCT".into(),
            "offsetof PAINTSTRUCT.paint" => offset("ps", "ps.paint"),
            "offsetof PAINTSTRUCT.reserved" => offset("ps", "ps.reserved"),
            "sizeof RECT" => "size of RECT".into(),
            other => panic!("no Kiln spelling for {other}"),
        };
        body.push_str(&format!("  call print_int64({line})\n"));
    }
    body.push_str("end\n");
    body
}

fn offset(whole: &str, member: &str) -> String {
    format!("ptr_to_int(address of {member}) - ptr_to_int(address of {whole})")
}

/// The Windows SDK's answer to the same table, and to the constants. Compiled
/// with the mingw headers — `-fsyntax-only`, so nothing is produced and
/// nothing is run: a `_Static_assert` that fails is a compile error naming the
/// row that disagrees.
#[test]
fn sdk_agrees_with_the_table() {
    if !mingw_present() {
        return;
    }
    let dir = scratch("sdk");
    let mut c = String::from("#include <windows.h>\n#include <stddef.h>\n#include <stdint.h>\n");
    for (label, want, expr) in LAYOUT {
        c.push_str(&format!("_Static_assert({expr} == {want}, \"{label}\");\n"));
    }
    for (i, expr) in CONSTANTS.iter().enumerate() {
        c.push_str(&format!("_Static_assert({expr}, \"constants row {i}\");\n"));
    }
    let path = dir.join("layout.c");
    std::fs::write(&path, c).expect("write the C check");

    let out = Command::new("x86_64-w64-mingw32-gcc")
        .args(["-fsyntax-only", "-Wall", "-Wextra", "-I", "abi", "-I", "runtime"])
        .arg(&path)
        .current_dir(repo())
        .output()
        .expect("run x86_64-w64-mingw32-gcc");
    assert!(
        out.status.success(),
        "the Windows SDK headers disagree with kits/win/user32.kdecl:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The Kiln side of the same table, measured by a real program on a real
/// Windows loader. A struct one field out of step shows up here as a number
/// that does not match the row.
#[test]
fn the_records_have_the_layout_windows_has() {
    if !mingw_present() {
        return;
    }
    let dir = scratch("layout");
    let bin = build_windows(&dir, &layout_program(), "layout.exe").expect("build should succeed");
    let Some(lines) = wine_lines(&bin, &dir) else {
        return;
    };
    assert_eq!(lines.len(), LAYOUT.len(), "printed {} lines, want {}", lines.len(), LAYOUT.len());
    for (line, (label, want, _)) in lines.iter().zip(LAYOUT) {
        assert_eq!(line, &want.to_string(), "{label}");
    }
}

/// Everything USER32 answers with no window, no display driver and no second
/// process — called for real, under wine.
const CALLS: &str = "\
module calls
use win

sub tally(hwnd: ptr, param: int64): bool system
  return true
end

sub main
  # An atom for a private clipboard format and a private window message: both
  # come out of the same 0xC000..0xFFFF range, so both must be >= 49152.
  call print_int(RegisterClipboardFormatA(\"KilnUser32Format\"))
  call print_int(RegisterWindowMessageA(\"KilnUser32Message\"))

  # A screen this program will never draw on still has a width and a height.
  call print_int(GetSystemMetrics(SM_CXSCREEN))
  call print_int(GetSystemMetrics(SM_CYSCREEN))
  call print_int(GetSystemMetrics(SM_CXICON))
  call print_int(GetDoubleClickTime())

  # F13 is not on the keyboard, so neither the down bit nor anything else is
  # set. This is what proves the SHORT return crosses the boundary at all.
  call print_int(GetAsyncKeyState(124))
  call print_int(GetKeyState(124))

  # The scan code of Return on any layout Windows ships.
  call print_int(MapVirtualKeyA(VK_RETURN, MAPVK_VK_TO_VSC))

  # A null HWND is not a window, and the desktop always is one.
  if IsWindow(ptr_null())
    call print_text(\"null-is-a-window\")
  else
    call print_text(\"null-is-not-a-window\")
  end
  var desktop: ptr = GetDesktopWindow()
  if IsWindow(desktop)
    call print_text(\"desktop-is-a-window\")
  else
    call print_text(\"desktop-is-not-a-window\")
  end

  # A callback handed to user32, which user32 calls back through. There may be
  # no windows at all here; what is proved is that the enumeration ran and
  # came back.
  if EnumWindows(address of tally, 0)
    call print_text(\"enumerated\")
  else
    call print_text(\"not-enumerated\")
  end

  # A timer with no window: the return is the id, and it must come back.
  let timer: int64 = SetTimer(ptr_null(), 0, 60000, ptr_null())
  if timer = int_to_int64(0)
    call print_text(\"no-timer\")
  else
    call print_text(\"timer\")
  end
  if KillTimer(ptr_null(), timer)
    call print_text(\"timer-killed\")
  else
    call print_text(\"timer-not-killed\")
  end

  # The RECT helpers are pure arithmetic on a struct this program owns, so
  # they read the c-record and write it back through the pointer.
  var box: RECT
  call SetRect(box, 1, 2, 31, 42)
  call print_int(box.right - box.left)
  call OffsetRect(box, 5, 5)
  call print_int(box.left)
  call InflateRect(box, 1, 1)
  call print_int(box.right)
  if IsRectEmpty(box)
    call print_text(\"empty\")
  else
    call print_text(\"not-empty\")
  end
  call SetRectEmpty(box)
  if IsRectEmpty(box)
    call print_text(\"empty\")
  else
    call print_text(\"not-empty\")
  end

  # A MAKEINTRESOURCE argument, passed as the pointer it is.
  if ptr_is_null(LoadCursorA(ptr_null(), ptr_from_int(IDC_ARROW)))
    call print_text(\"no-cursor\")
  else
    call print_text(\"cursor\")
  end

  # The clipboard is the wine prefix's, shared with anything else running in
  # it, so what is asserted is that the calls came back — not what is on it.
  call print_int(CountClipboardFormats())
  if IsClipboardFormatAvailable(CF_TEXT)
    call print_text(\"has-text\")
  else
    call print_text(\"no-text\")
  end

  # A message posted nowhere: DefWindowProcA on a null window answers 0, and
  # SendMessageA with WM_NULL does the nothing it is defined to do.
  call print_int64(SendMessageA(ptr_null(), WM_NULL, 0, 0))
  call print_int64(DefWindowProcA(ptr_null(), WM_NULL, 0, 0))

  # PeekMessageA on a queue with nothing in it.
  var msg: MSG
  if PeekMessageA(msg, ptr_null(), 0, 0, PM_NOREMOVE)
    call print_text(\"a-message\")
  else
    call print_text(\"no-message\")
  end
end
";

#[test]
fn the_calls_that_need_no_desktop_run_under_wine() {
    if !mingw_present() {
        return;
    }
    let dir = scratch("calls");
    let bin = build_windows(&dir, CALLS, "calls.exe").expect("build should succeed");
    let Some(lines) = wine_lines(&bin, &dir) else {
        return;
    };
    assert_eq!(lines.len(), 25, "printed {} lines:\n{}", lines.len(), lines.join("\n"));

    let num = |i: usize| -> i64 {
        lines[i]
            .parse()
            .unwrap_or_else(|_| panic!("line {} is not a number: {:?}", i + 1, lines[i]))
    };
    // A registered atom lives in 0xC000..=0xFFFF.
    for i in 0..2 {
        assert!((49152..=65535).contains(&num(i)), "atom {i} is {}", num(i));
    }
    // Screen metrics: a real number, whatever this machine's desktop is.
    assert!(num(2) > 0, "SM_CXSCREEN is {}", num(2));
    assert!(num(3) > 0, "SM_CYSCREEN is {}", num(3));
    assert!(num(4) > 0, "SM_CXICON is {}", num(4));
    assert!(num(5) > 0, "GetDoubleClickTime is {}", num(5));
    // A key that is not on the keyboard is not down and not toggled.
    assert_eq!(num(6), 0, "GetAsyncKeyState(VK_F13)");
    assert_eq!(num(7), 0, "GetKeyState(VK_F13)");
    assert_eq!(num(8), 28, "MapVirtualKeyA(VK_RETURN, MAPVK_VK_TO_VSC)");

    assert_eq!(lines[9], "null-is-not-a-window");
    assert_eq!(lines[10], "desktop-is-a-window");
    assert_eq!(lines[11], "enumerated");
    assert_eq!(lines[12], "timer");
    assert_eq!(lines[13], "timer-killed");

    assert_eq!(num(14), 30, "SetRect then right - left");
    assert_eq!(num(15), 6, "OffsetRect moved left by 5");
    assert_eq!(num(16), 37, "InflateRect grew right by 1");
    assert_eq!(lines[17], "not-empty");
    assert_eq!(lines[18], "empty");

    assert_eq!(lines[19], "cursor", "LoadCursorA(NULL, IDC_ARROW)");
    assert!(num(20) >= 0, "CountClipboardFormats is {}", num(20));
    assert!(
        lines[21] == "has-text" || lines[21] == "no-text",
        "IsClipboardFormatAvailable answered {:?}",
        lines[21]
    );
    assert_eq!(num(22), 0, "SendMessageA(NULL, WM_NULL, 0, 0)");
    assert_eq!(num(23), 0, "DefWindowProcA(NULL, WM_NULL, 0, 0)");
    assert_eq!(lines[24], "no-message");
}

/// The rest of the surface: a window class, a real WNDPROC handed to Windows
/// as a function pointer, a message pump, a low-level keyboard hook, a hot
/// key, synthesised input, a menu, and the clipboard. None of it can run
/// without a desktop; all of it has to parse, type-check, lower and link.
const GUI: &str = "\
module gui
use win

sub wndproc(hwnd: ptr, msg: int, wparam: int64, lparam: int64): int64 system
  if msg = WM_DESTROY
    call PostQuitMessage(0)
    return 0
  end
  if msg = WM_PAINT
    var ps: PAINTSTRUCT
    var dc: ptr = BeginPaint(hwnd, ps)
    call FillRect(dc, ps.paint, ptr_from_int(int_to_int64(COLOR_WINDOW + 1)))
    var area: RECT
    call GetClientRect(hwnd, area)
    call DrawTextA(dc, \"Kiln\", -1, area, DT_CENTER + DT_VCENTER + DT_SINGLELINE)
    call EndPaint(hwnd, ps)
    return 0
  end
  if msg = WM_COMMAND
    call SetWindowTextA(hwnd, \"clicked\")
    return 0
  end
  return DefWindowProcA(hwnd, msg, wparam, lparam)
end

sub keyhook(code: int, wparam: int64, lparam: int64): int64 system
  if code = HC_ACTION
    # LPARAM points at a KBDLLHOOKSTRUCT; vkCode is its first field.
    call print_int(ptr_read_int(ptr_from_int(lparam), 0))
  end
  return CallNextHookEx(ptr_null(), code, wparam, lparam)
end

sub main
  var wc: WNDCLASSEXA
  wc.cb_size = int64_to_int(size of WNDCLASSEXA)
  wc.style = CS_HREDRAW + CS_VREDRAW
  wc.wnd_proc = address of wndproc
  wc.cursor = LoadCursorA(ptr_null(), ptr_from_int(IDC_ARROW))
  wc.icon = LoadIconA(ptr_null(), ptr_from_int(IDI_APPLICATION))
  wc.background = ptr_from_int(int_to_int64(COLOR_WINDOW + 1))
  wc.class_name = \"KilnWindow\"
  let atom: int = RegisterClassExA(wc)

  var menu: ptr = CreateMenu()
  var popup: ptr = CreatePopupMenu()
  call AppendMenuA(popup, MF_STRING, 1001, \"E&xit\")
  call AppendSubMenuA(menu, MF_POPUP, popup, \"&File\")

  var hwnd: ptr = CreateWindowExA(WS_EX_CLIENTEDGE, \"KilnWindow\", \"Kiln\", WS_OVERLAPPEDWINDOW, CW_USEDEFAULT, CW_USEDEFAULT, 640, 480, ptr_null(), menu, ptr_null(), ptr_null())
  call ShowWindow(hwnd, SW_SHOWNORMAL)
  call UpdateWindow(hwnd)
  call SetMenu(hwnd, menu)
  call SetWindowLongPtrA(hwnd, GWLP_USERDATA, 7)
  call print_int64(GetWindowLongPtrA(hwnd, GWLP_USERDATA))
  call MoveWindow(hwnd, 10, 10, 320, 240, true)
  call SetWindowPos(hwnd, ptr_from_int(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE + SWP_NOSIZE)
  call SetWindowTextA(hwnd, \"Kiln\")
  call InvalidateRect(hwnd, ptr_null(), true)

  var area: RECT
  call GetClientRect(hwnd, area)
  var frame: RECT
  call GetWindowRect(hwnd, frame)
  var where: POINT
  call GetCursorPos(where)
  call ScreenToClient(hwnd, where)

  var hook: ptr = SetWindowsHookExA(WH_KEYBOARD_LL, address of keyhook, ptr_null(), 0)
  call RegisterHotKey(hwnd, 1, MOD_CONTROL + MOD_NOREPEAT, VK_F5)

  var keys: INPUT
  keys.kind = INPUT_KEYBOARD
  keys.ki.vk = VK_RETURN
  keys.ki.flags = KEYEVENTF_KEYUP
  call print_int(SendInput(1, address of keys, int64_to_int(size of INPUT)))

  if OpenClipboard(hwnd)
    call EmptyClipboard()
    var data: ptr = GetClipboardData(CF_TEXT)
    call SetClipboardData(CF_TEXT, data)
    call CloseClipboard()
  end

  let answer: int = MessageBoxA(hwnd, \"Built with a kit.\", \"Kiln\", MB_YESNO + MB_ICONINFORMATION)
  if answer = IDYES
    call print_text(\"yes\")
  end

  var msg: MSG
  while GetMessageA(msg, ptr_null(), 0, 0) > 0
    call TranslateMessage(msg)
    call DispatchMessageA(msg)
  end

  call UnregisterHotKey(hwnd, 1)
  call UnhookWindowsHookEx(hook)
  call DestroyMenu(menu)
  call DestroyWindow(hwnd)
  call UnregisterClassA(\"KilnWindow\", ptr_null())
  call print_int(atom)
end
";

/// A PE32+ image for x86-64: the whole GUI program above got through the
/// compiler and the linker.
fn assert_pe32_plus(image: &Path) {
    let bytes = std::fs::read(image).expect("read the built image");
    assert!(bytes.len() > 0x40, "image is too short to be a PE file");
    assert_eq!(&bytes[0..2], b"MZ", "no DOS stub signature");
    let pe = u32::from_le_bytes([bytes[0x3C], bytes[0x3D], bytes[0x3E], bytes[0x3F]]) as usize;
    assert_eq!(&bytes[pe..pe + 4], b"PE\0\0", "no PE signature at e_lfanew");
    assert_eq!(
        u16::from_le_bytes([bytes[pe + 4], bytes[pe + 5]]),
        0x8664,
        "machine is not x86-64"
    );
    assert_eq!(
        u16::from_le_bytes([bytes[pe + 24], bytes[pe + 25]]),
        0x20B,
        "optional header is not PE32+"
    );
}

#[test]
fn a_whole_windowed_program_cross_builds() {
    if !mingw_present() {
        return;
    }
    let dir = scratch("gui");
    let bin = build_windows(&dir, GUI, "gui.exe").expect("build should succeed");
    assert_pe32_plus(&bin);
}

/// The kit's declarations reach `kiln commands`, which is what the
/// language server's completion and the generated reference read.
#[test]
fn the_declarations_are_listed() {
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["commands", "--use", "win"])
        .current_dir(repo())
        .output()
        .expect("run kiln commands");
    assert!(
        out.status.success(),
        "kiln commands --use win failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let listing = String::from_utf8_lossy(&out.stdout);

    for name in [
        "CreateWindowExA",
        "RegisterClassExA",
        "DefWindowProcA",
        "ShowWindow",
        "UpdateWindow",
        "DestroyWindow",
        "GetClientRect",
        "GetWindowRect",
        "MoveWindow",
        "SetWindowTextA",
        "GetWindowLongPtrA",
        "SetWindowLongPtrA",
        "GetMessageA",
        "TranslateMessage",
        "DispatchMessageA",
        "PeekMessageA",
        "PostQuitMessage",
        "SendMessageA",
        "PostMessageA",
        "MessageBoxA",
        "LoadCursorA",
        "LoadIconA",
        "GetSystemMetrics",
        "GetAsyncKeyState",
        "GetKeyState",
        "SetWindowsHookExA",
        "CallNextHookEx",
        "UnhookWindowsHookEx",
        "RegisterHotKey",
        "OpenClipboard",
        "GetClipboardData",
        "SetClipboardData",
        "CloseClipboard",
        "EmptyClipboard",
    ] {
        assert!(
            listing.contains(&format!("dll: {name}(")) || listing.contains(&format!("dll: {name}()")),
            "`{name}` is not in `kiln commands --use win`"
        );
    }
    for record in ["MSG", "WNDCLASSEXA", "KBDLLHOOKSTRUCT", "INPUT"] {
        assert!(
            listing.contains(&format!("crecord: {record} ")),
            "`{record}` is not in `kiln commands --use win`"
        );
    }
    for name in [
        "WM_DESTROY", "WM_PAINT", "WM_COMMAND", "WM_QUIT",
        "WS_OVERLAPPEDWINDOW", "WS_VISIBLE", "MB_YESNO", "SW_SHOW",
        "CW_USEDEFAULT", "WH_KEYBOARD_LL", "IDC_ARROW", "IDI_APPLICATION",
        "COLOR_WINDOW", "SM_CXSCREEN", "VK_RETURN", "HWND_MESSAGE",
    ] {
        assert!(
            listing.contains(&format!("const: {name} ")),
            "`{name}` is not in `kiln commands --use win`"
        );
    }
}
