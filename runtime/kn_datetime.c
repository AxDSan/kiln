/* Core commands: date/time (slot ABI), UTC, int64 Unix seconds. */
#include <string.h>
#include <time.h>
#include "kiln_core.h"

/* Windows has no gmtime_r, and its gmtime_s refuses any instant before 1970
 * — `year()` of a birthday in 1969 would answer 0 there and 1969 everywhere
 * else. So on Windows the civil date is computed here instead of asked for
 * (Howard Hinnant's civil_from_days), which answers for any int64 second the
 * same way on every platform. One shim, so the commands below read the same
 * on both — the rule libs/README.md states for every library. */
static void kn_gmtime(time_t t, struct tm *out){
    memset(out, 0, sizeof *out);
#ifdef _WIN32
    long long secs = (long long)t;
    long long days = secs / 86400;
    long long rem  = secs % 86400;
    if (rem < 0) { rem += 86400; days -= 1; }
    out->tm_hour = (int)(rem / 3600);
    out->tm_min  = (int)(rem % 3600 / 60);
    out->tm_sec  = (int)(rem % 60);
    /* Day 0 was a Thursday; the +11 keeps a negative remainder in range. */
    out->tm_wday = (int)((days % 7 + 11) % 7);
    long long z   = days + 719468;
    long long era = (z >= 0 ? z : z - 146096) / 146097;
    unsigned  doe = (unsigned)(z - era * 146097);
    unsigned  yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    long long y   = (long long)yoe + era * 400;
    unsigned  doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    unsigned  mp  = (5 * doy + 2) / 153;
    unsigned  d   = doy - (153 * mp + 2) / 5 + 1;
    unsigned  m   = mp < 10 ? mp + 3 : mp - 9;
    if (m <= 2) y++;
    static const int before_month[] = {0,31,59,90,120,151,181,212,243,273,304,334};
    int leap = (y % 4 == 0 && (y % 100 != 0 || y % 400 == 0));
    out->tm_year = (int)(y - 1900);
    out->tm_mon  = (int)m - 1;
    out->tm_mday = (int)d;
    out->tm_yday = before_month[m - 1] + (int)d - 1 + (leap && m > 2 ? 1 : 0);
#else
    gmtime_r(&t, out);
#endif
}

void kn_now(Kiln_Slot *r, int32_t c, Kiln_Slot *argv){ (void)c; (void)argv; kn_ret_int64(r,(long long)time(NULL)); }

void kn_year(Kiln_Slot *r, int32_t c, Kiln_Slot *argv){
    (void)c; time_t t=(time_t)kn_arg_int64(argv,0); struct tm g; kn_gmtime(t,&g); kn_ret_int(r,g.tm_year+1900);
}
void kn_format_time(Kiln_Slot *r, int32_t c, Kiln_Slot *argv){
    (void)c; time_t t=(time_t)kn_arg_int64(argv,0); const char*fmt=kn_arg_text(argv,1);
    struct tm g; kn_gmtime(t,&g); char buf[256];
    size_t n=strftime(buf,sizeof buf, fmt?fmt:"%Y-%m-%d %H:%M:%S", &g);
    char*o=(char*)kn_malloc((long)n+1); memcpy(o,buf,n); o[n]='\0'; kn_ret_text(r,o);
}
