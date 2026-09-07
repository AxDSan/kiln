/* The "random" support library — pseudo-random numbers for games, sampling and
 * test data.
 *
 * NOT FOR SECURITY. This is a fast, fully deterministic generator whose entire
 * state can be recovered from a handful of outputs. Never use it for passwords,
 * tokens, keys, session ids, or anything else an attacker would like to guess.
 *
 * The generator is xoshiro256** seeded through splitmix64, written out here
 * rather than borrowed from rand(): a seed has to produce the SAME sequence on
 * every machine, or `random_seed` would promise a reproducibility it could not
 * deliver. A program that never calls `random_seed` is seeded once, lazily,
 * from the clock and the process id, so two runs differ.
 */
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <time.h>
/* getpid lives in <process.h> on Windows and is spelled _getpid; MSVC has no
 * <unistd.h> at all. */
#ifdef _WIN32
#include <process.h>
#define getpid _getpid
#else
#include <unistd.h>
#endif
#include "kiln_abi.h"

/* --- the generator ---------------------------------------------------- */

static uint64_t random_state[4];
static int      random_ready = 0;

static uint64_t random_splitmix64(uint64_t *x) {
    uint64_t z = (*x += 0x9E3779B97F4A7C15ULL);
    z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9ULL;
    z = (z ^ (z >> 27)) * 0x94D049BB133111EBULL;
    return z ^ (z >> 31);
}

static void random_reseed(uint64_t seed) {
    uint64_t s = seed;
    for (int i = 0; i < 4; i++) random_state[i] = random_splitmix64(&s);
    random_ready = 1;
}

static uint64_t random_rotl(uint64_t x, int k) { return (x << k) | (x >> (64 - k)); }

static uint64_t random_next(void) {
    if (!random_ready) {
        /* First use with no explicit seed: the clock alone repeats when two
         * runs start inside the same second, so mix the pid in too. */
        uint64_t t = (uint64_t)time(NULL);
        uint64_t p = (uint64_t)getpid();
        random_reseed(t * 0x9E3779B97F4A7C15ULL ^ (p << 17) ^ (uint64_t)(uintptr_t)&random_state);
    }
    uint64_t *s = random_state;
    uint64_t result = random_rotl(s[1] * 5, 7) * 9;
    uint64_t t = s[1] << 17;
    s[2] ^= s[0];
    s[3] ^= s[1];
    s[1] ^= s[2];
    s[0] ^= s[3];
    s[2] ^= t;
    s[3] = random_rotl(s[3], 45);
    return result;
}

/* Uniform in [0, bound) for bound > 0, without the modulo bias that would make
 * the low values slightly likelier than the high ones. */
static uint64_t random_below(uint64_t bound) {
    uint64_t limit = UINT64_MAX - (UINT64_MAX % bound) - 1;
    uint64_t x;
    do { x = random_next(); } while (x > limit);
    return x % bound;
}

/* --- commands --------------------------------------------------------- */

/* random_seed(int seed) : start the sequence over at a known point.
 * The same seed gives the same numbers on every platform. Infallible. */
void random_seed(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    ret->tag = KN_SDT_NULL;
    random_reseed((uint64_t)(int64_t)kn_arg_int(argv, 0));
}

/* random_seed_now() -> int : seed from the clock and the process id, and
 * return the seed used, so a run that surprised you can be replayed by feeding
 * that number back to random_seed. Infallible. */
void random_seed_now(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; (void)argv;
    uint64_t t = (uint64_t)time(NULL);
    uint64_t p = (uint64_t)getpid();
    uint64_t mixed = t * 0x9E3779B97F4A7C15ULL ^ (p << 17);
    int32_t seed = (int32_t)(mixed >> 20);
    random_reseed((uint64_t)(int64_t)seed);
    kn_ret_int(ret, seed);
}

/* random_int(int count) -> int : 0 .. count-1, the shape you want for picking
 * one of `count` things. count <= 0 has no answer to give: it reports
 * KN_ERR_INVALID_ARG and returns 0. */
void random_int(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    int32_t count = kn_arg_int(argv, 0);
    if (count <= 0) {
        kn_error_set(KN_ERR_INVALID_ARG, "random_int: count must be 1 or more");
        kn_ret_int(ret, 0);
        return;
    }
    kn_error_clear();
    kn_ret_int(ret, (int32_t)((int64_t)random_below((uint64_t)count)));
}

/* random_between(int lo, int hi) -> int : inclusive of both ends.
 * lo > hi is the one real mistake here — an empty range has no member — so it
 * reports KN_ERR_INVALID_ARG and returns 0. Because 0 is also a perfectly good
 * draw, a program that passes computed bounds should check last_error_code(). */
void random_between(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    int32_t lo = kn_arg_int(argv, 0), hi = kn_arg_int(argv, 1);
    if (lo > hi) {
        kn_error_set(KN_ERR_INVALID_ARG, "random_between: lo is greater than hi");
        kn_ret_int(ret, 0);
        return;
    }
    kn_error_clear();
    /* Widened to 64 bits: hi - lo overflows int32 for a full-range span. */
    uint64_t span = (uint64_t)((int64_t)hi - (int64_t)lo) + 1;
    kn_ret_int(ret, (int32_t)((int64_t)lo + (int64_t)random_below(span)));
}

/* random_double() -> double : 0.0 <= x < 1.0. Infallible. */
void random_double(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; (void)argv;
    /* 53 bits — every value is exactly representable, and none repeats. */
    kn_ret_double(ret, (double)(random_next() >> 11) * (1.0 / 9007199254740992.0));
}

/* random_double_between(double lo, double hi) -> double : lo <= x < hi (x may
 * equal lo when lo == hi). lo > hi reports KN_ERR_INVALID_ARG and returns 0.0. */
void random_double_between(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    double lo = kn_arg_double(argv, 0), hi = kn_arg_double(argv, 1);
    if (lo > hi) {
        kn_error_set(KN_ERR_INVALID_ARG, "random_double_between: lo is greater than hi");
        kn_ret_double(ret, 0.0);
        return;
    }
    kn_error_clear();
    double u = (double)(random_next() >> 11) * (1.0 / 9007199254740992.0);
    kn_ret_double(ret, lo + u * (hi - lo));
}

/* random_bool() -> bool : heads or tails. Infallible, so it never touches the
 * error slot and its `false` is always a genuine tails. */
void random_bool(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; (void)argv;
    kn_ret_bool(ret, (int32_t)(random_next() >> 63));
}

/* random_chance(int percent) -> bool : true roughly `percent` times in a
 * hundred. Out-of-range percentages are the honest answer rather than an error:
 * 0 or less is never, 100 or more is always. Infallible. */
void random_chance(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    int32_t percent = kn_arg_int(argv, 0);
    if (percent <= 0)   { kn_ret_bool(ret, 0); return; }
    if (percent >= 100) { kn_ret_bool(ret, 1); return; }
    kn_ret_bool(ret, (int32_t)(random_below(100) < (uint64_t)percent));
}

/* random_hex(int length) -> text : `length` lowercase hex digits, handy for a
 * test id or a throwaway colour. NOT a token or a password — see the header.
 * A negative length reports KN_ERR_INVALID_ARG and returns ""; length 0 is a
 * legitimate empty answer, not a failure. */
void random_hex(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    int32_t length = kn_arg_int(argv, 0);
    if (length < 0) {
        kn_error_set(KN_ERR_INVALID_ARG, "random_hex: length must be 0 or more");
        char *empty = (char *)kn_malloc(1);
        empty[0] = '\0';
        kn_ret_text(ret, empty);
        return;
    }
    kn_error_clear();
    static const char digits[] = "0123456789abcdef";
    char *out = (char *)kn_malloc((long)length + 1);
    for (int32_t i = 0; i < length; i++) out[i] = digits[random_next() & 0xF];
    out[length] = '\0';
    kn_ret_text(ret, out);
}
