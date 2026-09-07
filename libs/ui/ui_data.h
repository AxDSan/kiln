/* The rows behind a grid, and the datasource that can hold them instead.
 *
 * Plain C with no substrate in it, so a datasource is a thing a program can
 * fill from `main` before a window exists, and so a later `database` kit can
 * hand a query result to one without knowing what will draw it.  The RmlUi
 * backend reads tables through this header and never writes them; the writes
 * come from the form's properties and from the `grid_*` / `datasource_*`
 * commands, and a change counter is how the backend learns something moved.
 *
 * A table is rows of text cells with an optional header.  The program sees it
 * as ONE text — a newline between rows, a tab between cells — because a
 * component property value is a literal and a bare component id is not an
 * expression (see ui_libinfo.c), so that text is the only shape a form and a
 * running subroutine can both write.  A cell can therefore hold neither a tab
 * nor a newline; there is deliberately no escaping, because an escape the
 * inspector shows and the program has to spell is worse than a stated limit.
 */
#ifndef KILN_UI_DATA_H
#define KILN_UI_DATA_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct UiTable UiTable;
typedef struct UiEntry UiEntry;

enum { UI_ENTRY_GRID = 1, UI_ENTRY_DATASOURCE = 2 };

/* An entry is one component: its name, its own table, and for a grid the
 * name of the datasource it would rather show.  Kinds are separate namespaces
 * — a grid and a datasource may share a name — and both are looked up by the
 * commands with the name a program can spell. */
UiEntry    *ui_entry_new(int32_t kind);
void        ui_entry_set_name(UiEntry *e, const char *name);
const char *ui_entry_name(const UiEntry *e);
void        ui_entry_set_bind(UiEntry *e, const char *datasource_name);
const char *ui_entry_bind(const UiEntry *e);
UiEntry    *ui_entry_find(int32_t kind, const char *name);

/* The table this entry shows: the bound datasource's whenever one by that
 * name exists, else its own.  Resolved by name on EVERY call, because a form
 * is built before the module-level components it names, so at the moment
 * `bind` is written the datasource is not there yet. */
UiTable *ui_entry_table(UiEntry *e);

/* Bumped on every write.  The backend compares it against what it last drew
 * rather than being told — so filling a table from `main`, from a handler, or
 * from a datasource three grids share all reach the screen the same way. */
int32_t ui_table_version(const UiTable *t);

int32_t ui_table_row_count(const UiTable *t);
int32_t ui_table_column_count(const UiTable *t);

/* Header names, tab-separated.  The header decides the column count when
 * there is one; otherwise the widest row does. */
void  ui_table_set_columns(UiTable *t, const char *tabbed);
char *ui_table_columns(const UiTable *t);      /* runtime-owned copy */
const char *ui_table_column(const UiTable *t, int32_t col);   /* "" past the end */

/* All rows as one text, and the reverse.  The copy is runtime-owned so the
 * program may hold it. */
void  ui_table_set_rows(UiTable *t, const char *text);
char *ui_table_rows(const UiTable *t);

void    ui_table_clear(UiTable *t);                          /* rows only */
int32_t ui_table_add_row(UiTable *t, const char *tabbed);    /* -> its position */
/* Positions count from 1.  A cell a row does not reach reads as "", so a
 * ragged table is readable; NULL only for a row that does not exist. */
const char *ui_table_cell(const UiTable *t, int32_t row, int32_t col);
int32_t     ui_table_set_cell(UiTable *t, int32_t row, int32_t col, const char *value);

#ifdef __cplusplus
}
#endif
#endif /* KILN_UI_DATA_H */
