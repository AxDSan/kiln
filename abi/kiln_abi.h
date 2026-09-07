/* Kiln support-library ABI + SDK — v3 (Phase 2).
 *
 * The single documented contract between the Kiln runtime/compiler and a
 * support library, a clean-room descendant of EPL's
 * `GetNewInf`/`LIB_INFO`.  A library is a shared object that
 * exports ONE function, `kiln_get_lib_info`, returning a fully-populated
 * `Kiln_LibInfo`.  The compiler dlopens the library at build time to read
 * command signatures; the command *implementations* are then static-linked into
 * the program (BlackMoon model — no runtime library dependency.
 *
 * Third parties include THIS header to author a library in C/C++/Rust/Zig (M5).
 *
 * Frozen: the `SDT_*` numeric values, `Kiln_Slot` layout, and this struct
 * layout are ABI and MUST NOT change without bumping KILN_ABI_VERSION.
 */
#ifndef KILN_ABI_H
#define KILN_ABI_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

#define KILN_ABI_VERSION 4

/* --- Data-type tags (SDT_*) — the ABI type system. ---------
 * Numeric values are frozen.  Phase 2 uses INT/INT64/DOUBLE/TEXT; the rest are
 * reserved with fixed numbers so later phases don't renumber. */
enum {
    KN_SDT_NULL      = 0,   /* _SDT_NULL: no type (declarations only)   */
    KN_SDT_BYTE      = 1,
    KN_SDT_SHORT     = 2,
    KN_SDT_INT       = 3,   /* 32-bit int                               */
    KN_SDT_INT64     = 4,   /* 64-bit int                               */
    KN_SDT_FLOAT     = 5,
    KN_SDT_DOUBLE    = 6,   /* 64-bit IEEE-754                          */
    KN_SDT_DATE_TIME = 7,
    KN_SDT_BOOL      = 8,
    KN_SDT_TEXT      = 9,   /* char*, NUL-terminated; NULL = empty      */
    KN_SDT_BIN       = 10,  /* byte-set (Phase 3): ptr {int 1;int len;bytes} */
    KN_SDT_SUB_PTR   = 11,
    KN_SDT_STATMENT  = 12,
    /* 13 is KN_SDT_RECORD (below).  14 is the raw machine pointer: an opaque
     * 64-bit address for C interop — a buffer, a struct, a handle, an
     * out-parameter.  Its own number rather than riding KN_SDT_INT64 because a
     * library's declared signature is read back through these tags, so `ptr`
     * and `int64` MUST be distinguishable at that boundary — otherwise a
     * command declaring `ptr` and one declaring `int64` would round-trip to the
     * same IR type and the type system's ban on implicit int<->ptr conversion
     * could not be enforced.  It travels in the slot's pointer/int64 union like
     * TEXT does, so marshaling costs nothing new. */
    KN_SDT_PTR       = 14,
    KN_SDT_ALL       = 255  /* _SDT_ALL: any type (declarations only)   */
};

/* Array-ness is a FLAG above the element tag, not a second block of numbers:
 * every value above is frozen, so `int[]` has to be expressible without moving
 * `int`.  `KN_SDT_ARRAY_OF(KN_SDT_TEXT)` is an array of text. */
enum {
    KN_SDT_ARRAY_FLAG = 0x100,
    /* Declarations only, and only for commands the element type does not
     * change: `count` and `sort` read the element tag from the array itself at
     * run time.  Without it every such command would need one spelling per
     * element type. */
    KN_SDT_ANY_ARRAY  = 0x100 | 255,
    /* Declarations only: "whatever THIS call's array argument holds", which is
     * what makes appending text to an array of ints a compile error. */
    KN_SDT_ANY_ELEM   = 255
};
#define KN_SDT_ARRAY_OF(elem_tag) (KN_SDT_ARRAY_FLAG | (elem_tag))

/* A record and a dictionary, added the same way and for the same reason: no
 * frozen value moves.  13 is the first unassigned scalar tag, and the
 * dictionary flag sits one bit above the array flag, so `int{}` is expressible
 * without disturbing `int` or `int[]`.
 *
 * Which record a value is stays a compile-time fact — no record's identity
 * reaches a signature, only that it IS one.  The compiler mirrors these in
 * ir/src/lib.rs (`Ty::sdt_tag`) and the two MUST agree, because a library's
 * declared signature is read back through them. */
enum {
    KN_SDT_RECORD    = 13,          /* any record; which one is compile-time  */
    KN_SDT_DICT_FLAG = 0x200,       /* keyed collection of the value tag      */
    KN_SDT_ANY_DICT  = 0x200 | 255  /* declarations only: "a dictionary"      */
};
#define KN_SDT_DICT_OF(value_tag) (KN_SDT_DICT_FLAG | (value_tag))

/* --- Array object layout (SDT_ARRAY_OF) --------------------------------
 * A header followed by the elements, all one runtime-owned allocation:
 *
 *     { int32 elem_tag; int32 len; int32 cap; int32 _pad; int64 elems[cap]; }
 *
 * One slot-width per element, so an array of text holds pointers exactly the
 * way an array of int holds ints, and the value read out of an element is
 * already the 64 bits a slot carries.
 *
 * Allocated through kn_malloc like text, so kn_free_all() at exit releases it
 * and there is one ownership model rather than two.  Nothing ever moves an
 * array: `append` returns a NEW one, because reallocating would leave every
 * other name that held it pointing at freed memory. */
typedef struct Kiln_Array {
    int32_t elem_tag;   /* KN_SDT_* of one element                          */
    int32_t len;        /* elements in use                                  */
    int32_t cap;        /* elements allocated; always >= len                */
    int32_t _pad;       /* keep the elements 8-byte aligned                 */
} Kiln_Array;

/* --- Byte-set layout (SDT_BIN) ----------------------------------------
 * EPL's one-dimensional byte array: { int32 dims; int32 len; bytes[len] }.
 * The dimension count is always 1 here and exists so the shape matches what
 * EPL's own byte-sets carry. */
typedef struct Kiln_Bin {
    int32_t dims;       /* always 1                                         */
    int32_t len;
} Kiln_Bin;

/* --- Slot (MDATA_INF analog) — one argument or return value. ----------
 * A tagged 16-byte cell; the 8-byte value union sits at offset 8.  This is the
 * uniform in/out currency of every command. */
typedef struct Kiln_Slot {
    int32_t tag;      /* one of KN_SDT_*                                     */
    int32_t _pad;     /* keep the value 8-byte aligned                      */
    union {
        int32_t  i32;
        int64_t  i64;
        double   d;
        void    *ptr; /* SDT_TEXT: char*; SDT_BIN: byte-set ptr             */
    } v;
} Kiln_Slot;

/* Layout is ABI — the backend hard-codes these offsets when marshaling. */
_Static_assert(sizeof(Kiln_Slot) == 16, "Kiln_Slot must be 16 bytes");
_Static_assert(offsetof(Kiln_Slot, v) == 8, "value union must be at offset 8");

/* --- Command implementation signature. --------------------------------
 * cdecl.  `ret` points at the return slot (its tag is set by the callee, or
 * left KN_SDT_NULL for a void command).  `argv` is an array of `argc` slots. */
typedef void (*Kiln_CommandFn)(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);

/* --- Runtime↔library notification channel (NRS_*.2). -----------
 * All EPL-data heap allocation goes THROUGH the runtime so ownership stays
 * consistent across the program and its libraries.  A library asks the runtime
 * to allocate/free via kn_notify(); the runtime may also notify libraries of
 * lifecycle events over the same enum. */
enum {
    KN_NRS_MALLOC      = 1,  /* p1 = size (bytes)      -> returns void*       */
    KN_NRS_MFREE       = 2,  /* p1 = ptr                                      */
    KN_NRS_MREALLOC    = 3,  /* p1 = ptr, p2 = size    -> returns void*       */
    KN_NRS_FREE_ARY    = 4,  /* p1 = array ptr, p2 = SDT_* elem tag (Phase 3) */
    KN_NRS_RUNTIME_ERR = 5   /* p1 = const char* message (aborts)            */
};

/* The runtime's notification entry point (a global runtime symbol; libraries
 * call it directly when static-linked).  Returns a pointer for MALLOC/MREALLOC,
 * NULL otherwise. */
void *kn_notify(int32_t msg, void *p1, void *p2);

/* Convenience wrappers over the channel. */
static inline void *kn_malloc(long size)            { return kn_notify(KN_NRS_MALLOC, (void *)(size_t)size, 0); }
static inline void  kn_mfree(void *p)               { kn_notify(KN_NRS_MFREE, p, 0); }
static inline void *kn_mrealloc(void *p, long size) { return kn_notify(KN_NRS_MREALLOC, p, (void *)(size_t)size); }
static inline void  kn_runtime_error(const char *m) { kn_notify(KN_NRS_RUNTIME_ERR, (void *)m, 0); }

/* --- Error slot -------------------------------------------------------
 * A language with no out-parameters and no exceptions still has to report why
 * something failed.  A fallible command returns a sentinel (0 for a handle, -1
 * for a count, "" for text, false for a yes/no) and leaves the detail here, to
 * be read by the `last_error_code` / `last_error_text` core commands.
 *
 * The contract every fallible command follows: exactly one of kn_error_clear()
 * (success) or kn_error_set*() (failure) on every exit path.  Infallible
 * commands never touch the slot, so an error survives intervening arithmetic.
 * That is what lets `false` be read precisely: false with code 0 is a genuine
 * "no", false with a non-zero code is a failure.
 *
 * Plain externs rather than new KN_NRS_* messages: kn_notify carries two void*
 * and returns one, which cannot express the handle API below (three inputs plus
 * a function pointer, and function pointers do not portably round-trip through
 * void*).  Every NRS number is frozen forever; kn_notify is itself a plain
 * extern, so these are the same mechanism, not a new one. */
enum {
    KN_ERR_NONE        = 0,
    /* errno values pass through unchanged, so runtime codes start well clear
     * of any of them. */
    KN_ERR_BAD_HANDLE  = 10001, /* malformed, or index out of range          */
    KN_ERR_STALE       = 10002, /* generation mismatch: closed, maybe reused */
    KN_ERR_WRONG_KIND  = 10003, /* e.g. a directory handle sent to a file cmd*/
    KN_ERR_TABLE_FULL  = 10004,
    KN_ERR_INVALID_ARG = 10005,
    KN_ERR_UNSUPPORTED = 10006,
    KN_ERR_OUT_OF_RANGE= 10007  /* an index outside an array or byte-set     */
};

void    kn_error_clear(void);
void    kn_error_set(int32_t code, const char *msg);
/* `saved_errno` is a required parameter, and there is deliberately no argless
 * "capture errno now" variant: an author cannot use this without having copied
 * errno to a local first, which turns "I forgot to save it" from a wrong
 * message into a compile error.  The house rule is that the line immediately
 * after a failing call is `int e = errno;`, and the slot is written LAST, after
 * cleanup — fclose() and free() are exactly what clobber errno. */
void    kn_error_set_errno(int32_t saved_errno, const char *what);
int32_t kn_error_code(void);
const char *kn_error_message(void);   /* never NULL; "" when clear           */

/* --- Handle table -----------------------------------------------------
 * Resources a program holds across commands (an open file, a directory scan)
 * are named by a small positive int, never by an address: a program cannot
 * forge one, cannot dereference one, and cannot be handed a dangling one.
 *
 * The int is laid out (MSB first) sign:1 | kind:4 | generation:11 | index:16.
 * Bit 31 is always 0, so a handle is always positive.  Slot 0 is reserved and
 * kind 0 is KN_HK_NONE, so 0 is never a live handle and an uninitialised `int`
 * is rejected with no special case.  The generation is bumped on CLOSE, so a
 * handle outlives its resource detectably rather than silently addressing
 * whatever took the slot next.
 *
 * Limits, stated rather than discovered: 65535 live handles, 15 kinds, and a
 * generation that wraps after 2048 close/reuse cycles of one slot.
 *
 * Kinds are a flat namespace assigned HERE, not in library sources — there is
 * no link-time collision check for them the way there is for command names. */
enum {
    KN_HK_NONE   = 0,   /* reserved: never a valid kind                     */
    KN_HK_FILE   = 1,
    KN_HK_DIR    = 2,
    KN_HK_SOCKET = 3,
    KN_HK_PROC   = 4,
    KN_HK_CONFIG = 5,
    KN_HK_JSON   = 6
    /* 7..15 unassigned */
};

typedef void (*Kiln_HandleCloseFn)(void *payload);

/* All four set the error slot themselves on failure, so no library invents its
 * own bad-handle text and every family reports stale/wrong-kind identically. */
int32_t kn_handle_new(int32_t kind, void *payload, Kiln_HandleCloseFn close_fn);
void   *kn_handle_resolve(int32_t h, int32_t kind);  /* NULL on any failure  */
int32_t kn_handle_close(int32_t h, int32_t kind);    /* 1 ok, 0 failed       */
int32_t kn_handle_close_kind(int32_t kind);          /* -> count closed      */
void    kn_handle_close_all(void);                   /* idempotent           */

/* --- Event loop -------------------------------------------------------
 * A program lives while any event source is live.  The loop belongs to the
 * runtime rather than to any one library because a timer, a socket and a window
 * are all the same shape from here, and the alternative — whichever library
 * happens to be present owning the loop — leaves a console program with no way
 * to wait for anything at all.
 *
 * A source is a `pump` plus a period.  The loop calls `pump` when the period
 * has elapsed (0 = every turn, which is what a window wants), sleeps until the
 * earliest next due time when nothing is ready, and drops a source whose pump
 * answers 1.  When the last source is gone the loop returns, so a program that
 * registers nothing exits exactly as it would with no loop at all.
 *
 * `kn_loop_quit` latches: calling it before the loop starts — from `main`, the
 * common case — makes `kn_loop_run` return immediately rather than blocking on
 * sources that will never be serviced. */
typedef int32_t (*Kiln_PumpFn)(void *state);   /* 0 = still live, 1 = done  */

/* Returns a source id (>= 1), or 0 with the error slot set. */
int32_t kn_loop_add(Kiln_PumpFn pump, void *state, int32_t period_ms);
void    kn_loop_remove(int32_t source);
int32_t kn_loop_live(void);      /* sources still registered                  */
int32_t kn_loop_run(void);       /* -> the exit code passed to kn_loop_quit   */
void    kn_loop_quit(int32_t code);

/* --- Library metadata (LibInfo / GetNewInf analog). -------------------
 * Design-time metadata: names, signatures, versions.  It references command
 * symbols by NAME (`symbol`), not by pointer, so it can live in a metadata-only
 * translation unit that is compiled into the introspection `.so` ONLY and never
 * into a shipped program — the compiler resolves symbols at link time.  This is
 * EPL's `.fne` (design-time) vs `.fnr` (runtime) split, and the G8 "no metadata
 * in release output" story. */
/* --- Components. -----------------
 * A library contributes components through the SAME LibInfo mechanism
 * that carries commands.  A component declares its properties and events by
 * NAME, which is what makes the Object Inspector, form streaming, and the
 * designer generic — one primitive, exactly as Delphi's `published` RTTI does
 *.
 *
 * Accessibility is part of the descriptor, not an afterthought:
 * every component states its a11y role here, and per-instance name/state travel
 * with the properties below. */

/* A component either occupies a rectangle or it does not.  A timer, a server
 * and a tray icon have properties, events and an inspector row exactly as a
 * button does; what they lack is a parent to be drawn inside, which is why this
 * is one field rather than a second component mechanism.
 *
 * A non-visual component is created through its OWN library's entry points
 * instead of the kn_ui_* ones, named after the library that declares it:
 *
 *     int64_t     kn_<lib>_component_create(const char *type_name);
 *     int32_t     kn_<lib>_component_set(int64_t h, const char *prop, const char *value);
 *     const char *kn_<lib>_component_get(int64_t h, const char *prop);
 *     int32_t     kn_<lib>_component_get_int(int64_t h, const char *prop);
 *     int32_t     kn_<lib>_component_on(int64_t h, const char *event, void (*fn)(void));
 *
 * Handles count from 1 in creation order, per library, so the compiler knows
 * every handle as a constant and no instance id reaches the binary — the same
 * contract `kn_ui_create` already keeps (form root 1, children 2, 3, ...).
 * Two libraries' counters never meet: a handle is only ever passed back to the
 * entry points of the library that issued it. */
enum {
    KN_COMPONENT_VISUAL    = 0,   /* drawn inside a form                      */
    KN_COMPONENT_NONVISUAL = 1    /* declared at module level; no rectangle   */
};

/* An event handler emitted by the compiler.  Bound by function POINTER, never
 * by name: there is no name-based dispatch at run time, so no user identifier
 * reaches the shipped binary.
 *
 * The void signature is the binding currency, not the calling one.  An event
 * that declares parameters is dispatched by casting this back to a pointer
 * taking exactly those parameters — safe because the compiler emits the handler
 * side with that signature and only that one, whatever the user's subroutine
 * takes.  A component with no parameterised event never casts and nothing about
 * it changes. */
typedef void (*Kiln_HandlerFn)(void);

/* Accessibility roles (subset of the AccessKit/platform role vocabulary). */
enum {
    KN_ROLE_UNKNOWN = 0,
    KN_ROLE_WINDOW  = 1,
    KN_ROLE_BUTTON  = 2,
    KN_ROLE_LABEL   = 3,
    KN_ROLE_TEXTBOX = 4,
    KN_ROLE_CHECKBOX= 5,
    KN_ROLE_LIST    = 6,
    KN_ROLE_GROUP   = 7
};

typedef struct Kiln_PropertyDesc {
    const char *name;          /* surface property name, e.g. "text"        */
    int32_t     tag;           /* KN_SDT_* value type                       */
    const char *default_value; /* textual default; NULL = none              */
    /* Which editor an inspector should offer: "color", "file", "font",
     * "multiline".  NULL asks for the plain one the tag implies.  A hint, not
     * a type — a colour is still text, and a tool free to ignore this still
     * shows something correct. */
    const char *editor;
} Kiln_PropertyDesc;

typedef struct Kiln_EventDesc {
    const char *name;          /* surface event name, e.g. "click"          */
    /* What the event hands its handler.  Most events hand it nothing, which is
     * why these are APPENDED: a positional `{ "click" }` zero-fills to
     * param_count 0 and a NULL table, so every descriptor written against v2
     * still says exactly what it meant.
     *
     * A handler may declare these parameters or declare none — the checker
     * accepts both and the compiler emits a thunk with the event's signature
     * either way, so the library always calls through a pointer whose type it
     * knows.  Anything else is a compile error naming both signatures. */
    int32_t     param_count;
    const int32_t *param_tags; /* param_count KN_SDT_* tags                 */
} Kiln_EventDesc;

typedef struct Kiln_ComponentDesc {
    const char                 *name;        /* surface type name, e.g. "button" */
    int32_t                     a11y_role;   /* KN_ROLE_* (D16)                  */
    int32_t                     property_count;
    const Kiln_PropertyDesc *properties;
    int32_t                     event_count;
    const Kiln_EventDesc    *events;
    int32_t                     kind;        /* KN_COMPONENT_*; 0 = visual       */
} Kiln_ComponentDesc;

typedef struct Kiln_CommandDesc {
    const char    *name;      /* surface command name (English)             */
    const char    *symbol;    /* link symbol of the Kiln_CommandFn impl   */
    int32_t        ret_tag;   /* KN_SDT_* return, KN_SDT_NULL for void       */
    int32_t        argc;      /* parameter count                            */
    const int32_t *arg_tags;  /* argc parameter tags                        */
    /* Documentation. Both may be NULL, and a row written before these fields
     * existed leaves them NULL by C's aggregate-initialisation rules — which
     * is what lets a library be documented one command at a time instead of
     * all at once.
     *
     * `doc` is one sentence, no trailing full stop, describing what the
     * command does and what it returns when it fails. It is what the language
     * server shows on hover and what the generated reference prints.
     *
     * `example` is one to three lines of Kiln that call it, without the
     * surrounding `module`/`sub` — the documentation generator supplies those,
     * and `tools/check-docs.sh` compiles the result. An example that does not
     * compile fails the build, which is the point: an example nobody ran is
     * worse than no example. */
    const char    *doc;
    const char    *example;
} Kiln_CommandDesc;

typedef struct Kiln_LibInfo {
    int32_t                     abi_version;   /* must == KILN_ABI_VERSION */
    const char                 *name;
    const char                 *guid;          /* stable per library          */
    int32_t                     ver_major;
    int32_t                     ver_minor;
    int32_t                     ver_build;
    int32_t                     command_count;
    const Kiln_CommandDesc  *commands;
    /* Visual components contributed by this library (may be 0 / NULL). */
    int32_t                       component_count;
    const Kiln_ComponentDesc  *components;
} Kiln_LibInfo;

/* THE single required export (EPL: GetNewInf). */
const Kiln_LibInfo *kiln_get_lib_info(void);

/* --- SDK helpers for command authors ---------------------------------- */
static inline int32_t  kn_arg_int(Kiln_Slot *argv, int i)    { return argv[i].v.i32; }
static inline int64_t  kn_arg_int64(Kiln_Slot *argv, int i)  { return argv[i].v.i64; }
static inline double   kn_arg_double(Kiln_Slot *argv, int i) { return argv[i].v.d; }
static inline char    *kn_arg_text(Kiln_Slot *argv, int i)   { return (char *)argv[i].v.ptr; }
static inline void    *kn_arg_ptr(Kiln_Slot *argv, int i)    { return argv[i].v.ptr; }

static inline void kn_ret_int(Kiln_Slot *r, int32_t x)   { r->tag = KN_SDT_INT;    r->v.i32 = x; }
static inline void kn_ret_int64(Kiln_Slot *r, int64_t x) { r->tag = KN_SDT_INT64;  r->v.i64 = x; }
static inline void kn_ret_double(Kiln_Slot *r, double x) { r->tag = KN_SDT_DOUBLE; r->v.d   = x; }
static inline void kn_ret_text(Kiln_Slot *r, char *p)    { r->tag = KN_SDT_TEXT;   r->v.ptr = p; }
static inline void kn_ret_bool(Kiln_Slot *r, int32_t x)  { r->tag = KN_SDT_BOOL;   r->v.i32 = x ? 1 : 0; }
static inline void kn_ret_ptr(Kiln_Slot *r, void *p)     { r->tag = KN_SDT_PTR;    r->v.ptr = p; }
static inline int32_t kn_arg_bool(Kiln_Slot *argv, int i) { return argv[i].v.i32 != 0; }

#ifdef __cplusplus
}
#endif
#endif /* KILN_ABI_H */
