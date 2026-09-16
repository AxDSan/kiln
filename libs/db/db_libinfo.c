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
      "int h = DbOpen(\"sqlite::memory:\");\n"
      "if (h == 0)\n"
      "{\n"
      "    Console.WriteLine($\"could not open: {LastErrorText()}\");\n"
      "    return;\n"
      "}\n"
      "Console.WriteLine(\"open\");" },

    { "db_close", "db_close", KN_SDT_BOOL, 1, P_I,
      "Close a database handle",
      "int h = DbOpen(\"sqlite::memory:\");\n"
      "if (DbClose(h))\n"
      "{\n"
      "    Console.WriteLine(\"closed\");\n"
      "}" },

    { "db_exec", "db_exec", KN_SDT_INT, 3, P_ITA,
      "Run a statement with bound parameters; answers rows changed, -1 on failure",
      "int h = DbOpen(\"sqlite::memory:\");\n"
      "DbExec(h, \"create table t (name text)\", []);\n"
      "Console.WriteLine(DbExec(h, \"insert into t values (?)\", [\"Ada\"]));" },

    { "db_exec_n", "db_exec_n", KN_SDT_INT, 4, P_ITAA,
      "As db_exec, with a second list saying which parameters bind as SQL NULL",
      "int h = DbOpen(\"sqlite::memory:\");\n"
      "DbExec(h, \"create table t (name text, ip text)\", []);\n"
      "int ok = DbExecN(h, \"insert into t values (?, ?)\", [\"Ada\", \"\"], [false, true]);\n"
      "Console.WriteLine(ok);" },

    { "db_query", "db_query", KN_SDT_INT, 3, P_ITA,
      "Run a query with bound parameters; answers a result handle, 0 on failure",
      "int h = DbOpen(\"sqlite::memory:\");\n"
      "DbExec(h, \"create table t (name text)\", []);\n"
      "DbExec(h, \"insert into t values (?)\", [\"Ada\"]);\n"
      "int rows = DbQuery(h, \"select name from t where name = ?\", [\"Ada\"]);\n"
      "if (DbNext(rows))\n"
      "{\n"
      "    Console.WriteLine(DbText(rows, 1));\n"
      "}\n"
      "DbResultClose(rows);" },

    { "db_query_n", "db_query_n", KN_SDT_INT, 4, P_ITAA,
      "As db_query, with a second list saying which parameters bind as SQL NULL",
      "int h = DbOpen(\"sqlite::memory:\");\n"
      "DbExec(h, \"create table t (ip text)\", []);\n"
      "int rows = DbQueryN(h, \"select ip from t where ip is ?\", [\"\"], [true]);\n"
      "Console.WriteLine(DbColumns(rows));\n"
      "DbResultClose(rows);" },

    { "db_next", "db_next", KN_SDT_BOOL, 1, P_I,
      "Advance to the next row; false at the end, with no error set",
      "int h = DbOpen(\"sqlite::memory:\");\n"
      "DbExec(h, \"create table t (n int)\", []);\n"
      "DbExec(h, \"insert into t values (?)\", [\"1\"]);\n"
      "int rows = DbQuery(h, \"select n from t\", []);\n"
      "while (DbNext(rows))\n"
      "{\n"
      "    Console.WriteLine(DbInt(rows, 1));\n"
      "}" },

    { "db_text", "db_text_cmd", KN_SDT_TEXT, 2, P_II,
      "The current row's column as text, counting columns from 1",
      "int h = DbOpen(\"sqlite::memory:\");\n"
      "DbExec(h, \"create table t (name text)\", []);\n"
      "DbExec(h, \"insert into t values (?)\", [\"Ada\"]);\n"
      "int rows = DbQuery(h, \"select name from t\", []);\n"
      "if (DbNext(rows))\n"
      "{\n"
      "    Console.WriteLine(DbText(rows, 1));\n"
      "}" },

    { "db_int", "db_int", KN_SDT_INT, 2, P_II,
      "The current row's column as a whole number; 0 for NULL",
      "int h = DbOpen(\"sqlite::memory:\");\n"
      "DbExec(h, \"create table t (n int)\", []);\n"
      "DbExec(h, \"insert into t values (?)\", [\"42\"]);\n"
      "int rows = DbQuery(h, \"select n from t\", []);\n"
      "if (DbNext(rows))\n"
      "{\n"
      "    Console.WriteLine(DbInt(rows, 1));\n"
      "}" },

    { "db_int64", "db_int64", KN_SDT_INT64, 2, P_II,
      "The current row's column as a wide whole number; 0 for NULL",
      "int h = DbOpen(\"sqlite::memory:\");\n"
      "DbExec(h, \"create table t (n int)\", []);\n"
      "DbExec(h, \"insert into t values (?)\", [\"9000000000\"]);\n"
      "int rows = DbQuery(h, \"select n from t\", []);\n"
      "if (DbNext(rows))\n"
      "{\n"
      "    PrintInt64(DbInt64(rows, 1));\n"
      "}" },

    { "db_is_null", "db_is_null", KN_SDT_BOOL, 2, P_II,
      "Is the current row's column SQL NULL, rather than an empty value",
      "int h = DbOpen(\"sqlite::memory:\");\n"
      "DbExec(h, \"create table t (ip text)\", []);\n"
      "DbExecN(h, \"insert into t values (?)\", [\"\"], [true]);\n"
      "int rows = DbQuery(h, \"select ip from t\", []);\n"
      "if (DbNext(rows))\n"
      "{\n"
      "    Console.WriteLine($\"null: {DbIsNull(rows, 1)}\");\n"
      "}" },

    { "db_columns", "db_columns", KN_SDT_INT, 1, P_I,
      "How many columns the result has; -1 when the handle is not one",
      "int h = DbOpen(\"sqlite::memory:\");\n"
      "DbExec(h, \"create table t (a int, b int)\", []);\n"
      "int rows = DbQuery(h, \"select a, b from t\", []);\n"
      "Console.WriteLine(DbColumns(rows));" },

    { "db_result_close", "db_result_close", KN_SDT_BOOL, 1, P_I,
      "Close a result handle",
      "int h = DbOpen(\"sqlite::memory:\");\n"
      "DbExec(h, \"create table t (n int)\", []);\n"
      "int rows = DbQuery(h, \"select n from t\", []);\n"
      "if (DbResultClose(rows))\n"
      "{\n"
      "    Console.WriteLine(\"closed\");\n"
      "}" },
    { "db_begin", "db_begin", KN_SDT_BOOL, 1, P_I,
      "Start a transaction; every statement until db_commit or db_rollback is part of it",
      "int h = DbOpen(\"sqlite::memory:\");\n"
      "DbExec(h, \"create table t (n int)\", []);\n"
      "DbBegin(h);\n"
      "DbExec(h, \"insert into t values (?)\", [\"1\"]);\n"
      "DbCommit(h);\n"
      "Console.WriteLine(\"committed\");" },

    { "db_commit", "db_commit", KN_SDT_BOOL, 1, P_I,
      "Make the transaction's changes permanent",
      "int h = DbOpen(\"sqlite::memory:\");\n"
      "DbExec(h, \"create table t (n int)\", []);\n"
      "DbBegin(h);\n"
      "DbExec(h, \"insert into t values (?)\", [\"1\"]);\n"
      "if (DbCommit(h))\n"
      "{\n"
      "    Console.WriteLine(\"kept\");\n"
      "}" },

    { "db_rollback", "db_rollback", KN_SDT_BOOL, 1, P_I,
      "Undo everything since db_begin",
      "int h = DbOpen(\"sqlite::memory:\");\n"
      "DbExec(h, \"create table t (n int)\", []);\n"
      "DbBegin(h);\n"
      "DbExec(h, \"insert into t values (?)\", [\"1\"]);\n"
      "DbRollback(h);\n"
      "int rows = DbQuery(h, \"select count(*) from t\", []);\n"
      "if (DbNext(rows))\n"
      "{\n"
      "    Console.WriteLine(DbInt(rows, 1));\n"
      "}" },

    { "db_last_insert_id", "db_last_insert_id", KN_SDT_INT64, 1, P_I,
      "The id the last INSERT on this connection produced; 0 when there has been none",
      "int h = DbOpen(\"sqlite::memory:\");\n"
      "DbExec(h, \"create table t (id integer primary key, name text)\", []);\n"
      "DbExec(h, \"insert into t (name) values (?)\", [\"Ada\"]);\n"
      "PrintInt64(DbLastInsertId(h));" },

    { "db_double", "db_double", KN_SDT_DOUBLE, 2, P_II,
      "The current row's column as a double; 0.0 for NULL",
      "int h = DbOpen(\"sqlite::memory:\");\n"
      "DbExec(h, \"create table t (x real)\", []);\n"
      "DbExec(h, \"insert into t values (?)\", [\"2.5\"]);\n"
      "int rows = DbQuery(h, \"select x from t\", []);\n"
      "if (DbNext(rows))\n"
      "{\n"
      "    Console.WriteLine(DbDouble(rows, 1));\n"
      "}" },

    { "db_bool", "db_bool", KN_SDT_BOOL, 2, P_II,
      "The current row's column as a bool: 1, true or any non-zero number; false for NULL",
      "int h = DbOpen(\"sqlite::memory:\");\n"
      "DbExec(h, \"create table t (banned int)\", []);\n"
      "DbExec(h, \"insert into t values (?)\", [\"1\"]);\n"
      "int rows = DbQuery(h, \"select banned from t\", []);\n"
      "if (DbNext(rows))\n"
      "{\n"
      "    Console.WriteLine($\"banned: {DbBool(rows, 1)}\");\n"
      "}" },

    { "db_column_name", "db_column_name", KN_SDT_TEXT, 2, P_II,
      "The name of a result column, counting from 1",
      "int h = DbOpen(\"sqlite::memory:\");\n"
      "DbExec(h, \"create table t (a int, b int)\", []);\n"
      "int rows = DbQuery(h, \"select a, b as total from t\", []);\n"
      "Console.WriteLine(DbColumnName(rows, 2));" },
    /* --- asynchronous: the statement runs on a worker thread -------------
     * Everything here follows one rule, stated once in db_cmds.c: the worker
     * never calls into the runtime, and every handle a program sees is built by
     * the program on its own thread when it claims the answer.  That is what
     * makes a 10 Hz tick able to write to a database without holding the pump. */
    { "db_exec_async", "db_exec_async", KN_SDT_INT, 3, P_ITA,
      "Queue an INSERT, UPDATE or DELETE on a worker thread and answer a request id, so the pump keeps turning",
      "int h = DbOpen(\"sqlite::memory:\");\n"
      "int job = DbExecAsync(h, \"create table t (a int)\", []);\n"
      "Console.WriteLine($\"queued {job}\");" },

    { "db_exec_async_n", "db_exec_async_n", KN_SDT_INT, 4, P_ITAA,
      "The same, with a bool per parameter saying which of them bind SQL NULL",
      "int h = DbOpen(\"sqlite::memory:\");\n"
      "int job = DbExecAsyncN(h, \"insert into t values (?)\", [\"1\"], [true]);\n"
      "Console.WriteLine($\"queued {job}\");" },

    { "db_query_async", "db_query_async", KN_SDT_INT, 3, P_ITA,
      "Queue a SELECT on a worker thread and answer a request id; the rows are collected by the time it is ready",
      "int h = DbOpen(\"sqlite::memory:\");\n"
      "int job = DbQueryAsync(h, \"select 1\", []);\n"
      "Console.WriteLine($\"queued {job}\");" },

    { "db_query_async_n", "db_query_async_n", KN_SDT_INT, 4, P_ITAA,
      "The same, with a bool per parameter saying which of them bind SQL NULL",
      "int h = DbOpen(\"sqlite::memory:\");\n"
      "int job = DbQueryAsyncN(h, \"select ?\", [\"ada\"], [false]);\n"
      "Console.WriteLine($\"queued {job}\");" },

    { "db_req_ready", "db_req_ready", KN_SDT_BOOL, 1, P_I,
      "Whether a queued statement has finished; false while it is still running, and the error slot stays clear",
      "int h = DbOpen(\"sqlite::memory:\");\n"
      "int job = DbExecAsync(h, \"create table t (a int)\", []);\n"
      "bool done = DbReqReady(job);\n"
      "Console.WriteLine($\"finished: {done}\");" },

    { "db_req_rows", "db_req_rows", KN_SDT_INT, 1, P_I,
      "Rows changed by a finished execute, rows in a finished query, and -1 when it failed",
      "int h = DbOpen(\"sqlite::memory:\");\n"
      "int job = DbExecAsync(h, \"create table t (a int)\", []);\n"
      "Console.WriteLine(DbReqRows(job));" },

    { "db_req_columns", "db_req_columns", KN_SDT_INT, 1, P_I,
      "How many columns a finished query collected",
      "int h = DbOpen(\"sqlite::memory:\");\n"
      "int job = DbQueryAsync(h, \"select 1\", []);\n"
      "Console.WriteLine(DbReqColumns(job));" },

    { "db_req_text", "db_req_text", KN_SDT_TEXT, 3, P_III,
      "One cell of a finished query, rows and columns counting from 1, and \"\" for SQL NULL",
      "int h = DbOpen(\"sqlite::memory:\");\n"
      "int job = DbQueryAsync(h, \"select 1\", []);\n"
      "Console.WriteLine(DbReqText(job, 1, 1));" },

    { "db_req_is_null", "db_req_is_null", KN_SDT_BOOL, 3, P_III,
      "Whether a cell of a finished query is SQL NULL, which an empty string cannot say",
      "int h = DbOpen(\"sqlite::memory:\");\n"
      "int job = DbQueryAsync(h, \"select null\", []);\n"
      "Console.WriteLine($\"null: {DbReqIsNull(job, 1, 1)}\");" },

    { "db_req_error", "db_req_error", KN_SDT_TEXT, 1, P_I,
      "Why a finished statement failed, or \"\" when it succeeded",
      "int h = DbOpen(\"sqlite::memory:\");\n"
      "int job = DbExecAsync(h, \"select * from nosuchtable\", []);\n"
      "Console.WriteLine(DbReqError(job));" },

    { "db_req_free", "db_req_free", KN_SDT_BOOL, 1, P_I,
      "Release a finished request and everything it collected; refused while it is still running",
      "int h = DbOpen(\"sqlite::memory:\");\n"
      "int job = DbExecAsync(h, \"create table t (a int)\", []);\n"
      "Console.WriteLine($\"freed: {DbReqFree(job)}\");" },

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
