# Databases

`using Kiln.Db;` opens a database, runs statements with bound parameters, and
walks rows. Two backends — SQLite and MySQL — behind one set of commands, chosen
by the DSN's prefix so a program does not change between them.

```k2
namespace Accounts;

using Kiln.Db;

public static class P
{
    public static void Main()
    {
        var h = Db.Open("sqlite:accounts.db");
        if (h == 0)
        {
            Console.WriteLine($"could not open: {LastErrorText()}");
            return;
        }

        Db.Exec(h, "create table if not exists people (name text, age int)", []);
        Db.Exec(h, "insert into people values (?, ?)", ["Ada", "36"]);

        var rows = Db.Query(h, "select name, age from people where name = ?", ["Ada"]);
        while (Db.Next(rows))
            Console.WriteLine($"{Db.Text(rows, 1)} is {Db.Int(rows, 2)}");
        Db.ResultClose(rows);
        Db.Close(h);
    }
}
```

A record can also be the table: `[Table]` on a record gives it `Insert` and
`Select`, and a `Select` predicate written as a lambda is translated to SQL while
compiling. The [language guide](./kiln-2.md#talking-to-a-database) shows it.
This page is the layer underneath, which a program reaches for when the SQL is
its own.

## Every value is a bound parameter

There is no `Db.Exec(h, sql)` without a parameter list, and that is the whole
design. A library whose shortest call concatenates is a library that teaches
injection; here the shortest call binds, so the easy path and the safe path are
the same path.

```
// The value is a value, whatever it looks like.
var rows = Db.Query(h, "select id from accounts where username = ?",
                    ["ada' or '1'='1"]);
```

That query matches nothing. The text never reaches the server as syntax — it is
bound, so the database compares a column against a string that happens to
contain quotes.

Parameters are a `List<string>` whatever the column's type is, and the driver
coerces: `["36"]` into an INTEGER column stores 36. One type keeps the surface
one call wide. Read a column back as whatever it is — `Db.Int`, `Db.Int64`,
`Db.Text`.

## NULL is not an empty string

A database distinguishes "no value" from "the empty value", and so does
everything that reads it afterwards — a blank IP address on an admin page means
something different from "never logged in". A list of strings cannot say NULL,
so the `N` forms take a second list that can:

```
// last_login_ip binds as SQL NULL; the id binds as the string it is.
Db.ExecN(h, "update accounts set last_login_ip = ? where id = ?",
         ["", "1"], [true, false]);
```

`nulls[i] = true` binds parameter `i` as NULL whatever `params[i]` holds. A
shorter `nulls` list is not an error — the rest are not null, which is what
passing none already meant.

On the way back, `Db.IsNull` is the only thing that separates a stored empty
string from a missing value, because both answer `""` from `Db.Text`.

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

`Db.Query` answers a result handle; `Db.Next` advances to a row and answers
false at the end. False at the end is not a failure — the error slot is clear
— and false *with* a code set is. Columns count from 1, as everything in Kiln
does.

```
var rows = Db.Query(h, "select name, age from people", []);
while (Db.Next(rows))
    Console.WriteLine(Db.Text(rows, 1));
Db.ResultClose(rows);
```

A result handle is closed by `Db.ResultClose`, and closing the connection
closes what it opened. Both are runtime handles: a stale one is rejected rather
than reused, a connection handle passed where a result handle goes is refused
by kind, and `0` is never valid.

## Transactions

`Db.Begin` starts one; every statement on that handle until `Db.Commit` or
`Db.Rollback` is part of it. A second `Db.Begin` before either is refused
rather than flattened, and a commit or rollback with no transaction open is
refused too — both are bugs in the program, and the library says so instead of
guessing.

```
Db.Begin(h);
Db.Exec(h, "update bags set slot = ? where id = ?", ["7", "41"]);
Db.Exec(h, "update bags set slot = ? where id = ?", ["3", "42"]);
if (Db.Commit(h))
    Console.WriteLine("swapped");
```

`Db.LastInsertId(h)` answers the id the last INSERT on that connection
produced — the AUTO_INCREMENT value on MySQL, the rowid on SQLite — and 0 when
there has been none. Like MySQL's own `LAST_INSERT_ID()`, a statement that
inserts nothing leaves it as it was.

## Typed reads

`Db.Text`, `Db.Int` and `Db.Int64` are joined by `Db.Double` for a FLOAT,
DOUBLE or DECIMAL column and `Db.Bool` for a BOOLEAN or TINYINT(1) — `1`,
`true` and any non-zero number read as true, NULL as false. `Db.ColumnName`
answers a column's name or alias, for a program reading a row whose SELECT it
did not write.

## Statements that do not hold the loop

Every command above is synchronous: a query inside a server's event handler
holds every other client until it answers. On loopback that is a millisecond
and invisible, which is how a timing bug survives every local test. A server
that writes on every kill, pickup or equip wants the other shape:

```k2
namespace Poll;

using Kiln.Db;

Timer poller
{
    Interval = 20;
    Tick += OnTick;
}

public static class P
{
    static int job = 0;

    public static void Main()
    {
        var h = Db.Open("sqlite::memory:");
        Db.Exec(h, "create table items (id int)", []);
        job = Db.QueryAsync(h, "select count(*) from items", []);
    }

    public static void OnTick(int n)
    {
        if (!Db.ReqReady(job))
        {
            return;
        }
        int rows = Db.ReqRows(job);
        string cell = Db.ReqText(job, 1, 1);
        Console.WriteLine($"{rows} row(s), first cell {cell}");
        Db.ReqFree(job);
    }
}
```

`Db.ExecAsync`, `Db.ExecAsyncN`, `Db.QueryAsync` and `Db.QueryAsyncN` queue a
statement on a worker thread and answer a request id, then return.
`Db.ReqReady` is the poll; `Db.ReqRows`, `Db.ReqColumns`, `Db.ReqText`,
`Db.ReqIsNull` and `Db.ReqError` are the reading side; `Db.ReqFree` releases it,
and is refused while it is still running.

**The worker never touches the runtime**, so the answer is a value the program
claims rather than a cursor over a driver that is gone — which is why the
reading surface above did not change, and why a query's rows are all in memory
by the time it is ready. **A connection with a request in flight belongs to the
worker**: `Db.Exec`, `Db.Query`, the transactions and `Db.Close` refuse it by
name. Two threads on one connection is a crash in one client and corruption in
the other, not a race to be survived.

Measured rather than promised: a three-million-row query taking 780 ms, with a
20 ms timer counting turns of the loop, lets the timer fire **39 times**. A
synchronous call gets one turn, because the statement runs inside it.

## What is not here

No bulk insert, no connection pool, one connection per handle, and one worker
thread — two slow statements queue behind each other. The thread is started on
the first asynchronous call, so a program that never uses one never creates
one. The surface is the one a long-running program's data layer needs —
SELECT, UPDATE, DELETE and INSERT with bound parameters, transactions around
them, and the choice of paying for them now or later — and it grows when
something real needs more, not before.

For MySQL, a query's rows are fetched into memory at once rather than streamed.
That is the right trade for the statements this exists to run; a `select *` over
a large table is not one of them yet.

## When the client libraries are missing

`Kiln.Db` builds without SQLite or MySQL installed. Every command then answers
its failure sentinel with `LastErrorCode()` of `10006` (`KN_ERR_UNSUPPORTED`) and
a message naming what to install, so a checkout with no database headers still
builds and a program that never opens a database never notices. Install your
distribution's `sqlite3` and `libmariadb` development packages and rebuild.
