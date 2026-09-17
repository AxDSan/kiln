# Networking

`using Kiln.Net;` gives a program TCP, an HTTP client, and an HTTP server. The
server is a **component**: you drop it on a form or declare it at the top of the
file, set a port, wire one event, and you have a web service. There is no socket in the
program text anywhere.

```k2
namespace HelloWeb;

using Kiln.Net;

Httpserver site
{
    Port = 8080;
    Request += OnRequest;
}

public static class P
{
    static void OnRequest()
    {
        Route(Net.Request());
    }

    static void Route(int req)
    {
        Net.ReqReply(req, 200, $"you asked for {Net.ReqPath(req)}");
    }

    public static void Main()
    {
        Console.WriteLine("serving on http://127.0.0.1:8080");
    }
}
```

`Main` prints one line and returns, and the program keeps running. A server is
a live event source, exactly like a timer, and the runtime's loop stays in it
until something calls `Quit()`. `examples/webserver.kiln` starts you here.

## The request is a parameter, not a place

`Request` is one of the events that hands its handler nothing — deliberately,
so the request is fetched by the code that is about to use it — and the
handler asks for the one being dispatched and hands it on:

```
static void OnRequest()
{
    Route(Net.Request());
}

static void Route(int req)
{
    ...
}
```

That indirection is the point. A request kept in a static field or a
component property would answer the first caller correctly and then quietly
serve the second caller the first one's path, headers and body — a bug that
passes every test written with one browser tab open.

`req` is a handle, and it is retired the moment the handler returns. A program
that saves one and uses it on the next request is told the handle is stale
(`LastErrorCode()` is `10002`) instead of being handed someone else's
connection.

`Net.Request()` outside a handler is a failure, not an empty answer: it returns
0 and sets error `10005`.

## Reading a request

| Command | Answers |
|---|---|
| `Net.ReqMethod(req)` | `GET`, `POST`, … |
| `Net.ReqPath(req)` | the path, with the query string removed |
| `Net.ReqQuery(req, name)` | one query parameter, percent-decoded |
| `Net.ReqHeader(req, name)` | one header, matched case-insensitively |
| `Net.ReqBody(req)` | the request body |

`Net.ReqQuery` and `Net.ReqHeader` answer `""` for something the client did
not send. That is a genuine "no", not a failure: `LastErrorCode()` stays 0,
which is the whole reason an empty answer is readable here.

## Answering

```
Net.ReqReply(req, 200, "hello");
Net.ReqReplyAs(req, 200, "application/json", "{\"ok\":true}");
```

`Net.ReqReply` sends `text/plain; charset=utf-8`; `Net.ReqReplyAs` takes the
content type. Both answer once — a second reply on the same request fails
rather than sending two.

A handler that replies to nothing still answers: **200 with an empty body**. A
route you forgot is then a blank page, not a browser spinning until it gives
up.

## The defaults, and why they are what they are

- **`Bind` is `127.0.0.1`.** The server is reachable from this machine only
  until someone writes `Bind = "0.0.0.0";` on purpose. A RAD tool whose default
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

Calling `Quit()` from inside a handler works — the reply that handler already
set is flushed before the loop stops — but any *other* connection mid-response
is dropped with it.

## TCP components

Below HTTP there is plain TCP, and it has the same shape: two non-visual
components, `Tcpserver` and `Tcpclient`, the pair a Delphi programmer knows as
`TIdTCPServer` and `TIdTCPClient`. Drop one from Studio's toolbox or declare it
at the top of the file, set a port, wire the events, switch it on with `Active`.

Here is a whole echo server. It is `examples/tcpecho.kiln`, and it runs as
`kiln run examples/tcpecho.kiln 7000` with `nc localhost 7000` in another
terminal:

```k2
namespace Tcpecho;

using Kiln.Net;
using Kiln.System;

Tcpserver echo
{
    Name = "echo";
    Connect += OnConnect;
    Disconnect += OnDisconnect;
    Receive += OnReceive;
    Error += OnError;
}

public static class P
{
    public static void Main()
    {
        echo.Port = TextToInt(SysArg(1));
        echo.Active = true;
        Console.WriteLine($"echo server on port {echo.Port}");
    }

    public static void OnConnect(int client)
    {
        Console.WriteLine($"connect {client}");
        Console.WriteLine($"  from {TcpserverClientAddress("echo", client)}");
    }

    public static void OnDisconnect(int client)
    {
        Console.WriteLine($"disconnect {client}");
    }

    public static void OnReceive(int client, string data)
    {
        if (data == "quit")
        {
            TcpserverSendAll("echo", "bye\n");
            Quit();
        }
        else
        {
            TcpserverSend("echo", client, $"echo: {data}\n");
        }
    }

    // A port already in use lands here. With no `Error` handler wired the server
    // would say so on stderr and stop the program instead.
    public static void OnError(string message)
    {
        Console.WriteLine($"error: {message}");
        Quit();
    }
}
```

Three things in it carry the whole design.

**The events are typed.** Every server event names the client it is about —
a small int, counted from 1 in order of arrival and never reused within a run
— and `Receive` adds the line. A handler declares those parameters or none,
exactly as a `Timer`'s `Tick` handler may take the tick count or ignore it. A client id that has
gone is refused as stale (`LastErrorCode()` is `10002`) rather than quietly
meaning whoever connected next.

**A line is the unit.** `Delimiter` is a newline by default, and `Receive`
fires once per complete line with the delimiter stripped — a client that sends
half a line waits, unseen, until the rest arrives. Set it to `"\r\n"` for a
protocol that insists, or to `""` to be handed whatever bytes arrived, as they
arrived. What is left when a peer closes without a final delimiter is delivered
as one last `Receive`, then `Disconnect`.

**Text is the default unit, and bytes are the honest one.** `Receive` hands a
`string`, and a string is a C string: it stops at the first NUL. For a line
protocol that is exactly right and costs nothing. For a binary protocol it is
data loss — a frame whose second byte is `0x00` arrives one byte long.

So a `Tcpserver` and a `Tcpclient` each have a second delivery event,
`ReceiveBytes`, which hands the same unit as a `Bytes` value with every byte
of it intact, and a matching `TcpserverSendBytes` / `TcpclientSendBytes`
that measure what they send by the byte-set's own length rather than by
`strlen`. Both directions truncated at a NUL before these existed.

```k2
namespace Frames;

using Kiln.Net;

Tcpserver s
{
    Name = "s";
    Port = 9101;
    Delimiter = "";            // deliver whatever arrived, unsplit
    Active = true;
    ReceiveBytes += OnBytes;
}

public static class P
{
    static void OnBytes(int client, Bytes data)
    {
        Console.WriteLine($"{Bytes.Count(data)}");
        TcpserverSendBytes("s", client, data);     // echo it back, all of it
    }

    public static void Main()
    {
        Console.WriteLine("listening on 9101");
    }
}
```

Wire one or the other, not both: when `ReceiveBytes` has a handler the unit
goes there and `Receive` does not fire, because the same arrival delivered
twice under two shapes is one arrival a program would handle twice. Everything
else is unchanged — the delimiter still decides what a unit *is*, it is still
stripped, and a partial unit left by a peer that closed is still delivered.

TCP still splits and merges, so a program reading frames keeps its own
accumulator: append each `ReceiveBytes` to a `Bytes` buffer, read the length
prefix, take one frame at a time. That is program code, not library code.

**The commands take the component's `Name`.** `TcpserverSend("echo", ...)`
finds the server whose `Name` property is `"echo"`, the same way `GridCell`
finds a grid — nothing else a program can write names a component, and the
compiler hands the library no id. Set `Name` to the id you declared and forget
about it; a command naming a server that has no such `Name` fails with a
message saying which line to add.

### Tcpserver

| Property | Default | |
|---|---|---|
| `Name` | `""` | what the commands call it by |
| `Port` | `0` | must be set before `Active` |
| `Address` | `"0.0.0.0"` | every interface; `"127.0.0.1"` for this machine only |
| `Active` | `false` | `true` binds and listens; `false` tells every client and closes |
| `MaxClients` | `64` | the next connection past it is closed at once |
| `Delimiter` | `"\n"` | what ends a `receive`; `""` for raw chunks |

| Event | Hands the handler |
|---|---|
| `Connect` | `int client` |
| `Disconnect` | `int client` |
| `Receive` | `int client, string data` |
| `ReceiveBytes` | `int client, Bytes data` — the same unit, un-truncated |
| `Error` | `string message` |

| Command | Answers |
|---|---|
| `TcpserverSend(server, client, data)` | `bool` — queued; the pump drains it |
| `TcpserverSendBytes(server, client, data)` | `bool` — the same, for a `bytes` |
| `TcpserverSendAll(server, data)` | `int` — how many clients it went to |
| `TcpserverDisconnect(server, client)` | `bool` |
| `TcpserverClientCount(server)` | `int` |
| `TcpserverClientAddress(server, client)` | `string` — `ip:port` |
| `TcpserverClient(server, n)` | `int` — the n-th live client's id, from 1; 0 past the end |

Unlike `Httpserver`, `Address` is every interface: a chat server only its own
machine could reach is the surprising default here, and a `Tcpserver` does
nothing at all until `Active` is written, so nothing is exposed by accident.

A port that cannot be bound is an `Error` if a handler is wired. If none is,
the server says so on stderr and stops the program with exit code 1, as an
`Httpserver` does — a server that cannot listen must not look like one that is
running.

### Tcpclient

```k2
namespace Tcphello;

using Kiln.Net;

Tcpclient link
{
    Name = "link";
    Host = "127.0.0.1";
    Port = 7000;
    Active = true;
    Connect += OnConnect;
    Receive += OnReceive;
    Disconnect += OnDisconnect;
    Error += OnError;
}

public static class P
{
    public static void Main()
    {
        Console.WriteLine("connecting");
    }

    static void OnConnect()
    {
        TcpclientSend("link", "hello\n");
    }

    static void OnReceive(string data)
    {
        Console.WriteLine(data);
        link.Active = false;
    }

    static void OnDisconnect()
    {
        Console.WriteLine("done");
    }

    static void OnError(string message)
    {
        Console.WriteLine(message);
    }
}
```

`Active = true` connects — in the background, on the loop, so a form with a
client on it keeps painting while the connection is made. The outcome arrives
as `Connect` or as `Error` with a message (a refusal, an unknown host, or the
`TimeoutMs` deadline, 5 seconds by default). A client that fails switches
itself off, so a console program with nothing else to wait for simply ends.
`localhost` may resolve to more than one address; every one is tried before
the client gives up.

| Property | Default | |
|---|---|---|
| `Name` | `""` | what the commands call it by |
| `Host` | `""` | a name or an address |
| `Port` | `0` | |
| `Active` | `false` | `true` connects, `false` disconnects |
| `Connected` | — | read-only: `true` once `connect` has fired |
| `Delimiter` | `"\n"` | as for the server |
| `TimeoutMs` | `5000` | how long a connect may take |

| Event | Hands the handler |
|---|---|
| `Connect` | nothing |
| `Disconnect` | nothing |
| `Receive` | `string data` |
| `ReceiveBytes` | `Bytes data` — the same unit, un-truncated |
| `Error` | `string message` |

| Command | Answers |
|---|---|
| `TcpclientSend(client, data)` | `bool` — false with `10005` when not connected |
| `TcpclientSendBytes(client, data)` | `bool` — the same, for a `bytes` |
| `TcpclientConnect(client)` | `bool` — `Active = true` as a call |
| `TcpclientDisconnect(client)` | `bool` — false with code 0 when there was nothing to close |
| `TcpclientConnected(client)` | `bool` |

`examples/tcpchat.kiln` is a form with one of these on it: a memo for the
conversation, an `Editbox`, and Send and Connect buttons. Run it against the echo
server above.

### What both share with the http server

Neither blocks and neither starts a thread; each is one pump on the runtime's
loop while active, and a handler runs on the same turn as everything else, so
keep handlers short. A send queues and the pump drains, which means a handler
may answer and `Quit()` in the same breath and the answer still goes out. A
peer that sends a megabyte with no delimiter in it is dropped with an `Error`:
that is either the wrong protocol or an attempt to exhaust memory, and neither
is a thing to buffer through.

## The HTTP client

```k2
namespace Fetch;

using Kiln.Net;

public static class P
{
    public static void Main()
    {
        var body = Net.HttpGet("http://example.com/");
        Console.WriteLine($"{Net.HttpStatus()}");
        Console.WriteLine(Net.HttpHeader("content-type"));
    }
}
```

`Net.HttpPost(url, content_type, body)` is the same shape. `Net.TimeoutSet`
bounds every connect, send and receive (10 seconds by default), because a
program that hangs forever on a dead host looks like a working program that is
merely slow. `Net.HttpDownload(url, path)` writes bytes to a file, which is
what binary content needs — a string is NUL-terminated and would stop at the first
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

With mbedTLS vendored, `Net.HttpGet("https://...")` works and the default port
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

`Httpserver` does not terminate TLS. Put a reverse proxy in front of it if it
needs to be reachable over https.

## Memory, over a long run

Every string a command returns is owned by the runtime, and the runtime reclaims
it once the program can no longer reach it — so a server's memory settles at
what it is actually holding rather than growing with the number of requests it
has answered. What it holds is what it stores: a static field that
accumulates a request's text per connection grows because the program is
keeping it, and no collector can tell that apart from data still wanted. See
[Memory](./memory.md).
