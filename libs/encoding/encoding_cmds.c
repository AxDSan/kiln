/* The `encoding` library — the text encodings data files use, in and out.
 *
 * These files were written for a Chinese market in 2008, and their text is not
 * UTF-8: tables declare GB2312 over bytes that are really GBK, some files are
 * UTF-16LE, and the item, quest and NPC names the server has to send back are
 * in the same codepage the client reads.  Kiln text is UTF-8, so exactly one
 * place has to know how to turn one into the other, and this is it.
 *
 * There is no conversion table in this source.  GBK is 23,940 mappings, and
 * hand-copying them is how a decoder ends up 99% right and silently wrong on a
 * name.  The conversion is the platform's own instead — iconv on POSIX, the
 * Win32 codepage API on Windows — behind a thin shim, which is the shape
 * libs/README asks for.  Neither is a new dependency: both ship with the C
 * library the target already links.  Latin-1 and both UTF-16 byte orders are
 * written out here rather than delegated, because each is a byte loop and
 * sending them through two platform APIs would be two more places to be wrong.
 *
 * Two decoders rather than one, because a program replacing its own reader
 * The C# server being replaced reads GBK through a `StreamReader`, which
 * REPLACES malformed input with U+FFFD and carries on — so a faithful port has
 * to be able to do the same.  `encoding_decode` is the other half of the pair:
 * it refuses the input and names the byte offset that failed, which is what a
 * program wants when it is checking a file rather than consuming one.
 *
 * Failure follows the house rule: text for a failure is "", bytes are an EMPTY
 * byte-set (never a null pointer), a yes/no is false, and the reason is left in
 * the error slot for `last_error_code` / `last_error_text`.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "kiln_abi.h"
#include "kiln_core.h"      /* kn_bin_new — a library builds byte-sets too */

#ifdef _WIN32
#include <windows.h>
#else
#include <errno.h>
#include <iconv.h>
#endif

/* --- names -------------------------------------------------------------- */

enum { ENC_ICONV = 0, ENC_LATIN1, ENC_UTF16LE, ENC_UTF16BE, ENC_UTF8 };

typedef struct {
    const char *canonical;
    const char *iconv_name;    /* POSIX; NULL when the mode handles it      */
    int32_t     cp;            /* Windows codepage; 0 when handled here     */
    int32_t     mode;
    const char *aliases[6];
} Encoding;

static const Encoding ENCODINGS[] = {
    { "gbk",       "GBK",       936,   ENC_ICONV,
      { "gb2312", "cp936", "ms936", "windows-936", "gb-2312", "936" } },
    { "gb18030",   "GB18030",   54936, ENC_ICONV,
      { "cp54936", "gb-18030", "54936", NULL, NULL, NULL } },
    { "big5",      "BIG5",      950,   ENC_ICONV,
      { "cp950", "big-5", "950", NULL, NULL, NULL } },
    { "shift-jis", "SHIFT_JIS", 932,   ENC_ICONV,
      { "sjis", "cp932", "932", "shift_jis", NULL, NULL } },
    { "latin1",    NULL,        0,     ENC_LATIN1,
      { "latin-1", "iso-8859-1", "iso8859-1", "windows-1252", "cp1252", "1252" } },
    { "utf-16le",  NULL,        0,     ENC_UTF16LE,
      { "utf16le", "utf-16-le", "ucs-2le", "unicode", NULL, NULL } },
    { "utf-16be",  NULL,        0,     ENC_UTF16BE,
      { "utf16be", "utf-16-be", "ucs-2be", NULL, NULL, NULL } },
    { "utf-8",     "UTF-8",     65001, ENC_UTF8,
      { "utf8", "cp65001", "65001", NULL, NULL, NULL } },
};

#define ENC_COUNT ((int32_t)(sizeof ENCODINGS / sizeof ENCODINGS[0]))

/* ASCII case-insensitive, written out rather than taken from <strings.h>:
 * `strcasecmp` does not exist on Windows and `stricmp` does not exist here, and
 * an encoding name is ASCII whatever locale the machine is in. */
static int32_t enc_ieq(const char *a, const char *b) {
    for (; *a && *b; a++, b++) {
        char x = *a, y = *b;
        if (x >= 'A' && x <= 'Z') x = (char)(x + 32);
        if (y >= 'A' && y <= 'Z') y = (char)(y + 32);
        if (x != y) return 0;
    }
    return *a == *b;
}

static const Encoding *enc_find(const char *name) {
    if (!name) return NULL;
    for (int32_t i = 0; i < ENC_COUNT; i++) {
        const Encoding *e = &ENCODINGS[i];
        if (enc_ieq(e->canonical, name)) return e;
        for (int32_t a = 0; a < 6 && e->aliases[a]; a++) {
            if (enc_ieq(e->aliases[a], name)) return e;
        }
    }
    return NULL;
}

/* --- UTF-8, the language's own encoding --------------------------------- */

/* One code point, or 0 for a malformed sequence.  Overlong forms, surrogates
 * and anything past U+10FFFF are malformed — which matters, because these are
 * the bytes a lossy decode of a broken file would otherwise launder. */
static int32_t u8_next(const char *p, int32_t avail, uint32_t *cp) {
    const unsigned char *u = (const unsigned char *)p;
    if (avail < 1) return 0;
    if (u[0] < 0x80) { *cp = u[0]; return 1; }

    int32_t need;
    uint32_t v;
    if ((u[0] & 0xE0) == 0xC0) { need = 1; v = u[0] & 0x1F; }
    else if ((u[0] & 0xF0) == 0xE0) { need = 2; v = u[0] & 0x0F; }
    else if ((u[0] & 0xF8) == 0xF0) { need = 3; v = u[0] & 0x07; }
    else return 0;
    if (avail < need + 1) return 0;

    for (int32_t i = 1; i <= need; i++) {
        if ((u[i] & 0xC0) != 0x80) return 0;
        v = (v << 6) | (uint32_t)(u[i] & 0x3F);
    }
    if (need == 1 && v < 0x80) return 0;
    if (need == 2 && v < 0x800) return 0;
    if (need == 3 && v < 0x10000) return 0;
    if (v > 0x10FFFF || (v >= 0xD800 && v <= 0xDFFF)) return 0;
    *cp = v;
    return need + 1;
}

static int32_t u8_put(char *out, uint32_t cp) {
    if (cp < 0x80) { out[0] = (char)cp; return 1; }
    if (cp < 0x800) {
        out[0] = (char)(0xC0 | (cp >> 6));
        out[1] = (char)(0x80 | (cp & 0x3F));
        return 2;
    }
    if (cp < 0x10000) {
        out[0] = (char)(0xE0 | (cp >> 12));
        out[1] = (char)(0x80 | ((cp >> 6) & 0x3F));
        out[2] = (char)(0x80 | (cp & 0x3F));
        return 3;
    }
    out[0] = (char)(0xF0 | (cp >> 18));
    out[1] = (char)(0x80 | ((cp >> 12) & 0x3F));
    out[2] = (char)(0x80 | ((cp >> 6) & 0x3F));
    out[3] = (char)(0x80 | (cp & 0x3F));
    return 4;
}

/* --- buffers ------------------------------------------------------------ */

/* A growable output buffer, because the converted form can be three times the
 * input (a GBK byte pair is two bytes and up to three in UTF-8) and there is no
 * way to know before converting. */
typedef struct { char *p; size_t len, cap; } EncBuf;

static void encbuf_free(EncBuf *b) { free(b->p); b->p = NULL; b->len = b->cap = 0; }

static int32_t encbuf_reserve(EncBuf *b, size_t extra) {
    if (b->len + extra <= b->cap) return 1;
    size_t cap = b->cap ? b->cap : 64;
    while (cap < b->len + extra) cap *= 2;
    char *np = (char *)realloc(b->p, cap);
    if (!np) return 0;
    b->p = np;
    b->cap = cap;
    return 1;
}

static int32_t encbuf_put(EncBuf *b, const char *p, size_t n) {
    if (!encbuf_reserve(b, n)) return 0;
    memcpy(b->p + b->len, p, n);
    b->len += n;
    return 1;
}

/* --- the conversions ---------------------------------------------------- */

#define REPLACEMENT "\xEF\xBF\xBD"      /* U+FFFD, what .NET's reader emits */

static int32_t enc_latin1_to_utf8(EncBuf *b, const char *in, int32_t len) {
    for (int32_t i = 0; i < len; i++) {
        char out[4];
        int32_t n = u8_put(out, (unsigned char)in[i]);
        if (!encbuf_put(b, out, (size_t)n)) return 0;
    }
    return 1;
}

static int32_t enc_utf16_to_utf8(EncBuf *b, const char *in, int32_t len, int32_t be,
                                 int32_t lossy, int32_t *bad_at) {
    for (int32_t i = 0; i + 1 < len; i += 2) {
        uint32_t unit = be ? (((unsigned char)in[i] << 8) | (unsigned char)in[i+1])
                           : (((unsigned char)in[i+1] << 8) | (unsigned char)in[i]);
        char out[4];
        int32_t n;
        if (unit >= 0xD800 && unit <= 0xDBFF) {          /* high surrogate  */
            if (i + 3 < len) {
                uint32_t lo = be ? (((unsigned char)in[i+2] << 8) | (unsigned char)in[i+3])
                                 : (((unsigned char)in[i+3] << 8) | (unsigned char)in[i+2]);
                if (lo >= 0xDC00 && lo <= 0xDFFF) {
                    uint32_t cp = 0x10000 + ((unit - 0xD800) << 10) + (lo - 0xDC00);
                    n = u8_put(out, cp);
                    if (!encbuf_put(b, out, (size_t)n)) return 0;
                    i += 2;
                    continue;
                }
            }
            if (!lossy) { *bad_at = i; return 0; }
            if (!encbuf_put(b, REPLACEMENT, 3)) return 0;
            continue;
        }
        if (unit >= 0xDC00 && unit <= 0xDFFF) {          /* low, unpaired   */
            if (!lossy) { *bad_at = i; return 0; }
            if (!encbuf_put(b, REPLACEMENT, 3)) return 0;
            continue;
        }
        n = u8_put(out, unit);
        if (!encbuf_put(b, out, (size_t)n)) return 0;
    }
    if (len & 1) {                                        /* an odd byte     */
        if (!lossy) { *bad_at = len - 1; return 0; }
        if (!encbuf_put(b, REPLACEMENT, 3)) return 0;
    }
    return 1;
}

static int32_t enc_utf8_to_utf16(EncBuf *b, const char *in, int32_t len, int32_t be,
                                 int32_t *bad_at) {
    for (int32_t i = 0; i < len; ) {
        uint32_t cp;
        int32_t n = u8_next(in + i, len - i, &cp);
        if (n == 0) { *bad_at = i; return 0; }
        i += n;
        unsigned char out[4];
        if (cp >= 0x10000) {
            uint32_t v = cp - 0x10000;
            uint32_t hi = 0xD800 + (v >> 10), lo = 0xDC00 + (v & 0x3FF);
            out[0] = be ? (unsigned char)(hi >> 8) : (unsigned char)(hi & 0xFF);
            out[1] = be ? (unsigned char)(hi & 0xFF) : (unsigned char)(hi >> 8);
            out[2] = be ? (unsigned char)(lo >> 8) : (unsigned char)(lo & 0xFF);
            out[3] = be ? (unsigned char)(lo & 0xFF) : (unsigned char)(lo >> 8);
            if (!encbuf_put(b, (const char *)out, 4)) return 0;
        } else {
            out[0] = be ? (unsigned char)(cp >> 8) : (unsigned char)(cp & 0xFF);
            out[1] = be ? (unsigned char)(cp & 0xFF) : (unsigned char)(cp >> 8);
            if (!encbuf_put(b, (const char *)out, 2)) return 0;
        }
    }
    return 1;
}

static int32_t enc_utf8_to_latin1(EncBuf *b, const char *in, int32_t len, int32_t *bad_at) {
    for (int32_t i = 0; i < len; ) {
        uint32_t cp;
        int32_t n = u8_next(in + i, len - i, &cp);
        if (n == 0) { *bad_at = i; return 0; }
        /* Latin-1 holds 256 characters, and a name outside them is not
         * approximable: refusing says so, where writing '?' would corrupt a
         * name into something that looks deliberate. */
        if (cp > 0xFF) { *bad_at = i; return 0; }
        char c = (char)cp;
        if (!encbuf_put(b, &c, 1)) return 0;
        i += n;
    }
    return 1;
}

static int32_t enc_utf8_validate(const char *in, int32_t len, int32_t *bad_at) {
    for (int32_t i = 0; i < len; ) {
        uint32_t cp;
        int32_t n = u8_next(in + i, len - i, &cp);
        if (n == 0) { *bad_at = i; return 0; }
        i += n;
    }
    return 1;
}

#ifndef _WIN32
/* iconv, with the offset of the byte that stopped it.  A partial sequence at
 * the end of the input is reported the same way: for a data file both mean
 * "this is not the encoding you said it was". */
static int32_t enc_iconv(EncBuf *b, const Encoding *e, const char *in, int32_t len,
                         int32_t to_utf8, int32_t lossy, int32_t *bad_at) {
    iconv_t cd = iconv_open(to_utf8 ? "UTF-8" : e->iconv_name,
                            to_utf8 ? e->iconv_name : "UTF-8");
    if (cd == (iconv_t)-1) { *bad_at = 0; return 0; }

    char *ip = (char *)in;
    size_t left = (size_t)len;
    int32_t ok = 1;

    while (left > 0) {
        if (!encbuf_reserve(b, left * 4 + 8)) { ok = 0; break; }
        char *op = b->p + b->len;
        size_t room = b->cap - b->len;
        size_t r = iconv(cd, &ip, &left, &op, &room);
        b->len = b->cap - room;
        if (r != (size_t)-1) break;

        if (errno == E2BIG) continue;               /* grew; go round again  */
        if (!lossy) { *bad_at = (int32_t)(ip - in); ok = 0; break; }
        if (!encbuf_put(b, REPLACEMENT, 3)) { ok = 0; break; }
        if (left == 0) break;
        ip++;                                       /* skip the bad byte     */
        left--;
    }

    iconv_close(cd);
    return ok;
}
#else
/* The Win32 codepage API, which is the only converter Windows ships. */
static int32_t enc_win32(EncBuf *b, const Encoding *e, const char *in, int32_t len,
                         int32_t to_utf8, int32_t lossy, int32_t *bad_at) {
    DWORD flags = lossy ? 0 : MB_ERR_INVALID_CHARS;
    int wlen = MultiByteToWideChar(to_utf8 ? (UINT)e->cp : CP_UTF8, flags,
                                   in, len, NULL, 0);
    if (wlen <= 0 && !lossy) { *bad_at = 0; return 0; }

    wchar_t *wide = (wchar_t *)malloc(((size_t)wlen + 1) * sizeof(wchar_t));
    if (!wide) return 0;
    wlen = MultiByteToWideChar(to_utf8 ? (UINT)e->cp : CP_UTF8, flags, in, len, wide, wlen);
    if (wlen <= 0 && !lossy) { free(wide); *bad_at = 0; return 0; }

    int n = WideCharToMultiByte(to_utf8 ? CP_UTF8 : (UINT)e->cp, 0, wide, wlen,
                                NULL, 0, NULL, NULL);
    if (n <= 0) { free(wide); *bad_at = 0; return 0; }
    if (!encbuf_reserve(b, (size_t)n)) { free(wide); return 0; }
    WideCharToMultiByte(to_utf8 ? CP_UTF8 : (UINT)e->cp, 0, wide, wlen,
                        b->p + b->len, n, NULL, NULL);
    b->len += (size_t)n;
    free(wide);
    return 1;
}
#endif

/* --- the commands ------------------------------------------------------- */

static void enc_decode(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv, int32_t lossy) {
    (void)argc;
    const Kiln_Bin *b = (const Kiln_Bin *)kn_arg_ptr(argv, 0);
    const char *name = kn_arg_text(argv, 1);
    const Encoding *e = enc_find(name);

    if (!b) { kn_error_set(KN_ERR_INVALID_ARG, "encoding: expected a byte-set first"); kn_ret_text(ret, NULL); return; }
    if (!e) {
        char msg[128];
        snprintf(msg, sizeof msg, "encoding: '%s' is not an encoding this build knows",
                 name ? name : "");
        kn_error_set(KN_ERR_UNSUPPORTED, msg);
        kn_ret_text(ret, NULL);
        return;
    }

    const char *in = (const char *)(b + 1);
    int32_t len = b->len;
    int32_t bad_at = -1;
    EncBuf out = { NULL, 0, 0 };
    int32_t ok = 1;

    switch (e->mode) {
        case ENC_LATIN1:  ok = enc_latin1_to_utf8(&out, in, len); break;
        case ENC_UTF16LE: ok = enc_utf16_to_utf8(&out, in, len, 0, lossy, &bad_at); break;
        case ENC_UTF16BE: ok = enc_utf16_to_utf8(&out, in, len, 1, lossy, &bad_at); break;
        case ENC_UTF8:    ok = enc_utf8_validate(in, len, &bad_at); if (ok) ok = encbuf_put(&out, in, (size_t)len); break;
        default:
#ifndef _WIN32
            ok = enc_iconv(&out, e, in, len, 1, lossy, &bad_at);
#else
            ok = enc_win32(&out, e, in, len, 1, lossy, &bad_at);
#endif
            break;
    }

    if (!ok) {
        encbuf_free(&out);
        if (bad_at >= 0) {
            char msg[128];
            snprintf(msg, sizeof msg,
                     "encoding_decode: not %s: byte %d does not begin a character",
                     e->canonical, bad_at + 1);
            kn_error_set(KN_ERR_INVALID_ARG, msg);
        } else {
            kn_error_set(KN_ERR_INVALID_ARG, "encoding: out of memory");
        }
        kn_ret_text(ret, NULL);
        return;
    }

    char *text = (char *)kn_malloc((long)out.len + 1);
    if (!text) { encbuf_free(&out); kn_error_set(KN_ERR_INVALID_ARG, "encoding: out of memory"); kn_ret_text(ret, NULL); return; }
    if (out.len) memcpy(text, out.p, out.len);
    text[out.len] = '\0';
    encbuf_free(&out);
    kn_error_clear();
    kn_ret_text(ret, text);
}

void encoding_decode(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    enc_decode(ret, argc, argv, 0);
}

void encoding_decode_lossy(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    enc_decode(ret, argc, argv, 1);
}

void encoding_encode(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *text = kn_arg_text(argv, 0);
    const char *name = kn_arg_text(argv, 1);
    const Encoding *e = enc_find(name);

    if (!e) {
        char msg[128];
        snprintf(msg, sizeof msg, "encoding: '%s' is not an encoding this build knows",
                 name ? name : "");
        kn_error_set(KN_ERR_UNSUPPORTED, msg);
        ret->tag = KN_SDT_BIN;
        ret->v.ptr = kn_bin_new(0);
        return;
    }

    const char *in = text ? text : "";
    int32_t len = (int32_t)strlen(in);
    int32_t bad_at = -1;

    /* Validated before it is converted, and for a reason that is not tidiness:
     * a malformed byte here is a caller that handed over bytes which are not
     * text at all — GBK held in a `text`, most likely — and replacing it with
     * U+FFFD would send a wrong name quietly.  Refusing says
     * which byte, and the caller finds out at the call rather than in use. */
    if (!enc_utf8_validate(in, len, &bad_at)) {
        char msg[128];
        snprintf(msg, sizeof msg, "encoding_encode: the text is not UTF-8: byte %d",
                 bad_at + 1);
        kn_error_set(KN_ERR_INVALID_ARG, msg);
        ret->tag = KN_SDT_BIN;
        ret->v.ptr = kn_bin_new(0);
        return;
    }

    EncBuf out = { NULL, 0, 0 };
    int32_t ok = 1;

    switch (e->mode) {
        case ENC_LATIN1:  ok = enc_utf8_to_latin1(&out, in, len, &bad_at); break;
        case ENC_UTF16LE: ok = enc_utf8_to_utf16(&out, in, len, 0, &bad_at); break;
        case ENC_UTF16BE: ok = enc_utf8_to_utf16(&out, in, len, 1, &bad_at); break;
        case ENC_UTF8:    ok = enc_utf8_validate(in, len, &bad_at); if (ok) ok = encbuf_put(&out, in, (size_t)len); break;
        default:
#ifndef _WIN32
            ok = enc_iconv(&out, e, in, len, 0, 1, &bad_at);
#else
            ok = enc_win32(&out, e, in, len, 0, 1, &bad_at);
#endif
            break;
    }

    if (!ok) {
        encbuf_free(&out);
        char msg[160];
        if (bad_at >= 0) {
            snprintf(msg, sizeof msg,
                     "encoding_encode: cannot be written as %s: byte %d is not a character of it",
                     e->canonical, bad_at + 1);
        } else {
            snprintf(msg, sizeof msg, "encoding: out of memory");
        }
        kn_error_set(KN_ERR_INVALID_ARG, msg);
        ret->tag = KN_SDT_BIN;
        ret->v.ptr = kn_bin_new(0);
        return;
    }

    Kiln_Bin *bin = (Kiln_Bin *)kn_bin_new((int32_t)out.len);
    if (!bin) {
        encbuf_free(&out);
        kn_error_set(KN_ERR_INVALID_ARG, "encoding: out of memory");
        ret->tag = KN_SDT_BIN;
        ret->v.ptr = kn_bin_new(0);
        return;
    }
    if (out.len) memcpy((char *)(bin + 1), out.p, out.len);
    encbuf_free(&out);
    kn_error_clear();
    ret->tag = KN_SDT_BIN;
    ret->v.ptr = bin;
}

void encoding_known(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    kn_error_clear();
    kn_ret_bool(ret, enc_find(kn_arg_text(argv, 0)) != NULL);
}
