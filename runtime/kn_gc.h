/* The shape of a runtime allocation, and the collector that reclaims it.
 *
 * Every block the program owns — a text, an array, a byte-set, a record, a
 * dictionary — is one `malloc` with this header in front of it, threaded onto
 * a list the runtime keeps.  That list has always existed; until now nothing
 * walked it except the free-everything-at-exit sweep, so a loop that built a
 * string per iteration kept every string it had ever built.  The collector
 * below walks it, and frees what the program can no longer reach.
 *
 * Internal to the runtime: a library never sees a Block.  What a library sees
 * is kn_malloc, and — if it holds a Kiln value across a call that might
 * allocate — kn_gc_root, declared in the ABI header.
 */
#ifndef KILN_GC_H
#define KILN_GC_H

#include <stddef.h>
#include <stdint.h>

/* The header, 24 bytes on a 64-bit target.  `info` packs three things into one
 * word so it stays that size:
 *
 *   bit 63       the mark, set during a collection and cleared by the sweep
 *   bits 62..48  a magic number, so kn_mfree can tell a pointer it allocated
 *                from one it did not — a foreign pointer is ignored, which is
 *                what the previous list-walking implementation did too
 *   bits 47..0   the payload size, which the collector needs to know how far
 *                to scan and which addresses land inside this block
 *
 * 48 bits of size caps a single allocation at 256 TB.  A program that wants
 * more is not going to be stopped by a bigger header. */
typedef struct Block {
    struct Block *next;
    struct Block *prev;
    uint64_t      info;
} Block;

#define KN_GC_MARK   ((uint64_t)1 << 63)
#define KN_GC_MAGIC  ((uint64_t)0x5A1B << 48)   /* arbitrary, and unlikely */
#define KN_GC_MAGIC_MASK ((uint64_t)0x7FFF << 48)
#define KN_GC_SIZE_MASK  (((uint64_t)1 << 48) - 1)

#define KN_BLOCK_SIZE(b)   ((size_t)((b)->info & KN_GC_SIZE_MASK))
#define KN_BLOCK_MARKED(b) (((b)->info & KN_GC_MARK) != 0)
#define KN_BLOCK_OURS(b)   (((b)->info & KN_GC_MAGIC_MASK) == KN_GC_MAGIC)

/* payload <-> header */
#define KN_BLOCK_OF(p)   ((Block *)(p) - 1)
#define KN_PAYLOAD(b)    ((void *)((Block *)(b) + 1))

/* How much garbage may pile up before a collection, when the program is
 * holding almost nothing.  Above that floor the trigger is proportional to the
 * live set, so the cost of collecting stays a fixed share of the cost of
 * allocating whatever the program turns out to hold.
 *
 * The floor decides the pause for a program with a small live set: a
 * collection costs roughly what the blocks made since the last one cost, so
 * halving the floor halves the pause and doubles the number of pauses for the
 * same total work.  Measured on a loop building two million short texts, 1 MB
 * gives a 2 ms pause and an 8 MB resident set where 8 MB gives 19 ms and 46 MB
 * — smaller is better on every axis until the floor is small enough that a
 * script which allocates almost nothing starts collecting for no reason.
 * KILN_GC_MIN_HEAP, in megabytes, moves it. */
#define KN_GC_FLOOR ((size_t)1 * 1024 * 1024)

/* Owned by kn_mem.c, read by kn_gc.c. */
extern Block *kn_gc_blocks;
extern size_t kn_gc_live;         /* payload bytes on the list right now */
extern size_t kn_gc_count;        /* how many blocks that is                */
extern size_t kn_gc_since;        /* payload bytes allocated since the last collection */
extern size_t kn_gc_threshold;    /* collect when kn_gc_since passes this   */

/* Called by E_MAlloc before it allocates, when the threshold has been passed.
 * Answers the number of payload bytes reclaimed. */
size_t kn_gc_maybe_collect(void);

#endif /* KILN_GC_H */
