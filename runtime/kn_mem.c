/* The allocator behind kn_notify's memory messages.
 *
 * Every allocation the program owns carries the header in kn_gc.h and is
 * threaded onto one list.  Two things read that list: the collector, which
 * frees what the program can no longer reach (kn_gc.c), and kn_free_all at
 * exit, which frees the rest.  Before the collector existed this file was the
 * whole memory manager and the list only ever grew.
 *
 * The list is doubly linked so that an explicit kn_mfree is O(1) rather than a
 * walk — with a large live set the walk was quadratic, and the collector makes
 * large live sets ordinary.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "kiln_core.h"
#include "kn_gc.h"

Block *kn_gc_blocks    = NULL;
size_t kn_gc_live      = 0;
size_t kn_gc_count     = 0;
size_t kn_gc_since     = 0;
size_t kn_gc_threshold = KN_GC_FLOOR;

static void link_block(Block *b, size_t size) {
    b->info = KN_GC_MAGIC | ((uint64_t)size & KN_GC_SIZE_MASK);
    b->prev = NULL;
    b->next = kn_gc_blocks;
    if (kn_gc_blocks) kn_gc_blocks->prev = b;
    kn_gc_blocks = b;
    kn_gc_live  += size;
    kn_gc_count += 1;
    kn_gc_since += size;
}

static void unlink_block(Block *b) {
    if (b->prev) b->prev->next = b->next; else kn_gc_blocks = b->next;
    if (b->next) b->next->prev = b->prev;
    kn_gc_live -= KN_BLOCK_SIZE(b);
    kn_gc_count -= 1;
}

/* The header of a pointer this allocator handed out, or NULL for one it did
 * not.  A library that frees a pointer from somewhere else gets a no-op, which
 * is what it got before this file kept back-links and had to walk to find out. */
static Block *owned_block(void *p) {
    if (!p) return NULL;
    Block *b = KN_BLOCK_OF(p);
    return KN_BLOCK_OURS(b) ? b : NULL;
}

void *E_MAlloc(long size) {
    if (size < 0) size = 0;

    /* Before the allocation, not after: collecting here means the value being
     * built does not exist yet and cannot be collected by mistake, and the
     * caller's own live values are all still on the stack where the scan will
     * find them. */
    kn_gc_maybe_collect();

    Block *b = (Block *)malloc(sizeof(Block) + (size_t)size);
    if (!b) {
        /* One more try, having reclaimed everything reclaimable: an allocation
         * that fails when the heap is full of garbage should not end the
         * program. */
        if (kn_gc_collect() > 0) b = (Block *)malloc(sizeof(Block) + (size_t)size);
        if (!b) { kn_runtime_error("out of memory"); return NULL; }
    }
    link_block(b, (size_t)size);
    return KN_PAYLOAD(b);
}

void E_MFree(void *p) {
    Block *b = owned_block(p);
    if (!b) return;
    unlink_block(b);
    free(b);
}

void *E_MRealloc(void *p, long size) {
    if (size < 0) size = 0;
    if (!p) return E_MAlloc(size);

    Block *b = owned_block(p);
    if (!b) return NULL;   /* not ours; growing it would corrupt two heaps */

    /* No collection here.  realloc may move the block, and for the instant
     * between the old address dying and the new one being linked there is a
     * live value at neither — a scan in that window would be scanning a lie. */
    unlink_block(b);
    Block *nb = (Block *)realloc(b, sizeof(Block) + (size_t)size);
    if (!nb) {
        link_block(b, KN_BLOCK_SIZE(b));   /* put it back, unchanged */
        kn_runtime_error("out of memory");
        return NULL;
    }
    link_block(nb, (size_t)size);
    return KN_PAYLOAD(nb);
}

void *kn_notify(int32_t msg, void *p1, void *p2) {
    switch (msg) {
        case KN_NRS_MALLOC:   return E_MAlloc((long)(size_t)p1);
        case KN_NRS_MFREE:    E_MFree(p1); return NULL;
        case KN_NRS_MREALLOC: return E_MRealloc(p1, (long)(size_t)p2);
        case KN_NRS_FREE_ARY: /* byte-set/array free — the collector's job now */ return NULL;
        case KN_NRS_RUNTIME_ERR:
            fprintf(stderr, "kiln runtime error: %s\n", p1 ? (const char *)p1 : "(unknown)");
            exit(1);
        default: return NULL;
    }
}

/* Exit.  Nothing is reachable after this, so nothing is traced: the list is
 * simply emptied. */
void kn_free_all(void) {
    Block *b = kn_gc_blocks;
    while (b) {
        Block *n = b->next;
        free(b);
        b = n;
    }
    kn_gc_blocks = NULL;
    kn_gc_live   = 0;
    kn_gc_count  = 0;
    kn_gc_since  = 0;
}
