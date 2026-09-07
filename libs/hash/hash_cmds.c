/* The "hash" support library — digests and encodings.
 *
 * DIGESTS AND ENCODINGS ONLY. There is no cipher here, no password hashing and
 * no key derivation. SHA-256, SHA-1, MD5, CRC32 and Base64/hex are *not*
 * encryption: base64 hides nothing, and a bare digest of a password is broken
 * by a wordlist in seconds. If you want to store a password, you want a
 * purpose-built password hash (argon2, scrypt, bcrypt), which this library
 * deliberately does not pretend to offer.
 *
 * MD5 and SHA-1 are here because file formats and legacy protocols still speak
 * them. They are broken against a deliberate collision; do not choose them for
 * anything new. hash_sha256 and hash_hmac_sha256 are the ones to reach for.
 *
 * SHA-256, SHA-1 and MD5 are implemented here from their specifications, with
 * no dependency beyond the C library. The algorithms are checked against the
 * published vectors in examples/hashlib.kiln, which is a program you can run.
 *
 * Text is bytes: every command hashes or encodes the UTF-8 bytes of its
 * argument, so a digest matches what any other tool computes over the same
 * file content.
 *
 * ONE LIMIT, STATED PLAINLY: a decoded result is returned as `text`, and text
 * ends at the first NUL. base64_decode and hex_decode of data containing a
 * zero byte therefore hand back a string that stops there. Both are honest
 * about the bytes they produce for text; neither is a byte buffer, because the
 * language has none.
 */
#include <string.h>
#include <stdint.h>
#include "kiln_abi.h"

static const char *hash_nz(const char *s) { return s ? s : ""; }

/* Every result is runtime-owned memory, never a literal: the runtime frees
 * program data at exit. */
static char *hash_alloc(long len) { return (char *)kn_malloc(len + 1); }
static char *hash_empty(void) { char *o = hash_alloc(0); o[0] = '\0'; return o; }

/* ---------------------------------------------------------------- SHA-256 */

typedef struct { uint32_t h[8]; uint64_t len; unsigned char buf[64]; size_t n; } hash_sha256_ctx;

static const uint32_t HASH_K256[64] = {
    0x428a2f98u,0x71374491u,0xb5c0fbcfu,0xe9b5dba5u,0x3956c25bu,0x59f111f1u,0x923f82a4u,0xab1c5ed5u,
    0xd807aa98u,0x12835b01u,0x243185beu,0x550c7dc3u,0x72be5d74u,0x80deb1feu,0x9bdc06a7u,0xc19bf174u,
    0xe49b69c1u,0xefbe4786u,0x0fc19dc6u,0x240ca1ccu,0x2de92c6fu,0x4a7484aau,0x5cb0a9dcu,0x76f988dau,
    0x983e5152u,0xa831c66du,0xb00327c8u,0xbf597fc7u,0xc6e00bf3u,0xd5a79147u,0x06ca6351u,0x14292967u,
    0x27b70a85u,0x2e1b2138u,0x4d2c6dfcu,0x53380d13u,0x650a7354u,0x766a0abbu,0x81c2c92eu,0x92722c85u,
    0xa2bfe8a1u,0xa81a664bu,0xc24b8b70u,0xc76c51a3u,0xd192e819u,0xd6990624u,0xf40e3585u,0x106aa070u,
    0x19a4c116u,0x1e376c08u,0x2748774cu,0x34b0bcb5u,0x391c0cb3u,0x4ed8aa4au,0x5b9cca4fu,0x682e6ff3u,
    0x748f82eeu,0x78a5636fu,0x84c87814u,0x8cc70208u,0x90befffau,0xa4506cebu,0xbef9a3f7u,0xc67178f2u
};

static uint32_t hash_rotr32(uint32_t x, int n) { return (x >> n) | (x << (32 - n)); }

static void hash_sha256_block(hash_sha256_ctx *c, const unsigned char *p) {
    uint32_t w[64], a, b, d, e, f, g, hh, cc, t1, t2;
    int i;
    for (i = 0; i < 16; i++)
        w[i] = ((uint32_t)p[i*4] << 24) | ((uint32_t)p[i*4+1] << 16) |
               ((uint32_t)p[i*4+2] << 8) | (uint32_t)p[i*4+3];
    for (i = 16; i < 64; i++) {
        uint32_t s0 = hash_rotr32(w[i-15],7) ^ hash_rotr32(w[i-15],18) ^ (w[i-15] >> 3);
        uint32_t s1 = hash_rotr32(w[i-2],17) ^ hash_rotr32(w[i-2],19) ^ (w[i-2] >> 10);
        w[i] = w[i-16] + s0 + w[i-7] + s1;
    }
    a=c->h[0]; b=c->h[1]; cc=c->h[2]; d=c->h[3]; e=c->h[4]; f=c->h[5]; g=c->h[6]; hh=c->h[7];
    for (i = 0; i < 64; i++) {
        uint32_t S1 = hash_rotr32(e,6) ^ hash_rotr32(e,11) ^ hash_rotr32(e,25);
        uint32_t ch = (e & f) ^ ((~e) & g);
        t1 = hh + S1 + ch + HASH_K256[i] + w[i];
        uint32_t S0 = hash_rotr32(a,2) ^ hash_rotr32(a,13) ^ hash_rotr32(a,22);
        uint32_t mj = (a & b) ^ (a & cc) ^ (b & cc);
        t2 = S0 + mj;
        hh=g; g=f; f=e; e=d+t1; d=cc; cc=b; b=a; a=t1+t2;
    }
    c->h[0]+=a; c->h[1]+=b; c->h[2]+=cc; c->h[3]+=d;
    c->h[4]+=e; c->h[5]+=f; c->h[6]+=g; c->h[7]+=hh;
}

static void hash_sha256_init(hash_sha256_ctx *c) {
    c->h[0]=0x6a09e667u; c->h[1]=0xbb67ae85u; c->h[2]=0x3c6ef372u; c->h[3]=0xa54ff53au;
    c->h[4]=0x510e527fu; c->h[5]=0x9b05688cu; c->h[6]=0x1f83d9abu; c->h[7]=0x5be0cd19u;
    c->len = 0; c->n = 0;
}

static void hash_sha256_update(hash_sha256_ctx *c, const void *data, size_t len) {
    const unsigned char *p = (const unsigned char *)data;
    c->len += (uint64_t)len;
    while (len) {
        size_t take = 64 - c->n;
        if (take > len) take = len;
        memcpy(c->buf + c->n, p, take);
        c->n += take; p += take; len -= take;
        if (c->n == 64) { hash_sha256_block(c, c->buf); c->n = 0; }
    }
}

static void hash_sha256_final(hash_sha256_ctx *c, unsigned char out[32]) {
    uint64_t bits = c->len * 8;
    unsigned char pad = 0x80;
    int i;
    hash_sha256_update(c, &pad, 1);
    pad = 0;
    while (c->n != 56) hash_sha256_update(c, &pad, 1);
    for (i = 7; i >= 0; i--) { unsigned char b = (unsigned char)(bits >> (i*8)); hash_sha256_update(c, &b, 1); }
    for (i = 0; i < 8; i++) {
        out[i*4]   = (unsigned char)(c->h[i] >> 24);
        out[i*4+1] = (unsigned char)(c->h[i] >> 16);
        out[i*4+2] = (unsigned char)(c->h[i] >> 8);
        out[i*4+3] = (unsigned char)(c->h[i]);
    }
}

/* ------------------------------------------------------------------ SHA-1 */

typedef struct { uint32_t h[5]; uint64_t len; unsigned char buf[64]; size_t n; } hash_sha1_ctx;

static uint32_t hash_rotl32(uint32_t x, int n) { return (x << n) | (x >> (32 - n)); }

static void hash_sha1_block(hash_sha1_ctx *c, const unsigned char *p) {
    uint32_t w[80], a, b, d, e, cc, t;
    int i;
    for (i = 0; i < 16; i++)
        w[i] = ((uint32_t)p[i*4] << 24) | ((uint32_t)p[i*4+1] << 16) |
               ((uint32_t)p[i*4+2] << 8) | (uint32_t)p[i*4+3];
    for (i = 16; i < 80; i++) w[i] = hash_rotl32(w[i-3] ^ w[i-8] ^ w[i-14] ^ w[i-16], 1);
    a=c->h[0]; b=c->h[1]; cc=c->h[2]; d=c->h[3]; e=c->h[4];
    for (i = 0; i < 80; i++) {
        uint32_t f, k;
        if (i < 20)      { f = (b & cc) | ((~b) & d);        k = 0x5a827999u; }
        else if (i < 40) { f = b ^ cc ^ d;                   k = 0x6ed9eba1u; }
        else if (i < 60) { f = (b & cc) | (b & d) | (cc & d); k = 0x8f1bbcdcu; }
        else             { f = b ^ cc ^ d;                   k = 0xca62c1d6u; }
        t = hash_rotl32(a,5) + f + e + k + w[i];
        e=d; d=cc; cc=hash_rotl32(b,30); b=a; a=t;
    }
    c->h[0]+=a; c->h[1]+=b; c->h[2]+=cc; c->h[3]+=d; c->h[4]+=e;
}

static void hash_sha1_init(hash_sha1_ctx *c) {
    c->h[0]=0x67452301u; c->h[1]=0xefcdab89u; c->h[2]=0x98badcfeu;
    c->h[3]=0x10325476u; c->h[4]=0xc3d2e1f0u;
    c->len = 0; c->n = 0;
}

static void hash_sha1_update(hash_sha1_ctx *c, const void *data, size_t len) {
    const unsigned char *p = (const unsigned char *)data;
    c->len += (uint64_t)len;
    while (len) {
        size_t take = 64 - c->n;
        if (take > len) take = len;
        memcpy(c->buf + c->n, p, take);
        c->n += take; p += take; len -= take;
        if (c->n == 64) { hash_sha1_block(c, c->buf); c->n = 0; }
    }
}

static void hash_sha1_final(hash_sha1_ctx *c, unsigned char out[20]) {
    uint64_t bits = c->len * 8;
    unsigned char pad = 0x80;
    int i;
    hash_sha1_update(c, &pad, 1);
    pad = 0;
    while (c->n != 56) hash_sha1_update(c, &pad, 1);
    for (i = 7; i >= 0; i--) { unsigned char b = (unsigned char)(bits >> (i*8)); hash_sha1_update(c, &b, 1); }
    for (i = 0; i < 5; i++) {
        out[i*4]   = (unsigned char)(c->h[i] >> 24);
        out[i*4+1] = (unsigned char)(c->h[i] >> 16);
        out[i*4+2] = (unsigned char)(c->h[i] >> 8);
        out[i*4+3] = (unsigned char)(c->h[i]);
    }
}

/* -------------------------------------------------------------------- MD5
 * MD5 is little-endian where SHA is big-endian: both the message words and the
 * final digest are read and written low byte first. Getting that backwards
 * yields hex that looks entirely plausible and matches nothing. */

typedef struct { uint32_t h[4]; uint64_t len; unsigned char buf[64]; size_t n; } hash_md5_ctx;

static const uint32_t HASH_KMD5[64] = {
    0xd76aa478u,0xe8c7b756u,0x242070dbu,0xc1bdceeeu,0xf57c0fafu,0x4787c62au,0xa8304613u,0xfd469501u,
    0x698098d8u,0x8b44f7afu,0xffff5bb1u,0x895cd7beu,0x6b901122u,0xfd987193u,0xa679438eu,0x49b40821u,
    0xf61e2562u,0xc040b340u,0x265e5a51u,0xe9b6c7aau,0xd62f105du,0x02441453u,0xd8a1e681u,0xe7d3fbc8u,
    0x21e1cde6u,0xc33707d6u,0xf4d50d87u,0x455a14edu,0xa9e3e905u,0xfcefa3f8u,0x676f02d9u,0x8d2a4c8au,
    0xfffa3942u,0x8771f681u,0x6d9d6122u,0xfde5380cu,0xa4beea44u,0x4bdecfa9u,0xf6bb4b60u,0xbebfbc70u,
    0x289b7ec6u,0xeaa127fau,0xd4ef3085u,0x04881d05u,0xd9d4d039u,0xe6db99e5u,0x1fa27cf8u,0xc4ac5665u,
    0xf4292244u,0x432aff97u,0xab9423a7u,0xfc93a039u,0x655b59c3u,0x8f0ccc92u,0xffeff47du,0x85845dd1u,
    0x6fa87e4fu,0xfe2ce6e0u,0xa3014314u,0x4e0811a1u,0xf7537e82u,0xbd3af235u,0x2ad7d2bbu,0xeb86d391u
};
static const int HASH_SMD5[64] = {
    7,12,17,22, 7,12,17,22, 7,12,17,22, 7,12,17,22,
    5, 9,14,20, 5, 9,14,20, 5, 9,14,20, 5, 9,14,20,
    4,11,16,23, 4,11,16,23, 4,11,16,23, 4,11,16,23,
    6,10,15,21, 6,10,15,21, 6,10,15,21, 6,10,15,21
};

static void hash_md5_block(hash_md5_ctx *c, const unsigned char *p) {
    uint32_t m[16], a, b, cc, d;
    int i;
    for (i = 0; i < 16; i++)
        m[i] = (uint32_t)p[i*4] | ((uint32_t)p[i*4+1] << 8) |
               ((uint32_t)p[i*4+2] << 16) | ((uint32_t)p[i*4+3] << 24);
    a=c->h[0]; b=c->h[1]; cc=c->h[2]; d=c->h[3];
    for (i = 0; i < 64; i++) {
        uint32_t f; int g;
        if (i < 16)      { f = (b & cc) | ((~b) & d);  g = i; }
        else if (i < 32) { f = (d & b) | ((~d) & cc);  g = (5*i + 1) & 15; }
        else if (i < 48) { f = b ^ cc ^ d;             g = (3*i + 5) & 15; }
        else             { f = cc ^ (b | (~d));        g = (7*i) & 15; }
        f = f + a + HASH_KMD5[i] + m[g];
        a = d; d = cc; cc = b;
        b = b + hash_rotl32(f, HASH_SMD5[i]);
    }
    c->h[0]+=a; c->h[1]+=b; c->h[2]+=cc; c->h[3]+=d;
}

static void hash_md5_init(hash_md5_ctx *c) {
    c->h[0]=0x67452301u; c->h[1]=0xefcdab89u; c->h[2]=0x98badcfeu; c->h[3]=0x10325476u;
    c->len = 0; c->n = 0;
}

static void hash_md5_update(hash_md5_ctx *c, const void *data, size_t len) {
    const unsigned char *p = (const unsigned char *)data;
    c->len += (uint64_t)len;
    while (len) {
        size_t take = 64 - c->n;
        if (take > len) take = len;
        memcpy(c->buf + c->n, p, take);
        c->n += take; p += take; len -= take;
        if (c->n == 64) { hash_md5_block(c, c->buf); c->n = 0; }
    }
}

static void hash_md5_final(hash_md5_ctx *c, unsigned char out[16]) {
    uint64_t bits = c->len * 8;
    unsigned char pad = 0x80;
    int i;
    hash_md5_update(c, &pad, 1);
    pad = 0;
    while (c->n != 56) hash_md5_update(c, &pad, 1);
    for (i = 0; i < 8; i++) { unsigned char b = (unsigned char)(bits >> (i*8)); hash_md5_update(c, &b, 1); }
    for (i = 0; i < 4; i++) {
        out[i*4]   = (unsigned char)(c->h[i]);
        out[i*4+1] = (unsigned char)(c->h[i] >> 8);
        out[i*4+2] = (unsigned char)(c->h[i] >> 16);
        out[i*4+3] = (unsigned char)(c->h[i] >> 24);
    }
}

/* --------------------------------------------------------------- utilities */

static const char HASH_HEXDIGITS[] = "0123456789abcdef";

/* Digests are returned as lowercase hex, the form every command-line tool
 * prints, so a program's output can be compared with sha256sum by eye. */
static char *hash_to_hex(const unsigned char *b, long n) {
    char *o = hash_alloc(n * 2);
    long i;
    for (i = 0; i < n; i++) {
        o[i*2]   = HASH_HEXDIGITS[b[i] >> 4];
        o[i*2+1] = HASH_HEXDIGITS[b[i] & 15];
    }
    o[n*2] = '\0';
    return o;
}

/* ------------------------------------------------------------- the commands */

/* hash_sha256(text) -> text : lowercase hex, 64 characters. Infallible. */
void hash_sha256(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *s = hash_nz(kn_arg_text(argv, 0));
    unsigned char d[32];
    hash_sha256_ctx c;
    hash_sha256_init(&c);
    hash_sha256_update(&c, s, strlen(s));
    hash_sha256_final(&c, d);
    kn_ret_text(ret, hash_to_hex(d, 32));
}

/* hash_sha1(text) -> text : lowercase hex, 40 characters. Infallible.
 * Broken against deliberate collisions; here for legacy formats only. */
void hash_sha1(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *s = hash_nz(kn_arg_text(argv, 0));
    unsigned char d[20];
    hash_sha1_ctx c;
    hash_sha1_init(&c);
    hash_sha1_update(&c, s, strlen(s));
    hash_sha1_final(&c, d);
    kn_ret_text(ret, hash_to_hex(d, 20));
}

/* hash_md5(text) -> text : lowercase hex, 32 characters. Infallible.
 * Thoroughly broken; here for legacy formats only. */
void hash_md5(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *s = hash_nz(kn_arg_text(argv, 0));
    unsigned char d[16];
    hash_md5_ctx c;
    hash_md5_init(&c);
    hash_md5_update(&c, s, strlen(s));
    hash_md5_final(&c, d);
    kn_ret_text(ret, hash_to_hex(d, 16));
}

/* hash_crc32(text) -> int64 : the CRC-32 of IEEE 802.3 / zip / PNG (reflected,
 * polynomial 0xEDB88320, initial and final inversion). Returned as int64 so the
 * top bit is a value and not a sign: an int would report half of all checksums
 * as negative. Infallible — a checksum has no failure. */
void hash_crc32(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *s = hash_nz(kn_arg_text(argv, 0));
    uint32_t crc = 0xFFFFFFFFu;
    size_t i, n = strlen(s);
    int k;
    for (i = 0; i < n; i++) {
        crc ^= (unsigned char)s[i];
        for (k = 0; k < 8; k++)
            crc = (crc >> 1) ^ (0xEDB88320u & (uint32_t)(-(int32_t)(crc & 1)));
    }
    crc ^= 0xFFFFFFFFu;
    kn_ret_int64(ret, (int64_t)(uint64_t)crc);
}

/* hash_hmac_sha256(key, message) -> text : HMAC-SHA-256 as lowercase hex
 * (RFC 2104). A key longer than the 64-byte block is hashed first, as the
 * specification requires. Infallible.
 *
 * This is the command to use to authenticate a message. hash_sha256 of a key
 * concatenated with a message is NOT the same thing and is forgeable by length
 * extension. */
void hash_hmac_sha256(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *key = hash_nz(kn_arg_text(argv, 0));
    const char *msg = hash_nz(kn_arg_text(argv, 1));
    unsigned char k[64], ipad[64], opad[64], inner[32], outer[32];
    size_t klen = strlen(key);
    int i;
    hash_sha256_ctx c;

    memset(k, 0, sizeof k);
    if (klen > 64) {
        hash_sha256_init(&c);
        hash_sha256_update(&c, key, klen);
        hash_sha256_final(&c, k);          /* the key becomes its own digest */
    } else {
        memcpy(k, key, klen);
    }
    for (i = 0; i < 64; i++) { ipad[i] = (unsigned char)(k[i] ^ 0x36); opad[i] = (unsigned char)(k[i] ^ 0x5c); }

    hash_sha256_init(&c);
    hash_sha256_update(&c, ipad, 64);
    hash_sha256_update(&c, msg, strlen(msg));
    hash_sha256_final(&c, inner);

    hash_sha256_init(&c);
    hash_sha256_update(&c, opad, 64);
    hash_sha256_update(&c, inner, 32);
    hash_sha256_final(&c, outer);

    kn_ret_text(ret, hash_to_hex(outer, 32));
}

/* ------------------------------------------------------------------ base64 */

static const char HASH_B64[] =
    "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/* base64_encode(text) -> text : standard alphabet (RFC 4648 §4), always padded
 * with '=' to a multiple of four, never line-wrapped. Infallible.
 *
 * Encoding is not encryption. Anyone can read this back. */
void base64_encode(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const unsigned char *s = (const unsigned char *)hash_nz(kn_arg_text(argv, 0));
    long n = (long)strlen((const char *)s);
    long groups = (n + 2) / 3;
    char *o = hash_alloc(groups * 4);
    long i = 0, w = 0;
    while (i < n) {
        uint32_t v = (uint32_t)s[i] << 16;
        long have = 1;
        if (i + 1 < n) { v |= (uint32_t)s[i+1] << 8; have = 2; }
        if (i + 2 < n) { v |= (uint32_t)s[i+2];      have = 3; }
        o[w++] = HASH_B64[(v >> 18) & 63];
        o[w++] = HASH_B64[(v >> 12) & 63];
        o[w++] = (have > 1) ? HASH_B64[(v >> 6) & 63] : '=';
        o[w++] = (have > 2) ? HASH_B64[v & 63]        : '=';
        i += 3;
    }
    o[w] = '\0';
    kn_ret_text(ret, o);
}

static int hash_b64_value(unsigned char ch) {
    if (ch >= 'A' && ch <= 'Z') return ch - 'A';
    if (ch >= 'a' && ch <= 'z') return ch - 'a' + 26;
    if (ch >= '0' && ch <= '9') return ch - '0' + 52;
    if (ch == '+') return 62;
    if (ch == '/') return 63;
    return -1;
}

/* base64_decode(text) -> text : the inverse. FALLIBLE — "" plus the error slot
 * when the input is not valid base64.
 *
 * What is accepted, stated rather than left to be discovered: the standard
 * alphabet only (no URL-safe '-' or '_'); ASCII whitespace anywhere is ignored,
 * so text pasted across several lines decodes; padding is optional, but a
 * length that leaves one leftover character is impossible in base64 and is
 * rejected rather than guessed at.
 *
 * A decoded byte of zero ends the text, so binary data containing a NUL comes
 * back truncated. That is a property of `text`, not of this command. */
void base64_decode(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const unsigned char *s = (const unsigned char *)hash_nz(kn_arg_text(argv, 0));
    long n = (long)strlen((const char *)s);
    char *o = hash_alloc(n / 4 * 3 + 3);
    uint32_t acc = 0;
    int have = 0;
    long i, w = 0;
    int seen_pad = 0;

    for (i = 0; i < n; i++) {
        unsigned char ch = s[i];
        int v;
        if (ch == ' ' || ch == '\n' || ch == '\r' || ch == '\t' || ch == '\f' || ch == '\v') continue;
        if (ch == '=') { seen_pad = 1; continue; }
        v = hash_b64_value(ch);
        if (v < 0 || seen_pad) {          /* a character after padding is junk too */
            kn_mfree(o);
            kn_error_set(KN_ERR_INVALID_ARG, "base64_decode: not valid base64");
            kn_ret_text(ret, hash_empty());
            return;
        }
        acc = (acc << 6) | (uint32_t)v;
        have++;
        if (have == 4) { o[w++] = (char)(acc >> 16); o[w++] = (char)(acc >> 8); o[w++] = (char)acc; acc = 0; have = 0; }
    }
    if (have == 1) {                       /* six leftover bits encode nothing */
        kn_mfree(o);
        kn_error_set(KN_ERR_INVALID_ARG, "base64_decode: truncated base64 input");
        kn_ret_text(ret, hash_empty());
        return;
    }
    if (have == 2) { o[w++] = (char)(acc >> 4); }
    else if (have == 3) { acc >>= 2; o[w++] = (char)(acc >> 8); o[w++] = (char)acc; }
    o[w] = '\0';
    kn_error_clear();
    kn_ret_text(ret, o);
}

/* --------------------------------------------------------------------- hex */

/* hex_encode(text) -> text : lowercase hex of the bytes, two characters per
 * byte, no separators. Infallible. */
void hex_encode(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *s = hash_nz(kn_arg_text(argv, 0));
    kn_ret_text(ret, hash_to_hex((const unsigned char *)s, (long)strlen(s)));
}

static int hash_hex_value(unsigned char ch) {
    if (ch >= '0' && ch <= '9') return ch - '0';
    if (ch >= 'a' && ch <= 'f') return ch - 'a' + 10;
    if (ch >= 'A' && ch <= 'F') return ch - 'A' + 10;
    return -1;
}

/* hex_decode(text) -> text : the inverse. FALLIBLE — "" plus the error slot on
 * a non-hex character or an odd number of digits.
 *
 * Upper and lower case are both accepted, and ASCII whitespace is ignored so
 * that hex copied out of a dump with spaces between the bytes still decodes.
 * As with base64_decode, a decoded zero byte ends the text. */
void hex_decode(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const unsigned char *s = (const unsigned char *)hash_nz(kn_arg_text(argv, 0));
    long n = (long)strlen((const char *)s);
    char *o = hash_alloc(n / 2 + 1);
    long i, w = 0;
    int hi = -1;

    for (i = 0; i < n; i++) {
        unsigned char ch = s[i];
        int v;
        if (ch == ' ' || ch == '\n' || ch == '\r' || ch == '\t' || ch == '\f' || ch == '\v') continue;
        v = hash_hex_value(ch);
        if (v < 0) {
            kn_mfree(o);
            kn_error_set(KN_ERR_INVALID_ARG, "hex_decode: not a hex digit");
            kn_ret_text(ret, hash_empty());
            return;
        }
        if (hi < 0) hi = v;
        else { o[w++] = (char)((hi << 4) | v); hi = -1; }
    }
    if (hi >= 0) {                         /* half a byte is not a byte */
        kn_mfree(o);
        kn_error_set(KN_ERR_INVALID_ARG, "hex_decode: odd number of hex digits");
        kn_ret_text(ret, hash_empty());
        return;
    }
    o[w] = '\0';
    kn_error_clear();
    kn_ret_text(ret, o);
}
