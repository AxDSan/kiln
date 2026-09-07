/* The "config" support library — INI settings files.
 *
 * INI because it is the settings format a beginner can open in a text editor
 * and understand without being taught a grammar first: a `[section]`, then
 * `key = value` lines, and `#` or `;` for a comment.
 *
 * A document is a LIST OF LINES, not a map. That is the whole design decision
 * in this file, and it exists so that saving a file does not throw away the
 * comments and blank lines the person who wrote it put there. A map would
 * round-trip the data and silently delete the explanation next to it, and a
 * settings writer that eats your notes is one you stop letting near your files.
 * Unrecognised lines are kept verbatim for the same reason.
 *
 * Section and key names match case-insensitively (ASCII), because these files
 * are typed by hand and `[Window]` and `[window]` are the same section to
 * everyone except a computer.
 *
 * Only the public SDK header is included: this library uses nothing the
 * runtime does not promise to a third party.
 */
#include <ctype.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "kiln_abi.h"

/* --- the document ----------------------------------------------------- */

enum { CFG_RAW = 0, CFG_SECTION = 1, CFG_PAIR = 2 };

typedef struct ConfigLine {
    int   kind;
    char *raw;      /* CFG_RAW: the line, verbatim                          */
    char *section;  /* CFG_SECTION: its name. CFG_PAIR: its owning section  */
    char *key;      /* CFG_PAIR                                             */
    char *value;    /* CFG_PAIR                                             */
} ConfigLine;

typedef struct Config {
    char       *path;
    ConfigLine *lines;
    long        n, cap;
} Config;

/* Internal state is plain malloc: it is owned by the handle and freed by the
 * close function. kn_malloc is for values handed BACK to the program, which the
 * runtime owns and frees at exit. */
static char *config_dup(const char *s) {
    if (!s) s = "";
    size_t n = strlen(s) + 1;
    char *p = (char *)malloc(n);
    if (p) memcpy(p, s, n);
    return p;
}

static int config_ieq(const char *a, const char *b) {
    if (!a) a = "";
    if (!b) b = "";
    for (;; a++, b++) {
        int ca = tolower((unsigned char)*a), cb = tolower((unsigned char)*b);
        if (ca != cb) return 0;
        if (!ca) return 1;
    }
}

static void config_line_free(ConfigLine *l) {
    free(l->raw); free(l->section); free(l->key); free(l->value);
    l->raw = l->section = l->key = l->value = NULL;
}

static void config_free(void *payload) {
    Config *c = (Config *)payload;
    if (!c) return;
    for (long i = 0; i < c->n; i++) config_line_free(&c->lines[i]);
    free(c->lines);
    free(c->path);
    free(c);
}

/* Insert a blank line record at `at` (== n appends). Returns its index, or -1. */
static long config_line_insert(Config *c, long at) {
    if (at < 0 || at > c->n) at = c->n;
    if (c->n == c->cap) {
        long cap = c->cap ? c->cap * 2 : 16;
        ConfigLine *nl = (ConfigLine *)realloc(c->lines, (size_t)cap * sizeof *nl);
        if (!nl) return -1;
        c->lines = nl;
        c->cap = cap;
    }
    memmove(&c->lines[at + 1], &c->lines[at], (size_t)(c->n - at) * sizeof *c->lines);
    memset(&c->lines[at], 0, sizeof c->lines[at]);
    c->n++;
    return at;
}

static void config_line_remove(Config *c, long at) {
    if (at < 0 || at >= c->n) return;
    config_line_free(&c->lines[at]);
    memmove(&c->lines[at], &c->lines[at + 1], (size_t)(c->n - at - 1) * sizeof *c->lines);
    c->n--;
}

/* --- parsing ---------------------------------------------------------- */

/* Trim ASCII whitespace in place, returning a pointer into the same buffer. */
static char *config_trim(char *s) {
    while (*s && isspace((unsigned char)*s)) s++;
    char *e = s + strlen(s);
    while (e > s && isspace((unsigned char)e[-1])) e--;
    *e = '\0';
    return s;
}

/* Add one already-newline-stripped source line to the document. `cur` is the
 * section in force, updated when a header is seen. Returns 0 on allocation
 * failure. */
static int config_parse_line(Config *c, const char *line, char **cur) {
    long at = config_line_insert(c, c->n);
    if (at < 0) return 0;
    ConfigLine *l = &c->lines[at];

    char *work = config_dup(line);
    if (!work) { config_line_remove(c, at); return 0; }
    char *t = config_trim(work);

    if (t[0] == '\0' || t[0] == '#' || t[0] == ';') {
        l->kind = CFG_RAW;
        l->raw = config_dup(line);
    } else if (t[0] == '[' && strchr(t, ']')) {
        char *close = strchr(t, ']');
        *close = '\0';
        char *name = config_trim(t + 1);
        l->kind = CFG_SECTION;
        l->section = config_dup(name);
        free(*cur);
        *cur = config_dup(name);
    } else if (strchr(t, '=')) {
        char *eq = strchr(t, '=');
        *eq = '\0';
        char *key = config_trim(t);
        char *val = config_trim(eq + 1);
        if (key[0] == '\0') {           /* "= v" names nothing: keep it as text */
            l->kind = CFG_RAW;
            l->raw = config_dup(line);
        } else {
            l->kind = CFG_PAIR;
            l->section = config_dup(*cur ? *cur : "");
            l->key = config_dup(key);
            l->value = config_dup(val);
        }
    } else {
        /* Not a comment, a header or a pair. Kept verbatim rather than
         * dropped, so a file this library does not fully understand still
         * survives a save intact. */
        l->kind = CFG_RAW;
        l->raw = config_dup(line);
    }
    free(work);
    return 1;
}

/* Split a whole-file buffer into lines. Handles LF and CRLF. */
static int config_parse(Config *c, char *text, size_t len) {
    char *cur = config_dup("");
    if (!cur) return 0;
    size_t start = 0;
    for (size_t i = 0; i <= len; i++) {
        if (i == len || text[i] == '\n') {
            if (i == len && i == start) break;   /* trailing newline: no extra line */
            size_t end = i;
            if (end > start && text[end - 1] == '\r') end--;
            char saved = text[end];
            text[end] = '\0';
            int ok = config_parse_line(c, text + start, &cur);
            text[end] = saved;
            if (!ok) { free(cur); return 0; }
            start = i + 1;
        }
    }
    free(cur);
    return 1;
}

/* --- lookup ----------------------------------------------------------- */

static long config_find_pair(Config *c, const char *section, const char *key) {
    for (long i = 0; i < c->n; i++)
        if (c->lines[i].kind == CFG_PAIR &&
            config_ieq(c->lines[i].section, section) &&
            config_ieq(c->lines[i].key, key))
            return i;
    return -1;
}

static int config_section_exists(Config *c, const char *section) {
    if (section[0] == '\0') return 1;   /* the unnamed section always exists */
    for (long i = 0; i < c->n; i++)
        if (c->lines[i].kind == CFG_SECTION && config_ieq(c->lines[i].section, section))
            return 1;
    return 0;
}

/* Does line `i` introduce a section name not seen before it? The unnamed
 * section is introduced by its first key, since it has no header. */
static const char *config_declares(Config *c, long i) {
    ConfigLine *l = &c->lines[i];
    const char *name = NULL;
    if (l->kind == CFG_SECTION) name = l->section;
    else if (l->kind == CFG_PAIR && l->section && l->section[0] == '\0') name = "";
    if (!name) return NULL;
    for (long j = 0; j < i; j++) {
        const char *prev = NULL;
        if (c->lines[j].kind == CFG_SECTION) prev = c->lines[j].section;
        else if (c->lines[j].kind == CFG_PAIR && c->lines[j].section &&
                 c->lines[j].section[0] == '\0') prev = "";
        if (prev && config_ieq(prev, name)) return NULL;   /* already counted */
    }
    return name;
}

/* Where a new key for `section` should go. Returns the insertion index; sets
 * *exists to 0 when the section has no header yet. */
static long config_insert_point(Config *c, const char *section, int *exists) {
    if (section[0] == '\0') {
        long last = -1, first_header = -1;
        for (long i = 0; i < c->n; i++) {
            if (c->lines[i].kind == CFG_SECTION) { first_header = i; break; }
            if (c->lines[i].kind == CFG_PAIR) last = i;
        }
        *exists = 1;
        if (last >= 0) return last + 1;
        return first_header >= 0 ? first_header : c->n;
    }
    long header = -1, last = -1;
    int in = 0;
    for (long i = 0; i < c->n; i++) {
        if (c->lines[i].kind == CFG_SECTION) {
            in = config_ieq(c->lines[i].section, section);
            if (in && header < 0) header = i;
            if (in) last = i;
        } else if (in && c->lines[i].kind == CFG_PAIR) {
            last = i;
        }
    }
    *exists = header >= 0;
    return *exists ? last + 1 : c->n;
}

/* --- error-slot helpers ----------------------------------------------- */

/* The "" text sentinel is a real runtime-owned string, so a program can hold a
 * failed result exactly like a successful one. */
static char *config_empty(void) {
    char *s = (char *)kn_malloc(1);
    if (s) s[0] = '\0';
    return s;
}

static char *config_text(const char *s) {
    if (!s) s = "";
    size_t n = strlen(s) + 1;
    char *o = (char *)kn_malloc((long)n);
    if (o) memcpy(o, s, n);
    return o;
}

static void config_oom(void) { kn_error_set_errno(ENOMEM, "allocate"); }

/* Resolve a handle. On failure the handle table has already written the slot. */
static Config *config_of(int32_t h) {
    return (Config *)kn_handle_resolve(h, KN_HK_CONFIG);
}

static const char *config_nz(const char *s) { return s ? s : ""; }

/* --- open / create / close / save ------------------------------------- */

/* config_open(text path) -> int : a handle, or 0 on failure.
 * A missing FILE is a failure, unlike a missing key. */
void config_open(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *path = config_nz(kn_arg_text(argv, 0));
    if (path[0] == '\0') {
        kn_error_set(KN_ERR_INVALID_ARG, "config_open: the path is empty");
        kn_ret_int(ret, 0);
        return;
    }
    FILE *f = fopen(path, "rb");
    int e = errno;
    if (!f) { kn_error_set_errno(e, "open"); kn_ret_int(ret, 0); return; }

    /* Read the whole file: settings files are small, and a single buffer is
     * what lets the parser see CRLF and a missing final newline uniformly. */
    size_t cap = 4096, len = 0;
    char *buf = (char *)malloc(cap);
    if (!buf) { fclose(f); config_oom(); kn_ret_int(ret, 0); return; }
    int read_errno = 0, bad = 0;
    for (;;) {
        size_t got = fread(buf + len, 1, cap - len, f);
        int e2 = errno;                  /* nothing may intervene */
        len += got;
        if (len < cap) {                 /* short read: end of file, or error */
            if (ferror(f)) { bad = 1; read_errno = e2; }
            break;
        }
        char *nb = (char *)realloc(buf, cap * 2);
        if (!nb) { free(buf); fclose(f); config_oom(); kn_ret_int(ret, 0); return; }
        buf = nb;
        cap *= 2;
    }
    fclose(f);
    if (bad) { free(buf); kn_error_set_errno(read_errno, "read"); kn_ret_int(ret, 0); return; }

    Config *c = (Config *)calloc(1, sizeof *c);
    if (!c) { free(buf); config_oom(); kn_ret_int(ret, 0); return; }
    c->path = config_dup(path);
    if (!c->path || !config_parse(c, buf, len)) {
        free(buf); config_free(c); config_oom(); kn_ret_int(ret, 0); return;
    }
    free(buf);

    int32_t h = kn_handle_new(KN_HK_CONFIG, c, config_free);
    if (h == 0) { config_free(c); kn_ret_int(ret, 0); return; }  /* slot set */
    kn_error_clear();
    kn_ret_int(ret, h);
}

/* config_create(text path) -> int : an empty document remembering `path`.
 * Nothing touches the disk until config_save. */
void config_create(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *path = config_nz(kn_arg_text(argv, 0));
    if (path[0] == '\0') {
        kn_error_set(KN_ERR_INVALID_ARG, "config_create: the path is empty");
        kn_ret_int(ret, 0);
        return;
    }
    Config *c = (Config *)calloc(1, sizeof *c);
    if (!c) { config_oom(); kn_ret_int(ret, 0); return; }
    c->path = config_dup(path);
    if (!c->path) { config_free(c); config_oom(); kn_ret_int(ret, 0); return; }

    int32_t h = kn_handle_new(KN_HK_CONFIG, c, config_free);
    if (h == 0) { config_free(c); kn_ret_int(ret, 0); return; }
    kn_error_clear();
    kn_ret_int(ret, h);
}

/* config_close(int h) -> bool. */
void config_close(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    int32_t ok = kn_handle_close(kn_arg_int(argv, 0), KN_HK_CONFIG);
    kn_ret_bool(ret, ok);           /* the table wrote the slot either way */
}

/* config_close_all() -> int : how many were closed. */
void config_close_all(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; (void)argv;
    kn_ret_int(ret, kn_handle_close_kind(KN_HK_CONFIG));
}

/* config_path(int h) -> text : the file this document reads and writes. */
void config_path(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    Config *c = config_of(kn_arg_int(argv, 0));
    if (!c) { kn_ret_text(ret, config_empty()); return; }
    kn_error_clear();
    kn_ret_text(ret, config_text(c->path));
}

/* config_save(int h) -> bool : write the document back, comments and all. */
void config_save(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    Config *c = config_of(kn_arg_int(argv, 0));
    if (!c) { kn_ret_bool(ret, 0); return; }

    FILE *f = fopen(c->path, "wb");
    int e = errno;
    if (!f) { kn_error_set_errno(e, "open for writing"); kn_ret_bool(ret, 0); return; }
    int bad = 0, we = 0;
    for (long i = 0; i < c->n; i++) {
        ConfigLine *l = &c->lines[i];
        int r;
        if (l->kind == CFG_SECTION)   r = fprintf(f, "[%s]\n", config_nz(l->section));
        else if (l->kind == CFG_PAIR) r = fprintf(f, "%s = %s\n", config_nz(l->key), config_nz(l->value));
        else                          r = fprintf(f, "%s\n", config_nz(l->raw));
        if (r < 0 && !bad) { we = errno; bad = 1; }   /* errno on the next line */
    }
    if (fclose(f) != 0 && !bad) { we = errno; bad = 1; }
    if (bad) { kn_error_set_errno(we, "write"); kn_ret_bool(ret, 0); return; }
    kn_error_clear();
    kn_ret_bool(ret, 1);
}

/* --- reading ---------------------------------------------------------- */

/* config_get(int h, text section, text key) -> text : "" when there is no such
 * key. A missing key is NOT a failure — config_has is the predicate — so the
 * slot is cleared and only a bad handle sets it. */
void config_get(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    Config *c = config_of(kn_arg_int(argv, 0));
    if (!c) { kn_ret_text(ret, config_empty()); return; }
    long i = config_find_pair(c, config_nz(kn_arg_text(argv, 1)), config_nz(kn_arg_text(argv, 2)));
    kn_error_clear();
    kn_ret_text(ret, i < 0 ? config_empty() : config_text(c->lines[i].value));
}

/* The typed readers take the value to use when the key is absent or does not
 * parse. Without it their answer would be unreadable: -1 or false would mean
 * "not set", "not a number" and "genuinely -1/false" all at once, and no
 * predicate can separate the middle case from the last. */

/* config_get_int(int h, text section, text key, int fallback) -> int */
void config_get_int(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    int32_t fallback = kn_arg_int(argv, 3);
    Config *c = config_of(kn_arg_int(argv, 0));
    if (!c) { kn_ret_int(ret, fallback); return; }
    long i = config_find_pair(c, config_nz(kn_arg_text(argv, 1)), config_nz(kn_arg_text(argv, 2)));
    kn_error_clear();
    if (i < 0) { kn_ret_int(ret, fallback); return; }
    const char *v = config_nz(c->lines[i].value);
    char *end = NULL;
    errno = 0;
    long n = strtol(v, &end, 10);
    if (end == v || *end != '\0' || errno == ERANGE) { kn_ret_int(ret, fallback); return; }
    kn_ret_int(ret, (int32_t)n);
}

/* config_get_double(int h, text section, text key, double fallback) -> double */
void config_get_double(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    double fallback = kn_arg_double(argv, 3);
    Config *c = config_of(kn_arg_int(argv, 0));
    if (!c) { kn_ret_double(ret, fallback); return; }
    long i = config_find_pair(c, config_nz(kn_arg_text(argv, 1)), config_nz(kn_arg_text(argv, 2)));
    kn_error_clear();
    if (i < 0) { kn_ret_double(ret, fallback); return; }
    const char *v = config_nz(c->lines[i].value);
    char *end = NULL;
    double d = strtod(v, &end);
    if (end == v || *end != '\0') { kn_ret_double(ret, fallback); return; }
    kn_ret_double(ret, d);
}

/* config_get_bool(int h, text section, text key, bool fallback) -> bool.
 * Accepts what people actually type: true/false, yes/no, on/off, 1/0. */
void config_get_bool(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    int32_t fallback = kn_arg_bool(argv, 3);
    Config *c = config_of(kn_arg_int(argv, 0));
    if (!c) { kn_ret_bool(ret, fallback); return; }
    long i = config_find_pair(c, config_nz(kn_arg_text(argv, 1)), config_nz(kn_arg_text(argv, 2)));
    kn_error_clear();
    if (i < 0) { kn_ret_bool(ret, fallback); return; }
    const char *v = config_nz(c->lines[i].value);
    if (config_ieq(v, "true") || config_ieq(v, "yes") || config_ieq(v, "on")  || config_ieq(v, "1"))
        { kn_ret_bool(ret, 1); return; }
    if (config_ieq(v, "false") || config_ieq(v, "no") || config_ieq(v, "off") || config_ieq(v, "0"))
        { kn_ret_bool(ret, 0); return; }
    kn_ret_bool(ret, fallback);
}

/* config_has(int h, text section, text key) -> bool. */
void config_has(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    Config *c = config_of(kn_arg_int(argv, 0));
    if (!c) { kn_ret_bool(ret, 0); return; }
    long i = config_find_pair(c, config_nz(kn_arg_text(argv, 1)), config_nz(kn_arg_text(argv, 2)));
    kn_error_clear();
    kn_ret_bool(ret, i >= 0);
}

/* config_has_section(int h, text section) -> bool. */
void config_has_section(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    Config *c = config_of(kn_arg_int(argv, 0));
    if (!c) { kn_ret_bool(ret, 0); return; }
    int yes = config_section_exists(c, config_nz(kn_arg_text(argv, 1)));
    kn_error_clear();
    kn_ret_bool(ret, yes);
}

/* --- writing ---------------------------------------------------------- */

/* Set section/key to `value`, creating the section header if needed.
 * Returns 1 on success, 0 on allocation failure (slot already set). */
static int config_put(Config *c, const char *section, const char *key, const char *value) {
    if (key[0] == '\0') {
        kn_error_set(KN_ERR_INVALID_ARG, "config_set: the key is empty");
        return 0;
    }
    long i = config_find_pair(c, section, key);
    if (i >= 0) {
        char *nv = config_dup(value);
        if (!nv) { config_oom(); return 0; }
        free(c->lines[i].value);
        c->lines[i].value = nv;         /* the line keeps its place, and any
                                         * comment above it keeps its meaning */
        return 1;
    }
    int exists = 0;
    long at = config_insert_point(c, section, &exists);
    if (!exists) {
        long h = config_line_insert(c, c->n);
        if (h < 0) { config_oom(); return 0; }
        c->lines[h].kind = CFG_SECTION;
        c->lines[h].section = config_dup(section);
        if (!c->lines[h].section) { config_line_remove(c, h); config_oom(); return 0; }
        at = c->n;
    }
    long p = config_line_insert(c, at);
    if (p < 0) { config_oom(); return 0; }
    ConfigLine *l = &c->lines[p];
    l->kind = CFG_PAIR;
    l->section = config_dup(section);
    l->key = config_dup(key);
    l->value = config_dup(value);
    if (!l->section || !l->key || !l->value) { config_line_remove(c, p); config_oom(); return 0; }
    return 1;
}

static void config_set_common(Kiln_Slot *ret, Kiln_Slot *argv, const char *value) {
    Config *c = config_of(kn_arg_int(argv, 0));
    if (!c) { kn_ret_bool(ret, 0); return; }
    if (!config_put(c, config_nz(kn_arg_text(argv, 1)), config_nz(kn_arg_text(argv, 2)), value)) {
        kn_ret_bool(ret, 0);
        return;
    }
    kn_error_clear();
    kn_ret_bool(ret, 1);
}

/* config_set(int h, text section, text key, text value) -> bool */
void config_set(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    config_set_common(ret, argv, config_nz(kn_arg_text(argv, 3)));
}

/* config_set_int(int h, text section, text key, int value) -> bool */
void config_set_int(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    char buf[32];
    snprintf(buf, sizeof buf, "%d", kn_arg_int(argv, 3));
    config_set_common(ret, argv, buf);
}

/* config_set_double(int h, text section, text key, double value) -> bool */
void config_set_double(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    char buf[64];
    /* %.17g round-trips a double exactly, so a value written here reads back as
     * the same number rather than one that has quietly lost its tail. */
    snprintf(buf, sizeof buf, "%.17g", kn_arg_double(argv, 3));
    config_set_common(ret, argv, buf);
}

/* config_set_bool(int h, text section, text key, bool value) -> bool */
void config_set_bool(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    config_set_common(ret, argv, kn_arg_bool(argv, 3) ? "true" : "false");
}

/* config_remove(int h, text section, text key) -> bool : true if it was there.
 * A key that was already absent is a genuine "no", not a failure — false with
 * error code 0. */
void config_remove(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    Config *c = config_of(kn_arg_int(argv, 0));
    if (!c) { kn_ret_bool(ret, 0); return; }
    long i = config_find_pair(c, config_nz(kn_arg_text(argv, 1)), config_nz(kn_arg_text(argv, 2)));
    if (i >= 0) config_line_remove(c, i);
    kn_error_clear();
    kn_ret_bool(ret, i >= 0);
}

/* config_remove_section(int h, text section) -> bool.
 * Removes the header and the section's keys, plus any comment lines BETWEEN
 * them — a note inside a section goes with it — while comments trailing after
 * the last key stay, because they usually introduce whatever comes next. */
void config_remove_section(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    Config *c = config_of(kn_arg_int(argv, 0));
    if (!c) { kn_ret_bool(ret, 0); return; }
    const char *section = config_nz(kn_arg_text(argv, 1));
    int removed = 0;

    if (section[0] == '\0') {
        for (long i = c->n - 1; i >= 0; i--)
            if (c->lines[i].kind == CFG_PAIR && c->lines[i].section &&
                c->lines[i].section[0] == '\0') { config_line_remove(c, i); removed = 1; }
        kn_error_clear();
        kn_ret_bool(ret, removed);
        return;
    }

    for (;;) {
        long header = -1;
        for (long i = 0; i < c->n; i++)
            if (c->lines[i].kind == CFG_SECTION && config_ieq(c->lines[i].section, section))
                { header = i; break; }
        if (header < 0) break;
        long last = header;
        for (long i = header + 1; i < c->n; i++) {
            if (c->lines[i].kind == CFG_SECTION) break;
            if (c->lines[i].kind == CFG_PAIR) last = i;
        }
        for (long i = last; i >= header; i--) config_line_remove(c, i);
        removed = 1;
    }
    kn_error_clear();
    kn_ret_bool(ret, removed);
}

/* --- collections: count + indexed accessor ---------------------------- */

/* config_section_count(int h) -> int : -1 on failure. The unnamed section —
 * keys written above any header — counts as "" when it has any keys. */
void config_section_count(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    Config *c = config_of(kn_arg_int(argv, 0));
    if (!c) { kn_ret_int(ret, -1); return; }
    int32_t n = 0;
    for (long i = 0; i < c->n; i++) if (config_declares(c, i)) n++;
    kn_error_clear();
    kn_ret_int(ret, n);
}

/* config_section_at(int h, int index) -> text : "" when out of range. */
void config_section_at(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    Config *c = config_of(kn_arg_int(argv, 0));
    if (!c) { kn_ret_text(ret, config_empty()); return; }
    int32_t want = kn_arg_int(argv, 1), seen = 1;
    kn_error_clear();
    if (want >= 1) {
        for (long i = 0; i < c->n; i++) {
            const char *name = config_declares(c, i);
            if (!name) continue;
            if (seen == want) { kn_ret_text(ret, config_text(name)); return; }
            seen++;
        }
    }
    kn_ret_text(ret, config_empty());
}

/* config_key_count(int h, text section) -> int : -1 on failure, 0 for a
 * section that is not there — asking about an absent section is a fair
 * question with the answer "no keys". */
void config_key_count(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    Config *c = config_of(kn_arg_int(argv, 0));
    if (!c) { kn_ret_int(ret, -1); return; }
    const char *section = config_nz(kn_arg_text(argv, 1));
    int32_t n = 0;
    for (long i = 0; i < c->n; i++)
        if (c->lines[i].kind == CFG_PAIR && config_ieq(c->lines[i].section, section)) n++;
    kn_error_clear();
    kn_ret_int(ret, n);
}

/* config_key_at(int h, text section, int index) -> text : "" out of range. */
void config_key_at(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    Config *c = config_of(kn_arg_int(argv, 0));
    if (!c) { kn_ret_text(ret, config_empty()); return; }
    const char *section = config_nz(kn_arg_text(argv, 1));
    int32_t want = kn_arg_int(argv, 2), seen = 1;
    kn_error_clear();
    if (want >= 1) {
        for (long i = 0; i < c->n; i++) {
            if (c->lines[i].kind != CFG_PAIR || !config_ieq(c->lines[i].section, section)) continue;
            if (seen == want) { kn_ret_text(ret, config_text(c->lines[i].key)); return; }
            seen++;
        }
    }
    kn_ret_text(ret, config_empty());
}
