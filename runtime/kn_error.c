/* The error slot — how a fallible command says why it failed.
 *
 * Kiln has no exceptions and no out-parameters, so a command that fails
 * returns a sentinel and leaves the reason here.  `last_error_code()` and
 * `last_error_text()` read it; both are zero-arity, which is exactly what makes
 * an out-parameter expressible in a language that has none.
 *
 * The conventions this file enforces are documented in abi/kiln_abi.h, next
 * to the declarations, because that is the header a library author reads.
 */
#include <stdio.h>
#include <string.h>
#include "kiln_core.h"

/* Fixed storage, deliberately.  Setting an error must never allocate: E_MAlloc
 * on failure calls kn_runtime_error -> exit(1), so an allocating error path
 * could terminate the program while it was *reporting* an error. */
static int32_t g_code = 0;
static char    g_msg[256] = "";

void kn_error_clear(void) {
    g_code = 0;
    g_msg[0] = '\0';
}

void kn_error_set(int32_t code, const char *msg) {
    g_code = code;
    snprintf(g_msg, sizeof g_msg, "%s", msg ? msg : "");
}

void kn_error_set_errno(int32_t saved_errno, const char *what) {
    /* The code is stored first, before snprintf or strerror can touch errno. */
    g_code = saved_errno;
    /* strerror is not reentrant, but the runtime is single-threaded and the
     * text is copied immediately, so a later call cannot invalidate it. */
    snprintf(g_msg, sizeof g_msg, "%s: %s", what ? what : "operation",
             strerror(saved_errno));
}

int32_t     kn_error_code(void)    { return g_code; }
const char *kn_error_message(void) { return g_msg; }

/* A fresh empty string for the "" failure sentinel.  Text results are
 * runtime-owned like every other text result, so ownership stays uniform and a
 * program can hold a failed result without a special case. */
char *kn_empty_text(void) {
    char *s = (char *)kn_malloc(1);
    if (s) s[0] = '\0';
    return s;
}

/* --- commands --------------------------------------------------------- */

/* last_error_code() -> int.  Reading does NOT clear: both commands must be able
 * to read the same failure, and a program may want the code and the text. */
void kn_last_error_code(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; (void)argv;
    kn_ret_int(ret, g_code);
}

/* last_error_text() -> text.  Returns a COPY, not the static buffer: a program
 * must be able to bind the message, hit a second failure, and still print the
 * first.  Handing out the buffer would silently mutate a value it already owns. */
void kn_last_error_text(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; (void)argv;
    size_t n = strlen(g_msg);
    char *out = (char *)kn_malloc((long)n + 1);
    if (!out) { kn_ret_text(ret, NULL); return; }
    memcpy(out, g_msg, n + 1);
    kn_ret_text(ret, out);
}
