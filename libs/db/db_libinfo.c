/* "db" library metadata (design-time only; compiled into the introspection
 * .so, never a shipped program — same split as core_libinfo.c). */
#include "kiln_abi.h"

void db_open(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void db_close(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void db_exec(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void db_exec_n(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void db_query(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void db_query_n(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void db_next(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void db_text_cmd(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void db_int(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void db_int64(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void db_is_null(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void db_columns(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void db_result_close(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void db_begin(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void db_commit(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void db_rollback(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void db_last_insert_id(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void db_double(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void db_bool(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void db_column_name(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);

/* The asynchronous surface: the same statements, carried to a worker thread so
 * a 10 Hz tick does not hold the pump while a server answers. */
void db_exec_async(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void db_exec_async_n(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void db_query_async(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void db_query_async_n(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void db_req_ready(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void db_req_rows(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void db_req_columns(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void db_req_text(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void db_req_is_null(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void db_req_error(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void db_req_free(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);

static const int32_t P_T[]    = { KN_SDT_TEXT };
static const int32_t P_I[]    = { KN_SDT_INT };
static const int32_t P_II[]   = { KN_SDT_INT, KN_SDT_INT };
static const int32_t P_ITA[]  = { KN_SDT_INT, KN_SDT_TEXT, KN_SDT_ARRAY_OF(KN_SDT_TEXT) };
static const int32_t P_ITAA[] = { KN_SDT_INT, KN_SDT_TEXT, KN_SDT_ARRAY_OF(KN_SDT_TEXT), KN_SDT_ARRAY_OF(KN_SDT_BOOL) };
static const int32_t P_III[]  = { KN_SDT_INT, KN_SDT_INT, KN_SDT_INT };

/* The examples all open `:memory:`, so every one of them runs anywhere and
 * `tools/check-docs.sh` compiles them without a server or a file on disk.
 * The DSN prefix is the only line that changes for MySQL. */
static const Kiln_CommandDesc DB_COMMANDS[] = {
    { "db_open", "db_open", KN_SDT_INT, 1, P_T,
      "Open a database and answer its handle; 0 when it could not be opened",
      "let h: int = db_open(\"sqlite::memory:\")\n"
      "if h = 0\n"
      "  call print_text(\"could not open: {last_error_text()}\")\n"
      "  return\n"
      "end\n"
      "call print_text(\"open\")" },

    { "db_close", "db_close", KN_SDT_BOOL, 1, P_I,
      "Close a database handle",
      "let h: int = db_open(\"sqlite::memory:\")\n"
      "if db_close(h)\n"
      "  call print_text(\"closed\")\n"
      "end" },

    { "db_exec", "db_exec", KN_SDT_INT, 3, P_ITA,
      "Run a statement with bound parameters; answers rows changed, -1 on failure",
      "let h: int = db_open(\"sqlite::memory:\")\n"
      "call db_exec(h, \"create table t (name text)\", [])\n"
      "call print_int(db_exec(h, \"insert into t values (?)\", [\"Ada\"]))" },

    { "db_exec_n", "db_exec_n", KN_SDT_INT, 4, P_ITAA,
      "As db_exec, with a second list saying which parameters bind as SQL NULL",
      "let h: int = db_open(\"sqlite::memory:\")\n"
      "call db_exec(h, \"create table t (name text, ip text)\", [])\n"
      "let ok: int = db_exec_n(h, \"insert into t values (?, ?)\", [\"Ada\", \"\"], [false, true])\n"
      "call print_int(ok)" },

    { "db_query", "db_query", KN_SDT_INT, 3, P_ITA,
      "Run a query with bound parameters; answers a result handle, 0 on failure",
      "let h: int = db_open(\"sqlite::memory:\")\n"
      "call db_exec(h, \"create table t (name text)\", [])\n"
      "call db_exec(h, \"insert into t values (?)\", [\"Ada\"])\n"
      "let rows: int = db_query(h, \"select name from t where name = ?\", [\"Ada\"])\n"
      "if db_next(rows)\n"
      "  call print_text(db_text(rows, 1))\n"
      "end\n"
      "call db_result_close(rows)" },

    { "db_query_n", "db_query_n", KN_SDT_INT, 4, P_ITAA,
      "As db_query, with a second list saying which parameters bind as SQL NULL",
      "let h: int = db_open(\"sqlite::memory:\")\n"
      "call db_exec(h, \"create table t (ip text)\", [])\n"
      "let rows: int = db_query_n(h, \"select ip from t where ip is ?\", [\"\"], [true])\n"
      "call print_int(db_columns(rows))\n"
      "call db_result_close(rows)" },

    { "db_next", "db_next", KN_SDT_BOOL, 1, P_I,
      "Advance to the next row; false at the end, with no error set",
      "let h: int = db_open(\"sqlite::memory:\")\n"
      "call db_exec(h, \"create table t (n int)\", [])\n"
      "call db_exec(h, \"insert into t values (?)\", [\"1\"])\n"
      "let rows: int = db_query(h, \"select n from t\", [])\n"
      "while db_next(rows)\n"
      "  call print_int(db_int(rows, 1))\n"
      "end" },

    { "db_text", "db_text_cmd", KN_SDT_TEXT, 2, P_II,
      "The current row's column as text, counting columns from 1",
      "let h: int = db_open(\"sqlite::memory:\")\n"
      "call db_exec(h, \"create table t (name text)\", [])\n"
      "call db_exec(h, \"insert into t values (?)\", [\"Ada\"])\n"
      "let rows: int = db_query(h, \"select name from t\", [])\n"
      "if db_next(rows)\n"
      "  call print_text(db_text(rows, 1))\n"
      "end" },

    { "db_int", "db_int", KN_SDT_INT, 2, P_II,
      "The current row's column as a whole number; 0 for NULL",
      "let h: int = db_open(\"sqlite::memory:\")\n"
      "call db_exec(h, \"create table t (n int)\", [])\n"
      "call db_exec(h, \"insert into t values (?)\", [\"42\"])\n"
      "let rows: int = db_query(h, \"select n from t\", [])\n"
      "if db_next(rows)\n"
      "  call print_int(db_int(rows, 1))\n"
      "end" },

    { "db_int64", "db_int64", KN_SDT_INT64, 2, P_II,
      "The current row's column as a wide whole number; 0 for NULL",
      "let h: int = db_open(\"sqlite::memory:\")\n"
      "call db_exec(h, \"create table t (n int)\", [])\n"
      "call db_exec(h, \"insert into t values (?)\", [\"9000000000\"])\n"
      "let rows: int = db_query(h, \"select n from t\", [])\n"
      "if db_next(rows)\n"
      "  call print_int64(db_int64(rows, 1))\n"
      "end" },

    { "db_is_null", "db_is_null", KN_SDT_BOOL, 2, P_II,
      "Is the current row's column SQL NULL, rather than an empty value",
      "let h: int = db_open(\"sqlite::memory:\")\n"
      "call db_exec(h, \"create table t (ip text)\", [])\n"
      "call db_exec_n(h, \"insert into t values (?)\", [\"\"], [true])\n"
      "let rows: int = db_query(h, \"select ip from t\", [])\n"
      "if db_next(rows)\n"
      "  call print_text(\"null: {db_is_null(rows, 1)}\")\n"
      "end" },

    { "db_columns", "db_columns", KN_SDT_INT, 1, P_I,
      "How many columns the result has; -1 when the handle is not one",
      "let h: int = db_open(\"sqlite::memory:\")\n"
      "call db_exec(h, \"create table t (a int, b int)\", [])\n"
      "let rows: int = db_query(h, \"select a, b from t\", [])\n"
      "call print_int(db_columns(rows))" },

    { "db_result_close", "db_result_close", KN_SDT_BOOL, 1, P_I,
      "Close a result handle",
      "let h: int = db_open(\"sqlite::memory:\")\n"
      "call db_exec(h, \"create table t (n int)\", [])\n"
      "let rows: int = db_query(h, \"select n from t\", [])\n"
      "if db_result_close(rows)\n"
      "  call print_text(\"closed\")\n"
      "end" },
    { "db_begin", "db_begin", KN_SDT_BOOL, 1, P_I,
      "Start a transaction; every statement until db_commit or db_rollback is part of it",
      "let h: int = db_open(\"sqlite::memory:\")\n"
      "call db_exec(h, \"create table t (n int)\", [])\n"
      "call db_begin(h)\n"
      "call db_exec(h, \"insert into t values (?)\", [\"1\"])\n"
      "call db_commit(h)\n"
      "call print_text(\"committed\")" },

    { "db_commit", "db_commit", KN_SDT_BOOL, 1, P_I,
      "Make the transaction's changes permanent",
      "let h: int = db_open(\"sqlite::memory:\")\n"
      "call db_exec(h, \"create table t (n int)\", [])\n"
      "call db_begin(h)\n"
      "call db_exec(h, \"insert into t values (?)\", [\"1\"])\n"
      "if db_commit(h)\n"
      "  call print_text(\"kept\")\n"
      "end" },

    { "db_rollback", "db_rollback", KN_SDT_BOOL, 1, P_I,
      "Undo everything since db_begin",
      "let h: int = db_open(\"sqlite::memory:\")\n"
      "call db_exec(h, \"create table t (n int)\", [])\n"
      "call db_begin(h)\n"
      "call db_exec(h, \"insert into t values (?)\", [\"1\"])\n"
      "call db_rollback(h)\n"
      "let rows: int = db_query(h, \"select count(*) from t\", [])\n"
      "if db_next(rows)\n"
      "  call print_int(db_int(rows, 1))\n"
      "end" },

    { "db_last_insert_id", "db_last_insert_id", KN_SDT_INT64, 1, P_I,
      "The id the last INSERT on this connection produced; 0 when there has been none",
      "let h: int = db_open(\"sqlite::memory:\")\n"
      "call db_exec(h, \"create table t (id integer primary key, name text)\", [])\n"
      "call db_exec(h, \"insert into t (name) values (?)\", [\"Ada\"])\n"
      "call print_int64(db_last_insert_id(h))" },

    { "db_double", "db_double", KN_SDT_DOUBLE, 2, P_II,
      "The current row's column as a double; 0.0 for NULL",
      "let h: int = db_open(\"sqlite::memory:\")\n"
      "call db_exec(h, \"create table t (x real)\", [])\n"
      "call db_exec(h, \"insert into t values (?)\", [\"2.5\"])\n"
      "let rows: int = db_query(h, \"select x from t\", [])\n"
      "if db_next(rows)\n"
      "  call print_double(db_double(rows, 1))\n"
      "end" },

    { "db_bool", "db_bool", KN_SDT_BOOL, 2, P_II,
      "The current row's column as a bool: 1, true or any non-zero number; false for NULL",
      "let h: int = db_open(\"sqlite::memory:\")\n"
      "call db_exec(h, \"create table t (banned int)\", [])\n"
      "call db_exec(h, \"insert into t values (?)\", [\"1\"])\n"
      "let rows: int = db_query(h, \"select banned from t\", [])\n"
      "if db_next(rows)\n"
      "  call print_text(\"banned: {db_bool(rows, 1)}\")\n"
      "end" },

    { "db_column_name", "db_column_name", KN_SDT_TEXT, 2, P_II,
      "The name of a result column, counting from 1",
      "let h: int = db_open(\"sqlite::memory:\")\n"
      "call db_exec(h, \"create table t (a int, b int)\", [])\n"
      "let rows: int = db_query(h, \"select a, b as total from t\", [])\n"
      "call print_text(db_column_name(rows, 2))" },
    /* --- asynchronous: the statement runs on a worker thread -------------
     * Everything here follows one rule, stated once in db_cmds.c: the worker
     * never calls into the runtime, and every handle a program sees is built by
     * the program on its own thread when it claims the answer.  That is what
     * makes a 10 Hz tick able to write to a database without holding the pump. */
    { "db_exec_async", "db_exec_async", KN_SDT_INT, 3, P_ITA,
      "Queue an INSERT, UPDATE or DELETE on a worker thread and answer a request id, so the pump keeps turning",
      "let h: int = db_open(\"sqlite::memory:\")\nlet job: int = db_exec_async(h, \"create table t (a int)\", [])\ncall print_text(\"queued {job}\")" },

    { "db_exec_async_n", "db_exec_async_n", KN_SDT_INT, 4, P_ITAA,
      "The same, with a bool per parameter saying which of them bind SQL NULL",
      "let h: int = db_open(\"sqlite::memory:\")\nlet job: int = db_exec_async_n(h, \"insert into t values (?)\", [\"1\"], [true])\ncall print_text(\"queued {job}\")" },

    { "db_query_async", "db_query_async", KN_SDT_INT, 3, P_ITA,
      "Queue a SELECT on a worker thread and answer a request id; the rows are collected by the time it is ready",
      "let h: int = db_open(\"sqlite::memory:\")\nlet job: int = db_query_async(h, \"select 1\", [])\ncall print_text(\"queued {job}\")" },

    { "db_query_async_n", "db_query_async_n", KN_SDT_INT, 4, P_ITAA,
      "The same, with a bool per parameter saying which of them bind SQL NULL",
      "let h: int = db_open(\"sqlite::memory:\")\nlet job: int = db_query_async_n(h, \"select ?\", [\"ada\"], [false])\ncall print_text(\"queued {job}\")" },

    { "db_req_ready", "db_req_ready", KN_SDT_BOOL, 1, P_I,
      "Whether a queued statement has finished; false while it is still running, and the error slot stays clear",
      "let h: int = db_open(\"sqlite::memory:\")\nlet job: int = db_exec_async(h, \"create table t (a int)\", [])\nlet done: bool = db_req_ready(job)\ncall print_text(\"finished: {done}\")" },

    { "db_req_rows", "db_req_rows", KN_SDT_INT, 1, P_I,
      "Rows changed by a finished execute, rows in a finished query, and -1 when it failed",
      "let h: int = db_open(\"sqlite::memory:\")\nlet job: int = db_exec_async(h, \"create table t (a int)\", [])\ncall print_int(db_req_rows(job))" },

    { "db_req_columns", "db_req_columns", KN_SDT_INT, 1, P_I,
      "How many columns a finished query collected",
      "let h: int = db_open(\"sqlite::memory:\")\nlet job: int = db_query_async(h, \"select 1\", [])\ncall print_int(db_req_columns(job))" },

    { "db_req_text", "db_req_text", KN_SDT_TEXT, 3, P_III,
      "One cell of a finished query, rows and columns counting from 1, and \"\" for SQL NULL",
      "let h: int = db_open(\"sqlite::memory:\")\nlet job: int = db_query_async(h, \"select 1\", [])\ncall print_text(db_req_text(job, 1, 1))" },

    { "db_req_is_null", "db_req_is_null", KN_SDT_BOOL, 3, P_III,
      "Whether a cell of a finished query is SQL NULL, which an empty string cannot say",
      "let h: int = db_open(\"sqlite::memory:\")\nlet job: int = db_query_async(h, \"select null\", [])\ncall print_text(\"null: {db_req_is_null(job, 1, 1)}\")" },

    { "db_req_error", "db_req_error", KN_SDT_TEXT, 1, P_I,
      "Why a finished statement failed, or \"\" when it succeeded",
      "let h: int = db_open(\"sqlite::memory:\")\nlet job: int = db_exec_async(h, \"select * from nosuchtable\", [])\ncall print_text(db_req_error(job))" },

    { "db_req_free", "db_req_free", KN_SDT_BOOL, 1, P_I,
      "Release a finished request and everything it collected; refused while it is still running",
      "let h: int = db_open(\"sqlite::memory:\")\nlet job: int = db_exec_async(h, \"create table t (a int)\", [])\ncall print_text(\"freed: {db_req_free(job)}\")" },

};

static const Kiln_LibInfo DB_INFO = {
    KILN_ABI_VERSION,
    "db",
    "kiln-db-0000-0000-0000-000000000009",
    0, 2, 0,
    (int32_t)(sizeof(DB_COMMANDS) / sizeof(DB_COMMANDS[0])),
    DB_COMMANDS,
};

const Kiln_LibInfo *kiln_get_lib_info(void) {
    return &DB_INFO;
}
