/* The core library's non-visual components, and the entry points a program
 * addresses them through (abi/kiln_abi.h).
 *
 * `timer` is here rather than in a library of its own because it is the proof
 * that the loop belongs to the runtime: a console program with no window and no
 * `use` line can still ask to be woken later.  It is also the smallest possible
 * event source, so anything the shape gets wrong shows up immediately.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "kiln_core.h"

#define KN_TIMERS_MAX 16

/* `tick` hands its handler the tick count, so the pointer is called as this.
 * The compiler emits the handler side with exactly this signature whatever the
 * user's subroutine takes (backend/src/lib.rs, `handler_symbol`), so the cast
 * back is the one the callee was written with. */
typedef void (*TickFn)(int32_t);

typedef struct {
    int32_t        in_use;
    int32_t        interval_ms;
    int32_t        enabled;
    int32_t        source;      /* loop source id while armed, else 0        */
    int32_t        ticks;       /* how many have fired; what `tick` hands on */
    Kiln_HandlerFn on_tick;
} Timer;

static Timer   g_timers[KN_TIMERS_MAX];
/* Handles count from 1 in creation order, which is the whole reason the
 * compiler can hard-code them and no instance id has to reach the binary. */
static int32_t g_next_handle;

static Timer *resolve(int64_t h) {
    if (h < 1 || h > KN_TIMERS_MAX) return NULL;
    Timer *t = &g_timers[h - 1];
    return t->in_use ? t : NULL;
}

static int32_t tick(void *state) {
    Timer *t = (Timer *)state;
    /* Counted whether or not anyone is listening, and never reset by `rearm`:
     * changing the interval is not a reason for a program to be told it is
     * back at the first tick. */
    t->ticks++;
    if (t->on_tick) ((TickFn)t->on_tick)(t->ticks);
    /* Live until something disables it: a repeating timer is what a program
     * that wants one shot can stop from inside its own handler. */
    return 0;
}

/* Bring the registration in line with the properties.  Called after every
 * change rather than only at creation, because the compiler emits `create`
 * before the property assignments that follow it in the source. */
static void rearm(Timer *t) {
    if (t->source) {
        kn_loop_remove(t->source);
        t->source = 0;
    }
    if (t->enabled) t->source = kn_loop_add(tick, t, t->interval_ms);
}

int64_t kn_core_component_create(const char *type_name) {
    if (!type_name || strcmp(type_name, "timer") != 0) {
        kn_error_set(KN_ERR_INVALID_ARG, "core declares no such component type");
        return 0;
    }
    if (g_next_handle >= KN_TIMERS_MAX) {
        kn_error_set(KN_ERR_TABLE_FULL, "too many timers");
        return 0;
    }
    Timer *t = &g_timers[g_next_handle++];
    t->in_use = 1;
    t->interval_ms = 1000;
    t->enabled = 1;
    rearm(t);
    kn_error_clear();
    return g_next_handle;
}

int32_t kn_core_component_set(int64_t h, const char *prop, const char *value) {
    Timer *t = resolve(h);
    if (!t || !prop || !value) return 1;
    if (strcmp(prop, "interval") == 0) {
        int32_t ms = (int32_t)strtol(value, NULL, 10);
        /* A zero-interval timer would fire every turn and spin the loop at
         * 100%, which reads as a hung program rather than a fast one. */
        t->interval_ms = ms < 1 ? 1 : ms;
    } else if (strcmp(prop, "enabled") == 0) {
        t->enabled = (strcmp(value, "true") == 0 || strcmp(value, "1") == 0);
    } else {
        return 1;
    }
    rearm(t);
    return 0;
}

const char *kn_core_component_get(int64_t h, const char *prop) {
    Timer *t = resolve(h);
    if (!t || !prop) return NULL;
    if (strcmp(prop, "enabled") == 0) return t->enabled ? "true" : "false";
    if (strcmp(prop, "interval") == 0) {
        /* Runtime-owned like every other text result, so a caller may hold it
         * and nest it the way `kn_ui_get` already promises. */
        char *out = (char *)kn_malloc(16);
        if (out) snprintf(out, 16, "%d", t->interval_ms);
        return out;
    }
    return NULL;
}

int32_t kn_core_component_get_int(int64_t h, const char *prop) {
    Timer *t = resolve(h);
    if (!t || !prop) return 0;
    if (strcmp(prop, "interval") == 0) return t->interval_ms;
    if (strcmp(prop, "enabled") == 0) return t->enabled;
    return 0;
}

int32_t kn_core_component_on(int64_t h, const char *event, Kiln_HandlerFn handler) {
    Timer *t = resolve(h);
    if (!t || !event || !handler) return 1;
    if (strcmp(event, "tick") != 0) return 1;
    t->on_tick = handler;
    return 0;
}
