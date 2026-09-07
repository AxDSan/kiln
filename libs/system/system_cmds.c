/* The "system" support library — the environment the program runs in.
 *
 * Three families, one per prefix:
 *   env_*  the environment block          (get / has / set / unset / enumerate)
 *   os_*   the machine and the account    (name, arch, host, user, home, temp)
 *   sys_*  this process                   (arguments, path, pid, clock, exit)
 *
 * Running another program deliberately does NOT live here; that belongs to the
 * `process` library.  This library only answers questions about the process it
 * is already inside.
 *
 * "kiln_core.h" is included rather than the ABI header alone because the
 * program's arguments (kn_arg_total / kn_arg_at) and the "" text sentinel
 * (kn_empty_text) are runtime internals, not part of the third-party ABI.
 */
#ifndef _WIN32
#define _POSIX_C_SOURCE 200809L
#endif
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include "kiln_core.h"

#ifdef _WIN32
/* Windows has no password database, no /proc and no nanosleep, so the account,
 * the machine, the binary's own path and the sleep all go through Win32.
 *
 * Those four are asked for in their WIDE form and converted to UTF-8, because
 * Kiln text is UTF-8 and the ANSI entry points would silently mangle a user
 * whose name is not in the machine's codepage.  The ENVIRONMENT is the one
 * thing that stays narrow: getenv and the environment block are C-library
 * surface, and reading half of it wide would mean two disagreeing views of the
 * same variables. */
#include <windows.h>
#include <process.h>
#define getpid _getpid
/* <stdlib.h> already declares _environ, which is the same block environ names
 * on POSIX; redeclaring it here would drop its dllimport attribute. */
#define environ _environ
#else
#include <pwd.h>
#include <unistd.h>
#if defined(__APPLE__)
#include <mach-o/dyld.h>
#endif
/* POSIX guarantees this; declaring it here rather than relying on a
 * feature-test macro keeps the translation unit portable across libcs. */
extern char **environ;
#endif

/* --- small helpers ---------------------------------------------------- */

/* Every text result is runtime-owned, like every other text value in the
 * program, so a caller can hold a failed result without a special case. */
static char *sys_dup_text(const char *s) {
    if (!s) return kn_empty_text();
    size_t n = strlen(s);
    char *out = (char *)kn_malloc((long)n + 1);
    if (!out) return NULL;
    memcpy(out, s, n + 1);
    return out;
}

#ifdef _WIN32
/* UTF-16 in, runtime-owned UTF-8 out; NULL when the conversion fails, which a
 * caller reports the same way it reports an out-of-memory result. */
static char *sys_from_wide(const wchar_t *w) {
    if (!w) return kn_empty_text();
    int n = WideCharToMultiByte(CP_UTF8, 0, w, -1, NULL, 0, NULL, NULL);
    if (n <= 0) return NULL;
    char *out = (char *)kn_malloc(n);
    if (!out) return NULL;
    if (WideCharToMultiByte(CP_UTF8, 0, w, -1, out, n, NULL, NULL) <= 0) return NULL;
    return out;
}
#endif

/* Fail with `code`/`msg` and return the "" text sentinel. */
static void sys_fail_text(Kiln_Slot *ret, int32_t code, const char *msg) {
    kn_error_set(code, msg);
    kn_ret_text(ret, kn_empty_text());
}

static void sys_fail_text_errno(Kiln_Slot *ret, int e, const char *what) {
    kn_error_set_errno(e, what);
    kn_ret_text(ret, kn_empty_text());
}

#ifdef _WIN32
/* A Win32 status is NOT an errno value, so it goes through kn_error_set rather
 * than kn_error_set_errno: the number a program reads back from
 * last_error_code() is what GetLastError() said, and comparing it against an
 * errno constant would be meaningless. */
static void sys_fail_win(Kiln_Slot *ret, const char *what) {
    DWORD code = GetLastError();
    char msg[128];
    snprintf(msg, sizeof msg, "%s: Windows error %lu", what, (unsigned long)code);
    kn_error_set((int32_t)code, msg);
    kn_ret_text(ret, kn_empty_text());
}
#endif

/* A name that is empty or contains '=' cannot address a variable at all — that
 * is a mistake in the program, not a missing variable, so it is a failure
 * rather than a "no". */
static int sys_bad_env_name(const char *name) {
    return !name || name[0] == '\0' || strchr(name, '=') != NULL;
}

/* --- env_* ------------------------------------------------------------ */

/* env_get(text name) -> text.
 *
 * An unset variable is a "no", not a failure: the slot is CLEARED and "" comes
 * back.  That makes "" ambiguous between unset and set-to-empty, which is
 * exactly why env_has ships beside it — the same pairing as file_at_end next to
 * file_read_line. */
void system_env_get(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *name = kn_arg_text(argv, 0);
    if (sys_bad_env_name(name)) {
        sys_fail_text(ret, KN_ERR_INVALID_ARG, "env_get: bad variable name");
        return;
    }
    const char *v = getenv(name);
    kn_error_clear();
    kn_ret_text(ret, sys_dup_text(v));
}

/* env_has(text name) -> bool.  false with code 0 means genuinely absent. */
void system_env_has(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *name = kn_arg_text(argv, 0);
    if (sys_bad_env_name(name)) {
        kn_error_set(KN_ERR_INVALID_ARG, "env_has: bad variable name");
        kn_ret_bool(ret, 0);
        return;
    }
    const char *v = getenv(name);
    kn_error_clear();
    kn_ret_bool(ret, v != NULL);
}

/* env_set(text name, text value) -> bool.  Overwrites. */
void system_env_set(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *name = kn_arg_text(argv, 0);
    const char *value = kn_arg_text(argv, 1);
    if (sys_bad_env_name(name)) {
        kn_error_set(KN_ERR_INVALID_ARG, "env_set: bad variable name");
        kn_ret_bool(ret, 0);
        return;
    }
    if (!value) value = "";
#ifdef _WIN32
    /* _putenv_s returns an errno value directly rather than setting errno. */
    int rc = (int)_putenv_s(name, value);
    int e = rc;
#else
    int rc = setenv(name, value, 1);
    int e = errno;
#endif
    if (rc != 0) {
        kn_error_set_errno(e, "env_set");
        kn_ret_bool(ret, 0);
        return;
    }
    kn_error_clear();
    kn_ret_bool(ret, 1);
}

/* env_unset(text name) -> bool.  Removing something already absent succeeds:
 * the post-condition asked for is "not set", and it holds. */
void system_env_unset(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *name = kn_arg_text(argv, 0);
    if (sys_bad_env_name(name)) {
        kn_error_set(KN_ERR_INVALID_ARG, "env_unset: bad variable name");
        kn_ret_bool(ret, 0);
        return;
    }
#ifdef _WIN32
    /* Assigning an empty value is how Windows removes a variable; there is no
     * separate unset call. */
    int rc = (int)_putenv_s(name, "");
    int e = rc;
#else
    int rc = unsetenv(name);
    int e = errno;
#endif
    if (rc != 0) {
        kn_error_set_errno(e, "env_unset");
        kn_ret_bool(ret, 0);
        return;
    }
    kn_error_clear();
    kn_ret_bool(ret, 1);
}

static int sys_env_total(void) {
    int n = 0;
    if (!environ) return 0;
    while (environ[n]) n++;
    return n;
}

/* env_count() -> int.  A collection is a count plus an indexed accessor; this
 * one cannot fail, so it never touches the error slot. */
void system_env_count(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; (void)argv;
    kn_ret_int(ret, sys_env_total());
}

/* env_name_at(int index) -> text : the NAME of the i-th variable, 0-based.
 * The value is then env_get(name) — the accessor deliberately does not return
 * "NAME=value", which a program would only have to take apart again. */
void system_env_name_at(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    int i = kn_arg_int(argv, 0);
    if (i < 1 || i > sys_env_total()) {
        sys_fail_text(ret, KN_ERR_INVALID_ARG, "env_name_at: index out of range");
        return;
    }
    const char *entry = environ[i - 1];
    const char *eq = strchr(entry, '=');
    size_t n = eq ? (size_t)(eq - entry) : strlen(entry);
    char *out = (char *)kn_malloc((long)n + 1);
    if (!out) { sys_fail_text(ret, KN_ERR_TABLE_FULL, "env_name_at: out of memory"); return; }
    memcpy(out, entry, n);
    out[n] = '\0';
    kn_error_clear();
    kn_ret_text(ret, out);
}

/* --- os_* ------------------------------------------------------------- */

/* os_name() -> text : "linux", "macos", "windows", or "unknown".
 * Decided at compile time, so it cannot fail and never touches the slot. */
void system_os_name(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; (void)argv;
#if defined(_WIN32)
    const char *s = "windows";
#elif defined(__APPLE__)
    const char *s = "macos";
#elif defined(__linux__)
    const char *s = "linux";
#else
    const char *s = "unknown";
#endif
    kn_ret_text(ret, sys_dup_text(s));
}

/* os_arch() -> text : the CPU this binary was built for. */
void system_os_arch(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; (void)argv;
#if defined(__x86_64__) || defined(_M_X64)
    const char *s = "x86_64";
#elif defined(__aarch64__) || defined(_M_ARM64)
    const char *s = "aarch64";
#elif defined(__i386__) || defined(_M_IX86)
    const char *s = "x86";
#elif defined(__arm__)
    const char *s = "arm";
#elif defined(__riscv) && __riscv_xlen == 64
    const char *s = "riscv64";
#else
    const char *s = "unknown";
#endif
    kn_ret_text(ret, sys_dup_text(s));
}

/* os_host_name() -> text.  "" on failure. */
void system_os_host_name(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; (void)argv;
#ifdef _WIN32
    wchar_t w[256];
    DWORD n = (DWORD)(sizeof w / sizeof w[0]);
    if (!GetComputerNameExW(ComputerNameDnsHostname, w, &n)) {
        sys_fail_win(ret, "os_host_name");
        return;
    }
    char *out = sys_from_wide(w);
    if (!out) { sys_fail_text(ret, KN_ERR_UNSUPPORTED, "os_host_name: out of memory"); return; }
    kn_error_clear();
    kn_ret_text(ret, out);
#else
    char buf[256];
    buf[0] = '\0';
    int rc = gethostname(buf, sizeof buf);
    int e = errno;
    if (rc != 0) { sys_fail_text_errno(ret, e, "os_host_name"); return; }
    buf[sizeof buf - 1] = '\0';   /* truncation may leave it unterminated */
    kn_error_clear();
    kn_ret_text(ret, sys_dup_text(buf));
#endif
}

/* The account's name and home come from the environment first, because that is
 * what a login shell set and what the user would expect a program to honour,
 * and from the password database only when it is missing. */
void system_os_user_name(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; (void)argv;
#ifdef _WIN32
    const char *v = getenv("USERNAME");
    if (v && *v) { kn_error_clear(); kn_ret_text(ret, sys_dup_text(v)); return; }
    wchar_t w[256];
    DWORD n = (DWORD)(sizeof w / sizeof w[0]);
    if (!GetUserNameW(w, &n)) { sys_fail_win(ret, "os_user_name"); return; }
    char *out = sys_from_wide(w);
    if (!out) { sys_fail_text(ret, KN_ERR_UNSUPPORTED, "os_user_name: out of memory"); return; }
    kn_error_clear();
    kn_ret_text(ret, out);
#else
    const char *v = getenv("USER");
    if (!v || !*v) v = getenv("LOGNAME");
    if (v && *v) { kn_error_clear(); kn_ret_text(ret, sys_dup_text(v)); return; }
    errno = 0;
    struct passwd *pw = getpwuid(getuid());
    int e = errno;
    if (!pw || !pw->pw_name) {
        if (e) sys_fail_text_errno(ret, e, "os_user_name");
        else   sys_fail_text(ret, KN_ERR_UNSUPPORTED, "os_user_name: no such user");
        return;
    }
    kn_error_clear();
    kn_ret_text(ret, sys_dup_text(pw->pw_name));
#endif
}

void system_os_home_dir(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; (void)argv;
    const char *v = getenv("HOME");
#ifdef _WIN32
    /* HOME is honoured first because a shell that set it meant it; USERPROFILE
     * is what Windows itself sets, and there is no password database behind
     * either of them. */
    if (!v || !*v) v = getenv("USERPROFILE");
    if (v && *v) { kn_error_clear(); kn_ret_text(ret, sys_dup_text(v)); return; }
    sys_fail_text(ret, KN_ERR_UNSUPPORTED, "os_home_dir: no home directory");
#else
    if (v && *v) { kn_error_clear(); kn_ret_text(ret, sys_dup_text(v)); return; }
    errno = 0;
    struct passwd *pw = getpwuid(getuid());
    int e = errno;
    if (!pw || !pw->pw_dir || !*pw->pw_dir) {
        if (e) sys_fail_text_errno(ret, e, "os_home_dir");
        else   sys_fail_text(ret, KN_ERR_UNSUPPORTED, "os_home_dir: no home directory");
        return;
    }
    kn_error_clear();
    kn_ret_text(ret, sys_dup_text(pw->pw_dir));
#endif
}

/* os_temp_dir() -> text.  There is always an answer — the environment's, or the
 * platform default — so this cannot fail and never touches the slot. */
void system_os_temp_dir(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; (void)argv;
#ifdef _WIN32
    /* GetTempPathW consults TMP, TEMP and USERPROFILE in turn and falls back to
     * the Windows directory, so it already IS the search this command performs
     * by hand elsewhere.  It returns a trailing backslash, which is dropped so
     * the answer joins like every other directory this library reports. */
    wchar_t w[MAX_PATH + 2];
    DWORD n = GetTempPathW((DWORD)(sizeof w / sizeof w[0]), w);
    if (n == 0 || n >= sizeof w / sizeof w[0]) { kn_ret_text(ret, sys_dup_text("C:\\Windows\\Temp")); return; }
    while (n > 1 && (w[n - 1] == L'\\' || w[n - 1] == L'/')) w[--n] = L'\0';
    char *out = sys_from_wide(w);
    kn_ret_text(ret, out ? out : sys_dup_text("C:\\Windows\\Temp"));
#else
    const char *v = getenv("TMPDIR");
    if (!v || !*v) v = getenv("TMP");
    if (!v || !*v) v = getenv("TEMP");
    if (!v || !*v) v = "/tmp";
    kn_ret_text(ret, sys_dup_text(v));
#endif
}

/* --- sys_* ------------------------------------------------------------ */

/* sys_arg_count() -> int : how many command-line arguments, including the
 * program name at index 0.  A library target links no main(), so nothing
 * captured the arguments and this reports 0 — honest, rather than reading a
 * pointer nobody set.  It cannot fail, so it never touches the slot. */
void system_sys_arg_count(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; (void)argv;
    /* The count of REAL arguments, so `sys_arg(1)` .. `sys_arg(sys_arg_count())`
     * is the whole range and the program name is not quietly one of them. */
    int total = kn_arg_total();
    kn_ret_int(ret, total > 0 ? total - 1 : 0);
}

/* sys_arg(int index) -> text.  Index 0 is the program name as invoked.
 * Out of range is a failure rather than a silent "", because "" is itself a
 * perfectly legal argument value. */
void system_sys_arg(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    int i = kn_arg_int(argv, 0);
    /* sys_arg(1) is the first real argument.  argv[0] is the program name and
     * is NOT reachable here — sys_program_path() already reports it, and one
     * way to say a thing is enough. */
    const char *v = i < 1 ? 0 : kn_arg_at(i);
    if (!v) { sys_fail_text(ret, KN_ERR_INVALID_ARG, "sys_arg: index out of range"); return; }
    kn_error_clear();
    kn_ret_text(ret, sys_dup_text(v));
}

/* The running binary's own path, resolved from the OS rather than from argv[0],
 * which the caller controls and may not be a path at all. */
static int sys_exe_path(char *buf, size_t cap, int *saved_errno) {
    *saved_errno = 0;
#if defined(_WIN32)
    wchar_t w[4096];
    DWORD n = GetModuleFileNameW(NULL, w, (DWORD)(sizeof w / sizeof w[0]));
    if (n == 0 || n >= sizeof w / sizeof w[0]) { *saved_errno = ENAMETOOLONG; return 0; }
    if (WideCharToMultiByte(CP_UTF8, 0, w, -1, buf, (int)cap, NULL, NULL) <= 0) {
        *saved_errno = ENAMETOOLONG;
        return 0;
    }
    return 1;
#elif defined(__linux__)
    ssize_t n = readlink("/proc/self/exe", buf, cap - 1);
    int e = errno;
    if (n < 0) { *saved_errno = e; return 0; }
    buf[n] = '\0';
    return 1;
#elif defined(__APPLE__)
    uint32_t size = (uint32_t)cap;
    if (_NSGetExecutablePath(buf, &size) != 0) { *saved_errno = ENAMETOOLONG; return 0; }
    buf[cap - 1] = '\0';
    return 1;
#else
    (void)buf; (void)cap;
    return -1;   /* no way to ask on this platform */
#endif
}

void system_sys_program_path(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; (void)argv;
    char buf[4096];
    int e = 0;
    int rc = sys_exe_path(buf, sizeof buf, &e);
    if (rc < 0) { sys_fail_text(ret, KN_ERR_UNSUPPORTED, "sys_program_path: unsupported platform"); return; }
    if (rc == 0) { sys_fail_text_errno(ret, e, "sys_program_path"); return; }
    kn_error_clear();
    kn_ret_text(ret, sys_dup_text(buf));
}

/* sys_program_dir() -> text : the directory holding the binary, with no
 * trailing separator (except at the root, where "/" is the directory). */
void system_sys_program_dir(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; (void)argv;
    char buf[4096];
    int e = 0;
    int rc = sys_exe_path(buf, sizeof buf, &e);
    if (rc < 0) { sys_fail_text(ret, KN_ERR_UNSUPPORTED, "sys_program_dir: unsupported platform"); return; }
    if (rc == 0) { sys_fail_text_errno(ret, e, "sys_program_dir"); return; }
    char *slash = strrchr(buf, '/');
#ifdef _WIN32
    /* GetModuleFileNameW answers in backslashes, and a program may still have
     * been launched through a path that mixes the two. */
    char *back = strrchr(buf, '\\');
    if (!slash || (back && back > slash)) slash = back;
#endif
    if (!slash) { kn_error_clear(); kn_ret_text(ret, sys_dup_text(".")); return; }
    if (slash == buf) buf[1] = '\0'; else *slash = '\0';
    kn_error_clear();
    kn_ret_text(ret, sys_dup_text(buf));
}

/* sys_process_id() -> int.  Always available; never touches the slot. */
void system_sys_process_id(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; (void)argv;
    kn_ret_int(ret, (int32_t)getpid());
}

/* sys_tick_count() -> int64 : milliseconds from a monotonic clock, for timing.
 * The origin is arbitrary and only differences are meaningful — that is the
 * point: unlike now(), it cannot go backwards when the wall clock is adjusted.
 * -1 on failure. */
void system_sys_tick_count(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; (void)argv;
#ifdef _WIN32
    /* GetTickCount64 already counts milliseconds and cannot fail, so the -1
     * this command documents is unreachable here. */
    kn_error_clear();
    kn_ret_int64(ret, (int64_t)GetTickCount64());
#else
    struct timespec ts;
    int rc = clock_gettime(CLOCK_MONOTONIC, &ts);
    int e = errno;
    if (rc != 0) {
        kn_error_set_errno(e, "sys_tick_count");
        kn_ret_int64(ret, -1);
        return;
    }
    kn_error_clear();
    kn_ret_int64(ret, (int64_t)ts.tv_sec * 1000 + (int64_t)(ts.tv_nsec / 1000000));
#endif
}

/* sys_sleep_ms(int ms) : void.  A negative or zero wait returns at once.
 * Signals are absorbed by resuming the remaining time, so this waits for the
 * duration asked for; it has no way to report failure and never touches the
 * error slot. */
void system_sys_sleep_ms(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; (void)ret;
    int ms = kn_arg_int(argv, 0);
    if (ms <= 0) return;
#ifdef _WIN32
    Sleep((DWORD)ms);
#else
    struct timespec req;
    req.tv_sec = ms / 1000;
    req.tv_nsec = (long)(ms % 1000) * 1000000L;
    struct timespec rem;
    while (nanosleep(&req, &rem) != 0 && errno == EINTR) req = rem;
#endif
}

/* sys_quit(int code) : void, and does not return.  Runtime teardown runs first,
 * exactly as it does on the normal path out of main(), so a program that quits
 * early still closes its handles and frees its data. */
void system_sys_quit(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; (void)ret;
    int code = kn_arg_int(argv, 0);
    E_DestroyRes();
    exit(code);
}
