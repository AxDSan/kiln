/* Stopping a Kiln 2 program with a message.
 *
 * A Kiln 2 program stops, rather than reading past the end of a list or
 * dividing by zero, and says why on stderr. The compiler used to call `dprintf`
 * for the message, which a Windows C runtime does not have; this is the one
 * spelling every target links.
 */
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>

void kn_stop(const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    fputs("kiln: ", stderr);
    vfprintf(stderr, fmt, ap);
    fputc('\n', stderr);
    va_end(ap);
    fflush(stdout);
    fflush(stderr);
    exit(1);
}
