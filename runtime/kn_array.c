/* Arrays and byte-sets — the types that hold more than one value.
 *
 * Both are one runtime-owned allocation whose layout is stated in
 * abi/kiln_abi.h, and both live in the slot as a pointer, exactly the way
 * text does.  That is the whole trick: the slot's value field is eight bytes
 * and a pointer is eight bytes, so nothing in the marshaling path had to widen
 * to make aggregates work.
 *
 * NOTHING HERE EVER MOVES AN ARRAY.  `append` allocates a new one and copies,
 * rather than growing the old one through E_MRealloc, because a program may
 * hold the same array under two names — and reallocating would leave the other
 * name addressing freed memory.  The cost is a copy per append; the alternative
 * is the class of bug the handle table exists to make impossible.  `cap` is
 * still recorded truthfully, since the header states it and a wrong number in a
 * header is worse than a redundant one.
 *
 * Indexing is reached through the plain helpers at the top rather than through
 * the slot ABI: it is syntax, and building an argv array to read one element
 * would cost more code than the read.  They move raw 64-bit values, which is
 * what a slot's value field already holds.
 */
#include <stdlib.h>
#include <string.h>
#include <stdio.h>
#include "kiln_core.h"

/* Elements are one slot-width each, so an array of text holds pointers the
 * same way an array of int holds ints. */
static int64_t *elems(Kiln_Array *a) { return (int64_t *)(a + 1); }
static unsigned char *bin_bytes(Kiln_Bin *b) { return (unsigned char *)(b + 1); }

/* An array that was never created reads as empty rather than as an error: a
 * module-level `var xs: int[]` is zero until its initializer runs, and a
 * program asking how long nothing is deserves 0, not a diagnostic. */
static int32_t ary_len(const Kiln_Array *a) { return a ? a->len : 0; }

void *kn_ary_new(int32_t tag, int32_t len) {
    if (len < 0) len = 0;
    Kiln_Array *a = (Kiln_Array *)kn_malloc(
        (long)sizeof(Kiln_Array) + (long)len * 8);
    if (!a) return NULL;
    a->elem_tag = tag;
    a->len = len;
    a->cap = len;
    a->_pad = 0;
    memset(elems(a), 0, (size_t)len * 8);
    return a;
}

/* Out of range fails LOUDLY and returns a sentinel: reading whatever follows
 * the array is the one outcome an index must never have.  Text gets "" rather
 * than a null pointer, so a failed read stays printable. */
int64_t kn_ary_get(void *p, int32_t i) {
    Kiln_Array *a = (Kiln_Array *)p;
    if (!a || i < 1 || i > a->len) {
        char msg[96];
        snprintf(msg, sizeof msg, "index %d is outside a list of %d element(s)",
                 (int)i, (int)ary_len(a));
        kn_error_set(KN_ERR_OUT_OF_RANGE, msg);
        if (a && a->elem_tag == KN_SDT_TEXT) return (int64_t)(intptr_t)kn_empty_text();
        return 0;
    }
    kn_error_clear();
    return elems(a)[i - 1];
}

void kn_ary_set(void *p, int32_t i, int64_t v) {
    Kiln_Array *a = (Kiln_Array *)p;
    if (!a || i < 1 || i > a->len) {
        char msg[96];
        snprintf(msg, sizeof msg, "index %d is outside a list of %d element(s)",
                 (int)i, (int)ary_len(a));
        kn_error_set(KN_ERR_OUT_OF_RANGE, msg);
        return;
    }
    elems(a)[i - 1] = v;
    kn_error_clear();
}

void *kn_bin_new(int32_t len) {
    if (len < 0) len = 0;
    Kiln_Bin *b = (Kiln_Bin *)kn_malloc((long)sizeof(Kiln_Bin) + len);
    if (!b) return NULL;
    b->dims = 1;
    b->len = len;
    memset(bin_bytes(b), 0, (size_t)len);
    return b;
}

int32_t kn_bin_at(void *p, int32_t i) {
    Kiln_Bin *b = (Kiln_Bin *)p;
    if (!b || i < 1 || i > b->len) {
        char msg[96];
        snprintf(msg, sizeof msg, "index %d is outside %d byte(s)",
                 (int)i, b ? (int)b->len : 0);
        kn_error_set(KN_ERR_OUT_OF_RANGE, msg);
        return -1;
    }
    kn_error_clear();
    return bin_bytes(b)[i - 1];
}

void kn_bin_set(void *p, int32_t i, int32_t v) {
    Kiln_Bin *b = (Kiln_Bin *)p;
    if (!b || i < 1 || i > b->len) {
        char msg[96];
        snprintf(msg, sizeof msg, "index %d is outside %d byte(s)",
                 (int)i, b ? (int)b->len : 0);
        kn_error_set(KN_ERR_OUT_OF_RANGE, msg);
        return;
    }
    bin_bytes(b)[i - 1] = (unsigned char)(v & 0xFF);
    kn_error_clear();
}

/* --- comparing and formatting one element ------------------------------
 * Every element is 64 raw bits; only the array's tag says what they mean, so
 * these two functions are the single place that knows. */
static const char *as_text(int64_t v) {
    const char *s = (const char *)(intptr_t)v;
    return s ? s : "";
}

static int elem_cmp(int32_t tag, int64_t x, int64_t y) {
    if (tag == KN_SDT_TEXT) return strcmp(as_text(x), as_text(y));
    if (tag == KN_SDT_DOUBLE) {
        double a, b;
        memcpy(&a, &x, 8); memcpy(&b, &y, 8);
        return (a < b) ? -1 : (a > b) ? 1 : 0;
    }
    return (x < y) ? -1 : (x > y) ? 1 : 0;
}

/* Formatting goes through the same spellings as int_to_text/double_to_text, so
 * a joined list and a printed element never disagree about what a number
 * looks like. */
static void elem_text(int32_t tag, int64_t v, char *out, size_t n) {
    switch (tag) {
        case KN_SDT_INT:    snprintf(out, n, "%d", (int)(int32_t)v); break;
        case KN_SDT_INT64:  snprintf(out, n, "%lld", (long long)v); break;
        case KN_SDT_DOUBLE: { double d; memcpy(&d, &v, 8); snprintf(out, n, "%g", d); break; }
        case KN_SDT_BOOL:   snprintf(out, n, "%s", v ? "true" : "false"); break;
        default:            snprintf(out, n, "%s", as_text(v)); break;
    }
}

/* qsort's comparator carries no context, and the runtime is single-threaded,
 * so the tag of the array being sorted is parked here for the duration. */
static int32_t g_sort_tag = KN_SDT_INT;
static int sort_cmp(const void *x, const void *y) {
    return elem_cmp(g_sort_tag, *(const int64_t *)x, *(const int64_t *)y);
}

/* --- commands ---------------------------------------------------------- */

static Kiln_Array *arg_ary(Kiln_Slot *argv, int i) {
    return (Kiln_Array *)argv[i].v.ptr;
}
static Kiln_Bin *arg_bin(Kiln_Slot *argv, int i) {
    return (Kiln_Bin *)argv[i].v.ptr;
}

void kn_ary_count(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) {
    (void)c;
    kn_ret_int(r, ary_len(arg_ary(argv, 0)));
}

/* A NEW array, longer by one. See the file header for why this copies. */
void kn_ary_append(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) {
    (void)c;
    Kiln_Array *a = arg_ary(argv, 0);
    int32_t n = ary_len(a);
    int32_t tag = a ? a->elem_tag : argv[1].tag;
    Kiln_Array *out = (Kiln_Array *)kn_ary_new(tag, n + 1);
    if (!out) { r->tag = KN_SDT_BIN; r->v.ptr = NULL; return; }
    if (n) memcpy(elems(out), elems(a), (size_t)n * 8);
    elems(out)[n] = argv[1].v.i64;
    r->tag = KN_SDT_ARRAY_OF(tag);
    r->v.ptr = out;
}

/* A run of elements, from `start`, `count` of them, as a new array.
 *
 * The bounds are CLAMPED rather than refused, and clamped exactly the way
 * `substr` does it — which is the only slice the language had before this one,
 * and two slices that disagree about the same out-of-range request would be
 * worse than either rule on its own. So a start below 1 reads from 1, a count
 * that runs past the end stops at the end, and a request entirely outside the
 * array is the empty array. `xs[a..b]` reaches here, and a slice is where a
 * program asks how much is there: failing would mean writing the bounds check
 * the slice was meant to be.
 *
 * Elements are copied as the 64-bit values they are, exactly as `append` does:
 * an array of text ends up holding the same pointers, not copies of the text. */
void kn_ary_slice(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) {
    (void)c;
    Kiln_Array *a = arg_ary(argv, 0);
    int32_t len = ary_len(a);
    int32_t start = kn_arg_int(argv, 1), count = kn_arg_int(argv, 2);
    int32_t tag = a ? a->elem_tag : KN_SDT_INT;
    if (start < 1) start = 1;
    if (count < 0) count = 0;
    if (start > len) count = 0;
    else if (start - 1 + count > len) count = len - (start - 1);
    Kiln_Array *out = (Kiln_Array *)kn_ary_new(tag, count);
    if (!out) { r->tag = KN_SDT_ARRAY_OF(tag); r->v.ptr = NULL; return; }
    if (count) memcpy(elems(out), elems(a) + (start - 1), (size_t)count * 8);
    kn_error_clear();
    r->tag = KN_SDT_ARRAY_OF(tag);
    r->v.ptr = out;
}

/* In place: removing shortens, and shortening never needs to move anything.
 * That is why `remove` is a statement and `append` is a value. */
void kn_ary_remove(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) {
    (void)c; (void)r;
    Kiln_Array *a = arg_ary(argv, 0);
    int32_t i = kn_arg_int(argv, 1);
    if (!a || i < 1 || i > a->len) {
        char msg[96];
        snprintf(msg, sizeof msg, "index %d is outside a list of %d element(s)",
                 (int)i, (int)ary_len(a));
        kn_error_set(KN_ERR_OUT_OF_RANGE, msg);
        return;
    }
    memmove(elems(a) + i - 1, elems(a) + i, (size_t)(a->len - i) * 8);
    a->len--;
    kn_error_clear();
}

void kn_ary_sort(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) {
    (void)c; (void)r;
    Kiln_Array *a = arg_ary(argv, 0);
    if (!a || a->len < 2) return;
    g_sort_tag = a->elem_tag;
    qsort(elems(a), (size_t)a->len, 8, sort_cmp);
}

/* Returns a 1-based position, or 0 for absent.  Nothing indexes from 0, so 0
 * is free to mean "not there" — which retires the -1 that a 0-based language
 * needs and that reads as a position until you know better. */
static int32_t find_elem(Kiln_Array *a, int64_t want) {
    for (int32_t i = 0; i < ary_len(a); i++) {
        if (elem_cmp(a->elem_tag, elems(a)[i], want) == 0) return i + 1;
    }
    return 0;
}

void kn_ary_contains(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) {
    (void)c;
    kn_ret_bool(r, find_elem(arg_ary(argv, 0), argv[1].v.i64) > 0);
}

void kn_ary_index_of(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) {
    (void)c;
    kn_ret_int(r, find_elem(arg_ary(argv, 0), argv[1].v.i64));
}

void kn_ary_join(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) {
    (void)c;
    Kiln_Array *a = arg_ary(argv, 0);
    const char *sep = kn_arg_text(argv, 1);
    if (!sep) sep = "";
    size_t seplen = strlen(sep), total = 0;
    int32_t n = ary_len(a);
    char buf[64];
    /* Measure, then fill: one allocation of the right size, no growing. */
    for (int32_t i = 0; i < n; i++) {
        if (a->elem_tag == KN_SDT_TEXT) {
            total += strlen(as_text(elems(a)[i]));
        } else {
            elem_text(a->elem_tag, elems(a)[i], buf, sizeof buf);
            total += strlen(buf);
        }
        if (i + 1 < n) total += seplen;
    }
    char *out = (char *)kn_malloc((long)total + 1);
    if (!out) { kn_ret_text(r, NULL); return; }
    char *w = out;
    for (int32_t i = 0; i < n; i++) {
        const char *piece;
        if (a->elem_tag == KN_SDT_TEXT) {
            piece = as_text(elems(a)[i]);
        } else {
            elem_text(a->elem_tag, elems(a)[i], buf, sizeof buf);
            piece = buf;
        }
        size_t plen = strlen(piece);
        memcpy(w, piece, plen); w += plen;
        if (i + 1 < n) { memcpy(w, sep, seplen); w += seplen; }
    }
    *w = '\0';
    kn_ret_text(r, out);
}

/* The other direction: text in, list out.  This is what a program reads a file
 * into before it can do anything with the lines. */
void kn_ary_split(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) {
    (void)c;
    const char *s = kn_arg_text(argv, 0), *sep = kn_arg_text(argv, 1);
    if (!s) s = "";
    /* An empty separator has no answer that is not arbitrary — every position
     * matches it — so it is refused rather than guessed at. */
    if (!sep || !*sep) {
        kn_error_set(KN_ERR_INVALID_ARG, "split needs a separator to split on");
        r->tag = KN_SDT_ARRAY_OF(KN_SDT_TEXT);
        r->v.ptr = kn_ary_new(KN_SDT_TEXT, 0);
        return;
    }
    size_t seplen = strlen(sep);
    int32_t n = 1;
    for (const char *p = s; (p = strstr(p, sep)); p += seplen) n++;
    Kiln_Array *out = (Kiln_Array *)kn_ary_new(KN_SDT_TEXT, n);
    if (!out) { r->tag = KN_SDT_ARRAY_OF(KN_SDT_TEXT); r->v.ptr = NULL; return; }
    const char *p = s;
    for (int32_t i = 0; i < n; i++) {
        const char *hit = strstr(p, sep);
        size_t len = hit ? (size_t)(hit - p) : strlen(p);
        char *piece = (char *)kn_malloc((long)len + 1);
        if (!piece) break;
        memcpy(piece, p, len);
        piece[len] = '\0';
        elems(out)[i] = (int64_t)(intptr_t)piece;
        if (!hit) break;
        p = hit + seplen;
    }
    r->tag = KN_SDT_ARRAY_OF(KN_SDT_TEXT);
    r->v.ptr = out;
    kn_error_clear();
}

/* --- byte-sets --------------------------------------------------------- */

void kn_bin_make(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) {
    (void)c;
    r->tag = KN_SDT_BIN;
    r->v.ptr = kn_bin_new(kn_arg_int(argv, 0));
}

void kn_bin_size(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) {
    (void)c;
    Kiln_Bin *b = arg_bin(argv, 0);
    kn_ret_int(r, b ? b->len : 0);
}

void kn_bin_byte(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) {
    (void)c;
    kn_ret_int(r, kn_bin_at(argv[0].v.ptr, kn_arg_int(argv, 1)));
}

void kn_bin_put(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) {
    (void)c; (void)r;
    kn_bin_set(argv[0].v.ptr, kn_arg_int(argv, 1), kn_arg_int(argv, 2));
}

/* A run of bytes, clamped exactly as `slice` and `substr` are — see the note on
 * `kn_ary_slice`. Positions count from 1, and the count is how many bytes. */
void kn_bin_slice(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) {
    (void)c;
    Kiln_Bin *b = arg_bin(argv, 0);
    int32_t len = b ? b->len : 0;
    int32_t start = kn_arg_int(argv, 1), count = kn_arg_int(argv, 2);
    if (start < 1) start = 1;
    if (count < 0) count = 0;
    if (start > len) count = 0;
    else if (start - 1 + count > len) count = len - (start - 1);
    Kiln_Bin *out = (Kiln_Bin *)kn_bin_new(count);
    if (out && count) memcpy(bin_bytes(out), bin_bytes(b) + (start - 1), (size_t)count);
    kn_error_clear();
    r->tag = KN_SDT_BIN;
    r->v.ptr = out;
}

/* --- the bridge between a byte-set and an address -------------------------
 *
 * A `bytes` value has no address a program can name, and a c-record has one
 * (`address of rec`) but no way to be filled from a byte-set. Without these
 * two, moving a wire frame into a record is a `bytes_at` loop and moving one
 * out is a `bytes_set` loop — the offset arithmetic the record was supposed
 * to replace.
 *
 * Neither command can check that `p` points at `count` writable bytes: `ptr`
 * is an address the program vouched for, exactly as `mem_copy` is. What they
 * do check is the side they own — a negative count, and the byte-set's own
 * length — so the failure that is knowable is refused rather than discovered
 * as a corrupted record.
 */

/* bytes_from_ptr(p, count) -> bytes: `count` bytes copied out of an address. */
void kn_bin_from_ptr(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) {
    (void)c;
    void *p = kn_arg_ptr(argv, 0);
    int32_t count = kn_arg_int(argv, 1);
    if (count < 0) count = 0;
    if (!p && count > 0) {
        kn_error_set(KN_ERR_INVALID_ARG, "bytes_from_ptr: the address is null");
        r->tag = KN_SDT_BIN;
        r->v.ptr = kn_bin_new(0);
        return;
    }
    Kiln_Bin *out = (Kiln_Bin *)kn_bin_new(count);
    if (out && count) memcpy(bin_bytes(out), p, (size_t)count);
    kn_error_clear();
    r->tag = KN_SDT_BIN;
    r->v.ptr = out;
}

/* bytes_copy_to_ptr(b, p) -> int: the byte-set written at an address, and how
 * many bytes that was. The count is the answer rather than a bool because the
 * caller almost always wants it — it is the frame's length. */
void kn_bin_to_ptr(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) {
    (void)c;
    Kiln_Bin *b = arg_bin(argv, 0);
    void *p = kn_arg_ptr(argv, 1);
    int32_t n = b ? b->len : 0;
    if (!p && n > 0) {
        kn_error_set(KN_ERR_INVALID_ARG, "bytes_copy_to_ptr: the address is null");
        kn_ret_int(r, -1);
        return;
    }
    if (n) memcpy(p, bin_bytes(b), (size_t)n);
    kn_error_clear();
    kn_ret_int(r, n);
}

/* bytes_concat(a, b) -> bytes: the two runs, end to end. A frame header and
 * its body are two byte-sets and one write. */
void kn_bin_concat(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) {
    (void)c;
    Kiln_Bin *x = arg_bin(argv, 0);
    Kiln_Bin *y = arg_bin(argv, 1);
    int32_t xn = x ? x->len : 0, yn = y ? y->len : 0;
    Kiln_Bin *out = (Kiln_Bin *)kn_bin_new(xn + yn);
    if (out) {
        if (xn) memcpy(bin_bytes(out), bin_bytes(x), (size_t)xn);
        if (yn) memcpy(bin_bytes(out) + xn, bin_bytes(y), (size_t)yn);
    }
    kn_error_clear();
    r->tag = KN_SDT_BIN;
    r->v.ptr = out;
}

/* Text is UTF-8, so its bytes ARE its encoding: the round trip is exact, and
 * the byte count of text with an accent in it is larger than its length. */
void kn_bin_from_text(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) {
    (void)c;
    const char *s = kn_arg_text(argv, 0);
    if (!s) s = "";
    size_t n = strlen(s);
    Kiln_Bin *b = (Kiln_Bin *)kn_bin_new((int32_t)n);
    if (b) memcpy(bin_bytes(b), s, n);
    r->tag = KN_SDT_BIN;
    r->v.ptr = b;
}

/* A NUL byte anywhere would truncate the result, so it stops there rather than
 * handing back text whose length disagrees with the bytes behind it. */
void kn_bin_to_text(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) {
    (void)c;
    Kiln_Bin *b = arg_bin(argv, 0);
    int32_t n = b ? b->len : 0;
    for (int32_t i = 0; i < n; i++) {
        if (bin_bytes(b)[i] == 0) { n = i; break; }
    }
    char *out = (char *)kn_malloc((long)n + 1);
    if (!out) { kn_ret_text(r, NULL); return; }
    if (n) memcpy(out, bin_bytes(b), (size_t)n);
    out[n] = '\0';
    kn_ret_text(r, out);
}
