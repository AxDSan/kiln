/* The "time" support library — dates and clocks.
 *
 * UTC EVERYWHERE. Every command in this library that turns a timestamp into
 * calendar fields, or calendar fields into a timestamp, works in UTC — the
 * same basis as core's now(), year() and format_time(). There is deliberately
 * no local-time variant: a program that mixes the two silently produces
 * answers that are right on the author's machine and wrong on a server in
 * another zone, and that bug is invisible until it is expensive.
 *
 * Timestamps are Unix seconds as int64, so this library and core compose:
 * time_month(now()) is meaningful, and time_from_parts(...) can be handed to
 * core's format_time().
 *
 * Public SDK header only — no runtime-internal header.
 */
#include <string.h>
#include <stdio.h>
#include <time.h>
#include <limits.h>

/* Windows has no clock_gettime in its own C library — mingw supplies one,
 * MSVC does not — so the two clocks below go through Win32 there.  Everything
 * else in this file is arithmetic and needs no branch: the calendar is
 * computed here on every platform rather than asked of gmtime, because
 * Windows' gmtime_s refuses any instant before 1970. */
#ifdef _WIN32
#include <windows.h>
#endif

#include "kiln_abi.h"

/* --- civil-date arithmetic ------------------------------------------------
 * Days between 1970-01-01 and y-m-d in the proleptic Gregorian calendar
 * (Howard Hinnant's days_from_civil). Written out rather than calling timegm()
 * because timegm is not standard C and is absent on some platforms; this is
 * exact for every year an int64 second count can reach. Internal linkage, so
 * it needs no prefix. */
static int64_t days_from_civil(int64_t y, int m, int d) {
    y -= (m <= 2);
    int64_t era = (y >= 0 ? y : y - 399) / 400;
    int64_t yoe = y - era * 400;                     /* [0, 399] */
    int64_t doy = (153 * (m + (m > 2 ? -3 : 9)) + 2) / 5 + d - 1;
    int64_t doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    return era * 146097 + doe - 719468;
}

static int is_leap(int y) {
    return (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0);
}

static int month_length(int y, int m) {
    static const int len[12] = { 31,28,31,30,31,30,31,31,30,31,30,31 };
    if (m < 1 || m > 12) return -1;
    if (m == 2 && is_leap(y)) return 29;
    return len[m - 1];
}

/* Break a timestamp into UTC fields — Howard Hinnant's civil_from_days, the
 * inverse of days_from_civil above, and the same arithmetic the runtime's
 * year() and format_time() use. It is NOT gmtime: Windows' gmtime_s answers
 * an error for any instant before 1970, so a birthday in 1969 would be an
 * empty text there and a date everywhere else. Computing the date answers
 * the same on every platform for any int64 second, which is what "UTC
 * everywhere" promises. Returns 0 only when the year does not fit the int
 * struct tm holds — a timestamp no calendar can name. */
static int utc_fields(int64_t ts, struct tm *out) {
    memset(out, 0, sizeof *out);
    int64_t days = ts / 86400;
    int64_t rem  = ts % 86400;
    if (rem < 0) { rem += 86400; days -= 1; }
    out->tm_hour = (int)(rem / 3600);
    out->tm_min  = (int)(rem % 3600 / 60);
    out->tm_sec  = (int)(rem % 60);
    /* Day 0 was a Thursday; the +11 keeps a negative remainder in range. */
    out->tm_wday = (int)((days % 7 + 11) % 7);
    int64_t z   = days + 719468;
    int64_t era = (z >= 0 ? z : z - 146096) / 146097;
    unsigned doe = (unsigned)(z - era * 146097);                       /* [0, 146096] */
    unsigned yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; /* [0, 399] */
    int64_t  y   = (int64_t)yoe + era * 400;
    unsigned doy = doe - (365 * yoe + yoe / 4 - yoe / 100);             /* [0, 365] */
    unsigned mp  = (5 * doy + 2) / 153;                                 /* [0, 11] */
    unsigned d   = doy - (153 * mp + 2) / 5 + 1;                        /* [1, 31] */
    unsigned m   = mp < 10 ? mp + 3 : mp - 9;                           /* [1, 12] */
    if (m <= 2) y++;
    if (y - 1900 > INT_MAX || y - 1900 < INT_MIN) return 0;
    static const int before_month[12] = {0,31,59,90,120,151,181,212,243,273,304,334};
    int leap = is_leap((int)(y % 400));   /* leap-ness repeats every 400 years */
    out->tm_year = (int)(y - 1900);
    out->tm_mon  = (int)m - 1;
    out->tm_mday = (int)d;
    out->tm_yday = before_month[m - 1] + (int)d - 1 + (leap && m > 2 ? 1 : 0);
    return 1;
}

static char *time_alloc(long len) { return (char *)kn_malloc(len + 1); }

/* --- clocks ---------------------------------------------------------------
 * Two clocks, and they are NOT interchangeable. */

/* time_now_ms() -> int64 : Unix milliseconds, UTC. The wall clock — it can
 * jump backwards when the system clock is corrected, so it is for timestamps,
 * not for measuring how long something took. Infallible. */
void time_now_ms(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; (void)argv;
#ifdef _WIN32
    /* A FILETIME counts 100ns ticks from 1601-01-01; 11644473600 seconds
     * separate that epoch from the Unix one. */
    FILETIME ft;
    ULARGE_INTEGER u;
    GetSystemTimeAsFileTime(&ft);
    u.LowPart = ft.dwLowDateTime;
    u.HighPart = ft.dwHighDateTime;
    kn_ret_int64(ret, (int64_t)(u.QuadPart / 10000) - 11644473600000LL);
#else
    struct timespec ts;
    if (clock_gettime(CLOCK_REALTIME, &ts) != 0) { kn_ret_int64(ret, (int64_t)time(NULL) * 1000); return; }
    kn_ret_int64(ret, (int64_t)ts.tv_sec * 1000 + ts.tv_nsec / 1000000);
#endif
}

/* time_monotonic_ms() -> int64 : milliseconds from an ARBITRARY, unspecified
 * origin that never moves backwards. Only the DIFFERENCE between two readings
 * means anything — this is not a Unix time and must never be compared with
 * one, formatted, or passed to time_month and friends. Infallible. */
void time_monotonic_ms(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; (void)argv;
#ifdef _WIN32
    /* GetTickCount64 rather than QueryPerformanceCounter: this command's unit
     * is the millisecond, and a counter that already counts them cannot drift
     * from its own frequency. */
    kn_ret_int64(ret, (int64_t)GetTickCount64());
#else
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0) { kn_ret_int64(ret, 0); return; }
    kn_ret_int64(ret, (int64_t)ts.tv_sec * 1000 + ts.tv_nsec / 1000000);
#endif
}

/* --- calendar fields of a timestamp, UTC ----------------------------------
 * Infallible, like core's year(): they never touch the error slot, so an
 * earlier failure survives a line of date arithmetic. A timestamp whose year
 * no calendar can name yields 0, which is not a valid month, day,
 * weekday-as-ISO or day-of-year, and is a legitimate hour/minute/second only
 * for a timestamp that was representable anyway. */
#define TIME_FIELD(fn, expr)                                                  \
    void fn(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {            \
        (void)argc;                                                           \
        struct tm g;                                                          \
        if (!utc_fields(kn_arg_int64(argv, 0), &g)) { kn_ret_int(ret, 0); return; } \
        kn_ret_int(ret, (int32_t)(expr));                                     \
    }

TIME_FIELD(time_month,       g.tm_mon + 1)     /* 1..12                      */
TIME_FIELD(time_day,         g.tm_mday)        /* 1..31                      */
TIME_FIELD(time_hour,        g.tm_hour)        /* 0..23                      */
TIME_FIELD(time_minute,      g.tm_min)         /* 0..59                      */
TIME_FIELD(time_second,      g.tm_sec)         /* 0..59                      */
TIME_FIELD(time_weekday,     g.tm_wday == 0 ? 7 : g.tm_wday)  /* ISO: Mon=1..Sun=7 */
TIME_FIELD(time_day_of_year, g.tm_yday + 1)    /* 1..366                     */

/* --- building and moving timestamps --------------------------------------- */

/* time_from_parts(year, month, day, hour, minute, second) -> int64 : the UTC
 * timestamp for that calendar moment, or -1 with the error slot set when any
 * field is out of range (month 13, 30 February, hour 24). FALLIBLE — -1 is
 * also a real timestamp (1969-12-31T23:59:59Z), so a program that cares about
 * that second must read last_error_code() to tell them apart. */
void time_from_parts(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    int y  = kn_arg_int(argv, 0), mo = kn_arg_int(argv, 1), d = kn_arg_int(argv, 2);
    int h  = kn_arg_int(argv, 3), mi = kn_arg_int(argv, 4), s = kn_arg_int(argv, 5);
    int dim = month_length(y, mo);
    if (dim < 0 || d < 1 || d > dim || h < 0 || h > 23 ||
        mi < 0 || mi > 59 || s < 0 || s > 59) {
        kn_error_set(KN_ERR_INVALID_ARG, "time_from_parts: field out of range");
        kn_ret_int64(ret, -1);
        return;
    }
    int64_t days = days_from_civil(y, mo, d);
    kn_error_clear();
    kn_ret_int64(ret, days * 86400 + (int64_t)h * 3600 + (int64_t)mi * 60 + s);
}

/* time_add_seconds(timestamp, seconds) -> int64. Infallible; seconds may be
 * negative to move backwards. */
void time_add_seconds(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    kn_ret_int64(ret, kn_arg_int64(argv, 0) + kn_arg_int64(argv, 1));
}

/* time_diff_seconds(later, earlier) -> int64 : later - earlier, so a positive
 * result means the first argument is the later moment. Infallible. */
void time_diff_seconds(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    kn_ret_int64(ret, kn_arg_int64(argv, 0) - kn_arg_int64(argv, 1));
}

/* --- ISO 8601 ------------------------------------------------------------- */

/* time_format_iso(timestamp) -> text : "YYYY-MM-DDTHH:MM:SSZ", UTC. The
 * trailing Z is written always, so the text says which zone it is in and
 * time_parse_iso reads back exactly what was written. FALLIBLE: "" with the
 * error slot set when the timestamp is outside the representable range. */
void time_format_iso(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    struct tm g;
    if (!utc_fields(kn_arg_int64(argv, 0), &g)) {
        char *empty = time_alloc(0);
        empty[0] = '\0';
        kn_ret_text(ret, empty);
        kn_error_set(KN_ERR_INVALID_ARG, "time_format_iso: timestamp out of range");
        return;
    }
    char buf[64];
    int n = snprintf(buf, sizeof buf, "%04d-%02d-%02dT%02d:%02d:%02dZ",
                     g.tm_year + 1900, g.tm_mon + 1, g.tm_mday,
                     g.tm_hour, g.tm_min, g.tm_sec);
    if (n < 0) n = 0;
    char *o = time_alloc(n);
    memcpy(o, buf, (size_t)n);
    o[n] = '\0';
    kn_error_clear();
    kn_ret_text(ret, o);
}

static int digits(const char *s, int n) {
    for (int i = 0; i < n; i++) if (s[i] < '0' || s[i] > '9') return 0;
    return 1;
}
static int num(const char *s, int n) {
    int v = 0;
    for (int i = 0; i < n; i++) v = v * 10 + (s[i] - '0');
    return v;
}

/* time_parse_iso(text) -> int64 : reads "YYYY-MM-DDTHH:MM:SSZ" as UTC and
 * returns the timestamp, or -1 with the error slot set on anything else.
 * FALLIBLE — and -1 is also the timestamp of 1969-12-31T23:59:59Z, so this is
 * exactly the command whose caller must read last_error_code() to be sure.
 *
 * Accepted, all understood as UTC: the full form above; a space in place of
 * the T; the trailing Z omitted; and a bare "YYYY-MM-DD", which means midnight
 * that day. Anything else — an offset like +02:00, fractional seconds, a
 * two-digit year — is rejected rather than guessed at. */
void time_parse_iso(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *s = kn_arg_text(argv, 0);
    if (!s) s = "";
    size_t len = strlen(s);
    int y, mo, d, h = 0, mi = 0, sec = 0;

    if (len < 10 || !digits(s, 4) || s[4] != '-' || !digits(s + 5, 2) ||
        s[7] != '-' || !digits(s + 8, 2))
        goto bad;
    y = num(s, 4); mo = num(s + 5, 2); d = num(s + 8, 2);

    if (len > 10) {
        if (len < 19) goto bad;
        if (s[10] != 'T' && s[10] != 't' && s[10] != ' ') goto bad;
        if (!digits(s + 11, 2) || s[13] != ':' || !digits(s + 14, 2) ||
            s[16] != ':' || !digits(s + 17, 2))
            goto bad;
        h = num(s + 11, 2); mi = num(s + 14, 2); sec = num(s + 17, 2);
        if (len == 20 && (s[19] == 'Z' || s[19] == 'z')) { /* explicit UTC */ }
        else if (len != 19) goto bad;
    }

    {
        int dim = month_length(y, mo);
        if (dim < 0 || d < 1 || d > dim || h > 23 || mi > 59 || sec > 59) goto bad;
        int64_t t = days_from_civil(y, mo, d) * 86400 +
                    (int64_t)h * 3600 + (int64_t)mi * 60 + sec;
        kn_error_clear();
        kn_ret_int64(ret, t);
        return;
    }
bad:
    kn_error_set(KN_ERR_INVALID_ARG, "time_parse_iso: not an ISO 8601 UTC date-time");
    kn_ret_int64(ret, -1);
}

/* --- calendar questions --------------------------------------------------- */

/* time_is_leap_year(year) -> bool. Proleptic Gregorian: 2024 yes, 1900 no,
 * 2000 yes. Infallible, so a false here is always a genuine "no". */
void time_is_leap_year(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    kn_ret_bool(ret, is_leap(kn_arg_int(argv, 0)));
}

/* time_days_in_month(year, month) -> int : 28..31, or -1 with the error slot
 * set when month is not 1..12. FALLIBLE. */
void time_days_in_month(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    int n = month_length(kn_arg_int(argv, 0), kn_arg_int(argv, 1));
    if (n < 0) {
        kn_error_set(KN_ERR_INVALID_ARG, "time_days_in_month: month must be 1..12");
        kn_ret_int(ret, -1);
        return;
    }
    kn_error_clear();
    kn_ret_int(ret, n);
}
