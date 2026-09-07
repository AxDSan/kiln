/* The program's command-line arguments.
 *
 * Captured by main() in kn_start.c, which is the only place they exist, and
 * read by the `system` library's `arg_count` / `arg` commands.  A library
 * target excludes kn_start.c, so nothing sets these there and the count is 0 —
 * which is honest, rather than a pointer nobody initialised.
 */
#include "kiln_core.h"

static int    g_argc = 0;
static char **g_argv = NULL;

void kn_set_args(int argc, char **argv) {
    g_argc = argc;
    g_argv = argv;
}

int kn_arg_total(void) { return g_argc; }

const char *kn_arg_at(int i) {
    if (!g_argv || i < 0 || i >= g_argc) return NULL;
    return g_argv[i];
}
