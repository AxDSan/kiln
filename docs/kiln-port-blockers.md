# What the game-server port needs from Kiln

A companion to `docs/gbo-port-prerequisites.md`, which was written for the login
slice. The game server is a different size of problem: 25,161 lines in
`GodsBattle.GameServer` plus roughly 40,000 more in the `GodsBattle.Core` it
imports, against the 1,370 the login server took. The login port came out at
**651 lines of Kiln**, so the same ratio puts the game server somewhere between
15,000 and 25,000 lines — a project, not a session.

This page is the *toolchain* side of that: what Kiln had to grow before the port
can be written at all, what was measured, and what is deliberately left open.
It is ordered by what blocks, because the blocker is the useful part.

---

## 1. Reading the client's own tables — DONE, 2026-09-12

**The game's data is the game.** Item templates, forge probabilities, quests,
map links, skill tables and UI layouts live in files that only a client ships,
in formats Kiln could not read: XML everywhere, declared `GB2312` over bytes
that are really GBK, CRLF, comments carrying Chinese, and 3,430-row tables whose
attributes are the data. Without a reader, a port is a reimplementation of
tables a person retyped by hand.

**`libs/xml`** is a real reader, not a regex over tags:

- a document is a handle (kind `KN_HK_XML`); inside it an element is a small
  int, so walking costs no allocation and no id can outlive its document;
- **no byte is interpreted** — names and values are runs of your buffer, which
  is what makes a GB2312 declaration over GBK content a document to read rather
  than a problem to solve;
- no recursion: open elements are a heap stack, so a deeply nested UI layout
  cannot overflow the machine stack;
- 17 commands: `xml_parse`, `xml_close`, `xml_close_all`, `xml_root`,
  `xml_parent`, `xml_line`, `xml_count`, `xml_child`, `xml_name`, `xml_attr`,
  `xml_has_attr`, `xml_attr_count`, `xml_attr_name`, `xml_attr_at`, `xml_text`,
  `xml_first`, `xml_sibling`, `xml_descend`.

**`libs/encoding`** converts the codepages, and carries **no table of its own**:
GBK is 23,940 mappings and hand-copying them is how a decoder ends up 99% right
and silently wrong on a name. It is iconv on POSIX and the Win32 codepage API on
Windows, behind the thin shim `libs/README.md` asks for, plus hand-written
Latin-1 and both UTF-16 byte orders because each is a byte loop and delegating
them would be two more places to be wrong. `encoding_decode` refuses malformed
input and names the byte; `encoding_decode_lossy` replaces it with U+FFFD,
**because that is what the C# server being replaced does** — a `StreamReader`
over GBK — and a faithful port has to be able to do the same.

### Measured, not asserted

Against the leaked server's own data files
(`reference/gw2.0/Server Files (Compiled)/gameserver/Item/`), 2026-09-12:

| | |
|---|---|
| `ItemBaseAttribute.xml` | 535 elements, 7,164 attributes |
| `EquipForge.xml` | 177 elements, 1,506 attributes |
| `ItemAppendAttribute.xml` | 47 elements, 736 attributes |
| `Revive.xml` / `ItemShop.xml` / `Consortia.xml` / `BijouForge.xml` | 202 / 180 / 27 / 16 elements |

**Every element name, every `ID`, and all 10,362 attributes are byte-identical
to Python's `ElementTree`** reading the same files through iconv. The decoder's
output is **byte-identical to iconv** on `ItemBaseAttribute.xml`,
`EquipForge.xml` and `Consortia.xml` (134,248 bytes for the first), and the
Chinese in its comment block reads back as 武器 / 头盔. A round trip
`decode → encode` returns the original 134,212 bytes.

`cli/tests/datafiles.rs` holds the four tests that keep it that way, and the
fixtures are written to disk as bytes rather than as source strings, because a
GBK pair that survives a temporary file is the property that matters.

Two things this pair does *not* do, said here so nobody looks for them:
`xml_parse` takes a byte-set rather than a path — `file_read_bytes` is the
reader and this is the parser, so there is one place that opens a file; and
neither library decodes an image, so the console's icon atlases remain unread
(see the last section).

---

## 2. `db` inside the pump — DONE, 2026-09-12

The one prerequisite the login record named and deliberately did not build
(`gbo-port-prerequisites.md:69`). LoginServer ran four statements per login; the
game server writes on every kill, pickup, equip, forge and quest, on a 10 Hz
tick, and `db_*` was synchronous: a handler that called `db_query` held the pump
— every other client's frames — until the server answered. On loopback that is a
millisecond and invisible, which is exactly how the 0x2711 timing bug survived
every local run.

**Built as a worker thread inside `libs/db`, behind an asynchronous surface**,
with one rule that shapes all of it: **the worker never calls into the
runtime**. No `kn_malloc` (the collector traces the program's stack), no
`kn_error_set` (one slot, two writers), no `kn_handle_new` (the handle table).
Everything a request owns is plain `malloc`, and every handle a program sees is
built by the program, on its own thread, when it claims the answer.

| | |
|---|---|
| `db_exec_async`, `db_exec_async_n`, `db_query_async`, `db_query_async_n` | queue a statement and answer a request id, then return |
| `db_req_ready` | the poll — false while it is still running, with the slot clear |
| `db_req_rows`, `db_req_columns`, `db_req_text`, `db_req_is_null`, `db_req_error` | the reading side |
| `db_req_free` | releases the answer; refused while the request is still running |

Three consequences worth stating, because each was a decision:

- **A query's rows are collected by the worker while it runs**, so the answer is
  a value the program claims rather than a cursor over a driver that is gone.
  That is also why the synchronous reading surface (`db_next`, `db_text`,
  `db_int`) is untouched by any of this — the tested path did not move.
- **A connection with a request in flight is the worker's.** The synchronous
  surface, the transaction commands and `db_close` refuse it by name. Two
  threads on one connection is a crash in the MySQL client and a corrupt
  statement in SQLite, not a race to be survived.
- **The worker runs the same code the synchronous path runs.** The two shared
  bodies were split so that `db_exec_conn` and `db_query_rows` take the
  connection directly and `db_rows_next` is the one row step, rather than a
  second implementation free to drift from the tested one. The parameter list a
  request carries is a plain-`malloc` copy **in the ABI's array layout**, so the
  existing binding code is literally the code that runs.

### Measured, not asserted

`cli/tests/db.rs::a_slow_statement_does_not_hold_the_event_loop`, and the probe
it was written from: a three-million-row recursive CTE (780 ms) queued
asynchronously, with a 20 ms timer counting turns of the loop.

```text
queued job ...; finished already: false
synchronous use while in flight -> -1: this connection has an asynchronous request in flight ...
ready after 39 tick(s): 1 row(s), 1 column(s), first cell 3000000, null=false, error=''
freed: true
```

**Thirty-nine turns of the loop during a statement that took most of a second.**
A synchronous call gets one, because the statement runs inside the turn that
asked for it. That is the done-when the login record set — "a probe that holds
one client's query for a second while a second client's login completes
unhindered" — taken one level down, at the library, where it is a test in the
suite rather than a session with two clients.

What is **not** claimed: the worker is one thread, so two slow statements queue
behind each other — a pool is a later change if a measurement asks for one. The
thread is started on the first asynchronous call, so a program that never uses
this surface (the login server) never creates one. And the Windows branch of the
thread shim is thin but **unverified on this machine**: `libs/db` cannot be
cross-compiled with the client headers, so what is checked is the configuration
that matters there — Windows without the database clients, which builds clean.

---

## 3. Decided *not* to be blockers

Written down so they are not re-litigated, and each has a reason rather than a
preference.

- **Threads in the language.** Kiln has none, and the port should not want them.
  `WorldTick` simulates and then fires detached per-session work behind a
  semaphore gate; in Kiln that collapses to one synchronous pass — simulate,
  walk the sessions, frame, `tcpserver_send_bytes` — with no gate, because
  nothing else runs concurrently. The property the .NET design bought with locks
  and detached tasks is already there: `net_peer_send` buffers per peer up to
  16 MB and **refuses rather than blocking**, so one slow client cannot stall
  the pump. And the .NET server's one confirmed fatal incident was a diagnostic
  racing itself on a `Timer` callback, which is an argument for the single pump,
  not against it.
- **Dictionary keys are text, and a library cannot take or return a record or a
  dictionary.** The world is keyed by entity id, map id, character id, template
  id, so this is real friction — but the answer is architectural, not a language
  change: the world's structures (`SpawnDirectory`, `LiveWorld`, `MapSessions`,
  `StatusBook`, `QuestCatalogue`, `DropTables`) live in the program as units,
  where they were going to live anyway because they hold records, and only
  `db`, `net`, `xml` and `encoding` are libraries. Keys become `int_to_text()`.
- **`packed` c-records.** The four login frames fit naturally and were proven
  to; the client's own code never uses `StructLayout` at all, it hand-writes
  spans, so the frames that do not fit naturally are frames a program already
  writes byte-wise. Per-frame work at port time, not a language feature to
  build in advance. If one frame turns out to need it, `is packed` is a small
  change to the c-record path and a large one to the record *value* path, and
  nothing suggests it yet.
- **Regex.** 26 sites in the C# use it, nearly all for scraping — and there is
  now a real XML reader and a real INI reader, which is what most of those sites
  were standing in for. The remaining consumer is the 2008 server's Python NPC
  scripts, which is a scraping job `text_` commands do. A regex engine is a
  library of its own and nothing here needs one.

---

## 4. What the port still needs from Kiln, in the order it will be wanted

1. **Image decode** for the management console, and only for it: the item icons
   are cut from 1024×1024 `.gwo` atlases on a 36×36 grid, the minimaps and the
   inventory panel backdrop are the same shape. The server itself does not need
   a pixel; `GameAdminHost`/`GameAdminInventory`/`GameAdminWorld` do, and they
   are the last thing worth building anyway.
3. **The NPC scripts.** 362 Python files, 17,913 lines, under
   `gameserver/Script/`. GBO itself does not interpret them: it scrapes flag
   constants and return values, and prefers `NpcFunctions` / `NpcShops`
   measured from a capture of the live server. A port can carry that same
   scraper — `text_` commands are enough for it — and should, rather than
   embedding an interpreter, until something proves the scripts are the better
   source.

## The rule this page was written under

Measure before concluding, the same one the other port record ends on. Every
claim in section 1 was produced by running something; every item in section 3
names what would change the decision; and section 2 is open because the number
that would settle it has not been taken yet.
