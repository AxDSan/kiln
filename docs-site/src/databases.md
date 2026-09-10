# Databases

`use db` opens a database, runs statements with bound parameters, and walks
rows. Two backends — SQLite and MySQL — behind one set of commands, chosen by
the DSN's prefix so a program does not change between them.

```
module accounts
target console
use db

sub main
  let h: int = db_open("sqlite:accounts.db")
  if h = 0
    call print_text("could not open: {last_error_text()}")
    return
  end

  call db_exec(h, "create table if not exists people (name text, age int)", [])
  call db_exec(h, "insert into people values (?, ?)", ["Ada", "36"])

  let rows: int = db_query(h, "select name, age from people where name = ?", ["Ada"])
  while db_next(rows)
    call print_text("{db_text(rows, 1)} is {db_int(rows, 2)}")
  end
  call db_result_close(rows)
  call db_close(h)
end
```

## Every value is a bound parameter

There is no `db_exec(h, sql)` without a parameter list, and that is the whole
design. A library whose shortest call concatenates is a library that teaches
injection; here the shortest call binds, so the easy path and the safe path are
the same path.

```
# The value is a value, whatever it looks like.
let rows: int = db_query(h, "select id from accounts where username = ?",
                         ["ada' or '1'='1"])
```

That query matches nothing. The text never reaches the server as syntax — it is
bound, so the database compares a column against a string that happens to
contain quotes.

Parameters are `text[]` whatever the column's type is, and the driver coerces:
`["36"]` into an INTEGER column stores 36. One type keeps the surface one call
wide. Read a column back as whatever it is — `db_int`, `db_int64`, `db_text`.

## NULL is not an empty string

A database distinguishes "no value" from "the empty value", and so does
everything that reads it afterwards — a blank IP address on an admin page means
something different from "never logged in". `text[]` cannot say NULL, so the
`_n` forms take a second list that can:

```
# last_login_ip binds as SQL NULL; the id binds as the text it is.
call db_exec_n(h, "update accounts set last_login_ip = ? where id = ?",
               ["", "1"], [true, false])
```

`nulls[i] = true` binds parameter `i` as NULL whatever `params[i]` holds. A
shorter `nulls` list is not an error — the rest are not null, which is what
passing none already meant.

On the way back, `db_is_null` is the only thing that separates a stored empty
string from a missing value, because both answer `""` from `db_text`.

## Connecting

| DSN | Opens |
|---|---|
| `sqlite:notes.db` | a file, created if it is not there |
| `sqlite::memory:` | a database that lives as long as the handle |
| `mysql://user:pass@host/database` | a MySQL or MariaDB server |
| `mysql://user:pass@host:3307/database` | the same, on another port |

Everything in a MySQL DSN but the host and the database is optional. The
connection is set to `utf8mb4` explicitly rather than to the server's default:
a name is text, and a program whose encoding depends on how the server was
configured is a program that works until it is deployed.

## Rows

`db_query` answers a result handle; `db_next` advances to a row and answers
false at the end. False at the end is not a failure — the error slot is clear
— and false *with* a code set is. Columns count from 1, as everything in Kiln
does.

```
let rows: int = db_query(h, "select name, age from people", [])
while db_next(rows)
  call print_text(db_text(rows, 1))
end
call db_result_close(rows)
```

A result handle is closed by `db_result_close`, and closing the connection
closes what it opened. Both are runtime handles: a stale one is rejected rather
than reused, a connection handle passed where a result handle goes is refused
by kind, and `0` is never valid.

## Transactions

`db_begin` starts one; every statement on that handle until `db_commit` or
`db_rollback` is part of it. A second `db_begin` before either is refused
rather than flattened, and a commit or rollback with no transaction open is
refused too — both are bugs in the program, and the library says so instead of
guessing.

```
call db_begin(h)
call db_exec(h, "update bags set slot = ? where id = ?", ["7", "41"])
call db_exec(h, "update bags set slot = ? where id = ?", ["3", "42"])
if db_commit(h)
  call print_text("swapped")
end
```

`db_last_insert_id(h)` answers the id the last INSERT on that connection
produced — the AUTO_INCREMENT value on MySQL, the rowid on SQLite — and 0 when
there has been none. Like MySQL's own `LAST_INSERT_ID()`, a statement that
inserts nothing leaves it as it was.

## Typed reads

`db_text`, `db_int` and `db_int64` are joined by `db_double` for a FLOAT,
DOUBLE or DECIMAL column and `db_bool` for a BOOLEAN or TINYINT(1) — `1`,
`true` and any non-zero number read as true, NULL as false. `db_column_name`
answers a column's name or alias, for a program reading a row whose SELECT it
did not write.

## What is not here

No bulk insert, no connection pool, one connection per handle, and every
command is synchronous: a query inside a server's event handler holds every
other client until it answers. The surface is the one a login path and a game
server's data layer need — SELECT, UPDATE, DELETE and INSERT with bound
parameters, and transactions around them — and it grows when something real
needs more, not before.

For MySQL, a query's rows are fetched into memory at once rather than streamed.
That is the right trade for the statements this exists to run; a `select *` over
a large table is not one of them yet.

## When the client libraries are missing

`db` builds without SQLite or MySQL installed. Every command then answers its
failure sentinel with `last_error_code()` of `10006` (`KN_ERR_UNSUPPORTED`) and
a message naming what to install, so a checkout with no database headers still
builds and a program that never opens a database never notices. Install your
distribution's `sqlite3` and `libmariadb` development packages and rebuild.
