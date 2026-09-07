/* Core commands: console output (slot ABI). */
#include <stdio.h>
#include <stdlib.h>
#include "kiln_core.h"

void kn_print_int(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; (void)ret;
    printf("%d\n", kn_arg_int(argv, 0));
}
void kn_print_int64(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; (void)ret;
    printf("%lld\n", (long long)kn_arg_int64(argv, 0));
}
void kn_print_text(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; (void)ret;
    const char *t = kn_arg_text(argv, 0);
    printf("%s\n", t ? t : "");
}
void kn_print_double(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; (void)ret;
    printf("%g\n", kn_arg_double(argv, 0));
}

/* --- Input ------------------------------------------------------------
 * The core could print four ways and read none, which made the first program
 * anyone writes — ask a question, use the answer — impossible without a `use`
 * declaration.  These are the other half of the print_* pair, so they belong
 * beside them rather than in a support library. */

/* Read one line from stdin, without its newline.  Grows as it goes, so a long
 * line is not silently truncated the way a fixed buffer would. */
static char *read_line_into_text(void) {
    long cap = 128, len = 0;
    char *buf = (char *)kn_malloc(cap);
    if (!buf) return NULL;
    for (;;) {
        int c = getchar();
        if (c == EOF || c == '\n') break;
        if (len + 1 >= cap) {
            char *nb = (char *)kn_mrealloc(buf, cap * 2);
            if (!nb) break;
            buf = nb;
            cap *= 2;
        }
        buf[len++] = (char)c;
    }
    /* A trailing CR is stripped so a file written on Windows reads the same
     * here as it does there — otherwise every comparison against it fails for
     * a reason nothing on screen can show. */
    if (len > 0 && buf[len - 1] == '\r') len--;
    buf[len] = '\0';
    return buf;
}

/* read_line() -> text : the next line, or "" at end of input. */
void kn_read_line(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; (void)argv;
    kn_ret_text(ret, read_line_into_text());
}

/* input_ended() -> bool : the predicate that tells an empty line apart from no
 * line at all.  Peeks one character and puts it back, so it can be called
 * before a read without consuming anything. */
void kn_input_ended(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; (void)argv;
    int c = getchar();
    if (c == EOF) { kn_ret_bool(ret, 1); return; }
    ungetc(c, stdin);
    kn_ret_bool(ret, 0);
}

/* ask(text prompt) -> text : print the prompt, then read a line. */
void kn_ask(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *p = kn_arg_text(argv, 0);
    fputs(p ? p : "", stdout);
    /* stdout is line-buffered, and a prompt has no newline — without this the
     * question appears after the answer is typed. */
    fflush(stdout);
    kn_ret_text(ret, read_line_into_text());
}

/* --- assert ------------------------------------------------------------ */

/* assert_failed(text message) : what an `assert` whose condition was false
 * runs.  It prints and stops, and it stops with a *failing* exit status,
 * because an assertion that fired is a broken program and a script that runs
 * one has to be able to tell.
 *
 * On stderr rather than stdout: a program whose output is being piped
 * somewhere should not have that output silently gain a diagnostic line.  The
 * flush is because stdout may hold buffered output the message is about — the
 * exit below would discard the ordering otherwise. */
void kn_assert_failed(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)ret; (void)argc;
    const char *m = kn_arg_text(argv, 0);
    fflush(stdout);
    fprintf(stderr, "%s\n", m ? m : "assertion failed");
    fflush(stderr);
    exit(1);
}
