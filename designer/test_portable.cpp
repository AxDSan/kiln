/* Tests for designer/portable.h — the one file in Studio that knows which
 * operating system it is on.
 *
 * A console program, so it runs where Studio itself cannot be watched: under
 * wine with no display, on a build machine, in the test suite. It spawns
 * ITSELF as the child (`--echo`, `--sleep`, `--cat`), so it needs nothing
 * beside it, and it checks the child layer Studio's build, run, stop and
 * language-server code paths sit on: start a process with its output piped
 * back, read that output without blocking, learn that it exited and with
 * what, end one that will not, and hold a conversation over its stdin.
 *
 *   clang++ -std=c++17 -I designer designer/test_portable.cpp -o /tmp/t && /tmp/t
 *   x86_64-w64-mingw32-g++ -std=gnu++17 -I designer designer/test_portable.cpp \
 *       -static-libgcc -static-libstdc++ -o t.exe && wine t.exe
 */
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>

#include "portable.h"

#ifdef _WIN32
#include <windows.h>
#else
#include <unistd.h>
#endif

static int failures = 0;
static void check(const char* what, bool ok) {
    std::printf("  %-64s %s\n", what, ok ? "PASS" : "*** FAIL ***");
    std::fflush(stdout);
    if (!ok) failures++;
}

/// The C runtime on Windows writes `\r\n` for a `\n` on a text-mode
/// stream, so what comes through the pipe is compared with the `\r`s out —
/// as Studio's own line readers drop them.
static std::string lf(std::string s) {
    std::string out;
    for (char c : s) {
        if (c != '\r') out += c;
    }
    return out;
}

static void sleep_ms(int ms) {
#ifdef _WIN32
    Sleep(ms);
#else
    usleep(ms * 1000);
#endif
}

int main(int argc, char** argv) {
    using namespace kiln::sys;

    // --- the child roles -------------------------------------------------
    if (argc > 1 && std::strcmp(argv[1], "--echo") == 0) {
        std::printf("hello from stdout\n");
        std::fflush(stdout);
        std::fprintf(stderr, "hello from stderr\n");
        std::fflush(stderr);
        for (int i = 2; i < argc; i++) std::printf("arg:%s\n", argv[i]);
        return 3;
    }
    if (argc > 1 && std::strcmp(argv[1], "--sleep") == 0) {
        sleep_ms(30000);
        return 0;
    }
    if (argc > 1 && std::strcmp(argv[1], "--cat") == 0) {
        char buf[256];
        while (std::fgets(buf, sizeof buf, stdin)) {
            std::printf("echo:%s", buf);
            std::fflush(stdout);
        }
        return 0;
    }

    // --- the checks ------------------------------------------------------
    std::printf("portable\n");
    const std::string self = exe_path();
    check("exe_path is absolute and exists", is_absolute(self) && exists(self));
    check("exe_dir is the directory above it", self.rfind(exe_dir() + "/", 0) == 0);
    check("real_path of the executable is itself", real_path(self) == self);
    check("real_path of nothing is empty", real_path(exe_dir() + "/no-such-file-here").empty());
    check("temp_dir exists", exists(temp_dir()));
    check("cache_dir is somewhere", !cache_dir().empty());

    const std::string scratch = temp_dir() + "/kiln_portable_test";
    make_dirs(scratch + "/a/b");
    check("make_dirs makes every level", exists(scratch + "/a/b"));
    check("XDG_DATA_HOME wins for data_dir", [&] {
#ifdef _WIN32
        _putenv_s("XDG_DATA_HOME", scratch.c_str());
#else
        setenv("XDG_DATA_HOME", scratch.c_str(), 1);
#endif
        return data_dir() == scratch + "/kiln";
    }());
    check("file_uri has three slashes before a drive letter, two otherwise",
          file_uri(self).rfind(is_root_dir(self.substr(0, 3)) ? "file:///" : "file://", 0) == 0);
    check("is_root_dir: the top, and only the top",
          is_root_dir(root_dir()) && !is_root_dir(exe_dir()));
    check("parent_dir climbs one level", parent_dir(exe_dir() + "/x") == exe_dir());

#ifdef _WIN32
    check("quote_arg leaves a plain word alone", quote_arg("lsp") == "lsp");
    check("quote_arg quotes a space", quote_arg("a b") == "\"a b\"");
    check("quote_arg escapes a quote", quote_arg("a\"b") == "\"a\\\"b\"");
    check("quote_arg doubles backslashes before the closing quote",
          quote_arg("C:\\dir with space\\") == "\"C:\\dir with space\\\\\"");
#endif

    // capture_output: what Studio's catalogue, welcome screen and templates
    // go through. The child prints one line each to stdout and stderr and
    // exits 3.
    {
        std::string text;
        const int rc = capture_output(
#ifdef _WIN32
            quote_arg(self) + " --echo \"an arg\" plain",
#else
            "'" + self + "' --echo 'an arg' plain",
#endif
            true, text);
        text = lf(text);
        check("capture_output merged: exit status is the child's", rc == 3);
        check("capture_output merged: stdout arrives", text.find("hello from stdout\n") != std::string::npos);
        check("capture_output merged: stderr arrives", text.find("hello from stderr\n") != std::string::npos);
        check("capture_output: a quoted argument reaches the child whole",
              text.find("arg:an arg\n") != std::string::npos && text.find("arg:plain\n") != std::string::npos);
    }
    {
        std::string text;
        capture_output(
#ifdef _WIN32
            quote_arg(self) + " --echo",
#else
            "'" + self + "' --echo",
#endif
            false, text);
        text = lf(text);
        check("capture_output quiet: stdout arrives", text.find("hello from stdout\n") != std::string::npos);
        check("capture_output quiet: stderr does not", text.find("hello from stderr") == std::string::npos);
    }
    {
        std::string text;
        const int rc = capture_output(
#ifdef _WIN32
            "\"" + exe_dir() + "/no-such-program.exe\"",
#else
            "'" + exe_dir() + "/no-such-program'",
#endif
            false, text);
        check("capture_output of nothing fails rather than hangs", rc != 0);
    }

#ifdef _WIN32
    // The asynchronous child, as build/run/stop use it.
    {
        Child c;
        check("spawn --echo", spawn(c, quote_arg(self) + " --echo", true, false));
        std::string got;
        int code = -1;
        bool exited = false;
        for (int i = 0; i < 500 && !exited; i++) {
            char buf[64];
            int n;
            while ((n = read_nonblocking(c.out, buf, sizeof buf)) > 0) got.append(buf, (size_t)n);
            if (n == 0) close_output(c);
            exited = try_wait(c, code);
            if (!exited) Sleep(10);
        }
        check("the child exits and its code is read", exited && code == 3);
        got = lf(got);
        check("its output, both streams, arrived through the pipe",
              got.find("hello from stdout\n") != std::string::npos && got.find("hello from stderr\n") != std::string::npos);
        check("the pipe reports closed after the child is gone", c.out == nullptr);
        release(c);
    }
    {
        Child c;
        check("spawn --sleep", spawn(c, quote_arg(self) + " --sleep", true, false));
        int code = 0;
        Sleep(100);
        check("a running child is not reported exited", !try_wait(c, code));
        terminate(c);
        const DWORD w = WaitForSingleObject(c.process, 5000);
        check("terminate ends it", w == WAIT_OBJECT_0 && try_wait(c, code));
        char buf[8];
        check("its pipe reads as closed afterwards", read_nonblocking(c.out, buf, sizeof buf) == 0);
        release(c);
    }
    // The language server's shape: a conversation over stdin, then EOF.
    {
        Child c;
        check("spawn --cat with stdin", spawn(c, quote_arg(self) + " --cat", false, true));
        check("write to the child's stdin", write_all(c.in, "ping\n", 5));
        std::string got;
        for (int i = 0; i < 500 && lf(got).find("echo:ping\n") == std::string::npos; i++) {
            char buf[64];
            int n;
            while ((n = read_nonblocking(c.out, buf, sizeof buf)) > 0) got.append(buf, (size_t)n);
            if (n == 0) break;
            Sleep(10);
        }
        check("the reply comes back without blocking the reader", lf(got) == "echo:ping\n");
        close_stdin(c);
        int code = -1;
        bool exited = false;
        for (int i = 0; i < 500 && !(exited = try_wait(c, code)); i++) Sleep(10);
        check("closing stdin lets it finish", exited && code == 0);
        release(c);
    }
#endif

    std::printf("%d failure(s)\n", failures);
    return failures == 0 ? 0 : 1;
}
