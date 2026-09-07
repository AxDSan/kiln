/* The handle table — how a program holds a resource it cannot be given.
 *
 * A program never sees a pointer.  It sees a small positive int that indexes
 * this table, so it cannot forge a handle, cannot dereference one, and cannot
 * be handed one that dangles.  The encoding and its limits are documented in
 * abi/kiln_abi.h; this file is the implementation of that contract.
 */
#include <stdlib.h>
#include "kiln_core.h"

#define H_MAKE(k, g, i) ( ((int32_t)((k) & 0xF)   << 27) | \
                          ((int32_t)((g) & 0x7FF) << 16) | \
                          ((int32_t)((i) & 0xFFFF)) )
#define H_KIND(h)  (((h) >> 27) & 0xF)
#define H_GEN(h)   (((h) >> 16) & 0x7FF)
#define H_INDEX(h) ( (h)        & 0xFFFF)

#define H_MAX_INDEX 0xFFFF

typedef struct {
    int32_t                kind;      /* KN_HK_NONE when free */
    uint32_t               gen;
    void                  *payload;
    Kiln_HandleCloseFn  close_fn;
} Entry;

/* Plain malloc, not E_MAlloc: this is runtime bookkeeping, not program data.
 * Keeping it out of the tracked block list removes the ordering hazard where
 * kn_free_all() could free the table out from under a close function. */
static Entry  *g_tab = NULL;
static int32_t g_cap = 0;      /* slot 0 is reserved, so g_cap >= 1 when live */
static int     g_atexit_armed = 0;

static int grow(void) {
    int32_t want = g_cap ? g_cap * 2 : 32;
    if (want > H_MAX_INDEX + 1) want = H_MAX_INDEX + 1;
    if (want <= g_cap) return 0;                     /* already at the ceiling */
    Entry *t = (Entry *)realloc(g_tab, (size_t)want * sizeof(Entry));
    if (!t) return 0;
    for (int32_t i = g_cap; i < want; i++) {
        t[i].kind = KN_HK_NONE;
        t[i].gen = 1;            /* generations start at 1, never at 0 */
        t[i].payload = NULL;
        t[i].close_fn = NULL;
    }
    g_tab = t;
    g_cap = want;
    return 1;
}

/* Decode and validate in one place, so every command reports identically. */
static Entry *lookup(int32_t h, int32_t kind) {
    if (h <= 0) { kn_error_set(KN_ERR_BAD_HANDLE, "not a handle"); return NULL; }
    int32_t i = H_INDEX(h);
    if (i <= 0 || i >= g_cap) {
        kn_error_set(KN_ERR_BAD_HANDLE, "handle out of range");
        return NULL;
    }
    Entry *e = &g_tab[i];
    if (e->kind == KN_HK_NONE || e->gen != (uint32_t)H_GEN(h)) {
        kn_error_set(KN_ERR_STALE, "handle already closed");
        return NULL;
    }
    if (H_KIND(h) != e->kind || (kind != KN_HK_NONE && e->kind != kind)) {
        kn_error_set(KN_ERR_WRONG_KIND, "handle is of a different kind");
        return NULL;
    }
    return e;
}

int32_t kn_handle_new(int32_t kind, void *payload, Kiln_HandleCloseFn close_fn) {
    if (kind <= KN_HK_NONE || kind > 0xF) {
        kn_error_set(KN_ERR_INVALID_ARG, "invalid handle kind");
        return 0;
    }
    /* Index 0 is permanently reserved so that 0 is never a live handle. */
    if (g_cap == 0 && !grow()) {
        kn_error_set(KN_ERR_TABLE_FULL, "cannot allocate the handle table");
        return 0;
    }
    int32_t i = 1;
    for (;;) {
        for (; i < g_cap; i++) {
            if (g_tab[i].kind == KN_HK_NONE) goto found;
        }
        if (!grow()) {
            kn_error_set(KN_ERR_TABLE_FULL, "too many open handles");
            return 0;
        }
    }
found:
    g_tab[i].kind = kind;
    g_tab[i].payload = payload;
    g_tab[i].close_fn = close_fn;

    /* Armed on first use, so a program that opens nothing pays nothing.  This
     * is the only cleanup a library-target build gets: E_DestroyRes runs solely
     * from kn_start.c's main, which those builds exclude.  It also covers the
     * path E_DestroyRes misses entirely — KN_NRS_RUNTIME_ERR calls exit(1),
     * which runs atexit handlers. */
    if (!g_atexit_armed) {
        atexit(kn_handle_close_all);
        g_atexit_armed = 1;
    }
    return H_MAKE(kind, g_tab[i].gen, i);
}

void *kn_handle_resolve(int32_t h, int32_t kind) {
    Entry *e = lookup(h, kind);
    return e ? e->payload : NULL;
}

int32_t kn_handle_kind_of(int32_t h) {
    Entry *e = lookup(h, KN_HK_NONE);
    return e ? e->kind : 0;
}

/* Retire the entry BEFORE running the close function.  Clearing afterwards
 * would leave a live-looking entry if the close function aborted, and a double
 * close must report STALE regardless of what the close function did. */
static void retire(Entry *e) {
    void *payload = e->payload;
    Kiln_HandleCloseFn fn = e->close_fn;
    e->kind = KN_HK_NONE;
    e->payload = NULL;
    e->close_fn = NULL;
    e->gen = (e->gen + 1) & 0x7FF;
    if (e->gen == 0) e->gen = 1;      /* 0 is not a legal generation */
    if (fn) fn(payload);
}

int32_t kn_handle_close(int32_t h, int32_t kind) {
    Entry *e = lookup(h, kind);
    if (!e) return 0;
    retire(e);
    kn_error_clear();
    return 1;
}

int32_t kn_handle_close_kind(int32_t kind) {
    int32_t n = 0;
    for (int32_t i = 1; i < g_cap; i++) {
        if (g_tab[i].kind == kind) { retire(&g_tab[i]); n++; }
    }
    kn_error_clear();
    return n;
}

void kn_handle_close_all(void) {
    if (!g_tab) return;               /* idempotent: both hooks fire in an exe */
    for (int32_t i = 1; i < g_cap; i++) {
        if (g_tab[i].kind != KN_HK_NONE) retire(&g_tab[i]);
    }
    free(g_tab);
    g_tab = NULL;
    g_cap = 0;
}
