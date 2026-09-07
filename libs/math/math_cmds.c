/* The `math` support library — what core arithmetic leaves out.
 *
 * Core already covers the everyday cases: sqrt, sin/cos/tan, pow, exp, ln,
 * log10, floor/ceil/round, abs, min/max and mod_int.  This library is the rest
 * of what a program doing real geometry, graphics or number work reaches for —
 * inverse and hyperbolic trigonometry, the constants, angle conversion,
 * interpolation, and the integer functions that have no floating-point form.
 *
 * A double command with no answer for its input (asin(2), log_base(x, 0))
 * returns NaN and sets the error slot.  NaN is the honest sentinel for a real
 * result: unlike -1 or 0 it is not a value any correct computation produces, it
 * propagates through arithmetic instead of quietly becoming plausible, and
 * math_is_nan is there to test for it.
 */
#include <errno.h>
#include <math.h>
#include "kiln_abi.h"

/* One place to produce the double failure sentinel, so every command in this
 * file reports a domain error identically. */
static void math_fail(Kiln_Slot *ret, const char *msg) {
    kn_error_set(KN_ERR_INVALID_ARG, msg);
    kn_ret_double(ret, NAN);
}

#define MATH_D1(name, expr)                                                    \
    void math_##name(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {    \
        (void)argc;                                                            \
        double x = kn_arg_double(argv, 0);                                     \
        kn_ret_double(ret, (expr));                                            \
    }

/* --- constants -------------------------------------------------------- */
/* Commands rather than literals because the language has no constant
 * declaration, and a program writing 3.14159 by hand is a program with a bug
 * waiting in it. */
void math_pi(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; (void)argv; kn_ret_double(ret, 3.14159265358979323846);
}
void math_tau(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; (void)argv; kn_ret_double(ret, 6.28318530717958647692);
}
void math_e(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; (void)argv; kn_ret_double(ret, 2.71828182845904523536);
}
void math_infinity(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; (void)argv; kn_ret_double(ret, INFINITY);
}

/* --- inverse trigonometry --------------------------------------------- */
void math_asin(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    double x = kn_arg_double(argv, 0);
    if (x < -1.0 || x > 1.0) { math_fail(ret, "asin: argument outside -1..1"); return; }
    kn_error_clear();
    kn_ret_double(ret, asin(x));
}
void math_acos(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    double x = kn_arg_double(argv, 0);
    if (x < -1.0 || x > 1.0) { math_fail(ret, "acos: argument outside -1..1"); return; }
    kn_error_clear();
    kn_ret_double(ret, acos(x));
}
MATH_D1(atan, atan(x))

/* atan2(y, x) — the one that knows which quadrant it is in, and the reason a
 * program can turn a vector into an angle without special-casing x = 0. */
void math_atan2(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    kn_ret_double(ret, atan2(kn_arg_double(argv, 0), kn_arg_double(argv, 1)));
}

/* --- hyperbolic -------------------------------------------------------- */
MATH_D1(sinh, sinh(x))
MATH_D1(cosh, cosh(x))
MATH_D1(tanh, tanh(x))

/* --- logarithms and roots ---------------------------------------------- */
MATH_D1(log2, log2(x))
MATH_D1(cbrt, cbrt(x))

void math_log_base(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    double x = kn_arg_double(argv, 0), base = kn_arg_double(argv, 1);
    if (base <= 0.0 || base == 1.0) { math_fail(ret, "log_base: base must be positive and not 1"); return; }
    if (x <= 0.0) { math_fail(ret, "log_base: value must be positive"); return; }
    kn_error_clear();
    kn_ret_double(ret, log(x) / log(base));
}

/* hypot, not sqrt(x*x + y*y): the naive form overflows for large inputs and
 * loses precision for small ones, and a distance function should not. */
void math_hypot(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    kn_ret_double(ret, hypot(kn_arg_double(argv, 0), kn_arg_double(argv, 1)));
}

/* --- rounding and remainder -------------------------------------------- */
MATH_D1(trunc, trunc(x))

void math_fmod(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    double b = kn_arg_double(argv, 1);
    if (b == 0.0) { math_fail(ret, "fmod: division by zero"); return; }
    kn_error_clear();
    kn_ret_double(ret, fmod(kn_arg_double(argv, 0), b));
}

/* round_to(x, places) — what money and display need.  Core round() only goes
 * to whole numbers. */
void math_round_to(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    double x = kn_arg_double(argv, 0);
    int32_t places = kn_arg_int(argv, 1);
    if (places < 0 || places > 15) { math_fail(ret, "round_to: places must be 0..15"); return; }
    double scale = pow(10.0, (double)places);
    kn_error_clear();
    kn_ret_double(ret, round(x * scale) / scale);
}

/* --- sign, range, interpolation ---------------------------------------- */
void math_sign(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    double x = kn_arg_double(argv, 0);
    kn_ret_int(ret, x > 0.0 ? 1 : (x < 0.0 ? -1 : 0));
}
void math_sign_int(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    int32_t x = kn_arg_int(argv, 0);
    kn_ret_int(ret, x > 0 ? 1 : (x < 0 ? -1 : 0));
}

void math_clamp(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    double x = kn_arg_double(argv, 0), lo = kn_arg_double(argv, 1), hi = kn_arg_double(argv, 2);
    if (lo > hi) { math_fail(ret, "clamp: low bound above high bound"); return; }
    kn_error_clear();
    kn_ret_double(ret, x < lo ? lo : (x > hi ? hi : x));
}
void math_clamp_int(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    int32_t x = kn_arg_int(argv, 0), lo = kn_arg_int(argv, 1), hi = kn_arg_int(argv, 2);
    if (lo > hi) {
        kn_error_set(KN_ERR_INVALID_ARG, "clamp_int: low bound above high bound");
        kn_ret_int(ret, x);
        return;
    }
    kn_error_clear();
    kn_ret_int(ret, x < lo ? lo : (x > hi ? hi : x));
}

/* lerp(a, b, t) — t of the way from a to b.  Written as a + t*(b - a) rather
 * than (1-t)*a + t*b so that t = 1 returns exactly b. */
void math_lerp(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    double a = kn_arg_double(argv, 0), b = kn_arg_double(argv, 1), t = kn_arg_double(argv, 2);
    kn_ret_double(ret, a + t * (b - a));
}

/* --- angles ------------------------------------------------------------ */
/* Core's trigonometry takes radians; almost every program thinks in degrees.
 * Without these that conversion gets written by hand, wrongly, once per file. */
MATH_D1(degrees, x * (180.0 / 3.14159265358979323846))
MATH_D1(radians, x * (3.14159265358979323846 / 180.0))

/* --- integer functions -------------------------------------------------- */
void math_gcd(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    int32_t a = kn_arg_int(argv, 0), b = kn_arg_int(argv, 1);
    if (a < 0) a = -a;
    if (b < 0) b = -b;
    while (b) { int32_t t = a % b; a = b; b = t; }
    kn_ret_int(ret, a);
}

void math_lcm(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    int64_t a = kn_arg_int(argv, 0), b = kn_arg_int(argv, 1);
    if (a < 0) a = -a;
    if (b < 0) b = -b;
    if (a == 0 || b == 0) { kn_error_clear(); kn_ret_int64(ret, 0); return; }
    int64_t x = a, y = b;
    while (y) { int64_t t = x % y; x = y; y = t; }
    kn_error_clear();
    kn_ret_int64(ret, a / x * b);   /* divide first: (a*b) would overflow */
}

/* factorial(n) -> int64.  21! does not fit in 64 bits, so anything past 20 is
 * a failure rather than a silently wrapped answer. */
void math_factorial(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    int32_t n = kn_arg_int(argv, 0);
    if (n < 0)  { kn_error_set(KN_ERR_INVALID_ARG, "factorial: negative"); kn_ret_int64(ret, -1); return; }
    if (n > 20) { kn_error_set(KN_ERR_INVALID_ARG, "factorial: overflows a 64-bit integer past 20"); kn_ret_int64(ret, -1); return; }
    int64_t r = 1;
    for (int32_t i = 2; i <= n; i++) r *= i;
    kn_error_clear();
    kn_ret_int64(ret, r);
}

void math_is_prime(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    int32_t n = kn_arg_int(argv, 0);
    if (n < 2) { kn_ret_bool(ret, 0); return; }
    if (n % 2 == 0) { kn_ret_bool(ret, n == 2); return; }
    for (int64_t i = 3; i * i <= (int64_t)n; i += 2) {
        if (n % (int32_t)i == 0) { kn_ret_bool(ret, 0); return; }
    }
    kn_ret_bool(ret, 1);
}

/* --- float predicates --------------------------------------------------- */
/* NaN is the one value not equal to itself, so a program cannot test for it
 * with `=`.  These are the only way to ask. */
void math_is_nan(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    kn_ret_bool(ret, isnan(kn_arg_double(argv, 0)) ? 1 : 0);
}
void math_is_finite(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    kn_ret_bool(ret, isfinite(kn_arg_double(argv, 0)) ? 1 : 0);
}
