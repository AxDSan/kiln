/* Core library metadata (LibInfo / GetNewInf analog) — Phase 2.
 *
 * DESIGN-TIME METADATA ONLY.  This translation unit is compiled into the
 * introspection shared object (`libkiln_core.so`) that the compiler dlopens
 * to read command signatures — it is NEVER linked into a shipped program.  The
 * table holds a pointer to every command name/symbol; if it entered a program's
 * link line it would anchor all ~40 commands and defeat `--gc-sections`
 *.  Command *implementations* ship; this catalog does not.
 */
#include "kiln_core.h"

/* Distinct parameter-tag arrays, shared across commands. */
static const int32_t P_I[]     = { KN_SDT_INT };
static const int32_t P_II[]    = { KN_SDT_INT, KN_SDT_INT };
static const int32_t P_I64[]   = { KN_SDT_INT64 };
static const int32_t P_D[]     = { KN_SDT_DOUBLE };
static const int32_t P_DD[]    = { KN_SDT_DOUBLE, KN_SDT_DOUBLE };
static const int32_t P_T[]     = { KN_SDT_TEXT };
static const int32_t P_TT[]    = { KN_SDT_TEXT, KN_SDT_TEXT };
static const int32_t P_TI[]    = { KN_SDT_TEXT, KN_SDT_INT };
static const int32_t P_TII[]   = { KN_SDT_TEXT, KN_SDT_INT, KN_SDT_INT };
static const int32_t P_TTT[]   = { KN_SDT_TEXT, KN_SDT_TEXT, KN_SDT_TEXT };
static const int32_t P_I64T[]  = { KN_SDT_INT64, KN_SDT_TEXT };
/* Aggregates. ANY_ARRAY/ANY_ELEM keep one `append` instead of one per element
 * type; the array carries its element tag, and the compiler checks the pair. */
static const int32_t P_A[]     = { KN_SDT_ANY_ARRAY };
static const int32_t P_AE[]    = { KN_SDT_ANY_ARRAY, KN_SDT_ANY_ELEM };
static const int32_t P_AI[]    = { KN_SDT_ANY_ARRAY, KN_SDT_INT };
static const int32_t P_AT[]    = { KN_SDT_ANY_ARRAY, KN_SDT_TEXT };
static const int32_t P_AII[]   = { KN_SDT_ANY_ARRAY, KN_SDT_INT, KN_SDT_INT };
static const int32_t P_B[]     = { KN_SDT_BIN };
static const int32_t P_BI[]    = { KN_SDT_BIN, KN_SDT_INT };
static const int32_t P_BII[]   = { KN_SDT_BIN, KN_SDT_INT, KN_SDT_INT };
static const int32_t P_BB[]    = { KN_SDT_BIN, KN_SDT_BIN };
static const int32_t P_BP[]    = { KN_SDT_BIN, KN_SDT_PTR };
static const int32_t P_PI[]    = { KN_SDT_PTR, KN_SDT_INT };
/* Dictionaries: ANY_DICT/ANY_ELEM for the reason the arrays use their pair —
 * the dictionary carries its value tag, and the compiler checks it against the
 * value being stored. */
static const int32_t P_K[]     = { KN_SDT_ANY_DICT };
static const int32_t P_KT[]    = { KN_SDT_ANY_DICT, KN_SDT_TEXT };
static const int32_t P_KTE[]   = { KN_SDT_ANY_DICT, KN_SDT_TEXT, KN_SDT_ANY_ELEM };
/* Pointers. Offsets and sizes are INT64 so a buffer may exceed 2 GiB and a
 * 64-bit address round-trips whole. */
static const int32_t P_P[]        = { KN_SDT_PTR };
static const int32_t P_PI64[]     = { KN_SDT_PTR, KN_SDT_INT64 };
static const int32_t P_PI64I[]    = { KN_SDT_PTR, KN_SDT_INT64, KN_SDT_INT };
static const int32_t P_PI64I64[]  = { KN_SDT_PTR, KN_SDT_INT64, KN_SDT_INT64 };
static const int32_t P_PI64D[]    = { KN_SDT_PTR, KN_SDT_INT64, KN_SDT_DOUBLE };
static const int32_t P_PI64P[]    = { KN_SDT_PTR, KN_SDT_INT64, KN_SDT_PTR };
static const int32_t P_PI64T[]    = { KN_SDT_PTR, KN_SDT_INT64, KN_SDT_TEXT };
static const int32_t P_PPI64[]    = { KN_SDT_PTR, KN_SDT_PTR, KN_SDT_INT64 };

#define CMD(name, sym, ret, argc, tags) \
    { name, #sym, ret, argc, tags }

/* The documented form. The trailing two fields are the sentence the language
 * server shows on hover and the sample F1 opens, and `tools/check-docs.sh`
 * compiles the sample — so a wrong example fails the build rather than the
 * reader. See libs/file for the fully documented library. */
#define DCMD(name, sym, ret, argc, tags, doc, example) \
    { name, #sym, ret, argc, tags, doc, example }

static const Kiln_CommandDesc CORE_COMMANDS[] = {
    /* I/O (void) */
    CMD("print_int",    kn_print_int,    KN_SDT_NULL, 1, P_I),
    CMD("print_int64",  kn_print_int64,  KN_SDT_NULL, 1, P_I64),
    CMD("print_double", kn_print_double, KN_SDT_NULL, 1, P_D),
    CMD("print_text",   kn_print_text,   KN_SDT_NULL, 1, P_T),
    /* input — the other half of the pair */
    CMD("read_line",    kn_read_line,    KN_SDT_TEXT, 0, NULL),
    CMD("input_ended",  kn_input_ended,  KN_SDT_BOOL, 0, NULL),
    CMD("ask",          kn_ask,          KN_SDT_TEXT, 1, P_T),
    /* what a failed `assert` runs: print the message and stop, failing */
    CMD("assert_failed", kn_assert_failed, KN_SDT_NULL, 1, P_T),
    /* errors — zero arity is what makes an out-parameter expressible */
    CMD("last_error_code", kn_last_error_code, KN_SDT_INT,  0, NULL),
    CMD("last_error_text", kn_last_error_text, KN_SDT_TEXT, 0, NULL),
    /* memory — the runtime reclaims on its own; these are for looking and
       for saying "now is a good moment" */
    DCMD("memory_in_use", kn_memory_in_use, KN_SDT_INT64, 0, NULL,
         "How many bytes of program data the runtime is currently holding.",
         "let before: int64 = memory_in_use()\n"
         "for i = 1 to 10000\n"
         "  let s: text = \"row {i}\"\n"
         "end\n"
         "call print_text(\"held {memory_in_use() - before} more bytes\")"),
    DCMD("collect_garbage", kn_collect_garbage, KN_SDT_INT64, 0, NULL,
         "Reclaim unreachable memory now, and answer how many bytes came back.",
         "for i = 1 to 100000\n"
         "  let s: text = \"scratch {i}\"\n"
         "end\n"
         "call print_text(\"reclaimed {collect_garbage()} bytes\")"),
    /* integer math */
    CMD("abs_int", kn_abs_int, KN_SDT_INT, 1, P_I),
    CMD("min_int", kn_min_int, KN_SDT_INT, 2, P_II),
    CMD("max_int", kn_max_int, KN_SDT_INT, 2, P_II),
    CMD("mod_int", kn_mod_int, KN_SDT_INT, 2, P_II),
    CMD("pow_int", kn_pow_int, KN_SDT_INT, 2, P_II),
    /* float math */
    CMD("sqrt",  kn_sqrt,  KN_SDT_DOUBLE, 1, P_D),
    CMD("sin",   kn_sin,   KN_SDT_DOUBLE, 1, P_D),
    CMD("cos",   kn_cos,   KN_SDT_DOUBLE, 1, P_D),
    CMD("tan",   kn_tan,   KN_SDT_DOUBLE, 1, P_D),
    CMD("pow",   kn_pow,   KN_SDT_DOUBLE, 2, P_DD),
    CMD("exp",   kn_exp,   KN_SDT_DOUBLE, 1, P_D),
    CMD("ln",    kn_ln,    KN_SDT_DOUBLE, 1, P_D),
    CMD("log10", kn_log10, KN_SDT_DOUBLE, 1, P_D),
    CMD("floor", kn_floor, KN_SDT_DOUBLE, 1, P_D),
    CMD("ceil",  kn_ceil,  KN_SDT_DOUBLE, 1, P_D),
    CMD("round", kn_round, KN_SDT_DOUBLE, 1, P_D),
    CMD("abs_double", kn_abs_double, KN_SDT_DOUBLE, 1, P_D),
    CMD("min_double", kn_min_double, KN_SDT_DOUBLE, 2, P_DD),
    CMD("max_double", kn_max_double, KN_SDT_DOUBLE, 2, P_DD),
    /* conversions */
    CMD("int_to_double", kn_int_to_double, KN_SDT_DOUBLE, 1, P_I),
    CMD("double_to_int", kn_double_to_int, KN_SDT_INT,    1, P_D),
    CMD("int_to_int64",  kn_int_to_int64,  KN_SDT_INT64,  1, P_I),
    CMD("int64_to_int",  kn_int64_to_int,  KN_SDT_INT,    1, P_I64),
    CMD("int_to_text",   kn_int_to_text,   KN_SDT_TEXT,   1, P_I),
    CMD("int64_to_text", kn_int64_to_text, KN_SDT_TEXT,   1, P_I64),
    CMD("double_to_text",kn_double_to_text,KN_SDT_TEXT,   1, P_D),
    CMD("text_to_int",   kn_text_to_int,   KN_SDT_INT,    1, P_T),
    CMD("text_to_double",kn_text_to_double,KN_SDT_DOUBLE, 1, P_T),
    /* text */
    CMD("text_eq",   kn_text_eq,   KN_SDT_BOOL, 2, P_TT),
    CMD("length",    kn_length,    KN_SDT_INT,  1, P_T),
    CMD("uppercase", kn_uppercase, KN_SDT_TEXT, 1, P_T),
    CMD("lowercase", kn_lowercase, KN_SDT_TEXT, 1, P_T),
    CMD("trim",      kn_trim,      KN_SDT_TEXT, 1, P_T),
    CMD("substr",    kn_substr,    KN_SDT_TEXT, 3, P_TII),
    CMD("find",      kn_find,      KN_SDT_INT,  2, P_TT),
    CMD("replace",   kn_replace,   KN_SDT_TEXT, 3, P_TTT),
    CMD("concat",    kn_concat,    KN_SDT_TEXT, 2, P_TT),
    CMD("repeat",    kn_repeat,    KN_SDT_TEXT, 2, P_TI),
    CMD("reverse",   kn_reverse,   KN_SDT_TEXT, 1, P_T),
    /* datetime */
    CMD("now",         kn_now,         KN_SDT_INT64, 0, NULL),
    CMD("year",        kn_year,        KN_SDT_INT,   1, P_I64),
    CMD("format_time", kn_format_time, KN_SDT_TEXT,  2, P_I64T),
    /* arrays — the operations that cannot be syntax */
    CMD("count",    kn_ary_count,    KN_SDT_INT,       1, P_A),
    CMD("append",   kn_ary_append,   KN_SDT_ANY_ARRAY, 2, P_AE),
    CMD("remove",   kn_ary_remove,   KN_SDT_NULL,      2, P_AI),
    CMD("sort",     kn_ary_sort,     KN_SDT_NULL,      1, P_A),
    CMD("contains", kn_ary_contains, KN_SDT_BOOL,      2, P_AE),
    CMD("index_of", kn_ary_index_of, KN_SDT_INT,       2, P_AE),
    CMD("join",     kn_ary_join,     KN_SDT_TEXT,      2, P_AT),
    CMD("split",    kn_ary_split,    KN_SDT_ARRAY_OF(KN_SDT_TEXT), 2, P_TT),
    /* `slice(xs, start, count)` is what `xs[a..b]` becomes; it is a command in
     * its own right so the shorthand adds no semantics the language did not
     * already have, and so a computed run can be taken without one. */
    CMD("slice",    kn_ary_slice,    KN_SDT_ANY_ARRAY, 3, P_AII),
    /* byte-sets */
    CMD("bytes_new",       kn_bin_make,      KN_SDT_BIN,  1, P_I),
    CMD("bytes_count",     kn_bin_size,      KN_SDT_INT,  1, P_B),
    CMD("bytes_at",        kn_bin_byte,      KN_SDT_INT,  2, P_BI),
    CMD("bytes_set",       kn_bin_put,       KN_SDT_NULL, 3, P_BII),
    CMD("bytes_from_text", kn_bin_from_text, KN_SDT_BIN,  1, P_T),
    CMD("text_from_bytes", kn_bin_to_text,   KN_SDT_TEXT, 1, P_B),
    CMD("bytes_slice",     kn_bin_slice,     KN_SDT_BIN,  3, P_BII),
    DCMD("bytes_concat",   kn_bin_concat,    KN_SDT_BIN,  2, P_BB,
         "Two byte-sets end to end, as one",
         "let head: bytes = bytes_new(2)\n"
         "let body: bytes = bytes_from_text(\"hi\")\n"
         "call print_int(bytes_count(bytes_concat(head, body)))"),
    DCMD("bytes_from_ptr", kn_bin_from_ptr,  KN_SDT_BIN,  2, P_PI,
         "Copy a run of bytes out of an address, into a byte-set",
         "module frame\n"
         "target console\n"
         "\n"
         "record point is c\n"
         "  x: int\n"
         "  y: int\n"
         "end\n"
         "\n"
         "sub main\n"
         "  var p: point\n"
         "  p.x = 7\n"
         "  let raw: bytes = bytes_from_ptr(address of p, 8)\n"
         "  call print_int(bytes_at(raw, 1))\n"
         "end"),
    DCMD("bytes_copy_to_ptr", kn_bin_to_ptr, KN_SDT_INT,  2, P_BP,
         "Copy a byte-set to an address; answers how many bytes that was",
         "module frame\n"
         "target console\n"
         "\n"
         "record point is c\n"
         "  x: int\n"
         "  y: int\n"
         "end\n"
         "\n"
         "sub main\n"
         "  var raw: bytes = bytes_new(8)\n"
         "  call bytes_set(raw, 1, 7)\n"
         "  var p: point\n"
         "  call print_int(bytes_copy_to_ptr(raw, address of p))\n"
         "  call print_int(p.x)\n"
         "end"),
    /* dictionaries — values found by name.  `dict_get` on a key that is not
     * there answers the sentinel for its value type and sets the error slot;
     * `dict_has` is the predicate that tells that apart from a stored 0. */
    CMD("dict_count",  kn_dict_count,  KN_SDT_INT,      1, P_K),
    CMD("dict_has",    kn_dict_has,    KN_SDT_BOOL,     2, P_KT),
    CMD("dict_get",    kn_dict_lookup, KN_SDT_ANY_ELEM, 2, P_KT),
    CMD("dict_set",    kn_dict_store,  KN_SDT_NULL,     3, P_KTE),
    CMD("dict_remove", kn_dict_erase,  KN_SDT_BOOL,     2, P_KT),
    CMD("dict_keys",   kn_dict_keys,   KN_SDT_ARRAY_OF(KN_SDT_TEXT), 1, P_K),
    /* pointers and raw memory — the escape hatch to C. */
    CMD("ptr_null",        kn_ptr_null,        KN_SDT_PTR,   0, NULL),
    CMD("ptr_is_null",     kn_ptr_is_null,     KN_SDT_BOOL,  1, P_P),
    CMD("ptr_offset",      kn_ptr_offset,      KN_SDT_PTR,   2, P_PI64),
    CMD("ptr_from_int",    kn_ptr_from_int,    KN_SDT_PTR,   1, P_I64),
    CMD("ptr_to_int",      kn_ptr_to_int,      KN_SDT_INT64, 1, P_P),
    CMD("ptr_read_int",    kn_ptr_read_int,    KN_SDT_INT,   2, P_PI64),
    CMD("ptr_write_int",   kn_ptr_write_int,   KN_SDT_NULL,  3, P_PI64I),
    CMD("ptr_read_int64",  kn_ptr_read_int64,  KN_SDT_INT64, 2, P_PI64),
    CMD("ptr_write_int64", kn_ptr_write_int64, KN_SDT_NULL,  3, P_PI64I64),
    CMD("ptr_read_byte",   kn_ptr_read_byte,   KN_SDT_INT,   2, P_PI64),
    CMD("ptr_write_byte",  kn_ptr_write_byte,  KN_SDT_NULL,  3, P_PI64I),
    CMD("ptr_read_double", kn_ptr_read_double, KN_SDT_DOUBLE,2, P_PI64),
    CMD("ptr_write_double",kn_ptr_write_double,KN_SDT_NULL,  3, P_PI64D),
    CMD("ptr_read_ptr",    kn_ptr_read_ptr,    KN_SDT_PTR,   2, P_PI64),
    CMD("ptr_write_ptr",   kn_ptr_write_ptr,   KN_SDT_NULL,  3, P_PI64P),
    CMD("ptr_read_text",   kn_ptr_read_text,   KN_SDT_TEXT,  1, P_P),
    CMD("ptr_write_text",  kn_ptr_write_text,  KN_SDT_NULL,  3, P_PI64T),
    CMD("ptr_of_text",     kn_ptr_of_text,     KN_SDT_PTR,   1, P_T),
    CMD("mem_alloc",       kn_mem_alloc,       KN_SDT_PTR,   1, P_I64),
    CMD("mem_free",        kn_mem_free,        KN_SDT_NULL,  1, P_P),
    CMD("mem_zero",        kn_mem_zero,        KN_SDT_NULL,  2, P_PI64),
    CMD("mem_copy",        kn_mem_copy,        KN_SDT_NULL,  3, P_PPI64),
    /* event loop */
    CMD("quit",            kn_quit,          KN_SDT_NULL, 0, NULL),
};

/* --- timer: the core library's one non-visual component ----------------
 * Properties and events are declared exactly as a button's are — the whole
 * point of the `kind` field is that nothing else about the mechanism changes. */
static const Kiln_PropertyDesc TIMER_PROPS[] = {
    { "interval", KN_SDT_INT,  "1000", NULL },
    { "enabled",  KN_SDT_BOOL, "true", NULL },
};
/* The tick count, counting from 1 like every other position in the language.
 * A handler that wants it says so; one that does not is bound unchanged, which
 * is why adding this breaks nothing that already uses a timer. */
static const int32_t TIMER_TICK_PARAMS[] = { KN_SDT_INT };
static const Kiln_EventDesc TIMER_EVENTS[] = {
    { "tick", 1, TIMER_TICK_PARAMS },
};

static const Kiln_ComponentDesc CORE_COMPONENTS[] = {
    { "timer", KN_ROLE_UNKNOWN,
      (int32_t)(sizeof(TIMER_PROPS) / sizeof(TIMER_PROPS[0])), TIMER_PROPS,
      (int32_t)(sizeof(TIMER_EVENTS) / sizeof(TIMER_EVENTS[0])), TIMER_EVENTS,
      KN_COMPONENT_NONVISUAL },
};

static const Kiln_LibInfo CORE_INFO = {
    KILN_ABI_VERSION,
    "core",
    "kiln-core-0000-0000-0000-000000000001",
    0, 2, 0,
    (int32_t)(sizeof(CORE_COMMANDS) / sizeof(CORE_COMMANDS[0])),
    CORE_COMMANDS,
    (int32_t)(sizeof(CORE_COMPONENTS) / sizeof(CORE_COMPONENTS[0])),
    CORE_COMPONENTS,
};

const Kiln_LibInfo *kiln_get_lib_info(void) {
    return &CORE_INFO;
}
