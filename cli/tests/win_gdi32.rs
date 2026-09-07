//! The GDI32 half of the `win` declaration kit: `kits/win/gdi32.kdecl`.
//!
//! GDI is one of the few Win32 subsystems a headless machine can actually
//! exercise. A *memory* device context is software all the way down — no
//! window, no display server, no message loop — so a program can create one,
//! select a bitmap into it, draw, and read the pixels back. That is what these
//! tests do, cross-built for Windows with mingw and run under wine, so the
//! declarations are checked against the real gdi32.dll rather than against a
//! transcription of its header.
//!
//! Two programs run. The first draws into a memory DC and checks every struct
//! size, the WORD and float and byte-array fields, and the values GDI answers
//! with. The second walks the rest of the file — state getters and setters,
//! every object kind, shapes, paths, regions, DIB sections — and finishes with
//! a block behind a guard that is never true, holding the declarations that
//! need a real window or a printer. Those cannot run headless; referencing them
//! proves they parse, type-check and lower, which is all this can prove.
//!
//! A fourth test never runs a line of Kiln: it hands mingw a C file of
//! `_Static_assert`s over the real `<windows.h>` — every struct's size, every
//! field's offset, and the value of every constant this kit spells in decimal
//! because the language has no hex literal. That file is what the records were
//! transcribed from, so it pins the numbers the transcription was made against;
//! the sizes then meet Kiln's own `size of` in the drawing program's output,
//! and the field *order* is pinned by the fields the running program reads back
//! after GDI itself wrote them — `BITMAP.width`, `LOGPEN.width.x`,
//! `LOGFONTA.height`, `POINT.x`, `RECT.right`, `XFORM.m11`.
//!
//! Every test says out loud why it skipped when mingw or wine is absent: a
//! silent pass on a machine that cannot cross-build is not evidence.

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
    eprintln!("x86_64-w64-mingw32-gcc is not installed; skipping the win/gdi32 test");
    false
}

/// A scratch directory per test: a Windows program's working files land beside
/// it, and two tests must not share one.
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kiln_win_gdi32_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// The directory to build from, which decides which `kits/win` `use win` finds.
///
/// Normally that is the repository itself, so the tests run against the whole
/// merged kit — every subsystem file at once, which is what ships. While the
/// kit is being written subsystem by subsystem, another file may hold a name
/// twice and the merge fails for a reason that has nothing to do with gdi32; a
/// scratch project holding only the files this stage owns keeps the check
/// honest in the meantime, and says so.
fn kit_cwd(tag: &str) -> PathBuf {
    let listed = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["commands", "--use", "win"])
        .current_dir(repo())
        .output()
        .expect("run kiln commands");
    let stdout = String::from_utf8_lossy(&listed.stdout);
    if listed.status.success() && stdout.contains("dll: BitBlt") {
        return repo();
    }
    let why = String::from_utf8_lossy(&listed.stderr);
    eprintln!(
        "kits/win does not merge yet ({}); building against a scratch kit of the files this stage owns",
        why.lines().next().unwrap_or("no reason given").trim()
    );

    let dir = scratch(&format!("{tag}_kit"));
    let kit = dir.join("kits").join("win");
    std::fs::create_dir_all(&kit).expect("create scratch kit dir");
    // gdi32.kdecl, plus a shared `common*.kdecl` if the integrator has moved the
    // names two subsystems both need — the mechanism the kit spec names.
    std::fs::copy(repo().join("kits/win/gdi32.kdecl"), kit.join("gdi32.kdecl")).expect("copy gdi32.kdecl");
    for entry in std::fs::read_dir(repo().join("kits/win")).expect("read kits/win").flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("common") && name.ends_with(".kdecl") {
            std::fs::copy(entry.path(), kit.join(&name)).expect("copy common kdecl");
        }
    }
    std::fs::write(
        kit.join("lib.json"),
        "{\"display\":\"Windows API\",\"section\":\"System\",\"version\":\"0.1.0\",\"platforms\":[\"windows\"]}\n",
    )
    .expect("write scratch lib.json");
    dir
}

/// Build `source` for Windows from `cwd`, so `use win` resolves against that
/// directory's `kits/`.
fn build_windows(cwd: &Path, source: &str, name: &str) -> PathBuf {
    let dir = scratch(name);
    let src = dir.join(format!("{name}.kiln"));
    std::fs::write(&src, source).expect("write program source");
    let out = dir.join(format!("{name}.exe"));
    let result = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", src.to_str().unwrap(), "--os", "windows", "-o"])
        .arg(&out)
        .current_dir(cwd)
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln build");
    assert!(
        result.status.success(),
        "kiln build --os windows failed for {name}:\n{}",
        String::from_utf8_lossy(&result.stderr)
    );
    out
}

/// `MZ`, `PE\0\0` where `e_lfanew` points, an x86-64 machine word, and the
/// optional header magic that means PE32+.
fn assert_pe32_plus(image: &Path) {
    let bytes = std::fs::read(image).expect("read the built image");
    assert!(bytes.len() > 0x40, "image is too short to be a PE file");
    assert_eq!(&bytes[0..2], b"MZ", "no DOS stub signature");
    let pe = u32::from_le_bytes([bytes[0x3C], bytes[0x3D], bytes[0x3E], bytes[0x3F]]) as usize;
    assert_eq!(&bytes[pe..pe + 4], b"PE\0\0", "no PE signature at e_lfanew");
    let machine = u16::from_le_bytes([bytes[pe + 4], bytes[pe + 5]]);
    assert_eq!(machine, 0x8664, "machine is not x86-64");
    let magic = u16::from_le_bytes([bytes[pe + 24], bytes[pe + 25]]);
    assert_eq!(magic, 0x20B, "optional header is not PE32+");
}

/// Run the image under wine when it is here; `None` when it is not. A Windows
/// console program writes CRLF, and `lines()` strips it.
fn wine_lines(image: &Path) -> Option<Vec<String>> {
    if !on_path("wine") {
        eprintln!("wine is not installed; the Windows image was built but not run");
        return None;
    }
    let out = Command::new("wine")
        .arg(image)
        .current_dir(image.parent().unwrap())
        .env("WINEDEBUG", "-all")
        .output()
        .expect("run wine");
    assert!(
        out.status.success(),
        "the program failed under wine:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| l.trim_end_matches('\r').to_string())
            .collect(),
    )
}

// ---------------------------------------------------------------------------

const DRAW: &str = r#"module gdidraw
use win

sub say(label: text, value: int)
  call print_text(concat(concat(label, "="), int_to_text(value)))
end

sub say64(label: text, value: int64)
  call print_text(concat(concat(label, "="), int64_to_text(value)))
end

sub main
  # Every struct's size, against what the C compiler reports for the same
  # header on the same target.
  call say64("sizeof.POINT", size of POINT)
  call say64("sizeof.RECT", size of RECT)
  call say64("sizeof.SIZE", size of SIZE)
  call say64("sizeof.PAINTSTRUCT", size of PAINTSTRUCT)
  call say64("sizeof.BITMAP", size of BITMAP)
  call say64("sizeof.LOGFONTA", size of LOGFONTA)
  call say64("sizeof.TEXTMETRICA", size of TEXTMETRICA)
  call say64("sizeof.BITMAPINFOHEADER", size of BITMAPINFOHEADER)
  call say64("sizeof.BITMAPINFO", size of BITMAPINFO)
  call say64("sizeof.LOGBRUSH", size of LOGBRUSH)
  call say64("sizeof.LOGPEN", size of LOGPEN)
  call say64("sizeof.XFORM", size of XFORM)
  call say64("sizeof.DOCINFOA", size of DOCINFOA)

  # A memory DC is software all the way down: real GDI with no window.
  var screen: ptr = GetDC(ptr_null())
  var dc: ptr = CreateCompatibleDC(screen)
  var bmp: ptr = CreateCompatibleBitmap(screen, 4, 4)
  var old: ptr = SelectObject(dc, bmp)
  if ptr_is_null(dc) or ptr_is_null(bmp)
    call print_text("dc.made=0")
    return
  end
  call print_text("dc.made=1")

  # GetObjectA writes a real BITMAP through the pointer; `planes` is a WORD.
  var bm: BITMAP
  call say("GetObjectA.bytes", GetObjectA(bmp, 32, address of bm))
  call say("bitmap.width", bm.width)
  call say("bitmap.height", bm.height)
  call say("bitmap.planes", bm.planes)

  # Each of these answers with the state it replaced.
  call say("SetBkMode.prev", SetBkMode(dc, TRANSPARENT))
  call say("GetBkMode", GetBkMode(dc))
  call say("SetTextColor.prev", SetTextColor(dc, 16711680))
  call say("GetTextColor", GetTextColor(dc))

  # FillRect takes a RECT by pointer; the pixels change underneath.
  var box: RECT
  box.right = 4
  box.bottom = 4
  call say("FillRect", FillRect(dc, box, GetStockObject(WHITE_BRUSH)))
  call say("pixel.filled", GetPixel(dc, 1, 1))
  call say("SetPixel.result", SetPixel(dc, 2, 2, 255))
  call say("pixel.set", GetPixel(dc, 2, 2))

  call say("MoveToEx", MoveToEx(dc, 3, 4, ptr_null()))
  var at: POINT
  call say("GetCurrentPositionEx", GetCurrentPositionEx(dc, at))
  call say("position.x", at.x)
  call say("position.y", at.y)

  var tm: TEXTMETRICA
  call say("GetTextMetricsA", GetTextMetricsA(dc, tm))
  if tm.height > 0
    call print_text("metrics.height=positive")
  end

  var ext: SIZE
  call say("GetTextExtentPoint32A", GetTextExtentPoint32A(dc, "hi", 2, ext))
  if ext.cx > 0
    call print_text("extent.cx=positive")
  end

  # A float field round-trips through GDI's own world transform.
  call say("SetGraphicsMode.prev", SetGraphicsMode(dc, GM_ADVANCED))
  var xf: XFORM
  xf.m11 = 2.5
  xf.m22 = 1.0
  call say("SetWorldTransform", SetWorldTransform(dc, xf))
  var back: XFORM
  call say("GetWorldTransform", GetWorldTransform(dc, back))
  if back.m11 = 2.5
    call print_text("xform.m11=2.5")
  end
  call say("SetGraphicsMode.back", SetGraphicsMode(dc, GM_COMPATIBLE))

  # A byte[32] face name filled with mem_copy, then handed to GDI.
  var lf: LOGFONTA
  lf.height = 12
  lf.weight = FW_BOLD
  lf.char_set = DEFAULT_CHARSET
  call mem_copy(address of lf.face_name, ptr_of_text("Arial"), 5)
  call say("facename[1]", lf.face_name[1])
  call say("facename[5]", lf.face_name[5])
  var font: ptr = CreateFontIndirectA(lf)
  call say("font.type", GetObjectType(font))

  var rgn: ptr = CreateRectRgn(0, 0, 4, 4)
  call say("PtInRegion.inside", PtInRegion(rgn, 2, 2))
  call say("PtInRegion.outside", PtInRegion(rgn, 9, 9))
  call say("DeleteObject.region", DeleteObject(rgn))

  call say("Rectangle", Rectangle(dc, 0, 0, 3, 3))
  call say("TextOutA", TextOutA(dc, 0, 0, "hi", 2))

  # A blit between two memory DCs, then PatBlt paints the destination black.
  var dst: ptr = CreateCompatibleDC(screen)
  var dstbmp: ptr = CreateCompatibleBitmap(screen, 4, 4)
  var dstold: ptr = SelectObject(dst, dstbmp)
  call say("FillRect.src", FillRect(dc, box, GetStockObject(WHITE_BRUSH)))
  call say("BitBlt", BitBlt(dst, 0, 0, 4, 4, dc, 0, 0, SRCCOPY))
  call say("pixel.blitted", GetPixel(dst, 1, 1))
  call say("PatBlt", PatBlt(dst, 0, 0, 4, 4, BLACKNESS))
  call say("pixel.blackness", GetPixel(dst, 1, 1))

  call SelectObject(dst, dstold)
  call say("DeleteObject.dstbmp", DeleteObject(dstbmp))
  call say("DeleteDC.dst", DeleteDC(dst))
  call SelectObject(dc, old)
  call say("DeleteObject.font", DeleteObject(font))
  call say("DeleteObject.bmp", DeleteObject(bmp))
  call say("DeleteDC", DeleteDC(dc))
  call say("ReleaseDC", ReleaseDC(ptr_null(), screen))
end
"#;

const REST: &str = r#"module gdirest
use win

sub say(label: text, value: int)
  call print_text(concat(concat(label, "="), int_to_text(value)))
end

sub main
  var screen: ptr = GetDC(ptr_null())
  if ptr_is_null(screen)
    call print_text("screen=0")
    return
  end
  call say("caps.technology", GetDeviceCaps(screen, TECHNOLOGY))
  if GetDeviceCaps(screen, HORZRES) > 0
    call print_text("caps.horzres=positive")
  end

  var dc: ptr = CreateCompatibleDC(screen)
  var bmp: ptr = CreateCompatibleBitmap(screen, 8, 8)
  var old: ptr = SelectObject(dc, bmp)

  # Every setter answers with the state it replaced; RestoreDC puts it back.
  var saved: int = SaveDC(dc)
  call say("SetTextAlign.prev", SetTextAlign(dc, TA_CENTER))
  call say("GetTextAlign", GetTextAlign(dc))
  call say("SetMapMode.prev", SetMapMode(dc, MM_ANISOTROPIC))
  call say("GetMapMode", GetMapMode(dc))
  call say("SetROP2.prev", SetROP2(dc, R2_XORPEN))
  call say("GetROP2", GetROP2(dc))
  call say("SetPolyFillMode.prev", SetPolyFillMode(dc, WINDING))
  call say("SetStretchBltMode.prev", SetStretchBltMode(dc, COLORONCOLOR))
  call say("GetStretchBltMode", GetStretchBltMode(dc))
  call say("SetBkColor.prev", SetBkColor(dc, 255))
  call say("GetBkColor", GetBkColor(dc))
  call say("SetWindowOrgEx", SetWindowOrgEx(dc, 1, 1, ptr_null()))
  call say("SetViewportOrgEx", SetViewportOrgEx(dc, 0, 0, ptr_null()))
  call say("SetWindowExtEx", SetWindowExtEx(dc, 8, 8, ptr_null()))
  call say("SetViewportExtEx", SetViewportExtEx(dc, 8, 8, ptr_null()))
  call say("RestoreDC", RestoreDC(dc, saved))
  call say("GetMapMode.restored", GetMapMode(dc))

  # One object of every kind, named back by GetObjectType.
  var hatch: ptr = CreateHatchBrush(HS_CROSS, 255)
  call say("type.hatchbrush", GetObjectType(hatch))
  var pattern: ptr = CreatePatternBrush(bmp)
  call say("type.patternbrush", GetObjectType(pattern))
  var pen: ptr = CreatePen(PS_DASH, 1, 255)
  call say("type.pen", GetObjectType(pen))
  call say("type.memdc", GetObjectType(dc))

  var lb: LOGBRUSH
  lb.style = KN_SOLID
  lb.color = 65280
  call say("type.brushindirect", GetObjectType(CreateBrushIndirect(lb)))

  # A nested record field written by us and read back out of GDI.
  var lp: LOGPEN
  lp.style = PS_SOLID
  lp.width.x = 2
  lp.color = 255
  var pen2: ptr = CreatePenIndirect(lp)
  var lp2: LOGPEN
  call say("GetObjectA.logpen", GetObjectA(pen2, 16, address of lp2))
  call say("logpen.width.x", lp2.width.x)
  call say("logpen.color", lp2.color)

  var font: ptr = CreateFontA(14, 0, 0, 0, FW_NORMAL, 0, 0, 0, DEFAULT_CHARSET, OUT_TT_PRECIS, CLIP_DEFAULT_PRECIS, ANTIALIASED_QUALITY, VARIABLE_PITCH, "Arial")
  var lf: LOGFONTA
  call say("GetObjectA.logfont", GetObjectA(font, 60, address of lf))
  call say("logfont.height", lf.height)

  call say("SetDCBrushColor.prev", SetDCBrushColor(dc, 255))
  call say("SetDCPenColor.prev", SetDCPenColor(dc, 255))

  call say("Ellipse", Ellipse(dc, 0, 0, 6, 6))
  call say("RoundRect", RoundRect(dc, 0, 0, 6, 6, 2, 2))
  call say("Arc", Arc(dc, 0, 0, 6, 6, 0, 0, 6, 6))
  call say("Chord", Chord(dc, 0, 0, 6, 6, 0, 0, 6, 6))
  call say("Pie", Pie(dc, 0, 0, 6, 6, 0, 0, 6, 6))

  # An array of POINTs is bytes the program lays out, not a c-record.
  var pts: ptr = mem_alloc(32)
  call ptr_write_int(pts, 0, 0)
  call ptr_write_int(pts, 4, 0)
  call ptr_write_int(pts, 8, 6)
  call ptr_write_int(pts, 12, 0)
  call ptr_write_int(pts, 16, 3)
  call ptr_write_int(pts, 20, 6)
  call ptr_write_int(pts, 24, 0)
  call ptr_write_int(pts, 28, 6)
  call say("Polygon", Polygon(dc, pts, 4))
  call say("Polyline", Polyline(dc, pts, 4))
  call say("PolyBezier", PolyBezier(dc, pts, 4))
  call say("DPtoLP", DPtoLP(dc, pts, 4))
  call say("LPtoDP", LPtoDP(dc, pts, 4))
  call mem_free(pts)

  call say("BeginPath", BeginPath(dc))
  call say("path.MoveToEx", MoveToEx(dc, 0, 0, ptr_null()))
  call say("path.LineTo", LineTo(dc, 6, 6))
  call say("CloseFigure", CloseFigure(dc))
  call say("EndPath", EndPath(dc))
  call say("StrokePath", StrokePath(dc))

  var box: RECT
  box.right = 8
  box.bottom = 8
  var r1: ptr = CreateRectRgnIndirect(box)
  var r2: ptr = CreateEllipticRgn(0, 0, 8, 8)
  var r3: ptr = CreateRoundRectRgn(0, 0, 8, 8, 2, 2)
  var r4: ptr = CreateRectRgn(0, 0, 1, 1)
  call say("CombineRgn", CombineRgn(r4, r1, r2, RGN_AND))
  call say("FillRgn", FillRgn(dc, r1, GetStockObject(BLACK_BRUSH)))
  call say("FrameRgn", FrameRgn(dc, r1, GetStockObject(WHITE_BRUSH), 1, 1))
  call say("SelectClipRgn", SelectClipRgn(dc, r1))
  call say("IntersectClipRect", IntersectClipRect(dc, 0, 0, 4, 4))
  call say("ExcludeClipRect", ExcludeClipRect(dc, 0, 0, 1, 1))
  var clip: RECT
  call say("GetClipBox", GetClipBox(dc, clip))
  call say("clipbox.right", clip.right)
  call say("SelectClipRgn.none", SelectClipRgn(dc, ptr_null()))
  call say("DeleteObject.regions", DeleteObject(r1) + DeleteObject(r2) + DeleteObject(r3) + DeleteObject(r4))

  # A DIB section hands the program the pixel bytes; GDI reads what we wrote.
  var info: BITMAPINFO
  info.header.size = 40
  info.header.width = 4
  info.header.height = -4
  info.header.planes = 1
  info.header.bit_count = 32
  info.header.compression = BI_RGB
  var bitsout: ptr = mem_alloc(8)
  var dib: ptr = CreateDIBSection(dc, address of info, DIB_RGB_COLORS, bitsout, ptr_null(), 0)
  if ptr_is_null(dib)
    call print_text("dib=0")
  else
    var pixels: ptr = ptr_read_ptr(bitsout, 0)
    call ptr_write_int(pixels, 0, 16711680)
    var dibdc: ptr = CreateCompatibleDC(screen)
    var dibold: ptr = SelectObject(dibdc, dib)
    call say("dib.pixel", GetPixel(dibdc, 0, 0))
    call say("StretchBlt", StretchBlt(dc, 0, 0, 8, 8, dibdc, 0, 0, 4, 4, SRCCOPY))
    var raw: ptr = mem_alloc(64)
    call say("GetDIBits", GetDIBits(dibdc, dib, 0, 4, raw, address of info, DIB_RGB_COLORS))
    call say("SetDIBits", SetDIBits(dibdc, dib, 0, 4, raw, address of info, DIB_RGB_COLORS))
    call say("StretchDIBits", StretchDIBits(dc, 0, 0, 4, 4, 0, 0, 4, 4, raw, address of info, DIB_RGB_COLORS, SRCCOPY))
    call mem_free(raw)
    call SelectObject(dibdc, dibold)
    call say("DeleteDC.dib", DeleteDC(dibdc))
    call say("DeleteObject.dib", DeleteObject(dib))
  end
  call mem_free(bitsout)

  var raw2: ptr = mem_alloc(8)
  call mem_zero(raw2, 8)
  var mono: ptr = CreateBitmap(4, 4, 1, 1, raw2)
  call say("type.bitmap", GetObjectType(mono))
  call say("DeleteObject.mono", DeleteObject(mono))
  call mem_free(raw2)

  call say("ExtTextOutA", ExtTextOutA(dc, 0, 0, ETO_OPAQUE, address of box, "hi", 2, ptr_null()))
  call say("GdiFlush", GdiFlush())

  call say("DeleteObject.rest", DeleteObject(hatch) + DeleteObject(pattern) + DeleteObject(pen) + DeleteObject(pen2) + DeleteObject(font))
  call SelectObject(dc, old)
  call say("DeleteObject.bmp", DeleteObject(bmp))
  call say("DeleteDC", DeleteDC(dc))
  call say("ReleaseDC", ReleaseDC(ptr_null(), screen))

  # What is left needs a real window or a printer, so it is referenced behind a
  # guard that is never true: enough to prove every remaining declaration
  # parses, type-checks and lowers, which is all a headless run can prove.
  if not ptr_is_null(ptr_from_int(0))
    var wnd: ptr = GetWindowDC(ptr_null())
    var ps: PAINTSTRUCT
    call say("BeginPaint", GetObjectType(BeginPaint(ptr_null(), ps)))
    call say("paint.right", ps.paint.right)
    call say("paint.reserved", ps.reserved[1])
    if ps.erase
      call print_text("paint.erase")
    end
    call say("EndPaint", EndPaint(ptr_null(), ps))
    call say("ReleaseDC.window", ReleaseDC(ptr_null(), wnd))
    var printer: ptr = CreateDCA("WINSPOOL", "printer", "", ptr_null())
    var di: DOCINFOA
    di.cb_size = 40
    di.doc_name = "job"
    di.output = ""
    di.data_type = ""
    call say("StartDocA", StartDocA(printer, di))
    call say("StartPage", StartPage(printer))
    call say("EndPage", EndPage(printer))
    call say("EndDoc", EndDoc(printer))
    call say("AbortDoc", AbortDoc(printer))
    call say("FillPath", FillPath(printer))
    call say("StrokeAndFillPath", StrokeAndFillPath(printer))
  end
  call print_text("every.declaration=referenced")
end
"#;

/// Every line the drawing program prints, in order. The struct sizes are the
/// numbers a C compiler reports for the same headers on the same target — they
/// are the ones `record_layouts_match_the_real_windows_headers` asserts against
/// `<windows.h>`, repeated here so a change to a record breaks a test rather
/// than a program.
const DRAW_EXPECTED: &[&str] = &[
    "sizeof.POINT=8",
    "sizeof.RECT=16",
    "sizeof.SIZE=8",
    "sizeof.PAINTSTRUCT=72",
    "sizeof.BITMAP=32",
    "sizeof.LOGFONTA=60",
    "sizeof.TEXTMETRICA=56",
    "sizeof.BITMAPINFOHEADER=40",
    "sizeof.BITMAPINFO=44",
    "sizeof.LOGBRUSH=16",
    "sizeof.LOGPEN=16",
    "sizeof.XFORM=24",
    "sizeof.DOCINFOA=40",
    "dc.made=1",
    "GetObjectA.bytes=32",      // GDI wrote a whole BITMAP through the pointer
    "bitmap.width=4",
    "bitmap.height=4",
    "bitmap.planes=1",          // a WORD field, read back as an int
    "SetBkMode.prev=2",         // OPAQUE, the mode a fresh DC starts in
    "GetBkMode=1",              // TRANSPARENT
    "SetTextColor.prev=0",      // black
    "GetTextColor=16711680",    // 255 * 65536 — blue, in COLORREF order
    "FillRect=1",
    "pixel.filled=16777215",    // white, through a stock brush and a RECT
    "SetPixel.result=255",
    "pixel.set=255",
    "MoveToEx=1",
    "GetCurrentPositionEx=1",
    "position.x=3",             // written into a POINT by GDI
    "position.y=4",
    "GetTextMetricsA=1",
    "metrics.height=positive",
    "GetTextExtentPoint32A=1",
    "extent.cx=positive",
    "SetGraphicsMode.prev=1",   // GM_COMPATIBLE
    "SetWorldTransform=1",
    "GetWorldTransform=1",
    "xform.m11=2.5",            // a 4-byte float field, round-tripped by GDI
    "SetGraphicsMode.back=2",
    "facename[1]=65",           // 'A' — a byte[32] filled with mem_copy
    "facename[5]=108",          // 'l'
    "font.type=6",              // OBJ_FONT
    "PtInRegion.inside=1",
    "PtInRegion.outside=0",
    "DeleteObject.region=1",
    "Rectangle=1",
    "TextOutA=1",
    "FillRect.src=1",
    "BitBlt=1",
    "pixel.blitted=16777215",   // the white square arrived in the other DC
    "PatBlt=1",
    "pixel.blackness=0",
    "DeleteObject.dstbmp=1",
    "DeleteDC.dst=1",
    "DeleteObject.font=1",
    "DeleteObject.bmp=1",
    "DeleteDC=1",
    "ReleaseDC=1",
];

/// Every line the second program prints, in order.
const REST_EXPECTED: &[&str] = &[
    "caps.technology=1",        // DT_RASDISPLAY
    "caps.horzres=positive",
    "SetTextAlign.prev=0",      // TA_LEFT | TA_TOP
    "GetTextAlign=6",           // TA_CENTER
    "SetMapMode.prev=1",        // MM_TEXT
    "GetMapMode=8",             // MM_ANISOTROPIC
    "SetROP2.prev=13",          // R2_COPYPEN
    "GetROP2=7",                // R2_XORPEN
    "SetPolyFillMode.prev=1",   // ALTERNATE
    "SetStretchBltMode.prev=1", // BLACKONWHITE
    "GetStretchBltMode=3",      // COLORONCOLOR
    "SetBkColor.prev=16777215",
    "GetBkColor=255",
    "SetWindowOrgEx=1",
    "SetViewportOrgEx=1",
    "SetWindowExtEx=1",
    "SetViewportExtEx=1",
    "RestoreDC=1",
    "GetMapMode.restored=1",    // RestoreDC put MM_TEXT back
    "type.hatchbrush=2",        // OBJ_BRUSH
    "type.patternbrush=2",
    "type.pen=1",               // OBJ_PEN
    "type.memdc=10",            // OBJ_MEMDC
    "type.brushindirect=2",     // built from a LOGBRUSH
    "GetObjectA.logpen=16",
    "logpen.width.x=2",         // a nested POINT, written by us, read from GDI
    "logpen.color=255",
    "GetObjectA.logfont=60",
    "logfont.height=14",
    "SetDCBrushColor.prev=16777215",
    "SetDCPenColor.prev=0",
    "Ellipse=1",
    "RoundRect=1",
    "Arc=1",
    "Chord=1",
    "Pie=1",
    "Polygon=1",
    "Polyline=1",
    "PolyBezier=1",
    "DPtoLP=1",
    "LPtoDP=1",
    "BeginPath=1",
    "path.MoveToEx=1",
    "path.LineTo=1",
    "CloseFigure=1",
    "EndPath=1",
    "StrokePath=1",
    "CombineRgn=3",             // COMPLEXREGION: a rectangle met an ellipse
    "FillRgn=1",
    "FrameRgn=1",
    "SelectClipRgn=2",          // SIMPLEREGION
    "IntersectClipRect=2",
    "ExcludeClipRect=3",
    "GetClipBox=3",
    "clipbox.right=4",
    "SelectClipRgn.none=2",
    "DeleteObject.regions=4",
    "dib.pixel=255",            // the bytes we wrote, read back as a COLORREF
    "StretchBlt=1",
    "GetDIBits=4",
    "SetDIBits=4",
    "StretchDIBits=4",
    "DeleteDC.dib=1",
    "DeleteObject.dib=1",
    "type.bitmap=7",            // OBJ_BITMAP
    "DeleteObject.mono=1",
    "ExtTextOutA=1",
    "GdiFlush=1",
    "DeleteObject.rest=5",
    "DeleteObject.bmp=1",
    "DeleteDC=1",
    "ReleaseDC=1",
    "every.declaration=referenced",
];

/// The C file mingw checks: `<windows.h>` on the target these declarations are
/// for, asserting every size, every offset and every constant the kit states.
const LAYOUT_CHECK: &str = r#"/* Cross-check every struct kits/win/gdi32.kdecl declares against the real
 * <windows.h> on the same target the kit is built for. sizeof alone would miss
 * a transposed field, so every member's offset is asserted too. */
#include <windows.h>
#include <stddef.h>

#define OFF(T, F, N) _Static_assert(offsetof(T, F) == (N), #T "." #F " offset")
#define SZ(T, N)     _Static_assert(sizeof(T) == (N), "sizeof " #T)

SZ(POINT, 8);   OFF(POINT, x, 0); OFF(POINT, y, 4);
SZ(RECT, 16);   OFF(RECT, left, 0); OFF(RECT, top, 4); OFF(RECT, right, 8); OFF(RECT, bottom, 12);
SZ(SIZE, 8);    OFF(SIZE, cx, 0); OFF(SIZE, cy, 4);

SZ(PAINTSTRUCT, 72);
OFF(PAINTSTRUCT, hdc, 0);
OFF(PAINTSTRUCT, fErase, 8);
OFF(PAINTSTRUCT, rcPaint, 12);
OFF(PAINTSTRUCT, fRestore, 28);
OFF(PAINTSTRUCT, fIncUpdate, 32);
OFF(PAINTSTRUCT, rgbReserved, 36);
_Static_assert(sizeof(((PAINTSTRUCT *)0)->rgbReserved) == 32, "PAINTSTRUCT.rgbReserved count");

SZ(BITMAP, 32);
OFF(BITMAP, bmType, 0); OFF(BITMAP, bmWidth, 4); OFF(BITMAP, bmHeight, 8);
OFF(BITMAP, bmWidthBytes, 12); OFF(BITMAP, bmPlanes, 16); OFF(BITMAP, bmBitsPixel, 18);
OFF(BITMAP, bmBits, 24);

SZ(LOGFONTA, 60);
OFF(LOGFONTA, lfHeight, 0); OFF(LOGFONTA, lfWidth, 4); OFF(LOGFONTA, lfEscapement, 8);
OFF(LOGFONTA, lfOrientation, 12); OFF(LOGFONTA, lfWeight, 16); OFF(LOGFONTA, lfItalic, 20);
OFF(LOGFONTA, lfUnderline, 21); OFF(LOGFONTA, lfStrikeOut, 22); OFF(LOGFONTA, lfCharSet, 23);
OFF(LOGFONTA, lfOutPrecision, 24); OFF(LOGFONTA, lfClipPrecision, 25);
OFF(LOGFONTA, lfQuality, 26); OFF(LOGFONTA, lfPitchAndFamily, 27);
OFF(LOGFONTA, lfFaceName, 28);
_Static_assert(sizeof(((LOGFONTA *)0)->lfFaceName) == 32, "LOGFONTA.lfFaceName count");

SZ(TEXTMETRICA, 56);
OFF(TEXTMETRICA, tmHeight, 0); OFF(TEXTMETRICA, tmAscent, 4); OFF(TEXTMETRICA, tmDescent, 8);
OFF(TEXTMETRICA, tmInternalLeading, 12); OFF(TEXTMETRICA, tmExternalLeading, 16);
OFF(TEXTMETRICA, tmAveCharWidth, 20); OFF(TEXTMETRICA, tmMaxCharWidth, 24);
OFF(TEXTMETRICA, tmWeight, 28); OFF(TEXTMETRICA, tmOverhang, 32);
OFF(TEXTMETRICA, tmDigitizedAspectX, 36); OFF(TEXTMETRICA, tmDigitizedAspectY, 40);
OFF(TEXTMETRICA, tmFirstChar, 44); OFF(TEXTMETRICA, tmLastChar, 45);
OFF(TEXTMETRICA, tmDefaultChar, 46); OFF(TEXTMETRICA, tmBreakChar, 47);
OFF(TEXTMETRICA, tmItalic, 48); OFF(TEXTMETRICA, tmUnderlined, 49);
OFF(TEXTMETRICA, tmStruckOut, 50); OFF(TEXTMETRICA, tmPitchAndFamily, 51);
OFF(TEXTMETRICA, tmCharSet, 52);

SZ(BITMAPINFOHEADER, 40);
OFF(BITMAPINFOHEADER, biSize, 0); OFF(BITMAPINFOHEADER, biWidth, 4);
OFF(BITMAPINFOHEADER, biHeight, 8); OFF(BITMAPINFOHEADER, biPlanes, 12);
OFF(BITMAPINFOHEADER, biBitCount, 14); OFF(BITMAPINFOHEADER, biCompression, 16);
OFF(BITMAPINFOHEADER, biSizeImage, 20); OFF(BITMAPINFOHEADER, biXPelsPerMeter, 24);
OFF(BITMAPINFOHEADER, biYPelsPerMeter, 28); OFF(BITMAPINFOHEADER, biClrUsed, 32);
OFF(BITMAPINFOHEADER, biClrImportant, 36);

SZ(RGBQUAD, 4);
OFF(RGBQUAD, rgbBlue, 0); OFF(RGBQUAD, rgbGreen, 1); OFF(RGBQUAD, rgbRed, 2);
OFF(RGBQUAD, rgbReserved, 3);

SZ(BITMAPINFO, 44);
OFF(BITMAPINFO, bmiHeader, 0); OFF(BITMAPINFO, bmiColors, 40);

SZ(LOGBRUSH, 16);
OFF(LOGBRUSH, lbStyle, 0); OFF(LOGBRUSH, lbColor, 4); OFF(LOGBRUSH, lbHatch, 8);

SZ(LOGPEN, 16);
OFF(LOGPEN, lopnStyle, 0); OFF(LOGPEN, lopnWidth, 4); OFF(LOGPEN, lopnColor, 12);

SZ(XFORM, 24);
OFF(XFORM, eM11, 0); OFF(XFORM, eM12, 4); OFF(XFORM, eM21, 8);
OFF(XFORM, eM22, 12); OFF(XFORM, eDx, 16); OFF(XFORM, eDy, 20);

SZ(DOCINFOA, 40);
OFF(DOCINFOA, cbSize, 0); OFF(DOCINFOA, lpszDocName, 8); OFF(DOCINFOA, lpszOutput, 16);
OFF(DOCINFOA, lpszDatatype, 24); OFF(DOCINFOA, fwType, 32);

/* BITMAPFILEHEADER is #pragma pack(2): 14 bytes, not the 16 natural alignment
 * would give. The kit cannot express it, and this records why. */
SZ(BITMAPFILEHEADER, 14);

/* The constant values the kit hard-codes in decimal. */
_Static_assert(SRCCOPY == 13369376, "SRCCOPY");
_Static_assert(SRCPAINT == 15597702, "SRCPAINT");
_Static_assert(SRCAND == 8913094, "SRCAND");
_Static_assert(SRCINVERT == 6684742, "SRCINVERT");
_Static_assert(SRCERASE == 4457256, "SRCERASE");
_Static_assert(NOTSRCCOPY == 3342344, "NOTSRCCOPY");
_Static_assert(NOTSRCERASE == 1114278, "NOTSRCERASE");
_Static_assert(MERGECOPY == 12583114, "MERGECOPY");
_Static_assert(MERGEPAINT == 12255782, "MERGEPAINT");
_Static_assert(PATCOPY == 15728673, "PATCOPY");
_Static_assert(PATPAINT == 16452105, "PATPAINT");
_Static_assert(PATINVERT == 5898313, "PATINVERT");
_Static_assert(DSTINVERT == 5570569, "DSTINVERT");
_Static_assert(BLACKNESS == 66, "BLACKNESS");
_Static_assert(WHITENESS == 16711778, "WHITENESS");
_Static_assert(CAPTUREBLT == 1073741824, "CAPTUREBLT");
_Static_assert(TRANSPARENT == 1 && OPAQUE == 2, "bk modes");
_Static_assert(PS_SOLID == 0 && PS_DASH == 1 && PS_DOT == 2 && PS_DASHDOT == 3
    && PS_DASHDOTDOT == 4 && PS_NULL == 5 && PS_INSIDEFRAME == 6, "pen styles");
_Static_assert(KN_SOLID == 0 && KN_NULL == 1 && KN_HOLLOW == 1 && KN_HATCHED == 2
    && KN_PATTERN == 3 && KN_DIBPATTERN == 5, "brush styles");
_Static_assert(HS_HORIZONTAL == 0 && HS_VERTICAL == 1 && HS_FDIAGONAL == 2
    && HS_BDIAGONAL == 3 && HS_CROSS == 4 && HS_DIAGCROSS == 5, "hatch styles");
_Static_assert(WHITE_BRUSH == 0 && LTGRAY_BRUSH == 1 && GRAY_BRUSH == 2
    && DKGRAY_BRUSH == 3 && BLACK_BRUSH == 4 && NULL_BRUSH == 5 && HOLLOW_BRUSH == 5
    && WHITE_PEN == 6 && BLACK_PEN == 7 && NULL_PEN == 8 && OEM_FIXED_FONT == 10
    && ANSI_FIXED_FONT == 11 && ANSI_VAR_FONT == 12 && SYSTEM_FONT == 13
    && DEVICE_DEFAULT_FONT == 14 && DEFAULT_PALETTE == 15 && SYSTEM_FIXED_FONT == 16
    && DEFAULT_GUI_FONT == 17 && DC_BRUSH == 18 && DC_PEN == 19, "stock objects");
_Static_assert(TA_LEFT == 0 && TA_RIGHT == 2 && TA_CENTER == 6 && TA_TOP == 0
    && TA_BOTTOM == 8 && TA_BASELINE == 24 && TA_NOUPDATECP == 0 && TA_UPDATECP == 1,
    "text align");
_Static_assert(MM_TEXT == 1 && MM_LOMETRIC == 2 && MM_HIMETRIC == 3 && MM_LOENGLISH == 4
    && MM_HIENGLISH == 5 && MM_TWIPS == 6 && MM_ISOTROPIC == 7 && MM_ANISOTROPIC == 8,
    "map modes");
_Static_assert(DRIVERVERSION == 0 && TECHNOLOGY == 2 && HORZSIZE == 4 && VERTSIZE == 6
    && HORZRES == 8 && VERTRES == 10 && BITSPIXEL == 12 && PLANES == 14 && NUMCOLORS == 24
    && LOGPIXELSX == 88 && LOGPIXELSY == 90 && COLORRES == 108 && VREFRESH == 116
    && DESKTOPVERTRES == 117 && DESKTOPHORZRES == 118, "device caps");
_Static_assert(R2_BLACK == 1 && R2_NOT == 6 && R2_XORPEN == 7 && R2_COPYPEN == 13
    && R2_WHITE == 16, "rop2");
_Static_assert(ALTERNATE == 1 && WINDING == 2, "poly fill");
_Static_assert(BLACKONWHITE == 1 && WHITEONBLACK == 2 && COLORONCOLOR == 3 && HALFTONE == 4
    && STRETCH_ANDSCANS == 1 && STRETCH_ORSCANS == 2 && STRETCH_DELETESCANS == 3
    && STRETCH_HALFTONE == 4, "stretch modes");
_Static_assert(DIB_RGB_COLORS == 0 && DIB_PAL_COLORS == 1, "dib usage");
_Static_assert(BI_RGB == 0 && BI_RLE8 == 1 && BI_RLE4 == 2 && BI_BITFIELDS == 3, "bi compression");
_Static_assert(RGN_AND == 1 && RGN_OR == 2 && RGN_XOR == 3 && RGN_DIFF == 4 && RGN_COPY == 5,
    "region combine");
_Static_assert(NULLREGION == 1 && SIMPLEREGION == 2 && COMPLEXREGION == 3, "region kinds");
_Static_assert(FW_DONTCARE == 0 && FW_THIN == 100 && FW_NORMAL == 400 && FW_BOLD == 700
    && FW_HEAVY == 900, "font weights");
_Static_assert(ANSI_CHARSET == 0 && DEFAULT_CHARSET == 1 && OEM_CHARSET == 255, "charsets");
_Static_assert(OUT_DEFAULT_PRECIS == 0 && OUT_TT_PRECIS == 4 && CLIP_DEFAULT_PRECIS == 0,
    "font precision");
_Static_assert(DEFAULT_QUALITY == 0 && DRAFT_QUALITY == 1 && PROOF_QUALITY == 2
    && NONANTIALIASED_QUALITY == 3 && ANTIALIASED_QUALITY == 4 && CLEARTYPE_QUALITY == 5,
    "font quality");
_Static_assert(DEFAULT_PITCH == 0 && FIXED_PITCH == 1 && VARIABLE_PITCH == 2
    && FF_DONTCARE == 0 && FF_ROMAN == 16 && FF_SWISS == 32 && FF_MODERN == 48
    && FF_SCRIPT == 64 && FF_DECORATIVE == 80, "pitch and family");
_Static_assert(LF_FACESIZE == 32, "LF_FACESIZE");
_Static_assert(OBJ_PEN == 1 && OBJ_BRUSH == 2 && OBJ_DC == 3 && OBJ_METADC == 4
    && OBJ_PAL == 5 && OBJ_FONT == 6 && OBJ_BITMAP == 7 && OBJ_REGION == 8
    && OBJ_MEMDC == 10, "object kinds");
_Static_assert(DT_LEFT == 0 && DT_CENTER == 1 && DT_RIGHT == 2 && DT_TOP == 0
    && DT_VCENTER == 4 && DT_BOTTOM == 8 && DT_WORDBREAK == 16 && DT_SINGLELINE == 32
    && DT_NOCLIP == 256 && DT_CALCRECT == 1024, "DrawText flags");
_Static_assert((int)CLR_INVALID == -1, "CLR_INVALID");
_Static_assert(ETO_OPAQUE == 2 && ETO_CLIPPED == 4, "ExtTextOut flags");

int main(void) { return 0; }
"#;

/// Nothing in Kiln can tell you whether `record BITMAP is c` has its fields
/// in the order the real `BITMAP` does — two swapped `int`s give the same size.
/// The C compiler can, so it is asked.
#[test]
fn record_layouts_match_the_real_windows_headers() {
    if !mingw_present() {
        return;
    }
    let dir = scratch("layout");
    let src = dir.join("gdi_layout_check.c");
    std::fs::write(&src, LAYOUT_CHECK).expect("write the layout check");
    let out = Command::new("x86_64-w64-mingw32-gcc")
        .args(["-fsyntax-only", "-Wall", "-Wextra"])
        .arg(&src)
        .output()
        .expect("run the mingw cross compiler");
    assert!(
        out.status.success(),
        "kits/win/gdi32.kdecl disagrees with <windows.h>:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A memory DC, a bitmap, and the drawing calls that change its pixels — real
/// GDI, running, with no window anywhere.
#[test]
fn gdi32_draws_into_a_memory_dc() {
    if !mingw_present() {
        return;
    }
    let cwd = kit_cwd("draw");
    let exe = build_windows(&cwd, DRAW, "win_gdi32_draw");
    assert_pe32_plus(&exe);
    let Some(lines) = wine_lines(&exe) else {
        return;
    };
    assert_eq!(lines, DRAW_EXPECTED, "the drawing program said something else");
}

/// The rest of the file: every state call, every object kind, the shapes, the
/// paths, the regions and the DIB section — plus the window-and-printer block
/// that is referenced but never reached.
#[test]
fn gdi32_exercises_the_rest_of_the_bundle() {
    if !mingw_present() {
        return;
    }
    let cwd = kit_cwd("rest");
    let exe = build_windows(&cwd, REST, "win_gdi32_rest");
    assert_pe32_plus(&exe);
    let Some(lines) = wine_lines(&exe) else {
        return;
    };
    assert_eq!(lines, REST_EXPECTED, "the second program said something else");
}

/// `kiln commands --use win` is what Studio's completion and the generated
/// reference read, so the bundle has to be listed and not merely usable. This
/// one needs no cross compiler: listing a kit works on any machine.
#[test]
fn commands_lists_the_gdi32_bundle() {
    let cwd = kit_cwd("list");
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["commands", "--use", "win"])
        .current_dir(&cwd)
        .output()
        .expect("run kiln commands");
    assert!(
        out.status.success(),
        "kiln commands --use win failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let listed = String::from_utf8_lossy(&out.stdout);
    for want in [
        "dll: BitBlt(ptr, int, int, int, int, ptr, int, int, int) -> int from gdi32",
        "dll: TextOutA(ptr, int, int, text, int) -> int from gdi32",
        "dll: CreateSolidBrush(int) -> ptr from gdi32",
        "dll: SelectObject(ptr, ptr) -> ptr from gdi32",
        "dll: GetStockObject(int) -> ptr from gdi32",
        "dll: BeginPaint(ptr, PAINTSTRUCT) -> ptr from user32",
        "crecord: PAINTSTRUCT",
        "crecord: XFORM",
        "crecord: LOGFONTA",
        "const: SRCCOPY",
        "const: TRANSPARENT",
        "const: PS_SOLID",
        "const: WHITE_BRUSH",
        "const: DC_BRUSH",
        "const: KN_SOLID",
    ] {
        assert!(
            listed.lines().any(|l| l.starts_with(want)),
            "`kiln commands --use win` does not list `{want}`"
        );
    }
}
