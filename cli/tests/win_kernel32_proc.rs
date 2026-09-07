//! The `win` kit's kernel32 process/thread/synchronisation half:
//! `kits/win/kernel32_proc.kdecl`.
//!
//! Every assertion here is made against the real Windows API. The programs are
//! cross-built for Windows through mingw's linker and run under wine, so the
//! numbers they print — a `size of` for each structure, a wait result, a
//! thread's exit code, the name tool-help reports for the running image — come
//! back from Windows itself rather than from a table written by hand.
//!
//! The kit under test is copied into a scratch directory of its own as
//! `winproc`, and the programs say `use winproc`. That is deliberate: the `win`
//! kit is written by several hands at once, and a fault in another subsystem's
//! `.kdecl` must not be able to fail this file's proof of its own. The last test
//! checks the merged `win` kit as it actually stands.

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

/// A scratch project per test, carrying a private copy of the declaration file
/// as the one-file kit `winproc`.
fn project(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kiln_win_kernel32_proc_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    let kit = dir.join("kits").join("winproc");
    std::fs::create_dir_all(&kit).expect("create the scratch kit directory");
    std::fs::copy(repo().join("kits/win/kernel32_proc.kdecl"), kit.join("winproc.kdecl"))
        .expect("copy kernel32_proc.kdecl into the scratch kit");
    std::fs::write(
        kit.join("lib.json"),
        "{ \"display\": \"Win kernel32 process\", \"section\": \"System\", \
          \"version\": \"0.1.0\", \"platforms\": [\"windows\"] }\n",
    )
    .expect("write the scratch kit's lib.json");
    dir
}

/// Build `src` in `dir` for Windows. The working directory is the scratch
/// project, so `use winproc` resolves against the copy beside it.
fn build_for_windows(dir: &Path, src: &str, out: &str) -> PathBuf {
    let srcpath = dir.join(format!("{out}.kiln"));
    std::fs::write(&srcpath, src).expect("write the program source");
    let exe = dir.join(format!("{out}.exe"));
    let output = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", srcpath.to_str().unwrap(), "--os", "windows", "-o", exe.to_str().unwrap()])
        .current_dir(dir)
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln build");
    assert!(
        output.status.success(),
        "cross build failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_pe32_plus(&exe);
    exe
}

/// The image's own headers: `MZ`, `e_lfanew`, `PE\0\0`, x86-64, and the
/// optional-header magic that means PE32+.
fn assert_pe32_plus(image: &Path) {
    let bytes = std::fs::read(image).expect("read the built image");
    assert!(bytes.len() > 0x40, "image is too short to be a PE file");
    assert_eq!(&bytes[0..2], b"MZ", "no DOS stub signature");
    let pe = u32::from_le_bytes([bytes[0x3C], bytes[0x3D], bytes[0x3E], bytes[0x3F]]) as usize;
    assert!(pe + 26 <= bytes.len(), "e_lfanew points outside the file");
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

/// Run the image under wine. CRLF is what a Windows console program writes.
fn run_under_wine(exe: &Path, dir: &Path) -> Vec<String> {
    let out = Command::new("wine")
        .arg(exe)
        .current_dir(dir)
        .env("WINEDEBUG", "-all")
        .output()
        .expect("run under wine");
    assert!(
        out.status.success(),
        "the program exited non-zero under wine:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim_end_matches('\r').to_string())
        .collect()
}

/// Structures, identity, an event, a mutex, a semaphore, a critical section, a
/// thread, a handle on our own process, and all three tool-help snapshots.
const SURFACE: &str = r#"
module winproc
use winproc

# The body CreateThread runs. It touches nothing but its own arithmetic: what
# is under test is that Windows called a Kiln sub and kept its result as
# the thread's exit code.
sub worker(parameter: ptr): int system
  return 4242
end

sub main
  call print_int64(size of STARTUPINFOA)
  call print_int64(size of PROCESS_INFORMATION)
  call print_int64(size of PROCESSENTRY32)
  call print_int64(size of THREADENTRY32)
  call print_int64(size of MODULEENTRY32)
  call print_int64(size of CRITICAL_SECTION)
  call print_int64(size of SECURITY_ATTRIBUTES)

  var pid: int = GetCurrentProcessId()
  if pid > 0
    call print_text("pid-ok")
  end
  if GetCurrentThreadId() > 0
    call print_text("tid-ok")
  end
  if ptr_to_int(GetCurrentProcess()) = int_to_int64(INVALID_HANDLE_VALUE)
    call print_text("self-handle-is-pseudo")
  end
  if length(GetCommandLineA()) > 0
    call print_text("cmdline-ok")
  end

  # An event driven through its three states: a zero-length wait reports
  # WAIT_TIMEOUT, then WAIT_OBJECT_0, then WAIT_TIMEOUT again.
  var ev: ptr = CreateEventA(ptr_null(), true, false, ptr_null())
  call print_int(WaitForSingleObject(ev, 0))
  var okset: bool = SetEvent(ev)
  call print_int(WaitForSingleObject(ev, 0))
  var okreset: bool = ResetEvent(ev)
  call print_int(WaitForSingleObject(ev, 0))
  if okset and okreset and CloseHandle(ev)
    call print_text("event-ok")
  end

  # A mutex taken at creation and given back.
  var mx: ptr = CreateMutexA(ptr_null(), true, ptr_null())
  if ReleaseMutex(mx) and CloseHandle(mx)
    call print_text("mutex-ok")
  end

  # A semaphore of one: the second wait finds it empty, and the release
  # reports a previous count of zero through its out-parameter.
  var sem: ptr = CreateSemaphoreA(ptr_null(), 1, 1, ptr_null())
  call print_int(WaitForSingleObject(sem, 0))
  call print_int(WaitForSingleObject(sem, 0))
  var prev: ptr = mem_alloc(4)
  var okrel: bool = ReleaseSemaphore(sem, 1, prev)
  call print_int(ptr_read_int(prev, 0))
  call mem_free(prev)
  if okrel and CloseHandle(sem)
    call print_text("semaphore-ok")
  end

  # A critical section entered twice, which only works if the record has the
  # real 40 bytes and the alignment a pointer field forces.
  var cs: CRITICAL_SECTION
  call InitializeCriticalSection(cs)
  call EnterCriticalSection(cs)
  call EnterCriticalSection(cs)
  call LeaveCriticalSection(cs)
  call LeaveCriticalSection(cs)
  if TryEnterCriticalSection(cs)
    call LeaveCriticalSection(cs)
    call print_text("critsec-ok")
  end
  call DeleteCriticalSection(cs)

  # A real Windows thread running Kiln code, waited on, its exit code read
  # back out of a four-byte buffer.
  var tid: ptr = mem_alloc(4)
  var th: ptr = CreateThread(ptr_null(), 0, address of worker, ptr_null(), 0, tid)
  call print_int(WaitForSingleObject(th, INFINITE))
  var code: ptr = mem_alloc(4)
  var okcode: bool = GetExitCodeThread(th, code)
  call print_int(ptr_read_int(code, 0))
  if ptr_read_int(tid, 0) > 0
    call print_text("threadid-ok")
  end
  call mem_free(code)
  call mem_free(tid)
  if okcode and CloseHandle(th)
    call print_text("thread-ok")
  end

  # A handle on our own process: still running, and it knows its own id.
  var me: ptr = OpenProcess(PROCESS_QUERY_INFORMATION + SYNCHRONIZE, false, pid)
  if ptr_is_null(me)
    call print_text("openprocess-failed")
  else
    var pcode: ptr = mem_alloc(4)
    var okp: bool = GetExitCodeProcess(me, pcode)
    call print_int(ptr_read_int(pcode, 0))
    call mem_free(pcode)
    if GetProcessId(me) = pid
      call print_text("processid-matches")
    end
    if okp and CloseHandle(me)
      call print_text("openprocess-ok")
    end
  end

  # The process snapshot, walked until it names us. The name comes out of the
  # 260-byte inline array, so the struct's padding has to be right for the
  # entry to line up at all.
  var snap: ptr = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
  if ptr_to_int(snap) = int_to_int64(INVALID_HANDLE_VALUE)
    call print_text("snapshot-failed")
  else
    var e: PROCESSENTRY32
    call mem_zero(address of e, size of PROCESSENTRY32)
    e.dwSize = SIZEOF_PROCESSENTRY32
    var found: bool = false
    var more: bool = Process32First(snap, e)
    while more
      if e.th32ProcessID = pid
        found = true
        call print_text(ptr_read_text(address of e.szExeFile))
        more = false
      else
        more = Process32Next(snap, e)
      end
    end
    if found
      call print_text("process32-ok")
    else
      call print_text("process32-missed-self")
    end
    var oks: bool = CloseHandle(snap)
  end

  # The thread snapshot: we own at least one thread.
  var tsnap: ptr = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0)
  var te: THREADENTRY32
  call mem_zero(address of te, size of THREADENTRY32)
  te.dwSize = SIZEOF_THREADENTRY32
  var mine: int = 0
  var tmore: bool = Thread32First(tsnap, te)
  while tmore
    if te.th32OwnerProcessID = pid
      mine = mine + 1
    end
    tmore = Thread32Next(tsnap, te)
  end
  if mine > 0
    call print_text("thread32-ok")
  else
    call print_text("thread32-missed-self")
  end
  var okts: bool = CloseHandle(tsnap)

  # The module snapshot: a process's first module is its own image.
  var msnap: ptr = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE, pid)
  var me32: MODULEENTRY32
  call mem_zero(address of me32, size of MODULEENTRY32)
  me32.dwSize = SIZEOF_MODULEENTRY32
  if Module32First(msnap, me32)
    call print_text(ptr_read_text(address of me32.szModule))
    if ptr_is_null(me32.modBaseAddr)
      call print_text("module-base-null")
    else
      call print_text("module32-ok")
    end
  else
    call print_text("module32-failed")
  end
  var okms: bool = CloseHandle(msnap)

  # Sleep, measured on the tick counter.
  var t0: int = GetTickCount()
  call Sleep(40)
  if GetTickCount() - t0 >= 20
    call print_text("sleep-ok")
  end
end
"#;

/// Every line the program above must print. The first seven are `size of` for
/// each record; they are the numbers `x86_64-w64-mingw32-gcc` reports for the
/// same structures in `<windows.h>` and `<tlhelp32.h>`, so a transcription
/// slip anywhere in a field list moves one of them.
const SURFACE_EXPECTED: &[&str] = &[
    "104",                   // sizeof(STARTUPINFOA)
    "24",                    // sizeof(PROCESS_INFORMATION)
    "304",                   // sizeof(PROCESSENTRY32)
    "28",                    // sizeof(THREADENTRY32)
    "568",                   // sizeof(MODULEENTRY32)
    "40",                    // sizeof(CRITICAL_SECTION)
    "24",                    // sizeof(SECURITY_ATTRIBUTES)
    "pid-ok",
    "tid-ok",
    "self-handle-is-pseudo", // GetCurrentProcess() is (HANDLE)-1
    "cmdline-ok",
    "258",                   // WAIT_TIMEOUT: the event is unsignalled
    "0",                     // WAIT_OBJECT_0 after SetEvent
    "258",                   // WAIT_TIMEOUT again after ResetEvent
    "event-ok",
    "mutex-ok",
    "0",                     // the semaphore's one count is taken
    "258",                   // ...and the next wait finds none
    "0",                     // ReleaseSemaphore reports a previous count of 0
    "semaphore-ok",
    "critsec-ok",
    "0",                     // the thread finished
    "4242",                  // ...and Windows kept the sub's return as its exit code
    "threadid-ok",
    "thread-ok",
    "259",                   // STILL_ACTIVE: we are the running process
    "processid-matches",
    "openprocess-ok",
    "surface.exe",           // szExeFile out of the tool-help entry
    "process32-ok",
    "thread32-ok",
    "surface.exe",           // szModule out of the module entry
    "module32-ok",
    "sleep-ok",
];

/// The whole subsystem, exercised against Windows under wine.
#[test]
fn kernel32_process_surface_runs_on_windows() {
    if !on_path("x86_64-w64-mingw32-gcc") {
        eprintln!("x86_64-w64-mingw32-gcc is not installed; skipping");
        return;
    }
    let dir = project("surface");
    let exe = build_for_windows(&dir, SURFACE, "surface");
    if !on_path("wine") {
        eprintln!("wine is not installed; built the PE but did not run it");
        return;
    }
    assert_eq!(
        run_under_wine(&exe, &dir),
        SURFACE_EXPECTED,
        "the kernel32 process surface behaved differently under wine"
    );
}

/// CreateProcessA end to end: a zeroed STARTUPINFOA with its `cb` set, a
/// PROCESS_INFORMATION filled by Windows, a wait on the child and its exit
/// code read back. Its own test, so a wine `cmd.exe` that misbehaves cannot
/// take the rest of the subsystem down with it.
const SPAWN: &str = r#"
module winspawn
use winproc

sub main
  var si: STARTUPINFOA
  call mem_zero(address of si, size of STARTUPINFOA)
  si.cb = SIZEOF_STARTUPINFOA
  var pi: PROCESS_INFORMATION
  call mem_zero(address of pi, size of PROCESS_INFORMATION)
  var started: bool = CreateProcessA(ptr_null(), "cmd.exe /c exit 7", ptr_null(), ptr_null(), false, CREATE_NO_WINDOW, ptr_null(), ptr_null(), si, pi)
  if started
    call print_int(WaitForSingleObject(pi.hProcess, INFINITE))
    var code: ptr = mem_alloc(4)
    var okc: bool = GetExitCodeProcess(pi.hProcess, code)
    call print_int(ptr_read_int(code, 0))
    call mem_free(code)
    if pi.dwProcessId > 0
      call print_text("child-pid-ok")
    end
    if okc and CloseHandle(pi.hThread) and CloseHandle(pi.hProcess)
      call print_text("createprocess-ok")
    end
  else
    call print_text("createprocess-failed")
  end
end
"#;

#[test]
fn create_process_starts_a_child_and_reads_its_exit_code() {
    if !on_path("x86_64-w64-mingw32-gcc") {
        eprintln!("x86_64-w64-mingw32-gcc is not installed; skipping");
        return;
    }
    let dir = project("spawn");
    let exe = build_for_windows(&dir, SPAWN, "spawn");
    if !on_path("wine") {
        eprintln!("wine is not installed; built the PE but did not run it");
        return;
    }
    assert_eq!(
        run_under_wine(&exe, &dir),
        &[
            "0",                  // WAIT_OBJECT_0: the child ended
            "7",                  // ...with the exit code it was told to
            "child-pid-ok",
            "createprocess-ok",
        ],
        "CreateProcessA did not start and report a child correctly"
    );
}

/// The names a program writes are the names the toolchain reports, so Studio's
/// completion and the generated reference see the whole subsystem.
fn assert_lists_the_subsystem(text: &str, what: &str) {
    let has = |needle: &str| text.lines().any(|l| l.contains(needle));
    for dll in [
        "dll: CreateProcessA(",
        "dll: OpenProcess(int, bool, int) -> ptr",
        "dll: GetCurrentProcessId() -> int",
        "dll: TerminateProcess(ptr, int) -> bool",
        "dll: CreateThread(ptr, int64, ptr, ptr, int, ptr) -> ptr",
        "dll: CreateRemoteThread(",
        "dll: ResumeThread(ptr) -> int",
        "dll: SuspendThread(ptr) -> int",
        "dll: WaitForSingleObject(ptr, int) -> int",
        "dll: WaitForMultipleObjects(",
        "dll: CloseHandle(ptr) -> bool",
        "dll: CreateToolhelp32Snapshot(int, int) -> ptr",
        "dll: Process32First(ptr, PROCESSENTRY32) -> bool",
        "dll: Process32Next(ptr, PROCESSENTRY32) -> bool",
        "dll: Thread32First(ptr, THREADENTRY32) -> bool",
        "dll: Module32First(ptr, MODULEENTRY32) -> bool",
        "dll: CreateMutexA(ptr, bool, ptr) -> ptr",
        "dll: ReleaseMutex(ptr) -> bool",
        "dll: CreateEventA(ptr, bool, bool, ptr) -> ptr",
        "dll: SetEvent(ptr) -> bool",
        "dll: ResetEvent(ptr) -> bool",
        "dll: CreateSemaphoreA(ptr, int, int, ptr) -> ptr",
        "dll: ReleaseSemaphore(ptr, int, ptr) -> bool",
        "dll: InitializeCriticalSection(CRITICAL_SECTION)",
        "dll: EnterCriticalSection(CRITICAL_SECTION)",
        "dll: LeaveCriticalSection(CRITICAL_SECTION)",
        "dll: DeleteCriticalSection(CRITICAL_SECTION)",
        "dll: Sleep(int)",
        "dll: ExitProcess(int)",
    ] {
        assert!(has(dll), "{what}: `{dll}` is missing from the listing");
    }
    for rec in [
        "crecord: STARTUPINFOA",
        "crecord: PROCESS_INFORMATION",
        "crecord: PROCESSENTRY32",
        "crecord: THREADENTRY32",
        "crecord: MODULEENTRY32",
        "crecord: CRITICAL_SECTION",
        "crecord: SECURITY_ATTRIBUTES",
    ] {
        assert!(has(rec), "{what}: `{rec}` is missing from the listing");
    }
    for c in [
        "const: PROCESS_ALL_ACCESS int",
        "const: PROCESS_VM_READ int",
        "const: PROCESS_VM_WRITE int",
        "const: PROCESS_VM_OPERATION int",
        "const: PROCESS_QUERY_INFORMATION int",
        "const: PROCESS_CREATE_THREAD int",
        "const: THREAD_ALL_ACCESS int",
        "const: INFINITE int",
        "const: WAIT_OBJECT_0 int",
        "const: WAIT_TIMEOUT int",
        "const: TH32CS_SNAPPROCESS int",
        "const: TH32CS_SNAPTHREAD int",
        "const: TH32CS_SNAPMODULE int",
        "const: STILL_ACTIVE int",
        "const: CREATE_SUSPENDED int",
        "const: INVALID_HANDLE_VALUE int",
    ] {
        assert!(has(c), "{what}: `{c}` is missing from the listing");
    }
    // The 260-byte name array is what a tool-help entry is read through, and
    // the listing is where a person finds out it is one.
    assert!(
        has("szExeFile: byte[260]"),
        "{what}: PROCESSENTRY32's inline name array should be in the listing"
    );
}

/// The declaration file on its own, listed through the toolchain. This needs
/// no cross compiler: a Win32 kit must complete and document on a machine that
/// cannot build for Windows.
#[test]
fn commands_lists_the_kernel32_process_subsystem() {
    let dir = project("list");
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["commands", "--use", "winproc"])
        .current_dir(&dir)
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln commands");
    assert!(
        out.status.success(),
        "commands --use winproc failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_lists_the_subsystem(&String::from_utf8_lossy(&out.stdout), "winproc");
}

/// And the same names through the merged `win` kit, which is what a program
/// actually says. The `win` kit is assembled from several subsystem files at
/// once, so this test is about the merge as it stands rather than about one
/// file: a name declared in two subsystems is reported and left to the
/// integrator, who owns the reconciliation and has a test of its own for it.
/// What this file will not let past is `kernel32_proc.kdecl` failing to parse or
/// validate on its own account.
#[test]
fn the_win_kit_carries_the_kernel32_process_subsystem() {
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["commands", "--use", "win"])
        .current_dir(repo())
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln commands");
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        eprintln!("the merged `win` kit does not list yet:\n{err}");
        // "`X` is declared in both A and B" — two subsystems chose one name.
        // That is the integrator's to settle; it says nothing about whether
        // this file is correct.
        assert!(
            err.contains("is declared in both"),
            "the merged `win` kit failed for something other than a name collision:\n{err}"
        );
        assert!(
            !err.contains("kernel32_proc.kdecl:"),
            "`kernel32_proc.kdecl` itself failed to parse or validate:\n{err}"
        );
        return;
    }
    assert_lists_the_subsystem(&String::from_utf8_lossy(&out.stdout), "win");
}
