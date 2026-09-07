/* The event loop and its source registry.
 *
 * A program that has asked to be woken later — by a timer, by a socket, by a
 * window — must not exit the moment `main` returns.  The loop that keeps it
 * alive lives here rather than in whichever library happens to be linked,
 * because a console program has no window library and would otherwise have no
 * way to wait for anything at all.  `ui` registers its frame pump as one source
 * among others; it does not own the loop.
 *
 * The contract sources are written against is in abi/kiln_abi.h.
 */
#include <string.h>
#include "kiln_core.h"

/* The one place in this file that knows the platform: a monotonic clock and a
 * sleep. Monotonic, so a clock adjustment mid-run cannot make a 50ms timer wait
 * an hour (or fire in a tight spin). */
#ifdef _WIN32
#include <windows.h>
static int64_t now_ms(void) { return (int64_t)GetTickCount64(); }
static void sleep_ms(int64_t ms) { Sleep((DWORD)ms); }
#else
#include <time.h>
static int64_t now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (int64_t)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}
static void sleep_ms(int64_t ms) {
    struct timespec ts;
    ts.tv_sec = (time_t)(ms / 1000);
    ts.tv_nsec = (long)(ms % 1000) * 1000000L;
    nanosleep(&ts, NULL);
}
#endif

/* Fixed storage.  Registering a source must never allocate: the sources that
 * exist are declared in the program's source text, so their count is known
 * before it runs, and a failed allocation while arming a timer would be a
 * failure with nowhere to report itself. */
#define KN_LOOP_MAX 32

typedef struct {
    Kiln_PumpFn pump;
    void          *state;
    int32_t        period_ms;
    int64_t        due_ms;      /* when this source next wants a turn        */
    int32_t        live;
} Source;

static Source  g_src[KN_LOOP_MAX];
static int32_t g_live;
static int32_t g_running;
static int32_t g_quit;          /* latched: survives until kn_loop_run reads it */
static int32_t g_exit_code;

int32_t kn_loop_add(Kiln_PumpFn pump, void *state, int32_t period_ms) {
    if (!pump) {
        kn_error_set(KN_ERR_INVALID_ARG, "event source has no pump");
        return 0;
    }
    for (int32_t i = 0; i < KN_LOOP_MAX; i++) {
        if (g_src[i].live) continue;
        g_src[i].pump = pump;
        g_src[i].state = state;
        g_src[i].period_ms = period_ms < 0 ? 0 : period_ms;
        g_src[i].due_ms = now_ms() + g_src[i].period_ms;
        g_src[i].live = 1;
        g_live++;
        kn_error_clear();
        return i + 1;
    }
    kn_error_set(KN_ERR_TABLE_FULL, "too many event sources");
    return 0;
}

void kn_loop_remove(int32_t source) {
    if (source < 1 || source > KN_LOOP_MAX) return;
    if (!g_src[source - 1].live) return;
    memset(&g_src[source - 1], 0, sizeof g_src[0]);
    g_live--;
}

int32_t kn_loop_live(void) { return g_live; }

void kn_loop_quit(int32_t code) {
    g_quit = 1;
    g_exit_code = code;
}

int32_t kn_loop_run(void) {
    /* A source's pump may enter the loop again (a library that runs a nested
     * modal, a handler that calls into `ui`).  One loop is enough; a second
     * would service the same sources twice per turn. */
    if (g_running) return 0;
    g_running = 1;

    while (!g_quit && g_live > 0) {
        int64_t now = now_ms();
        int64_t earliest = -1;

        for (int32_t i = 0; i < KN_LOOP_MAX; i++) {
            if (!g_src[i].live) continue;
            if (g_src[i].due_ms > now) {
                if (earliest < 0 || g_src[i].due_ms < earliest) earliest = g_src[i].due_ms;
                continue;
            }
            /* The next deadline is measured from now, not from the last one:
             * a pump that overruns its period must not then be called back to
             * back until it has caught up. */
            g_src[i].due_ms = now + g_src[i].period_ms;
            if (g_src[i].pump(g_src[i].state)) kn_loop_remove(i + 1);
            earliest = 0;   /* something ran; re-examine before sleeping */
        }

        if (earliest > 0) {
            now = now_ms();
            if (earliest > now) sleep_ms(earliest - now);
        }
    }

    g_running = 0;
    /* Read and clear together: the latch exists so a `quit` from `main`, before
     * the loop starts, is not lost — not so the program can never loop again. */
    int32_t code = g_quit ? g_exit_code : 0;
    g_quit = 0;
    g_exit_code = 0;
    return code;
}

/* --- commands --------------------------------------------------------- */

/* quit().  The counterpart to every source that keeps a program alive: without
 * it a program with a repeating timer has no way to stop short of killing it. */
void kn_quit(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)ret; (void)argc; (void)argv;
    kn_loop_quit(0);
}
