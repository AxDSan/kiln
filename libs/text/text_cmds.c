/* The "text" support library — string work beyond what core already carries.
 *
 * Core owns length, uppercase, lowercase, trim, substr, find, replace, concat,
 * repeat, reverse, and (inside this prefix) text_eq, text_to_int and
 * text_to_double. Nothing here duplicates those.
 *
 * Text is UTF-8, so every position, length and count below is measured in
 * CHARACTERS, never in bytes — the same rule core's length() and substr()
 * follow. The three decoding helpers are copied from runtime/kn_text.c rather
 * than shared: they are `static` there, and a library sees only the public
 * abi/kiln_abi.h.
 *
 * Case folding is ASCII-only, matching core's uppercase/lowercase. Folding the
 * rest of Unicode needs tables this library deliberately does not carry.
 */
#include <ctype.h>
#include <string.h>
#include "kiln_abi.h"

/* --- small shared plumbing ---------------------------------------------- */

static const char *text_nz(const char *s) { return s ? s : ""; }
static char *text_alloc(long len) { return (char *)kn_malloc(len + 1); }

static char *text_dup(const char *s, long n) {
    char *o = text_alloc(n);
    memcpy(o, s, (size_t)n);
    o[n] = '\0';
    return o;
}
static char *text_empty(void) { return text_dup("", 0); }

/* Byte length of the character starting at `i`; a truncated sequence counts as
 * one byte so malformed input degrades instead of running off the end. */
static long text_u8_len(const char *s, long i, long n) {
    unsigned char b = (unsigned char)s[i];
    long len = 1;
    if ((b & 0xE0) == 0xC0) len = 2;
    else if ((b & 0xF0) == 0xE0) len = 3;
    else if ((b & 0xF8) == 0xF0) len = 4;
    return (i + len > n) ? 1 : len;
}
/* Byte offset of character index `chars`, clamped to the end. */
static long text_u8_offset(const char *s, long n, long chars) {
    long i = 0;
    while (i < n && chars > 0) { i += text_u8_len(s, i, n); chars--; }
    return i;
}
static long text_u8_count(const char *s, long n) {
    long i = 0, c = 0;
    while (i < n) { i += text_u8_len(s, i, n); c++; }
    return c;
}

/* --- predicates (infallible: they never touch the error slot) ------------ */

/* text_starts_with(text s, text prefix) -> bool */
void text_starts_with(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) {
    (void)c;
    const char *s = text_nz(kn_arg_text(argv, 0)), *p = text_nz(kn_arg_text(argv, 1));
    size_t lp = strlen(p);
    kn_ret_bool(r, strlen(s) >= lp && memcmp(s, p, lp) == 0);
}

/* text_ends_with(text s, text suffix) -> bool */
void text_ends_with(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) {
    (void)c;
    const char *s = text_nz(kn_arg_text(argv, 0)), *p = text_nz(kn_arg_text(argv, 1));
    size_t ls = strlen(s), lp = strlen(p);
    kn_ret_bool(r, ls >= lp && memcmp(s + (ls - lp), p, lp) == 0);
}

/* text_contains(text s, text needle) -> bool */
void text_contains(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) {
    (void)c;
    const char *s = text_nz(kn_arg_text(argv, 0)), *n = text_nz(kn_arg_text(argv, 1));
    kn_ret_bool(r, strstr(s, n) != 0);
}

/* text_equals_ignore_case(text a, text b) -> bool (ASCII folding) */
void text_equals_ignore_case(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) {
    (void)c;
    const char *a = text_nz(kn_arg_text(argv, 0)), *b = text_nz(kn_arg_text(argv, 1));
    while (*a && *b) {
        if (tolower((unsigned char)*a) != tolower((unsigned char)*b)) { kn_ret_bool(r, 0); return; }
        a++; b++;
    }
    kn_ret_bool(r, *a == '\0' && *b == '\0');
}

/* --- positions and counts (infallible; -1 is a genuine "not found") ------ */

/* text_index_of(text s, text needle) -> int : character index, -1 when absent */
void text_index_of(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) {
    (void)c;
    const char *s = text_nz(kn_arg_text(argv, 0)), *n = text_nz(kn_arg_text(argv, 1));
    const char *hit = strstr(s, n);
    /* A position counting from 1, and 0 for absent — 0 is not a position, so
     * it can carry that meaning without a magic -1. */
    kn_ret_int(r, hit ? (int32_t)text_u8_count(s, (long)(hit - s)) + 1 : 0);
}

/* text_last_index_of(text s, text needle) -> int : character index, -1 absent */
void text_last_index_of(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) {
    (void)c;
    const char *s = text_nz(kn_arg_text(argv, 0)), *n = text_nz(kn_arg_text(argv, 1));
    long ln = (long)strlen(n);
    const char *last = 0, *p = s;
    if (ln == 0) { kn_ret_int(r, (int32_t)text_u8_count(s, (long)strlen(s)) + 1); return; }
    while ((p = strstr(p, n))) { last = p; p += 1; }
    kn_ret_int(r, last ? (int32_t)text_u8_count(s, (long)(last - s)) + 1 : 0);
}

/* text_count(text s, text needle) -> int : non-overlapping occurrences, 0 for
 * an empty needle (there is no useful answer, and 0 is not a failure). */
void text_count(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) {
    (void)c;
    const char *s = text_nz(kn_arg_text(argv, 0)), *n = text_nz(kn_arg_text(argv, 1));
    long ln = (long)strlen(n), count = 0;
    if (ln == 0) { kn_ret_int(r, 0); return; }
    for (const char *p = s; (p = strstr(p, n)); p += ln) count++;
    kn_ret_int(r, (int32_t)count);
}

/* text_compare(text a, text b) -> int : -1, 0 or 1.
 * UTF-8 byte order is code-point order, so a plain byte comparison already
 * sorts characters correctly. */
void text_compare(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) {
    (void)c;
    const char *a = text_nz(kn_arg_text(argv, 0)), *b = text_nz(kn_arg_text(argv, 1));
    int d = strcmp(a, b);
    kn_ret_int(r, d < 0 ? -1 : (d > 0 ? 1 : 0));
}

/* --- builders (infallible: out-of-range positions clamp, as substr does) -- */

/* text_trim_start(text s) -> text */
void text_trim_start(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) {
    (void)c;
    const char *s = text_nz(kn_arg_text(argv, 0));
    const char *a = s;
    while (*a && isspace((unsigned char)*a)) a++;
    kn_ret_text(r, text_dup(a, (long)strlen(a)));
}

/* text_trim_end(text s) -> text */
void text_trim_end(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) {
    (void)c;
    const char *s = text_nz(kn_arg_text(argv, 0));
    const char *e = s + strlen(s);
    while (e > s && isspace((unsigned char)e[-1])) e--;
    kn_ret_text(r, text_dup(s, (long)(e - s)));
}

/* Shared body of the two pads. `width` is in characters; `pad` contributes its
 * FIRST character, and an empty pad means a space, so padding never produces
 * text shorter than it claims. */
static void text_pad(Kiln_Slot *r, Kiln_Slot *argv, int left) {
    const char *s = text_nz(kn_arg_text(argv, 0));
    int32_t width = kn_arg_int(argv, 1);
    const char *pad = text_nz(kn_arg_text(argv, 2));
    long n = (long)strlen(s), chars = text_u8_count(s, n);
    long plen;
    if (*pad == '\0') { pad = " "; }
    plen = text_u8_len(pad, 0, (long)strlen(pad));
    if (width < 0) width = 0;
    if (chars >= (long)width) { kn_ret_text(r, text_dup(s, n)); return; }
    {
        long fill = (long)width - chars;
        char *o = text_alloc(n + fill * plen), *w = o;
        if (!left) { memcpy(w, s, (size_t)n); w += n; }
        for (long i = 0; i < fill; i++) { memcpy(w, pad, (size_t)plen); w += plen; }
        if (left) { memcpy(w, s, (size_t)n); w += n; }
        *w = '\0';
        kn_ret_text(r, o);
    }
}

/* text_pad_left(text s, int width, text pad) -> text */
void text_pad_left(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) { (void)c; text_pad(r, argv, 1); }
/* text_pad_right(text s, int width, text pad) -> text */
void text_pad_right(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) { (void)c; text_pad(r, argv, 0); }

/* text_title_case(text s) -> text : ASCII letters only. A word starts after
 * whitespace; a multi-byte character is left alone and does not start a word,
 * so "o'neill" and "café au lait" come out as expected rather than mangled. */
void text_title_case(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) {
    (void)c;
    const char *s = text_nz(kn_arg_text(argv, 0));
    long n = (long)strlen(s);
    char *o = text_alloc(n);
    int at_start = 1;
    for (long i = 0; i < n; i++) {
        unsigned char b = (unsigned char)s[i];
        if (isspace(b)) { o[i] = (char)b; at_start = 1; continue; }
        if (b < 0x80 && isalpha(b)) {
            o[i] = (char)(at_start ? toupper(b) : tolower(b));
        } else {
            o[i] = (char)b;
        }
        at_start = 0;
    }
    o[n] = '\0';
    kn_ret_text(r, o);
}

/* text_insert(text s, int at, text piece) -> text : `at` is a character
 * position counting from 1, clamped to [1, length + 1]. */
void text_insert(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) {
    (void)c;
    const char *s = text_nz(kn_arg_text(argv, 0));
    int32_t at = kn_arg_int(argv, 1);
    const char *piece = text_nz(kn_arg_text(argv, 2));
    long n = (long)strlen(s), lp = (long)strlen(piece), cut;
    if (at < 1) at = 1;
    cut = text_u8_offset(s, n, at - 1);
    {
        char *o = text_alloc(n + lp);
        memcpy(o, s, (size_t)cut);
        memcpy(o + cut, piece, (size_t)lp);
        memcpy(o + cut + lp, s + cut, (size_t)(n - cut));
        o[n + lp] = '\0';
        kn_ret_text(r, o);
    }
}

/* text_remove(text s, int at, int count) -> text : characters from position
 * `at`, counting from 1, clamped. */
void text_remove(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) {
    (void)c;
    const char *s = text_nz(kn_arg_text(argv, 0));
    int32_t at = kn_arg_int(argv, 1), count = kn_arg_int(argv, 2);
    long n = (long)strlen(s), from, to;
    if (at < 1) at = 1;
    if (count < 0) count = 0;
    from = text_u8_offset(s, n, at - 1);
    to = text_u8_offset(s, n, (long)at - 1 + count);
    {
        long out = from + (n - to);
        char *o = text_alloc(out);
        memcpy(o, s, (size_t)from);
        memcpy(o + from, s + to, (size_t)(n - to));
        o[out] = '\0';
        kn_ret_text(r, o);
    }
}

/* --- characters and code points (fallible) ------------------------------ */

/* Decode the code point at byte offset `i`; a malformed byte decodes to its own
 * value, which is what core's reverse does with the same input. */
static int32_t text_decode(const char *s, long i, long n, long *len_out) {
    unsigned char b = (unsigned char)s[i];
    long len = text_u8_len(s, i, n);
    int32_t cp;
    *len_out = len;
    if (len == 1) return (int32_t)b;
    if (len == 2) cp = b & 0x1F; else if (len == 3) cp = b & 0x0F; else cp = b & 0x07;
    for (long k = 1; k < len; k++) {
        unsigned char cb = (unsigned char)s[i + k];
        if ((cb & 0xC0) != 0x80) { *len_out = 1; return (int32_t)b; }
        cp = (cp << 6) | (cb & 0x3F);
    }
    return cp;
}

/* text_char_at(text s, int i) -> text : the single character at position `i`.
 * "" with error code 0 cannot happen — an out-of-range index is a failure. */
void text_char_at(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) {
    (void)c;
    const char *s = text_nz(kn_arg_text(argv, 0));
    int32_t i = kn_arg_int(argv, 1);
    long n = (long)strlen(s), from, len;
    if (i < 1 || (long)i > text_u8_count(s, n)) {
        kn_error_set(KN_ERR_INVALID_ARG, "text_char_at: index out of range");
        kn_ret_text(r, text_empty());
        return;
    }
    from = text_u8_offset(s, n, i - 1);
    len = text_u8_len(s, from, n);
    kn_error_clear();
    kn_ret_text(r, text_dup(s + from, len));
}

/* text_char_code(text s, int i) -> int : code point at position `i`, counting
 * from 1; -1 on failure. */
void text_char_code(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) {
    (void)c;
    const char *s = text_nz(kn_arg_text(argv, 0));
    int32_t i = kn_arg_int(argv, 1);
    long n = (long)strlen(s), from, len;
    if (i < 1 || (long)i > text_u8_count(s, n)) {
        kn_error_set(KN_ERR_INVALID_ARG, "text_char_code: index out of range");
        kn_ret_int(r, -1);
        return;
    }
    from = text_u8_offset(s, n, i - 1);
    kn_error_clear();
    kn_ret_int(r, text_decode(s, from, n, &len));
}

/* text_from_code(int code) -> text : one character, "" on failure. Surrogates
 * and anything past U+10FFFF are rejected rather than encoded, because the
 * result would not be valid UTF-8 and every later command would inherit it. */
void text_from_code(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) {
    (void)c;
    int32_t cp = kn_arg_int(argv, 0);
    char buf[4];
    long len;
    if (cp < 0 || cp > 0x10FFFF || (cp >= 0xD800 && cp <= 0xDFFF)) {
        kn_error_set(KN_ERR_INVALID_ARG, "text_from_code: not a Unicode code point");
        kn_ret_text(r, text_empty());
        return;
    }
    if (cp < 0x80) { buf[0] = (char)cp; len = 1; }
    else if (cp < 0x800) {
        buf[0] = (char)(0xC0 | (cp >> 6)); buf[1] = (char)(0x80 | (cp & 0x3F)); len = 2;
    } else if (cp < 0x10000) {
        buf[0] = (char)(0xE0 | (cp >> 12));
        buf[1] = (char)(0x80 | ((cp >> 6) & 0x3F));
        buf[2] = (char)(0x80 | (cp & 0x3F)); len = 3;
    } else {
        buf[0] = (char)(0xF0 | (cp >> 18));
        buf[1] = (char)(0x80 | ((cp >> 12) & 0x3F));
        buf[2] = (char)(0x80 | ((cp >> 6) & 0x3F));
        buf[3] = (char)(0x80 | (cp & 0x3F)); len = 4;
    }
    kn_error_clear();
    kn_ret_text(r, text_dup(buf, len));
}

/* --- splitting: a count plus an indexed accessor ------------------------ */
/* There is no array type, so a split is exposed the way every other collection
 * is. Empty fields are kept: "a,,b" is three fields and "a," is two, so a
 * trailing separator is visible rather than silently swallowed. */

/* Byte span of field `want` (0-based). Returns 0 if there is no such field. */
static int text_field(const char *s, const char *sep, long lsep, int32_t want,
                      const char **out, long *out_len) {
    const char *p = s;
    int32_t idx = 0;
    for (;;) {
        const char *hit = strstr(p, sep);
        if (idx == want) {
            *out = p;
            *out_len = hit ? (long)(hit - p) : (long)strlen(p);
            return 1;
        }
        if (!hit) return 0;
        p = hit + lsep;
        idx++;
    }
}

/* text_split_count(text s, text sep) -> int : field count, -1 on failure. */
void text_split_count(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) {
    (void)c;
    const char *s = text_nz(kn_arg_text(argv, 0)), *sep = text_nz(kn_arg_text(argv, 1));
    long lsep = (long)strlen(sep), count = 1;
    if (lsep == 0) {
        kn_error_set(KN_ERR_INVALID_ARG, "text_split_count: separator is empty");
        kn_ret_int(r, -1);
        return;
    }
    for (const char *p = s; (p = strstr(p, sep)); p += lsep) count++;
    kn_error_clear();
    kn_ret_int(r, (int32_t)count);
}

/* text_split_at(text s, text sep, int i) -> text : field `i`, counting from 1;
 * "" on failure. */
void text_split_at(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) {
    (void)c;
    const char *s = text_nz(kn_arg_text(argv, 0)), *sep = text_nz(kn_arg_text(argv, 1));
    int32_t i = kn_arg_int(argv, 2);
    long lsep = (long)strlen(sep), flen = 0;
    const char *field = 0;
    if (lsep == 0) {
        kn_error_set(KN_ERR_INVALID_ARG, "text_split_at: separator is empty");
        kn_ret_text(r, text_empty());
        return;
    }
    if (i < 1 || !text_field(s, sep, lsep, i - 1, &field, &flen)) {
        kn_error_set(KN_ERR_INVALID_ARG, "text_split_at: index out of range");
        kn_ret_text(r, text_empty());
        return;
    }
    kn_error_clear();
    kn_ret_text(r, text_dup(field, flen));
}
