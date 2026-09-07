/* "math" library metadata (design-time only; compiled into the introspection
 * .so, never a shipped program — same split as core_libinfo.c). */
#include "kiln_abi.h"

static const int32_t P_D[]   = { KN_SDT_DOUBLE };
static const int32_t P_DD[]  = { KN_SDT_DOUBLE, KN_SDT_DOUBLE };
static const int32_t P_DDD[] = { KN_SDT_DOUBLE, KN_SDT_DOUBLE, KN_SDT_DOUBLE };
static const int32_t P_DI[]  = { KN_SDT_DOUBLE, KN_SDT_INT };
static const int32_t P_I[]   = { KN_SDT_INT };
static const int32_t P_II[]  = { KN_SDT_INT, KN_SDT_INT };
static const int32_t P_III[] = { KN_SDT_INT, KN_SDT_INT, KN_SDT_INT };

#define CMD(name, sym, ret, argc, tags) { name, #sym, ret, argc, tags }

static const Kiln_CommandDesc MATH_COMMANDS[] = {
    /* constants */
    CMD("math_pi",       math_pi,       KN_SDT_DOUBLE, 0, NULL),
    CMD("math_tau",      math_tau,      KN_SDT_DOUBLE, 0, NULL),
    CMD("math_e",        math_e,        KN_SDT_DOUBLE, 0, NULL),
    CMD("math_infinity", math_infinity, KN_SDT_DOUBLE, 0, NULL),
    /* inverse trigonometry */
    CMD("math_asin",  math_asin,  KN_SDT_DOUBLE, 1, P_D),
    CMD("math_acos",  math_acos,  KN_SDT_DOUBLE, 1, P_D),
    CMD("math_atan",  math_atan,  KN_SDT_DOUBLE, 1, P_D),
    CMD("math_atan2", math_atan2, KN_SDT_DOUBLE, 2, P_DD),
    /* hyperbolic */
    CMD("math_sinh", math_sinh, KN_SDT_DOUBLE, 1, P_D),
    CMD("math_cosh", math_cosh, KN_SDT_DOUBLE, 1, P_D),
    CMD("math_tanh", math_tanh, KN_SDT_DOUBLE, 1, P_D),
    /* logarithms and roots */
    CMD("math_log2",     math_log2,     KN_SDT_DOUBLE, 1, P_D),
    CMD("math_log_base", math_log_base, KN_SDT_DOUBLE, 2, P_DD),
    CMD("math_cbrt",     math_cbrt,     KN_SDT_DOUBLE, 1, P_D),
    CMD("math_hypot",    math_hypot,    KN_SDT_DOUBLE, 2, P_DD),
    /* rounding and remainder */
    CMD("math_trunc",    math_trunc,    KN_SDT_DOUBLE, 1, P_D),
    CMD("math_fmod",     math_fmod,     KN_SDT_DOUBLE, 2, P_DD),
    CMD("math_round_to", math_round_to, KN_SDT_DOUBLE, 2, P_DI),
    /* sign, range, interpolation */
    CMD("math_sign",      math_sign,      KN_SDT_INT,    1, P_D),
    CMD("math_sign_int",  math_sign_int,  KN_SDT_INT,    1, P_I),
    CMD("math_clamp",     math_clamp,     KN_SDT_DOUBLE, 3, P_DDD),
    CMD("math_clamp_int", math_clamp_int, KN_SDT_INT,    3, P_III),
    CMD("math_lerp",      math_lerp,      KN_SDT_DOUBLE, 3, P_DDD),
    /* angles */
    CMD("math_degrees", math_degrees, KN_SDT_DOUBLE, 1, P_D),
    CMD("math_radians", math_radians, KN_SDT_DOUBLE, 1, P_D),
    /* integers */
    CMD("math_gcd",       math_gcd,       KN_SDT_INT,   2, P_II),
    CMD("math_lcm",       math_lcm,       KN_SDT_INT64, 2, P_II),
    CMD("math_factorial", math_factorial, KN_SDT_INT64, 1, P_I),
    CMD("math_is_prime",  math_is_prime,  KN_SDT_BOOL,  1, P_I),
    /* float predicates */
    CMD("math_is_nan",    math_is_nan,    KN_SDT_BOOL, 1, P_D),
    CMD("math_is_finite", math_is_finite, KN_SDT_BOOL, 1, P_D),
};

static const Kiln_LibInfo MATH_INFO = {
    KILN_ABI_VERSION,
    "math",
    "kiln-math-0000-0000-0000-6d6174680001",
    0, 1, 0,
    (int32_t)(sizeof(MATH_COMMANDS) / sizeof(MATH_COMMANDS[0])),
    MATH_COMMANDS,
    0, NULL,
};

const Kiln_LibInfo *kiln_get_lib_info(void) { return &MATH_INFO; }
