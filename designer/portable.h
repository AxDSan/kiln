// The one place in Studio that knows which operating system it is on.
//
// Same rule as libs/README.md gives a support library: the branch is a thin
// shim, `#ifdef _WIN32` appears here and in the few process-handling
// functions that cannot be shimmed at this size, and everything else reads
// the same on both. The POSIX half is the calls Studio always made, inlined
// — a Linux build behaves exactly as it did before this header existed.
//
// What differs on Windows, and where it lands here:
//   - the executable's own path      GetModuleFileName, not /proc/self/exe
//   - canonical paths                _fullpath (which does not check
//                                    existence, so this does), forward slashes
//   - per-user directories           %APPDATA% / %LOCALAPPDATA% / %TEMP%, after
//                                    the XDG variables, which a test sets on
//                                    both platforms to keep its scratch out of
//                                    a person's lists
//   - a child process with pipes     CreateProcess + PeekNamedPipe in place of
//                                    fork + O_NONBLOCK; see `Child` at the end
//   - the null device on a shell line  `NUL`, not `/dev/null`. Wine maps Z:\ to
//                                    `/`, so `2>/dev/null` passes under wine and
//                                    fails on Windows; do not trust that test.
#ifndef KILN_DESIGNER_PORTABLE_H
#define KILN_DESIGNER_PORTABLE_H

#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>
#include <sys/stat.h>

#ifdef _WIN32
#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>
// WIN32_LEAN_AND_MEAN leaves commdlg.h out, and with it OPENFILENAME,
// GetOpenFileName and every OFN_ flag — which `pick_open_file` needs.
#include <commdlg.h>
#include <direct.h>
#include <io.h>
#else
#include <limits.h>
#include <sys/wait.h>
#include <unistd.h>
#endif

namespace kiln::sys {

/// Does a path end in this extension?
///
/// Spelled out rather than written inline as `compare(n - 4, 4, ".x")`,
/// because the inline form hard-codes the extension's length beside it and the
/// two fall out of step the moment the extension is renamed. That is exactly
/// what happened at 1.0.0: `.oir` became `.kiln`, every literal was updated,
/// every `4` was not, and Studio decided no file was ever a project and opened
/// the welcome screen instead.
inline bool has_ext(const std::string& path, const std::string& ext) {
    return path.size() > ext.size() &&
           path.compare(path.size() - ext.size(), ext.size(), ext) == 0;
}

/// Hand a URL — or a file:// path — to whatever the desktop opens it with.
/// Returns false when the launcher itself could not be run; a browser that
/// opens and then fails is beyond anything we can see from here.
inline bool open_url(const std::string& url) {
#ifdef _WIN32
    // `start` is a cmd builtin, not a program, so it needs a shell. The empty
    // quoted argument is the window title `start` would otherwise take the
    // URL to be.
    const std::string cmd = "start \"\" \"" + url + "\"";
#else
    const std::string cmd = "xdg-open '" + url + "' >/dev/null 2>&1 &";
#endif
    return std::system(cmd.c_str()) == 0;
}

/// Backslashes to forward slashes. Studio splits and joins paths on '/',
/// and every Windows API accepts either.
inline std::string slashes(std::string s) {
#ifdef _WIN32
    for (auto& c : s) {
        if (c == '\\') c = '/';
    }
#endif
    return s;
}

/// This executable's absolute path, or "" when the platform will not say.
inline std::string exe_path() {
#ifdef _WIN32
    char buf[4096];
    const DWORD n = GetModuleFileNameA(nullptr, buf, sizeof buf);
    if (n == 0 || n >= sizeof buf) return "";
    return slashes(std::string(buf, n));
#else
    char buf[4096];
    const ssize_t n = ::readlink("/proc/self/exe", buf, sizeof buf - 1);
    if (n <= 0) return "";
    buf[n] = 0;
    return buf;
#endif
}

/// The directory `exe_path` is in, without the trailing slash; "" if unknown.
inline std::string exe_dir() {
    const std::string exe = exe_path();
    const size_t slash = exe.find_last_of('/');
    return slash == std::string::npos ? "" : exe.substr(0, slash);
}

/// The name the toolchain's executable has beside Studio.
inline const char* kiln_exe_name() {
#ifdef _WIN32
    return "kiln.exe";
#else
    return "kiln";
#endif
}

inline bool exists(const std::string& p) { return ::access(p.c_str(), F_OK) == 0; }
inline bool readable(const std::string& p) { return ::access(p.c_str(), R_OK) == 0; }

/// Can this be run? `X_OK` is not a mode the Microsoft CRT accepts — it
/// answers EINVAL to it — so on Windows the question is whether it is there.
inline bool executable(const std::string& p) {
#ifdef _WIN32
    return exists(p);
#else
    return ::access(p.c_str(), X_OK) == 0;
#endif
}

/// The canonical absolute path of something that exists, or "" otherwise —
/// what `realpath(3)` answers, on both platforms.
inline std::string real_path(const std::string& p) {
#ifdef _WIN32
    char buf[4096];
    if (!_fullpath(buf, p.c_str(), sizeof buf)) return "";
    if (!exists(buf)) return "";
    std::string out = slashes(buf);
    while (out.size() > 3 && out.back() == '/') out.pop_back();
    return out;
#else
    char buf[PATH_MAX];
    if (!::realpath(p.c_str(), buf)) return "";
    return buf;
#endif
}

/// One directory, as `mkdir -p` would make one level of it: existing is fine.
inline void make_dir(const std::string& p) {
#ifdef _WIN32
    _mkdir(p.c_str());
#else
    ::mkdir(p.c_str(), 0755);
#endif
}

/// `mkdir -p` over a '/'-separated path: the loop the callers had.
inline void make_dirs(const std::string& dir) {
    std::string acc;
    for (size_t i = 0; i < dir.size(); i++) {
        acc += dir[i];
        if (dir[i] == '/' || i + 1 == dir.size()) make_dir(acc);
    }
}

/// What to append to a shell line to silence its stderr.
inline const char* quiet_stderr() {
#ifdef _WIN32
    return " 2>NUL";
#else
    return " 2>/dev/null";
#endif
}

/// The person's home directory, or "" when the environment has none.
inline std::string home_dir() {
#ifdef _WIN32
    const char* p = std::getenv("USERPROFILE");
#else
    const char* p = std::getenv("HOME");
#endif
    return p && *p ? slashes(p) : "";
}

/// The root a path browser starts from when nothing better is known.
inline std::string root_dir() {
#ifdef _WIN32
    return "C:/";
#else
    return "/";
#endif
}

/// Is `dir` the top of its tree — `/`, or `C:/` on Windows?
inline bool is_root_dir(const std::string& dir) {
#ifdef _WIN32
    return dir.size() <= 3 && dir.size() >= 2 && dir[1] == ':';
#else
    return dir == "/";
#endif
}

/// The directory above `dir`, for a browser's `..` entry.
inline std::string parent_dir(const std::string& dir) {
    const size_t slash = dir.find_last_of('/');
    if (slash == std::string::npos) return dir;
#ifdef _WIN32
    if (slash <= 2) return dir.substr(0, 2) + "/";   // C:/x -> C:/
#endif
    return slash == 0 ? "/" : dir.substr(0, slash);
}

/// Absolute, as this platform spells it.
inline bool is_absolute(const std::string& p) {
#ifdef _WIN32
    return (p.size() >= 2 && p[1] == ':') || (!p.empty() && (p[0] == '/' || p[0] == '\\'));
#else
    return !p.empty() && p[0] == '/';
#endif
}

/// The filesystem path behind the URL form Studio hands RmlUi — a leading
/// slash doubled, because its URL parser eats one. On Windows the doubled
/// slash sits before a drive letter: `/C:/x` is `C:/x`.
inline std::string path_of_url(const std::string& url) {
    if (url.compare(0, 2, "//") == 0) return url.substr(1);
#ifdef _WIN32
    if (url.size() >= 3 && url[0] == '/' && url[2] == ':') return url.substr(1);
#endif
    return url;
}

/// The system's scratch directory.
inline std::string temp_dir() {
#ifdef _WIN32
    for (const char* v : {"TEMP", "TMP"}) {
        if (const char* p = std::getenv(v)) {
            if (*p) {
                std::string out = slashes(p);
                while (out.size() > 3 && out.back() == '/') out.pop_back();
                return out;
            }
        }
    }
    return "C:/Windows/Temp";
#else
    return "/tmp";
#endif
}

/// Where Studio keeps per-user data (the recent list). `XDG_DATA_HOME`
/// first on both platforms: it is how a test run keeps its scratch files out
/// of the list a person sees on their next real start. "" when nowhere.
inline std::string data_dir() {
    const char* xdg = std::getenv("XDG_DATA_HOME");
    if (xdg && *xdg) return slashes(xdg) + "/kiln";
#ifdef _WIN32
    const char* app = std::getenv("APPDATA");
    if (app && *app) return slashes(app) + "/kiln";
    return "";
#else
    const char* home = std::getenv("HOME");
    if (!home) return "";
    return std::string(home) + "/.local/share/kiln";
#endif
}

/// Where Studio keeps per-user cache files (the dot-grid tile). Always
/// somewhere: the scratch directory is the last resort.
inline std::string cache_dir() {
    const char* xdg = std::getenv("XDG_CACHE_HOME");
    if (xdg && *xdg) return slashes(xdg) + "/kiln";
#ifdef _WIN32
    const char* local = std::getenv("LOCALAPPDATA");
    if (local && *local) return slashes(local) + "/kiln/cache";
    return temp_dir();
#else
    const char* home = std::getenv("HOME");
    if (home && *home) return std::string(home) + "/.cache/kiln";
    return "/tmp";
#endif
}

/// A `file:` URI for an absolute path. A drive letter needs the third slash:
/// `file://C:/x` has a host called `C`.
inline std::string file_uri(const std::string& abs) {
#ifdef _WIN32
    return "file:///" + slashes(abs);
#else
    return "file://" + abs;
#endif
}

/// Delete a file, saying whether it was there to delete. Missing is not an
/// error: the callers are cleaning up, and "already gone" is the goal state.
inline bool remove_file(const std::string& path) {
#ifdef _WIN32
    return DeleteFileA(path.c_str()) != 0;
#else
    return ::remove(path.c_str()) == 0;
#endif
}

/// Delete a directory, and only when it is empty. The emptiness check is the
/// operating system's, not ours: a directory holding something a user put
/// there refuses to go, which is the guard the callers want.
inline bool remove_dir(const std::string& path) {
#ifdef _WIN32
    return RemoveDirectoryA(path.c_str()) != 0;
#else
    return ::rmdir(path.c_str()) == 0;
#endif
}

/// A program built by Studio: what to call the file so the platform runs it.
inline std::string program_name(const std::string& stem) {
#ifdef _WIN32
    return stem + ".exe";
#else
    return stem;
#endif
}

/// The platform's own Open dialog, or "" when there is not one to ask.
///
/// The empty answer is not a failure — it is "no native dialog here", and the
/// caller falls back to Studio's own list. That path has to keep working:
/// a machine with no portal and no zenity is exactly the minimal container
/// and wine setup the project is tested on, and a dialog that cannot open is
/// worse than a list that can.
///
/// A cancelled dialog is also "": the caller shows its own browser, which has
/// a Back button. Telling the two apart would buy nothing.
///
/// `patterns` is a display name and a glob, e.g. {"Kiln project", "*.kiln"}.
inline std::string pick_open_file(const std::string& title, const std::string& start_dir,
                                  const std::string& filter_name,
                                  const std::string& filter_glob) {
#ifdef _WIN32
    // comdlg32, which every Windows since 95 has and wine implements.
    std::string filter = filter_name;
    filter.push_back('\0');
    filter += filter_glob;
    filter.push_back('\0');
    filter += "All files";
    filter.push_back('\0');
    filter += "*.*";
    filter.push_back('\0');
    filter.push_back('\0');

    char file[MAX_PATH] = {0};
    const std::string dir = start_dir;
    OPENFILENAMEA ofn{};
    ofn.lStructSize = sizeof ofn;
    ofn.hwndOwner = nullptr;
    ofn.lpstrFilter = filter.c_str();
    ofn.lpstrFile = file;
    ofn.nMaxFile = sizeof file;
    ofn.lpstrTitle = title.c_str();
    ofn.lpstrInitialDir = dir.empty() ? nullptr : dir.c_str();
    // No CHDIR: Studio resolves its own paths against the working directory,
    // and a dialog that quietly moved it would break the next relative build.
    ofn.Flags = OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR | OFN_EXPLORER;
    if (!GetOpenFileNameA(&ofn)) return "";
    return slashes(file);
#else
    // zenity first, then kdialog: between them they cover GNOME and KDE, and
    // both are a plain child process writing one path to stdout. Neither is a
    // dependency — `command -v` decides, and nothing is installed to suit us.
    const char* tools[] = {"zenity", "kdialog"};
    for (const char* tool : tools) {
        std::string probe = "command -v ";
        probe += tool;
        probe += " >/dev/null 2>&1";
        if (std::system(probe.c_str()) != 0) continue;

        auto shell_quote = [](const std::string& s) {
            std::string q = "'";
            for (char c : s) {
                if (c == '\'') q += "'\\''";
                else q += c;
            }
            return q + "'";
        };
        std::string cmd;
        if (std::string(tool) == "zenity") {
            cmd = "zenity --file-selection --title=" + shell_quote(title);
            if (!start_dir.empty()) cmd += " --filename=" + shell_quote(start_dir + "/");
            cmd += " --file-filter=" + shell_quote(filter_name + " | " + filter_glob);
            cmd += " --file-filter=" + shell_quote("All files | *");
        } else {
            cmd = "kdialog --getopenfilename ";
            cmd += shell_quote(start_dir.empty() ? "." : start_dir);
            cmd += " " + shell_quote(filter_glob + "|" + filter_name);
            cmd += " --title " + shell_quote(title);
        }
        cmd += " 2>/dev/null";

        std::string out;
        if (FILE* p = ::popen(cmd.c_str(), "r")) {
            char buf[4096];
            while (::fgets(buf, sizeof buf, p)) out += buf;
            // A non-zero status is a cancelled dialog, which is not an error.
            if (::pclose(p) != 0) return "";
        }
        while (!out.empty() && (out.back() == '\n' || out.back() == '\r')) out.pop_back();
        return out;
    }
    (void)title;
    (void)start_dir;
    (void)filter_name;
    (void)filter_glob;
    return "";
#endif
}

/// Whether `pick_open_file` has a dialog to show. Studio asks so it can label
/// the tile honestly rather than opening nothing.
inline bool has_native_file_dialog() {
#ifdef _WIN32
    return true;
#else
    return std::system("command -v zenity >/dev/null 2>&1") == 0 ||
           std::system("command -v kdialog >/dev/null 2>&1") == 0;
#endif
}

#ifdef _WIN32
/// A child process with its output piped back, and optionally its input
/// piped in — what fork + pipe + O_NONBLOCK give Studio on POSIX. The reads
/// never block: `PeekNamedPipe` says what is there before `ReadFile` takes
/// it. Console-subsystem children get no console of their own
/// (CREATE_NO_WINDOW): Studio is a GUI program, and every `kiln.exe` it
/// runs would otherwise flash a black window.
struct Child {
    HANDLE process = nullptr;
    HANDLE out = nullptr;   ///< our end of the child's stdout (and stderr, when merged)
    HANDLE in = nullptr;    ///< our end of the child's stdin, or null
    DWORD pid = 0;
    bool running() const { return process != nullptr; }
};

/// Quote one argument for a CreateProcess command line, the way the
/// Microsoft CRT unquotes it. Backslashes only matter before a quote.
inline std::string quote_arg(const std::string& a) {
    if (!a.empty() && a.find_first_of(" \t\"") == std::string::npos) return a;
    std::string out = "\"";
    size_t bs = 0;
    for (char c : a) {
        if (c == '\\') {
            bs++;
            continue;
        }
        if (c == '"') {
            out.append(bs * 2 + 1, '\\');
            bs = 0;
            out += '"';
            continue;
        }
        out.append(bs, '\\');
        bs = 0;
        out += c;
    }
    out.append(bs * 2, '\\');
    out += '"';
    return out;
}

/// Start `cmdline`. Its stdout comes back through `out`; stderr joins it when
/// `merge_stderr`, else goes to the null device; stdin is piped when
/// `want_stdin`, else is the null device. False when it could not start.
inline bool spawn(Child& c, const std::string& cmdline, bool merge_stderr, bool want_stdin) {
    SECURITY_ATTRIBUTES sa{};
    sa.nLength = sizeof sa;
    sa.bInheritHandle = TRUE;
    HANDLE out_r = nullptr, out_w = nullptr, in_r = nullptr, in_w = nullptr;
    if (!CreatePipe(&out_r, &out_w, &sa, 0)) return false;
    SetHandleInformation(out_r, HANDLE_FLAG_INHERIT, 0);
    if (want_stdin) {
        if (!CreatePipe(&in_r, &in_w, &sa, 0)) {
            CloseHandle(out_r);
            CloseHandle(out_w);
            return false;
        }
        SetHandleInformation(in_w, HANDLE_FLAG_INHERIT, 0);
    }
    // A child whose stderr or stdin is not ours still needs a valid handle
    // there: an invalid one makes some runtimes refuse to start.
    HANDLE null_h = CreateFileA("NUL", GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE,
                                &sa, OPEN_EXISTING, 0, nullptr);
    STARTUPINFOA si{};
    si.cb = sizeof si;
    si.dwFlags = STARTF_USESTDHANDLES;
    si.hStdOutput = out_w;
    si.hStdError = merge_stderr ? out_w : null_h;
    si.hStdInput = want_stdin ? in_r : null_h;
    PROCESS_INFORMATION pi{};
    std::string cmd = cmdline;   // CreateProcess may write into it
    const BOOL ok = CreateProcessA(nullptr, &cmd[0], nullptr, nullptr, TRUE, CREATE_NO_WINDOW, nullptr,
                                   nullptr, &si, &pi);
    CloseHandle(out_w);
    if (in_r) CloseHandle(in_r);
    if (null_h != INVALID_HANDLE_VALUE) CloseHandle(null_h);
    if (!ok) {
        CloseHandle(out_r);
        if (in_w) CloseHandle(in_w);
        return false;
    }
    CloseHandle(pi.hThread);
    c.process = pi.hProcess;
    c.out = out_r;
    c.in = in_w;
    c.pid = pi.dwProcessId;
    return true;
}

/// Read what the child has written so far, with `read(2)`'s answers on a
/// non-blocking pipe: > 0 bytes, 0 when the child closed its end, -1 when
/// there is nothing yet.
inline int read_nonblocking(HANDLE h, char* buf, size_t n) {
    DWORD avail = 0;
    if (!PeekNamedPipe(h, nullptr, 0, nullptr, &avail, nullptr)) return 0;   // broken: closed
    if (avail == 0) return -1;
    DWORD got = 0;
    if (!ReadFile(h, buf, (DWORD)(n < avail ? n : avail), &got, nullptr)) return 0;
    return (int)got;
}

/// Everything, or false when the child has gone.
inline bool write_all(HANDLE h, const char* data, size_t n) {
    size_t sent = 0;
    while (sent < n) {
        DWORD w = 0;
        if (!WriteFile(h, data + sent, (DWORD)(n - sent), &w, nullptr) || w == 0) return false;
        sent += w;
    }
    return true;
}

/// Has the child exited? Its exit code when so.
inline bool try_wait(const Child& c, int& code) {
    if (!c.process || WaitForSingleObject(c.process, 0) != WAIT_OBJECT_0) return false;
    DWORD ec = 0;
    GetExitCodeProcess(c.process, &ec);
    code = (int)ec;
    return true;
}

inline void terminate(const Child& c) {
    if (c.process) TerminateProcess(c.process, 1);
}

inline void close_stdin(Child& c) {
    if (c.in) CloseHandle(c.in);
    c.in = nullptr;
}

inline void close_output(Child& c) {
    if (c.out) CloseHandle(c.out);
    c.out = nullptr;
}

/// Release every handle. The process, if still running, is left alone.
inline void release(Child& c) {
    close_stdin(c);
    close_output(c);
    if (c.process) CloseHandle(c.process);
    c.process = nullptr;
    c.pid = 0;
}

/// Run `cmdline` to completion and hand back its stdout, the way `popen`
/// would — for the short listings Studio asks the toolchain for. Its stderr
/// joins the text when `merge_stderr`. -1 when it could not start.
inline int capture(const std::string& cmdline, bool merge_stderr, std::string& text) {
    Child c;
    if (!spawn(c, cmdline, merge_stderr, false)) return -1;
    char buf[4096];
    for (;;) {
        const int n = read_nonblocking(c.out, buf, sizeof buf);
        if (n > 0) {
            text.append(buf, (size_t)n);
            continue;
        }
        if (n == 0) break;
        Sleep(5);
    }
    WaitForSingleObject(c.process, INFINITE);
    int code = -1;
    try_wait(c, code);
    release(c);
    return code;
}
#endif

/// The text a command prints, and its exit status (-1 when it could not be
/// started). `popen` on POSIX; on Windows a direct CreateProcess, because
/// `_popen` from a program with no console opens one to run `cmd.exe` in.
inline int capture_output(const std::string& cmd, bool merge_stderr, std::string& text) {
#ifdef _WIN32
    return capture(cmd, merge_stderr, text);
#else
    const std::string line = cmd + (merge_stderr ? " 2>&1" : quiet_stderr());
    FILE* pipe = popen(line.c_str(), "r");
    if (!pipe) return -1;
    char buf[4096];
    size_t n;
    while ((n = std::fread(buf, 1, sizeof buf, pipe)) > 0) text.append(buf, n);
    const int rc = pclose(pipe);
    return rc == -1 ? -1 : (WIFEXITED(rc) ? WEXITSTATUS(rc) : -1);
#endif
}

} // namespace kiln::sys

#endif
