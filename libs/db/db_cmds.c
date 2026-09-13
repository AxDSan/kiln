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
/* <mysql.h>, not <mysql/mysql.h>: the client's headers live in
 * /usr/include/mysql on Fedora and MySQL, and /usr/include/mariadb on Debian's
 * MariaDB. pkg-config puts whichever one it is on the include path, so the
 * unqualified name is the one that resolves everywhere and the qualified one
 * is the name that happened to work here. */
#include <mysql.h>
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
    int in_tx;               /* between db_begin and its commit or rollback  */
    int in_flight;           /* asynchronous requests the worker is running   */
    int64_t last_insert;     /* MySQL: the last statement's AUTO_INCREMENT id;
                                SQLite asks the connection at read time      */
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

/* --- an asynchronous request ---------------------------------------------
 * A program writing on a tick holds the pump with a synchronous `db_query` —
 * every other event waits — until the server answers.  A request is that
 * statement carried to a worker thread instead, so the pump keeps turning and
 * the answer
 * is collected when it is ready.
 *
 * Everything a request owns is PLAIN malloc, and that is a correctness
 * requirement rather than a taste: `kn_malloc` belongs to the runtime, whose
 * collector traces the program's stack, and neither is safe to touch from
 * another thread.  The worker never calls into the runtime at all — the result
 * handle, if any, is built by the program when it claims the answer. */
typedef struct DbReq {
    struct DbReq *next;      /* the queue, and the live list                  */
    DbConn       *conn;
    int32_t       handle;    /* the request's own handle, for in-flight count */
    int           is_query;
    char         *sql;       /* malloc'd copies: the worker owns these        */
    void         *params;    /* an ABI-shaped array of malloc'd strings       */
    void         *nulls;     /* ditto, or NULL                                */
    volatile int  state;     /* DB_REQ_*; the only field both threads touch   */
    int32_t       rows;      /* rows affected, or rows in the set             */
    int32_t       columns;
    char        **cells;     /* rows * columns; NULL cell means SQL NULL      */
    int32_t       error_code;
    char         *error;     /* the failure text, or NULL                     */
} DbReq;

enum { DB_REQ_PENDING = 0, DB_REQ_READY = 1 };

/* Which request the driver code below is running for, if it is running for one.
 * Thread-local, because the program's own thread may be failing a statement on
 * one connection while the worker fails one on another. */
static _Thread_local DbReq *g_req = NULL;

static char *db_strdup(const char *s) {
    if (!s) return NULL;
    size_t n = strlen(s);
    char *o = (char *)malloc(n + 1);
    if (o) memcpy(o, s, n + 1);
    return o;
}

/* Where a failure goes.  The bodies further down are shared by the program's
 * own thread and by the worker, and `kn_error_set` is the runtime's — writing
 * it from the worker would race the program for one slot.  While a request is
 * running the message is attached to it instead, and read back with
 * `db_req_error`. */
static void db_fail(int32_t code, const char *msg) {
    if (g_req) {
        if (!g_req->error) g_req->error = db_strdup(msg);
        g_req->error_code = code;
        return;
    }
    kn_error_set(code, msg);
}

/* A connection with a request in flight is the worker's until the program
 * claims the answer.  Two threads on one connection is not a race to be
 * survived: it is a crash in the MySQL client and a corrupt statement in
 * SQLite, so the second user is refused by name. */
/* The success half of the same rule: the worker must not clear the runtime's
 * error slot either. */
static void db_ok(void) { if (!g_req) kn_error_clear(); }

static int32_t db_busy(void) {
    db_fail(KN_ERR_INVALID_ARG,
                 "this connection has an asynchronous request in flight; wait for it "
                 "(db_req_ready) before using the connection again");
    return -1;
}

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
    /* A connection is not closed under a request the worker is still running:
     * the worker holds the connection itself, so freeing it here is a
     * use-after-free in the driver rather than a failed call. */
    const int32_t h = kn_arg_int(argv, 0);
    DbConn *c = (DbConn *)kn_handle_resolve(h, KN_HK_DB);
    if (c && c->in_flight > 0) {
        kn_error_set(KN_ERR_INVALID_ARG,
                     "db_close: this connection has an asynchronous request in flight");
        kn_ret_bool(ret, 0);
        return;
    }
    kn_ret_bool(ret, kn_handle_close(h, KN_HK_DB));
}

#ifdef KILN_DB
/* The shared body of `db_exec` and `db_exec_n`: prepare, bind, run, and answer
 * how many rows it changed. -1 on any failure, with the driver's own message
 * in the slot — a SQL error the program cannot see is a program that reports
 * "0 rows" for a typo. */
static int32_t db_exec_conn(DbConn *c, const char *sql, void *params, void *nulls) {
    if (c->backend == DB_SQLITE) {
        sqlite3_stmt *st = NULL;
        if (sqlite3_prepare_v2(c->lite, sql, -1, &st, NULL) != SQLITE_OK) {
            char msg[512];
            snprintf(msg, sizeof msg, "sqlite: %s", sqlite3_errmsg(c->lite));
            db_fail(KN_ERR_INVALID_ARG, msg);
            return -1;
        }
        if (!db_bind_sqlite(st, params, nulls)) { sqlite3_finalize(st); return -1; }
        const int rc = sqlite3_step(st);
        if (rc != SQLITE_DONE && rc != SQLITE_ROW) {
            char msg[512];
            snprintf(msg, sizeof msg, "sqlite: %s", sqlite3_errmsg(c->lite));
            db_fail(KN_ERR_INVALID_ARG, msg);
            sqlite3_finalize(st);
            return -1;
        }
        const int32_t changed = (int32_t)sqlite3_changes(c->lite);
        sqlite3_finalize(st);
        db_ok();
        return changed;
    }

    MYSQL_STMT *st = mysql_stmt_init(c->my);
    if (!st) {
        db_fail(KN_ERR_TABLE_FULL, "out of memory preparing a statement");
        return -1;
    }
    if (mysql_stmt_prepare(st, sql, (unsigned long)strlen(sql)) != 0) {
        char msg[512];
        snprintf(msg, sizeof msg, "mysql: %s", mysql_stmt_error(st));
        db_fail(KN_ERR_INVALID_ARG, msg);
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
        db_fail(KN_ERR_TABLE_FULL, "out of memory binding parameters");
        return -1;
    }
    db_bind_mysql(b, lens, isnull, params, nulls);
    int32_t changed = -1;
    if (n && mysql_stmt_bind_param(st, b) != 0) {
        char msg[512];
        snprintf(msg, sizeof msg, "mysql: %s", mysql_stmt_error(st));
        db_fail(KN_ERR_INVALID_ARG, msg);
    } else if (mysql_stmt_execute(st) != 0) {
        char msg[512];
        snprintf(msg, sizeof msg, "mysql: %s", mysql_stmt_error(st));
        db_fail(KN_ERR_INVALID_ARG, msg);
    } else {
        changed = (int32_t)mysql_stmt_affected_rows(st);
        /* MySQL's own LAST_INSERT_ID() keeps its value across statements that
         * insert nothing, and so does this: a non-zero id is the only thing
         * that overwrites the last one. */
        const my_ulonglong id = mysql_stmt_insert_id(st);
        if (id != 0) c->last_insert = (int64_t)id;
        db_ok();
    }
    free(b); free(lens); free(isnull);
    mysql_stmt_close(st);
    return changed;
}

/* The shared body of `db_query` and `db_query_n`, and of an asynchronous
 * query: it takes the connection directly so the worker can run it too. */
static DbRows *db_query_rows(DbConn *c, const char *sql, void *params, void *nulls) {
    DbRows *rows = (DbRows *)calloc(1, sizeof *rows);
    if (!rows) {
        db_fail(KN_ERR_TABLE_FULL, "out of memory starting a query");
        return 0;
    }
    rows->backend = c->backend;

    if (c->backend == DB_SQLITE) {
        if (sqlite3_prepare_v2(c->lite, sql, -1, &rows->lite, NULL) != SQLITE_OK) {
            char msg[512];
            snprintf(msg, sizeof msg, "sqlite: %s", sqlite3_errmsg(c->lite));
            db_fail(KN_ERR_INVALID_ARG, msg);
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
            db_fail(KN_ERR_TABLE_FULL, "out of memory building a query");
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
            db_fail(KN_ERR_INVALID_ARG, msg);
            free(rows);
            return 0;
        }
        rows->my = mysql_store_result(c->my);
        if (!rows->my) {
            char msg[512];
            snprintf(msg, sizeof msg, "mysql: %s",
                     mysql_errno(c->my) ? mysql_error(c->my) : "the statement returned no rows");
            db_fail(KN_ERR_INVALID_ARG, msg);
            free(rows);
            return 0;
        }
        rows->columns = (int32_t)mysql_num_fields(rows->my);
    }

    db_ok();
    return rows;
}

/* The command's half of a query: resolve the handle, run the shared body, and
 * give the result set a handle of its own. */
static int32_t db_query_impl(int32_t h, const char *sql, void *params, void *nulls) {
    DbConn *c = (DbConn *)kn_handle_resolve(h, KN_HK_DB);
    if (!c) return 0;
    if (c->in_flight > 0) { db_busy(); return 0; }
    DbRows *rows = db_query_rows(c, sql, params, nulls);
    if (!rows) return 0;
    const int32_t rh = kn_handle_new(KN_HK_DB_ROWS, rows, db_close_rows);
    if (rh == 0) {
        db_close_rows(rows);
        return 0;
    }
    db_ok();
    return rh;
}

/* The command's half of an execute. */
static int32_t db_exec_impl(int32_t h, const char *sql, void *params, void *nulls) {
    DbConn *c = (DbConn *)kn_handle_resolve(h, KN_HK_DB);
    if (!c) return -1;
    /* The connection is the worker's while a request is in flight: two threads
     * on one connection is a crash in the MySQL client and a corrupt statement
     * in SQLite, not a race to be survived. */
    if (c->in_flight > 0) return db_busy();
    return db_exec_conn(c, sql, params, nulls);
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

#ifdef KILN_DB
/* One step of a result set: 1 when a row is current, 0 at the end or on a
 * failure — which `db_fail` has already reported.  Shared by the command above
 * and by the worker, so an asynchronous query walks its rows through exactly
 * the code a synchronous one does. */
static int db_rows_next(DbRows *r) {
    if (r->done) return 0;

    if (r->backend == DB_SQLITE) {
        const int rc = sqlite3_step(r->lite);
        if (rc == SQLITE_ROW) return 1;
        r->done = 1;
        if (rc != SQLITE_DONE) {
            db_fail(KN_ERR_INVALID_ARG, "sqlite: the query failed part way through");
            return 0;
        }
        return 0;
    }

    r->row = mysql_fetch_row(r->my);
    if (!r->row) {
        r->done = 1;
        r->lengths = NULL;
        return 0;
    }
    r->lengths = mysql_fetch_lengths(r->my);
    return 1;
}
#endif

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
    const int got = db_rows_next(r);
    if (got) kn_error_clear();
    kn_ret_bool(ret, got);
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

#ifdef KILN_DB
static int db_tx_fail(DbConn *c, const char *what) {
    char msg[512];
    if (c->backend == DB_SQLITE) {
        snprintf(msg, sizeof msg, "sqlite: %s: %s", what, sqlite3_errmsg(c->lite));
    } else {
        snprintf(msg, sizeof msg, "mysql: %s: %s", what, mysql_error(c->my));
    }
    kn_error_set(KN_ERR_INVALID_ARG, msg);
    return 0;
}
#endif

/* db_begin(h) -> bool: start a transaction. Nested begins are refused rather
 * than silently flattened — a program that begins twice and commits once has
 * a bug, and SQLite would say so while MySQL would not. */
void db_begin(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    {
        DbConn *busy = (DbConn *)kn_handle_resolve(kn_arg_int(argv, 0), KN_HK_DB);
        if (busy && busy->in_flight > 0) { db_busy(); kn_ret_bool(ret, 0); return; }
    }
#ifndef KILN_DB
    db_unsupported();
    kn_ret_bool(ret, 0);
#else
    DbConn *c = (DbConn *)kn_handle_resolve(kn_arg_int(argv, 0), KN_HK_DB);
    if (!c) { kn_ret_bool(ret, 0); return; }
    if (c->in_tx) {
        kn_error_set(KN_ERR_INVALID_ARG, "already in a transaction — commit or roll back first");
        kn_ret_bool(ret, 0);
        return;
    }
    int ok;
    if (c->backend == DB_SQLITE) {
        ok = sqlite3_exec(c->lite, "BEGIN", NULL, NULL, NULL) == SQLITE_OK;
    } else {
        /* Autocommit off is the transaction: every prepared statement that
         * follows joins it, and commit/rollback end it and turn autocommit
         * back on, so a connection outside a transaction behaves as before. */
        ok = mysql_autocommit(c->my, 0) == 0;
    }
    if (!ok) { db_tx_fail(c, "begin"); kn_ret_bool(ret, 0); return; }
    c->in_tx = 1;
    kn_error_clear();
    kn_ret_bool(ret, 1);
#endif
}

#ifdef KILN_DB
static void db_tx_end(Kiln_Slot *ret, Kiln_Slot *argv, int commit) {
    {
        DbConn *busy = (DbConn *)kn_handle_resolve(kn_arg_int(argv, 0), KN_HK_DB);
        if (busy && busy->in_flight > 0) { db_busy(); kn_ret_bool(ret, 0); return; }
    }
    DbConn *c = (DbConn *)kn_handle_resolve(kn_arg_int(argv, 0), KN_HK_DB);
    if (!c) { kn_ret_bool(ret, 0); return; }
    if (!c->in_tx) {
        kn_error_set(KN_ERR_INVALID_ARG, "not in a transaction — db_begin starts one");
        kn_ret_bool(ret, 0);
        return;
    }
    int ok;
    if (c->backend == DB_SQLITE) {
        ok = sqlite3_exec(c->lite, commit ? "COMMIT" : "ROLLBACK", NULL, NULL, NULL) == SQLITE_OK;
    } else {
        ok = (commit ? mysql_commit(c->my) : mysql_rollback(c->my)) == 0;
        mysql_autocommit(c->my, 1);
    }
    /* Either way the transaction is over: a failed COMMIT has rolled back on
     * both backends, and leaving the flag set would refuse the next begin. */
    c->in_tx = 0;
    if (!ok) { db_tx_fail(c, commit ? "commit" : "rollback"); kn_ret_bool(ret, 0); return; }
    kn_error_clear();
    kn_ret_bool(ret, 1);
}
#endif

/* db_commit(h) -> bool */
void db_commit(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
#ifndef KILN_DB
    db_unsupported();
    kn_ret_bool(ret, 0);
#else
    db_tx_end(ret, argv, 1);
#endif
}

/* db_rollback(h) -> bool */
void db_rollback(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
#ifndef KILN_DB
    db_unsupported();
    kn_ret_bool(ret, 0);
#else
    db_tx_end(ret, argv, 0);
#endif
}

/* db_last_insert_id(h) -> int64: the AUTO_INCREMENT / rowid the last INSERT
 * on this connection produced; 0 when there has been none. */
void db_last_insert_id(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
#ifndef KILN_DB
    db_unsupported();
    kn_ret_int64(ret, 0);
#else
    DbConn *c = (DbConn *)kn_handle_resolve(kn_arg_int(argv, 0), KN_HK_DB);
    if (!c) { kn_ret_int64(ret, 0); return; }
    kn_error_clear();
    if (c->backend == DB_SQLITE) {
        kn_ret_int64(ret, (int64_t)sqlite3_last_insert_rowid(c->lite));
        return;
    }
    kn_ret_int64(ret, c->last_insert);
#endif
}

/* db_double(r, column) -> double: 0.0 for NULL, 0.0 with a code set for a bad
 * column. Both backends hand a FLOAT/DOUBLE/DECIMAL back as its decimal text,
 * and strtod is the inverse of that. */
void db_double(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
#ifndef KILN_DB
    db_unsupported();
    kn_ret_double(ret, 0.0);
#else
    DbRows *r = (DbRows *)kn_handle_resolve(kn_arg_int(argv, 0), KN_HK_DB_ROWS);
    if (!r) { kn_ret_double(ret, 0.0); return; }
    int bad = 0;
    const char *v = db_cell(r, kn_arg_int(argv, 1), NULL, &bad);
    if (bad) { db_bad_column(); kn_ret_double(ret, 0.0); return; }
    kn_error_clear();
    kn_ret_double(ret, v ? strtod(v, NULL) : 0.0);
#endif
}

/* db_bool(r, column) -> bool: false for NULL. A BOOLEAN/TINYINT(1) arrives as
 * "0" or "1"; SQLite has no bool and stores what it was given, so "true" and
 * any non-zero number also read as true. */
void db_bool(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
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
    if (!v) { kn_ret_bool(ret, 0); return; }
    if ((v[0] == 't' || v[0] == 'T') && (v[1] == 'r' || v[1] == 'R')) { kn_ret_bool(ret, 1); return; }
    kn_ret_bool(ret, strtod(v, NULL) != 0.0);
#endif
}

/* db_column_name(r, column) -> text: the name (or alias) of a result column,
 * so a program can read a row it did not write the SELECT for. */
void db_column_name(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
#ifndef KILN_DB
    db_unsupported();
    kn_ret_text(ret, kn_empty_text());
#else
    DbRows *r = (DbRows *)kn_handle_resolve(kn_arg_int(argv, 0), KN_HK_DB_ROWS);
    if (!r) { kn_ret_text(ret, kn_empty_text()); return; }
    const int32_t col = kn_arg_int(argv, 1);
    if (col < 1 || col > r->columns) {
        kn_error_set(KN_ERR_OUT_OF_RANGE, "no such column — columns count from 1");
        kn_ret_text(ret, kn_empty_text());
        return;
    }
    const char *name;
    if (r->backend == DB_SQLITE) {
        name = sqlite3_column_name(r->lite, col - 1);
    } else {
        MYSQL_FIELD *f = mysql_fetch_field_direct(r->my, (unsigned)(col - 1));
        name = f ? f->name : NULL;
    }
    kn_error_clear();
    kn_ret_text(ret, db_text(name));
#endif
}

/* db_result_close(r) -> bool */
void db_result_close(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    kn_ret_bool(ret, kn_handle_close(kn_arg_int(argv, 0), KN_HK_DB_ROWS));
}


/* --- asynchronous requests -------------------------------------------------
 *
 * A program writing on a tick holds the pump with a synchronous `db_query` —
 * every other event waits — until the server answers.  On loopback that is a
 * millisecond and invisible, which is how a timing problem survives every
 * local run.  A request is that statement carried to a worker thread instead.
 *
 * **The worker never calls into the runtime.**  No `kn_malloc` (the collector
 * traces the program's stack), no `kn_error_set` (one slot, two writers), no
 * `kn_handle_new` (the handle table).  Everything a request owns is plain
 * malloc, and every handle a program sees is built by the program, on its own
 * thread, when it claims the answer.  That is the whole reason this is shaped
 * the way it is, and it is why `db_fail` and `db_ok` exist above.
 *
 * A connection with a request in flight is the worker's.  The synchronous
 * surface refuses to use it until the answer is claimed — two threads on one
 * connection is a crash in the MySQL client and a corrupt statement in SQLite,
 * not a race to be survived — and `db_close` refuses too.
 *
 * The program drives it with a timer:
 *
 *     on_tick
 *       if db_req_ready(job)
 *         while db_req_rows(job) ... db_req_text(job, r, c) ...
 *         call db_req_free(job)
 *       end
 *     end
 */

#ifdef KILN_DB

/* --- the thread shim ------------------------------------------------------
 * One place knows the platform; the queue below reads the same on both. */
#ifdef _WIN32
#include <windows.h>

static INIT_ONCE  g_once  = INIT_ONCE_STATIC_INIT;
static CRITICAL_SECTION g_cs;
static CONDITION_VARIABLE g_wake;
static HANDLE g_thread = NULL;
static int g_ready = 0;

static BOOL CALLBACK db_once(PINIT_ONCE once, PVOID param, PVOID *ctx) {
    (void)once; (void)param; (void)ctx;
    InitializeCriticalSection(&g_cs);
    InitializeConditionVariable(&g_wake);
    g_ready = 1;
    return TRUE;
}
static void db_shim_init(void) { InitOnceExecuteOnce(&g_once, db_once, NULL, NULL); }
static void db_lock(void) { EnterCriticalSection(&g_cs); }
static void db_unlock(void) { LeaveCriticalSection(&g_cs); }
static void db_signal(void) { WakeConditionVariable(&g_wake); }
static void db_wait(void) { SleepConditionVariableCS(&g_wake, &g_cs, INFINITE); }
static void db_thread_start(void);
static DWORD WINAPI db_thread_body(LPVOID arg);
#else
#include <pthread.h>

static pthread_mutex_t g_lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t  g_wake = PTHREAD_COND_INITIALIZER;
static pthread_t       g_thread;
static int g_ready = 1;

static void db_shim_init(void) { /* statically initialised on POSIX */ }
static void db_lock(void) { pthread_mutex_lock(&g_lock); }
static void db_unlock(void) { pthread_mutex_unlock(&g_lock); }
static void db_signal(void) { pthread_cond_signal(&g_wake); }
static void db_wait(void) { pthread_cond_wait(&g_wake, &g_lock); }
static void db_thread_start(void);
static void *db_thread_body(void *arg);
#endif

static void db_worker_loop(void);
static void db_run(DbReq *r);

/* --- what a request owns -------------------------------------------------- */

/* A copy of a parameter list in the ABI's own array layout, so the binding code
 * below is the code the synchronous path runs: it reads `len` and `elems` and
 * nothing else, and the strings are ours.  That is the whole trick — it is a
 * lookalike built with plain malloc, not a runtime array. */
static void *db_copy_array(void *src, int as_text) {
    const int32_t n = src ? db_ary_len(src) : 0;
    if (n <= 0) return NULL;
    Kiln_Array *a = (Kiln_Array *)malloc(sizeof(Kiln_Array) + (size_t)n * sizeof(int64_t));
    if (!a) return NULL;
    a->elem_tag = KN_SDT_TEXT;
    a->len = n;
    a->cap = n;
    a->_pad = 0;
    int64_t *elems = (int64_t *)(a + 1);
    for (int32_t i = 1; i <= n; i++) {
        if (as_text) {
            const char *v = (const char *)(intptr_t)db_ary_at(src, i);
            elems[i - 1] = (int64_t)(intptr_t)db_strdup(v ? v : "");
        } else {
            elems[i - 1] = db_ary_at(src, i) ? 1 : 0;
        }
    }
    return a;
}

static void db_free_array(void *p, int as_text) {
    if (!p) return;
    Kiln_Array *a = (Kiln_Array *)p;
    if (as_text) {
        int64_t *elems = (int64_t *)(a + 1);
        for (int32_t i = 0; i < a->len; i++) free((void *)(intptr_t)elems[i]);
    }
    free(a);
}

static void db_req_release(DbReq *r) {
    if (!r) return;
    free(r->sql);
    db_free_array(r->params, 1);
    db_free_array(r->nulls, 0);
    if (r->cells) {
        for (int32_t i = 0; i < r->rows * r->columns; i++) free(r->cells[i]);
        free(r->cells);
    }
    free(r->error);
    free(r);
}

/* The handle's close function.  A request the worker is still running is left
 * alone rather than freed under it: the leak is one struct at exit, and the
 * alternative is the worker writing into freed memory. */
static void db_close_req(void *payload) {
    DbReq *r = (DbReq *)payload;
    if (!r || r->state != DB_REQ_READY) return;
    db_req_release(r);
}

/* --- the worker ----------------------------------------------------------- */

static int      g_stop = 0;
static int      g_started = 0;
static DbReq   *g_queue_head = NULL;
static DbReq   *g_queue_tail = NULL;

static void db_run(DbReq *r) {
    g_req = r;
    if (r->is_query) {
        DbRows *rows = db_query_rows(r->conn, r->sql, r->params, r->nulls);
        if (!rows) { g_req = NULL; return; }        /* the message is on the request */
        r->columns = rows->columns;
        int32_t cap = 0;
        while (db_rows_next(rows)) {
            if (r->rows == cap) {
                const int32_t ncap = cap ? cap * 2 : 16;
                char **g = (char **)realloc(r->cells,
                                            (size_t)ncap * (size_t)rows->columns * sizeof(char *));
                if (!g) { db_fail(KN_ERR_TABLE_FULL, "out of memory collecting a result set"); break; }
                r->cells = g;
                cap = ncap;
            }
            /* Copied now: a driver's buffers are its own and are gone the
             * moment the result set is closed — and the program reads this on
             * another thread anyway.  A NULL cell is SQL NULL, which is a
             * different value from an empty string. */
            for (int32_t c = 1; c <= rows->columns; c++) {
                size_t n = 0;
                int bad = 0;
                const char *v = db_cell(rows, c, &n, &bad);
                char *copy = NULL;
                if (v && !bad) {
                    copy = (char *)malloc(n + 1);
                    if (copy) { memcpy(copy, v, n); copy[n] = '\0'; }
                }
                r->cells[(size_t)r->rows * (size_t)rows->columns + (size_t)(c - 1)] = copy;
            }
            r->rows++;
        }
        db_close_rows(rows);
    } else {
        r->rows = db_exec_conn(r->conn, r->sql, r->params, r->nulls);
    }
    g_req = NULL;
}

static void db_worker_loop(void) {
    for (;;) {
        db_lock();
        while (!g_queue_head && !g_stop) db_wait();
        DbReq *r = g_queue_head;
        if (r) {
            g_queue_head = r->next;
            if (!g_queue_head) g_queue_tail = NULL;
            r->next = NULL;
        }
        const int stopping = g_stop;
        db_unlock();

        if (!r) { if (stopping) break; continue; }

        db_run(r);

        db_lock();
        r->state = DB_REQ_READY;
        if (r->conn) r->conn->in_flight--;
        db_unlock();
    }
}

#ifndef _WIN32
static void *db_thread_body(void *arg) { (void)arg; db_worker_loop(); return NULL; }
static void db_thread_start(void) { pthread_create(&g_thread, NULL, db_thread_body, NULL); }
#else
static DWORD WINAPI db_thread_body(LPVOID arg) { (void)arg; db_worker_loop(); return 0; }
static void db_thread_start(void) { g_thread = CreateThread(NULL, 0, db_thread_body, NULL, 0, NULL); }
#endif

/* --- submitting ----------------------------------------------------------- */

static int32_t db_submit(int32_t h, const char *sql, void *params, void *nulls, int is_query) {
    DbConn *c = (DbConn *)kn_handle_resolve(h, KN_HK_DB);
    if (!c) return 0;

    DbReq *r = (DbReq *)calloc(1, sizeof *r);
    if (!r) {
        kn_error_set(KN_ERR_TABLE_FULL, "out of memory queueing a statement");
        return 0;
    }
    r->conn = c;
    r->is_query = is_query;
    r->sql = db_strdup(db_nz(sql));
    r->params = db_copy_array(params, 1);
    r->nulls = db_copy_array(nulls, 0);
    if (!r->sql || (params && db_ary_len(params) > 0 && !r->params)) {
        db_req_release(r);
        kn_error_set(KN_ERR_TABLE_FULL, "out of memory copying a statement");
        return 0;
    }

    const int32_t rh = kn_handle_new(KN_HK_DB_REQ, r, db_close_req);
    if (rh == 0) { db_req_release(r); return 0; }
    r->handle = rh;

    db_shim_init();
    if (!g_ready && !g_started) { db_req_release(r); kn_handle_close(rh, KN_HK_DB_REQ); return 0; }
    db_lock();
    if (!g_started) { db_thread_start(); g_started = 1; }
    c->in_flight++;
    if (g_queue_tail) g_queue_tail->next = r; else g_queue_head = r;
    g_queue_tail = r;
    db_signal();
    db_unlock();

    kn_error_clear();
    return rh;
}

/* The reading side's shared first step: the request, and whether it is done. */
static DbReq *db_req_of(int32_t req, const char *cmd, int need_ready) {
    DbReq *r = (DbReq *)kn_handle_resolve(req, KN_HK_DB_REQ);
    if (!r) return NULL;
    if (need_ready && r->state != DB_REQ_READY) {
        char msg[128];
        snprintf(msg, sizeof msg, "%s: the request has not finished; poll db_req_ready", cmd);
        kn_error_set(KN_ERR_INVALID_ARG, msg);
        return NULL;
    }
    return r;
}

#endif /* KILN_DB */

/* --- the commands --------------------------------------------------------- */

/* db_exec_async(h, sql, params) -> int : a request id, or 0 with the reason in
 * the slot.  The connection is the worker's until the answer is claimed. */
void db_exec_async(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
#ifndef KILN_DB
    db_unsupported();
    kn_ret_int(ret, 0);
#else
    kn_ret_int(ret, db_submit(kn_arg_int(argv, 0), db_nz(kn_arg_text(argv, 1)),
                              kn_arg_ptr(argv, 2), NULL, 0));
#endif
}

/* db_exec_async_n(h, sql, params, nulls) -> int */
void db_exec_async_n(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
#ifndef KILN_DB
    db_unsupported();
    kn_ret_int(ret, 0);
#else
    kn_ret_int(ret, db_submit(kn_arg_int(argv, 0), db_nz(kn_arg_text(argv, 1)),
                              kn_arg_ptr(argv, 2), kn_arg_ptr(argv, 3), 0));
#endif
}

/* db_query_async(h, sql, params) -> int */
void db_query_async(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
#ifndef KILN_DB
    db_unsupported();
    kn_ret_int(ret, 0);
#else
    kn_ret_int(ret, db_submit(kn_arg_int(argv, 0), db_nz(kn_arg_text(argv, 1)),
                              kn_arg_ptr(argv, 2), NULL, 1));
#endif
}

/* db_query_async_n(h, sql, params, nulls) -> int */
void db_query_async_n(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
#ifndef KILN_DB
    db_unsupported();
    kn_ret_int(ret, 0);
#else
    kn_ret_int(ret, db_submit(kn_arg_int(argv, 0), db_nz(kn_arg_text(argv, 1)),
                              kn_arg_ptr(argv, 2), kn_arg_ptr(argv, 3), 1));
#endif
}

/* db_req_ready(req) -> bool : false while it is still running, and false with
 * a code set when the handle is not one. */
void db_req_ready(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
#ifndef KILN_DB
    db_unsupported();
    kn_ret_bool(ret, 0);
#else
    DbReq *r = db_req_of(kn_arg_int(argv, 0), "db_req_ready", 0);
    if (!r) { kn_ret_bool(ret, 0); return; }
    kn_error_clear();
    kn_ret_bool(ret, r->state == DB_REQ_READY);
#endif
}

/* db_req_rows(req) -> int : rows changed by an execute, rows in the set for a
 * query, and -1 when it failed — db_req_error says why. */
void db_req_rows(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
#ifndef KILN_DB
    db_unsupported();
    kn_ret_int(ret, -1);
#else
    DbReq *r = db_req_of(kn_arg_int(argv, 0), "db_req_rows", 1);
    if (!r) { kn_ret_int(ret, -1); return; }
    if (r->error) {
        kn_error_set(r->error_code, r->error);
        kn_ret_int(ret, -1);
        return;
    }
    kn_error_clear();
    kn_ret_int(ret, r->rows);
#endif
}

void db_req_columns(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
#ifndef KILN_DB
    db_unsupported();
    kn_ret_int(ret, -1);
#else
    DbReq *r = db_req_of(kn_arg_int(argv, 0), "db_req_columns", 1);
    if (!r) { kn_ret_int(ret, -1); return; }
    if (r->error) {
        kn_error_set(r->error_code, r->error);
        kn_ret_int(ret, -1);
        return;
    }
    kn_error_clear();
    kn_ret_int(ret, r->columns);
#endif
}

/* db_req_text(req, row, column) -> text : a cell of the collected set, "" for
 * SQL NULL, and "" with a code set for a row or column that is not there. */
void db_req_text(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
#ifndef KILN_DB
    db_unsupported();
    kn_ret_text(ret, kn_empty_text());
#else
    DbReq *r = db_req_of(kn_arg_int(argv, 0), "db_req_text", 1);
    if (!r) { kn_ret_text(ret, kn_empty_text()); return; }
    const int32_t row = kn_arg_int(argv, 1), col = kn_arg_int(argv, 2);
    if (row < 1 || row > r->rows || col < 1 || col > r->columns) {
        kn_error_set(KN_ERR_OUT_OF_RANGE,
                     "db_req_text: no such cell — rows and columns count from 1");
        kn_ret_text(ret, kn_empty_text());
        return;
    }
    const char *v = r->cells[(size_t)(row - 1) * (size_t)r->columns + (size_t)(col - 1)];
    kn_error_clear();
    kn_ret_text(ret, v ? db_text(v) : kn_empty_text());
#endif
}

/* db_req_is_null(req, row, column) -> bool : what separates a NULL cell from
 * an empty one. */
void db_req_is_null(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
#ifndef KILN_DB
    db_unsupported();
    kn_ret_bool(ret, 0);
#else
    DbReq *r = db_req_of(kn_arg_int(argv, 0), "db_req_is_null", 1);
    if (!r) { kn_ret_bool(ret, 0); return; }
    const int32_t row = kn_arg_int(argv, 1), col = kn_arg_int(argv, 2);
    if (row < 1 || row > r->rows || col < 1 || col > r->columns) {
        kn_error_set(KN_ERR_OUT_OF_RANGE,
                     "db_req_is_null: no such cell — rows and columns count from 1");
        kn_ret_bool(ret, 0);
        return;
    }
    const char *v = r->cells[(size_t)(row - 1) * (size_t)r->columns + (size_t)(col - 1)];
    kn_error_clear();
    kn_ret_bool(ret, v == NULL);
#endif
}

/* db_req_error(req) -> text : "" when the statement succeeded. */
void db_req_error(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
#ifndef KILN_DB
    db_unsupported();
    kn_ret_text(ret, kn_empty_text());
#else
    DbReq *r = db_req_of(kn_arg_int(argv, 0), "db_req_error", 1);
    if (!r) { kn_ret_text(ret, kn_empty_text()); return; }
    kn_error_clear();
    kn_ret_text(ret, r->error ? db_text(r->error) : kn_empty_text());
#endif
}

/* db_req_free(req) -> bool : the answer is released.  Refused while the request
 * is still running, because the worker owns it until then. */
void db_req_free(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
#ifndef KILN_DB
    db_unsupported();
    kn_ret_bool(ret, 0);
#else
    const int32_t req = kn_arg_int(argv, 0);
    DbReq *r = db_req_of(req, "db_req_free", 0);
    if (!r) { kn_ret_bool(ret, 0); return; }
    if (r->state != DB_REQ_READY) {
        kn_error_set(KN_ERR_INVALID_ARG,
                     "db_req_free: the request is still running; wait for db_req_ready");
        kn_ret_bool(ret, 0);
        return;
    }
    kn_ret_bool(ret, kn_handle_close(req, KN_HK_DB_REQ));
#endif
}
