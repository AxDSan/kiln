/* Handler environments, and keeping them alive.
 *
 * A handler wired in a form's designer block reaches the form's state through
 * module globals, and needs nothing else. A handler built at run time does:
 * one button per row of a grid has to know which row, and that cannot live in
 * a global because there is one of it and many rows. So such a handler is
 * bound as a pair — the function, and the environment it closes over.
 *
 * That environment is the problem this file exists for. It is allocated by the
 * collector and then stored in a UI library's own tables, which are C++ memory
 * the collector does not scan. Nothing on the Kiln side refers to it any more,
 * so the next collection frees it, and the next click calls a function whose
 * captured variables are gone. The symptom is a crash in the event loop under
 * memory pressure, arbitrarily far from the cause.
 *
 * So a library that stores an environment holds it here first. The table is
 * itself collector-allocated and rooted, and the collector traces a marked
 * block's contents — so every environment the table holds is reachable, and
 * stays alive exactly as long as the handler that needs it.
 *
 * The token a hold returns is an index, never a pointer: it survives the table
 * growing, and a library that keeps one can release it later without having to
 * have kept the environment pointer too.
 */
#include "kiln_core.h"
#include "kn_gc.h"

#include <stdlib.h>
#include <string.h>

/* The table: `E_MAlloc`ed so the collector traces it, and rooted so it is
 * traced at all. A freed slot is NULL and is reused before the table grows. */
static void  **g_envs  = NULL;
static int32_t g_cap   = 0;
static int32_t g_count = 0;   /* slots in use, for kn_handler_count */
static int32_t g_rooted = 0;

static int grow(int32_t want) {
    void **fresh = (void **)E_MAlloc((long)want * (long)sizeof *fresh);
    if (!fresh) return 0;
    memset(fresh, 0, (size_t)want * sizeof *fresh);
    if (g_envs && g_cap) memcpy(fresh, g_envs, (size_t)g_cap * sizeof *fresh);
    g_envs = fresh;
    g_cap  = want;
    /* One root, for the table itself. Rooting each slot instead would dangle
     * the moment the table moved, which is what growing it does. */
    if (!g_rooted) {
        kn_gc_root((void **)&g_envs);
        g_rooted = 1;
    }
    return 1;
}

int32_t kn_handler_hold(void *env) {
    if (!env) return -1;
    for (int32_t i = 0; i < g_cap; i++) {
        if (!g_envs[i]) {
            g_envs[i] = env;
            g_count++;
            return i;
        }
    }
    /* Full: the first slot of the new half is the one to use. Taking `g_count`
     * instead would be right only while nothing had ever been released. */
    const int32_t first_new = g_cap;
    if (!grow(g_cap ? g_cap * 2 : 16)) return -1;
    g_envs[first_new] = env;
    g_count++;
    return first_new;
}

void kn_handler_release(int32_t token) {
    if (token < 0 || token >= g_cap || !g_envs[token]) return;
    g_envs[token] = NULL;
    g_count--;
}

void *kn_handler_env(int32_t token) {
    if (token < 0 || token >= g_cap) return NULL;
    return g_envs[token];
}

int32_t kn_handler_count(void) { return g_count; }
