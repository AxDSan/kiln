/* The "process" support library — running other programs.
 *
 * Two ways to run something, and the difference is the whole design:
 *
 *   process_run / process_run_capture   run it to completion, hand back what
 *                                       it said or how it ended;
 *   process_start                        keep it alive behind a handle and
 *                                        talk to it line by line.
 *
 * The distinction this file is most careful about is "could not start" versus
 * "ran and failed".  A program that mistakes one for the other retries a
 * missing binary forever, or reports a compiler's honest exit 1 as a broken
 * installation.  So a child is spawned through an errno pipe: the child writes
 * its exec failure into a close-on-exec pipe, and the parent reading a value
 * there knows the program never ran.  "Could not start" is -1 (or handle 0)
 * with the error slot set; "ran and failed" is the real exit status with the
 * slot clear.
 *
 * Only the SDK header — nothing runtime-internal.
 */
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

/* Windows has no fork, so the errno-pipe trick described above is not how it
 * tells "could not start" from "ran and failed": CreateProcess reports the
 * failure to start directly, in its return value, which is the same
 * distinction reached by a shorter road.  The pipe ends are handed to the CRT
 * with _open_osfhandle, so everything below the spawn — the fd reads, the
 * FILE* wrapper, the line loop — is the same code on both platforms.
 *
 * A child is identified by a HANDLE here and by a pid there, and the five
 * primitives that differ (spawn, poll, reap, terminate, release) are written
 * twice and used once. */
#ifdef _WIN32
#include <windows.h>
#include <io.h>
#include <fcntl.h>
typedef HANDLE process_pid_t;
#define PROCESS_NO_PID NULL
#define read  _read
#define close _close
#define fdopen _fdopen
#else
#include <fcntl.h>
#include <signal.h>
#include <unistd.h>
#include <sys/types.h>
#include <sys/wait.h>
typedef pid_t process_pid_t;
#define PROCESS_NO_PID ((pid_t)-1)
#endif

#include "kiln_abi.h"

/* A spawn failure carries a platform code: an errno value on POSIX and a Win32
 * status on Windows, which is not an errno value and must not be run through
 * strerror. */
static void process_fail(int code, const char *what) {
#ifdef _WIN32
    char msg[128];
    snprintf(msg, sizeof msg, "%s: Windows error %d", what, code);
    kn_error_set((int32_t)code, msg);
#else
    kn_error_set_errno(code, what);
#endif
}

/* --- small helpers ---------------------------------------------------- */

static const char *process_nz(const char *s) { return s ? s : ""; }

/* Text results are runtime-owned like every other text result, so even the ""
 * failure sentinel is a fresh runtime allocation. */
static char *process_empty_text(void) {
    char *s = (char *)kn_malloc(1);
    if (s) s[0] = '\0';
    return s;
}

static char *process_dup_text(const char *s, long n) {
    char *o = (char *)kn_malloc(n + 1);
    if (!o) return process_empty_text();
    memcpy(o, s, (size_t)n);
    o[n] = '\0';
    return o;
}

#ifndef _WIN32
/* A wait status turned into the number a shell would report. */
static int32_t process_status_of(int wstatus) {
    if (WIFEXITED(wstatus)) return (int32_t)WEXITSTATUS(wstatus);
    if (WIFSIGNALED(wstatus)) return (int32_t)(128 + WTERMSIG(wstatus));
    return -1;
}
#endif

/* Writing to a child that has closed its input raises SIGPIPE, which would
 * kill the *parent* — a program should get `false` and an error code instead.
 * Disposition is process-wide, so it is set once, lazily, when this library
 * first spawns anything. */
static void process_ignore_sigpipe(void) {
#ifdef _WIN32
    /* Nothing to do: Windows has no SIGPIPE.  A write to a pipe whose reader
     * has gone returns an error, which is what this call buys on POSIX. */
#else
    static int done = 0;
    if (!done) { signal(SIGPIPE, SIG_IGN); done = 1; }
#endif
}

#ifdef _WIN32
/* getline is POSIX; this is the same contract — grow the caller's buffer, keep
 * the newline, return -1 at end of input with nothing read — so the one reader
 * below needs no branch of its own. */
static ssize_t process_getline(char **line, size_t *cap, FILE *f) {
    size_t len = 0;
    for (;;) {
        if (len + 2 > *cap) {
            size_t want = *cap ? *cap * 2 : 128;
            char *nb = (char *)realloc(*line, want);
            if (!nb) return -1;
            *line = nb;
            *cap = want;
        }
        int c = getc(f);
        if (c == EOF) break;
        (*line)[len++] = (char)c;
        if (c == '\n') break;
    }
    if (len == 0) return -1;
    (*line)[len] = '\0';
    return (ssize_t)len;
}
#define getline process_getline
#endif

/* A millisecond, for the escalation loops in the close function. */
static void process_nap(void) {
#ifdef _WIN32
    Sleep(1);
#else
    struct timespec ts = { 0, 1000000 };
    nanosleep(&ts, NULL);
#endif
}

/* --- the child --------------------------------------------------------- */

typedef struct {
    process_pid_t pid;
    int   in_fd;     /* write end of the child's stdin, -1 once closed  */
    FILE *out;       /* buffered read end of the child's stdout          */
    int   reaped;    /* the child has been waited for; `status` is real  */
    int32_t status;
    int   at_end;    /* stdout reached end of input                      */
} Proc;

/* Plain malloc, not kn_malloc: this is library bookkeeping, not program data,
 * and it must stay valid while the handle table's close functions run at exit,
 * after program data has been freed. */

/* Spawn `command` under /bin/sh.
 *
 * `in_fd`/`out_fd` are optional: pass NULL for a stream the child should
 * inherit from us instead of having piped.  Returns the pid, or -1 with
 * *err_out set to the errno that explains why it never ran. */
#ifdef _WIN32
static process_pid_t process_spawn(const char *command, int *in_fd, int *out_fd, int *err_out) {
    HANDLE in_r = NULL, in_w = NULL, out_r = NULL, out_w = NULL;
    *err_out = 0;

    /* The child inherits the pipe ends it reads and writes; OUR ends must not
     * be inheritable, or the child would hold them open and the reader would
     * never see end of input. */
    SECURITY_ATTRIBUTES sa;
    sa.nLength = sizeof sa;
    sa.lpSecurityDescriptor = NULL;
    sa.bInheritHandle = TRUE;

    if (in_fd && (!CreatePipe(&in_r, &in_w, &sa, 0) ||
                  !SetHandleInformation(in_w, HANDLE_FLAG_INHERIT, 0))) {
        *err_out = (int)GetLastError();
        if (in_r) CloseHandle(in_r);
        if (in_w) CloseHandle(in_w);
        return PROCESS_NO_PID;
    }
    if (out_fd && (!CreatePipe(&out_r, &out_w, &sa, 0) ||
                   !SetHandleInformation(out_r, HANDLE_FLAG_INHERIT, 0))) {
        *err_out = (int)GetLastError();
        if (in_r) CloseHandle(in_r);
        if (in_w) CloseHandle(in_w);
        if (out_r) CloseHandle(out_r);
        if (out_w) CloseHandle(out_w);
        return PROCESS_NO_PID;
    }

    /* cmd.exe /c is the Windows spelling of /bin/sh -c, so a pipeline and a
     * redirection work here for the same reason they work there. */
    const char *shell = getenv("COMSPEC");
    if (!shell || !*shell) shell = "cmd.exe";
    size_t n = strlen(shell) + strlen(command) + 8;
    char *line = (char *)malloc(n);
    if (!line) {
        *err_out = (int)ERROR_NOT_ENOUGH_MEMORY;
        if (in_r) CloseHandle(in_r);
        if (in_w) CloseHandle(in_w);
        if (out_r) CloseHandle(out_r);
        if (out_w) CloseHandle(out_w);
        return PROCESS_NO_PID;
    }
    snprintf(line, n, "%s /c %s", shell, command);

    STARTUPINFOA si;
    PROCESS_INFORMATION pi;
    memset(&si, 0, sizeof si);
    memset(&pi, 0, sizeof pi);
    si.cb = sizeof si;
    si.dwFlags = STARTF_USESTDHANDLES;
    si.hStdInput  = in_fd  ? in_r : GetStdHandle(STD_INPUT_HANDLE);
    si.hStdOutput = out_fd ? out_w : GetStdHandle(STD_OUTPUT_HANDLE);
    si.hStdError  = GetStdHandle(STD_ERROR_HANDLE);

    BOOL ok = CreateProcessA(NULL, line, NULL, NULL, TRUE, 0, NULL, NULL, &si, &pi);
    int code = (int)GetLastError();
    free(line);
    /* The child has its own copies now. */
    if (in_r) CloseHandle(in_r);
    if (out_w) CloseHandle(out_w);
    if (!ok) {
        if (in_w) CloseHandle(in_w);
        if (out_r) CloseHandle(out_r);
        *err_out = code;
        return PROCESS_NO_PID;
    }
    CloseHandle(pi.hThread);

    /* Hand the pipe ends to the CRT so the fd-shaped code below is unchanged;
     * closing the fd from then on closes the HANDLE with it. */
    if (in_fd)  *in_fd  = _open_osfhandle((intptr_t)in_w, _O_WRONLY);
    if (out_fd) *out_fd = _open_osfhandle((intptr_t)out_r, _O_RDONLY | _O_BINARY);
    return pi.hProcess;
}

/* 1 = it finished and *status is real, 0 = still running, -1 = failure. */
static int process_poll(process_pid_t pid, int32_t *status) {
    DWORD w = WaitForSingleObject(pid, 0);
    if (w == WAIT_TIMEOUT) return 0;
    if (w != WAIT_OBJECT_0) return -1;
    DWORD code = 1;
    if (!GetExitCodeProcess(pid, &code)) return -1;
    *status = (int32_t)code;
    return 1;
}

/* Wait for a child to finish.  1 on success. */
static int process_reap(process_pid_t pid, int32_t *status) {
    if (WaitForSingleObject(pid, INFINITE) != WAIT_OBJECT_0) return 0;
    DWORD code = 1;
    if (!GetExitCodeProcess(pid, &code)) return 0;
    *status = (int32_t)code;
    return 1;
}

/* Windows has no signals to send, so "stop it now" is the one blunt verb.
 * 137 is what a shell reports for a SIGKILL, and this is that same event. */
static int process_terminate(process_pid_t pid) {
    return TerminateProcess(pid, 137) ? 1 : 0;
}

/* A pid needs no releasing; a HANDLE does. */
static void process_release(process_pid_t pid) { CloseHandle(pid); }
#else
static process_pid_t process_spawn(const char *command, int *in_fd, int *out_fd, int *err_out) {
    int inp[2] = { -1, -1 }, outp[2] = { -1, -1 }, errp[2] = { -1, -1 };
    *err_out = 0;

    process_ignore_sigpipe();

    if (pipe(errp) != 0) { *err_out = errno; return -1; }
    /* Close-on-exec: a successful exec closes it, and the parent's read
     * returning 0 bytes is precisely "the program started". */
    if (fcntl(errp[1], F_SETFD, FD_CLOEXEC) != 0) {
        int e = errno;
        close(errp[0]); close(errp[1]);
        *err_out = e;
        return -1;
    }
    if (in_fd && pipe(inp) != 0) {
        int e = errno;
        close(errp[0]); close(errp[1]);
        *err_out = e;
        return -1;
    }
    if (out_fd && pipe(outp) != 0) {
        int e = errno;
        close(errp[0]); close(errp[1]);
        if (in_fd) { close(inp[0]); close(inp[1]); }
        *err_out = e;
        return -1;
    }

    pid_t pid = fork();
    if (pid < 0) {
        int e = errno;
        close(errp[0]); close(errp[1]);
        if (in_fd)  { close(inp[0]);  close(inp[1]); }
        if (out_fd) { close(outp[0]); close(outp[1]); }
        *err_out = e;
        return -1;
    }

    if (pid == 0) {                              /* --- child --- */
        close(errp[0]);
        if (in_fd)  { close(inp[1]);  dup2(inp[0], STDIN_FILENO);   close(inp[0]); }
        if (out_fd) { close(outp[0]); dup2(outp[1], STDOUT_FILENO); close(outp[1]); }
        execl("/bin/sh", "sh", "-c", command, (char *)NULL);
        {
            int e = errno;
            ssize_t ignored = write(errp[1], &e, sizeof e);
            (void)ignored;
        }
        _exit(127);
    }

    /* --- parent --- */
    close(errp[1]);
    if (in_fd)  close(inp[0]);
    if (out_fd) close(outp[1]);

    int child_err = 0;
    ssize_t got;
    do { got = read(errp[0], &child_err, sizeof child_err); }
    while (got < 0 && errno == EINTR);
    close(errp[0]);
    if (got == (ssize_t)sizeof child_err && child_err != 0) {
        /* It never ran.  Reap the shell corpse so it is not a zombie. */
        int st;
        while (waitpid(pid, &st, 0) < 0 && errno == EINTR) { }
        if (in_fd)  close(inp[1]);
        if (out_fd) close(outp[0]);
        *err_out = child_err;
        return -1;
    }

    if (in_fd)  *in_fd  = inp[1];
    if (out_fd) *out_fd = outp[0];
    return pid;
}

/* 1 = it finished and *status is real, 0 = still running, -1 = failure. */
static int process_poll(process_pid_t pid, int32_t *status) {
    int st = 0;
    pid_t r;
    do { r = waitpid(pid, &st, WNOHANG); } while (r < 0 && errno == EINTR);
    if (r == 0) return 0;
    if (r < 0) return -1;
    *status = process_status_of(st);
    return 1;
}

/* Wait for a child, restarting across signals.  1 on success. */
static int process_reap(process_pid_t pid, int32_t *status) {
    int st = 0;
    pid_t r;
    do { r = waitpid(pid, &st, 0); } while (r < 0 && errno == EINTR);
    if (r != pid) return 0;
    *status = process_status_of(st);
    return 1;
}

static int process_terminate(process_pid_t pid) {
    if (kill(pid, SIGKILL) == 0) return 1;
    return errno == ESRCH;               /* already gone is a success */
}

static void process_release(process_pid_t pid) { (void)pid; }
#endif

/* The close function handed to kn_handle_new: it runs on process_close AND on
 * exit cleanup, which is what keeps a forgetful program from accumulating
 * zombies.  Closing the child's stdin first gives a well-behaved child its
 * cue to finish; only then do we insist. */
static void process_close_fn(void *payload) {
    Proc *p = (Proc *)payload;
    if (!p) return;

    if (p->in_fd >= 0) { close(p->in_fd); p->in_fd = -1; }
    if (p->out) { fclose(p->out); p->out = NULL; }

    if (!p->reaped) {
        int32_t st = -1;
        /* Give it a moment to notice the closed input, then escalate. */
        for (int i = 0; i < 100; i++) {           /* up to ~100ms */
            int r = process_poll(p->pid, &st);
            if (r == 1) { p->reaped = 1; p->status = st; break; }
            if (r < 0) { p->reaped = 1; break; }  /* already gone */
            process_nap();
        }
        if (!p->reaped) {
            process_terminate(p->pid);
            for (int i = 0; i < 100; i++) {
                int r = process_poll(p->pid, &st);
                if (r == 1) { p->reaped = 1; p->status = st; break; }
                if (r < 0) { p->reaped = 1; break; }
                process_nap();
            }
        }
        if (!p->reaped) {
            process_terminate(p->pid);
            if (process_reap(p->pid, &st)) p->status = st;
            p->reaped = 1;
        }
    }
    process_release(p->pid);
    free(p);
}

/* Resolve a handle; the handle table sets the error slot on every failure. */
static Proc *process_get(int32_t h) {
    return (Proc *)kn_handle_resolve(h, KN_HK_PROC);
}

/* --- run to completion ------------------------------------------------- */

/* process_run(text command) -> int
 * The command's exit status (128+signal if it was killed), or -1 if it could
 * not be started at all.  stdin/stdout are this program's own.
 *
 * Every command here runs under /bin/sh, so a pipeline, a redirection and a
 * loop all work.  That also fixes what "could not start" means: the shell is
 * the program being started, so a *missing* program inside the command is the
 * shell's honest exit 127 — the same answer a terminal gives — while -1 with
 * the error slot set means the spawn itself failed and nothing ran at all. */
void process_run(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *cmd = process_nz(kn_arg_text(argv, 0));
    int err = 0;
    process_pid_t pid = process_spawn(cmd, NULL, NULL, &err);
    if (pid == PROCESS_NO_PID) {
        process_fail(err, "run");
        kn_ret_int(ret, -1);
        return;
    }
    int32_t st = -1;
    if (!process_reap(pid, &st)) {
        int e = errno;
        process_release(pid);
        kn_error_set_errno(e, "wait");
        kn_ret_int(ret, -1);
        return;
    }
    process_release(pid);
    kn_error_clear();
    kn_ret_int(ret, st);
}

/* process_run_capture(text command) -> text
 * Everything the command wrote to stdout, "" if it could not be started.  An
 * empty result with error code 0 means it ran and said nothing. */
void process_run_capture(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *cmd = process_nz(kn_arg_text(argv, 0));
    int err = 0, out_fd = -1;
    process_pid_t pid = process_spawn(cmd, NULL, &out_fd, &err);
    if (pid == PROCESS_NO_PID) {
        process_fail(err, "run");
        kn_ret_text(ret, process_empty_text());
        return;
    }

    char *buf = NULL;
    size_t len = 0, cap = 0;
    char chunk[4096];
    ssize_t n;
    int read_err = 0;
    while ((n = read(out_fd, chunk, sizeof chunk)) != 0) {
        if (n < 0) {
            if (errno == EINTR) continue;
            read_err = errno;
            break;
        }
        if (len + (size_t)n + 1 > cap) {
            size_t want = cap ? cap * 2 : 8192;
            while (want < len + (size_t)n + 1) want *= 2;
            char *nb = (char *)realloc(buf, want);
            if (!nb) { read_err = ENOMEM; break; }
            buf = nb; cap = want;
        }
        memcpy(buf + len, chunk, (size_t)n);
        len += (size_t)n;
    }
    close(out_fd);

    int32_t st = -1;
    process_reap(pid, &st);              /* reap regardless: no zombies */
    process_release(pid);

    if (read_err) {
        free(buf);
        kn_error_set_errno(read_err, "read");
        kn_ret_text(ret, process_empty_text());
        return;
    }
    char *out = process_dup_text(buf ? buf : "", (long)len);
    free(buf);
    kn_error_clear();                    /* a non-zero exit is not a failure */
    kn_ret_text(ret, out);
}

/* --- a child kept alive behind a handle -------------------------------- */

/* process_start(text command) -> int handle, 0 if it could not be started. */
void process_start(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *cmd = process_nz(kn_arg_text(argv, 0));
    int err = 0, in_fd = -1, out_fd = -1;
    process_pid_t pid = process_spawn(cmd, &in_fd, &out_fd, &err);
    if (pid == PROCESS_NO_PID) {
        process_fail(err, "start");
        kn_ret_int(ret, 0);
        return;
    }
    FILE *out = fdopen(out_fd, "r");
    if (!out) {
        int e = errno;
        close(in_fd); close(out_fd);
        process_terminate(pid);
        { int32_t st; process_reap(pid, &st); }
        process_release(pid);
        kn_error_set_errno(e, "start");
        kn_ret_int(ret, 0);
        return;
    }
    Proc *p = (Proc *)malloc(sizeof *p);
    if (!p) {
        fclose(out); close(in_fd);
        process_terminate(pid);
        { int32_t st; process_reap(pid, &st); }
        process_release(pid);
        kn_error_set(KN_ERR_TABLE_FULL, "out of memory");
        kn_ret_int(ret, 0);
        return;
    }
    p->pid = pid; p->in_fd = in_fd; p->out = out;
    p->reaped = 0; p->status = -1; p->at_end = 0;

    int32_t h = kn_handle_new(KN_HK_PROC, p, process_close_fn);
    if (h == 0) {                        /* the table set the slot itself */
        process_close_fn(p);
        kn_ret_int(ret, 0);
        return;
    }
    kn_error_clear();
    kn_ret_int(ret, h);
}

/* process_read_line(int h) -> text
 * One line of the child's output, without its newline.  "" at end of input —
 * which is why process_at_end sits beside it: a blank line the child printed
 * and no line at all are otherwise the same text. */
void process_read_line(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    Proc *p = process_get(kn_arg_int(argv, 0));
    if (!p) { kn_ret_text(ret, process_empty_text()); return; }
    if (!p->out) {
        kn_error_set(KN_ERR_INVALID_ARG, "output is closed");
        kn_ret_text(ret, process_empty_text());
        return;
    }

    char *line = NULL;
    size_t cap = 0;
    errno = 0;
    ssize_t n = getline(&line, &cap, p->out);
    int e = errno;
    if (n < 0) {
        free(line);
        if (ferror(p->out)) {
            kn_error_set_errno(e ? e : EIO, "read");
            kn_ret_text(ret, process_empty_text());
            return;
        }
        p->at_end = 1;
        kn_error_clear();                /* end of input is not a failure */
        kn_ret_text(ret, process_empty_text());
        return;
    }
    if (n > 0 && line[n - 1] == '\n') n--;
    if (n > 0 && line[n - 1] == '\r') n--;
    char *out = process_dup_text(line, (long)n);
    free(line);
    kn_error_clear();
    kn_ret_text(ret, out);
}

/* process_at_end(int h) -> bool : the child's output has ended. */
void process_at_end(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    Proc *p = process_get(kn_arg_int(argv, 0));
    if (!p) { kn_ret_bool(ret, 0); return; }
    kn_error_clear();
    kn_ret_bool(ret, p->at_end || !p->out);
}

/* process_write_line(int h, text line) -> bool
 * Sends the line and a newline to the child's input.  false with a non-zero
 * error code is a failure; the child having closed its input is EPIPE. */
void process_write_line(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    Proc *p = process_get(kn_arg_int(argv, 0));
    if (!p) { kn_ret_bool(ret, 0); return; }
    if (p->in_fd < 0) {
        kn_error_set(KN_ERR_INVALID_ARG, "input is closed");
        kn_ret_bool(ret, 0);
        return;
    }
    const char *s = process_nz(kn_arg_text(argv, 1));
    size_t len = strlen(s);

    /* One buffer so the line and its newline cannot be split by a short
     * write, which a line-oriented child would read as two lines. */
    char *buf = (char *)malloc(len + 1);
    if (!buf) { kn_error_set(KN_ERR_TABLE_FULL, "out of memory"); kn_ret_bool(ret, 0); return; }
    memcpy(buf, s, len);
    buf[len] = '\n';

    size_t off = 0;
    while (off < len + 1) {
        ssize_t w = write(p->in_fd, buf + off, len + 1 - off);
        int e = errno;
        if (w < 0) {
            if (e == EINTR) continue;
            free(buf);
            kn_error_set_errno(e, "write");
            kn_ret_bool(ret, 0);
            return;
        }
        off += (size_t)w;
    }
    free(buf);
    kn_error_clear();
    kn_ret_bool(ret, 1);
}

/* process_is_running(int h) -> bool
 * Checking also reaps a child that has finished, so polling this in a loop
 * never leaves a zombie behind. */
void process_is_running(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    Proc *p = process_get(kn_arg_int(argv, 0));
    if (!p) { kn_ret_bool(ret, 0); return; }
    if (p->reaped) { kn_error_clear(); kn_ret_bool(ret, 0); return; }

    int32_t st = -1;
    int r = process_poll(p->pid, &st);
    int e = errno;
    if (r == 0) { kn_error_clear(); kn_ret_bool(ret, 1); return; }
    if (r < 0) { kn_error_set_errno(e, "wait"); kn_ret_bool(ret, 0); return; }
    p->reaped = 1;
    p->status = st;
    kn_error_clear();
    kn_ret_bool(ret, 0);
}

/* process_wait(int h) -> int
 * The child's exit status, -1 if it could not be waited for.  The child's
 * input is closed first: a child still reading from us would otherwise never
 * finish, and the program would hang in a command that promises to return. */
void process_wait(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    Proc *p = process_get(kn_arg_int(argv, 0));
    if (!p) { kn_ret_int(ret, -1); return; }
    if (p->reaped) { kn_error_clear(); kn_ret_int(ret, p->status); return; }

    if (p->in_fd >= 0) { close(p->in_fd); p->in_fd = -1; }
    int32_t st = -1;
    if (!process_reap(p->pid, &st)) {
        int e = errno;
        kn_error_set_errno(e, "wait");
        kn_ret_int(ret, -1);
        return;
    }
    p->reaped = 1;
    p->status = st;
    kn_error_clear();
    kn_ret_int(ret, p->status);
}

/* process_kill(int h) -> bool : stop the child now (SIGKILL) and reap it.
 * Killing an already-finished child is a success, not a failure — the caller
 * asked for it to be gone, and it is. */
void process_kill(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    Proc *p = process_get(kn_arg_int(argv, 0));
    if (!p) { kn_ret_bool(ret, 0); return; }
    if (p->reaped) { kn_error_clear(); kn_ret_bool(ret, 1); return; }

    if (!process_terminate(p->pid)) {
        int e = errno;
        kn_error_set_errno(e ? e : EPERM, "kill");
        kn_ret_bool(ret, 0);
        return;
    }
    int32_t st = -1;
    if (process_reap(p->pid, &st)) p->status = st;
    p->reaped = 1;
    kn_error_clear();
    kn_ret_bool(ret, 1);
}

/* process_close(int h) -> bool : release the handle, ending the child if it
 * is still running. */
void process_close(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    int32_t h = kn_arg_int(argv, 0);
    if (kn_handle_close(h, KN_HK_PROC)) { kn_error_clear(); kn_ret_bool(ret, 1); return; }
    kn_ret_bool(ret, 0);                 /* the handle table set the slot */
}

/* process_close_all() -> int : how many children were closed. */
void process_close_all(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; (void)argv;
    int32_t n = kn_handle_close_kind(KN_HK_PROC);
    kn_error_clear();
    kn_ret_int(ret, n);
}
