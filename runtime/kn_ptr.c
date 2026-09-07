/* Pointers and raw memory — the escape hatch to C.
 *
 * A `ptr` is an opaque 64-bit machine address.  These commands are the raw
 * bridge that lets an Kiln program hand a buffer, a struct, a handle or an
 * out-parameter to a C API: everything a DLL call or a callback trampoline
 * needs to move bytes across the boundary.
 *
 * RAW BY DESIGN.  A read or write at a bad address faults exactly as it would
 * in C — there is no bounds check a C programmer would not have, because the
 * whole point is to reach memory the runtime does not own (a Win32 struct, a
 * mmap, a library's own allocation).  The ONE safety is `ptr_read_text`, which
 * copies the C string out and so can answer "" for a null pointer instead of
 * dereferencing it; every other read at null is undefined, as in C.
 *
 * Two allocator rules, both deliberate:
 *   - `mem_alloc`/`mem_free` are plain `malloc`/`free`, NOT the runtime's
 *     tracked E_MAlloc.  A block handed to a C API may be freed or realloced by
 *     that API, and the runtime's allocator puts a bookkeeping header before the
 *     payload — so a C `free()` on it would corrupt the heap.  The block is the
 *     caller's to `mem_free`; it is not swept at exit, so a leaked one leaks
 *     exactly as in C.
 *   - `ptr_read_text` returns a runtime-owned text (via kn_malloc), so its
 *     result lives and is freed like every other text result.  `ptr_of_text`
 *     does the reverse and hands back the char* BACKING a text: borrowed, valid
 *     only while that text is, and pointing at read-only bytes when the text is
 *     a literal (strings are emitted as constants) — writing through it faults.
 *
 * Offsets and sizes are int64_t, never `long`: on Windows `long` is 32-bit
 * (LLP64), and a size or offset must be the full 64 bits.  Reads and writes go
 * through memcpy rather than a cast-and-deref so an unaligned field inside a
 * packed Win32 struct is well-defined; on x86-64 the codegen is identical.
 */
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include "kiln_core.h"

/* The address `p` plus a byte offset, as a plain byte pointer for memcpy. */
static unsigned char *at(void *p, int64_t offset) {
    return (unsigned char *)p + offset;
}

/* --- identity and arithmetic ------------------------------------------ */

void kn_ptr_null(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; (void)argv;
    kn_ret_ptr(ret, NULL);
}

void kn_ptr_is_null(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    kn_ret_bool(ret, kn_arg_ptr(argv, 0) == NULL);
}

void kn_ptr_offset(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    kn_ret_ptr(ret, at(kn_arg_ptr(argv, 0), kn_arg_int64(argv, 1)));
}

/* The escape hatch both ways.  A ptr is stored in the slot's union member that
 * overlaps int64, but the conversion is spelled out so the type system can ban
 * the implicit form. */
void kn_ptr_from_int(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    kn_ret_ptr(ret, (void *)(uintptr_t)kn_arg_int64(argv, 0));
}

void kn_ptr_to_int(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    kn_ret_int64(ret, (int64_t)(uintptr_t)kn_arg_ptr(argv, 0));
}

/* --- typed reads and writes at an offset ------------------------------ */

void kn_ptr_read_int(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    int32_t v;
    memcpy(&v, at(kn_arg_ptr(argv, 0), kn_arg_int64(argv, 1)), sizeof v);
    kn_ret_int(ret, v);
}

void kn_ptr_write_int(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)ret; (void)argc;
    int32_t v = kn_arg_int(argv, 2);
    memcpy(at(kn_arg_ptr(argv, 0), kn_arg_int64(argv, 1)), &v, sizeof v);
}

void kn_ptr_read_int64(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    int64_t v;
    memcpy(&v, at(kn_arg_ptr(argv, 0), kn_arg_int64(argv, 1)), sizeof v);
    kn_ret_int64(ret, v);
}

void kn_ptr_write_int64(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)ret; (void)argc;
    int64_t v = kn_arg_int64(argv, 2);
    memcpy(at(kn_arg_ptr(argv, 0), kn_arg_int64(argv, 1)), &v, sizeof v);
}

/* One byte, read as an int 0..255 and written from an int's low 8 bits. */
void kn_ptr_read_byte(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    unsigned char b = *at(kn_arg_ptr(argv, 0), kn_arg_int64(argv, 1));
    kn_ret_int(ret, (int32_t)b);
}

void kn_ptr_write_byte(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)ret; (void)argc;
    *at(kn_arg_ptr(argv, 0), kn_arg_int64(argv, 1)) = (unsigned char)kn_arg_int(argv, 2);
}

void kn_ptr_read_double(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    double v;
    memcpy(&v, at(kn_arg_ptr(argv, 0), kn_arg_int64(argv, 1)), sizeof v);
    kn_ret_double(ret, v);
}

void kn_ptr_write_double(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)ret; (void)argc;
    double v = kn_arg_double(argv, 2);
    memcpy(at(kn_arg_ptr(argv, 0), kn_arg_int64(argv, 1)), &v, sizeof v);
}

/* Deref a pointer-to-pointer — what a trampoline reading a vtable or an array
 * of pointers needs. */
void kn_ptr_read_ptr(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    void *v;
    memcpy(&v, at(kn_arg_ptr(argv, 0), kn_arg_int64(argv, 1)), sizeof v);
    kn_ret_ptr(ret, v);
}

void kn_ptr_write_ptr(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)ret; (void)argc;
    void *v = kn_arg_ptr(argv, 2);
    memcpy(at(kn_arg_ptr(argv, 0), kn_arg_int64(argv, 1)), &v, sizeof v);
}

/* --- text at the boundary --------------------------------------------- */

/* Copy the NUL-terminated C string at `p` into a runtime-owned text.  The one
 * read that is safe at null: it answers "" rather than dereferencing, because
 * the result is copied out anyway and a program building a struct will hit a
 * null field before it hits a bad one. */
void kn_ptr_read_text(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *p = (const char *)kn_arg_ptr(argv, 0);
    if (!p) { kn_ret_text(ret, kn_empty_text()); return; }
    size_t n = strlen(p);
    char *out = (char *)kn_malloc((long)n + 1);
    memcpy(out, p, n + 1);
    kn_ret_text(ret, out);
}

/* Copy a text's bytes, including the terminating NUL, into `dest` at `offset`.
 * The caller owns `dest` and must have made it large enough — this is memcpy,
 * not a checked write.  An empty text (NULL char*) writes a single NUL. */
void kn_ptr_write_text(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)ret; (void)argc;
    unsigned char *dest = at(kn_arg_ptr(argv, 0), kn_arg_int64(argv, 1));
    const char *s = kn_arg_text(argv, 2);
    if (!s) { dest[0] = '\0'; return; }
    memcpy(dest, s, strlen(s) + 1);
}

/* The char* backing a text, to pass to a C API.  Borrowed: valid only while
 * that text is, and read-only when the text is a literal. */
void kn_ptr_of_text(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    kn_ret_ptr(ret, kn_arg_text(argv, 0));
}

/* --- raw allocation --------------------------------------------------- */

/* malloc, NOT the runtime allocator: the block may cross into a C API that
 * frees or reallocs it, and the runtime's header-before-payload layout would
 * make that corrupt the heap.  Returns null on failure; the caller checks with
 * ptr_is_null, exactly as C checks malloc. */
void kn_mem_alloc(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    int64_t bytes = kn_arg_int64(argv, 0);
    kn_ret_ptr(ret, bytes < 0 ? NULL : malloc((size_t)bytes));
}

void kn_mem_free(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)ret; (void)argc;
    free(kn_arg_ptr(argv, 0));
}

void kn_mem_zero(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)ret; (void)argc;
    int64_t bytes = kn_arg_int64(argv, 1);
    if (bytes > 0) memset(kn_arg_ptr(argv, 0), 0, (size_t)bytes);
}

void kn_mem_copy(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)ret; (void)argc;
    int64_t bytes = kn_arg_int64(argv, 2);
    if (bytes > 0) memcpy(kn_arg_ptr(argv, 0), kn_arg_ptr(argv, 1), (size_t)bytes);
}
