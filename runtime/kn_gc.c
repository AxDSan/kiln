/* The collector.
 *
 * Kiln's allocator has always been a list: E_MAlloc pushes, and nothing pops
 * until the process exits.  That is fine for a program that runs, prints and
 * stops.  It is not fine for a game server, a UI that lives for hours, or any
 * loop that builds a value per iteration — a two-million-iteration loop
 * building one short text per pass held 220 MB, all of it unreachable.
 *
 * What is here is a conservative mark-and-sweep collector, in the Boehm shape:
 *
 *   - Roots are the machine stack, the callee-saved registers, the program's
 *     module-level variables, and anything a library explicitly rooted.
 *   - Everything reachable from a root, transitively, is marked.
 *   - Everything else is freed.
 *
 * "Conservative" means the stack is read as a field of machine words and any
 * word that could be a pointer into a block is treated as one.  That keeps a
 * live value alive without the compiler having to tell us where pointers are,
 * which matters because at -O2 a text can live in a register, in a spill slot,
 * or as an interior pointer to a record's third field.  The cost is that an
 * integer that happens to look like an address keeps one block alive for one
 * cycle.  That is the right trade: the failure mode is a little garbage, not a
 * freed string.
 *
 * Why this design and not the alternatives:
 *
 *   Reference counting would mean the backend emitting retain and release on
 *   every assignment, argument, return and scope exit, and every library
 *   learning the protocol — and records and dictionaries are references, so
 *   cycles would leak anyway.
 *
 *   A per-call arena would need to know which values escape the call, and Kiln
 *   has no type that says so.  The fallback — copying on return — would break
 *   the promise that two names for one record are two names for the same
 *   fields.  That is a language change, not a memory-management change.
 *
 *   Tracing preserves aliasing exactly, needs nothing from the front end but a
 *   list of the module's variables, and the inventory it traces is the block
 *   list that already existed.
 *
 * The one thing it is not is concurrent: the runtime has no thread that
 * touches program data (checked), so there is no world to stop.
 */
#include <setjmp.h>
#include <stdio.h>
#ifndef _WIN32
#include <time.h>
#endif
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "kiln_core.h"
#include "kn_gc.h"

#ifdef _WIN32
#include <windows.h>
#endif

/* --- roots ------------------------------------------------------------- */

/* The bottom of the stack: `main` records it, so the scan knows where to stop.
 * NULL until it does, and that is the switch that keeps a library target from
 * collecting — a shared library has no main, and no promise about which
 * thread's stack it would be scanning. */
static char *g_stack_base = NULL;

/* The program's module-level variables, as an array of the addresses of the
 * pointer-typed ones.  The backend emits the table and hands it over; a
 * program with no module variables hands over nothing. */
static void **g_globals      = NULL;
static int32_t g_global_count = 0;

/* Explicit roots, for a library that must hold a Kiln value across a call that
 * allocates.  None of the first-party libraries need one — they keep their own
 * bookkeeping in plain malloc and copy through kn_malloc only what crosses
 * back to the program — but a third-party library might, and the alternative
 * to giving it a way is giving it a use-after-free. */
static void ***g_extra     = NULL;
static int32_t g_extra_n   = 0;
static int32_t g_extra_cap = 0;

static int g_enabled   = 1;   /* KILN_GC=0 turns it off                     */
static int g_trace     = 0;   /* KILN_GC_TRACE=1 reports every collection    */

/* The collection floor, from kn_gc.h; KILN_GC_MIN_HEAP overrides it. */
static size_t g_floor  = KN_GC_FLOOR;
static int g_checked_env = 0;
static int g_collecting = 0;  /* re-entry guard: the collector allocates    */

/* --- the block index --------------------------------------------------- */

/* A collection asks "is this word an address inside some block?" once per word
 * of the stack and once per word of everything it marks, so that question has
 * to be cheap.  The first version of this sorted the blocks by address and
 * binary-searched; with a few hundred thousand blocks the sort alone was 40 ms
 * of a 50 ms pause, which is three dropped frames.
 *
 * What is here instead is a hash table from 512-byte granule to the blocks
 * that cover it — built in one linear pass, no comparisons.  A block is
 * entered under every granule it spans, so a pointer into the middle of a
 * large array finds it as directly as a pointer to its first byte.  That is
 * bounded work: the number of entries is the number of blocks plus the total
 * bytes divided by 512, not the number of blocks times their size.
 *
 * The chains hold blocks, not granules: a chain walk compares the candidate
 * against each block's real range, so a hash collision costs one comparison
 * rather than a wrong answer.
 */
#define KN_GC_GRANULE_SHIFT 9                          /* 512 bytes */
#define KN_GC_GRAN(p) ((uintptr_t)(p) >> KN_GC_GRANULE_SHIFT)

static Block  **g_ent      = NULL;   /* entry -> the block it belongs to   */
static int32_t *g_ent_next = NULL;   /* entry -> next entry in its bucket  */
static size_t   g_ent_cap  = 0;
static int32_t *g_bucket   = NULL;   /* granule hash -> first entry, or -1 */
static size_t   g_bucket_n = 0;      /* always a power of two              */

/* The lowest and highest address any block occupies.  Almost every word the
 * scan looks at is not a pointer at all — the characters of a string, a
 * counter, a float — and two comparisons throw those out before the hash is
 * computed.  On a heap of text this is most of the work avoided. */
static const char *g_lo = NULL;
static const char *g_hi = NULL;

static size_t hash_gran(uintptr_t g) {
    /* Knuth's multiplicative hash. Granules from one malloc arena are nearly
     * consecutive, and consecutive keys must not land in one bucket. */
    return (size_t)((g * (uintptr_t)2654435761u) & (uintptr_t)(g_bucket_n - 1));
}

/* Entries grow on demand rather than from a pre-computed bound.  A bound tight
 * enough to be cheap is not a bound, and a real one — two per block plus one
 * per granule of payload — reserves roughly twice what a heap of small values
 * actually uses.  Chains hold entry indices, so growing the arrays leaves
 * every chain valid; only the bucket count is fixed for the pass, and that is
 * a question of chain length, never of correctness. */
static int grow_entries(size_t nent) {
    if (nent <= g_ent_cap) return 1;
    size_t want = g_ent_cap ? g_ent_cap : 1024;
    while (want < nent) want *= 2;
    Block **e = (Block **)realloc(g_ent, want * sizeof *e);
    if (!e) return 0;
    g_ent = e;
    int32_t *n = (int32_t *)realloc(g_ent_next, want * sizeof *n);
    if (!n) return 0;
    g_ent_next = n;
    g_ent_cap  = want;
    return 1;
}

static int grow_buckets(size_t nbucket) {
    if (nbucket <= g_bucket_n) return 1;
    size_t want = g_bucket_n ? g_bucket_n : 1024;
    while (want < nbucket) want *= 2;
    int32_t *b = (int32_t *)realloc(g_bucket, want * sizeof *b);
    if (!b) return 0;
    g_bucket   = b;
    g_bucket_n = want;
    return 1;
}

static int build_index(void) {
    /* One pass over the block list, not two.  The list is hundreds of
     * thousands of nodes scattered across the heap, so walking it is the most
     * expensive thing a collection does, and walking it once to count and
     * again to fill cost as much as the indexing.  The allocator keeps the
     * count instead. */
    size_t nblock = kn_gc_count;
    if (!nblock) {
        /* The buckets still have to be emptied, or a lookup would follow last
         * collection's chains into memory that is now free. */
        for (size_t i = 0; i < g_bucket_n; i++) g_bucket[i] = -1;
        g_lo = NULL;
        g_hi = NULL;
        return 1;
    }
    if (!grow_buckets(nblock * 2) || !grow_entries(nblock)) return 0;
    for (size_t i = 0; i < g_bucket_n; i++) g_bucket[i] = -1;
    g_lo = NULL;
    g_hi = NULL;

    size_t e = 0;
    for (Block *b = kn_gc_blocks; b; b = b->next) {
        const char *beg = (const char *)b;
        const char *end = (const char *)KN_PAYLOAD(b) + KN_BLOCK_SIZE(b);
        if (!g_lo || beg < g_lo) g_lo = beg;
        if (!g_hi || end > g_hi) g_hi = end;
        uintptr_t g0 = KN_GC_GRAN(b);
        uintptr_t g1 = KN_GC_GRAN(end);
        for (uintptr_t g = g0; g <= g1; g++) {
            /* Running out of room here would leave a block out of the index,
             * and a block out of the index looks unreachable — so this fails
             * the collection rather than under-indexing it. */
            if (e == g_ent_cap && !grow_entries(e + 1)) return 0;
            size_t h      = hash_gran(g);
            g_ent[e]      = b;
            g_ent_next[e] = g_bucket[h];
            g_bucket[h]   = (int32_t)e;
            e++;
        }
    }
    return 1;
}

/* The block containing `p`, or NULL.  Interior pointers count — at -O2 the
 * compiler is entitled to keep only `&record.field` while the record's base
 * is dead, and a scan that demanded an exact payload address would free the
 * record out from under it.  One past the end counts too, for the same
 * reason: a loop that has walked off the end of an array still owns it. */
static Block *block_containing(const void *p) {
    const char *c = (const char *)p;
    if (c < g_lo || c > g_hi) return NULL;
    if (!g_bucket_n) return NULL;
    for (int32_t e = g_bucket[hash_gran(KN_GC_GRAN(c))]; e >= 0; e = g_ent_next[e]) {
        Block      *b   = g_ent[e];
        const char *beg = (const char *)b;
        const char *end = (const char *)KN_PAYLOAD(b) + KN_BLOCK_SIZE(b);
        if (c >= beg && c <= end) return b;
    }
    return NULL;
}

/* The mark worklist.  An explicit stack, not recursion: a linked structure a
 * million records deep would otherwise overflow the C stack inside the very
 * routine that is trying to reclaim memory. */
static Block **g_work     = NULL;
static size_t  g_work_n   = 0;
static size_t  g_work_cap = 0;

static int work_push(Block *b) {
    if (g_work_n == g_work_cap) {
        size_t want = g_work_cap ? g_work_cap * 2 : 256;
        Block **t = (Block **)realloc(g_work, want * sizeof *t);
        if (!t) return 0;
        g_work     = t;
        g_work_cap = want;
    }
    g_work[g_work_n++] = b;
    return 1;
}

/* Mark one candidate word.  Marking is idempotent, so a stack full of copies
 * of the same pointer costs one push. */
static void mark_one(const void *p) {
    Block *b = block_containing(p);
    if (!b || KN_BLOCK_MARKED(b)) return;
    b->info |= KN_GC_MARK;
    if (!work_push(b)) {
        /* Out of memory building the worklist.  Marking is now incomplete, so
         * the only safe thing is to keep everything: the sweep is skipped by
         * the caller when this flag is set. */
        g_work_n = (size_t)-1;
    }
}

/* Every aligned word in [lo, hi) as a candidate pointer. */
static void scan_range(const char *lo, const char *hi) {
    if (!lo || !hi || hi <= lo) return;
    /* Align up: an unaligned start would read words that straddle two values
     * and, on a strict-alignment target, fault. */
    uintptr_t a = ((uintptr_t)lo + sizeof(void *) - 1) & ~(uintptr_t)(sizeof(void *) - 1);
    for (const char *q = (const char *)a; q + sizeof(void *) <= hi; q += sizeof(void *)) {
        void *cand;
        memcpy(&cand, q, sizeof cand);
        mark_one(cand);
    }
}

static void drain_worklist(void) {
    while (g_work_n && g_work_n != (size_t)-1) {
        Block *b = g_work[--g_work_n];
        char  *p = (char *)KN_PAYLOAD(b);
        scan_range(p, p + KN_BLOCK_SIZE(b));
    }
}

/* --- the public switches ----------------------------------------------- */

void kn_gc_set_stack_base(void *base) { g_stack_base = (char *)base; }

void kn_gc_set_roots(void **globals, int32_t count) {
    g_globals      = globals;
    g_global_count = count < 0 ? 0 : count;
}

int32_t kn_gc_root(void **slot) {
    if (!slot) return 0;
    if (g_extra_n == g_extra_cap) {
        int32_t want = g_extra_cap ? g_extra_cap * 2 : 16;
        void ***t = (void ***)realloc(g_extra, (size_t)want * sizeof *t);
        if (!t) return 0;
        g_extra     = t;
        g_extra_cap = want;
    }
    g_extra[g_extra_n++] = slot;
    return 1;
}

void kn_gc_unroot(void **slot) {
    for (int32_t i = 0; i < g_extra_n; i++) {
        if (g_extra[i] == slot) {
            g_extra[i] = g_extra[--g_extra_n];
            return;
        }
    }
}

int64_t kn_gc_live_bytes(void) { return (int64_t)kn_gc_live; }

/* --- the collection ---------------------------------------------------- */

/* Split out and never inlined so that `regs` and `here` really are in this
 * frame, below everything the program was holding when it called us. */
#if defined(__GNUC__) || defined(__clang__)
#define KN_NOINLINE __attribute__((noinline))
#else
#define KN_NOINLINE
#endif
static KN_NOINLINE size_t collect_now(void);

int64_t kn_gc_collect(void) {
    if (!g_stack_base || g_collecting) return 0;
    return (int64_t)collect_now();
}

size_t kn_gc_maybe_collect(void) {
    if (!g_checked_env) {
        const char *e = getenv("KILN_GC");
        if (e && e[0] == '0' && e[1] == '\0') g_enabled = 0;
        /* What a collection cost, and when.  A pause is the one thing a game
         * loop cannot measure for itself from inside the language, and the one
         * thing it most needs to know. */
        e = getenv("KILN_GC_TRACE");
        if (e && e[0] && !(e[0] == '0' && e[1] == '\0')) g_trace = 1;
        e = getenv("KILN_GC_MIN_HEAP");
        if (e && e[0]) {
            long mb = strtol(e, NULL, 10);
            if (mb > 0 && mb < 65536) g_floor = (size_t)mb * 1024u * 1024u;
        }
        if (kn_gc_threshold > g_floor) kn_gc_threshold = g_floor;
        g_checked_env = 1;
    }
    if (!g_enabled || !g_stack_base || g_collecting) return 0;
    if (kn_gc_since < kn_gc_threshold) return 0;
    return collect_now();
}

static double now_ms(void) {
#ifdef _WIN32
    /* clock_gettime is not in the MSVC runtime, and a trace path that will not
     * compile is a bundle that will not build. */
    LARGE_INTEGER f, c;
    QueryPerformanceFrequency(&f);
    QueryPerformanceCounter(&c);
    return f.QuadPart ? (double)c.QuadPart * 1000.0 / (double)f.QuadPart : 0.0;
#else
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (double)t.tv_sec * 1000.0 + (double)t.tv_nsec / 1e6;
#endif
}

static KN_NOINLINE size_t collect_now(void) {
    g_collecting = 1;
    double t0 = g_trace ? now_ms() : 0.0;

    /* Spill the callee-saved registers somewhere scannable.  A live text may
     * exist only in a register at this point — this is the standard trick, and
     * it is why the collector can run from inside E_MAlloc at all. */
    jmp_buf regs;
    memset(&regs, 0, sizeof regs);
    (void)setjmp(regs);

    char here;   /* the top of the stack, as of this frame */

    if (!build_index()) { g_collecting = 0; return 0; }
    double t1 = g_trace ? now_ms() : 0.0;
    g_work_n = 0;

    /* The stack.  It grows down on every target Kiln builds for, but the
     * comparison costs nothing and a wrong-way scan would be a silent
     * catastrophe. */
    if (&here < g_stack_base) scan_range(&here, g_stack_base);
    else                      scan_range(g_stack_base, &here + 1);

    scan_range((const char *)&regs, (const char *)&regs + sizeof regs);

    for (int32_t i = 0; i < g_global_count; i++) mark_one(g_globals[i] ? *(void **)g_globals[i] : NULL);
    for (int32_t i = 0; i < g_extra_n; i++)      mark_one(g_extra[i] ? *g_extra[i] : NULL);

    drain_worklist();
    double t2 = g_trace ? now_ms() : 0.0;

    /* Marking ran out of memory: everything stays.  A collection that cannot
     * finish must not free anything, and the marks have to come off. */
    if (g_work_n == (size_t)-1) {
        for (Block *b = kn_gc_blocks; b; b = b->next) b->info &= ~KN_GC_MARK;
        g_work_n     = 0;
        kn_gc_since  = 0;
        g_collecting = 0;
        return 0;
    }

    /* Sweep.  Unmarked blocks are unreachable; marked ones lose their mark and
     * stay.  Unlinking is O(1) because the list is doubly linked. */
    size_t freed = 0;
    Block *b     = kn_gc_blocks;
    while (b) {
        Block *next = b->next;
        if (KN_BLOCK_MARKED(b)) {
            b->info &= ~KN_GC_MARK;
        } else {
            size_t n = KN_BLOCK_SIZE(b);
            if (b->prev) b->prev->next = b->next; else kn_gc_blocks = b->next;
            if (b->next) b->next->prev = b->prev;
            kn_gc_live  -= n;
            kn_gc_count -= 1;
            freed       += n;
            free(b);
        }
        b = next;
    }

    /* Collect again once the program has allocated as much as it is presently
     * holding, with a floor so a small program never pays for a collection it
     * does not need.  Doubling keeps the amortised cost of collection a fixed
     * fraction of the cost of allocation, whatever the live set turns out to
     * be. */
    kn_gc_since     = 0;
    kn_gc_threshold = kn_gc_live * 2;
    if (kn_gc_threshold < g_floor) kn_gc_threshold = g_floor;

    if (g_trace) {
        fprintf(stderr,
                "kiln gc: %.2f ms (index %.2f, mark %.2f, sweep %.2f), freed %zu KB, holding %zu KB\n",
                now_ms() - t0, t1 - t0, t2 - t1, now_ms() - t2,
                freed / 1024, kn_gc_live / 1024);
    }

    g_collecting = 0;
    return freed;
}

/* --- the two core commands --------------------------------------------- */

/* What the program is holding, in bytes: the payload of every block still on
 * the list, headers excluded.  Useful for the same reason a fuel gauge is:
 * not to act on every reading, but to notice a number that only goes up. */
void kn_memory_in_use(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; (void)argv;
    kn_ret_int64(ret, kn_gc_live_bytes());
}

/* Collect now, and answer the bytes reclaimed.  Rarely needed — the runtime
 * collects on its own as a program allocates — but a program that knows when
 * it is idle knows something the runtime does not: the end of a frame, the end
 * of a request, the moment a level finished loading. */
void kn_collect_garbage(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; (void)argv;
    kn_ret_int64(ret, kn_gc_collect());
}
