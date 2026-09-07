/* "time" library metadata (design-time only; compiled into the introspection
 * .so, never a shipped program — the same split as core_libinfo.c).
 *
 * Every command works in UTC and speaks Unix seconds as int64, the same basis
 * core's now() uses, so the two libraries compose. See time_cmds.c. */
#include "kiln_abi.h"

void time_now_ms(Kiln_Slot *, int32_t, Kiln_Slot *);
void time_monotonic_ms(Kiln_Slot *, int32_t, Kiln_Slot *);
void time_month(Kiln_Slot *, int32_t, Kiln_Slot *);
void time_day(Kiln_Slot *, int32_t, Kiln_Slot *);
void time_hour(Kiln_Slot *, int32_t, Kiln_Slot *);
void time_minute(Kiln_Slot *, int32_t, Kiln_Slot *);
void time_second(Kiln_Slot *, int32_t, Kiln_Slot *);
void time_weekday(Kiln_Slot *, int32_t, Kiln_Slot *);
void time_day_of_year(Kiln_Slot *, int32_t, Kiln_Slot *);
void time_from_parts(Kiln_Slot *, int32_t, Kiln_Slot *);
void time_add_seconds(Kiln_Slot *, int32_t, Kiln_Slot *);
void time_diff_seconds(Kiln_Slot *, int32_t, Kiln_Slot *);
void time_format_iso(Kiln_Slot *, int32_t, Kiln_Slot *);
void time_parse_iso(Kiln_Slot *, int32_t, Kiln_Slot *);
void time_is_leap_year(Kiln_Slot *, int32_t, Kiln_Slot *);
void time_days_in_month(Kiln_Slot *, int32_t, Kiln_Slot *);

static const int32_t P_I[]     = { KN_SDT_INT };
static const int32_t P_II[]    = { KN_SDT_INT, KN_SDT_INT };
static const int32_t P_I64[]   = { KN_SDT_INT64 };
static const int32_t P_I64I64[]= { KN_SDT_INT64, KN_SDT_INT64 };
static const int32_t P_T[]     = { KN_SDT_TEXT };
static const int32_t P_6I[]    = { KN_SDT_INT, KN_SDT_INT, KN_SDT_INT,
                                   KN_SDT_INT, KN_SDT_INT, KN_SDT_INT };

static const Kiln_CommandDesc TIME_COMMANDS[] = {
    /* clocks */
    { "time_now_ms",       "time_now_ms",       KN_SDT_INT64, 0, NULL },
    { "time_monotonic_ms", "time_monotonic_ms", KN_SDT_INT64, 0, NULL },
    /* UTC calendar fields of a timestamp */
    { "time_month",        "time_month",        KN_SDT_INT,   1, P_I64 },
    { "time_day",          "time_day",          KN_SDT_INT,   1, P_I64 },
    { "time_hour",         "time_hour",         KN_SDT_INT,   1, P_I64 },
    { "time_minute",       "time_minute",       KN_SDT_INT,   1, P_I64 },
    { "time_second",       "time_second",       KN_SDT_INT,   1, P_I64 },
    { "time_weekday",      "time_weekday",      KN_SDT_INT,   1, P_I64 },
    { "time_day_of_year",  "time_day_of_year",  KN_SDT_INT,   1, P_I64 },
    /* building and moving timestamps */
    { "time_from_parts",   "time_from_parts",   KN_SDT_INT64, 6, P_6I },
    { "time_add_seconds",  "time_add_seconds",  KN_SDT_INT64, 2, P_I64I64 },
    { "time_diff_seconds", "time_diff_seconds", KN_SDT_INT64, 2, P_I64I64 },
    /* ISO 8601, UTC */
    { "time_format_iso",   "time_format_iso",   KN_SDT_TEXT,  1, P_I64 },
    { "time_parse_iso",    "time_parse_iso",    KN_SDT_INT64, 1, P_T },
    /* calendar questions */
    { "time_is_leap_year", "time_is_leap_year", KN_SDT_BOOL,  1, P_I },
    { "time_days_in_month","time_days_in_month",KN_SDT_INT,   2, P_II },
};

static const Kiln_LibInfo TIME_INFO = {
    KILN_ABI_VERSION,
    "time",
    "kiln-time-7c1e-4b2a-9f30-5d8e2a41c6b7",
    0, 1, 0,
    (int32_t)(sizeof(TIME_COMMANDS) / sizeof(TIME_COMMANDS[0])),
    TIME_COMMANDS,
};

const Kiln_LibInfo *kiln_get_lib_info(void) {
    return &TIME_INFO;
}
