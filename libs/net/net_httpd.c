/* `httpserver` — an HTTP/1.1 server as a non-visual component, plus the
 * commands that read a request and answer it.
 *
 * The server never blocks and never starts a thread.  It registers ONE pump
 * with the runtime's event loop (runtime/kn_loop.c) and does a slice of work
 * each turn: accept what is waiting, read what has arrived, dispatch what is
 * complete, write what it can.  That is the whole reason the loop exists — a
 * form with a server on it keeps repainting while it serves, and a console
 * program with a server needs no window to stay alive.
 *
 * A request reaches the handler as a HANDLE, passed as a parameter, never
 * parked in a property.  A property would hold exactly one request: it would
 * appear to work, and then quietly answer the second caller with the first
 * caller's data.  The handle is generation-checked and retired the moment the
 * handler returns, so a program that saves one and uses it on the next request
 * is told it is stale instead of being handed someone else's connection.
 *
 * The defaults are the documentation, because the people this is for do not
 * read any: bind is 127.0.0.1 until someone deliberately writes 0.0.0.0, a
 * request body is capped, a request that never completes is dropped on a
 * deadline, and a handler that sets no response still answers — 200 with an
 * empty body — rather than leaving a browser spinning.
 *
 * There is no https here for the same reason there is none in net_cmds.c, and
 * it matters more on this side: a server that terminated TLS with a stack this
 * library could not keep patched would be worse than one that plainly does
 * not.  Put a reverse proxy in front of it.
 */
#include "net_internal.h"

#define NET_SERVERS_MAX      4
#define NET_CONNS_MAX       16
#define NET_HTTPD_PERIOD_MS  5      /* 0 would spin the loop at 100%          */
#define NET_HTTPD_BACKLOG   64
#define NET_REQ_MAX_HEAD    (64L * 1024L)
#define NET_REQ_MAX_BODY    (1L * 1024L * 1024L)
#define NET_CONN_IDLE_MS    15000

/* One connection, from accept to the last byte of the response.
 *
 * `sock` is first on purpose: the request handle carries KN_HK_SOCKET, so a
 * connection is a valid socket seen through the same pointer.  See the
 * NetSock comment in net_internal.h. */
typedef struct {
    NetSock  sock;
    NetBuf   in;            /* everything read from the client so far        */
    NetBuf   out;           /* the response, being written across turns      */
    size_t   sent;
    size_t   head_len;      /* bytes through the blank line; 0 until seen    */
    size_t   body_len;      /* Content-Length, once the head is parsed       */
    int32_t  handle;        /* live request handle; 0 once retired           */
    int      dispatched;
    int      replied;
    int64_t  deadline;      /* a client that stops talking is not a leak     */
    char    *parsed;        /* one NUL-punched copy of the request head      */
    char    *method, *path, *query, *headers;
} NetConn;

typedef struct {
    int32_t           in_use;
    int32_t           port;
    char              bind[64];
    int               listen_fd;
    int32_t           source;      /* loop source id                          */
    Kiln_HandlerFn on_request;
    NetConn          *conns[NET_CONNS_MAX];
} NetServer;

static NetServer g_servers[NET_SERVERS_MAX];
static int32_t   g_next_handle;
/* The request being dispatched right now, for net_request().  Saved and
 * restored rather than zeroed, because a handler is free to turn the loop. */
static int32_t   g_current_req;

/* --- request handles --------------------------------------------------- */

/* The handle dies; the connection does not.  The server owns the slot and is
 * mid-response when the program closes its handle, so this may only forget the
 * number — freeing here would pull the connection out from under the pump. */
static void net_req_release(void *payload) {
    NetConn *c = (NetConn *)payload;
    if (c) c->handle = 0;
}

/* A request handle and a client-socket handle share one kind, so resolving is
 * not enough on its own: `net_tcp_connect`'s payload is a bare NetSock, and
 * reading it as a NetConn would run off the end of it.  A live connection is
 * one of the pointers a server is holding, and there are at most 64 of them —
 * so ask, rather than trust the kind. */
static NetConn *net_req_live(void *payload) {
    if (!payload) return NULL;
    for (int i = 0; i < NET_SERVERS_MAX; i++) {
        if (!g_servers[i].in_use) continue;
        for (int j = 0; j < NET_CONNS_MAX; j++) {
            if (g_servers[i].conns[j] == payload) return (NetConn *)payload;
        }
    }
    return NULL;
}

static NetConn *net_req_arg(Kiln_Slot *argv, int i) {
    void *p = kn_handle_resolve(kn_arg_int(argv, i), KN_HK_SOCKET);
    if (!p) return NULL;                    /* the handle table set the slot */
    NetConn *c = net_req_live(p);
    if (!c) kn_error_set(KN_ERR_WRONG_KIND, "not an http request handle");
    return c;
}

/* --- request parsing --------------------------------------------------- */

/* Look a header up in the raw block, case-insensitively; NULL when absent. */
static const char *net_hdr_find(const char *block, const char *name, size_t *len) {
    if (!block || !name || !*name) return NULL;
    size_t nlen = strlen(name);
    for (const char *line = block; line && *line;) {
        const char *eol = strpbrk(line, "\r\n");
        if (!eol || eol == line) break;                 /* blank line = end  */
        if (strncasecmp(line, name, nlen) == 0) {
            const char *v = line + nlen;
            while (*v == ' ') v++;
            if (*v == ':') {
                v++;
                while (*v == ' ' || *v == '\t') v++;
                *len = (size_t)(eol - v);
                return v;
            }
        }
        line = eol + (eol[0] == '\r' && eol[1] == '\n' ? 2 : 1);
    }
    return NULL;
}

static int net_hexval(char c) {
    if (c >= '0' && c <= '9') return c - '0';
    if (c >= 'a' && c <= 'f') return c - 'a' + 10;
    if (c >= 'A' && c <= 'F') return c - 'A' + 10;
    return -1;
}

/* Percent-decode a form value in place-ish, into `out` (which must hold `n`+1).
 * `+` is a space here and nowhere else: that is a form-encoding rule, not a
 * URL rule, and a path must never have it applied. */
static size_t net_form_decode(const char *p, size_t n, char *out) {
    size_t w = 0;
    for (size_t i = 0; i < n; i++) {
        if (p[i] == '+') { out[w++] = ' '; continue; }
        if (p[i] == '%' && i + 2 < n) {
            int hi = net_hexval(p[i + 1]), lo = net_hexval(p[i + 2]);
            if (hi >= 0 && lo >= 0) { out[w++] = (char)(hi * 16 + lo); i += 2; continue; }
        }
        out[w++] = p[i];
    }
    out[w] = '\0';
    return w;
}

/* Split the head into method, path, query and the header block.  Everything
 * points into one copy, so freeing the connection frees all of it. */
static int net_req_parse(NetConn *c) {
    c->parsed = (char *)malloc(c->head_len + 1);
    if (!c->parsed) return 0;
    memcpy(c->parsed, c->in.p, c->head_len);
    c->parsed[c->head_len] = '\0';

    char *line = c->parsed;
    char *eol = strpbrk(line, "\r\n");
    if (!eol) return 0;
    c->headers = eol + (eol[0] == '\r' && eol[1] == '\n' ? 2 : 1);
    *eol = '\0';

    c->method = line;
    char *sp = strchr(line, ' ');
    if (!sp) return 0;
    *sp = '\0';
    c->path = sp + 1;
    sp = strchr(c->path, ' ');
    if (sp) *sp = '\0';                 /* drop the HTTP/1.x version         */

    c->query = strchr(c->path, '?');
    if (c->query) { *c->query = '\0'; c->query++; }
    return 1;
}

/* --- responses --------------------------------------------------------- */

static const char *net_reason(int status) {
    switch (status) {
        case 200: return "OK";
        case 201: return "Created";
        case 204: return "No Content";
        case 301: return "Moved Permanently";
        case 302: return "Found";
        case 400: return "Bad Request";
        case 401: return "Unauthorized";
        case 403: return "Forbidden";
        case 404: return "Not Found";
        case 405: return "Method Not Allowed";
        case 413: return "Content Too Large";
        case 431: return "Request Header Fields Too Large";
        case 500: return "Internal Server Error";
        case 501: return "Not Implemented";
        case 503: return "Service Unavailable";
        default:  return "Status";
    }
}

/* Build the whole response into the connection's out-buffer.  Nothing is sent
 * from here: the pump drains it, so a slow client cannot stall the loop.
 *
 * Every response says `Connection: close`, and means it.  Keep-alive would
 * need a per-connection request queue, and a server that promises to reuse a
 * connection and then does not is worse than one that never promised. */
static int net_response(NetConn *c, int status, const char *type,
                        const char *body, size_t blen) {
    char head[256];
    int n = snprintf(head, sizeof head,
                     "HTTP/1.1 %d %s\r\n"
                     "Content-Type: %s\r\n"
                     "Content-Length: %lu\r\n"
                     "Connection: close\r\n"
                     "\r\n",
                     status, net_reason(status), type, (unsigned long)blen);
    if (n < 0 || (size_t)n >= sizeof head) return 0;
    c->out.n = 0;
    c->sent = 0;
    if (!net_buf_add(&c->out, head, (size_t)n)) return 0;
    if (blen && !net_buf_add(&c->out, body, blen)) return 0;
    c->replied = 1;
    return 1;
}

/* --- connections ------------------------------------------------------- */

static void net_conn_free(NetServer *s, int slot) {
    NetConn *c = s->conns[slot];
    if (!c) return;
    s->conns[slot] = NULL;              /* clear first: net_req_live must not
                                         * hand out a connection being freed */
    if (c->handle) kn_handle_close(c->handle, KN_HK_SOCKET);
    if (c->sock.fd >= 0) close(c->sock.fd);
    net_buf_free(&c->in);
    net_buf_free(&c->out);
    free(c->parsed);
    free(c);
}

/* Answer without ever reaching the handler.  Used for the failures that are
 * the server's to detect — a head or a body past the cap — where handing the
 * program a half-read request would be the wrong shape of honesty. */
static void net_conn_refuse(NetConn *c, int status, const char *msg) {
    if (!net_response(c, status, "text/plain; charset=utf-8", msg, strlen(msg)))
        c->sock.eof = 1;
    c->dispatched = 1;
}

static void net_conn_dispatch(NetServer *s, NetConn *c) {
    c->dispatched = 1;
    if (!net_req_parse(c)) {
        net_conn_refuse(c, 400, "malformed request\n");
        return;
    }
    if (!s->on_request) {
        net_response(c, 200, "text/plain; charset=utf-8", "", 0);
        return;
    }

    c->handle = kn_handle_new(KN_HK_SOCKET, c, net_req_release);
    int32_t saved = g_current_req;
    g_current_req = c->handle;
    s->on_request();
    g_current_req = saved;

    /* A handler that answered nothing still gets an answer.  The alternative
     * is a connection that hangs until the client's own timeout, which reads
     * as a broken network rather than as an unfinished handler. */
    if (!c->replied) net_response(c, 200, "text/plain; charset=utf-8", "", 0);

    /* Retired here, not at teardown: the handle's whole job is over, and
     * retiring it now is what makes a saved handle fail loudly on the next
     * request instead of addressing whatever took the slot. */
    if (c->handle) {
        int32_t h = c->handle;
        c->handle = 0;
        kn_handle_close(h, KN_HK_SOCKET);
    }
}

/* Read whatever has arrived.  0 = the connection is finished with. */
static int net_conn_read(NetConn *c) {
    for (;;) {
        /* Stop reading once nothing more could be legal.  The caps are only
         * a cap on memory if they bound what is BUFFERED — refusing a large
         * body after reading it would be a policy, not a limit. */
        if ((long)c->in.n > NET_REQ_MAX_HEAD + NET_REQ_MAX_BODY) return 1;
        char buf[4096];
        ssize_t r = recv(c->sock.fd, buf, (int)sizeof buf, 0);
        int e = net_errno();
        if (r > 0) {
            if (!net_buf_add(&c->in, buf, (size_t)r)) return 0;
            c->deadline = net_now_ms() + NET_CONN_IDLE_MS;
            continue;
        }
        if (r == 0) { c->sock.eof = 1; return 1; }
        if (e == NET_EINTR) continue;
        if (e == NET_EWOULDBLOCK || e == EAGAIN) return 1;
        return 0;
    }
}

/* Write what the socket will take.  1 = still going, 0 = finished with. */
static int net_conn_write(NetConn *c) {
    while (c->sent < c->out.n) {
        ssize_t w = send(c->sock.fd, c->out.p + c->sent,
                         (int)(c->out.n - c->sent), MSG_NOSIGNAL);
        int e = net_errno();
        if (w > 0) { c->sent += (size_t)w; continue; }
        if (w < 0 && e == NET_EINTR) continue;
        if (w < 0 && (e == NET_EWOULDBLOCK || e == EAGAIN)) return 1;
        return 0;                                   /* the client went away  */
    }
    return 0;                                       /* done: close the conn  */
}

/* One turn of one connection.  1 = keep it, 0 = tear it down. */
static int net_conn_step(NetServer *s, NetConn *c) {
    if (c->replied) return net_conn_write(c);   /* a slow client, resumed */

    if (!net_conn_read(c)) return 0;

    if (!c->head_len) {
        const char *end = c->in.p ? strstr(c->in.p, "\r\n\r\n") : NULL;
        size_t skip = 4;
        if (!end && c->in.p) { end = strstr(c->in.p, "\n\n"); skip = 2; }
        if (end) {
            c->head_len = (size_t)(end - c->in.p) + skip;
        } else if ((long)c->in.n > NET_REQ_MAX_HEAD) {
            net_conn_refuse(c, 431, "request head too large\n");
            return 1;
        } else if (c->sock.eof) {
            return 0;                   /* closed before saying anything     */
        }
    }

    if (c->head_len && !c->dispatched) {
        size_t vlen = 0;
        /* A chunked body has no Content-Length, so taking the length as 0
         * would dispatch a request whose body this server then discarded —
         * the handler would read "" with no error, which is the one failure
         * shape nothing here is allowed to have. */
        if (net_hdr_find(c->in.p, "Transfer-Encoding", &vlen)) {
            net_conn_refuse(c, 501, "chunked requests are not supported\n");
            return 1;
        }
        /* Stops at the blank line, so the body that follows is never
         * scanned for headers. */
        const char *v = net_hdr_find(c->in.p, "Content-Length", &vlen);
        if (v) {
            char num[32];
            size_t n = vlen < sizeof num - 1 ? vlen : sizeof num - 1;
            memcpy(num, v, n);
            num[n] = '\0';
            long cl = strtol(num, NULL, 10);
            c->body_len = cl > 0 ? (size_t)cl : 0;
            if (cl > NET_REQ_MAX_BODY) {
                net_conn_refuse(c, 413, "request body too large\n");
                return 1;
            }
        }
        if (c->in.n >= c->head_len + c->body_len) {
            net_conn_dispatch(s, c);
            /* Write the response now rather than next turn.  A handler is
             * allowed to call `quit`, and the loop stops the moment this pump
             * returns — a reply still sitting in the buffer would be a program
             * that answered and then swallowed its own answer. */
            return net_conn_write(c);
        }
        if (c->sock.eof) return 0;      /* body promised, never delivered    */
    }

    if (net_now_ms() > c->deadline) return 0;
    return 1;
}

/* --- the listener and the pump ----------------------------------------- */

/* Bind late, on the first turn of the loop, not at creation: the compiler
 * emits `create` before the property assignments that follow it in the source,
 * exactly as it does for a timer, so at creation the port is still the
 * default.  By the first pump turn the properties are final. */
static int net_listen_open(NetServer *s) {
    if (!net_start()) return 0;

    char portstr[16];
    snprintf(portstr, sizeof portstr, "%d", s->port);

    struct addrinfo hints, *list = NULL;
    memset(&hints, 0, sizeof hints);
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_STREAM;
    hints.ai_flags = AI_PASSIVE;
    if (getaddrinfo(s->bind, portstr, &hints, &list) != 0 || !list) return 0;

    int fd = -1;
    for (struct addrinfo *ai = list; ai; ai = ai->ai_next) {
        fd = socket(ai->ai_family, ai->ai_socktype, ai->ai_protocol);
        if (fd < 0) continue;
        int on = 1;
        setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, (const char *)&on, sizeof on);
        if (bind(fd, ai->ai_addr, (socklen_t)ai->ai_addrlen) == 0 &&
            listen(fd, NET_HTTPD_BACKLOG) == 0 && net_set_nonblocking(fd)) {
            break;
        }
        close(fd);
        fd = -1;
    }
    freeaddrinfo(list);
    if (fd < 0) return 0;
    s->listen_fd = fd;
    return 1;
}

static void net_accept_all(NetServer *s) {
    for (;;) {
        int fd = (int)accept(s->listen_fd, NULL, NULL);
        int e = net_errno();
        if (fd < 0) {
            if (e == NET_EINTR) continue;
            return;                     /* would-block, or nothing to accept */
        }
        int slot = -1;
        for (int i = 0; i < NET_CONNS_MAX; i++) {
            if (!s->conns[i]) { slot = i; break; }
        }
        /* Refuse rather than queue: an unbounded backlog of half-read
         * requests is how a small server becomes a memory leak with a port. */
        if (slot < 0) { close(fd); continue; }

        NetConn *c = (NetConn *)calloc(1, sizeof *c);
        if (!c) { close(fd); continue; }
        c->sock.fd = fd;
        c->deadline = net_now_ms() + NET_CONN_IDLE_MS;
        if (!net_set_nonblocking(fd)) { close(fd); free(c); continue; }
        s->conns[slot] = c;
    }
}

static int32_t net_httpd_pump(void *state) {
    NetServer *s = (NetServer *)state;

    if (s->listen_fd < 0) {
        if (!net_listen_open(s)) {
            /* A server that cannot bind must not look like one that is
             * running: it would answer nothing, forever, silently. */
            fprintf(stderr, "kiln: httpserver cannot listen on %s:%d\n",
                    s->bind, s->port);
            kn_loop_quit(1);
            return 1;
        }
    }

    net_accept_all(s);
    for (int i = 0; i < NET_CONNS_MAX; i++) {
        NetConn *c = s->conns[i];
        if (!c) continue;
        if (!net_conn_step(s, c)) net_conn_free(s, i);
    }
    /* Live until the program quits: a server with no client right now is not
     * a server that is finished. */
    return 0;
}

/* --- the component's hooks (net_component.c) --------------------------- */

void *net_httpd_create(void) {
    if (g_next_handle >= NET_SERVERS_MAX) {
        kn_error_set(KN_ERR_TABLE_FULL, "too many http servers");
        return NULL;
    }
    NetServer *s = &g_servers[g_next_handle++];
    s->in_use = 1;
    s->port = 8080;
    /* Loopback until someone deliberately writes 0.0.0.0.  A RAD tool whose
     * default put a half-written server on every interface of the machine
     * would be shipping the mistake, not the user. */
    snprintf(s->bind, sizeof s->bind, "127.0.0.1");
    s->listen_fd = -1;
    s->source = kn_loop_add(net_httpd_pump, s, NET_HTTPD_PERIOD_MS);
    return s;
}

int32_t net_httpd_set(void *obj, const char *prop, const char *value) {
    NetServer *s = (NetServer *)obj;
    if (strcmp(prop, "port") == 0) {
        long p = strtol(value, NULL, 10);
        s->port = (p < 1 || p > 65535) ? 8080 : (int32_t)p;
    } else if (strcmp(prop, "bind") == 0) {
        snprintf(s->bind, sizeof s->bind, "%s", *value ? value : "127.0.0.1");
    } else {
        return 1;
    }
    /* Rebind on the next turn, the way a timer rearms: a property changed
     * after the listener opened must not leave it on the old address. */
    if (s->listen_fd >= 0) {
        close(s->listen_fd);
        s->listen_fd = -1;
    }
    return 0;
}

const char *net_httpd_get(void *obj, const char *prop) {
    NetServer *s = (NetServer *)obj;
    if (strcmp(prop, "bind") == 0) return net_text(s->bind, strlen(s->bind));
    if (strcmp(prop, "port") == 0) {
        char n[16];
        snprintf(n, sizeof n, "%d", s->port);
        return net_text(n, strlen(n));
    }
    return NULL;
}

int32_t net_httpd_get_int(void *obj, const char *prop) {
    NetServer *s = (NetServer *)obj;
    if (strcmp(prop, "port") == 0) return s->port;
    return 0;
}

int32_t net_httpd_on(void *obj, const char *event, Kiln_HandlerFn handler) {
    NetServer *s = (NetServer *)obj;
    if (strcmp(event, "request") != 0) return 1;
    s->on_request = handler;
    return 0;
}

/* --- commands ---------------------------------------------------------- */

/* net_request() -> int handle, 0 outside a request handler.
 *
 * The bridge between an event handler — which the compiler calls with no
 * arguments — and a subroutine that takes the request as a parameter:
 *
 *     sub on_request
 *       call handle(net_request())
 *     end
 *     sub handle(req: int)
 *       call net_req_reply(req, 200, "hi")
 *     end
 */
void net_request(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; (void)argv;
    if (!g_current_req) {
        kn_error_set(KN_ERR_INVALID_ARG,
                     "there is no request here: net_request() answers only "
                     "inside an httpserver `request` handler");
        kn_ret_int(ret, 0);
        return;
    }
    kn_error_clear();
    kn_ret_int(ret, g_current_req);
}

void net_req_method(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    NetConn *c = net_req_arg(argv, 0);
    if (!c) { kn_ret_text(ret, net_empty()); return; }
    kn_error_clear();
    kn_ret_text(ret, net_text(net_nz(c->method), strlen(net_nz(c->method))));
}

void net_req_path(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    NetConn *c = net_req_arg(argv, 0);
    if (!c) { kn_ret_text(ret, net_empty()); return; }
    kn_error_clear();
    kn_ret_text(ret, net_text(net_nz(c->path), strlen(net_nz(c->path))));
}

/* The body as text.  A body with an embedded NUL stops there, which is what a
 * text slot can carry; a server that needs bytes wants a file, not a slot. */
void net_req_body(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    NetConn *c = net_req_arg(argv, 0);
    if (!c) { kn_ret_text(ret, net_empty()); return; }
    kn_error_clear();
    size_t have = c->in.n > c->head_len ? c->in.n - c->head_len : 0;
    if (have > c->body_len) have = c->body_len;
    kn_ret_text(ret, net_text(have ? c->in.p + c->head_len : "", have));
}

/* An absent header is a genuine "no", so this is infallible on a good handle:
 * "" with error code 0 means the client did not send it. */
void net_req_header(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    NetConn *c = net_req_arg(argv, 0);
    if (!c) { kn_ret_text(ret, net_empty()); return; }
    kn_error_clear();
    size_t vlen = 0;
    const char *v = net_hdr_find(c->headers, net_nz(kn_arg_text(argv, 1)), &vlen);
    kn_ret_text(ret, v ? net_text(v, vlen) : net_empty());
}

/* A query parameter, percent-decoded; "" when the request did not carry it. */
void net_req_query(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    NetConn *c = net_req_arg(argv, 0);
    if (!c) { kn_ret_text(ret, net_empty()); return; }
    kn_error_clear();
    const char *name = net_nz(kn_arg_text(argv, 1));
    size_t nlen = strlen(name);
    if (!c->query || !nlen) { kn_ret_text(ret, net_empty()); return; }

    for (const char *p = c->query; *p;) {
        const char *amp = strchr(p, '&');
        const char *end = amp ? amp : p + strlen(p);
        const char *eq = memchr(p, '=', (size_t)(end - p));
        size_t klen = (size_t)((eq ? eq : end) - p);
        if (klen == nlen && strncmp(p, name, nlen) == 0) {
            const char *v = eq ? eq + 1 : end;
            size_t vlen = (size_t)(end - v);
            char *tmp = (char *)malloc(vlen + 1);
            if (!tmp) { kn_ret_text(ret, net_empty()); return; }
            size_t w = net_form_decode(v, vlen, tmp);
            char *out = net_text(tmp, w);
            free(tmp);
            kn_ret_text(ret, out);
            return;
        }
        if (!amp) break;
        p = amp + 1;
    }
    kn_ret_text(ret, net_empty());
}

static void net_reply_impl(Kiln_Slot *ret, Kiln_Slot *argv, const char *type) {
    NetConn *c = net_req_arg(argv, 0);
    if (!c) { kn_ret_bool(ret, 0); return; }
    if (c->replied) {
        kn_error_set(KN_ERR_INVALID_ARG, "this request has already been answered");
        kn_ret_bool(ret, 0);
        return;
    }
    int status = kn_arg_int(argv, 1);
    if (status < 100 || status > 599) {
        kn_error_set(KN_ERR_INVALID_ARG, "status must be 100..599");
        kn_ret_bool(ret, 0);
        return;
    }
    /* A content type carrying CR or LF would let a program append headers of
     * its own to the response — response splitting, from a value that looks
     * like nothing more than a mime type. */
    const char *ct = type ? type : net_nz(kn_arg_text(argv, 2));
    if (strpbrk(ct, "\r\n") || !*ct) {
        kn_error_set(KN_ERR_INVALID_ARG, "content type is empty or has a line break");
        kn_ret_bool(ret, 0);
        return;
    }
    const char *body = net_nz(kn_arg_text(argv, type ? 2 : 3));
    if (!net_response(c, status, ct, body, strlen(body))) {
        kn_error_set(KN_ERR_TABLE_FULL, "cannot build the response");
        kn_ret_bool(ret, 0);
        return;
    }
    kn_error_clear();
    kn_ret_bool(ret, 1);
}

/* net_req_reply(req, status, text) -> bool */
void net_req_reply(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    net_reply_impl(ret, argv, "text/plain; charset=utf-8");
}

/* net_req_reply_as(req, status, content_type, text) -> bool.  The same reply
 * with the type spelled out, because a server that can only send plain text
 * cannot serve a page or an API. */
void net_req_reply_as(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    net_reply_impl(ret, argv, NULL);
}
