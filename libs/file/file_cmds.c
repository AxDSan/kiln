/* The "file" support library — files, directories and paths.
 *
 * Three surfaces, in order of how often they are the right one:
 *
 *   file_*  path-level, one shot.  Read a file, write a file, ask how big it
 *           is.  No resource is held, so nothing can be leaked or left open.
 *   file_open/...  a handle, for the file that does not fit in memory.  The
 *           program is handed a small positive int, never an address, and the
 *           handle table closes anything it still holds at exit.
 *   dir_*   directories.  A listing is a count plus an indexed accessor,
 *           because the language has no arrays.
 *   path_*  pure text.  These never touch the filesystem and cannot fail, so
 *           they never touch the error slot either.
 *
 * TEXT IS A C STRING.  file_read_text stops at an embedded NUL, so a file with
 * one in it reads short.  That is honest for text and wrong for a PNG, which
 * is why file_read_bytes / file_write_bytes / file_append_bytes exist beside
 * them: the byte-set carries its own length and so survives a NUL.
 *
 * Every fallible command below takes exactly one of kn_error_clear() or
 * kn_error_set*() on every exit path, and copies errno to a local on the line
 * immediately after the call that failed — fclose() and free() clobber it.
 */
#include <errno.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>

/* Windows has no dirent and no unistd, and its separator is the backslash.
 * Directory listing goes through FindFirstFileW; everything else has a narrow
 * C-library spelling with a leading underscore.
 *
 * PATHS CROSS THE BOUNDARY AS UTF-8.  The wide entry points are used and the
 * results converted, because the ANSI ones go through the machine's codepage
 * and would mangle any name outside it — a path is exactly the kind of text
 * that has an accent in it. */
#ifdef _WIN32
#include <windows.h>
#include <direct.h>
#include <io.h>
#else
#include <dirent.h>
#include <unistd.h>
#endif

/* kiln_core.h rather than the ABI header alone, for kn_bin_new: a byte-set
 * is allocated by the runtime, and its constructor is a runtime internal the
 * same way kn_empty_text is. */
#include "kiln_core.h"

/* --- small helpers ---------------------------------------------------- */

static const char *file_nz(const char *s) { return s ? s : ""; }

/* Windows accepts both separators and programs mix them freely, so every path
 * command asks this rather than comparing against '/' — one place decides what
 * a separator is, and the path family agrees with itself. */
static int file_is_sep(char c) {
#ifdef _WIN32
    return c == '/' || c == '\\';
#else
    return c == '/';
#endif
}

/* How much of a path is its root and can never be walked off the front of:
 * "/" on POSIX, and "C:\\" or "\\\\server\\share\\" on Windows.  0 for a relative
 * path.  path_parent and path_absolute both need it, and getting it wrong is
 * how ".." escapes a drive. */
static size_t file_root_len(const char *p) {
#ifdef _WIN32
    if (file_is_sep(p[0]) && file_is_sep(p[1])) {
        /* \\server\share — the share is part of the root, not a component. */
        size_t i = 2;
        for (int part = 0; part < 2 && p[i]; part++) {
            while (p[i] && !file_is_sep(p[i])) i++;
            while (file_is_sep(p[i])) i++;
        }
        return i;
    }
    if (p[0] && p[1] == ':') return file_is_sep(p[2]) ? 3 : 2;
    return file_is_sep(p[0]) ? 1 : 0;
#else
    return file_is_sep(p[0]) ? 1 : 0;
#endif
}

/* --- the platform shim ------------------------------------------------
 * Every filesystem call in this file goes through one of these, so the rest of
 * the library reads the same on both platforms and there is exactly one place
 * that knows a Windows path is UTF-16.  Each wide wrapper leaves errno set the
 * way its POSIX twin does, so the error handling above them needs no branch. */
#ifdef _WIN32
#define FILE_SEP '\\'

/* UTF-8 in, a wide path out in a caller-owned buffer.  Plain malloc: this is
 * bookkeeping for one call, not program data the runtime must free at exit. */
static wchar_t *file_to_wide(const char *s) {
    int n = MultiByteToWideChar(CP_UTF8, 0, s, -1, NULL, 0);
    if (n <= 0) return NULL;
    wchar_t *w = (wchar_t *)malloc((size_t)n * sizeof(wchar_t));
    if (!w) return NULL;
    if (MultiByteToWideChar(CP_UTF8, 0, s, -1, w, n) <= 0) { free(w); return NULL; }
    return w;
}

/* The other direction, into runtime-owned text — a path handed back to the
 * program is program data. */
static char *file_from_wide(const wchar_t *w) {
    int n = WideCharToMultiByte(CP_UTF8, 0, w, -1, NULL, 0, NULL, NULL);
    if (n <= 0) return NULL;
    char *o = (char *)kn_malloc(n);
    if (!o) return NULL;
    if (WideCharToMultiByte(CP_UTF8, 0, w, -1, o, n, NULL, NULL) <= 0) return NULL;
    return o;
}

typedef struct _stat64 file_stat_t;
#define FILE_ISDIR(st) (((st).st_mode & _S_IFMT) == _S_IFDIR)
#define FILE_ISREG(st) (((st).st_mode & _S_IFMT) == _S_IFREG)

/* One shape for all of them: widen, call, restore errno across free(). */
#define FILE_WIDE_1(name, call, fail)                                          \
    static int name(const char *p) {                                           \
        wchar_t *w = file_to_wide(p);                                          \
        if (!w) { errno = EINVAL; return (fail); }                             \
        int rc = (call);                                                       \
        int e = errno;                                                         \
        free(w);                                                               \
        errno = e;                                                             \
        return rc;                                                             \
    }
FILE_WIDE_1(file_unlink, _wunlink(w), -1)
FILE_WIDE_1(file_rmdir,  _wrmdir(w),  -1)
FILE_WIDE_1(file_chdir,  _wchdir(w),  -1)
FILE_WIDE_1(file_mkdir,  _wmkdir(w),  -1)
#undef FILE_WIDE_1

static int file_stat(const char *p, file_stat_t *st) {
    wchar_t *w = file_to_wide(p);
    if (!w) { errno = EINVAL; return -1; }
    int rc = _wstat64(w, st);
    int e = errno;
    free(w);
    errno = e;
    return rc;
}

static FILE *file_fopen(const char *p, const char *mode) {
    wchar_t *w = file_to_wide(p);
    if (!w) { errno = EINVAL; return NULL; }
    wchar_t wm[8];
    size_t i = 0;
    for (; mode[i] && i < 7; i++) wm[i] = (wchar_t)mode[i];
    wm[i] = L'\0';
    FILE *f = _wfopen(w, wm);
    int e = errno;
    free(w);
    errno = e;
    return f;
}

static int file_getcwd_buf(char *buf, size_t cap) {
    wchar_t w[4096];
    if (!_wgetcwd(w, (int)(sizeof w / sizeof w[0]))) return 0;
    return WideCharToMultiByte(CP_UTF8, 0, w, -1, buf, (int)cap, NULL, NULL) > 0;
}
#else
#define FILE_SEP '/'
typedef struct stat file_stat_t;
#define FILE_ISDIR(st) (S_ISDIR((st).st_mode))
#define FILE_ISREG(st) (S_ISREG((st).st_mode))
static int file_unlink(const char *p) { return unlink(p); }
static int file_rmdir(const char *p)  { return rmdir(p); }
static int file_chdir(const char *p)  { return chdir(p); }
static int file_mkdir(const char *p)  { return mkdir(p, 0777); }
static int file_stat(const char *p, file_stat_t *st) { return stat(p, st); }
static FILE *file_fopen(const char *p, const char *mode) { return fopen(p, mode); }
static int file_getcwd_buf(char *buf, size_t cap) {
    return getcwd(buf, cap) != NULL;
}
#endif

static char *file_getcwd_text(void) {
    char buf[4096];
    if (!file_getcwd_buf(buf, sizeof buf)) return NULL;
    size_t n = strlen(buf);
    char *o = (char *)kn_malloc((long)n + 1);
    if (o) memcpy(o, buf, n + 1);
    return o;
}

/* A result string, runtime-owned like every other text result. */
static char *file_text_n(const char *s, size_t n) {
    char *o = (char *)kn_malloc((long)n + 1);
    if (!o) return NULL;
    if (n) memcpy(o, s, n);
    o[n] = '\0';
    return o;
}
static char *file_text(const char *s) { return file_text_n(s, strlen(file_nz(s))); }
/* The "" failure sentinel: a fresh empty string, so ownership is uniform. */
static char *file_empty(void) { return file_text_n("", 0); }

/* --- byte-sets --------------------------------------------------------
 * The layout is stated in abi/kiln_abi.h: a header, then the bytes, all one
 * runtime-owned allocation.  A bytes result that failed is an EMPTY byte-set
 * rather than NULL, the exact analog of the "" a failed text result returns —
 * a caller that ignores the error slot still holds something it can measure. */

static unsigned char *file_bin_at(Kiln_Bin *b) { return (unsigned char *)(b + 1); }
static int32_t file_bin_len(const Kiln_Bin *b) { return b ? b->len : 0; }

static void file_ret_bin(Kiln_Slot *ret, void *b) {
    ret->tag = KN_SDT_BIN;
    ret->v.ptr = b;
}
static void file_ret_bin_empty(Kiln_Slot *ret) { file_ret_bin(ret, kn_bin_new(0)); }

/* --- one-shot file commands ------------------------------------------- */

/* file_read_text(path) -> text : the whole file, "" on failure.  Read in
 * chunks rather than by seeking to the end, so a pipe or a /proc file — which
 * report no size — read correctly too. */
void file_read_text(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *path = file_nz(kn_arg_text(argv, 0));
    FILE *f = file_fopen(path, "rb");
    int e = errno;
    if (!f) { kn_error_set_errno(e, "open"); kn_ret_text(ret, file_empty()); return; }

    long cap = 4096, len = 0;
    char *buf = (char *)kn_malloc(cap);
    if (!buf) { fclose(f); kn_error_set(KN_ERR_UNSUPPORTED, "out of memory"); kn_ret_text(ret, NULL); return; }
    for (;;) {
        size_t got = fread(buf + len, 1, (size_t)(cap - len - 1), f);
        len += (long)got;
        if (got == 0) break;
        if (len + 1 >= cap) {
            char *nb = (char *)kn_mrealloc(buf, cap * 2);
            if (!nb) break;
            buf = nb;
            cap *= 2;
        }
    }
    int bad = ferror(f);
    e = errno;
    fclose(f);
    if (bad) { kn_error_set_errno(e, "read"); kn_ret_text(ret, file_empty()); return; }
    buf[len] = '\0';
    kn_error_clear();
    kn_ret_text(ret, buf);
}

/* Shared by file_write_text and file_append_text. */
static void file_put(Kiln_Slot *ret, Kiln_Slot *argv, const char *mode, const char *what) {
    const char *path = file_nz(kn_arg_text(argv, 0));
    const char *body = file_nz(kn_arg_text(argv, 1));
    FILE *f = file_fopen(path, mode);
    int e = errno;
    if (!f) { kn_error_set_errno(e, what); kn_ret_bool(ret, 0); return; }
    size_t n = strlen(body);
    int err = 0;
    if (n && fwrite(body, 1, n, f) != n) { err = errno ? errno : EIO; }
    /* fclose flushes, so a short write can surface here and nowhere earlier. */
    if (fclose(f) != 0 && !err) { err = errno ? errno : EIO; }
    if (err) { kn_error_set_errno(err, "write"); kn_ret_bool(ret, 0); return; }
    kn_error_clear();
    kn_ret_bool(ret, 1);
}

/* file_write_text(path, content) -> bool : replaces the file. */
void file_write_text(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; file_put(ret, argv, "wb", "create");
}
/* file_append_text(path, content) -> bool : adds to the end, creating it. */
void file_append_text(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; file_put(ret, argv, "ab", "open for append");
}

/* file_read_bytes(path) -> bytes : the whole file, empty on failure.  This is
 * what file_read_text cannot be: a PNG is full of NULs and a C string ends at
 * the first one.
 *
 * The byte-set itself is what grows, with its header reserved at the front, so
 * the file is never held twice — a 200MB image read into a buffer and then
 * copied into a byte-set would peak at 400MB for no reason.  Read in chunks
 * rather than seeking to the end, so a pipe or a /proc file reads correctly. */
void file_read_bytes(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *path = file_nz(kn_arg_text(argv, 0));
    FILE *f = file_fopen(path, "rb");
    int e = errno;
    if (!f) { kn_error_set_errno(e, "open"); file_ret_bin_empty(ret); return; }

    long cap = 8192, len = 0;
    Kiln_Bin *b = (Kiln_Bin *)kn_bin_new((int32_t)cap);
    if (!b) { fclose(f); kn_error_set(KN_ERR_UNSUPPORTED, "out of memory"); file_ret_bin_empty(ret); return; }
    for (;;) {
        size_t got = fread(file_bin_at(b) + len, 1, (size_t)(cap - len), f);
        len += (long)got;
        if (len < cap) break;                      /* short read: end, or error */
        /* A byte-set counts its length in an int32, so a file at or past 2GB
         * has no honest answer here — refuse it rather than wrap the header. */
        if (cap > (long)INT32_MAX / 2) {
            fclose(f);
            kn_error_set(KN_ERR_OUT_OF_RANGE, "file is too large to hold in one byte-set");
            file_ret_bin_empty(ret);
            return;
        }
        Kiln_Bin *nb = (Kiln_Bin *)kn_mrealloc(b, (long)sizeof(Kiln_Bin) + cap * 2);
        if (!nb) { fclose(f); kn_error_set(KN_ERR_UNSUPPORTED, "out of memory"); file_ret_bin_empty(ret); return; }
        b = nb;
        cap *= 2;
    }
    int bad = ferror(f);
    e = errno;
    fclose(f);
    if (bad) { kn_error_set_errno(e, "read"); file_ret_bin_empty(ret); return; }
    b->dims = 1;
    b->len = (int32_t)len;                 /* the tail of the allocation is slack */
    kn_error_clear();
    file_ret_bin(ret, b);
}

/* Shared by file_write_bytes and file_append_bytes. */
static void file_put_bytes(Kiln_Slot *ret, Kiln_Slot *argv, const char *mode, const char *what) {
    const char *path = file_nz(kn_arg_text(argv, 0));
    Kiln_Bin *b = (Kiln_Bin *)argv[1].v.ptr;
    int32_t n = file_bin_len(b);
    FILE *f = file_fopen(path, mode);
    int e = errno;
    if (!f) { kn_error_set_errno(e, what); kn_ret_bool(ret, 0); return; }
    int err = 0;
    if (n && fwrite(file_bin_at(b), 1, (size_t)n, f) != (size_t)n) { err = errno ? errno : EIO; }
    if (fclose(f) != 0 && !err) { err = errno ? errno : EIO; }
    if (err) { kn_error_set_errno(err, "write"); kn_ret_bool(ret, 0); return; }
    kn_error_clear();
    kn_ret_bool(ret, 1);
}

/* file_write_bytes(path, content) -> bool : replaces the file. */
void file_write_bytes(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; file_put_bytes(ret, argv, "wb", "create");
}
/* file_append_bytes(path, content) -> bool : adds to the end, creating it. */
void file_append_bytes(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; file_put_bytes(ret, argv, "ab", "open for append");
}

/* "the path is not there" is an answer, not a failure: those errno values
 * clear the slot and return false, so `false, code 0` means a genuine no. */
static int file_absent(int e) { return e == ENOENT || e == ENOTDIR; }

static void file_stat_is(Kiln_Slot *ret, Kiln_Slot *argv, int want_dir) {
    const char *path = file_nz(kn_arg_text(argv, 0));
    file_stat_t st;
    int rc = file_stat(path, &st);
    int e = errno;
    if (rc != 0) {
        if (file_absent(e)) { kn_error_clear(); kn_ret_bool(ret, 0); return; }
        kn_error_set_errno(e, "stat");
        kn_ret_bool(ret, 0);
        return;
    }
    kn_error_clear();
    kn_ret_bool(ret, want_dir ? FILE_ISDIR(st) != 0 : FILE_ISREG(st) != 0);
}

/* file_exists(path) -> bool : true for a regular file.  A directory is not a
 * file, so this is false for one — ask dir_exists instead. */
void file_exists(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; file_stat_is(ret, argv, 0);
}

/* file_size(path) -> int64 : bytes, -1 on failure. */
void file_size(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *path = file_nz(kn_arg_text(argv, 0));
    file_stat_t st;
    int rc = file_stat(path, &st);
    int e = errno;
    if (rc != 0) { kn_error_set_errno(e, "stat"); kn_ret_int64(ret, -1); return; }
    kn_error_clear();
    kn_ret_int64(ret, (int64_t)st.st_size);
}

/* file_modified(path) -> int64 : last-modified time in seconds since the
 * epoch, the same scale core's now() and format_time() use.  -1 on failure. */
void file_modified(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *path = file_nz(kn_arg_text(argv, 0));
    file_stat_t st;
    int rc = file_stat(path, &st);
    int e = errno;
    if (rc != 0) { kn_error_set_errno(e, "stat"); kn_ret_int64(ret, -1); return; }
    kn_error_clear();
    kn_ret_int64(ret, (int64_t)st.st_mtime);
}

/* file_delete(path) -> bool.  unlink, not remove: a directory must be refused
 * rather than quietly removed by a command whose name says "file". */
void file_delete(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *path = file_nz(kn_arg_text(argv, 0));
    int rc = file_unlink(path);
    int e = errno;
    if (rc != 0) { kn_error_set_errno(e, "delete"); kn_ret_bool(ret, 0); return; }
    kn_error_clear();
    kn_ret_bool(ret, 1);
}

/* Byte-for-byte copy.  Returns 0 on success, otherwise an errno value, and
 * writes the stage that failed to *what. */
static int file_copy_bytes(const char *from, const char *to, const char **what) {
    FILE *in = file_fopen(from, "rb");
    int e = errno;
    if (!in) { *what = "open source"; return e ? e : EIO; }
    FILE *out = file_fopen(to, "wb");
    e = errno;
    if (!out) { fclose(in); *what = "create destination"; return e ? e : EIO; }

    char buf[8192];
    int err = 0;
    for (;;) {
        size_t got = fread(buf, 1, sizeof buf, in);
        if (got == 0) break;
        size_t put = fwrite(buf, 1, got, out);
        e = errno;
        if (put != got) { err = e ? e : EIO; *what = "write"; break; }
    }
    if (!err && ferror(in)) { err = errno ? errno : EIO; *what = "read"; }
    fclose(in);
    if (fclose(out) != 0 && !err) { err = errno ? errno : EIO; *what = "write"; }
    return err;
}

/* file_copy(from, to) -> bool : contents only; permissions and timestamps are
 * whatever a newly created file gets. */
void file_copy(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *from = file_nz(kn_arg_text(argv, 0));
    const char *to   = file_nz(kn_arg_text(argv, 1));
    const char *what = "copy";
    int err = file_copy_bytes(from, to, &what);
    if (err) { kn_error_set_errno(err, what); kn_ret_bool(ret, 0); return; }
    kn_error_clear();
    kn_ret_bool(ret, 1);
}

/* file_move(from, to) -> bool.  rename() first — it is atomic and cheap — and
 * fall back to copy-then-delete only for the one case it cannot do, a move
 * across filesystems. */
void file_move(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *from = file_nz(kn_arg_text(argv, 0));
    const char *to   = file_nz(kn_arg_text(argv, 1));
#ifdef _WIN32
    /* MoveFileExW does the cross-volume copy itself, so the fallback below is
     * POSIX-only: Windows never reports EXDEV to fall back on.  REPLACE_EXISTING
     * matches what rename() does, which is what this command already promised. */
    wchar_t *wf = file_to_wide(from), *wt = file_to_wide(to);
    if (!wf || !wt) {
        free(wf); free(wt);
        kn_error_set(KN_ERR_INVALID_ARG, "move: path is not valid UTF-8");
        kn_ret_bool(ret, 0);
        return;
    }
    BOOL ok = MoveFileExW(wf, wt, MOVEFILE_COPY_ALLOWED | MOVEFILE_REPLACE_EXISTING);
    DWORD code = GetLastError();
    free(wf); free(wt);
    if (!ok) {
        char msg[96];
        snprintf(msg, sizeof msg, "move: Windows error %lu", (unsigned long)code);
        kn_error_set((int32_t)code, msg);
        kn_ret_bool(ret, 0);
        return;
    }
    kn_error_clear();
    kn_ret_bool(ret, 1);
#else
    int rc = rename(from, to);
    int e = errno;
    if (rc == 0) { kn_error_clear(); kn_ret_bool(ret, 1); return; }
    if (e != EXDEV) { kn_error_set_errno(e, "move"); kn_ret_bool(ret, 0); return; }

    const char *what = "move";
    int err = file_copy_bytes(from, to, &what);
    if (err) { kn_error_set_errno(err, what); kn_ret_bool(ret, 0); return; }
    rc = file_unlink(from);
    e = errno;
    if (rc != 0) { kn_error_set_errno(e, "delete source"); kn_ret_bool(ret, 0); return; }
    kn_error_clear();
    kn_ret_bool(ret, 1);
#endif
}

/* file_line_count(path) -> int : -1 on failure.  A last line without a
 * trailing newline still counts, so the number matches what an editor shows. */
void file_line_count(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *path = file_nz(kn_arg_text(argv, 0));
    FILE *f = file_fopen(path, "rb");
    int e = errno;
    if (!f) { kn_error_set_errno(e, "open"); kn_ret_int(ret, -1); return; }

    char buf[8192];
    long lines = 0;
    int last = '\n';                 /* an empty file has no lines at all */
    for (;;) {
        size_t got = fread(buf, 1, sizeof buf, f);
        if (got == 0) break;
        for (size_t i = 0; i < got; i++) if (buf[i] == '\n') lines++;
        last = (unsigned char)buf[got - 1];
    }
    int bad = ferror(f);
    e = errno;
    fclose(f);
    if (bad) { kn_error_set_errno(e, "read"); kn_ret_int(ret, -1); return; }
    if (last != '\n') lines++;
    kn_error_clear();
    kn_ret_int(ret, (int32_t)lines);
}

/* --- handles ----------------------------------------------------------- */

static void file_close_fn(void *payload) {
    if (payload) fclose((FILE *)payload);
}

/* file_open(path, mode) -> int : a handle, 0 on failure.  The mode is a word,
 * not a punctuation soup: "read", "write" or "append". */
void file_open(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *path = file_nz(kn_arg_text(argv, 0));
    const char *mode = file_nz(kn_arg_text(argv, 1));
    const char *cmode = NULL;
    if      (strcmp(mode, "read")   == 0) cmode = "rb";
    else if (strcmp(mode, "write")  == 0) cmode = "wb";
    else if (strcmp(mode, "append") == 0) cmode = "ab";
    if (!cmode) {
        kn_error_set(KN_ERR_INVALID_ARG, "mode must be \"read\", \"write\" or \"append\"");
        kn_ret_int(ret, 0);
        return;
    }
    FILE *f = file_fopen(path, cmode);
    int e = errno;
    if (!f) { kn_error_set_errno(e, "open"); kn_ret_int(ret, 0); return; }

    int32_t h = kn_handle_new(KN_HK_FILE, f, file_close_fn);
    if (h == 0) { fclose(f); kn_ret_int(ret, 0); return; }  /* slot set by the table */
    kn_error_clear();
    kn_ret_int(ret, h);
}

/* file_read_line(handle) -> text : the next line without its newline, "" at
 * the end of the file.  A blank line and the end of the file both read "",
 * which is why file_at_end ships next to this one. */
void file_read_line(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    FILE *f = (FILE *)kn_handle_resolve(kn_arg_int(argv, 0), KN_HK_FILE);
    if (!f) { kn_ret_text(ret, file_empty()); return; }   /* slot set by the table */

    long cap = 128, len = 0;
    char *buf = (char *)kn_malloc(cap);
    if (!buf) { kn_error_set(KN_ERR_UNSUPPORTED, "out of memory"); kn_ret_text(ret, NULL); return; }
    for (;;) {
        int c = getc(f);
        if (c == EOF || c == '\n') break;
        if (len + 1 >= cap) {
            char *nb = (char *)kn_mrealloc(buf, cap * 2);
            if (!nb) break;
            buf = nb;
            cap *= 2;
        }
        buf[len++] = (char)c;
    }
    int bad = ferror(f);
    int e = errno;
    if (bad) { kn_error_set_errno(e, "read"); kn_ret_text(ret, file_empty()); return; }
    /* A trailing CR is stripped so a file written on Windows reads the same
     * here as it does there. */
    if (len > 0 && buf[len - 1] == '\r') len--;
    buf[len] = '\0';
    kn_error_clear();
    kn_ret_text(ret, buf);
}

/* file_at_end(handle) -> bool : true when the next read would find nothing.
 * Peeks one character and puts it back, so it can be asked before a read
 * without consuming anything — feof() alone only turns true after a read has
 * already run off the end. */
void file_at_end(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    FILE *f = (FILE *)kn_handle_resolve(kn_arg_int(argv, 0), KN_HK_FILE);
    if (!f) { kn_ret_bool(ret, 0); return; }              /* slot set by the table */
    int c = getc(f);
    int e = errno;
    if (c == EOF) {
        if (ferror(f)) { kn_error_set_errno(e, "read"); kn_ret_bool(ret, 0); return; }
        kn_error_clear();
        kn_ret_bool(ret, 1);
        return;
    }
    ungetc(c, f);
    kn_error_clear();
    kn_ret_bool(ret, 0);
}

/* file_write_line(handle, line) -> bool : the line plus a newline. */
void file_write_line(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    FILE *f = (FILE *)kn_handle_resolve(kn_arg_int(argv, 0), KN_HK_FILE);
    if (!f) { kn_ret_bool(ret, 0); return; }              /* slot set by the table */
    const char *line = file_nz(kn_arg_text(argv, 1));
    int rc = fputs(line, f);
    int e = errno;
    if (rc != EOF) { rc = fputc('\n', f); e = errno; }
    if (rc == EOF) { kn_error_set_errno(e ? e : EIO, "write"); kn_ret_bool(ret, 0); return; }
    kn_error_clear();
    kn_ret_bool(ret, 1);
}

/* file_close(handle) -> bool.  Closing twice is a failure, not a crash: the
 * handle table bumps a generation and reports a stale handle. */
void file_close(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    /* The table clears or sets the slot itself, so every family reports a bad
     * handle in the same words. */
    kn_ret_bool(ret, kn_handle_close(kn_arg_int(argv, 0), KN_HK_FILE));
}

/* file_close_all() -> int : how many were still open.  The safety net for a
 * program that lost track; exit closes them anyway. */
void file_close_all(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; (void)argv;
    kn_ret_int(ret, kn_handle_close_kind(KN_HK_FILE));
}

/* --- directories ------------------------------------------------------- */

/* dir_exists(path) -> bool. */
void dir_exists(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; file_stat_is(ret, argv, 1);
}

/* dir_create(path) -> bool : creates missing parents too, and succeeds when
 * the directory is already there — asking for a directory to exist should not
 * fail because it does. */
void dir_create(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *path = file_nz(kn_arg_text(argv, 0));
    size_t n = strlen(path);
    if (n == 0) { kn_error_set(KN_ERR_INVALID_ARG, "empty path"); kn_ret_bool(ret, 0); return; }

    char *work = (char *)malloc(n + 1);      /* bookkeeping, not program data */
    if (!work) { kn_error_set(KN_ERR_UNSUPPORTED, "out of memory"); kn_ret_bool(ret, 0); return; }
    memcpy(work, path, n + 1);

    /* Start past the root: "C:" and "\\\\server\\share" are not directories that
     * can be created, and asking to create them fails where the whole call
     * should have succeeded. */
    for (size_t i = file_root_len(work) + 1; i <= n; i++) {
        if (!file_is_sep(work[i]) && work[i] != '\0') continue;
        char saved = work[i];
        work[i] = '\0';
        int rc = file_mkdir(work);
        int e = errno;
        if (rc != 0 && e != EEXIST) {
            free(work);
            kn_error_set_errno(e, "create directory");
            kn_ret_bool(ret, 0);
            return;
        }
        work[i] = saved;
    }
    free(work);
    /* EEXIST above may have been a *file* in the way; confirm what is there. */
    file_stat_t st;
    int rc = file_stat(path, &st);
    int e = errno;
    if (rc != 0) { kn_error_set_errno(e, "create directory"); kn_ret_bool(ret, 0); return; }
    if (!FILE_ISDIR(st)) {
        kn_error_set_errno(ENOTDIR, "create directory");
        kn_ret_bool(ret, 0);
        return;
    }
    kn_error_clear();
    kn_ret_bool(ret, 1);
}

/* dir_delete(path) -> bool : the directory must be empty.  There is no
 * recursive delete here on purpose — one mistyped path should not be able to
 * erase a tree. */
void dir_delete(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *path = file_nz(kn_arg_text(argv, 0));
    int rc = file_rmdir(path);
    int e = errno;
    if (rc != 0) { kn_error_set_errno(e, "delete directory"); kn_ret_bool(ret, 0); return; }
    kn_error_clear();
    kn_ret_bool(ret, 1);
}

/* dir_current() -> text : the working directory, "" on failure. */
void dir_current(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; (void)argv;
    char *p = file_getcwd_text();
    int e = errno;
    if (!p) { kn_error_set_errno(e, "current directory"); kn_ret_text(ret, file_empty()); return; }
    kn_error_clear();
    kn_ret_text(ret, p);
}

/* dir_set_current(path) -> bool. */
void dir_set_current(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *path = file_nz(kn_arg_text(argv, 0));
    int rc = file_chdir(path);
    int e = errno;
    if (rc != 0) { kn_error_set_errno(e, "change directory"); kn_ret_bool(ret, 0); return; }
    kn_error_clear();
    kn_ret_bool(ret, 1);
}

/* The listing snapshot.  A directory changes under a loop that reads it, so
 * dir_entry_count takes a sorted snapshot and dir_entry only reads it: a loop
 * sees one stable view, and refreshing is an explicit act — calling the count
 * again.  Plain malloc, like the handle table: this is bookkeeping, not
 * program data, and must not be freed by kn_free_all() at exit. */
static char  *g_snap_path = NULL;
static char **g_snap = NULL;
static long   g_snap_n = 0;

static void file_snap_free(void) {
    for (long i = 0; i < g_snap_n; i++) free(g_snap[i]);
    free(g_snap);
    free(g_snap_path);
    g_snap = NULL;
    g_snap_path = NULL;
    g_snap_n = 0;
}

static int file_name_cmp(const void *a, const void *b) {
    return strcmp(*(const char *const *)a, *(const char *const *)b);
}

/* One name into the snapshot.  Shared by the two collection loops below, which
 * differ only in how the platform hands names over. */
static int file_snap_push(const char *name, long *cap) {
    if (strcmp(name, ".") == 0 || strcmp(name, "..") == 0) return 1;
    if (g_snap_n == *cap) {
        long want = *cap ? *cap * 2 : 32;
        char **nb = (char **)realloc(g_snap, (size_t)want * sizeof(char *));
        if (!nb) return 0;
        g_snap = nb;
        *cap = want;
    }
    size_t n = strlen(name);
    char *copy = (char *)malloc(n + 1);
    if (!copy) return 0;
    memcpy(copy, name, n + 1);
    g_snap[g_snap_n++] = copy;
    return 1;
}

/* dir_entry_count(path) -> int : -1 on failure.  Re-reads the directory and
 * re-takes the snapshot that dir_entry reads. */
void dir_entry_count(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *path = file_nz(kn_arg_text(argv, 0));
    long cap = 0;

#ifdef _WIN32
    /* FindFirstFileW wants a pattern, not a directory, so the wildcard is
     * appended here — the one place a Windows listing differs in shape from a
     * POSIX one. */
    size_t pn = strlen(path);
    char *pattern = (char *)malloc(pn + 3);
    if (!pattern) { file_snap_free(); kn_error_set(KN_ERR_UNSUPPORTED, "out of memory"); kn_ret_int(ret, -1); return; }
    memcpy(pattern, path, pn);
    size_t w = pn;
    if (w == 0 || !file_is_sep(pattern[w - 1])) pattern[w++] = FILE_SEP;
    pattern[w++] = '*';
    pattern[w] = '\0';
    wchar_t *wide = file_to_wide(pattern);
    free(pattern);
    if (!wide) { file_snap_free(); kn_error_set(KN_ERR_INVALID_ARG, "open directory: path is not valid UTF-8"); kn_ret_int(ret, -1); return; }

    WIN32_FIND_DATAW fd;
    HANDLE h = FindFirstFileW(wide, &fd);
    free(wide);
    if (h == INVALID_HANDLE_VALUE) {
        DWORD code = GetLastError();
        file_snap_free();                    /* a failed listing leaves none */
        char msg[96];
        snprintf(msg, sizeof msg, "open directory: Windows error %lu", (unsigned long)code);
        kn_error_set((int32_t)code, msg);
        kn_ret_int(ret, -1);
        return;
    }
    file_snap_free();
    do {
        char *name = file_from_wide(fd.cFileName);
        if (!name || !file_snap_push(name, &cap)) {
            FindClose(h);
            file_snap_free();
            kn_error_set(KN_ERR_UNSUPPORTED, "out of memory");
            kn_ret_int(ret, -1);
            return;
        }
    } while (FindNextFileW(h, &fd));
    DWORD end = GetLastError();
    FindClose(h);
    if (end != ERROR_NO_MORE_FILES) {
        file_snap_free();
        char msg[96];
        snprintf(msg, sizeof msg, "read directory: Windows error %lu", (unsigned long)end);
        kn_error_set((int32_t)end, msg);
        kn_ret_int(ret, -1);
        return;
    }
#else
    DIR *d = opendir(path);
    int e = errno;
    if (!d) {
        file_snap_free();                    /* a failed listing leaves none */
        kn_error_set_errno(e, "open directory");
        kn_ret_int(ret, -1);
        return;
    }
    file_snap_free();

    struct dirent *de;
    errno = 0;
    while ((de = readdir(d)) != NULL) {
        if (!file_snap_push(de->d_name, &cap)) {
            closedir(d);
            file_snap_free();
            kn_error_set(KN_ERR_UNSUPPORTED, "out of memory");
            kn_ret_int(ret, -1);
            return;
        }
        errno = 0;
    }
    int read_err = errno;
    closedir(d);
    if (read_err != 0) {
        file_snap_free();
        kn_error_set_errno(read_err, "read directory");
        kn_ret_int(ret, -1);
        return;
    }
#endif

    /* Sorted, so two runs of the same program list the same order — the
     * platform's own order is whatever the filesystem happens to hand back. */
    if (g_snap_n > 1) qsort(g_snap, (size_t)g_snap_n, sizeof(char *), file_name_cmp);

    size_t keep = strlen(path);
    g_snap_path = (char *)malloc(keep + 1);
    if (!g_snap_path) { file_snap_free(); kn_error_set(KN_ERR_UNSUPPORTED, "out of memory"); kn_ret_int(ret, -1); return; }
    memcpy(g_snap_path, path, keep + 1);

    kn_error_clear();
    kn_ret_int(ret, (int32_t)g_snap_n);
}

/* dir_entry(path, index) -> text : one name from the snapshot, without its
 * directory.  Out of range is "" with no error — the count is the authority,
 * and no real entry is ever named "".  Asking for a path that was not the last
 * one counted IS an error, because the answer would otherwise be silently
 * about a different directory. */
void dir_entry(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *path = file_nz(kn_arg_text(argv, 0));
    int32_t i = kn_arg_int(argv, 1);
    if (!g_snap_path || strcmp(g_snap_path, path) != 0) {
        kn_error_set(KN_ERR_INVALID_ARG, "no listing for this path; call dir_entry_count first");
        kn_ret_text(ret, file_empty());
        return;
    }
    if (i < 1 || (long)i > g_snap_n) { kn_error_clear(); kn_ret_text(ret, file_empty()); return; }
    kn_error_clear();
    kn_ret_text(ret, file_text(g_snap[i - 1]));
}

/* --- paths ------------------------------------------------------------
 * Pure text.  None of these look at the filesystem, so none of them can fail,
 * so none of them touches the error slot — an error set by a file command
 * survives the path arithmetic done while reporting it. */

/* path_join(a, b) -> text : one separator, never two.  An absolute b wins,
 * which is what every other path_join in the world does. */
void path_join(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *a = file_nz(kn_arg_text(argv, 0));
    const char *b = file_nz(kn_arg_text(argv, 1));
    /* "rooted" rather than "starts with a separator": on Windows "C:\\x" is
     * absolute and has no leading separator at all. */
    if (file_root_len(b) > 0 || *a == '\0') { kn_ret_text(ret, file_text(b)); return; }
    if (*b == '\0') { kn_ret_text(ret, file_text(a)); return; }
    size_t la = strlen(a), lb = strlen(b);
    int sep = !file_is_sep(a[la - 1]);
    char *o = (char *)kn_malloc((long)(la + (size_t)sep + lb + 1));
    if (!o) { kn_ret_text(ret, NULL); return; }
    memcpy(o, a, la);
    if (sep) o[la] = FILE_SEP;
    memcpy(o + la + sep, b, lb + 1);
    kn_ret_text(ret, o);
}

/* Where the last component starts, ignoring trailing slashes. */
static void file_split(const char *p, size_t *base, size_t *end) {
    size_t root = file_root_len(p);
    size_t n = strlen(p);
    /* Never trim into the root: the trailing separator of "C:\\" and of "/" is
     * the root itself, not decoration on a component. */
    while (n > root && n > 1 && file_is_sep(p[n - 1])) n--;
    size_t b = n;
    while (b > root && !file_is_sep(p[b - 1])) b--;
    *base = b;
    *end = n;
}

/* path_name(path) -> text : the last component. */
void path_name(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *p = file_nz(kn_arg_text(argv, 0));
    size_t b, e;
    file_split(p, &b, &e);
    kn_ret_text(ret, file_text_n(p + b, e - b));
}

/* path_parent(path) -> text : everything before the last component, "" when
 * there is none.  "" joins as nothing, so path_join(path_parent(x), y) does
 * the right thing for a bare filename. */
void path_parent(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *p = file_nz(kn_arg_text(argv, 0));
    size_t b, e;
    file_split(p, &b, &e);
    (void)e;
    size_t root = file_root_len(p);
    if (b == 0) { kn_ret_text(ret, file_text_n("", 0)); return; }
    /* At the root the parent IS the root, separator included — dropping it
     * would turn "C:\\x" into a path relative to the drive's own directory. */
    if (b <= root) { kn_ret_text(ret, file_text_n(p, root)); return; }
    kn_ret_text(ret, file_text_n(p, b - 1));            /* drop the separator */
}

/* path_extension(path) -> text : after the last dot of the last component,
 * without the dot.  A name that only begins with a dot has no extension —
 * ".bashrc" is a hidden file, not a "bashrc" file. */
void path_extension(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *p = file_nz(kn_arg_text(argv, 0));
    size_t b, e;
    file_split(p, &b, &e);
    size_t dot = e;
    for (size_t i = e; i > b; i--) if (p[i - 1] == '.') { dot = i - 1; break; }
    if (dot == e || dot == b) { kn_ret_text(ret, file_text_n("", 0)); return; }
    kn_ret_text(ret, file_text_n(p + dot + 1, e - dot - 1));
}

/* path_absolute(path) -> text : the path rooted at the working directory, with
 * "." and ".." resolved textually.  Deliberately lexical: realpath() fails for
 * a path that does not exist yet, and "where would I write this?" is a
 * question about text, not about what is on disk.  It follows that a symbolic
 * link is not resolved. */
void path_absolute(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *p = file_nz(kn_arg_text(argv, 0));
    char cwd[4096];
    const char *parts[2];
    int np = 0;
    size_t need = strlen(p) + 2;
    if (file_root_len(p) == 0) {
        if (!file_getcwd_buf(cwd, sizeof cwd)) {
            /* Infallible by contract: hand back what we were given rather than
             * set an error slot a path command is not allowed to touch. */
            kn_ret_text(ret, file_text(p));
            return;
        }
        need += strlen(cwd) + 1;
        parts[np++] = cwd;
    }
    parts[np++] = p;
    char *o = (char *)kn_malloc((long)need);
    if (!o) { kn_ret_text(ret, NULL); return; }

    /* The root is copied verbatim from whichever part supplies it, so a drive
     * letter or a UNC share survives intact and ".." can never rewind past it.
     * Then walk component by component, writing each after the last separator
     * already emitted; ".." rewinds that write position. */
    size_t root = file_root_len(parts[0]);
    if (root == 0) { o[0] = FILE_SEP; root = 1; }
    else memcpy(o, parts[0], root);
    size_t w = root;
    for (int k = 0; k < np; k++) {
        const char *s = parts[k];
        size_t i = file_root_len(s), n = strlen(s);
        while (i < n) {
            while (i < n && file_is_sep(s[i])) i++;
            size_t start = i;
            while (i < n && !file_is_sep(s[i])) i++;
            size_t len = i - start;
            if (len == 0) continue;
            if (len == 1 && s[start] == '.') continue;
            if (len == 2 && s[start] == '.' && s[start + 1] == '.') {
                while (w > root && !file_is_sep(o[w - 1])) w--;
                if (w > root) w--;                      /* drop the separator */
                continue;
            }
            if (w > root) o[w++] = FILE_SEP;
            memcpy(o + w, s + start, len);
            w += len;
        }
    }
    o[w] = '\0';                        /* w >= root: the root is always there */
    kn_ret_text(ret, o);
}
