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

## Measured against Kiln 1.0.1 — 2026-09-07

The page below was written on 2026-09-06 from reading. This section is what a day of probing the
built binary (`target/release/kiln`, commit `3c547c7`) against the real LoginServer source turned
up, and it is ordered as **work items**: each one says what to change, where, and how to know it is
done. When it was written, nothing on the 09-06 list had been implemented — `libs/` was
`config file hash hello json math net process random system text time ui`, and `tcpserver` only
delivered text. All three are now done.

Two things are better than the page said, one is worse, and one is missing from it entirely.
**All three items have since been implemented** — see their headings below.

| | 09-06 said | 09-07 measured |
|---|---|---|
| Byte-set delivery | "a change to the delivery path and the `receive` event's parameter type in one C file" | **True, and it is C only.** The compiler already carries a `bytes` event parameter end to end (see item 1). |
| Send path | not mentioned | **Also truncates.** `tcpserver_send` measures its argument with `strlen`; a reply of `"A\0B\0C"` reached the peer as one byte. Half of item 1. |
| C-struct records for wire frames | "close to a perfect fit" | **Exact fit for all four login frames, measured** — but a c-record cannot be filled from, or turned into, a `bytes` value. Item 3. |
| The cipher | not mentioned | Every byte on the wire is XORed with a 256-byte keystream, per direction, position advancing. Not a Kiln gap, but it is *why* the program must own reassembly. See "Not blockers". |

### The probes, so the numbers can be rerun

Each is a whole program, kept in `docs/gbo-port-probes/`; the result is what the built binary
printed. P3 is *meant* to fail to build — it is the checker error, kept verbatim.

**P1/P2 — both directions truncate at NUL.** A `tcpserver` with `delimiter = ""`, sent
`printf '\x04\x00\x11\x27'`, whose handler prints `length(data)` and replies `"A\0B\0C"`:

```text
received length=1        <- 4 bytes in, 1 delivered
41                       <- od of what nc got back: one byte, "A"
```

**P3 — a `bytes` handler is refused at the checker, verbatim:**

```text
line 12: `s`: event `receive` hands a handler (int, text), but `on_receive` takes (int, bytes)
— take exactly those, or none: `sub on_receive(n: int, s: text)`
```

That is the declared descriptor doing its job, not a type-system gap: the parameter tags live in
`libs/net/net_libinfo.c` (`EV_IT = { KN_SDT_INT, KN_SDT_TEXT }`).

**P5 — what the byte commands do with a NUL.** `text_from_bytes` on a 32-byte set with three
bytes set answers a text of length 3 — it **stops at the first NUL**, which is exactly the
fixed-width-field read the protocol needs (`text_from_bytes(body[1..32])` is the account name).
`bytes_from_text("A\0B")` answers **1 byte** — text is a C string, so `bytes` can never be built
*from* text where a NUL is meant. A dictionary of records (`var sessions: session{}`) compiles and
works; per-client session state is not a gap.

**P7 — the four login frames as `is c` records come out at their wire sizes**, with natural
alignment and no `packed` attribute:

```text
msg_head              4     (word size, word type)
login_request       136     (byte[32] ×2, int, byte[32] ×2, int)
response_game_server 68     (byte, byte[32], int, byte, byte[25])  — the int lands at +36 because 33 pads to 36
realm_record         48     (word, word, byte[36], eight bytes)
```

and `port = 6001` written into `response_game_server` reads back as `113, 23` at +36/+37 — little
endian, right offset. The fit is luck, not design: `response_game_server` only works because the
padding after a 33-byte prefix happens to be the 3 bytes the protocol leaves there. A frame with a
`u32` at an odd offset would not fit, and there is no way to say `packed`.

---

### Item 1 — `tcpserver`/`tcpclient` byte-set delivery, both directions — **done, 2026-09-07**

**Shipped.** `receive_bytes` and `tcpserver_send_bytes` / `tcpclient_send_bytes` exist, and
`docs/gbo-port-probes/p2_bytes_round_trip.kiln` is the rerun: the same
`printf '\x04\x00\x11\x27'` now prints `received length=4` and the peer gets `04 00 11 27`
back, where before it was 1 byte each way. `receive` is untouched, so `examples/tcpecho.kiln`,
`examples/tcpchat.kiln` and every sample on the networking page still build and still behave
as they did.

It was C only, as predicted: `libs/net/net_tcp_component.c` and `libs/net/net_libinfo.c`, nothing
in `ir/`, `cli/` or `backend/`. One thing the plan below did not foresee — `net_peer_deliver`
copies the unit into a scratch buffer and frees it after the callback, rather than handing over
the `net_text` allocation, because the two exits want different values built from the same bytes
and the bytes must leave the input buffer before user code runs.

The rule as implemented: `receive_bytes` wins when both are wired, and `receive` does not fire.
The delimiter still decides what a unit is and is still stripped; a partial unit left by a peer
that closed is still delivered.

What remains from this item's own text: the plan below is kept as written, because the reasoning
for the design is still the reasoning, and because the "not part of this item" note at the end is
still true — TCP still splits and merges, and the accumulator is program code.

**The plan as written on 09-07.** The 09-06 page had the receive half; the send half is new.

**Where.** `libs/net/net_tcp_component.c` and `libs/net/net_libinfo.c`. Nothing in `ir/`, `cli/`
or `backend/`: this was checked, not assumed —

- `cli/src/libload.rs:671` reads each event parameter tag through `Ty::from_sdt_tag`, and
  `ir/src/lib.rs:413` maps tag `10` to `Ty::Bytes`;
- `backend/src/lib.rs:888` (`handler_symbol`) builds the thunk from those types, and `llvm_ty`
  (line 133) lowers `Ty::Bytes` to `ptr`, the same as `Ty::Text`.

So a descriptor that says `{ KN_SDT_INT, KN_SDT_BIN }` gets a handler typed `(int, bytes)` and a
thunk that passes the byte-set pointer, with no compiler change.

**Design call: add an event, do not change `receive`.** Changing `receive`'s parameter type
would break `examples/tcpecho.kiln`, `examples/tcpchat.kiln` and every `tcpserver` sample in
`docs-site/src/networking.md` — all of which `tools/check-docs.sh` compiles. Add instead:

| | |
|---|---|
| event `receive_bytes` on `tcpserver` | `(client: int, data: bytes)` |
| event `receive_bytes` on `tcpclient` | `(data: bytes)` |
| command `tcpserver_send_bytes(server, client, data: bytes) -> bool` | measured by `Kiln_Bin.len`, never `strlen` |
| command `tcpclient_send_bytes(client, data: bytes) -> bool` | same |

Rule: when `receive_bytes` is wired the peer's chunk goes there as a byte-set and `receive` does
not fire; when only `receive` is wired nothing changes. With a delimiter set and `receive_bytes`
wired, deliver the unit **without** the delimiter as bytes, the same contract as `receive`. That
keeps one delivery path (`net_peer_deliver`) with two exits rather than two paths.

**The C change, concretely.**

- `net_peer_deliver` calls `net_text(p->in.p, n)` (a malloc of `n+1` and a NUL) and hands a
  `char *` to `NetDeliverFn`. Give the deliver callback the raw `(const char *p, size_t n)` and let
  the two exits build what they need: `net_text` for `receive`, `kn_bin_new(n)` + `memcpy` into
  `(Kiln_Bin *) + 1` for `receive_bytes`. `kn_bin_new` is in `runtime/kiln_core.h`, not the ABI
  header — `libs/file/file_cmds.c:48` documents including `kiln_core.h` for exactly this.
- `net_peer_flush_partial` (the "peer closed without a delimiter" case) takes the same route.
- Two new handler typedefs beside `NetIdTextFn`: `void (*)(int32_t, void *)` and
  `void (*)(void *)`. The thunk's parameter is an LLVM `ptr`, so `void *` is the honest C type.
- `tcpserver_send_bytes`: `Kiln_Bin *b = kn_arg_ptr(argv, 2); net_peer_send(&c->peer,
  (const char *)(b + 1), (size_t)b->len)`. `net_peer_send` already takes a length; only the text
  wrapper around it uses `strlen`.
- `net_tcpserver_on` / `net_tcpclient_on`: one more `strcmp` each.
- `net_libinfo.c`: the two event rows (`EV_IB = { KN_SDT_INT, KN_SDT_BIN }`, `EV_B`), the two
  command rows with `KN_SDT_ARRAY`-free `KN_SDT_BIN` argument tags, and a `doc`/`example` on each
  so `check-docs` compiles the example.

**Done when** P1/P2 rerun through `receive_bytes` and `tcpserver_send_bytes` print
`received length=4` and the peer sees `04 00 11 27` back; and `cargo test` and
`tools/check-docs.sh` still pass with `receive` untouched.

**Not part of this item, and worth saying:** TCP still splits and merges frames. The Kiln program
keeps a per-client `bytes` accumulator, reads the 2-byte little-endian length at +0, and dispatches
one frame at a time. That is program code, not library code — the `.NET` server's `FrameBuffer`
is 90 lines and the Kiln one will be shorter.

### Item 2 — `libs/db`, scoped to what the login slice actually runs — **done, 2026-09-07**

**Shipped, and the done-when is met.** Against a MariaDB server seeded with the login schema, a
Kiln program opened `mysql://gbo:secret@127.0.0.1:13306/godswar`, ran
`select id, username, password_hash, last_login_ip from accounts where username = ? limit 1` with
the name bound, and printed the hash — with the parameter never appearing in the SQL text. The
`UPDATE` that writes NULL ran through `db_exec_n` and read back `db_is_null` true, and
`ada' or '1'='1` matched nothing.

The surface is as designed, plus `db_columns(r)` — a result whose column count a program cannot
ask for makes `db_text(r, n)` a guess. SQLite and MySQL both work; SQLite is what
`cli/tests/db.rs` exercises, because `:memory:` needs no server, no file and no credentials, and
the five cases there are the four login statements, the NULL distinction, the injection payload,
and every way a handle can be wrong.

Two departures from the plan above, both deliberate:

- **The two client libraries are one optional group, not two.** A `lib.json` carries one
  `optional_*` set, so `KILN_DB` is defined only when both headers are present. A machine with
  sqlite3 and no libmariadb gets neither, which is worse than it should be; splitting them needs
  the manifest to grow a second optional group, and that is a change to `cli/src/libload.rs`
  rather than to this library. Noted rather than hidden.
- **MySQL queries go through `mysql_real_escape_string`, not `mysql_stmt_bind_param`.** The
  prepared-statement result path needs a bind buffer per column sized for the widest value in it,
  which the text-shaped surface would then convert back to text anyway. `db_exec` *does* use
  prepared statements and real binds; `db_query` escapes against the live connection and
  substitutes, which is charset-aware and injection-safe, and is what the test's payloads check.
  The day a column needs a typed read, the query path changes with it.

**The plan as written on 09-07.**

**The gate, unchanged — but smaller than "a database library".** Everything LoginServer does
against MySQL is four statements through two shapes of call:

| shape | statement |
|---|---|
| query, one row | `SELECT <account columns> FROM accounts WHERE username = ? LIMIT 1` |
| execute | `UPDATE accounts SET last_login_at = ?, last_login_ip = ?, last_login_mac = ? WHERE id = ?` |
| execute | `DELETE FROM handoff_tickets WHERE expires_at < ?` |
| execute | `INSERT INTO handoff_tickets (...) VALUES (?, ?, ?, ?, ?) ON DUPLICATE KEY UPDATE ...` |

(`GodsBattle.Core/Data/Accounts/AccountRepository.cs` and
`GodsBattle.Core/Sessions/MySqlHandoffTicketStore.cs` in the GBO repo, verbatim.) No transaction,
no bulk insert, no scalar-returning insert on this path. `BulkInsertAsync` and `LAST_INSERT_ID()`
are GameServer's and the admin API's, and can wait.

**A surface that fits Kiln's handle model** (small positive ints, `KN_HK_*` kinds, the error slot):

```text
db_open(dsn: text) -> int                       a connection handle; 0 + error slot on failure
db_close(h: int) -> bool
db_exec(h: int, sql: text, params: text[]) -> int      rows affected; -1 on failure
db_query(h: int, sql: text, params: text[]) -> int     a result handle; 0 on failure
db_next(r: int) -> bool                         advance to the next row
db_text(r: int, column: int) -> text            column value of the current row, by 1-based index
db_int(r: int, column: int) -> int
db_int64(r: int, column: int) -> int64
db_is_null(r: int, column: int) -> bool
db_result_close(r: int) -> bool
```

`params: text[]` is the deliberate choice from 09-06 — **binding is the only way a value reaches
SQL**, and there is no `db_exec(h, sql)` without a parameter list, so the idiomatic path cannot
concatenate. Passing every parameter as text and letting the driver coerce is what
`MYSQL_STMT` with `MYSQL_TYPE_STRING` binds do anyway, and it is what keeps the surface to one
type. A typed `db_exec_int` family can come later if a column ever needs it.

**NULL has to be writable, and `text[]` cannot say it.** The one UPDATE on this path writes
`last_login_ip` and `last_login_mac` as **NULL, not `""`**, when the client sent nothing
(`AccountRepository.RecordSuccessfulLoginAsync` → `Truncate(...)` answers null on empty), and
`db_is_null` above only covers the read side. Decide it in the surface rather than at the first
port: add a companion `db_exec_n(h, sql, params: text[], nulls: bool[]) -> int` (and `db_query_n`)
where `nulls[i] = true` binds parameter `i` as SQL NULL whatever `params[i]` holds. The two-array
form is what keeps the plain call one type wide. The alternative — the Kiln port writing `""`
where .NET writes NULL — is drift the website's admin pages would show as a blank IP rather than
"never", so it is not taken.

**Implementation notes, measured on this machine.**

- Two handle kinds are needed and the header says kinds are assigned in `abi/kiln_abi.h`, not in
  library sources: add `KN_HK_DB = 7` and `KN_HK_DB_RESULT = 8` there (`7..15` are unassigned).
- `/usr/include/mysql/mysql.h` and `/usr/include/sqlite3.h` are both present;
  `pkg-config --libs libmariadb sqlite3` answers `-L/usr/lib64/ -lmariadb -lsqlite3`.
- Follow `libs/net/lib.json`'s `optional_requires` / `optional_feature` /
  `optional_link_args` pattern so a checkout without the client headers still builds the
  library and `db_open` fails loudly at run time with `KN_ERR_UNSUPPORTED`. `libs/db` becomes the
  second library with a `lib.json`.
- One connection per handle, opened by `db_open`, is enough for the login slice: the program is
  single-threaded and the pump is the only caller. The "one connection per unit of work, from a
  pool" advice from 09-06 is about GameServer's concurrency and can wait with it.
- SQLite first is still the right first backend to prove the surface — every command above is a
  thin wrapper over `sqlite3_prepare_v2` / `sqlite3_bind_text` / `sqlite3_step` — and MySQL
  second over `mysql_stmt_prepare` / `mysql_stmt_bind_param` / `mysql_stmt_fetch`. The DSN
  prefix (`sqlite:` / `mysql:`) picks the backend so the Kiln program does not change between
  them.

**Done when** a Kiln program can `db_open("mysql://gbo:...@127.0.0.1/godswar")`, run the
`SELECT ... WHERE username = ?` with a bound name, and print the `password_hash` column, with
the parameter never appearing in the SQL text.

### Item 3 — a bridge between `bytes` and a c-record — **done, 2026-09-07**

**Shipped, and it is the win the 09-06 page promised.**
`bytes_from_ptr(p, count)`, `bytes_copy_to_ptr(b, p)` and `bytes_concat(a, b)` are core commands
in `runtime/kn_array.c`. `docs/gbo-port-probes/p8_bytes_record_bridge.kiln` is the proof: a
136-byte `login_request` filled from a byte-set in one call, every field then read by name, taken
back out as bytes to send, and a header and a body joined into one write. It prints exactly what
its comments predict.

So `LoginRequest.Read` is now what the page said it would be — `var req: login_request` and
`bytes_copy_to_ptr(body, address of req)` — rather than a `bytes_at` loop over offsets.

Neither command can check that the address has `count` writable bytes behind it: a `ptr` is an
address the program vouched for, exactly as `mem_copy`'s is. They check the side they own — a
negative count, a null address, and the byte-set's own length.

`bytes_read_u16le` and the `u32` pair are still not written. `bytes_concat` covers the join;
the length prefix is still two `bytes_at` calls and a shift.

**The plan as written on 09-07.**

P7 shows the frames fit. What is missing is any way to get the bytes the socket delivered *into*
the record, or the record *onto* the wire:

- `address of rec` is a `ptr`, and `mem_copy(ptr, ptr, n)` exists, but **a `bytes` value has no
  address a program can name** — no `bytes_ptr`, no `bytes_from_ptr`.
- So today a decode is a `bytes_at` loop and an encode is a `bytes_new` + `bytes_set` loop, one
  byte at a time, which is the offset-by-offset arithmetic the c-record was supposed to replace.

Two commands close it, both in the core (`runtime/kn_array.c`, beside `kn_bin_slice`):

```text
bytes_from_ptr(p: ptr, count: int) -> bytes     copy `count` bytes out of an address
bytes_copy_to_ptr(b: bytes, p: ptr) -> int      copy the byte-set to an address; the count copied
```

With those, `LoginRequest.Read` is `var req: login_request` / `bytes_copy_to_ptr(body, address of
req)` and every field is a name. **Ship item 1 and 2 first; this one turns the port from parity
into the demonstrable improvement the 09-06 page promised.**

While in `kn_array.c`, the byte toolkit is six commands (`bytes_at`, `bytes_count`,
`bytes_from_text`, `bytes_new`, `bytes_set`, `bytes_slice`, plus `text_from_bytes`). A
`bytes_concat(a, b)` and `bytes_read_u16le / bytes_write_u16le` (and the `u32` pair) would take the
frame-header code from a dozen lines to two. Not needed for the port; noted so nobody re-derives
the list.

### Not blockers — settled here so they are not re-litigated

- **The cipher.** `PacketCipher` (`GodsBattle.Core/Crypto/PacketCipher.cs`) is a 256-byte XOR
  keystream (whose second half repeats the first), one instance per direction per connection,
  position advancing by one per byte and wrapping at 256. The client applies it to the *whole*
  stream including the 4-byte header, which is why delivery has to be raw chunks: the length
  prefix is unreadable until decrypted. In Kiln: a 256-entry `int[]`, a per-client `rx_pos` /
  `tx_pos` in the session record, and a `bytes_at` / `bytes_set` loop. No library needed.
- **`StringShift`** (the account name is letter-shifted by a table keyed on its length) and the
  MD5 credential compare are pure table and string code. `hash_md5` exists but is not needed: the
  client sends the MD5 as text and the server compares it, case-insensitively, to the stored
  column.
- **The 750 ms handoff delay** (`ServerListHandler.GameHandoffDelay`, the 0x2711 trap in the
  09-06 page) must **not** be `sys_sleep_ms` — that stalls the pump and every other client with
  it. A core `timer` at 50 ms walking a `pending{}` dictionary of `{client, due_tick, frame}` and
  sending what is due is the whole mechanism, and the loop already interleaves timers with
  sockets (`kn_loop_add`, period per source).
- **The login throttle** (`LoginThrottle.cs`: free failures, exponential delay, a block at 20)
  is a dictionary keyed on the address text. The delay is the same pending-list, not a sleep.
- **The handoff token** — 15 characters from a 62-letter alphabet — is a `random_between` loop
  over `use random`.
- **Threading.** Settled on 09-06, and the login slice never needed it: one pump, one timer.
- **Out of scope for the slice:** the admin endpoint (`LoginAdminHost`, `AdminService`), the
  fan-out log, `CharacterHandler` (a stub in .NET too), the SIGTERM plumbing (Kiln's `quit()` and
  the loop's own exit cover it), and constant-time comparison.
- **Settings.** `Settings.ini` is an ini; `use config` reads it, and `env_get("GODSWAR_SETTINGS")`
  (`use system`) reads the override the container sets.

### The order, revised

1. **Item 1** — an afternoon, C only, testable with `nc`. Rerun P1/P2.
2. **Item 2** — SQLite backend first to prove the surface, then MySQL; the login slice needs the
   MySQL one against the real `godswar` schema (`docs/schema.sql` in the GBO repo).
3. **Port LoginServer** — `Program.cs`, `LoginHandler.cs`, `ServerListHandler.cs`, plus the
   Core pieces it pulls in: `PacketCipher`, `StringShift`, `MsgHead`, `LoginRequest`,
   `LoginResponse`, `ResponseGameServerMessage`, `HandoffToken`, `AccountRepository.Authenticate`,
   `MySqlHandoffTicketStore.Issue`. About 1,400 lines of C# whose Kiln form should be well under
   half that.
4. **Item 3** — after the port works, as the refactor that makes it read the way it should.
5. The acceptance test is unchanged: `tools/verify-login-fix.sh` in the GBO repo, ten clean logins
   from a fresh client launch against the Kiln login server with the .NET GameServer behind it.


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

*Written 2026-09-06 from reading. The section above is the measured pass; where the two disagree,
the measured one wins.*

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
