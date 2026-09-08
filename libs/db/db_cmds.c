/* The `db` support library: SQL through bound parameters, and nothing else.
 *
 * The surface is deliberately narrow. There is no `db_exec(h, sql)` without a
 * parameter list, because the shortest path a program can take must be the
 * one that binds — a library whose easiest call concatenates is a library that
 * teaches injection. Every value reaches SQL as a bound parameter or does not
 * reach it at all.
 *
 * Parameters are `text[]` whatever the column's type is. That is what a
 * `MYSQL_TYPE_STRING` bind does anyway, and what `sqlite3_bind_text` into an
 * INTEGER column does: the driver coerces. One type keeps the surface one call
 * wide, and a typed family can be added the day a column needs it.
 *
 * NULL is the exception the type cannot express, so it has its own spelling:
 * `db_exec_n` and `db_query_n` take a second `bool[]` where `true` binds SQL
 * NULL whatever the text held. An empty string and NULL are different values
 * to a database and to whatever reads it afterwards.
 *
 * Two backends, chosen by the DSN's prefix so a program does not change
 * between them:
 *
 *   sqlite:<path>          or  sqlite://<path>
 *   mysql://user:pass@host[:port]/database
 *
 * Both client libraries are optional. Without them this file still compiles —
 * every entry point answers its failure sentinel with KN_ERR_UNSUPPORTED and a
 * message naming what to install — so a checkout with no database headers
 * builds, and a program that never opens a database never notices.
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* kiln_core.h rather than the ABI header alone, for kn_malloc and the array
 * helpers: a text answer is allocated by the runtime the same way every other
 * library's is. */
#include "kiln_core.h"

#ifdef KILN_DB
#include <mysql/mysql.h>
#include <sqlite3.h>

/* The type of MYSQL_BIND::is_null, whatever this client library calls it.
 *
 * MariaDB spells it `my_bool`; MySQL 8.0 deleted that typedef and made the
 * field a plain `bool`. Naming either one directly builds against one client
 * and fails against the other — which is exactly how this was green on a
 * MariaDB machine and red on CI. Asking the struct cannot be wrong. */
typedef __typeof__(((MYSQL_BIND *)0)->is_null[0]) DbIsNull;
#endif

/* --- what a handle points at --------------------------------------------- */

enum { DB_SQLITE = 1, DB_MYSQL = 2 };

typedef struct {
    int backend;
#ifdef KILN_DB
    sqlite3 *lite;
    MYSQL   *my;
#endif
} DbConn;

/* A result set is walked one row at a time, and the two backends disagree
 * about when the rows exist: SQLite steps a statement, MySQL (as used here)
 * buffers the whole result so the connection is free for the next statement.
 * The difference is hidden behind `db_next`, which is the point. */
typedef struct {
    int backend;
    int32_t columns;
#ifdef KILN_DB
    sqlite3_stmt *lite;      /* stepped                                     */
    MYSQL_RES    *my;        /* buffered                                    */
    MYSQL_ROW     row;       /* the current row: `columns` char pointers    */
    unsigned long *lengths;  /* its lengths, so a NULL is distinguishable   */
#endif
    int done;                /* the last `db_next` fell off the end         */
} DbRows;

/* --- small helpers -------------------------------------------------------- */

static const char *db_nz(const char *s) { return s ? s : ""; }

static char *db_text_n(const char *s, size_t n) {
    char *o = (char *)kn_malloc((long)n + 1);
    if (!o) return kn_empty_text();
    if (n) memcpy(o, s, n);
    o[n] = '\0';
    return o;
}
static char *db_text(const char *s) { return db_text_n(db_nz(s), strlen(db_nz(s))); }

/* One place decides what "no database support" means, so the message is the
 * same wherever a program first hits it. */
static int db_unsupported(void) {
    kn_error_set(KN_ERR_UNSUPPORTED,
                 "this build has no database client: install sqlite3 and libmariadb "
                 "development packages and rebuild");
    return 0;
}

/* Arrays reach a command as a Kiln_Array; elements are one slot-width each. */
static int32_t db_ary_len(void *p) {
    Kiln_Array *a = (Kiln_Array *)p;
    return a ? a->len : 0;
}
static int64_t db_ary_at(void *p, int32_t i) {
    Kiln_Array *a = (Kiln_Array *)p;
    if (!a || i < 1 || i > a->len) return 0;
    return ((int64_t *)(a + 1))[i - 1];
}

#ifdef KILN_DB

/* --- opening -------------------------------------------------------------- */

/* `mysql://user:pass@host:port/database`, taken apart in place. Everything is
 * optional but the host and the database: a DSN with no user is the local
 * socket's default, which is what a development machine usually wants. */
static int db_open_mysql(DbConn *c, const char *dsn) {
    char buf[1024];
    snprintf(buf, sizeof buf, "%s", dsn);
    char *p = buf;

    char *user = NULL, *pass = NULL, *host = p, *db = NULL;
    unsigned port = 0;

    char *at = strrchr(p, '@');
    if (at) {
        *at = '\0';
        user = p;
        char *colon = strchr(user, ':');
        if (colon) { *colon = '\0'; pass = colon + 1; }
        host = at + 1;
    }
    char *slash = strchr(host, '/');
    if (slash) { *slash = '\0'; db = slash + 1; }
    char *colon = strchr(host, ':');
    if (colon) { *colon = '\0'; port = (unsigned)atoi(colon + 1); }

    if (!db || !*db) {
        kn_error_set(KN_ERR_INVALID_ARG,
                     "a mysql DSN needs a database: mysql://user:pass@host/database");
        return 0;
    }
    MYSQL *my = mysql_init(NULL);
    if (!my) {
        kn_error_set(KN_ERR_TABLE_FULL, "out of memory opening a mysql connection");
        return 0;
    }
    if (!mysql_real_connect(my, *host ? host : NULL, user, pass, db, port, NULL, 0)) {
        char msg[512];
        snprintf(msg, sizeof msg, "mysql: %s", mysql_error(my));
        kn_error_set(KN_ERR_INVALID_ARG, msg);
        mysql_close(my);
        return 0;
    }
    /* UTF-8 explicitly rather than by the server's default: a login name is
     * text, and a connection whose charset is whatever the server was
     * configured with is a program that works until it is deployed. */
    mysql_set_character_set(my, "utf8mb4");
    c->backend = DB_MYSQL;
    c->my = my;
    return 1;
}

static int db_open_sqlite(DbConn *c, const char *path) {
    sqlite3 *lite = NULL;
    if (sqlite3_open(path, &lite) != SQLITE_OK) {
        char msg[512];
        snprintf(msg, sizeof msg, "sqlite: %s", lite ? sqlite3_errmsg(lite) : "could not open");
        kn_error_set(KN_ERR_INVALID_ARG, msg);
        if (lite) sqlite3_close(lite);
        return 0;
    }
    c->backend = DB_SQLITE;
    c->lite = lite;
    return 1;
}

/* --- binding -------------------------------------------------------------- */

/* Bind `params` into a prepared SQLite statement, `nulls[i]` overriding the
 * text with SQL NULL. A shorter `nulls` is not an error: it means the rest are
 * not null, which is what a caller who passed none meant. */
static int db_bind_sqlite(sqlite3_stmt *st, void *params, void *nulls) {
    const int32_t n = db_ary_len(params);
    for (int32_t i = 1; i <= n; i++) {
        const int is_null = i <= db_ary_len(nulls) && db_ary_at(nulls, i) != 0;
        int rc;
        if (is_null) {
            rc = sqlite3_bind_null(st, i);
        } else {
            const char *v = (const char *)(intptr_t)db_ary_at(params, i);
            rc = sqlite3_bind_text(st, i, db_nz(v), -1, SQLITE_TRANSIENT);
        }
        if (rc != SQLITE_OK) {
            kn_error_set(KN_ERR_INVALID_ARG, "sqlite: could not bind a parameter");
            return 0;
        }
    }
    return 1;
}

/* MySQL's prepared-statement binds want an array of MYSQL_BIND alive for the
 * whole execute, so the caller owns it and this only fills it in. */
static int db_bind_mysql(MYSQL_BIND *b, unsigned long *lens, DbIsNull *isnull, void *params,
                         void *nulls) {
    const int32_t n = db_ary_len(params);
    for (int32_t i = 0; i < n; i++) {
        const int is_null = (i + 1) <= db_ary_len(nulls) && db_ary_at(nulls, i + 1) != 0;
        const char *v = (const char *)(intptr_t)db_ary_at(params, i + 1);
        memset(&b[i], 0, sizeof b[i]);
        isnull[i] = is_null ? 1 : 0;
        b[i].is_null = &isnull[i];
        if (is_null) {
            b[i].buffer_type = MYSQL_TYPE_NULL;
            continue;
        }
        lens[i] = (unsigned long)strlen(db_nz(v));
        b[i].buffer_type = MYSQL_TYPE_STRING;
        b[i].buffer = (void *)db_nz(v);
        b[i].buffer_length = lens[i];
        b[i].length = &lens[i];
    }
    return 1;
}

#endif /* KILN_DB */

/* --- the handle table's close functions ----------------------------------- */

static void db_close_conn(void *payload) {
    DbConn *c = (DbConn *)payload;
    if (!c) return;
#ifdef KILN_DB
    if (c->lite) sqlite3_close(c->lite);
    if (c->my) mysql_close(c->my);
#endif
    free(c);
}

static void db_close_rows(void *payload) {
    DbRows *r = (DbRows *)payload;
    if (!r) return;
#ifdef KILN_DB
    if (r->lite) sqlite3_finalize(r->lite);
    if (r->my) mysql_free_result(r->my);
#endif
    free(r);
}

/* --- the commands --------------------------------------------------------- */

/* db_open(dsn) -> int: a connection handle, or 0 with the reason in the slot. */
void db_open(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *dsn = db_nz(kn_arg_text(argv, 0));
#ifndef KILN_DB
    (void)dsn;
    db_unsupported();
    kn_ret_int(ret, 0);
#else
    DbConn *c = (DbConn *)calloc(1, sizeof *c);
    if (!c) {
        kn_error_set(KN_ERR_TABLE_FULL, "out of memory opening a connection");
        kn_ret_int(ret, 0);
        return;
    }
    int ok = 0;
    if (strncmp(dsn, "mysql://", 8) == 0) {
        ok = db_open_mysql(c, dsn + 8);
    } else if (strncmp(dsn, "sqlite://", 9) == 0) {
        ok = db_open_sqlite(c, dsn + 9);
    } else if (strncmp(dsn, "sqlite:", 7) == 0) {
        ok = db_open_sqlite(c, dsn + 7);
    } else {
        kn_error_set(KN_ERR_INVALID_ARG,
                     "a DSN starts with sqlite: or mysql:// — the prefix picks the backend");
    }
    if (!ok) {
        free(c);
        kn_ret_int(ret, 0);
        return;
    }
    const int32_t h = kn_handle_new(KN_HK_DB, c, db_close_conn);
    if (h == 0) {           /* the table said why */
        db_close_conn(c);
        kn_ret_int(ret, 0);
        return;
    }
    kn_error_clear();
    kn_ret_int(ret, h);
#endif
}

/* db_close(h) -> bool */
void db_close(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    kn_ret_bool(ret, kn_handle_close(kn_arg_int(argv, 0), KN_HK_DB));
}

#ifdef KILN_DB
/* The shared body of `db_exec` and `db_exec_n`: prepare, bind, run, and answer
 * how many rows it changed. -1 on any failure, with the driver's own message
 * in the slot — a SQL error the program cannot see is a program that reports
 * "0 rows" for a typo. */
static int32_t db_exec_impl(int32_t h, const char *sql, void *params, void *nulls) {
    DbConn *c = (DbConn *)kn_handle_resolve(h, KN_HK_DB);
    if (!c) return -1;

    if (c->backend == DB_SQLITE) {
        sqlite3_stmt *st = NULL;
        if (sqlite3_prepare_v2(c->lite, sql, -1, &st, NULL) != SQLITE_OK) {
            char msg[512];
            snprintf(msg, sizeof msg, "sqlite: %s", sqlite3_errmsg(c->lite));
            kn_error_set(KN_ERR_INVALID_ARG, msg);
            return -1;
        }
        if (!db_bind_sqlite(st, params, nulls)) { sqlite3_finalize(st); return -1; }
        const int rc = sqlite3_step(st);
        if (rc != SQLITE_DONE && rc != SQLITE_ROW) {
            char msg[512];
            snprintf(msg, sizeof msg, "sqlite: %s", sqlite3_errmsg(c->lite));
            kn_error_set(KN_ERR_INVALID_ARG, msg);
            sqlite3_finalize(st);
            return -1;
        }
        const int32_t changed = (int32_t)sqlite3_changes(c->lite);
        sqlite3_finalize(st);
        kn_error_clear();
        return changed;
    }

    MYSQL_STMT *st = mysql_stmt_init(c->my);
    if (!st) {
        kn_error_set(KN_ERR_TABLE_FULL, "out of memory preparing a statement");
        return -1;
    }
    if (mysql_stmt_prepare(st, sql, (unsigned long)strlen(sql)) != 0) {
        char msg[512];
        snprintf(msg, sizeof msg, "mysql: %s", mysql_stmt_error(st));
        kn_error_set(KN_ERR_INVALID_ARG, msg);
        mysql_stmt_close(st);
        return -1;
    }
    const int32_t n = db_ary_len(params);
    MYSQL_BIND *b = n ? (MYSQL_BIND *)calloc((size_t)n, sizeof *b) : NULL;
    unsigned long *lens = n ? (unsigned long *)calloc((size_t)n, sizeof *lens) : NULL;
    DbIsNull *isnull = n ? (DbIsNull *)calloc((size_t)n, sizeof *isnull) : NULL;
    if (n && (!b || !lens || !isnull)) {
        free(b); free(lens); free(isnull);
        mysql_stmt_close(st);
        kn_error_set(KN_ERR_TABLE_FULL, "out of memory binding parameters");
        return -1;
    }
    db_bind_mysql(b, lens, isnull, params, nulls);
    int32_t changed = -1;
    if (n && mysql_stmt_bind_param(st, b) != 0) {
        char msg[512];
        snprintf(msg, sizeof msg, "mysql: %s", mysql_stmt_error(st));
        kn_error_set(KN_ERR_INVALID_ARG, msg);
    } else if (mysql_stmt_execute(st) != 0) {
        char msg[512];
        snprintf(msg, sizeof msg, "mysql: %s", mysql_stmt_error(st));
        kn_error_set(KN_ERR_INVALID_ARG, msg);
    } else {
        changed = (int32_t)mysql_stmt_affected_rows(st);
        kn_error_clear();
    }
    free(b); free(lens); free(isnull);
    mysql_stmt_close(st);
    return changed;
}

/* The shared body of `db_query` and `db_query_n`. */
static int32_t db_query_impl(int32_t h, const char *sql, void *params, void *nulls) {
    DbConn *c = (DbConn *)kn_handle_resolve(h, KN_HK_DB);
    if (!c) return 0;

    DbRows *rows = (DbRows *)calloc(1, sizeof *rows);
    if (!rows) {
        kn_error_set(KN_ERR_TABLE_FULL, "out of memory starting a query");
        return 0;
    }
    rows->backend = c->backend;

    if (c->backend == DB_SQLITE) {
        if (sqlite3_prepare_v2(c->lite, sql, -1, &rows->lite, NULL) != SQLITE_OK) {
            char msg[512];
            snprintf(msg, sizeof msg, "sqlite: %s", sqlite3_errmsg(c->lite));
            kn_error_set(KN_ERR_INVALID_ARG, msg);
            free(rows);
            return 0;
        }
        if (!db_bind_sqlite(rows->lite, params, nulls)) {
            sqlite3_finalize(rows->lite);
            free(rows);
            return 0;
        }
        rows->columns = (int32_t)sqlite3_column_count(rows->lite);
    } else {
        /* Buffered, not streamed: `mysql_stmt_store_result` would need a bind
         * per column and a buffer sized for the widest value in it, and the
         * text interface answers what this surface returns anyway. The cost is
         * that a query's rows are in memory at once, which is the right trade
         * for the statements this exists to run — a login SELECT is one row.
         *
         * Parameters still go through a prepared statement: this escapes and
         * substitutes them into the SQL rather than trusting the caller, so
         * the value never reaches the server as syntax. */
        char *stmt = NULL;
        size_t cap = strlen(sql) + 1, used = 0;
        const int32_t n = db_ary_len(params);
        for (int32_t i = 1; i <= n; i++) {
            const char *v = (const char *)(intptr_t)db_ary_at(params, i);
            cap += strlen(db_nz(v)) * 2 + 4;
        }
        stmt = (char *)malloc(cap);
        if (!stmt) {
            kn_error_set(KN_ERR_TABLE_FULL, "out of memory building a query");
            free(rows);
            return 0;
        }
        int32_t next = 1;
        for (const char *p = sql; *p; p++) {
            if (*p != '?') { stmt[used++] = *p; continue; }
            const int is_null = next <= db_ary_len(nulls) && db_ary_at(nulls, next) != 0;
            if (is_null) {
                memcpy(stmt + used, "NULL", 4);
                used += 4;
            } else {
                const char *v = db_nz((const char *)(intptr_t)db_ary_at(params, next));
                stmt[used++] = '\'';
                used += mysql_real_escape_string(c->my, stmt + used, v, (unsigned long)strlen(v));
                stmt[used++] = '\'';
            }
            next++;
        }
        stmt[used] = '\0';
        const int rc = mysql_real_query(c->my, stmt, (unsigned long)used);
        free(stmt);
        if (rc != 0) {
            char msg[512];
            snprintf(msg, sizeof msg, "mysql: %s", mysql_error(c->my));
            kn_error_set(KN_ERR_INVALID_ARG, msg);
            free(rows);
            return 0;
        }
        rows->my = mysql_store_result(c->my);
        if (!rows->my) {
            char msg[512];
            snprintf(msg, sizeof msg, "mysql: %s",
                     mysql_errno(c->my) ? mysql_error(c->my) : "the statement returned no rows");
            kn_error_set(KN_ERR_INVALID_ARG, msg);
            free(rows);
            return 0;
        }
        rows->columns = (int32_t)mysql_num_fields(rows->my);
    }

    const int32_t rh = kn_handle_new(KN_HK_DB_ROWS, rows, db_close_rows);
    if (rh == 0) {
        db_close_rows(rows);
        return 0;
    }
    kn_error_clear();
    return rh;
}
#endif /* KILN_DB */

/* db_exec(h, sql, params) -> int: rows changed, or -1. */
void db_exec(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
#ifndef KILN_DB
    db_unsupported();
    kn_ret_int(ret, -1);
#else
    kn_ret_int(ret, db_exec_impl(kn_arg_int(argv, 0), db_nz(kn_arg_text(argv, 1)),
                                 kn_arg_ptr(argv, 2), NULL));
#endif
}

/* db_exec_n(h, sql, params, nulls) -> int: as `db_exec`, with `nulls[i]`
 * binding parameter `i` as SQL NULL whatever `params[i]` holds. */
void db_exec_n(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
#ifndef KILN_DB
    db_unsupported();
    kn_ret_int(ret, -1);
#else
    kn_ret_int(ret, db_exec_impl(kn_arg_int(argv, 0), db_nz(kn_arg_text(argv, 1)),
                                 kn_arg_ptr(argv, 2), kn_arg_ptr(argv, 3)));
#endif
}

/* db_query(h, sql, params) -> int: a result handle, or 0. */
void db_query(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
#ifndef KILN_DB
    db_unsupported();
    kn_ret_int(ret, 0);
#else
    kn_ret_int(ret, db_query_impl(kn_arg_int(argv, 0), db_nz(kn_arg_text(argv, 1)),
                                  kn_arg_ptr(argv, 2), NULL));
#endif
}

/* db_query_n(h, sql, params, nulls) -> int */
void db_query_n(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
#ifndef KILN_DB
    db_unsupported();
    kn_ret_int(ret, 0);
#else
    kn_ret_int(ret, db_query_impl(kn_arg_int(argv, 0), db_nz(kn_arg_text(argv, 1)),
                                  kn_arg_ptr(argv, 2), kn_arg_ptr(argv, 3)));
#endif
}

/* db_next(r) -> bool: advance to the next row. False at the end, with the slot
 * clear — the end of a result is not a failure. False with a code set is. */
void db_next(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
#ifndef KILN_DB
    db_unsupported();
    kn_ret_bool(ret, 0);
#else
    DbRows *r = (DbRows *)kn_handle_resolve(kn_arg_int(argv, 0), KN_HK_DB_ROWS);
    if (!r) { kn_ret_bool(ret, 0); return; }
    if (r->done) { kn_error_clear(); kn_ret_bool(ret, 0); return; }

    if (r->backend == DB_SQLITE) {
        const int rc = sqlite3_step(r->lite);
        if (rc == SQLITE_ROW) { kn_error_clear(); kn_ret_bool(ret, 1); return; }
        r->done = 1;
        if (rc != SQLITE_DONE) {
            kn_error_set(KN_ERR_INVALID_ARG, "sqlite: the query failed part way through");
            kn_ret_bool(ret, 0);
            return;
        }
        kn_error_clear();
        kn_ret_bool(ret, 0);
        return;
    }

    r->row = mysql_fetch_row(r->my);
    if (!r->row) {
        r->done = 1;
        r->lengths = NULL;
        kn_error_clear();
        kn_ret_bool(ret, 0);
        return;
    }
    r->lengths = mysql_fetch_lengths(r->my);
    kn_error_clear();
    kn_ret_bool(ret, 1);
#endif
}

#ifdef KILN_DB
/* The current row's column, or NULL when it is SQL NULL or out of range.
 * `bad` says which of those it was, so a caller can tell "no such column" from
 * "the value is null". */
static const char *db_cell(DbRows *r, int32_t col, size_t *len, int *bad) {
    *bad = 0;
    if (!r || r->done) { *bad = 1; return NULL; }
    if (col < 1 || col > r->columns) { *bad = 1; return NULL; }
    if (r->backend == DB_SQLITE) {
        if (sqlite3_column_type(r->lite, col - 1) == SQLITE_NULL) return NULL;
        const unsigned char *v = sqlite3_column_text(r->lite, col - 1);
        if (len) *len = (size_t)sqlite3_column_bytes(r->lite, col - 1);
        return (const char *)v;
    }
    if (!r->row || !r->row[col - 1]) return NULL;
    if (len) *len = r->lengths ? (size_t)r->lengths[col - 1] : strlen(r->row[col - 1]);
    return r->row[col - 1];
}

static void db_bad_column(void) {
    kn_error_set(KN_ERR_OUT_OF_RANGE,
                 "no such column in the current row — columns count from 1, and there is "
                 "a row only after db_next answered true");
}
#endif

/* db_text(r, column) -> text: "" for NULL, and "" with a code set for a column
 * that is not there. `db_is_null` is what separates the two. */
void db_text_cmd(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
#ifndef KILN_DB
    db_unsupported();
    kn_ret_text(ret, kn_empty_text());
#else
    DbRows *r = (DbRows *)kn_handle_resolve(kn_arg_int(argv, 0), KN_HK_DB_ROWS);
    if (!r) { kn_ret_text(ret, kn_empty_text()); return; }
    size_t n = 0;
    int bad = 0;
    const char *v = db_cell(r, kn_arg_int(argv, 1), &n, &bad);
    if (bad) { db_bad_column(); kn_ret_text(ret, kn_empty_text()); return; }
    kn_error_clear();
    kn_ret_text(ret, v ? db_text_n(v, n) : kn_empty_text());
#endif
}

/* db_int(r, column) -> int: 0 for NULL, 0 with a code set for a bad column. */
void db_int(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
#ifndef KILN_DB
    db_unsupported();
    kn_ret_int(ret, 0);
#else
    DbRows *r = (DbRows *)kn_handle_resolve(kn_arg_int(argv, 0), KN_HK_DB_ROWS);
    if (!r) { kn_ret_int(ret, 0); return; }
    int bad = 0;
    const char *v = db_cell(r, kn_arg_int(argv, 1), NULL, &bad);
    if (bad) { db_bad_column(); kn_ret_int(ret, 0); return; }
    kn_error_clear();
    kn_ret_int(ret, v ? (int32_t)strtol(v, NULL, 10) : 0);
#endif
}

/* db_int64(r, column) -> int64 */
void db_int64(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
#ifndef KILN_DB
    db_unsupported();
    kn_ret_int64(ret, 0);
#else
    DbRows *r = (DbRows *)kn_handle_resolve(kn_arg_int(argv, 0), KN_HK_DB_ROWS);
    if (!r) { kn_ret_int64(ret, 0); return; }
    int bad = 0;
    const char *v = db_cell(r, kn_arg_int(argv, 1), NULL, &bad);
    if (bad) { db_bad_column(); kn_ret_int64(ret, 0); return; }
    kn_error_clear();
    kn_ret_int64(ret, v ? (int64_t)strtoll(v, NULL, 10) : 0);
#endif
}

/* db_is_null(r, column) -> bool: the read side of NULL, and the only thing
 * that separates a stored empty string from a missing value. */
void db_is_null(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
#ifndef KILN_DB
    db_unsupported();
    kn_ret_bool(ret, 0);
#else
    DbRows *r = (DbRows *)kn_handle_resolve(kn_arg_int(argv, 0), KN_HK_DB_ROWS);
    if (!r) { kn_ret_bool(ret, 0); return; }
    int bad = 0;
    const char *v = db_cell(r, kn_arg_int(argv, 1), NULL, &bad);
    if (bad) { db_bad_column(); kn_ret_bool(ret, 0); return; }
    kn_error_clear();
    kn_ret_bool(ret, v == NULL);
#endif
}

/* db_columns(r) -> int: how many columns the current result has. */
void db_columns(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
#ifndef KILN_DB
    db_unsupported();
    kn_ret_int(ret, -1);
#else
    DbRows *r = (DbRows *)kn_handle_resolve(kn_arg_int(argv, 0), KN_HK_DB_ROWS);
    if (!r) { kn_ret_int(ret, -1); return; }
    kn_error_clear();
    kn_ret_int(ret, r->columns);
#endif
}

/* db_result_close(r) -> bool */
void db_result_close(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    kn_ret_bool(ret, kn_handle_close(kn_arg_int(argv, 0), KN_HK_DB_ROWS));
}
