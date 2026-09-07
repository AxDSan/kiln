/* Tables, the components that own them, and the commands that fill them.
 *
 * Bookkeeping here is malloc'd rather than kn_malloc'd: a table is rewritten
 * many times over a program's life and the runtime's allocator is a bump
 * allocator freed at exit, so every `grid_set_cell` would leak its previous
 * value until the window closed.  What crosses into the program — a cell read
 * back, the rows as one text — is copied through kn_malloc like every other
 * text result, so the program may hold it.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "kiln_abi.h"
#include "ui_data.h"

typedef struct {
    char  **cells;
    int32_t count;
} Row;

struct UiTable {
    char  **header;
    int32_t header_count;
    Row    *rows;
    int32_t row_count;
    int32_t row_cap;
    int32_t version;
};

struct UiEntry {
    int32_t  kind;
    char    *name;
    char    *bind;
    UiTable *own;
};

/* Entries are never removed: a component lives as long as the program, and
 * the count is the number written in the source. */
static UiEntry **g_entries;
static int32_t   g_entry_count;
static int32_t   g_entry_cap;

static char *dup_text(const char *s) {
    size_t n = strlen(s ? s : "");
    char *out = (char *)malloc(n + 1);
    if (out) memcpy(out, s ? s : "", n + 1);
    return out;
}

static char *kn_dup(const char *s, size_t n) {
    char *out = (char *)kn_malloc((long)n + 1);
    if (out) { memcpy(out, s, n); out[n] = 0; }
    return out;
}

/* --- entries ---------------------------------------------------------- */

UiEntry *ui_entry_new(int32_t kind) {
    UiEntry *e = (UiEntry *)calloc(1, sizeof *e);
    if (!e) return NULL;
    e->kind = kind;
    e->name = dup_text("");
    e->bind = dup_text("");
    e->own  = (UiTable *)calloc(1, sizeof *e->own);
    if (g_entry_count == g_entry_cap) {
        int32_t cap = g_entry_cap ? g_entry_cap * 2 : 8;
        UiEntry **grown = (UiEntry **)realloc(g_entries, (size_t)cap * sizeof *grown);
        if (!grown) return e;
        g_entries = grown;
        g_entry_cap = cap;
    }
    g_entries[g_entry_count++] = e;
    return e;
}

void ui_entry_set_name(UiEntry *e, const char *name) {
    if (!e) return;
    free(e->name);
    e->name = dup_text(name);
}

const char *ui_entry_name(const UiEntry *e) { return e ? e->name : ""; }

void ui_entry_set_bind(UiEntry *e, const char *datasource_name) {
    if (!e) return;
    free(e->bind);
    e->bind = dup_text(datasource_name);
}

const char *ui_entry_bind(const UiEntry *e) { return e ? e->bind : ""; }

UiEntry *ui_entry_find(int32_t kind, const char *name) {
    if (!name || !*name) return NULL;
    for (int32_t i = 0; i < g_entry_count; i++) {
        UiEntry *e = g_entries[i];
        if (e->kind == kind && strcmp(e->name, name) == 0) return e;
    }
    return NULL;
}

UiTable *ui_entry_table(UiEntry *e) {
    if (!e) return NULL;
    if (e->kind == UI_ENTRY_GRID && *e->bind) {
        UiEntry *ds = ui_entry_find(UI_ENTRY_DATASOURCE, e->bind);
        if (ds) return ds->own;
    }
    return e->own;
}

/* --- tables ----------------------------------------------------------- */

int32_t ui_table_version(const UiTable *t) { return t ? t->version : 0; }
int32_t ui_table_row_count(const UiTable *t) { return t ? t->row_count : 0; }

int32_t ui_table_column_count(const UiTable *t) {
    if (!t) return 0;
    if (t->header_count > 0) return t->header_count;
    int32_t widest = 0;
    for (int32_t i = 0; i < t->row_count; i++) {
        if (t->rows[i].count > widest) widest = t->rows[i].count;
    }
    return widest;
}

static void free_cells(char **cells, int32_t count) {
    for (int32_t i = 0; i < count; i++) free(cells[i]);
    free(cells);
}

/* Split on one delimiter into fresh strings.  A trailing delimiter is the
 * separator after the last field rather than an empty field after it, which
 * is what makes a text ending in a newline the same rows as one that does not.
 * An empty text is no fields at all. */
static char **split(const char *text, char sep, int32_t *count) {
    *count = 0;
    if (!text || !*text) return NULL;
    int32_t n = 1;
    for (const char *c = text; *c; c++) if (*c == sep) n++;
    if (text[strlen(text) - 1] == sep) n--;
    char **out = (char **)calloc((size_t)n, sizeof *out);
    if (!out) return NULL;
    const char *start = text;
    for (int32_t i = 0; i < n; i++) {
        const char *end = strchr(start, sep);
        size_t len = end ? (size_t)(end - start) : strlen(start);
        out[i] = (char *)malloc(len + 1);
        if (out[i]) { memcpy(out[i], start, len); out[i][len] = 0; }
        start = end ? end + 1 : start + len;
    }
    *count = n;
    return out;
}

static char *join(char **parts, int32_t count, char sep) {
    size_t total = 0;
    for (int32_t i = 0; i < count; i++) total += strlen(parts[i]) + 1;
    char *out = (char *)kn_malloc((long)total + 1);
    if (!out) return NULL;
    char *p = out;
    for (int32_t i = 0; i < count; i++) {
        if (i) *p++ = sep;
        size_t len = strlen(parts[i]);
        memcpy(p, parts[i], len);
        p += len;
    }
    *p = 0;
    return out;
}

void ui_table_set_columns(UiTable *t, const char *tabbed) {
    if (!t) return;
    free_cells(t->header, t->header_count);
    t->header = split(tabbed, '\t', &t->header_count);
    t->version++;
}

char *ui_table_columns(const UiTable *t) {
    if (!t) return kn_dup("", 0);
    return join(t->header, t->header_count, '\t');
}

const char *ui_table_column(const UiTable *t, int32_t col) {
    if (!t || col < 1 || col > t->header_count) return "";
    return t->header[col - 1];
}

void ui_table_clear(UiTable *t) {
    if (!t) return;
    for (int32_t i = 0; i < t->row_count; i++) free_cells(t->rows[i].cells, t->rows[i].count);
    t->row_count = 0;
    t->version++;
}

int32_t ui_table_add_row(UiTable *t, const char *tabbed) {
    if (!t) return 0;
    if (t->row_count == t->row_cap) {
        int32_t cap = t->row_cap ? t->row_cap * 2 : 16;
        Row *grown = (Row *)realloc(t->rows, (size_t)cap * sizeof *grown);
        if (!grown) return 0;
        t->rows = grown;
        t->row_cap = cap;
    }
    Row *r = &t->rows[t->row_count];
    r->cells = split(tabbed, '\t', &r->count);
    t->row_count++;
    t->version++;
    return t->row_count;
}

void ui_table_set_rows(UiTable *t, const char *text) {
    if (!t) return;
    ui_table_clear(t);
    int32_t n = 0;
    char **lines = split(text, '\n', &n);
    for (int32_t i = 0; i < n; i++) ui_table_add_row(t, lines[i]);
    free_cells(lines, n);
}

char *ui_table_rows(const UiTable *t) {
    if (!t || t->row_count == 0) return kn_dup("", 0);
    /* Two passes so the text is built once at its final size rather than
     * grown a row at a time through the bump allocator. */
    size_t total = 0;
    for (int32_t i = 0; i < t->row_count; i++) {
        const Row *r = &t->rows[i];
        for (int32_t c = 0; c < r->count; c++) total += strlen(r->cells[c]) + 1;
        total += 1;
    }
    char *out = (char *)kn_malloc((long)total + 1);
    if (!out) return NULL;
    char *p = out;
    for (int32_t i = 0; i < t->row_count; i++) {
        const Row *r = &t->rows[i];
        if (i) *p++ = '\n';
        for (int32_t c = 0; c < r->count; c++) {
            if (c) *p++ = '\t';
            size_t len = strlen(r->cells[c]);
            memcpy(p, r->cells[c], len);
            p += len;
        }
    }
    *p = 0;
    return out;
}

const char *ui_table_cell(const UiTable *t, int32_t row, int32_t col) {
    if (!t || row < 1 || row > t->row_count || col < 1) return NULL;
    const Row *r = &t->rows[row - 1];
    return col <= r->count ? r->cells[col - 1] : "";
}

int32_t ui_table_set_cell(UiTable *t, int32_t row, int32_t col, const char *value) {
    if (!t || row < 1 || row > t->row_count || col < 1) return 0;
    Row *r = &t->rows[row - 1];
    /* Writing past the row's end widens it: a row added as "Ada" and then
     * given an age is the common way a program builds a row up. */
    if (col > r->count) {
        char **grown = (char **)realloc(r->cells, (size_t)col * sizeof *grown);
        if (!grown) return 0;
        r->cells = grown;
        for (int32_t i = r->count; i < col; i++) r->cells[i] = dup_text("");
        r->count = col;
    }
    free(r->cells[col - 1]);
    r->cells[col - 1] = dup_text(value);
    t->version++;
    return 1;
}

/* --- the commands ------------------------------------------------------
 *
 * Both families take the component's `name`, because a bare component id is
 * not an expression and so cannot be an argument (ui_libinfo.c).  A grid's
 * commands reach the table it SHOWS — the bound datasource's when there is
 * one — so filling through the grid and filling through its datasource put
 * rows in the same place, and a program that starts unbound keeps working
 * when a datasource is wired in later.
 */

static UiTable *table_named(int32_t kind, const char *name) {
    UiEntry *e = ui_entry_find(kind, name);
    if (e) return ui_entry_table(e);
    char msg[128];
    snprintf(msg, sizeof msg, "no %s named \"%s\"",
             kind == UI_ENTRY_GRID ? "grid" : "datasource", name ? name : "");
    kn_error_set(KN_ERR_INVALID_ARG, msg);
    return NULL;
}

static void cmd_clear(int32_t kind, Kiln_Slot *ret, Kiln_Slot *argv) {
    UiTable *t = table_named(kind, kn_arg_text(argv, 0));
    if (!t) { kn_ret_bool(ret, 0); return; }
    ui_table_clear(t);
    kn_error_clear();
    kn_ret_bool(ret, 1);
}

static void cmd_add_row(int32_t kind, Kiln_Slot *ret, Kiln_Slot *argv) {
    UiTable *t = table_named(kind, kn_arg_text(argv, 0));
    if (!t) { kn_ret_int(ret, 0); return; }
    kn_error_clear();
    kn_ret_int(ret, ui_table_add_row(t, kn_arg_text(argv, 1)));
}

static void cmd_cell(int32_t kind, Kiln_Slot *ret, Kiln_Slot *argv) {
    UiTable *t = table_named(kind, kn_arg_text(argv, 0));
    const char *cell = t ? ui_table_cell(t, kn_arg_int(argv, 1), kn_arg_int(argv, 2)) : NULL;
    if (!cell) {
        if (t) {
            char msg[96];
            snprintf(msg, sizeof msg, "row %d is outside a table of %d row(s)",
                     (int)kn_arg_int(argv, 1), (int)ui_table_row_count(t));
            kn_error_set(KN_ERR_OUT_OF_RANGE, msg);
        }
        kn_ret_text(ret, kn_dup("", 0));
        return;
    }
    kn_error_clear();
    kn_ret_text(ret, kn_dup(cell, strlen(cell)));
}

static void cmd_set_cell(int32_t kind, Kiln_Slot *ret, Kiln_Slot *argv) {
    UiTable *t = table_named(kind, kn_arg_text(argv, 0));
    if (!t) { kn_ret_bool(ret, 0); return; }
    if (!ui_table_set_cell(t, kn_arg_int(argv, 1), kn_arg_int(argv, 2), kn_arg_text(argv, 3))) {
        char msg[96];
        snprintf(msg, sizeof msg, "row %d, column %d is outside a table of %d row(s)",
                 (int)kn_arg_int(argv, 1), (int)kn_arg_int(argv, 2), (int)ui_table_row_count(t));
        kn_error_set(KN_ERR_OUT_OF_RANGE, msg);
        kn_ret_bool(ret, 0);
        return;
    }
    kn_error_clear();
    kn_ret_bool(ret, 1);
}

static void cmd_row_count(int32_t kind, Kiln_Slot *ret, Kiln_Slot *argv) {
    UiTable *t = table_named(kind, kn_arg_text(argv, 0));
    if (!t) { kn_ret_int(ret, -1); return; }
    kn_error_clear();
    kn_ret_int(ret, ui_table_row_count(t));
}

#define UI_CMD(name, kind, fn) \
    void name(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) { (void)argc; fn(kind, ret, argv); }

UI_CMD(ui_grid_clear,           UI_ENTRY_GRID,       cmd_clear)
UI_CMD(ui_grid_add_row,         UI_ENTRY_GRID,       cmd_add_row)
UI_CMD(ui_grid_cell,            UI_ENTRY_GRID,       cmd_cell)
UI_CMD(ui_grid_set_cell,        UI_ENTRY_GRID,       cmd_set_cell)
UI_CMD(ui_grid_row_count,       UI_ENTRY_GRID,       cmd_row_count)
UI_CMD(ui_datasource_clear,     UI_ENTRY_DATASOURCE, cmd_clear)
UI_CMD(ui_datasource_add_row,   UI_ENTRY_DATASOURCE, cmd_add_row)
UI_CMD(ui_datasource_cell,      UI_ENTRY_DATASOURCE, cmd_cell)
UI_CMD(ui_datasource_set_cell,  UI_ENTRY_DATASOURCE, cmd_set_cell)
UI_CMD(ui_datasource_row_count, UI_ENTRY_DATASOURCE, cmd_row_count)
