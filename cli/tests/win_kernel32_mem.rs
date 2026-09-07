//! `kits/win/kernel32_mem.kdecl` — the memory, module and file third of the
//! `win` declaration kit — held to what Windows actually does.
//!
//! The programs here are cross-built for Windows with mingw and run under
//! wine, which is where a Win32 binding can be proved rather than asserted:
//! a wrong struct offset, a `DWORD` written as `int64`, a symbol that is not
//! exported — none of those show up in a parse, and all of them show up the
//! moment `VirtualQuery` fills a struct or `FindFirstFileA` writes a name.
//!
//! Two programs, because two things need proving:
//!
//! * `RUN` calls everything whose effect is visible inside one console
//!   process, and checks the effect: the layouts against C's `sizeof`, the
//!   protection `VirtualQuery` reports for a page just allocated, the bytes a
//!   file gives back through a mapping, the name `FindFirstFileA` writes into
//!   `cFileName`.
//! * `TOUCH` calls every remaining declaration once, from a subroutine behind
//!   a condition that is never true. A `dll` nobody calls proves nothing —
//!   the library is opened lazily and the signature is never looked at — so a
//!   call site is what makes the compiler check the line. It exists to build,
//!   not to run.
//!
//! Both build against a scratch copy of the kit holding this one `.kdecl`, plus
//! a stand-in for the handful of names the sibling files own (`CloseHandle`,
//! `GetCurrentProcess`, `SECURITY_ATTRIBUTES`, `INVALID_HANDLE_VALUE` and
//! three `ERROR_*`). That keeps this test about this file: the whole `kits/win`
//! is several agents' work merging into one namespace, and a collision between
//! two other files is not a reason for this one to go red.
//!
//! Each test says why it is skipping when mingw or wine is not on the machine,
//! so a green run there is not mistaken for proof.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
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
    eprintln!("x86_64-w64-mingw32-gcc is not installed; skipping the win kernel32 memory test");
    false
}

/// The names the process and registry halves of the kit own. This file uses
/// them and deliberately does not declare them — a name belongs to one `.kdecl`
/// per kit — so the scratch kit supplies them the way `kits/win` does.
const SIBLING_STANDINS: &str = r#"dll CloseHandle(hObject: ptr): bool from "kernel32" system
dll GetCurrentProcess(): ptr from "kernel32" system
record SECURITY_ATTRIBUTES is c
  nLength: int
  lpSecurityDescriptor: ptr
  bInheritHandle: bool
end
const INVALID_HANDLE_VALUE = -1
const ERROR_SUCCESS = 0
const ERROR_FILE_NOT_FOUND = 2
const ERROR_ACCESS_DENIED = 5
"#;

/// A scratch project with `kits/win/` holding the real `kernel32_mem.kdecl`, the
/// stand-ins, and a `lib.json` that gates the kit to Windows. The build runs
/// with this directory as the working directory, so `use win` resolves it as a
/// project kit exactly as it would beside a person's own program.
fn project(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kiln_win_kernel32_mem_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    let kit = dir.join("kits").join("win");
    std::fs::create_dir_all(&kit).expect("create the scratch kit directory");
    std::fs::copy(repo().join("kits/win/kernel32_mem.kdecl"), kit.join("kernel32_mem.kdecl"))
        .expect("copy kernel32_mem.kdecl into the scratch kit");
    std::fs::write(kit.join("siblings.kdecl"), SIBLING_STANDINS).unwrap();
    std::fs::write(
        kit.join("lib.json"),
        "{ \"display\": \"Windows API\", \"section\": \"System\", \"version\": \"0.1.0\", \
          \"platforms\": [\"windows\"] }\n",
    )
    .unwrap();
    dir
}

/// Build `source` for Windows inside `dir`; the error text on failure.
fn build_for_windows(dir: &Path, name: &str, source: &str) -> Result<PathBuf, String> {
    let src = dir.join(format!("{name}.kiln"));
    std::fs::write(&src, source).expect("write the program source");
    let out = dir.join(name);
    let done = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", src.to_str().unwrap(), "--os", "windows", "-o", out.to_str().unwrap()])
        .current_dir(dir)
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln build");
    if !done.status.success() {
        return Err(String::from_utf8_lossy(&done.stderr).into_owned());
    }
    let image = dir.join(format!("{name}.exe"));
    assert!(image.is_file(), "expected {} to be written", image.display());
    Ok(image)
}


/// The whole observable surface, run for real: allocate a page and ask Windows
/// what it says about it, walk a heap block, move bytes with the Win32
/// spellings, write and read a file, map it, enumerate it, and read an error
/// back. Every line prints `<label> ok` or `<label> FAILED`, so a regression
/// names itself.
const RUN: &str = r#"module kernel32_mem_run
use win

sub say(label: text, ok: bool)
  if ok
    call print_text(concat(label, " ok"))
  else
    call print_text(concat(label, " FAILED"))
  end
end

sub main
  # --- layouts: the numbers sizeof() gives in C -------------------------
  call print_int64(size of MEMORY_BASIC_INFORMATION)
  call print_int64(size of SYSTEM_INFO)
  call print_int64(size of MEMORYSTATUSEX)
  call print_int64(size of WIN32_FIND_DATAA)
  call print_int64(size of OVERLAPPED)
  call print_int64(size of BY_HANDLE_FILE_INFORMATION)
  call print_int64(size of FILETIME)

  # --- virtual memory ---------------------------------------------------
  var base: ptr = VirtualAlloc(ptr_null(), 4096, MEM_COMMIT_RESERVE, PAGE_READWRITE)
  call say("valloc", not ptr_is_null(base))
  call ptr_write_int(base, 0, 1234)

  var mbi: MEMORY_BASIC_INFORMATION
  let got: int64 = VirtualQuery(base, mbi, size of MEMORY_BASIC_INFORMATION)
  call say("vquery", got = size of MEMORY_BASIC_INFORMATION)
  call say("vq.state", mbi.State = MEM_COMMIT)
  call say("vq.protect", mbi.Protect = PAGE_READWRITE)
  call say("vq.type", mbi.Type = MEM_PRIVATE)
  call say("vq.region", mbi.RegionSize >= int_to_int64(4096))
  call say("vq.base", ptr_to_int(mbi.BaseAddress) = ptr_to_int(base))

  var oldcell: ptr = mem_alloc(4)
  call say("vprotect", VirtualProtect(base, 4096, PAGE_READONLY, oldcell))
  call say("vprotect.old", ptr_read_int(oldcell, 0) = PAGE_READWRITE)
  call say("vprotect.back", VirtualProtect(base, 4096, PAGE_READWRITE, oldcell))
  call mem_free(oldcell)

  call say("fic", FlushInstructionCache(GetCurrentProcess(), base, 4096))

  # --- read/write our own process through the process-memory API --------
  var out: ptr = mem_alloc(8)
  var cell: ptr = mem_alloc(4)
  call say("rpm", ReadProcessMemory(GetCurrentProcess(), base, cell, 4, out))
  call say("rpm.n", ptr_read_int64(out, 0) = int_to_int64(4))
  call say("rpm.value", ptr_read_int(cell, 0) = 1234)
  call ptr_write_int(cell, 0, 4321)
  call say("wpm", WriteProcessMemory(GetCurrentProcess(), base, cell, 4, out))
  call say("wpm.value", ptr_read_int(base, 0) = 4321)
  call mem_free(out)
  call mem_free(cell)

  call say("vfree", VirtualFree(base, 0, MEM_RELEASE))

  # --- heap -------------------------------------------------------------
  var heap: ptr = GetProcessHeap()
  call say("heap", not ptr_is_null(heap))
  var block: ptr = HeapAlloc(heap, HEAP_ZERO_MEMORY, 64)
  call say("halloc", not ptr_is_null(block))
  call say("halloc.zeroed", ptr_read_int(block, 0) = 0)
  call say("hsize", HeapSize(heap, 0, block) >= int_to_int64(64))
  call say("hfree", HeapFree(heap, 0, block))

  var priv: ptr = HeapCreate(0, 4096, 65536)
  call say("hcreate", not ptr_is_null(priv))
  var pb: ptr = HeapAlloc(priv, HEAP_ZERO_MEMORY, 32)
  call say("hcreate.alloc", not ptr_is_null(pb))
  call say("hdestroy", HeapDestroy(priv))

  # --- block moves ------------------------------------------------------
  var a: ptr = mem_alloc(8)
  var b: ptr = mem_alloc(8)
  call ptr_write_int64(a, 0, 7777)
  call CopyMemory(b, a, 8)
  call say("copymemory", ptr_read_int64(b, 0) = int_to_int64(7777))
  call ZeroMemory(b, 8)
  call say("zeromemory", ptr_read_int64(b, 0) = int_to_int64(0))
  call FillMemory(b, 8, 255)
  call say("fillmemory", ptr_read_byte(b, 0) = 255)
  call mem_free(a)
  call mem_free(b)

  # --- what the machine has ---------------------------------------------
  var si: SYSTEM_INFO
  call GetSystemInfo(si)
  call say("pagesize", si.dwPageSize = 4096)
  call say("granularity", si.dwAllocationGranularity = 65536)
  call say("cpus", si.dwNumberOfProcessors >= 1)
  call say("minaddr", not ptr_is_null(si.lpMaximumApplicationAddress))

  var ms: MEMORYSTATUSEX
  ms.dwLength = int64_to_int(size of MEMORYSTATUSEX)
  call say("memstatus", GlobalMemoryStatusEx(ms))
  call say("memstatus.phys", ms.ullTotalPhys > int_to_int64(0))

  # --- modules ----------------------------------------------------------
  var k32: ptr = LoadLibraryA("kernel32.dll")
  call say("loadlibrary", not ptr_is_null(k32))
  var fn: ptr = GetProcAddress(k32, "GetLastError")
  call say("getprocaddress", not ptr_is_null(fn))
  call say("getprocaddress.missing", ptr_is_null(GetProcAddress(k32, "NoSuchExportHere")))
  call say("freelibrary", FreeLibrary(k32))
  call say("getmodulehandle", not ptr_is_null(GetModuleHandleA("kernel32.dll")))
  var self: ptr = GetModuleHandleNull(ptr_null())
  call say("getmodulehandle.null", not ptr_is_null(self))

  var namebuf: ptr = mem_alloc(int_to_int64(MAX_PATH))
  let n: int = GetModuleFileNameA(self, namebuf, MAX_PATH)
  call say("getmodulefilename", n > 0)
  call say("getmodulefilename.text", length(ptr_read_text(namebuf)) = n)
  call mem_free(namebuf)

  # --- files ------------------------------------------------------------
  var h: ptr = CreateFileA("k32mem.bin", GENERIC_READ_WRITE, 0, ptr_null(), CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, ptr_null())
  call say("createfile", not (ptr_to_int(h) = int_to_int64(INVALID_HANDLE_VALUE)))

  var payload: ptr = mem_alloc(16)
  call ptr_write_text(payload, 0, "kernel32 memory")
  var wrote: ptr = mem_alloc(4)
  call say("writefile", WriteFile(h, payload, 16, wrote, ptr_null()))
  call say("writefile.n", ptr_read_int(wrote, 0) = 16)
  call say("flush", FlushFileBuffers(h))

  var sizecell: ptr = mem_alloc(8)
  call say("getfilesize", GetFileSizeEx(h, sizecell))
  call say("getfilesize.n", ptr_read_int64(sizecell, 0) = int_to_int64(16))

  call say("seek", SetFilePointerEx(h, 0, sizecell, FILE_BEGIN))
  call say("seek.pos", ptr_read_int64(sizecell, 0) = int_to_int64(0))

  var back: ptr = mem_alloc(16)
  var readn: ptr = mem_alloc(4)
  call say("readfile", ReadFile(h, back, 16, readn, ptr_null()))
  call say("readfile.n", ptr_read_int(readn, 0) = 16)
  call say("readfile.text", ptr_read_text(back) = "kernel32 memory")

  var info: BY_HANDLE_FILE_INFORMATION
  call say("fileinfo", GetFileInformationByHandle(h, info))
  call say("fileinfo.size", info.nFileSizeLow = 16)

  call say("closehandle", CloseHandle(h))
  call mem_free(payload)
  call mem_free(wrote)
  call mem_free(sizecell)
  call mem_free(back)
  call mem_free(readn)

  call say("attrs", not (GetFileAttributesA("k32mem.bin") = INVALID_FILE_ATTRIBUTES))
  call say("attrs.missing", GetFileAttributesA("no_such_k32.bin") = INVALID_FILE_ATTRIBUTES)

  # --- find -------------------------------------------------------------
  var fd: WIN32_FIND_DATAA
  var find: ptr = FindFirstFileA("k32mem.bin", fd)
  call say("findfirst", not (ptr_to_int(find) = int_to_int64(INVALID_HANDLE_VALUE)))
  call say("findfirst.name", ptr_read_text(address of fd.cFileName) = "k32mem.bin")
  call say("findfirst.size", fd.nFileSizeLow = 16)
  call say("findnext.end", not FindNextFileA(find, fd))
  call say("findnext.err", GetLastError() = ERROR_NO_MORE_FILES)
  call say("findclose", FindClose(find))

  # --- mapping ----------------------------------------------------------
  var mh: ptr = CreateFileA("k32mem.bin", GENERIC_READ, FILE_SHARE_READ, ptr_null(), OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, ptr_null())
  var mapping: ptr = CreateFileMappingA(mh, ptr_null(), PAGE_READONLY, 0, 0, ptr_null())
  call say("createmapping", not ptr_is_null(mapping))
  var view: ptr = MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, 0)
  call say("mapview", not ptr_is_null(view))
  call say("mapview.text", ptr_read_text(view) = "kernel32 memory")
  call say("unmapview", UnmapViewOfFile(view))
  call say("closemapping", CloseHandle(mapping))
  call say("closemapfile", CloseHandle(mh))

  call say("delete", DeleteFileA("k32mem.bin"))

  # --- errors -----------------------------------------------------------
  call SetLastError(ERROR_INVALID_PARAMETER)
  call say("lasterror", GetLastError() = ERROR_INVALID_PARAMETER)
  var msg: ptr = mem_alloc(512)
  let mlen: int = FormatMessageA(FORMAT_MESSAGE_FROM_SYSTEM + FORMAT_MESSAGE_IGNORE_INSERTS, ptr_null(), ERROR_FILE_NOT_FOUND, 0, msg, 512, ptr_null())
  call say("formatmessage", mlen > 0)
  call say("formatmessage.text", length(ptr_read_text(msg)) > 0)
  call mem_free(msg)

  # --- directories ------------------------------------------------------
  var cwd: ptr = mem_alloc(int_to_int64(MAX_PATH))
  call say("getcwd", GetCurrentDirectoryA(MAX_PATH, cwd) > 0)
  call say("mkdir", CreateDirectoryA("k32mem_dir", ptr_null()))
  call say("rmdir", RemoveDirectoryA("k32mem_dir"))
  call mem_free(cwd)

  call print_text("done")
end
"#;

/// Every declaration `RUN` cannot exercise from one console process. Never
/// executed; the point is that it compiles, which is what checks each
/// signature. Every `dll`, `record` field and `const` in the file is reached
/// by one program or the other.
const TOUCH: &str = r#"module kernel32_mem_touch
use win

# Every declaration in kernel32_mem.kdecl that the running test cannot exercise
# without a second process, a second machine or a GUI. A `dll` nobody calls
# proves nothing — loading is lazy and the signature is never looked at — so
# each one is called here, from a subroutine guarded by a condition that is
# never true. Building this program is what checks that every line parses,
# type-checks and lowers.
sub show(b: bool)
  if b
    call print_text("y")
  end
end

sub touch_all
  var proc: ptr = GetCurrentProcess()
  var p: ptr = ptr_null()
  var cell: ptr = mem_alloc(8)

  var remote: ptr = VirtualAllocEx(proc, p, 4096, MEM_COMMIT_RESERVE, PAGE_EXECUTE_READWRITE)
  call show(VirtualProtectEx(proc, remote, 4096, PAGE_EXECUTE_READ, cell))
  var mbi: MEMORY_BASIC_INFORMATION
  call print_int64(VirtualQueryEx(proc, remote, mbi, size of MEMORY_BASIC_INFORMATION))
  call show(VirtualFreeEx(proc, remote, 0, MEM_RELEASE))
  call show(VirtualLock(p, 4096))
  call show(VirtualUnlock(p, 4096))

  var heap: ptr = GetProcessHeap()
  var block: ptr = HeapAlloc(heap, 0, 16)
  call show(ptr_is_null(HeapReAlloc(heap, HEAP_ZERO_MEMORY, block, 32)))
  call show(HeapValidate(heap, 0, block))
  call print_int64(HeapCompact(heap, 0))

  call MoveMemory(cell, cell, 8)
  call RtlMoveMemory(cell, cell, 8)

  var si: SYSTEM_INFO
  call GetNativeSystemInfo(si)
  call print_int(si.wProcessorArchitecture)
  call print_int(si.wProcessorLevel)
  call print_int(si.wProcessorRevision)
  call print_int(si.wReserved)
  call print_int64(ptr_to_int(si.lpMinimumApplicationAddress))
  call print_int64(si.dwActiveProcessorMask)
  call print_int(si.dwProcessorType)

  var lib: ptr = LoadLibraryExA("kernel32.dll", ptr_null(), LOAD_LIBRARY_SEARCH_SYSTEM32)
  call show(GetModuleHandleExA(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, lib, cell))
  call show(DisableThreadLibraryCalls(lib))
  call show(FreeLibrary(lib))

  call show(SetFileAttributesA("a.bin", FILE_ATTRIBUTE_READONLY + FILE_ATTRIBUTE_HIDDEN))
  call show(MoveFileA("a.bin", "b.bin"))
  call show(CopyFileA("b.bin", "c.bin", true))
  call show(SetCurrentDirectoryA("."))
  call print_int(GetTempPathA(MAX_PATH, cell))
  call print_int(GetFullPathNameA("b.bin", MAX_PATH, cell, ptr_null()))

  var h: ptr = CreateFileA("b.bin", GENERIC_WRITE, FILE_SHARE_READ_WRITE + FILE_SHARE_DELETE, ptr_null(), OPEN_ALWAYS, FILE_FLAG_SEQUENTIAL_SCAN + FILE_FLAG_RANDOM_ACCESS, ptr_null())
  call show(SetEndOfFile(h))
  var mapping: ptr = CreateFileMappingA(h, ptr_null(), PAGE_READWRITE + SEC_COMMIT, 0, 4096, ptr_of_text("Local\\kiln_touch"))
  var opened: ptr = OpenFileMappingA(FILE_MAP_ALL_ACCESS, false, "Local\\kiln_touch")
  var view: ptr = MapViewOfFile(opened, FILE_MAP_WRITE + FILE_MAP_COPY + FILE_MAP_EXECUTE, 0, 0, 4096)
  call show(FlushViewOfFile(view, 4096))
  call show(UnmapViewOfFile(view))

  # An OVERLAPPED handed to the asynchronous read path, and a
  # SECURITY_ATTRIBUTES that makes a handle inheritable: both are `ptr`
  # parameters, so they cross as `address of`.
  var ov: OVERLAPPED
  ov.Offset = 4096
  ov.OffsetHigh = 0
  ov.hEvent = ptr_null()
  call show(ReadFile(h, cell, 8, ptr_null(), address of ov))
  call print_int64(ov.Internal)
  call print_int64(ov.InternalHigh)

  var sa: SECURITY_ATTRIBUTES
  sa.nLength = int64_to_int(size of SECURITY_ATTRIBUTES)
  sa.bInheritHandle = true
  sa.lpSecurityDescriptor = ptr_null()
  call show(CreateDirectoryA("d", address of sa))

  var fd: WIN32_FIND_DATAA
  call print_int(fd.cAlternateFileName[1])
  call print_int(fd.dwReserved0)
  call print_int(fd.dwReserved1)
  call print_int(fd.ftLastWriteTime.dwHighDateTime)
  call print_int(fd.ftLastAccessTime.dwLowDateTime)
  call print_int(fd.ftCreationTime.dwLowDateTime)
  call print_int(fd.nFileSizeHigh)
  call print_int(fd.dwFileAttributes)

  var bh: BY_HANDLE_FILE_INFORMATION
  call print_int(bh.dwVolumeSerialNumber + bh.nNumberOfLinks + bh.nFileIndexHigh + bh.nFileIndexLow + bh.nFileSizeHigh)
  call print_int(bh.ftCreationTime.dwHighDateTime + bh.ftLastAccessTime.dwHighDateTime + bh.ftLastWriteTime.dwHighDateTime)

  var mem: MEMORYSTATUSEX
  call print_int64(mem.ullTotalPageFile + mem.ullAvailPageFile + mem.ullTotalVirtual + mem.ullAvailVirtual + mem.ullAvailExtendedVirtual)
  call print_int(mem.dwMemoryLoad)

  call print_int(mbi.AllocationProtect + mbi.PartitionId + mbi.Alignment1 + mbi.Alignment2)
  call print_int64(ptr_to_int(mbi.AllocationBase))

  call show(CloseHandle(mapping))
  call show(CloseHandle(opened))
  call show(CloseHandle(h))
  call show(DeleteFileA("c.bin"))
  call show(RemoveDirectoryA("d"))
  call mem_free(cell)
end

# Every constant this file ships, so a typo in one is a build error rather
# than a surprise at the call that uses it.
sub touch_consts
  call print_int(MEM_COMMIT + MEM_RESERVE + MEM_COMMIT_RESERVE + MEM_DECOMMIT + MEM_RELEASE + MEM_FREE)
  call print_int(MEM_PRIVATE + MEM_MAPPED + MEM_RESET + MEM_TOP_DOWN + MEM_IMAGE)
  call print_int(PAGE_NOACCESS + PAGE_READONLY + PAGE_READWRITE + PAGE_WRITECOPY + PAGE_EXECUTE)
  call print_int(PAGE_EXECUTE_READ + PAGE_EXECUTE_READWRITE + PAGE_EXECUTE_WRITECOPY + PAGE_GUARD + PAGE_NOCACHE + PAGE_WRITECOMBINE)
  call print_int(HEAP_NO_SERIALIZE + HEAP_GENERATE_EXCEPTIONS + HEAP_ZERO_MEMORY + HEAP_REALLOC_IN_PLACE_ONLY + HEAP_CREATE_ENABLE_EXECUTE)
  call print_int(GENERIC_READ_WRITE - GENERIC_READ - GENERIC_WRITE + GENERIC_EXECUTE + GENERIC_ALL)
  call print_int(FILE_SHARE_READ + FILE_SHARE_WRITE + FILE_SHARE_DELETE + FILE_SHARE_READ_WRITE)
  call print_int(CREATE_NEW + CREATE_ALWAYS + OPEN_EXISTING + OPEN_ALWAYS + TRUNCATE_EXISTING)
  call print_int(FILE_ATTRIBUTE_READONLY + FILE_ATTRIBUTE_HIDDEN + FILE_ATTRIBUTE_SYSTEM + FILE_ATTRIBUTE_DIRECTORY + FILE_ATTRIBUTE_ARCHIVE + FILE_ATTRIBUTE_NORMAL + FILE_ATTRIBUTE_TEMPORARY)
  call print_int(INVALID_FILE_ATTRIBUTES + INVALID_FILE_SIZE + INVALID_SET_FILE_POINTER)
  call print_int(FILE_FLAG_DELETE_ON_CLOSE + FILE_FLAG_SEQUENTIAL_SCAN + FILE_FLAG_RANDOM_ACCESS + FILE_FLAG_NO_BUFFERING)
  call print_int(FILE_FLAG_OVERLAPPED)
  call print_int(FILE_FLAG_WRITE_THROUGH)
  call print_int(FILE_BEGIN + FILE_CURRENT + FILE_END)
  call print_int(FILE_MAP_COPY + FILE_MAP_WRITE + FILE_MAP_READ + FILE_MAP_EXECUTE + FILE_MAP_ALL_ACCESS)
  call print_int(SEC_COMMIT + SEC_IMAGE + SEC_RESERVE)
  call print_int(DONT_RESOLVE_DLL_REFERENCES + LOAD_LIBRARY_AS_DATAFILE + LOAD_WITH_ALTERED_SEARCH_PATH + LOAD_LIBRARY_SEARCH_SYSTEM32)
  call print_int(GET_MODULE_HANDLE_EX_FLAG_PIN + GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT + GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS)
  call print_int(FORMAT_MESSAGE_ALLOCATE_BUFFER + FORMAT_MESSAGE_IGNORE_INSERTS + FORMAT_MESSAGE_FROM_STRING + FORMAT_MESSAGE_FROM_HMODULE + FORMAT_MESSAGE_FROM_SYSTEM + FORMAT_MESSAGE_MAX_WIDTH_MASK)
  call print_int(PROCESSOR_ARCHITECTURE_INTEL + PROCESSOR_ARCHITECTURE_ARM + PROCESSOR_ARCHITECTURE_AMD64 + PROCESSOR_ARCHITECTURE_ARM64 + PROCESSOR_ARCHITECTURE_UNKNOWN)
  call print_int(MAX_PATH)
  call print_int(ERROR_PATH_NOT_FOUND + ERROR_INVALID_HANDLE + ERROR_NOT_ENOUGH_MEMORY + ERROR_NO_MORE_FILES + ERROR_INVALID_PARAMETER + ERROR_INSUFFICIENT_BUFFER + ERROR_ALREADY_EXISTS + ERROR_MOD_NOT_FOUND + ERROR_PROC_NOT_FOUND)
end

sub main
  # Never true — SetLastError(0) is the line before, so this is 0 = -1.
  call SetLastError(0)
  if GetLastError() = -1
    call touch_all()
    call touch_consts()
  end
  call print_text("touched")
end
"#;

/// The struct sizes `RUN` prints first, in order — the numbers a C compiler
/// gives for `MEMORY_BASIC_INFORMATION`, `SYSTEM_INFO`, `MEMORYSTATUSEX`,
/// `WIN32_FIND_DATAA`, `OVERLAPPED`, `BY_HANDLE_FILE_INFORMATION` and
/// `FILETIME` on x64. A field declared at the wrong width moves one of these.
const SIZES: &[&str] = &["48", "48", "64", "320", "32", "52", "8"];

/// Checks worth naming: if the list of `ok` lines ever silently shortens,
/// these say what went missing.
const MUST_PASS: &[&str] = &[
    "valloc ok",
    "vquery ok",
    "vq.state ok",
    "vq.protect ok",
    "vq.type ok",
    "vq.base ok",
    "vprotect.old ok",
    "rpm.value ok",
    "wpm.value ok",
    "vfree ok",
    "halloc.zeroed ok",
    "hdestroy ok",
    "copymemory ok",
    "zeromemory ok",
    "fillmemory ok",
    "pagesize ok",
    "granularity ok",
    "memstatus.phys ok",
    "loadlibrary ok",
    "getprocaddress ok",
    "getprocaddress.missing ok",
    "getmodulehandle.null ok",
    "getmodulefilename.text ok",
    "createfile ok",
    "writefile.n ok",
    "getfilesize.n ok",
    "seek.pos ok",
    "readfile.text ok",
    "fileinfo.size ok",
    "attrs.missing ok",
    "findfirst.name ok",
    "findfirst.size ok",
    "findnext.err ok",
    "mapview.text ok",
    "delete ok",
    "lasterror ok",
    "formatmessage.text ok",
];

/// The kit's memory, module and file surface, exercised under wine. Skips
/// itself with a line saying why when mingw or wine is missing.
#[test]
fn the_running_surface_behaves_on_windows() {
    if !mingw_present() {
        return;
    }
    let dir = project("run");
    let image = build_for_windows(&dir, "run", RUN).expect("the running program should build");

    if !on_path("wine") {
        eprintln!("wine is not installed; the Windows image was built but not run");
        return;
    }
    let out = Command::new("wine")
        .arg(&image)
        .current_dir(&dir)
        .env("WINEDEBUG", "-all")
        .output()
        .expect("run wine");
    let text = String::from_utf8_lossy(&out.stdout).replace('\r', "");
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        out.status.success(),
        "the program exited non-zero under wine:\n{}\n{}",
        text,
        String::from_utf8_lossy(&out.stderr)
    );

    // A wrong field width shows up here before it shows up anywhere else.
    assert_eq!(
        &lines[..SIZES.len().min(lines.len())],
        SIZES,
        "the struct sizes are not the ones C gives:\n{text}"
    );

    let failed: Vec<&&str> = lines.iter().filter(|l| l.ends_with("FAILED")).collect();
    assert!(failed.is_empty(), "checks failed under wine: {failed:?}\n{text}");

    for want in MUST_PASS {
        assert!(lines.contains(want), "the check {want:?} did not run:\n{text}");
    }
    assert_eq!(lines.last(), Some(&"done"), "the program stopped early:\n{text}");

    // The program is a constant, so the number of checks it makes is one too:
    // a check that quietly stops being printed is a regression like any other.
    let oks = lines.iter().filter(|l| l.ends_with(" ok")).count();
    assert_eq!(oks, 77, "the program made {oks} checks, not the 77 it writes:\n{text}");
}

/// Every declaration the running program cannot reach still has to be a
/// declaration the compiler accepts: the *Ex forms that need a second process,
/// the private heap calls, the named mapping, the asynchronous read. Building
/// this is the proof — each is called, so each signature is checked and
/// lowered.
#[test]
fn every_declaration_builds_for_windows() {
    if !mingw_present() {
        return;
    }
    let dir = project("touch");
    build_for_windows(&dir, "touch", TOUCH).expect("every declaration should build");
}

/// `kiln commands --use win` reports the bundle, so Studio's completion,
/// the language server and the generated reference see it. The listing works
/// on Linux even though the kit itself is Windows-only.
#[test]
fn commands_lists_the_bundle() {
    let dir = project("commands");
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["commands", "--use", "win"])
        .current_dir(&dir)
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln commands");
    assert!(
        out.status.success(),
        "commands --use win failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    // A `dll` line carries ` from <library>` and a `crecord` line its fields,
    // so a listing is matched by its head.
    let has = |needle: &str| text.lines().any(|l| l.trim().starts_with(needle));

    for want in [
        "dll: VirtualAlloc(ptr, int64, int, int) -> ptr",
        "dll: VirtualProtect(ptr, int64, int, ptr) -> bool",
        "dll: VirtualQuery(ptr, MEMORY_BASIC_INFORMATION, int64) -> int64",
        "dll: ReadProcessMemory(ptr, ptr, ptr, int64, ptr) -> bool",
        "dll: HeapAlloc(ptr, int, int64) -> ptr",
        "dll: GetProcessHeap() -> ptr",
        "dll: CopyMemory(ptr, ptr, int64)",
        "dll: FlushInstructionCache(ptr, ptr, int64) -> bool",
        "dll: LoadLibraryA(text) -> ptr",
        "dll: GetProcAddress(ptr, text) -> ptr",
        "dll: GetModuleFileNameA(ptr, ptr, int) -> int",
        "dll: CreateFileA(text, int, int, ptr, int, int, ptr) -> ptr",
        "dll: ReadFile(ptr, ptr, int, ptr, ptr) -> bool",
        "dll: SetFilePointerEx(ptr, int64, ptr, int) -> bool",
        "dll: CreateFileMappingA(ptr, ptr, int, int, int, ptr) -> ptr",
        "dll: MapViewOfFile(ptr, int, int, int, int64) -> ptr",
        "dll: FindFirstFileA(text, WIN32_FIND_DATAA) -> ptr",
        "dll: FindNextFileA(ptr, WIN32_FIND_DATAA) -> bool",
        "dll: GetLastError() -> int",
        "dll: SetLastError(int)",
        "dll: FormatMessageA(int, ptr, int, int, ptr, int, ptr) -> int",
        // In full, because the field order and the field widths are the
        // layout: this is the listing Studio and the reference show.
        "crecord: MEMORY_BASIC_INFORMATION BaseAddress: ptr, AllocationBase: ptr, \
AllocationProtect: int, PartitionId: int16, Alignment1: int16, RegionSize: int64, \
State: int, Protect: int, Type: int, Alignment2: int",
        "crecord: WIN32_FIND_DATAA dwFileAttributes: int, ftCreationTime: FILETIME, \
ftLastAccessTime: FILETIME, ftLastWriteTime: FILETIME, nFileSizeHigh: int, \
nFileSizeLow: int, dwReserved0: int, dwReserved1: int, cFileName: byte[260], \
cAlternateFileName: byte[14]",
        "crecord: SYSTEM_INFO",
        "crecord: MEMORYSTATUSEX",
        "crecord: OVERLAPPED",
        "crecord: BY_HANDLE_FILE_INFORMATION",
        "crecord: FILETIME",
        "const: MEM_COMMIT int",
        "const: MEM_RELEASE int",
        "const: PAGE_EXECUTE_READWRITE int",
        "const: GENERIC_READ int",
        "const: FILE_SHARE_READ int",
        "const: OPEN_EXISTING int",
        "const: FILE_ATTRIBUTE_NORMAL int",
        "const: HEAP_ZERO_MEMORY int",
        "const: FILE_MAP_READ int",
        "const: FORMAT_MESSAGE_FROM_SYSTEM int",
        "const: INVALID_FILE_SIZE int",
        "const: MAX_PATH int",
    ] {
        assert!(has(want), "`{want}` is not in `commands --use win`:\n{text}");
    }
}

/// The same seven `sizeof`s, asked of `windows.h` itself.
///
/// The running program prints what Kiln computes from the `.kdecl`; this
/// prints what mingw's headers say. Two independent sources agreeing is what
/// makes the layouts a fact rather than a transcription that looked right.
const SIZEOF_C: &str = r#"#include <windows.h>
#include <stdio.h>
#include <stddef.h>
int main(void) {
  printf("%zu\n%zu\n%zu\n%zu\n%zu\n%zu\n%zu\n",
         sizeof(MEMORY_BASIC_INFORMATION), sizeof(SYSTEM_INFO),
         sizeof(MEMORYSTATUSEX), sizeof(WIN32_FIND_DATAA), sizeof(OVERLAPPED),
         sizeof(BY_HANDLE_FILE_INFORMATION), sizeof(FILETIME));
  /* The offsets the running program proves behaviourally, stated outright. */
  printf("%zu %zu %zu %zu %zu %zu %zu %zu\n",
         offsetof(MEMORY_BASIC_INFORMATION, RegionSize),
         offsetof(MEMORY_BASIC_INFORMATION, State),
         offsetof(MEMORY_BASIC_INFORMATION, Protect),
         offsetof(MEMORY_BASIC_INFORMATION, Type),
         offsetof(SYSTEM_INFO, dwActiveProcessorMask),
         offsetof(SYSTEM_INFO, wProcessorLevel),
         offsetof(WIN32_FIND_DATAA, cFileName),
         offsetof(WIN32_FIND_DATAA, cAlternateFileName));
  return 0;
}
"#;

#[test]
fn the_layouts_are_the_ones_windows_h_gives() {
    if !mingw_present() {
        return;
    }
    if !on_path("wine") {
        eprintln!("wine is not installed; skipping the windows.h layout comparison");
        return;
    }
    let dir = project("sizeof");
    let src = dir.join("sizeof.c");
    std::fs::write(&src, SIZEOF_C).unwrap();
    let exe = dir.join("sizeof.exe");
    let built = Command::new("x86_64-w64-mingw32-gcc")
        .arg("-o")
        .arg(&exe)
        .arg(&src)
        .output()
        .expect("run the mingw compiler");
    assert!(
        built.status.success(),
        "mingw could not build the sizeof probe:\n{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let out = Command::new("wine")
        .arg(&exe)
        .env("WINEDEBUG", "-all")
        .output()
        .expect("run the sizeof probe under wine");
    let text = String::from_utf8_lossy(&out.stdout).replace('\r', "");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        &lines[..SIZES.len().min(lines.len())],
        SIZES,
        "windows.h does not agree with the sizes the kit's records give:\n{text}"
    );
    assert_eq!(
        lines.get(SIZES.len()).copied(),
        Some("24 32 36 40 24 44 44 304"),
        "windows.h puts these members at other offsets than the records do:\n{text}"
    );
}

/// The names this file deliberately does not declare, and the sibling that
/// owns each. `kernel32_mem.kdecl` uses all of them, so if a sibling renames or
/// drops one the merged kit stops building — and the hermetic tests above,
/// which supply their own stand-ins, would not notice. This is the check that
/// does, and it reads the sibling files as text so it holds even while another
/// file in the kit is mid-edit and does not parse.
const BORROWED: &[(&str, &str)] = &[
    ("CloseHandle", "kernel32_proc.kdecl"),
    ("GetCurrentProcess", "kernel32_proc.kdecl"),
    ("SECURITY_ATTRIBUTES", "kernel32_proc.kdecl"),
    ("INVALID_HANDLE_VALUE", "kernel32_proc.kdecl"),
    ("ERROR_SUCCESS", "advapi32.kdecl"),
    ("ERROR_FILE_NOT_FOUND", "advapi32.kdecl"),
    ("ERROR_ACCESS_DENIED", "advapi32.kdecl"),
];

#[test]
fn the_borrowed_names_are_still_declared_by_a_sibling() {
    let kit = repo().join("kits/win");
    let mut declared: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(&kit).expect("read kits/win") {
        let path = entry.expect("a directory entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("kdecl") {
            continue;
        }
        if path.file_name().and_then(|f| f.to_str()) == Some("kernel32_mem.kdecl") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("read a sibling .kdecl");
        for line in text.lines() {
            for lead in ["dll ", "record ", "const "] {
                if let Some(rest) = line.strip_prefix(lead) {
                    let name: String = rest
                        .chars()
                        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                        .collect();
                    declared.push(name);
                }
            }
        }
    }
    for (name, owner) in BORROWED {
        assert!(
            declared.iter().any(|d| d == name),
            "`{name}` is used by kernel32_mem.kdecl and was expected from {owner}, but no \
             sibling .kdecl in kits/win declares it any more — either that sibling moved it \
             or kernel32_mem.kdecl must declare it itself"
        );
    }

    // And the other half of the bargain: this file must not declare them, or
    // the merged kit is a name-collision error.
    let mine = std::fs::read_to_string(kit.join("kernel32_mem.kdecl")).expect("read the file");
    for (name, _) in BORROWED {
        for lead in ["dll ", "record ", "const "] {
            assert!(
                !mine.lines().any(|l| l.starts_with(&format!("{lead}{name}"))),
                "kernel32_mem.kdecl declares `{name}`, which a sibling owns — one name, one file"
            );
        }
    }
}

/// A Windows-only kit must not build for Linux, and the message must say so
/// rather than let the program reach a linker that cannot find kernel32.
#[test]
fn the_kit_is_refused_on_linux() {
    let dir = project("gate");
    let src = dir.join("app.kiln");
    std::fs::write(
        &src,
        "module app\nuse win\nsub main\n  call print_int(MEM_COMMIT)\nend\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", src.to_str().unwrap(), "--os", "linux", "-o", dir.join("app").to_str().unwrap()])
        .current_dir(&dir)
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln build");
    assert!(!out.status.success(), "a windows-only kit must not build for linux");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("kit `win`") && err.contains("--os windows"),
        "the error must name the kit and the OS it needs, got:\n{err}"
    );
}
