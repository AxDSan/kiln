/* Dictionaries — values found by name rather than by position.
 *
 * The keyed half of the pair arrays are the other half of.  Same ownership as
 * everything else the program holds: allocated through kn_malloc, released by
 * E_DestroyRes, carried in the slot as a pointer.
 *
 * Two allocations rather than an array's one, and that difference is the whole
 * design.  An array never grows — `append` returns a new one — so its single
 * block can be the thing the program holds.  A dictionary DOES grow in place:
 * `d["new"] = 1` has to be visible through every name that holds `d`, and a
 * name is a pointer.  So the block that moves when it grows is not the block
 * the program is holding; the header address is stable for the life of the
 * dictionary, and only the entries behind it are reallocated.
 *
 * Entries are kept in INSERTION ORDER and searched by scanning.  That makes
 * `dict_keys` answer the same order twice — which is what makes iterating one
 * reproducible, and a program's output testable — and it makes `dict_remove` a
 * memmove, exactly as removing from an array is.  The cost is a linear scan
 * per lookup; a hash index over the same entries can be added later without
 * changing a single one of the six commands, because none of them promises
 * anything about how a key is found.
 */
#include <string.h>
#include <stdio.h>
#include "kiln_core.h"

void *kn_dict_new(int32_t val_tag) {
    Kiln_Dict *d = (Kiln_Dict *)kn_malloc((long)sizeof(Kiln_Dict));
    if (!d) return NULL;
    d->val_tag = val_tag;
    d->len = 0;
    d->cap = 0;
    d->_pad = 0;
    d->entries = NULL;
    return d;
}

/* A dictionary that was never created reads as empty, for the reason an array
 * does: a module-level `var d: int{}` is zero until its initializer runs, and
 * asking how many keys nothing has deserves 0, not a diagnostic. */
static int32_t dict_len(const Kiln_Dict *d) { return d ? d->len : 0; }

/* The 1-based position of `key`, or 0 for absent — the same answer `find` and
 * `index_of` give, for the same reason: nothing counts from 0, so 0 is free to
 * mean "not there". */
static int32_t find_key(Kiln_Dict *d, const char *key) {
    if (!d || !key) return 0;
    for (int32_t i = 0; i < d->len; i++) {
        if (strcmp(d->entries[i].key, key) == 0) return i + 1;
    }
    return 0;
}

/* Keys are copied, not borrowed.  A key is routinely a temporary the caller
 * built — `concat("user_", name)` — and a dictionary that held the pointer
 * would be correct exactly until that text was reused. */
static char *dup_key(const char *key) {
    size_t n = strlen(key);
    char *out = (char *)kn_malloc((long)n + 1);
    if (out) memcpy(out, key, n + 1);
    return out;
}

static int grow(Kiln_Dict *d) {
    if (d->len < d->cap) return 1;
    int32_t want = d->cap ? d->cap * 2 : 8;
    Kiln_DictEntry *e = (Kiln_DictEntry *)kn_mrealloc(
        d->entries, (long)want * (long)sizeof(Kiln_DictEntry));
    if (!e) return 0;
    d->entries = e;
    d->cap = want;
    return 1;
}

/* The sentinel a missing key answers with, chosen by what the dictionary holds
 * so that a failed lookup is still a printable value of the right type — the
 * rule kn_ary_get follows for a bad index. */
static int64_t miss_value(Kiln_Dict *d) {
    if (d && d->val_tag == KN_SDT_TEXT) return (int64_t)(intptr_t)kn_empty_text();
    return 0;
}

int64_t kn_dict_at(void *p, const char *key) {
    Kiln_Dict *d = (Kiln_Dict *)p;
    int32_t at = find_key(d, key);
    if (!at) {
        /* There is no KN_ERR_NOT_FOUND to reach for: a key that is not there is
         * the keyed spelling of an index that is not there, so it reports as
         * one.  `dict_has` is the predicate that separates this from a stored
         * 0 — a value and a miss are otherwise the same eight bytes. */
        char msg[128];
        snprintf(msg, sizeof msg, "no key `%.80s` in a dictionary of %d entr%s",
                 key ? key : "", (int)dict_len(d),
                 dict_len(d) == 1 ? "y" : "ies");
        kn_error_set(KN_ERR_OUT_OF_RANGE, msg);
        return miss_value(d);
    }
    kn_error_clear();
    return d->entries[at - 1].val;
}

void kn_dict_put(void *p, const char *key, int64_t v) {
    Kiln_Dict *d = (Kiln_Dict *)p;
    if (!d || !key) {
        kn_error_set(KN_ERR_INVALID_ARG, "a dictionary needs a key to store under");
        return;
    }
    int32_t at = find_key(d, key);
    if (at) {
        d->entries[at - 1].val = v;
        kn_error_clear();
        return;
    }
    if (!grow(d)) {
        kn_error_set(KN_ERR_TABLE_FULL, "cannot grow the dictionary");
        return;
    }
    char *owned = dup_key(key);
    if (!owned) {
        kn_error_set(KN_ERR_TABLE_FULL, "cannot store the key");
        return;
    }
    d->entries[d->len].key = owned;
    d->entries[d->len].val = v;
    d->len++;
    kn_error_clear();
}

/* --- commands ---------------------------------------------------------- */

static Kiln_Dict *arg_dict(Kiln_Slot *argv, int i) {
    return (Kiln_Dict *)argv[i].v.ptr;
}

void kn_dict_count(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) {
    (void)c;
    kn_ret_int(r, dict_len(arg_dict(argv, 0)));
}

/* A predicate that cannot fail, so it never touches the error slot — which is
 * what lets it be asked immediately after a lookup to find out whether the
 * sentinel meant "absent" or "that is what is stored". */
void kn_dict_has(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) {
    (void)c;
    kn_ret_bool(r, find_key(arg_dict(argv, 0), kn_arg_text(argv, 1)) > 0);
}

void kn_dict_lookup(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) {
    (void)c;
    Kiln_Dict *d = arg_dict(argv, 0);
    r->tag = d ? d->val_tag : KN_SDT_INT;
    r->v.i64 = kn_dict_at(d, kn_arg_text(argv, 1));
}

void kn_dict_store(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) {
    (void)c; (void)r;
    kn_dict_put(arg_dict(argv, 0), kn_arg_text(argv, 1), argv[2].v.i64);
}

/* Removing shortens, and shortening never moves the entries block — so the
 * later keys slide down and the insertion order of the rest survives.
 *
 * `false` here is a genuine "no such key", reported with the error slot CLEAR:
 * asking to remove something that was already gone is an answer, not a
 * failure. */
void kn_dict_erase(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) {
    (void)c;
    Kiln_Dict *d = arg_dict(argv, 0);
    int32_t at = find_key(d, kn_arg_text(argv, 1));
    if (!at) {
        kn_error_clear();
        kn_ret_bool(r, 0);
        return;
    }
    kn_mfree(d->entries[at - 1].key);
    memmove(d->entries + at - 1, d->entries + at,
            (size_t)(d->len - at) * sizeof(Kiln_DictEntry));
    d->len--;
    kn_error_clear();
    kn_ret_bool(r, 1);
}

/* The keys as a list, in insertion order — which is what makes iterating a
 * dictionary possible at all now that arrays exist: `for i = 1 to count(ks)`
 * and a lookup per key. */
void kn_dict_keys(Kiln_Slot *r, int32_t c, Kiln_Slot *argv) {
    (void)c;
    Kiln_Dict *d = arg_dict(argv, 0);
    int32_t n = dict_len(d);
    Kiln_Array *out = (Kiln_Array *)kn_ary_new(KN_SDT_TEXT, n);
    r->tag = KN_SDT_ARRAY_OF(KN_SDT_TEXT);
    r->v.ptr = out;
    if (!out) return;
    /* Copies, not the dictionary's own keys: the list outlives whatever the
     * program does to the dictionary next, and `dict_remove` frees a key. */
    for (int32_t i = 0; i < n; i++) {
        char *copy = dup_key(d->entries[i].key);
        if (!copy) break;
        kn_ary_set(out, i + 1, (int64_t)(intptr_t)copy);
    }
    kn_error_clear();
}
