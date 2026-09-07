/* The `json` library — reading and writing JSON.
 *
 * A parsed document is a HANDLE (kind KN_HK_JSON).  A program never sees the
 * tree; it names a place inside the document with a PATH string, because
 * Kiln has no record type and no arrays.  The path is therefore the whole
 * interface, and this is its grammar:
 *
 *     ""            the document itself (the root)
 *     name          the member `name` of an object
 *     a.b.c         nested members, dot-separated
 *     [2]           element 2 of an array (0-based)
 *     items[2].id   the two combined, in any order
 *
 * A member name may contain anything except `.` and `[`; a document whose keys
 * contain those characters cannot be addressed, which is the price of a path
 * syntax a person can type.  An index must be a plain decimal number.
 *
 * `json_set_*` creates the objects (or arrays, when the next step is an index)
 * it walks through, so `json_set_text(h, "user.name", "Ada")` works on an empty
 * object.  Writing at index == count appends; any larger index is out of range.
 *
 * Failure follows the house rule in libs/README.md: a handle command returns 0,
 * a count -1, text "", a yes/no false, and the reason — including the byte
 * offset of a parse error — is left in the error slot for `last_error_code` and
 * `last_error_text`.  Every fallible command below calls exactly one of
 * kn_error_clear() or kn_error_set*() on every exit path.
 */
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "kiln_abi.h"
#include "json_internal.h"

/* --- small helpers ----------------------------------------------------- */

static const char *json_nz(const char *s) { return s ? s : ""; }

/* Every text result is runtime-owned, so a program can hold it like any other. */
static void json_ret_copy(Kiln_Slot *ret, const char *s) {
    size_t n = strlen(s);
    char *out = (char *)kn_malloc((long)n + 1);
    if (!out) { kn_ret_text(ret, NULL); return; }
    memcpy(out, s, n + 1);
    kn_ret_text(ret, out);
}

static void json_fail_text(Kiln_Slot *ret, int32_t code, const char *msg) {
    kn_error_set(code, msg);
    json_ret_copy(ret, "");
}

static JsonNode *json_doc(int32_t h) {
    return (JsonNode *)kn_handle_resolve(h, KN_HK_JSON);   /* sets the slot */
}

/* --- the path walk ----------------------------------------------------- */

/* One step of a path.  Returns 1 for a segment, 0 at the end, -1 malformed. */
static int json_path_next(const char **pp, int *is_index, long *idx,
                          const char **kstart, size_t *klen) {
    const char *p = *pp;
    if (*p == '\0') return 0;
    if (*p == '[') {
        p++;
        if (*p < '0' || *p > '9') return -1;
        long n = 0;
        while (*p >= '0' && *p <= '9') {
            n = n * 10 + (*p - '0');
            if (n > 100000000L) return -1;             /* absurd index        */
            p++;
        }
        if (*p != ']') return -1;
        p++;
        *is_index = 1;
        *idx = n;
    } else {
        const char *s = p;
        while (*p && *p != '.' && *p != '[') p++;
        if (p == s) return -1;                         /* empty member name   */
        *is_index = 0;
        *kstart = s;
        *klen = (size_t)(p - s);
    }
    if (*p == '.') {
        p++;
        if (*p == '\0' || *p == '.' || *p == '[') return -1;
    }
    *pp = p;
    return 1;
}

static long json_obj_index_n(const JsonNode *o, const char *k, size_t n) {
    if (o->type != JSON_T_OBJ || !o->keys) return -1;
    for (long i = 0; i < o->count; i++) {
        const char *key = o->keys[i];
        if (key && strlen(key) == n && memcmp(key, k, n) == 0) return i;
    }
    return -1;
}

/* Look a path up.  1 = found (*out set), 0 = no such place, -1 = malformed. */
static int json_path_find(JsonNode *root, const char *path, JsonNode **out) {
    const char *p = json_nz(path);
    JsonNode *cur = root;
    for (;;) {
        int is_index = 0; long idx = 0; const char *ks = NULL; size_t kl = 0;
        int rc = json_path_next(&p, &is_index, &idx, &ks, &kl);
        if (rc == 0) { *out = cur; return 1; }
        if (rc < 0) return -1;
        if (is_index) {
            /* Paths index from 1, like everything else in this language.  That
             * differs from JSONPath, which counts from 0 — but a language with
             * two indexing bases is worse than one that differs from a spec
             * nobody reads while writing `items[1]`. */
            if (cur->type != JSON_T_ARR || idx < 1 || idx > cur->count) return 0;
            cur = cur->kids[idx - 1];
        } else {
            long at = json_obj_index_n(cur, ks, kl);
            if (at < 0) return 0;
            cur = cur->kids[at];
        }
    }
}

/* Walk to the CONTAINER that holds the last segment, creating what is missing
 * on the way when `create` is set.  1 on success; 0 with *why on failure. */
static int json_path_parent(JsonNode *root, const char *path, int create,
                            JsonNode **parent, int *last_is_index, long *last_idx,
                            const char **last_key, size_t *last_klen,
                            const char **why) {
    const char *p = json_nz(path);
    JsonNode *cur = root;
    int is_index = 0; long idx = 0; const char *ks = NULL; size_t kl = 0;

    int rc = json_path_next(&p, &is_index, &idx, &ks, &kl);
    if (rc == 0) { *why = "the root itself cannot be set or removed"; return 0; }
    if (rc < 0)  { *why = "malformed path"; return 0; }

    for (;;) {
        if (*p == '\0') {                              /* this is the last one */
            *parent = cur;
            *last_is_index = is_index;
            *last_idx = idx;
            *last_key = ks;
            *last_klen = kl;
            return 1;
        }
        /* Peek at the next segment: an index wants an array, a name an object. */
        const char *peek = p;
        int pi = 0; long pidx = 0; const char *pk = NULL; size_t pl = 0;
        int prc = json_path_next(&peek, &pi, &pidx, &pk, &pl);
        if (prc < 0) { *why = "malformed path"; return 0; }

        JsonNode *next = NULL;
        if (is_index) {
            if (cur->type != JSON_T_ARR) { *why = "an index was applied to something that is not an array"; return 0; }
            if (idx < 1 || idx > cur->count) { *why = "array index out of range"; return 0; }
            next = cur->kids[idx - 1];
        } else {
            long at = json_obj_index_n(cur, ks, kl);
            if (at >= 0) {
                next = cur->kids[at];
            } else {
                if (!create) { *why = "no such member"; return 0; }
                if (cur->type != JSON_T_OBJ) { *why = "a member name was applied to something that is not an object"; return 0; }
                JsonNode *made = json_node_new(pi ? JSON_T_ARR : JSON_T_OBJ);
                char *key = (char *)malloc(kl + 1);
                if (!made || !key) {
                    json_node_free(made); free(key);
                    *why = "out of memory";
                    return 0;
                }
                memcpy(key, ks, kl);
                key[kl] = '\0';
                int ok = json_obj_put(cur, key, made);
                free(key);
                if (!ok) { json_node_free(made); *why = "out of memory"; return 0; }
                next = made;
            }
        }
        cur = next;
        is_index = pi; idx = pidx; ks = pk; kl = pl;
        p = peek;
    }
}

/* Put `val` (already owned by us) at the last segment of `path`.  Frees `val`
 * and returns 0 with *why on failure. */
static int json_place(JsonNode *root, const char *path, JsonNode *val, const char **why) {
    JsonNode *parent = NULL;
    int is_index = 0; long idx = 0; const char *ks = NULL; size_t kl = 0;
    if (!json_path_parent(root, path, 1, &parent, &is_index, &idx, &ks, &kl, why)) {
        json_node_free(val);
        return 0;
    }
    if (is_index) {
        if (parent->type != JSON_T_ARR) {
            *why = "an index was applied to something that is not an array";
            json_node_free(val);
            return 0;
        }
        if (idx >= 1 && idx <= parent->count) {
            json_node_free(parent->kids[idx - 1]);
            parent->kids[idx - 1] = val;
            return 1;
        }
        if (idx == parent->count + 1) {                /* one past the end: append */
            if (!json_arr_push(parent, val)) { *why = "out of memory"; json_node_free(val); return 0; }
            return 1;
        }
        *why = "array index out of range";
        json_node_free(val);
        return 0;
    }
    if (parent->type != JSON_T_OBJ) {
        *why = "a member name was applied to something that is not an object";
        json_node_free(val);
        return 0;
    }
    char *key = (char *)malloc(kl + 1);
    if (!key) { *why = "out of memory"; json_node_free(val); return 0; }
    memcpy(key, ks, kl);
    key[kl] = '\0';
    int ok = json_obj_put(parent, key, val);
    free(key);
    if (!ok) { *why = "out of memory"; json_node_free(val); return 0; }
    return 1;
}

/* The three-way find every getter starts with: resolve the handle, walk the
 * path, and report the two ways that can go wrong identically everywhere. */
static JsonNode *json_locate(int32_t h, const char *path, const char **why) {
    JsonNode *root = json_doc(h);
    if (!root) { *why = NULL; return NULL; }           /* slot already set    */
    JsonNode *at = NULL;
    int rc = json_path_find(root, json_nz(path), &at);
    if (rc < 0) { *why = "malformed path"; return NULL; }
    if (rc == 0) { *why = "no such path in the document"; return NULL; }
    return at;
}

/* --- opening and closing ----------------------------------------------- */

static int32_t json_adopt(JsonNode *root) {
    int32_t h = kn_handle_new(KN_HK_JSON, root, json_node_free_payload);
    if (!h) { json_node_free(root); return 0; }        /* slot already set    */
    kn_error_clear();
    return h;
}

/* json_parse(text) -> int : a document handle, 0 on malformed input. */
void json_parse(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    char err[192];
    JsonNode *root = json_parse_text(json_nz(kn_arg_text(argv, 0)), err, sizeof err);
    if (!root) {
        char msg[224];
        snprintf(msg, sizeof msg, "json_parse: %s", err);
        kn_error_set(KN_ERR_INVALID_ARG, msg);
        kn_ret_int(ret, 0);
        return;
    }
    kn_ret_int(ret, json_adopt(root));
}

/* json_parse_file(text path) -> int */
void json_parse_file(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *path = json_nz(kn_arg_text(argv, 0));

    FILE *f = fopen(path, "rb");
    int e = errno;
    if (!f) { kn_error_set_errno(e, "json_parse_file"); kn_ret_int(ret, 0); return; }

    char  *buf = NULL;
    size_t len = 0, cap = 0;
    int    read_errno = 0;
    for (;;) {
        if (len + 4096 + 1 > cap) {
            size_t want = cap ? cap * 2 : 8192;
            while (want < len + 4096 + 1) want *= 2;
            char *q = (char *)realloc(buf, want);
            if (!q) {
                free(buf);
                fclose(f);
                kn_error_set(KN_ERR_INVALID_ARG, "json_parse_file: out of memory");
                kn_ret_int(ret, 0);
                return;
            }
            buf = q;
            cap = want;
        }
        size_t got = fread(buf + len, 1, 4096, f);
        int re = errno;                    /* nothing may intervene           */
        len += got;
        if (got < 4096) { read_errno = re; break; }
    }
    int bad = ferror(f);
    int re = read_errno;
    fclose(f);
    if (bad) {
        free(buf);
        kn_error_set_errno(re, "json_parse_file");
        kn_ret_int(ret, 0);
        return;
    }
    if (!buf) {                                        /* an empty file       */
        buf = (char *)malloc(1);
        if (!buf) {
            kn_error_set(KN_ERR_INVALID_ARG, "json_parse_file: out of memory");
            kn_ret_int(ret, 0);
            return;
        }
    }
    buf[len] = '\0';

    char err[192];
    JsonNode *root = json_parse_text(buf, err, sizeof err);
    free(buf);
    if (!root) {
        char msg[256];
        snprintf(msg, sizeof msg, "json_parse_file: %s: %s", path, err);
        kn_error_set(KN_ERR_INVALID_ARG, msg);
        kn_ret_int(ret, 0);
        return;
    }
    kn_ret_int(ret, json_adopt(root));
}

/* json_new_object() -> int */
void json_new_object(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; (void)argv;
    JsonNode *root = json_node_new(JSON_T_OBJ);
    if (!root) { kn_error_set(KN_ERR_INVALID_ARG, "json_new_object: out of memory"); kn_ret_int(ret, 0); return; }
    kn_ret_int(ret, json_adopt(root));
}

/* json_new_array() -> int */
void json_new_array(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; (void)argv;
    JsonNode *root = json_node_new(JSON_T_ARR);
    if (!root) { kn_error_set(KN_ERR_INVALID_ARG, "json_new_array: out of memory"); kn_ret_int(ret, 0); return; }
    kn_ret_int(ret, json_adopt(root));
}

/* json_close(int) -> bool.  kn_handle_close writes the slot either way. */
void json_close(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    kn_ret_bool(ret, kn_handle_close(kn_arg_int(argv, 0), KN_HK_JSON));
}

/* json_close_all() -> int : how many were open.  Cannot fail. */
void json_close_all(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; (void)argv;
    kn_ret_int(ret, kn_handle_close_kind(KN_HK_JSON));
}

/* --- writing the document out ------------------------------------------ */

static void json_stringify_into(Kiln_Slot *ret, int32_t h, int pretty) {
    JsonNode *root = json_doc(h);
    if (!root) { json_ret_copy(ret, ""); return; }      /* slot already set    */
    char *text = json_write(root, pretty);
    if (!text) { json_fail_text(ret, KN_ERR_INVALID_ARG, "json_stringify: out of memory"); return; }
    kn_error_clear();
    json_ret_copy(ret, text);
    free(text);
}

/* json_stringify(int) -> text : compact, one line. */
void json_stringify(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    json_stringify_into(ret, kn_arg_int(argv, 0), 0);
}

/* json_stringify_pretty(int) -> text : two-space indentation. */
void json_stringify_pretty(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    json_stringify_into(ret, kn_arg_int(argv, 0), 1);
}

/* json_save(int, text path) -> bool : pretty-printed, with a trailing newline. */
void json_save(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    JsonNode *root = json_doc(kn_arg_int(argv, 0));
    if (!root) { kn_ret_bool(ret, 0); return; }         /* slot already set    */
    const char *path = json_nz(kn_arg_text(argv, 1));

    char *text = json_write(root, 1);
    if (!text) { kn_error_set(KN_ERR_INVALID_ARG, "json_save: out of memory"); kn_ret_bool(ret, 0); return; }

    FILE *f = fopen(path, "wb");
    int e = errno;
    if (!f) { free(text); kn_error_set_errno(e, "json_save"); kn_ret_bool(ret, 0); return; }

    size_t n = strlen(text);
    size_t wrote = fwrite(text, 1, n, f);
    int we = errno;
    free(text);
    if (wrote == n) { fputc('\n', f); we = errno; }
    int failed = (wrote != n) || ferror(f);
    int cr = fclose(f);
    int ce = errno;                        /* nothing may intervene           */
    if (cr != 0) { failed = 1; we = ce; }  /* ENOSPC often surfaces only here */
    if (failed) { kn_error_set_errno(we, "json_save"); kn_ret_bool(ret, 0); return; }
    kn_error_clear();
    kn_ret_bool(ret, 1);
}

/* --- asking about a place ---------------------------------------------- */

/* json_type(int, text path) -> text : object array text number bool null none */
void json_type(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    JsonNode *root = json_doc(kn_arg_int(argv, 0));
    if (!root) { json_ret_copy(ret, ""); return; }      /* slot already set    */
    JsonNode *at = NULL;
    int rc = json_path_find(root, json_nz(kn_arg_text(argv, 1)), &at);
    if (rc < 0) { json_fail_text(ret, KN_ERR_INVALID_ARG, "json_type: malformed path"); return; }
    kn_error_clear();
    if (rc == 0) { json_ret_copy(ret, "none"); return; }
    switch (at->type) {
    case JSON_T_OBJ:  json_ret_copy(ret, "object"); return;
    case JSON_T_ARR:  json_ret_copy(ret, "array");  return;
    case JSON_T_STR:  json_ret_copy(ret, "text");   return;
    case JSON_T_NUM:  json_ret_copy(ret, "number"); return;
    case JSON_T_BOOL: json_ret_copy(ret, "bool");   return;
    default:          json_ret_copy(ret, "null");   return;
    }
}

/* json_has(int, text path) -> bool : false with code 0 is a genuine "no". */
void json_has(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    JsonNode *root = json_doc(kn_arg_int(argv, 0));
    if (!root) { kn_ret_bool(ret, 0); return; }         /* slot already set    */
    JsonNode *at = NULL;
    int rc = json_path_find(root, json_nz(kn_arg_text(argv, 1)), &at);
    if (rc < 0) { kn_error_set(KN_ERR_INVALID_ARG, "json_has: malformed path"); kn_ret_bool(ret, 0); return; }
    kn_error_clear();
    kn_ret_bool(ret, rc == 1);
}

/* json_count(int, text path) -> int : elements of an array, members of an
 * object, -1 for anything else. */
void json_count(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *why = NULL;
    JsonNode *at = json_locate(kn_arg_int(argv, 0), kn_arg_text(argv, 1), &why);
    if (!at) {
        if (why) kn_error_set(KN_ERR_INVALID_ARG, why);
        kn_ret_int(ret, -1);
        return;
    }
    if (at->type != JSON_T_ARR && at->type != JSON_T_OBJ) {
        kn_error_set(KN_ERR_INVALID_ARG, "json_count: not an array or an object");
        kn_ret_int(ret, -1);
        return;
    }
    kn_error_clear();
    kn_ret_int(ret, (int32_t)at->count);
}

/* json_key_at(int, text path, int i) -> text : the i'th member name. */
void json_key_at(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *why = NULL;
    JsonNode *at = json_locate(kn_arg_int(argv, 0), kn_arg_text(argv, 1), &why);
    if (!at) {
        if (why) kn_error_set(KN_ERR_INVALID_ARG, why);
        json_ret_copy(ret, "");
        return;
    }
    if (at->type != JSON_T_OBJ) { json_fail_text(ret, KN_ERR_INVALID_ARG, "json_key_at: not an object"); return; }
    int32_t i = kn_arg_int(argv, 2);
    if (i < 1 || (long)i > at->count) { json_fail_text(ret, KN_ERR_INVALID_ARG, "json_key_at: index out of range"); return; }
    kn_error_clear();
    json_ret_copy(ret, at->keys[i - 1] ? at->keys[i - 1] : "");
}

/* --- reading a value --------------------------------------------------- */

/* json_get_text(int, text path) -> text : the value must BE text; a number is
 * not silently formatted, because a program that meant a number should say so. */
void json_get_text(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *why = NULL;
    JsonNode *at = json_locate(kn_arg_int(argv, 0), kn_arg_text(argv, 1), &why);
    if (!at) {
        if (why) kn_error_set(KN_ERR_INVALID_ARG, why);
        json_ret_copy(ret, "");
        return;
    }
    if (at->type != JSON_T_STR) { json_fail_text(ret, KN_ERR_INVALID_ARG, "json_get_text: the value is not text"); return; }
    kn_error_clear();
    json_ret_copy(ret, at->str ? at->str : "");
}

/* json_get_int(int, text path) -> int : 0 on failure, with the reason set. */
void json_get_int(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *why = NULL;
    JsonNode *at = json_locate(kn_arg_int(argv, 0), kn_arg_text(argv, 1), &why);
    if (!at) {
        if (why) kn_error_set(KN_ERR_INVALID_ARG, why);
        kn_ret_int(ret, 0);
        return;
    }
    if (at->type != JSON_T_NUM) { kn_error_set(KN_ERR_INVALID_ARG, "json_get_int: the value is not a number"); kn_ret_int(ret, 0); return; }
    if (at->num > 2147483647.0 || at->num < -2147483648.0) {
        kn_error_set(KN_ERR_INVALID_ARG, "json_get_int: the number does not fit in an int");
        kn_ret_int(ret, 0);
        return;
    }
    kn_error_clear();
    kn_ret_int(ret, (int32_t)at->num);
}

/* json_get_double(int, text path) -> double : 0 on failure. */
void json_get_double(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *why = NULL;
    JsonNode *at = json_locate(kn_arg_int(argv, 0), kn_arg_text(argv, 1), &why);
    if (!at) {
        if (why) kn_error_set(KN_ERR_INVALID_ARG, why);
        kn_ret_double(ret, 0.0);
        return;
    }
    if (at->type != JSON_T_NUM) { kn_error_set(KN_ERR_INVALID_ARG, "json_get_double: the value is not a number"); kn_ret_double(ret, 0.0); return; }
    kn_error_clear();
    kn_ret_double(ret, at->num);
}

/* json_get_bool(int, text path) -> bool */
void json_get_bool(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *why = NULL;
    JsonNode *at = json_locate(kn_arg_int(argv, 0), kn_arg_text(argv, 1), &why);
    if (!at) {
        if (why) kn_error_set(KN_ERR_INVALID_ARG, why);
        kn_ret_bool(ret, 0);
        return;
    }
    if (at->type != JSON_T_BOOL) { kn_error_set(KN_ERR_INVALID_ARG, "json_get_bool: the value is not a yes/no"); kn_ret_bool(ret, 0); return; }
    kn_error_clear();
    kn_ret_bool(ret, at->bval);
}

/* --- writing a value --------------------------------------------------- */

static void json_set_common(Kiln_Slot *ret, int32_t h, const char *path, JsonNode *val, const char *who) {
    JsonNode *root = json_doc(h);
    if (!root) { json_node_free(val); kn_ret_bool(ret, 0); return; }  /* slot set */
    if (!val) { kn_error_set(KN_ERR_INVALID_ARG, "out of memory"); kn_ret_bool(ret, 0); return; }
    const char *why = "failed";
    if (!json_place(root, json_nz(path), val, &why)) {
        char msg[224];
        snprintf(msg, sizeof msg, "%s: %s", who, why);
        kn_error_set(KN_ERR_INVALID_ARG, msg);
        kn_ret_bool(ret, 0);
        return;
    }
    kn_error_clear();
    kn_ret_bool(ret, 1);
}

/* json_set_text(int, text path, text value) -> bool */
void json_set_text(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    JsonNode *v = json_node_new(JSON_T_STR);
    if (v && !json_node_set_str(v, json_nz(kn_arg_text(argv, 2)))) { json_node_free(v); v = NULL; }
    json_set_common(ret, kn_arg_int(argv, 0), kn_arg_text(argv, 1), v, "json_set_text");
}

/* json_set_int(int, text path, int value) -> bool */
void json_set_int(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    JsonNode *v = json_node_new(JSON_T_NUM);
    if (v) v->num = (double)kn_arg_int(argv, 2);
    json_set_common(ret, kn_arg_int(argv, 0), kn_arg_text(argv, 1), v, "json_set_int");
}

/* json_set_double(int, text path, double value) -> bool */
void json_set_double(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    JsonNode *v = json_node_new(JSON_T_NUM);
    if (v) v->num = kn_arg_double(argv, 2);
    json_set_common(ret, kn_arg_int(argv, 0), kn_arg_text(argv, 1), v, "json_set_double");
}

/* json_set_bool(int, text path, bool value) -> bool */
void json_set_bool(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    JsonNode *v = json_node_new(JSON_T_BOOL);
    if (v) v->bval = kn_arg_bool(argv, 2);
    json_set_common(ret, kn_arg_int(argv, 0), kn_arg_text(argv, 1), v, "json_set_bool");
}

/* json_set_null(int, text path) -> bool : JSON's null is a value, not an
 * absence, so a program that must emit one needs a way to say it. */
void json_set_null(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    JsonNode *v = json_node_new(JSON_T_NULL);
    json_set_common(ret, kn_arg_int(argv, 0), kn_arg_text(argv, 1), v, "json_set_null");
}

/* json_set_object(int, text path) -> bool : an empty object, to nest into. */
void json_set_object(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    JsonNode *v = json_node_new(JSON_T_OBJ);
    json_set_common(ret, kn_arg_int(argv, 0), kn_arg_text(argv, 1), v, "json_set_object");
}

/* json_set_array(int, text path) -> bool : an empty array, to append into. */
void json_set_array(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    JsonNode *v = json_node_new(JSON_T_ARR);
    json_set_common(ret, kn_arg_int(argv, 0), kn_arg_text(argv, 1), v, "json_set_array");
}

/* json_remove(int, text path) -> bool : false with code 0 means it was not
 * there; false with a code means the path could not be walked. */
void json_remove(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    JsonNode *root = json_doc(kn_arg_int(argv, 0));
    if (!root) { kn_ret_bool(ret, 0); return; }         /* slot already set    */

    JsonNode *parent = NULL;
    int is_index = 0; long idx = 0; const char *ks = NULL; size_t kl = 0;
    const char *why = "failed";
    if (!json_path_parent(root, json_nz(kn_arg_text(argv, 1)), 0,
                          &parent, &is_index, &idx, &ks, &kl, &why)) {
        if (strcmp(why, "no such member") == 0) { kn_error_clear(); kn_ret_bool(ret, 0); return; }
        char msg[224];
        snprintf(msg, sizeof msg, "json_remove: %s", why);
        kn_error_set(KN_ERR_INVALID_ARG, msg);
        kn_ret_bool(ret, 0);
        return;
    }
    long at;
    if (is_index) {
        if (parent->type != JSON_T_ARR) {
            kn_error_set(KN_ERR_INVALID_ARG, "json_remove: an index was applied to something that is not an array");
            kn_ret_bool(ret, 0);
            return;
        }
        at = (idx < parent->count) ? idx : -1;
    } else {
        at = json_obj_index_n(parent, ks, kl);
    }
    kn_error_clear();
    if (at < 0) { kn_ret_bool(ret, 0); return; }        /* a genuine "no"      */
    json_container_remove(parent, at);
    kn_ret_bool(ret, 1);
}
