/* libkiln_core — internal runtime header (Phase 2).
 *
 * The core support library, now speaking the real slot ABI (abi/kiln_abi.h):
 * every command is an `Kiln_CommandFn` — `void cmd(Slot* ret, int argc,
 * Slot* argv)`.  Command implementations live in the family `.c` files and are
 * static-linked into each program; the LibInfo *table* that names them lives in
 * `core_libinfo.c`, compiled ONLY into the introspection `.so` (never a shipped
 * program) so `--gc-sections` still strips unused commands.
 */
#ifndef KILN_CORE_H
#define KILN_CORE_H

#include "kiln_abi.h"

/* --- Record and dictionary layouts ------------------------------------
 * Their tags are in abi/kiln_abi.h with every other SDT_* value; what
 * follows is the memory each one actually has.
 *
 * A record: a fixed number of slot-width fields, one runtime-owned allocation.
 *
 *     { int32 count; int32 _pad; int64 fields[count]; }
 *
 * The shape an array has, minus the element tag — a record's fields do not
 * share one type, and each field's type is a compile-time fact, so nothing at
 * run time has to ask.  Fields are reached by POSITION, counting from 1, which
 * is also why no field name reaches a shipped binary. */
typedef struct Kiln_Record {
    int32_t count;
    int32_t _pad;
} Kiln_Record;

/* One key and its value.  The value is the same 64 raw bits a slot carries, so
 * a dictionary of text holds pointers exactly as one of int holds ints. */
typedef struct Kiln_DictEntry {
    char   *key;
    int64_t val;
} Kiln_DictEntry;

/* A dictionary: a header the program holds, and an entry block that grows.
 *
 * TWO allocations, unlike an array's one, and that is the whole design: a
 * dictionary grows in place — `d["new"] = 1` must be visible through every
 * name that holds it — so the thing that MOVES when it grows must not be the
 * thing the program is holding.  The header address never changes; the entry
 * block behind it is what reallocates. */
typedef struct Kiln_Dict {
    int32_t            val_tag;  /* KN_SDT_* of one value                    */
    int32_t            len;      /* entries in use                           */
    int32_t            cap;      /* entries allocated; always >= len         */
    int32_t            _pad;
    Kiln_DictEntry *entries;
} Kiln_Dict;

/* Program entry emitted by the backend. */
extern int ECodeStart(void);

/* Runtime lifecycle. */
void E_Init(void);
void E_DestroyRes(void);

/* Notification channel + allocation.  `kn_notify` is declared
 * in the ABI header; these are the concrete runtime entry points behind it. */
void *E_MAlloc(long size);
void  E_MFree(void *p);
void *E_MRealloc(void *p, long size);

/* The collector (kn_gc.c).  `main` records where the stack begins and the
 * generated module hands over the addresses of its pointer-typed variables;
 * between them those are the roots a trace starts from.  A build with no main
 * — a shared or static library target — sets neither, and the collector stays
 * off there rather than trace a stack it cannot vouch for. */
void kn_gc_set_stack_base(void *base);
void kn_gc_set_roots(void **globals, int32_t count);

/* Every core command (Kiln_CommandFn).  Referenced by core_libinfo.c. */
#define KN_CMD(n) void n(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv)

/* I/O */
KN_CMD(kn_print_int); KN_CMD(kn_print_int64); KN_CMD(kn_print_double); KN_CMD(kn_print_text);
KN_CMD(kn_read_line); KN_CMD(kn_input_ended); KN_CMD(kn_ask);
KN_CMD(kn_assert_failed);
/* errors */
KN_CMD(kn_last_error_code); KN_CMD(kn_last_error_text);
/* memory */
KN_CMD(kn_memory_in_use); KN_CMD(kn_collect_garbage);
/* integer math */
KN_CMD(kn_abs_int); KN_CMD(kn_min_int); KN_CMD(kn_max_int); KN_CMD(kn_mod_int); KN_CMD(kn_pow_int);
/* float math */
KN_CMD(kn_sqrt); KN_CMD(kn_sin); KN_CMD(kn_cos); KN_CMD(kn_tan); KN_CMD(kn_pow);
KN_CMD(kn_exp); KN_CMD(kn_ln); KN_CMD(kn_log10); KN_CMD(kn_floor); KN_CMD(kn_ceil);
KN_CMD(kn_round); KN_CMD(kn_abs_double); KN_CMD(kn_min_double); KN_CMD(kn_max_double);
/* conversions */
KN_CMD(kn_int_to_double); KN_CMD(kn_double_to_int); KN_CMD(kn_int_to_int64); KN_CMD(kn_int64_to_int);
KN_CMD(kn_int_to_text); KN_CMD(kn_int64_to_text); KN_CMD(kn_double_to_text);
KN_CMD(kn_text_to_int); KN_CMD(kn_text_to_double);
/* text */
KN_CMD(kn_text_eq); KN_CMD(kn_length); KN_CMD(kn_uppercase); KN_CMD(kn_lowercase); KN_CMD(kn_trim); KN_CMD(kn_substr);
KN_CMD(kn_find); KN_CMD(kn_replace); KN_CMD(kn_concat); KN_CMD(kn_repeat); KN_CMD(kn_reverse);
/* datetime */
KN_CMD(kn_now); KN_CMD(kn_year); KN_CMD(kn_format_time);
/* arrays */
KN_CMD(kn_ary_count); KN_CMD(kn_ary_append); KN_CMD(kn_ary_remove); KN_CMD(kn_ary_sort);
KN_CMD(kn_ary_contains); KN_CMD(kn_ary_index_of); KN_CMD(kn_ary_join); KN_CMD(kn_ary_split);
KN_CMD(kn_ary_slice);
/* byte-sets */
KN_CMD(kn_bin_make); KN_CMD(kn_bin_size); KN_CMD(kn_bin_byte); KN_CMD(kn_bin_put);
KN_CMD(kn_bin_from_text); KN_CMD(kn_bin_to_text); KN_CMD(kn_bin_slice);
KN_CMD(kn_bin_from_ptr);  KN_CMD(kn_bin_to_ptr);  KN_CMD(kn_bin_concat);
/* dictionaries */
KN_CMD(kn_dict_count); KN_CMD(kn_dict_has); KN_CMD(kn_dict_lookup);
KN_CMD(kn_dict_store); KN_CMD(kn_dict_erase); KN_CMD(kn_dict_keys);
/* pointers and raw memory (kn_ptr.c) */
KN_CMD(kn_ptr_null); KN_CMD(kn_ptr_is_null); KN_CMD(kn_ptr_offset);
KN_CMD(kn_ptr_from_int); KN_CMD(kn_ptr_to_int);
KN_CMD(kn_ptr_read_int); KN_CMD(kn_ptr_write_int);
KN_CMD(kn_ptr_read_int64); KN_CMD(kn_ptr_write_int64);
KN_CMD(kn_ptr_read_byte); KN_CMD(kn_ptr_write_byte);
KN_CMD(kn_ptr_read_double); KN_CMD(kn_ptr_write_double);
KN_CMD(kn_ptr_read_ptr); KN_CMD(kn_ptr_write_ptr);
KN_CMD(kn_ptr_read_text); KN_CMD(kn_ptr_write_text); KN_CMD(kn_ptr_of_text);
KN_CMD(kn_mem_alloc); KN_CMD(kn_mem_free); KN_CMD(kn_mem_zero); KN_CMD(kn_mem_copy);
/* event loop */
KN_CMD(kn_quit);

/* Aggregate access, NOT commands: indexing is syntax, so the backend calls
 * these directly rather than marshaling an argv array to read one element.
 * They move raw 64-bit values — what a slot's value field already holds. */
void   *kn_ary_new(int32_t tag, int32_t len);
int64_t kn_ary_get(void *a, int32_t i);
void    kn_ary_set(void *a, int32_t i, int64_t v);
void   *kn_bin_new(int32_t len);
int32_t kn_bin_at(void *b, int32_t i);
void    kn_bin_set(void *b, int32_t i, int32_t v);
/* A field is named in the source and reached by position here, so these take
 * the index the compiler worked out rather than the name it read. */
void   *kn_rec_new(int32_t field_count);
int64_t kn_rec_get(void *r, int32_t i);
void    kn_rec_set(void *r, int32_t i, int64_t v);
void   *kn_dict_new(int32_t val_tag);
int64_t kn_dict_at(void *d, const char *key);
void    kn_dict_put(void *d, const char *key, int64_t v);

/* Core's non-visual components (abi/kiln_abi.h).  NOT commands: the backend
 * calls these directly, exactly as it calls the kn_ui_* entry points for a
 * visual one. */
int64_t     kn_core_component_create(const char *type_name);
int32_t     kn_core_component_set(int64_t h, const char *prop, const char *value);
const char *kn_core_component_get(int64_t h, const char *prop);
int32_t     kn_core_component_get_int(int64_t h, const char *prop);
int32_t     kn_core_component_on(int64_t h, const char *event, Kiln_HandlerFn handler);

/* Shared internals, not commands.  The error slot and handle table are declared
 * in the ABI header so libraries reach them the same way the core does; these
 * are the few pieces that stay runtime-private. */
char *kn_empty_text(void);              /* from kn_error.c  */
int32_t kn_handle_kind_of(int32_t h);   /* from kn_handle.c */
/* The foreign-function loader (kn_dll.c).  These are called only from emitted
 * IR — a `dll` call — never by another command, but they are declared here so
 * the runtime has one header. `kn_dll_get` resolves `sym` in `library`, caching
 * the address in `*cache` so it is looked up once; `kn_dll_text` copies a C
 * string a foreign call returned into a runtime-owned text. */
void *kn_dll_get(void **cache, const char *library, const char *sym);
char *kn_dll_text(const char *p);
void kn_set_args(int argc, char **argv);/* from kn_args.c   */
int   kn_arg_total(void);
const char *kn_arg_at(int i);

#endif /* KILN_CORE_H */
