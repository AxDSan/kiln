/* Process entry. Provides `main`, which runs the lean EPL entry:
 *   E_Init(); ECodeStart();
 * Kept in its own object so alternate entries (WinMain/DllMain) can replace it
 * per build target without touching the command objects. */
#include "kiln_core.h"

#ifdef _WIN32
#include <windows.h>
#include <shellapi.h>
#include <stdlib.h>

/* Windows hands `main` its arguments in the ANSI code page, which loses any
 * character the page cannot spell — a path with an accent arrives as `?`.
 * Every other string in a program is UTF-8, so the arguments are re-read from
 * the wide command line and converted once, here, where they are captured.
 * The array is never freed: it lives exactly as long as the process, the
 * same as the argv it replaces. */
static char **utf8_args(int *argc_out) {
    int     n    = 0;
    LPWSTR *wide = CommandLineToArgvW(GetCommandLineW(), &n);
    if (!wide) { *argc_out = 0; return NULL; }
    char **out = (char **)calloc((size_t)n + 1, sizeof *out);
    if (!out) { LocalFree(wide); *argc_out = 0; return NULL; }
    for (int i = 0; i < n; i++) {
        int len = WideCharToMultiByte(CP_UTF8, 0, wide[i], -1, NULL, 0, NULL, NULL);
        out[i]  = (char *)malloc(len > 0 ? (size_t)len : 1);
        if (!out[i]) { *argc_out = 0; LocalFree(wide); return NULL; }
        if (len > 0) WideCharToMultiByte(CP_UTF8, 0, wide[i], -1, out[i], len, NULL, NULL);
        else out[i][0] = '\0';
    }
    LocalFree(wide);
    *argc_out = n;
    return out;
}
#endif

int main(int argc, char **argv) {
#ifdef _WIN32
    /* The runtime prints UTF-8; a console left on its default code page shows
     * `—` as three wrong glyphs. Output only: input stays as the shell gave it. */
    SetConsoleOutputCP(CP_UTF8);
    /* A conversion that fails leaves the arguments as the C runtime gave
     * them — narrowed, but present — rather than none at all. */
    int    wargc;
    char **wargv = utf8_args(&wargc);
    if (wargv) { argc = wargc; argv = wargv; }
#endif
    /* Captured here because this is the only place the program's arguments
     * exist.  A library target excludes this file, so `arg_count()` there
     * reports 0 rather than reading a pointer nobody set. */
    kn_set_args(argc, argv);
    E_Init();
    int rc = ECodeStart();
    E_DestroyRes();
    return rc;
}
