/* The foreign-function loader — how a `dll` call reaches a C export.
 *
 * A `dll add(a: int, b: int): int from "mathdll"` is lowered by the backend to
 * one call to `kn_dll_get` (which hands back the resolved address, caching it)
 * followed by an indirect call through that address with the declared C
 * signature.  This file is the whole of the run-time half: open the library,
 * find the symbol, and copy a returned string back across the boundary.
 *
 * LOADING IS LAZY.  Nothing is opened until the first call, so a program may
 * DECLARE a `dll` from a library that is not present and still build and run —
 * it fails only if it actually calls it.  The backend caches the address in a
 * per-declaration global, so the resolution here happens once; the library and
 * symbol caches below make two declarations that name the same library share a
 * single open, and honour "resolve each symbol once" even across them.
 *
 * A MISSING LIBRARY OR SYMBOL IS A NAMED FAILURE.  `kn_runtime_error` prints a
 * line naming both the library and the symbol and exits 1 — never a silent 0,
 * because a foreign call that quietly returned nothing would be a bug a program
 * could not see.
 *
 * PLATFORM PARITY mirrors cli/src/libload.rs: `dlopen`/`dlsym` on POSIX,
 * `LoadLibraryA`/`GetProcAddress` under _WIN32.  The `from` name decorates the
 * same way a linker's `-l` does — a bare `mathdll` becomes `libmathdll.so` /
 * `mathdll.dll` / `libmathdll.dylib` — and each candidate is tried beside the
 * program first, then on the OS search path.  A name that already carries an
 * extension or a slash is used verbatim.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include "kiln_core.h"

#ifdef _WIN32
#include <windows.h>
static void *lib_open(const char *path) { return (void *)LoadLibraryA(path); }
static void *lib_sym(void *h, const char *s) {
    return (void *)GetProcAddress((HMODULE)h, s);
}
#else
#include <dlfcn.h>
#include <unistd.h>
#ifdef __APPLE__
#include <mach-o/dyld.h>
#endif
static void *lib_open(const char *path) { return dlopen(path, RTLD_LAZY | RTLD_LOCAL); }
static void *lib_sym(void *h, const char *s) { return dlsym(h, s); }
#endif

/* Platform spellings of a bare library name (`prefix<name><suffix>`). */
#ifdef _WIN32
#define KN_DLL_PREFIX ""
#define KN_DLL_SUFFIX ".dll"
#elif defined(__APPLE__)
#define KN_DLL_PREFIX "lib"
#define KN_DLL_SUFFIX ".dylib"
#else
#define KN_DLL_PREFIX "lib"
#define KN_DLL_SUFFIX ".so"
#endif

#define KN_DLL_PATH_MAX 4096

/* --- caches ----------------------------------------------------------- */

/* Program-lifetime linked lists, never freed — the same bargain every runtime
 * cache makes.  A failed node allocation simply skips caching and still returns
 * the resolved value, so a low-memory machine loses speed, not correctness. */
typedef struct LibNode { char *name; void *handle; struct LibNode *next; } LibNode;
typedef struct SymNode { char *lib; char *sym; void *addr; struct SymNode *next; } SymNode;
static LibNode *g_libs = NULL;
static SymNode *g_syms = NULL;

/* strdup without depending on it — it is POSIX, not C, and this file compiles
 * under mingw too. */
static char *dup_str(const char *s) {
    size_t n = strlen(s) + 1;
    char *out = (char *)malloc(n);
    if (out) memcpy(out, s, n);
    return out;
}

/* --- executable directory --------------------------------------------- */

/* Fill `buf` with the directory the running program sits in, INCLUDING a
 * trailing separator, and return 1; return 0 if it cannot be found (a static
 * musl binary with no /proc, say), in which case the beside-program candidates
 * are simply skipped and only the OS search path is tried. */
static int program_dir(char *buf, size_t n) {
    size_t len = 0;
#ifdef _WIN32
    DWORD got = GetModuleFileNameA(NULL, buf, (DWORD)n);
    if (got == 0 || got >= n) return 0;
    len = (size_t)got;
#elif defined(__APPLE__)
    uint32_t size = (uint32_t)n;
    if (_NSGetExecutablePath(buf, &size) != 0) return 0;
    len = strlen(buf);
#else
    ssize_t got = readlink("/proc/self/exe", buf, n - 1);
    if (got <= 0 || (size_t)got >= n) return 0;
    buf[got] = '\0';
    len = (size_t)got;
#endif
    /* Trim back to and including the last path separator. */
    while (len > 0 && buf[len - 1] != '/' && buf[len - 1] != '\\') len--;
    if (len == 0) return 0;
    buf[len] = '\0';
    return 1;
}

/* --- opening a library ------------------------------------------------ */

static int has_separator(const char *s) {
    return strchr(s, '/') != NULL || strchr(s, '\\') != NULL;
}

/* Whether the basename carries a `.` — an already-decorated name like
 * `user32.dll` or `libfoo.so.1` that must not be decorated again. */
static int has_extension(const char *s) {
    const char *base = s;
    for (const char *p = s; *p; p++) {
        if (*p == '/' || *p == '\\') base = p + 1;
    }
    return strchr(base, '.') != NULL;
}

/* Try one candidate filename beside the program (if `dir` is non-empty) and
 * then on the OS search path.  Returns a handle or NULL. */
static void *try_candidate(const char *dir, const char *file) {
    if (dir && dir[0]) {
        char path[KN_DLL_PATH_MAX];
        if (strlen(dir) + strlen(file) < sizeof path) {
            memcpy(path, dir, strlen(dir) + 1);
            strcat(path, file);
            void *h = lib_open(path);
            if (h) return h;
        }
    }
    return lib_open(file);
}

/* Resolve `name` (the `from` string) to an open handle, trying the platform's
 * spellings beside the program then on the OS path.  NULL if none open. */
static void *open_library(const char *name) {
    char dir[KN_DLL_PATH_MAX];
    const char *d = program_dir(dir, sizeof dir) ? dir : "";

    /* A path or an already-decorated name is used as written. */
    if (has_separator(name)) return lib_open(name);
    if (has_extension(name)) return try_candidate(d, name);

    /* A bare name is decorated. `prefix<name>suffix` is the linker's spelling
     * and the first thing to try; a plain `<name>suffix` covers a library that
     * ships without the `lib` prefix (a Windows-style name on Linux). */
    char decorated[KN_DLL_PATH_MAX];
    void *h;

    if (snprintf(decorated, sizeof decorated, "%s%s%s", KN_DLL_PREFIX, name, KN_DLL_SUFFIX)
        < (int)sizeof decorated) {
        h = try_candidate(d, decorated);
        if (h) return h;
    }
#ifndef _WIN32
    /* On Windows the prefix is empty, so this second spelling equals the first;
     * only the two Unixes gain a `<name>.so` / `<name>.dylib` fallback. */
    if (snprintf(decorated, sizeof decorated, "%s%s", name, KN_DLL_SUFFIX)
        < (int)sizeof decorated) {
        h = try_candidate(d, decorated);
        if (h) return h;
    }
#endif
    return NULL;
}

/* The open handle for `name`, from the cache or freshly opened.  Aborts through
 * kn_runtime_error (naming the symbol that wanted it) if it cannot be loaded. */
static void *library_handle(const char *name, const char *sym) {
    for (LibNode *n = g_libs; n; n = n->next) {
        if (strcmp(n->name, name) == 0) return n->handle;
    }
    void *handle = open_library(name);
    if (!handle) {
        char msg[512];
        snprintf(msg, sizeof msg,
                 "cannot load library `%s` for `%s`", name, sym);
        kn_runtime_error(msg); /* prints and exits 1 */
        return NULL;           /* unreachable */
    }
    LibNode *node = (LibNode *)malloc(sizeof *node);
    if (node) {
        node->name = dup_str(name);
        node->handle = handle;
        node->next = g_libs;
        g_libs = node;
    }
    return handle;
}

/* --- the two entry points emitted IR calls ---------------------------- */

/* Resolve `sym` in `library`, caching the address in `*cache` so a call site
 * in a loop resolves once.  The per-declaration global the backend passes as
 * `cache` starts NULL; the first call fills it. */
void *kn_dll_get(void **cache, const char *library, const char *sym) {
    if (*cache) return *cache;

    /* Cross-declaration cache: another `dll` that named the same library and
     * symbol has already resolved it. */
    for (SymNode *n = g_syms; n; n = n->next) {
        if (strcmp(n->lib, library) == 0 && strcmp(n->sym, sym) == 0) {
            *cache = n->addr;
            return n->addr;
        }
    }

    void *handle = library_handle(library, sym);
    void *addr = lib_sym(handle, sym);
    if (!addr) {
        char msg[512];
        snprintf(msg, sizeof msg,
                 "symbol `%s` not found in library `%s`", sym, library);
        kn_runtime_error(msg); /* prints and exits 1 */
        return NULL;           /* unreachable */
    }

    SymNode *node = (SymNode *)malloc(sizeof *node);
    if (node) {
        node->lib = dup_str(library);
        node->sym = dup_str(sym);
        node->addr = addr;
        node->next = g_syms;
        g_syms = node;
    }
    *cache = addr;
    return addr;
}

/* Copy the NUL-terminated C string a foreign call returned into a runtime-owned
 * text, so the result lives and is freed like every other text.  Mirrors
 * kn_ptr_read_text: a NULL pointer answers "" rather than dereferencing.  The
 * pointer the C side handed back is COPIED, not freed — a `char*` a library
 * owns is not the runtime's to release. */
char *kn_dll_text(const char *p) {
    if (!p) return kn_empty_text();
    size_t n = strlen(p);
    char *out = (char *)kn_malloc((long)n + 1);
    if (out) memcpy(out, p, n + 1);
    return out;
}
