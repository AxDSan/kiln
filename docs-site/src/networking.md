# Networking

`use net` gives a program TCP, an HTTP client, and an HTTP server. The server
is a **component**: you drop it on a form or declare it at module level, set a
port, wire one event, and you have a web service. There is no socket in the
program text anywhere.

```
module hello_web
use net

httpserver site
  port = 8080
  on request: on_request
end

sub on_request
  call route(net_request())
end

sub route(req: int)
  call net_req_reply(req, 200, "you asked for {net_req_path(req)}")
end

sub main
  call print_text("serving on http://127.0.0.1:8080")
end
```

`main` prints one line and returns, and the program keeps running. A server is
a live event source, exactly like a timer, and the runtime's loop stays in it
until something calls `quit`. `examples/webserver.kiln` starts you here.

## The request is a parameter, not a place

`request` is one of the events that hands its handler nothing — deliberately,
so the request is fetched by the code that is about to use it — and the
handler asks for the one being dispatched and hands it on:

```
sub on_request
  call route(net_request())
end

sub route(req: int)
  ...
end
```

That indirection is the point. A request kept in a module variable or a
component property would answer the first caller correctly and then quietly
serve the second caller the first one's path, headers and body — a bug that
passes every test written with one browser tab open.

`req` is a handle, and it is retired the moment the handler returns. A program
that saves one and uses it on the next request is told the handle is stale
(`last_error_code()` is `10002`) instead of being handed someone else's
connection.

`net_request()` outside a handler is a failure, not an empty answer: it returns
0 and sets error `10005`.

## Reading a request

| Command | Answers |
|---|---|
| `net_req_method(req)` | `GET`, `POST`, … |
| `net_req_path(req)` | the path, with the query string removed |
| `net_req_query(req, name)` | one query parameter, percent-decoded |
| `net_req_header(req, name)` | one header, matched case-insensitively |
| `net_req_body(req)` | the request body |

`net_req_query` and `net_req_header` answer `""` for something the client did
not send. That is a genuine "no", not a failure: `last_error_code()` stays 0,
which is the whole reason an empty answer is readable here.

## Answering

```
call net_req_reply(req, 200, "hello")
call net_req_reply_as(req, 200, "application/json", "{{\"ok\":true}}")
```

`net_req_reply` sends `text/plain; charset=utf-8`; `net_req_reply_as` takes the
content type. Both answer once — a second reply on the same request fails
rather than sending two.

A handler that replies to nothing still answers: **200 with an empty body**. A
route you forgot is then a blank page, not a browser spinning until it gives
up.

## The defaults, and why they are what they are

- **`bind` is `127.0.0.1`.** The server is reachable from this machine only
  until someone writes `bind = "0.0.0.0"` on purpose. A RAD tool whose default
  put a half-written service on every interface would be shipping the mistake.
- **A request body is capped at 1 MiB**, and a request head at 64 KiB. Past
  either, the client gets `413` or `431` and the handler never runs — handing a
  program half a request would be the wrong kind of honesty.
- **A chunked request is refused with `501`.** A body sent with
  `Transfer-Encoding: chunked` has no length to check, and dispatching it
  would hand the handler an empty body with no error — the one failure shape
  nothing here is allowed to have.
- **A connection that goes quiet for 15 seconds is dropped**, so a client that
  connects and says nothing cannot hold a slot.
- **16 connections at a time, per server.** The 17th is refused rather than
  queued: an unbounded backlog of half-read requests is how a small server
  becomes a memory leak with a port number.
- **Every response says `Connection: close`.** There is no keep-alive, because
  a server that promises to reuse a connection and then does not is worse than
  one that never promised.

## Nothing blocks

The server registers one pump with the runtime's event loop and does a slice of
work per turn: accept what is waiting, read what has arrived, dispatch what is
complete, write what it can. No thread is started and no call waits. A form
with a server on it keeps repainting while it serves — that is what the loop is
for.

The consequence worth knowing: **a handler runs on the same turn as the rest of
the program.** A handler that sleeps or does a slow HTTP call of its own stops
everything else, including the window. Keep handlers short.

Calling `quit()` from inside a handler works — the reply that handler already
set is flushed before the loop stops — but any *other* connection mid-response
is dropped with it.

## TCP components

Below HTTP there is plain TCP, and it has the same shape: two non-visual
components, `tcpserver` and `tcpclient`, the pair a Delphi programmer knows as
`TIdTCPServer` and `TIdTCPClient`. Drop one from Studio's toolbox or declare it
at module level, set a port, wire the events, switch it on with `active`.

Here is a whole echo server. It is `examples/tcpecho.kiln`, and it runs as
`kiln run examples/tcpecho.kiln 7000` with `nc localhost 7000` in another
terminal:

```
module tcpecho
use net
use system

tcpserver echo
  name = "echo"
  on connect: on_connect
  on disconnect: on_disconnect
  on receive: on_receive
  on error: on_error
end

sub main
  echo.port = text_to_int(sys_arg(1))
  echo.active = true
  call print_text("echo server on port {echo.port}")
end

sub on_connect(client: int)
  call print_text("connect {client}")
  call print_text("  from " + tcpserver_client_address("echo", client))
end

sub on_disconnect(client: int)
  call print_text("disconnect {client}")
end

sub on_receive(client: int, data: text)
  if data = "quit"
    call tcpserver_send_all("echo", "bye\n")
    call quit()
  else
    call tcpserver_send("echo", client, "echo: {data}\n")
  end
end

# A port already in use lands here. With no `error` handler wired the server
# would say so on stderr and stop the program instead.
sub on_error(message: text)
  call print_text("error: {message}")
  call quit()
end
```

Three things in it carry the whole design.

**The events are typed.** Every server event names the client it is about —
a small int, counted from 1 in order of arrival and never reused within a run
— and `receive` adds the line. A handler declares those parameters or none,
exactly as a `timer`'s `tick` handler may take the tick count or ignore it;
the compiler checks the header against the component. A client id that has
gone is refused as stale (`last_error_code()` is `10002`) rather than quietly
meaning whoever connected next.

**A line is the unit.** `delimiter` is a newline by default, and `receive`
fires once per complete line with the delimiter stripped — a client that sends
half a line waits, unseen, until the rest arrives. Set it to `"\r\n"` for a
protocol that insists, or to `""` to be handed whatever bytes arrived, as they
arrived. What is left when a peer closes without a final delimiter is delivered
as one last `receive`, then `disconnect`.

**Text is the default unit, and bytes are the honest one.** `receive` hands a
`text`, and a text is a C string: it stops at the first NUL. For a line
protocol that is exactly right and costs nothing. For a binary protocol it is
data loss — a frame whose second byte is `0x00` arrives one byte long.

So a `tcpserver` and a `tcpclient` each have a second delivery event,
`receive_bytes`, which hands the same unit as a `bytes` value with every byte
of it intact, and a matching `tcpserver_send_bytes` / `tcpclient_send_bytes`
that measure what they send by the byte-set's own length rather than by
`strlen`. Both directions truncated at a NUL before these existed.

```
module frames
use net

tcpserver s
  name = "s"
  port = 9101
  delimiter = ""            # deliver whatever arrived, unsplit
  active = true
  on receive_bytes: on_bytes
end

sub on_bytes(client: int, data: bytes)
  call print_int(bytes_count(data))
  call tcpserver_send_bytes("s", client, data)     # echo it back, all of it
end

sub main
  call print_text("listening on 9101")
end
```

Wire one or the other, not both: when `receive_bytes` has a handler the unit
goes there and `receive` does not fire, because the same arrival delivered
twice under two shapes is one arrival a program would handle twice. Everything
else is unchanged — the delimiter still decides what a unit *is*, it is still
stripped, and a partial unit left by a peer that closed is still delivered.

TCP still splits and merges, so a program reading frames keeps its own
accumulator: append each `receive_bytes` to a `bytes` buffer, read the length
prefix, take one frame at a time. That is program code, not library code.

**The commands take the component's `name`.** `tcpserver_send("echo", ...)`
finds the server whose `name` property is `"echo"`, the same way `grid_cell`
finds a grid — nothing else a program can write names a component, and the
compiler hands the library no id. Set `name` to the id you declared and forget
about it; a command naming a server that has no such `name` fails with a
message saying which line to add.

### tcpserver

| Property | Default | |
|---|---|---|
| `name` | `""` | what the commands call it by |
| `port` | `0` | must be set before `active` |
| `address` | `"0.0.0.0"` | every interface; `"127.0.0.1"` for this machine only |
| `active` | `false` | `true` binds and listens; `false` tells every client and closes |
| `max_clients` | `64` | the next connection past it is closed at once |
| `delimiter` | `"\n"` | what ends a `receive`; `""` for raw chunks |

| Event | Hands the handler |
|---|---|
| `connect` | `client: int` |
| `disconnect` | `client: int` |
| `receive` | `client: int, data: text` |
| `receive_bytes` | `client: int, data: bytes` — the same unit, un-truncated |
| `error` | `message: text` |

| Command | Answers |
|---|---|
| `tcpserver_send(server, client, data)` | `bool` — queued; the pump drains it |
| `tcpserver_send_bytes(server, client, data)` | `bool` — the same, for a `bytes` |
| `tcpserver_send_all(server, data)` | `int` — how many clients it went to |
| `tcpserver_disconnect(server, client)` | `bool` |
| `tcpserver_client_count(server)` | `int` |
| `tcpserver_client_address(server, client)` | `text` — `ip:port` |
| `tcpserver_client(server, n)` | `int` — the n-th live client's id, from 1; 0 past the end |

Unlike `httpserver`, `address` is every interface: a chat server only its own
machine could reach is the surprising default here, and a `tcpserver` does
nothing at all until `active` is written, so nothing is exposed by accident.

A port that cannot be bound is an `error` if a handler is wired. If none is,
the server says so on stderr and stops the program with exit code 1, as an
`httpserver` does — a server that cannot listen must not look like one that is
running.

### tcpclient

```
module tcphello
use net

tcpclient link
  name = "link"
  host = "127.0.0.1"
  port = 7000
  active = true
  on connect: on_connect
  on receive: on_receive
  on disconnect: on_disconnect
  on error: on_error
end

sub main
  call print_text("connecting")
end

sub on_connect
  call tcpclient_send("link", "hello\n")
end

sub on_receive(data: text)
  call print_text(data)
  link.active = false
end

sub on_disconnect
  call print_text("done")
end

sub on_error(message: text)
  call print_text(message)
end
```

`active = true` connects — in the background, on the loop, so a form with a
client on it keeps painting while the connection is made. The outcome arrives
as `connect` or as `error` with a message (a refusal, an unknown host, or the
`timeout_ms` deadline, 5 seconds by default). A client that fails switches
itself off, so a console program with nothing else to wait for simply ends.
`localhost` may resolve to more than one address; every one is tried before
the client gives up.

| Property | Default | |
|---|---|---|
| `name` | `""` | what the commands call it by |
| `host` | `""` | a name or an address |
| `port` | `0` | |
| `active` | `false` | `true` connects, `false` disconnects |
| `connected` | — | read-only: `true` once `connect` has fired |
| `delimiter` | `"\n"` | as for the server |
| `timeout_ms` | `5000` | how long a connect may take |

| Event | Hands the handler |
|---|---|
| `connect` | nothing |
| `disconnect` | nothing |
| `receive` | `data: text` |
| `receive_bytes` | `data: bytes` — the same unit, un-truncated |
| `error` | `message: text` |

| Command | Answers |
|---|---|
| `tcpclient_send(client, data)` | `bool` — false with `10005` when not connected |
| `tcpclient_send_bytes(client, data)` | `bool` — the same, for a `bytes` |
| `tcpclient_connect(client)` | `bool` — `active = true` as a call |
| `tcpclient_disconnect(client)` | `bool` — false with code 0 when there was nothing to close |
| `tcpclient_connected(client)` | `bool` |

`examples/tcpchat.kiln` is a form with one of these on it: a memo for the
conversation, an editbox, and Send and Connect buttons. Run it against the echo
server above.

### What both share with the http server

Neither blocks and neither starts a thread; each is one pump on the runtime's
loop while active, and a handler runs on the same turn as everything else, so
keep handlers short. `send` queues and the pump drains, which means a handler
may answer and `quit()` in the same breath and the answer still goes out. A
peer that sends a megabyte with no delimiter in it is dropped with an `error`:
that is either the wrong protocol or an attempt to exhaust memory, and neither
is a thing to buffer through.

## The HTTP client

```
module fetch
use net

sub main
  let body: text = net_http_get("http://example.com/")
  call print_int(net_http_status())
  call print_text(net_http_header("content-type"))
end
```

`net_http_post(url, content_type, body)` is the same shape. `net_timeout_set`
bounds every connect, send and receive (10 seconds by default), because a
program that hangs forever on a dead host looks like a working program that is
merely slow. `net_http_download(url, path)` writes bytes to a file, which is
what binary content needs — text is NUL-terminated and would stop at the first
zero byte.

## https is optional, and never assumed

TLS is a dependency you opt into. Kiln links a program's libraries in, so a
TLS stack is vendored into every binary that uses one — megabytes of code and a
security-critical dependency to patch on someone else's schedule. Nobody who
only speaks `http://` should pay that, and nobody who wants `https://` should
have to talk the build system into it. So:

```sh
tools/fetch-mbedtls.sh     # once; everything else builds without it
```

With mbedTLS vendored, `net_http_get("https://...")` works and the default port
becomes 443. Without it, the same call **fails** with `KN_ERR_UNSUPPORTED`
(`10006`) and a message naming the script. Every other command, and every other
library, builds and behaves identically either way.

### What never happens

**No downgrade.** An `https://` URL is never rewritten to `http://` — not when
TLS is missing, and not when a server answers a redirect with an `http://`
`Location:`. That redirect is refused with `KN_ERR_UNSUPPORTED`. A silent
downgrade would put a password on the wire in the clear and the program that
"worked" would be the vulnerability.

**No unverified certificate.** The certificate chain is verified against the
machine's trust store and the hostname is checked against the certificate, with
no option to turn either off. An https client that skips verification is
encrypted to whoever is on the path: it offers the appearance of security and
none of it, which is worse than the honest refusal it replaced, because the
refusal is visible and this is not. A certificate that does not verify fails the
request and says why — expired, wrong name, unknown issuer.

The store is found at one of the usual locations
(`/etc/ssl/certs/ca-certificates.crt`, `/etc/pki/tls/certs/ca-bundle.crt`, and
the rest). `KILN_CA_BUNDLE` overrides it with a file or a directory, which is
what a container, a corporate proxy or a test with its own certificate needs. If
no store can be found the request fails with `KN_ERR_UNSUPPORTED` rather than
falling back to trusting everything — that fallback is the same hole arriving
through a different door.

### The server side is still plaintext

`httpserver` does not terminate TLS. Put a reverse proxy in front of it if it
needs to be reachable over https.

## Memory, over a long run

Every text a command returns is owned by the runtime and released when the
program exits, not when the value goes out of scope. That is fine for a program
that runs and stops, and it is worth knowing for a server that runs for weeks:
its memory grows with the number of requests it has answered. Restart it, or
put it behind something that will.
