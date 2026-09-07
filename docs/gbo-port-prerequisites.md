# What Kiln needs before it can host a real server

A handoff from the GodsBattle Online (GBO) repositories, written 2026-09-06. GBO is a working
MMO server — a live closed beta on a VPS — written in C#/.NET 8, and the question that produced
this list was whether it could be ported to Kiln.

**The point is not to retire the .NET server.** It keeps serving the beta either way. The point is
that a binary-protocol game server with a database behind it is the most honest load test Kiln
has been offered: it exercises sockets, framing, fixed-layout records, a database, a tick loop and
long-running stability all at once, and every gap it exposes is a gap a real toolsmith language has
to close anyway. **Read this as a Kiln feature list that happens to have a demanding first
customer**, not as a porting chore.

Every number below was measured, not estimated. Where a claim comes from reading code, the file and
line are given so it can be rechecked rather than believed.

---

## What is being ported, and how big it is

| Project | Lines | What it is |
|---|---|---|
| `GodsBattle.LoginServer` | **1,370** | Account auth, realm list, handoff ticket to the game server |
| `GodsBattle.GameServer` | 25,161 | The world: sessions, tick, combat, inventory, NPCs |
| `GodsBattle.Core` | 44,363 | Protocol frames, data access, item/character models, config |
| *(total, all 7 projects)* | *78,614 over 334 files* | |

**The dependency list is two packages.** `MySqlConnector` and
`System.Text.Encoding.CodePages` — no ORM, no ASP.NET, no DI container, no serialization framework.
That is the single most encouraging fact here: there is no framework to reimplement, only a
language runtime to stand under it.

---

## The three blockers, in the order they block

### 1. There is no database library — and this is the whole prerequisite

`libs/` today is `config file hash hello json math net process random system text time ui`. Nothing
speaks to a database. Every other item on this page is small; this one is the gate.

**The surface required is smaller than it sounds.** GBO does not use MySqlConnector directly across
78k lines — it funnels everything through one interface, `IDatabase`
(`src/GodsBattle.Core/Data/IDatabase.cs`, 78 lines), with **six methods**:

| Method | What Kiln needs to offer |
|---|---|
| `ExecuteAsync(sql, params)` | run INSERT/UPDATE/DELETE/DDL, return affected row count |
| `QueryAsync(sql, params)` | run a SELECT, materialise the whole result set |
| `QueryAsync<T>(sql, map, params)` | same, projecting each row |
| `QuerySingleAsync<T>(...)` | first row or nothing |
| `ExecuteScalarAsync<T>(...)` | first column of first row |
| `BulkInsertAsync(table, columns, rows)` | one multi-VALUES statement |

The projections (`QueryAsync<T>`, `QuerySingleAsync<T>`) are C# generics over a row mapper. In
Kiln they collapse into "read the result set, then walk it" — so **the real target is closer to
four commands than six.**

**Parameters are mandatory, never string interpolation.** GBO's `DbParams`
(`src/GodsBattle.Core/Data/DbParams.cs`) exists specifically so that no call site can concatenate a
value into SQL; the interface deliberately has *no* overload taking pre-formatted SQL. Whatever
`libs/db` looks like, **bind parameters must be the only way to pass a value**, or Kiln ships an
injection vector as its idiomatic path. This is a language-design decision, not a porting detail.

Connection handling to copy rather than invent: one connection **per unit of work**, from a pool,
disposed promptly. GBO's own comment records why — the 2008 server it replaced held a single
process-wide connection open forever and recursed into `Init` with a blocking 5-second sleep on
failure, which was a single point of failure and could stack-overflow if the database stayed down.

**The build pattern already exists in this repo.** `libs/net` wraps mbedTLS as an *optional*
dependency: `libs/net/lib.json` declares `optional_requires` / `optional_feature`
(`KILN_NET_TLS`) / `optional_include_dirs` / `optional_link_args`, and `tools/fetch-mbedtls.sh`
fills the paths in. Without mbedTLS vendored the library still builds and still speaks http; https
fails loudly at run time **rather than failing everyone's build**. A `libs/db` over
libmysqlclient (or libmariadb) is that same shape, with `tools/fetch-mysql.sh` beside its sibling.

Note for whoever writes it: most libs are just `X_cmds.c` + `X_libinfo.c` (see `libs/config`, 680 +
81 lines). A `lib.json` is only needed **because** of the optional external dependency — `libs/net`
is currently the only one that has one, and `libs/db` will be the second.

*A choice worth making deliberately:* SQLite first would be easier and useful to far more Kiln
programs than MySQL. GBO's schema is MySQL, so the port eventually needs MySQL — but if `libs/db`
is designed as a gateway with a backend behind it, SQLite can land first and prove the surface.

### 2. `tcpserver` delivers as text, so binary frames truncate at the first NUL

This one was rewritten after reading the delivery path rather than its comments, and the correction
matters: **raw delivery already exists.** The gap is narrower and further in than it first looked.

`libs/net/net_tcp_component.c` gives `tcpserver` the right *shape* — `max_clients`, `connect` /
`receive` / `disconnect` / `error` events, per-client ids — which is genuinely the right model for a
game server. And `net_peer_deliver` (line 168) branches on `if (dl)`: **set `delimiter = ""` and the
delimiter scan is skipped entirely**, handing the program everything currently buffered as one
piece, every turn of the loop. The "sent N bytes with no delimiter; dropped" guard
(`net_client_step`, line 437) fires when the input buffer *fills* — which is the
delimiter-set-but-never-found case, not this one, since an empty delimiter drains the buffer every
turn. So the doc's first draft was wrong about that guard; do not go looking for a raw-bytes mode
that needs inventing.

**What actually breaks is the type.** `net_peer_deliver` calls `net_text(p->in.p, n)`
(`net_cmds.c:81`), which mallocs `n + 1` and writes `o[n] = '\0'` — a **NUL-terminated `char *`**,
delivered as text. GBO's protocol is **length-prefixed binary opcodes**: a 2-byte little-endian
length in which the high byte is `0x00` for any frame under 256 bytes, opcode words with a zero
byte, and zero padding throughout. Every one of those frames is truncated at its first NUL by any
consumer that measures the delivered text by `strlen`.

**What is needed:** deliver the chunk as a **byte-set** rather than as text. The byte-set type
(`{1, len, bytes}`) already exists in the ABI and already survives embedded NULs, so this is a
change to the delivery path and the `receive` event's parameter type in one C file — not a
type-system change, and not a new socket mode. Worth pairing with a length-prefix helper
(`read 2 bytes, then that many`), though the program can write that itself once bytes arrive intact.

TCP remains a stream either way: a frame can split across two reads and two frames can arrive in one,
so **program-side reassembly is required regardless** and the delimiter was never going to do it.

This is still the smallest item on the page and still the one to do first — it is testable in an
afternoon by sending `printf '\x04\x00\x11\x27'` at it and checking all four bytes arrive.

### 3. There is no threading, and GBO uses it

Measured on both sides:

- **Kiln:** no `pthread_create` anywhere in `runtime/ libs/ cli/ backend/`. The only thread API
  in the tree is a raw `CreateThread` declaration in `kits/win` (`kernel32_proc.kdecl`). Threading
  primitives were **item 6 of the 0.6.0 roadmap** — "a `thread`, a mutex, an atomic int, so
  concurrent programs don't drop to raw Win32/pthread" — and they did not ship.
- **GBO:** 33 `new Thread` / `Task.Run` / `Parallel.` / `Concurrent*` sites and 38 `lock (` blocks
  across `GameServer` and `Core`.

**Do not port the locks.** A large share of that concurrency exists because .NET made threads the
path of least resistance, and GBO has already been burned by it: a diagnostic (`MovementObserver`)
snapshotted a `ConcurrentBag` by reading `Count`, allocating, then copying — a player moving between
the read and the copy overran the array, and because it ran on a `Timer` callback with no caller to
catch the exception, **.NET terminated the process**. It took the whole realm down twice in one log,
mid-session, and never once on an idle test machine.

That is an argument for Kiln's existing model, not against it: **the event loop shipped in 0.4.0
plus an explicit tick is very likely the better architecture here**, and the port is the chance to
prove it. Decide this consciously — "the port needs threads" is the wrong conclusion to reach by
default.

Threads remain worth having in the language for their own sake (roadmap item 6 is still right). They
are just not a prerequisite for this, and the port should be attempted single-threaded first.

---

## Smaller gaps

**Text encodings.** GBO's second and last dependency is `System.Text.Encoding.CodePages`, for GBK —
the game client's data tables are GBK, and some are UTF-16LE. Kiln's `libs/file` and
`libs/system` already convert UTF-16 (for Windows paths — `file_cmds.c:93`, `system_cmds.c:67`), so
the machinery is partly there; what is missing is a general "decode these bytes from codepage X"
command. Narrow, and not needed until the port touches client data files.

**A tick with a real clock.** GBO's world runs on a fixed tick (`GameServer/World/WorldTick.cs`).
Kiln has `libs/time` and an event loop; whether the loop can carry a reliable periodic timer
alongside socket readiness needs checking before the GameServer slice, though not before LoginServer.

---

## What will port *better* than it is now

Worth saying, because the list above is all deficits.

**0.6.0's C-struct records with explicit layout are close to a perfect fit for wire frames.** GBO's
protocol frames are fixed-layout binary structures — the player introduction (`0x2725`), bag slot
records, the 77-field character sheet — and in C# they are hand-written span arithmetic, offset by
offset, which is exactly where its decode bugs have come from (thirty-two bytes going out as zero;
two appearance bytes read as fixed; a field misread as a direction that was a stale send buffer).
A Kiln record with a declared layout says the same thing **declaratively**. There is a real
chance these decoders come out shorter and more correct than the originals, and that is a
demonstrable win to point at, not just parity.

`docs/protocol.md` and `docs/client-opcode-map.md` in the GBO repo are the specification; 82 opcodes
are confirmed against the client's own switch table.

---

## Suggested order

1. **Byte-set delivery on `tcpserver`.** Smallest, testable against `nc`, unblocks everything else.
   (Raw *chunking* already works via `delimiter = ""`; it is the text type that has to change.)
2. **`libs/db`.** The gate. Design the parameter-only surface first; SQLite backend to prove it is a
   legitimate first step, MySQL is what the port needs.
3. **Port `LoginServer` — 1,370 lines.** It touches TCP, the database and protocol structs, which
   means it exercises items 1 and 2 for real, and it is small enough to finish.
4. **Decide about `GameServer` from what step 3 taught you.** `Core`'s protocol structs travel with
   whichever slice needs them.

### The acceptance test for step 3 already exists, and it is unforgiving

LoginServer can be tested **against the real game client**, with no mock and no harness: point the
client's `config.ini` at the Kiln build while the .NET GameServer keeps running behind it. A
successful login is a real one.

It also carries a known, *measured* trap that will cost a day if it is met cold — and it is
LoginServer's, not the game server's. `src/GodsBattle.LoginServer/Handlers/ServerListHandler.cs`
owns the `0x2711` handoff reply and **deliberately holds it back by 750 ms**. The reasoning is
worth reading in full before porting that file, but in short:

- The client builds its two character-preview avatars exactly once, when the LOGIN module is
  switched in. `MSG_ROLE_INFO` (`0x2712`) then dereferences that global **with no null check**, so
  if the module was skipped, character select takes an access violation.
- The realm list only *arms* the module switch. What converts it into a request is a per-frame tick
  that runs **only when at least 20 ms have elapsed**. The `0x2711` reply overwrites the same
  request word with a different module — so if it lands before that tick, LOGIN is skipped for the
  life of the process and the avatars are never built.
- With one realm the client auto-picks it and replies in 2-7 ms; the measured window across four
  captured logins was 11.05, 12.30, 12.93 and 15.51 ms — **all under the 20 ms gate**. Whether a
  tick falls inside is pure frame phase, which is why it crashed about half the time. The gaps
  overlap (12.30 survived, 12.93 crashed), so the cure is not "a bigger gap" but a gap long enough
  that a tick is certain.
- The original 2008 server never triggered it only because it was slow: 790-830 ms measured, against
  our 11-15 ms.

**This is exactly the class of bug a faster runtime reintroduces.** If the Kiln LoginServer is
quicker than the .NET one — and it may well be — a delay that looks like a superstitious sleep is
the only thing standing between the port and a 50% crash rate. Port the delay, and treat **"logs in
ten times in a row from a fresh launch without a crash"** as the pass condition, not "logs in once".
`tools/verify-login-fix.sh` in the GBO repo runs exactly that.

---

## The standing rule that produced this page

From GBO's own `CLAUDE.md`, and it applies here:

> **Measure before concluding.** The best findings come from a debug run, a reproducer and a census;
> the worst detours come from reasoning about what ought to be true.

Three items on this page were written wrong first and corrected by going back to the source, which
is the only reason they are right now:

- **`tcpserver`** was first written up as having no raw-delivery mode at all, on the strength of its
  comments and a grep for `"none"`. Reading `net_peer_deliver` showed `if (dl)` skips the scan on an
  empty delimiter — raw chunking was there the whole time, and the real defect (text, not bytes) is
  one function further in.
- **Threading** was nearly dismissed on the assumption that a game server is a single-threaded tick
  loop. The grep said 33 concurrency sites and 38 `lock` blocks.
- **The `0x2711` delay** was nearly filed against the *game* server, because the crash shows up at
  character select. `grep -rn 0x2711 src/` put it in `LoginServer/Handlers/ServerListHandler.cs` —
  which is the slice being ported first, so the trap is live from day one rather than deferred.

Recheck these numbers rather than trusting them; they were true on 2026-09-06.
