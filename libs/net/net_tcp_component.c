/* `tcpserver` and `tcpclient` — plain TCP as two non-visual components, the
 * shape Delphi programmers know as TIdTCPServer and TIdTCPClient.
 *
 * Neither blocks and neither starts a thread.  Each registers ONE pump with
 * the runtime's event loop while it is active and does a slice of work per
 * turn — accept, read, split, dispatch, write — so a form with a server on it
 * keeps repainting while it serves, and a console program with a client in it
 * stays alive exactly as long as the connection does.  Deactivating a
 * component drops its pump, so an idle one holds nothing open: a program
 * whose client failed to connect ends, rather than waiting for nobody.
 *
 * A client of the server is a small positive int, counted from 1 in the order
 * of arrival and NEVER reused within a run.  A program that keeps a stale one
 * is told so (KN_ERR_STALE), which is the same promise the handle table makes
 * and for the same reason: a number that quietly came to mean someone else's
 * connection is the worst thing a chat server can hand its author.
 *
 * The property boundary is textual and a component is created BEFORE the
 * properties written under it arrive (backend/src/lib.rs emits `create` first,
 * exactly as it does for a timer), so `active = true` may reach a server whose
 * port is still 0.  Nothing binds or connects in `set`: the pump reconciles
 * the wish with the wire on its first turn, when the properties are final.
 *
 * Teardown is the part that is easy to get wrong.  A handler may disconnect
 * the very client it was called for, deactivate the whole server, or call
 * `quit()`, all while the pump is walking the client list.  So nothing here
 * frees a client on the spot: a drop closes the socket, fires `disconnect`
 * once, and marks the client dead; the memory goes in a sweep after the walk.
 * Every walk re-checks "still active, still alive" after every callback.
 */
#include "net_internal.h"

/* kiln_core.h rather than the ABI header alone, for kn_bin_new: a byte-set is
 * allocated by the runtime, and its constructor is a runtime internal the same
 * way kn_empty_text is. `libs/file/file_cmds.c` includes it for the same
 * reason. */
#include "kiln_core.h"

#define NET_TCP_PERIOD_MS       5           /* 0 would spin the loop at 100% */
#define NET_TCP_BACKLOG         64
#define NET_TCP_NAME_MAX        63
#define NET_TCP_DELIM_MAX       15
#define NET_TCP_HOST_MAX        255
#define NET_TCP_CLIENT_CAP      4096        /* the most `max_clients` may ask  */
#define NET_TCP_IN_MAX          (1L * 1024L * 1024L)
#define NET_TCP_OUT_MAX         (16L * 1024L * 1024L)
#define NET_TCPSERVERS_MAX      8
#define NET_TCPCLIENTS_MAX      8

/* glibc hides these two behind _DEFAULT_SOURCE; the values are the ones
 * every platform uses. */
#ifndef NI_MAXHOST
#define NI_MAXHOST 1025
#endif
#ifndef NI_MAXSERV
#define NI_MAXSERV 32
#endif

/* The signatures the typed events are dispatched through.  The compiler emits
 * the handler side with exactly these (backend/src/lib.rs, `handler_symbol`),
 * so the cast back from Kiln_HandlerFn is the one the callee was written
 * with — see the descriptor in net_libinfo.c. */
typedef void (*NetIdFn)(int32_t);
typedef void (*NetIdTextFn)(int32_t, char *);
typedef void (*NetTextFn)(char *);
typedef void (*NetPlainFn)(void);
/* The byte-set forms of the two `receive` shapes.  A byte-set reaches a
 * handler as an LLVM `ptr` exactly as text does, so `void *` is the honest C
 * type for the parameter — the callee reads it as a Kiln_Bin. */
typedef void (*NetIdBinFn)(int32_t, void *);
typedef void (*NetBinFn)(void *);

/* --- one byte stream, either side ----------------------------------------
 * What a server's client and the client component have in common: a
 * non-blocking socket, what has arrived and not yet been delivered, and what
 * the program has sent and the wire has not yet taken. */
typedef struct {
    int    fd;
    NetBuf in;
    NetBuf out;
    int    eof;         /* the peer closed its side                         */
    int    dead;        /* torn down; to be freed by the owner's sweep      */
} NetPeer;

static void net_peer_init(NetPeer *p) {
    memset(p, 0, sizeof *p);
    p->fd = -1;
}

static void net_peer_close(NetPeer *p) {
    if (p->fd >= 0) close(p->fd);
    p->fd = -1;
    net_buf_free(&p->in);
    net_buf_free(&p->out);
}

/* Read whatever has arrived.  1 = fine (perhaps with `eof` now set), 0 = the
 * stream is finished with, `*err` saying why: a platform code, or 0 for the
 * one failure that is this library's own — no delimiter in a megabyte, which
 * is either a protocol mismatch or a peer trying to exhaust memory, and is
 * not a thing to buffer through. */
static int net_peer_read(NetPeer *p, int *err) {
    for (;;) {
        if ((long)p->in.n > NET_TCP_IN_MAX) { *err = 0; return 0; }
        char buf[4096];
        ssize_t r = recv(p->fd, buf, (int)sizeof buf, 0);
        int e = net_errno();
        if (r > 0) {
            if (!net_buf_add(&p->in, buf, (size_t)r)) { *err = ENOMEM; return 0; }
            continue;
        }
        if (r == 0) { p->eof = 1; return 1; }
        if (e == NET_EINTR) continue;
        if (e == NET_EWOULDBLOCK || e == EAGAIN) return 1;
        *err = e;
        return 0;
    }
}

/* Write what the socket will take.  1 = fine, 0 = the peer is gone. */
static int net_peer_write(NetPeer *p, int *err) {
    size_t sent = 0;
    int ok = 1;
    while (sent < p->out.n) {
        ssize_t w = send(p->fd, p->out.p + sent, (int)(p->out.n - sent), MSG_NOSIGNAL);
        int e = net_errno();
        if (w > 0) { sent += (size_t)w; continue; }
        if (w < 0 && e == NET_EINTR) continue;
        if (w < 0 && (e == NET_EWOULDBLOCK || e == EAGAIN)) break;
        *err = e;
        ok = 0;
        break;
    }
    if (sent) {
        memmove(p->out.p, p->out.p + sent, p->out.n - sent);
        p->out.n -= sent;
        p->out.p[p->out.n] = '\0';
    }
    return ok;
}

/* Queue data, then push as much as the wire takes right now.  The immediate
 * push is what lets a handler answer and `quit()` in the same breath: the
 * loop stops the moment the pump returns, and a reply still in the buffer
 * would be an answer the program swallowed.  A push that fails is left for
 * the pump to notice — it reports the disconnect once, in one place. */
static int net_peer_send(NetPeer *p, const char *data, size_t n) {
    if (p->out.n + n > NET_TCP_OUT_MAX) {
        kn_error_set(KN_ERR_TABLE_FULL, "send buffer is full: the peer is not reading");
        return 0;
    }
    if (!net_buf_add(&p->out, data, n)) {
        kn_error_set(KN_ERR_TABLE_FULL, "out of memory queueing data to send");
        return 0;
    }
    int e = 0;
    net_peer_write(p, &e);
    kn_error_clear();
    return 1;
}

/* Where `delim` next occurs in the first `n` bytes; NULL when it does not.
 * Not strstr: a text protocol may still carry a NUL, and stopping at one would
 * hide every line after it. */
static const char *net_find(const char *p, size_t n, const char *delim, size_t dl) {
    for (size_t i = 0; i + dl <= n; i++) {
        if (memcmp(p + i, delim, dl) == 0) return p + i;
    }
    return NULL;
}

/* Hand the owner every complete unit waiting in `in`: one per delimiter with
 * the delimiter stripped, or — with no delimiter — everything, as one piece.
 * The unit is copied out and the bytes consumed BEFORE the callback runs,
 * because the callback is user code and may tear the stream down; after it,
 * only `dead` is looked at.  Answers 0 when the callback said to stop.
 *
 * The callback is handed the raw `(pointer, length)` rather than a finished
 * text, and builds whichever value its event wants.  That is what lets one
 * delivery path have two exits — `receive` makes a text, `receive_bytes` a
 * byte-set — instead of two paths that have to be kept agreeing about
 * delimiters, partial flushes and teardown. */
typedef int (*NetDeliverFn)(void *ctx, const char *p, size_t n);

static int net_peer_deliver(NetPeer *p, const char *delim, void *ctx, NetDeliverFn deliver) {
    size_t dl = strlen(delim);
    for (;;) {
        if (p->dead) return 0;
        if (p->in.n == 0) return 1;
        size_t n = p->in.n;
        if (dl) {
            const char *hit = net_find(p->in.p, p->in.n, delim, dl);
            if (!hit) return 1;
            n = (size_t)(hit - p->in.p);
        }
        char *copy = (char *)malloc(n + 1);
        if (!copy) {
            kn_error_set(KN_ERR_TABLE_FULL, "out of memory delivering received data");
            return 0;
        }
        memcpy(copy, p->in.p, n);
        copy[n] = '\0';
        size_t used = n + dl;
        memmove(p->in.p, p->in.p + used, p->in.n - used);
        p->in.n -= used;
        p->in.p[p->in.n] = '\0';
        int go = deliver(ctx, copy, n);
        free(copy);
        if (!go) return 0;
    }
}

/* What is left when the peer closes without a final delimiter.  It is
 * delivered rather than dropped: `printf hello | nc` sends no newline, and a
 * last line that vanished because the sender hung up is data lost for no
 * reason the program could see. */
static int net_peer_flush_partial(NetPeer *p, void *ctx, NetDeliverFn deliver) {
    if (p->dead || p->in.n == 0) return !p->dead;
    size_t n = p->in.n;
    char *copy = (char *)malloc(n + 1);
    if (!copy) {
        kn_error_set(KN_ERR_TABLE_FULL, "out of memory delivering received data");
        return 0;
    }
    memcpy(copy, p->in.p, n);
    copy[n] = '\0';
    p->in.n = 0;
    p->in.p[0] = '\0';
    int go = deliver(ctx, copy, n);
    free(copy);
    return go;
}

/* "ip:port" for a socket address, the v6 host in brackets so the colons of
 * the address and the one before the port cannot be confused. */
static void net_address_text(const struct sockaddr *sa, socklen_t sl, char *out, size_t cap) {
    char host[NI_MAXHOST], serv[NI_MAXSERV];
    if (getnameinfo(sa, sl, host, sizeof host, serv, sizeof serv,
                    NI_NUMERICHOST | NI_NUMERICSERV) != 0) {
        snprintf(out, cap, "?");
        return;
    }
    if (strchr(host, ':')) snprintf(out, cap, "[%s]:%s", host, serv);
    else                   snprintf(out, cap, "%s:%s", host, serv);
}

/* Fire `error`, or say it on stderr when nothing is wired to hear it.  The
 * error slot is set by the caller and LEFT set, so a handler that asks
 * last_error_code() reads the failure that woke it. */
static void net_report(Kiln_HandlerFn on_error, const char *type, const char *name) {
    const char *msg = kn_error_message();
    if (on_error) {
        ((NetTextFn)on_error)(net_text(msg, strlen(msg)));
    } else {
        fprintf(stderr, "kiln: %s %s: %s\n", type, *name ? name : "(unnamed)", msg);
    }
}

static int net_copy_bounded(char *dst, size_t cap, const char *src, const char *what) {
    if (strlen(src) >= cap) {
        char msg[96];
        snprintf(msg, sizeof msg, "%s is longer than %d bytes", what, (int)cap - 1);
        kn_error_set(KN_ERR_INVALID_ARG, msg);
        return 0;
    }
    memcpy(dst, src, strlen(src) + 1);
    return 1;
}

/* =========================================================================
 * tcpserver
 * ========================================================================= */

typedef struct {
    NetPeer  peer;
    int32_t  id;
    int      told;                  /* `disconnect` has fired               */
    char     address[72];           /* "ip:port", captured at accept        */
} NetClient;

typedef struct {
    char     name[NET_TCP_NAME_MAX + 1];
    int32_t  port;
    char     address[NET_TCP_HOST_MAX + 1];
    int32_t  active;                /* what the program asked for           */
    int32_t  max_clients;
    char     delimiter[NET_TCP_DELIM_MAX + 1];
    int      listen_fd;             /* -1 until the pump has bound          */
    int32_t  source;                /* loop source id while active, else 0  */
    int32_t  next_id;
    NetClient **clients;
    int32_t  count, cap;
    /* > 0 while a walk over `clients` is on the stack, however it got there —
     * the pump, or `active = false` called from inside a handler the pump
     * called.  No sweep and no source removal happens while it is. */
    int      busy;
    Kiln_HandlerFn on_connect, on_disconnect, on_receive, on_receive_bytes, on_error;
} NetTcpServer;

static NetTcpServer g_tcpservers[NET_TCPSERVERS_MAX];
static int32_t      g_tcpserver_count;

static int32_t net_tcpserver_pump(void *state);

static void net_server_sweep(NetTcpServer *s) {
    int32_t w = 0;
    for (int32_t i = 0; i < s->count; i++) {
        NetClient *c = s->clients[i];
        if (c->peer.dead) {
            net_peer_close(&c->peer);
            free(c);
        } else {
            s->clients[w++] = c;
        }
    }
    s->count = w;
}

/* The bookkeeping that must wait until no walk is on the stack: free the
 * dead, and let go of the loop once the server has been switched off. */
static void net_server_settle(NetTcpServer *s) {
    if (s->busy) return;
    net_server_sweep(s);
    if (!s->active && s->source) {
        kn_loop_remove(s->source);
        s->source = 0;
    }
}

/* Close a client and say so, once.  Never frees: see the file comment. */
static void net_client_drop(NetTcpServer *s, NetClient *c) {
    if (c->peer.dead) return;
    c->peer.dead = 1;
    if (c->peer.fd >= 0) { close(c->peer.fd); c->peer.fd = -1; }
    if (!c->told) {
        c->told = 1;
        if (s->on_disconnect) ((NetIdFn)s->on_disconnect)(c->id);
    }
}

/* `active = false`: every client is told, then the listener goes. */
static void net_server_stop(NetTcpServer *s) {
    s->active = 0;
    if (s->listen_fd >= 0) { close(s->listen_fd); s->listen_fd = -1; }
    s->busy++;
    for (int32_t i = 0; i < s->count; i++) net_client_drop(s, s->clients[i]);
    s->busy--;
    net_server_settle(s);
}

static void net_server_start(NetTcpServer *s) {
    s->active = 1;
    if (s->source) return;              /* switched off and on in one turn */
    s->source = kn_loop_add(net_tcpserver_pump, s, NET_TCP_PERIOD_MS);
    if (!s->source) {
        s->active = 0;                  /* the loop set the error slot     */
        net_report(s->on_error, "tcpserver", s->name);
    }
}

static int net_server_push(NetTcpServer *s, NetClient *c) {
    if (s->count == s->cap) {
        int32_t cap = s->cap ? s->cap * 2 : 16;
        NetClient **grown = (NetClient **)realloc(s->clients, (size_t)cap * sizeof *grown);
        if (!grown) return 0;
        s->clients = grown;
        s->cap = cap;
    }
    s->clients[s->count++] = c;
    return 1;
}

/* Bind and listen.  On failure the error slot says why, with the address in
 * the message, because "address already in use" alone sends someone to the
 * wrong port. */
static int net_tcpserver_listen(NetTcpServer *s) {
    if (!net_start()) {
        kn_error_set(KN_ERR_UNSUPPORTED, "Winsock could not be started");
        return 0;
    }
    if (s->port < 1 || s->port > 65535) {
        kn_error_set(KN_ERR_INVALID_ARG, "port is not set: a tcpserver needs a port in 1..65535");
        return 0;
    }
    char portstr[16], where[NET_TCP_HOST_MAX + 32];
    snprintf(portstr, sizeof portstr, "%d", s->port);
    snprintf(where, sizeof where, "listen on %s:%d", s->address, s->port);

    struct addrinfo hints, *list = NULL;
    memset(&hints, 0, sizeof hints);
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_STREAM;
    hints.ai_flags = AI_PASSIVE;
    int rc = getaddrinfo(s->address, portstr, &hints, &list);
    if (rc != 0 || !list) {
        char msg[NET_TCP_HOST_MAX + 96];
        snprintf(msg, sizeof msg, "resolve %s: %s", s->address, gai_strerror(rc));
        kn_error_set(KN_ERR_INVALID_ARG, msg);
        return 0;
    }

    int fd = -1, e = 0;
    for (struct addrinfo *ai = list; ai; ai = ai->ai_next) {
        fd = (int)socket(ai->ai_family, ai->ai_socktype, ai->ai_protocol);
        if (fd < 0) { e = net_errno(); continue; }
        int on = 1;
        setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, (const char *)&on, sizeof on);
        if (bind(fd, ai->ai_addr, (socklen_t)ai->ai_addrlen) == 0 &&
            listen(fd, NET_TCP_BACKLOG) == 0 && net_set_nonblocking(fd)) {
            break;
        }
        e = net_errno();
        close(fd);
        fd = -1;
    }
    freeaddrinfo(list);
    if (fd < 0) {
        net_fail(e, where);
        return 0;
    }
    s->listen_fd = fd;
    return 1;
}

static void net_tcpserver_accept(NetTcpServer *s) {
    for (;;) {
        struct sockaddr_storage sa;
        socklen_t sl = sizeof sa;
        int fd = (int)accept(s->listen_fd, (struct sockaddr *)&sa, &sl);
        int e = net_errno();
        if (fd < 0) {
            if (e == NET_EINTR) continue;
            return;                     /* would-block, or nothing waiting */
        }
        /* Refuse rather than queue: past `max_clients` the connection is
         * closed at once, so the peer sees a refusal instead of a silence. */
        if (s->count >= s->max_clients || !net_set_nonblocking(fd)) {
            close(fd);
            continue;
        }
        NetClient *c = (NetClient *)calloc(1, sizeof *c);
        if (!c) { close(fd); continue; }
        net_peer_init(&c->peer);
        c->peer.fd = fd;
        c->id = ++s->next_id;
        net_address_text((const struct sockaddr *)&sa, sl, c->address, sizeof c->address);
        if (!net_server_push(s, c)) { close(fd); free(c); continue; }
        if (s->on_connect) ((NetIdFn)s->on_connect)(c->id);
        if (!s->active) return;         /* the handler switched us off      */
    }
}

typedef struct { NetTcpServer *s; NetClient *c; } NetServerDeliver;

/* A delivered unit as a byte-set: every byte of it, NULs included, which is
 * the whole reason `receive_bytes` exists.  NULL when the allocation failed,
 * and the caller treats that as nothing to deliver rather than as a handler
 * it must call with garbage. */
static void *net_bin_from(const char *p, size_t n) {
    void *b = kn_bin_new((int32_t)n);
    if (!b) {
        kn_error_set(KN_ERR_TABLE_FULL, "out of memory building the received byte-set");
        return NULL;
    }
    memcpy((Kiln_Bin *)b + 1, p, n);
    return b;
}

/* One unit, one exit.  `receive_bytes` wins when both are wired: a program
 * that asked for bytes asked because text loses what it needs, and firing
 * both would hand the same unit twice under two shapes. */
static int net_server_deliver(void *ctx, const char *p, size_t n) {
    NetServerDeliver *d = (NetServerDeliver *)ctx;
    if (d->s->on_receive_bytes) {
        void *b = net_bin_from(p, n);
        if (b) ((NetIdBinFn)d->s->on_receive_bytes)(d->c->id, b);
    } else if (d->s->on_receive) {
        ((NetIdTextFn)d->s->on_receive)(d->c->id, net_text(p, n));
    }
    return d->s->active && !d->c->peer.dead;
}

/* One turn of one client. */
static void net_client_step(NetTcpServer *s, NetClient *c) {
    if (c->peer.dead) return;
    NetServerDeliver d = { s, c };
    int e = 0;
    if (!net_peer_read(&c->peer, &e)) {
        /* A reset is the peer leaving, which `disconnect` already says.  A
         * full buffer is the one failure that is ours to explain. */
        if (e == 0) {
            char msg[128];
            snprintf(msg, sizeof msg, "client %d sent %ld bytes with no delimiter; dropped",
                     c->id, (long)NET_TCP_IN_MAX);
            kn_error_set(KN_ERR_TABLE_FULL, msg);
            net_report(s->on_error, "tcpserver", s->name);
        }
        net_client_drop(s, c);
        return;
    }
    if (!net_peer_deliver(&c->peer, s->delimiter, &d, net_server_deliver)) return;
    if (c->peer.eof) {
        if (!net_peer_flush_partial(&c->peer, &d, net_server_deliver)) return;
        net_client_drop(s, c);
        return;
    }
    if (!net_peer_write(&c->peer, &e)) net_client_drop(s, c);
}

static int32_t net_tcpserver_pump(void *state) {
    NetTcpServer *s = (NetTcpServer *)state;
    s->busy++;
    if (s->active && s->listen_fd < 0 && !net_tcpserver_listen(s)) {
        s->active = 0;
        if (s->on_error) {
            net_report(s->on_error, "tcpserver", s->name);
        } else {
            /* Nothing is listening for the failure, so the program cannot
             * be one that meant to carry on: say it and stop, the way an
             * httpserver does, rather than run deaf with a port number. */
            fprintf(stderr, "kiln: tcpserver %s cannot %s\n",
                    *s->name ? s->name : "(unnamed)", kn_error_message());
            kn_loop_quit(1);
        }
    }
    if (s->active) net_tcpserver_accept(s);
    for (int32_t i = 0; i < s->count && s->active; i++) net_client_step(s, s->clients[i]);
    s->busy--;
    net_server_sweep(s);
    if (!s->active) {
        /* Dropped by returning 1, never by kn_loop_remove from inside the
         * pump: the loop removes this slot itself on 1, and a slot removed
         * twice could take a source registered in between with it. */
        s->source = 0;
        return 1;
    }
    return 0;
}

/* --- hooks (net_component.c) --------------------------------------------- */

void *net_tcpserver_create(void) {
    if (g_tcpserver_count >= NET_TCPSERVERS_MAX) {
        kn_error_set(KN_ERR_TABLE_FULL, "too many tcp servers");
        return NULL;
    }
    NetTcpServer *s = &g_tcpservers[g_tcpserver_count++];
    memset(s, 0, sizeof *s);
    snprintf(s->address, sizeof s->address, "0.0.0.0");
    snprintf(s->delimiter, sizeof s->delimiter, "\n");
    s->max_clients = 64;
    s->listen_fd = -1;
    return s;
}

int32_t net_tcpserver_set(void *obj, const char *prop, const char *value) {
    NetTcpServer *s = (NetTcpServer *)obj;
    if (strcmp(prop, "name") == 0) {
        return net_copy_bounded(s->name, sizeof s->name, value, "name") ? 0 : 1;
    }
    if (strcmp(prop, "port") == 0 || strcmp(prop, "address") == 0) {
        if (*prop == 'p') {
            long p = strtol(value, NULL, 10);
            s->port = (p < 0 || p > 65535) ? 0 : (int32_t)p;
        } else if (!net_copy_bounded(s->address, sizeof s->address,
                                     *value ? value : "0.0.0.0", "address")) {
            return 1;
        }
        /* Rebind on the next turn: a listener left on the old address would
         * make the property a lie.  Clients already connected stay. */
        if (s->listen_fd >= 0) { close(s->listen_fd); s->listen_fd = -1; }
        return 0;
    }
    if (strcmp(prop, "max_clients") == 0) {
        long n = strtol(value, NULL, 10);
        s->max_clients = n < 1 ? 1 : n > NET_TCP_CLIENT_CAP ? NET_TCP_CLIENT_CAP : (int32_t)n;
        return 0;
    }
    if (strcmp(prop, "delimiter") == 0) {
        return net_copy_bounded(s->delimiter, sizeof s->delimiter, value, "delimiter") ? 0 : 1;
    }
    if (strcmp(prop, "active") == 0) {
        int want = net_bool_of(value);
        if (want && !s->active) net_server_start(s);
        else if (!want && s->active) net_server_stop(s);
        return 0;
    }
    return 1;
}

const char *net_tcpserver_get(void *obj, const char *prop) {
    NetTcpServer *s = (NetTcpServer *)obj;
    char n[16];
    if (strcmp(prop, "name") == 0)      return net_text(s->name, strlen(s->name));
    if (strcmp(prop, "address") == 0)   return net_text(s->address, strlen(s->address));
    if (strcmp(prop, "delimiter") == 0) return net_text(s->delimiter, strlen(s->delimiter));
    if (strcmp(prop, "active") == 0)    return s->active ? "true" : "false";
    if (strcmp(prop, "port") == 0)        { snprintf(n, sizeof n, "%d", s->port); return net_text(n, strlen(n)); }
    if (strcmp(prop, "max_clients") == 0) { snprintf(n, sizeof n, "%d", s->max_clients); return net_text(n, strlen(n)); }
    return NULL;
}

int32_t net_tcpserver_get_int(void *obj, const char *prop) {
    NetTcpServer *s = (NetTcpServer *)obj;
    if (strcmp(prop, "port") == 0)        return s->port;
    if (strcmp(prop, "max_clients") == 0) return s->max_clients;
    if (strcmp(prop, "active") == 0)      return s->active;
    return 0;
}

int32_t net_tcpserver_on(void *obj, const char *event, Kiln_HandlerFn fn) {
    NetTcpServer *s = (NetTcpServer *)obj;
    if (strcmp(event, "connect") == 0)    { s->on_connect = fn;    return 0; }
    if (strcmp(event, "disconnect") == 0) { s->on_disconnect = fn; return 0; }
    if (strcmp(event, "receive") == 0)    { s->on_receive = fn;    return 0; }
    if (strcmp(event, "receive_bytes") == 0) { s->on_receive_bytes = fn; return 0; }
    if (strcmp(event, "error") == 0)      { s->on_error = fn;      return 0; }
    return 1;
}

/* --- commands ------------------------------------------------------------ */

/* A command names its server by the `name` property, exactly as `grid_cell`
 * names a grid: nothing a program can write names a component otherwise, and
 * the compiler passes no id to `create`.  The message says what to do about
 * it, because "not found" alone sends someone to check the spelling of a
 * name that was never set. */
static NetTcpServer *net_tcpserver_named(const char *name) {
    name = net_nz(name);
    for (int32_t i = 0; *name && i < g_tcpserver_count; i++) {
        if (strcmp(g_tcpservers[i].name, name) == 0) return &g_tcpservers[i];
    }
    char msg[NET_TCP_NAME_MAX * 2 + 96];
    snprintf(msg, sizeof msg, "no tcpserver named \"%s\": set name = \"%s\" in the component",
             name, name);
    kn_error_set(KN_ERR_INVALID_ARG, msg);
    return NULL;
}

/* A live client by id.  A positive id that is not live was one once — the
 * peer has gone — and the distinction from a nonsense id is the one a chat
 * server acts on (drop the room entry, not fix the code). */
static NetClient *net_client_by_id(NetTcpServer *s, int32_t id) {
    if (id < 1) {
        kn_error_set(KN_ERR_BAD_HANDLE, "client ids count from 1");
        return NULL;
    }
    for (int32_t i = 0; i < s->count; i++) {
        NetClient *c = s->clients[i];
        if (c->id == id && !c->peer.dead) return c;
    }
    kn_error_set(id > s->next_id ? KN_ERR_BAD_HANDLE : KN_ERR_STALE,
                 id > s->next_id ? "no such client" : "that client has disconnected");
    return NULL;
}

/* tcpserver_send(server, client, data) -> bool */
void tcpserver_send(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    NetTcpServer *s = net_tcpserver_named(kn_arg_text(argv, 0));
    NetClient *c = s ? net_client_by_id(s, kn_arg_int(argv, 1)) : NULL;
    if (!c) { kn_ret_bool(ret, 0); return; }
    const char *data = net_nz(kn_arg_text(argv, 2));
    kn_ret_bool(ret, net_peer_send(&c->peer, data, strlen(data)));
}

/* tcpserver_send_bytes(server, client, data) -> bool
 *
 * The byte-set half of `tcpserver_send`, and the reason it exists: the text
 * form measures its argument with strlen, so a reply carrying a NUL reached
 * the peer cut off at it.  A byte-set carries its own length. */
void tcpserver_send_bytes(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    NetTcpServer *s = net_tcpserver_named(kn_arg_text(argv, 0));
    NetClient *c = s ? net_client_by_id(s, kn_arg_int(argv, 1)) : NULL;
    if (!c) { kn_ret_bool(ret, 0); return; }
    Kiln_Bin *b = (Kiln_Bin *)kn_arg_ptr(argv, 2);
    if (!b) { kn_ret_bool(ret, 0); kn_error_set(KN_ERR_INVALID_ARG, "no bytes to send"); return; }
    kn_ret_bool(ret, net_peer_send(&c->peer, (const char *)(b + 1), (size_t)b->len));
}

/* tcpserver_send_all(server, data) -> int: how many clients it was queued
 * for; -1 when there is no such server. */
void tcpserver_send_all(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    NetTcpServer *s = net_tcpserver_named(kn_arg_text(argv, 0));
    if (!s) { kn_ret_int(ret, -1); return; }
    const char *data = net_nz(kn_arg_text(argv, 1));
    int32_t sent = 0;
    for (int32_t i = 0; i < s->count; i++) {
        NetClient *c = s->clients[i];
        if (!c->peer.dead && net_peer_send(&c->peer, data, strlen(data))) sent++;
    }
    kn_error_clear();
    kn_ret_int(ret, sent);
}

/* tcpserver_disconnect(server, client) -> bool */
void tcpserver_disconnect(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    NetTcpServer *s = net_tcpserver_named(kn_arg_text(argv, 0));
    NetClient *c = s ? net_client_by_id(s, kn_arg_int(argv, 1)) : NULL;
    if (!c) { kn_ret_bool(ret, 0); return; }
    net_client_drop(s, c);
    net_server_settle(s);
    kn_error_clear();
    kn_ret_bool(ret, 1);
}

/* tcpserver_client_count(server) -> int, -1 when there is no such server. */
void tcpserver_client_count(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    NetTcpServer *s = net_tcpserver_named(kn_arg_text(argv, 0));
    if (!s) { kn_ret_int(ret, -1); return; }
    int32_t live = 0;
    for (int32_t i = 0; i < s->count; i++) live += !s->clients[i]->peer.dead;
    kn_error_clear();
    kn_ret_int(ret, live);
}

/* tcpserver_client_address(server, client) -> text, "ip:port". */
void tcpserver_client_address(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    NetTcpServer *s = net_tcpserver_named(kn_arg_text(argv, 0));
    NetClient *c = s ? net_client_by_id(s, kn_arg_int(argv, 1)) : NULL;
    if (!c) { kn_ret_text(ret, net_empty()); return; }
    kn_error_clear();
    kn_ret_text(ret, net_text(c->address, strlen(c->address)));
}

/* tcpserver_client(server, n) -> int: the n-th live client's id, counting
 * from 1; 0 past the end, which is a genuine "none" and not a failure. */
void tcpserver_client(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    NetTcpServer *s = net_tcpserver_named(kn_arg_text(argv, 0));
    if (!s) { kn_ret_int(ret, 0); return; }
    int32_t n = kn_arg_int(argv, 1);
    if (n < 1) {
        kn_error_set(KN_ERR_INVALID_ARG, "positions count from 1");
        kn_ret_int(ret, 0);
        return;
    }
    kn_error_clear();
    for (int32_t i = 0; i < s->count; i++) {
        NetClient *c = s->clients[i];
        if (c->peer.dead) continue;
        if (--n == 0) { kn_ret_int(ret, c->id); return; }
    }
    kn_ret_int(ret, 0);
}

/* =========================================================================
 * tcpclient
 * ========================================================================= */

enum { NET_CL_IDLE, NET_CL_CONNECTING, NET_CL_CONNECTED };

typedef struct {
    char     name[NET_TCP_NAME_MAX + 1];
    char     host[NET_TCP_HOST_MAX + 1];
    int32_t  port;
    int32_t  active;
    char     delimiter[NET_TCP_DELIM_MAX + 1];
    int32_t  timeout_ms;
    int      state;
    NetPeer  peer;
    /* Every address the host resolved to, tried in order across turns.
     * `localhost` commonly resolves to ::1 first, and a server bound to
     * 127.0.0.1 refuses there; stopping at the first refusal would make the
     * most ordinary local test fail. */
    struct addrinfo *list, *cur;
    int64_t  deadline;
    int32_t  source;
    int      busy;
    Kiln_HandlerFn on_connect, on_disconnect, on_receive, on_receive_bytes, on_error;
} NetTcpClient;

static NetTcpClient g_tcpclients[NET_TCPCLIENTS_MAX];
static int32_t      g_tcpclient_count;

static int32_t net_tcpclient_pump(void *state);

static void net_tcpclient_forget_addresses(NetTcpClient *c) {
    if (c->list) freeaddrinfo(c->list);
    c->list = c->cur = NULL;
}

/* Back to idle, telling the program if it had a connection to lose.  The
 * source is dropped here only outside the pump; the pump drops itself. */
static void net_tcpclient_stop(NetTcpClient *c) {
    int had = c->state == NET_CL_CONNECTED;
    c->active = 0;
    c->state = NET_CL_IDLE;
    c->peer.dead = 1;
    net_peer_close(&c->peer);
    net_tcpclient_forget_addresses(c);
    if (had && c->on_disconnect) ((NetPlainFn)c->on_disconnect)();
    if (!c->busy && c->source) {
        kn_loop_remove(c->source);
        c->source = 0;
    }
}

static void net_tcpclient_start(NetTcpClient *c) {
    c->active = 1;
    if (c->source) return;
    c->source = kn_loop_add(net_tcpclient_pump, c, NET_TCP_PERIOD_MS);
    if (!c->source) {
        c->active = 0;
        net_report(c->on_error, "tcpclient", c->name);
    }
}

/* The attempt is over and did not connect: the slot names the reason and
 * the component switches itself off, so a console program with nothing else
 * to wait for ends rather than idling. */
static void net_tcpclient_fail(NetTcpClient *c, int code, const char *fallback_msg) {
    char what[NET_TCP_HOST_MAX + 32];
    snprintf(what, sizeof what, "connect %s:%d", c->host, c->port);
    if (code) net_fail(code, what);
    else {
        char msg[NET_TCP_HOST_MAX + 160];
        snprintf(msg, sizeof msg, "%s: %s", what, fallback_msg);
        kn_error_set(KN_ERR_INVALID_ARG, msg);
    }
    c->state = NET_CL_IDLE;
    c->active = 0;
    c->peer.dead = 1;
    net_peer_close(&c->peer);
    net_tcpclient_forget_addresses(c);
    net_report(c->on_error, "tcpclient", c->name);
}

static void net_tcpclient_established(NetTcpClient *c) {
    c->state = NET_CL_CONNECTED;
    net_tcpclient_forget_addresses(c);
    if (c->on_connect) ((NetPlainFn)c->on_connect)();
}

/* Start a non-blocking connect to the next address, moving on past any that
 * fail outright.  A connect that is merely in progress is left for the pump
 * to poll. */
static void net_tcpclient_try(NetTcpClient *c) {
    int e = 0;
    while (c->cur) {
        struct addrinfo *ai = c->cur;
        c->cur = ai->ai_next;
        int fd = (int)socket(ai->ai_family, ai->ai_socktype, ai->ai_protocol);
        if (fd < 0) { e = net_errno(); continue; }
        if (!net_set_nonblocking(fd)) { e = net_errno(); close(fd); continue; }
        int r = connect(fd, ai->ai_addr, (socklen_t)ai->ai_addrlen);
        e = net_errno();
        if (r == 0) {
            net_peer_init(&c->peer);
            c->peer.fd = fd;
            net_tcpclient_established(c);
            return;
        }
        if (e == NET_EINPROGRESS || e == NET_EWOULDBLOCK) {
            net_peer_init(&c->peer);
            c->peer.fd = fd;
            c->state = NET_CL_CONNECTING;
            return;
        }
        close(fd);
    }
    net_tcpclient_fail(c, e, "no address could be reached");
}

static void net_tcpclient_begin(NetTcpClient *c) {
    if (!net_start()) {
        net_tcpclient_fail(c, 0, "Winsock could not be started");
        return;
    }
    if (!*c->host) { net_tcpclient_fail(c, 0, "host is not set"); return; }
    if (c->port < 1 || c->port > 65535) {
        net_tcpclient_fail(c, 0, "port is not set: a tcpclient needs a port in 1..65535");
        return;
    }
    char portstr[16];
    snprintf(portstr, sizeof portstr, "%d", c->port);
    struct addrinfo hints;
    memset(&hints, 0, sizeof hints);
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_STREAM;
    int rc = getaddrinfo(c->host, portstr, &hints, &c->list);
    if (rc != 0 || !c->list) {
        char msg[NET_TCP_HOST_MAX + 96];
        snprintf(msg, sizeof msg, "resolve %s: %s", c->host, gai_strerror(rc));
        kn_error_set(KN_ERR_INVALID_ARG, msg);
        c->list = NULL;
        c->state = NET_CL_IDLE;
        c->active = 0;
        net_report(c->on_error, "tcpclient", c->name);
        return;
    }
    c->cur = c->list;
    c->deadline = net_now_ms() + c->timeout_ms;
    net_tcpclient_try(c);
}

/* Has the connect in progress finished?  Writable means done, and SO_ERROR
 * says whether well.  On Windows this is WSAPoll, which before Windows 10
 * 2004 did not report a refused connect at all; there a refusal surfaces as
 * the timeout — later, never wrong. */
static void net_tcpclient_poll(NetTcpClient *c) {
    struct pollfd pfd;
    pfd.fd = c->peer.fd;
    pfd.events = POLLOUT;
    pfd.revents = 0;
    int pr = poll(&pfd, 1, 0);
    if (pr > 0) {
        int so_err = 0;
        socklen_t len = sizeof so_err;
        if (getsockopt(c->peer.fd, SOL_SOCKET, SO_ERROR, (char *)&so_err, &len) < 0)
            so_err = net_errno();
        if (so_err == 0 && !(pfd.revents & (POLLERR | POLLHUP))) {
            net_tcpclient_established(c);
            return;
        }
        if (so_err == 0) so_err = NET_ECONNREFUSED;
        close(c->peer.fd);
        c->peer.fd = -1;
        if (c->cur) { net_tcpclient_try(c); return; }
        net_tcpclient_fail(c, so_err, NULL);
        return;
    }
    if (net_now_ms() > c->deadline) net_tcpclient_fail(c, NET_ETIMEDOUT, NULL);
}

static int net_tcpclient_deliver(void *ctx, const char *p, size_t n) {
    NetTcpClient *c = (NetTcpClient *)ctx;
    if (c->on_receive_bytes) {
        void *b = net_bin_from(p, n);
        if (b) ((NetBinFn)c->on_receive_bytes)(b);
    } else if (c->on_receive) {
        ((NetTextFn)c->on_receive)(net_text(p, n));
    }
    return c->state == NET_CL_CONNECTED && !c->peer.dead;
}

static void net_tcpclient_step(NetTcpClient *c) {
    int e = 0;
    if (!net_peer_read(&c->peer, &e)) {
        if (e == 0) {
            char msg[96];
            snprintf(msg, sizeof msg, "%ld bytes arrived with no delimiter; disconnected",
                     (long)NET_TCP_IN_MAX);
            kn_error_set(KN_ERR_TABLE_FULL, msg);
            net_report(c->on_error, "tcpclient", c->name);
        }
        net_tcpclient_stop(c);
        return;
    }
    if (!net_peer_deliver(&c->peer, c->delimiter, c, net_tcpclient_deliver)) return;
    if (c->peer.eof) {
        if (!net_peer_flush_partial(&c->peer, c, net_tcpclient_deliver)) return;
        net_tcpclient_stop(c);
        return;
    }
    if (!net_peer_write(&c->peer, &e)) net_tcpclient_stop(c);
}

static int32_t net_tcpclient_pump(void *state) {
    NetTcpClient *c = (NetTcpClient *)state;
    c->busy++;
    if (c->active && c->state == NET_CL_IDLE)       net_tcpclient_begin(c);
    if (c->active && c->state == NET_CL_CONNECTING) net_tcpclient_poll(c);
    if (c->active && c->state == NET_CL_CONNECTED)  net_tcpclient_step(c);
    c->busy--;
    if (!c->active) {
        c->source = 0;                  /* dropped by returning 1; see the server */
        return 1;
    }
    return 0;
}

/* --- hooks (net_component.c) --------------------------------------------- */

void *net_tcpclient_create(void) {
    if (g_tcpclient_count >= NET_TCPCLIENTS_MAX) {
        kn_error_set(KN_ERR_TABLE_FULL, "too many tcp clients");
        return NULL;
    }
    NetTcpClient *c = &g_tcpclients[g_tcpclient_count++];
    memset(c, 0, sizeof *c);
    snprintf(c->delimiter, sizeof c->delimiter, "\n");
    c->timeout_ms = 5000;
    net_peer_init(&c->peer);
    return c;
}

int32_t net_tcpclient_set(void *obj, const char *prop, const char *value) {
    NetTcpClient *c = (NetTcpClient *)obj;
    if (strcmp(prop, "name") == 0)
        return net_copy_bounded(c->name, sizeof c->name, value, "name") ? 0 : 1;
    if (strcmp(prop, "host") == 0)
        return net_copy_bounded(c->host, sizeof c->host, value, "host") ? 0 : 1;
    if (strcmp(prop, "delimiter") == 0)
        return net_copy_bounded(c->delimiter, sizeof c->delimiter, value, "delimiter") ? 0 : 1;
    if (strcmp(prop, "port") == 0) {
        long p = strtol(value, NULL, 10);
        c->port = (p < 0 || p > 65535) ? 0 : (int32_t)p;
        return 0;
    }
    if (strcmp(prop, "timeout_ms") == 0) {
        long t = strtol(value, NULL, 10);
        c->timeout_ms = t < 1 ? 1 : (int32_t)t;
        return 0;
    }
    if (strcmp(prop, "active") == 0) {
        int want = net_bool_of(value);
        if (want && !c->active) net_tcpclient_start(c);
        else if (!want && c->active) net_tcpclient_stop(c);
        return 0;
    }
    if (strcmp(prop, "connected") == 0) {
        /* Read-only, but Studio writes every descriptor default into the
         * source when a component is dropped, so `connected = false` reaches
         * every client that was ever on a toolbox.  Restating what is already
         * true is silent; only a CHANGE is refused — and since the backend
         * ignores a setter's answer, the slot is the only place to say so. */
        if (net_bool_of(value) == (c->state == NET_CL_CONNECTED)) return 0;
        kn_error_set(KN_ERR_INVALID_ARG, "connected is read-only: set active instead");
        return 1;
    }
    return 1;
}

const char *net_tcpclient_get(void *obj, const char *prop) {
    NetTcpClient *c = (NetTcpClient *)obj;
    char n[16];
    if (strcmp(prop, "name") == 0)      return net_text(c->name, strlen(c->name));
    if (strcmp(prop, "host") == 0)      return net_text(c->host, strlen(c->host));
    if (strcmp(prop, "delimiter") == 0) return net_text(c->delimiter, strlen(c->delimiter));
    if (strcmp(prop, "active") == 0)    return c->active ? "true" : "false";
    if (strcmp(prop, "connected") == 0) return c->state == NET_CL_CONNECTED ? "true" : "false";
    if (strcmp(prop, "port") == 0)       { snprintf(n, sizeof n, "%d", c->port); return net_text(n, strlen(n)); }
    if (strcmp(prop, "timeout_ms") == 0) { snprintf(n, sizeof n, "%d", c->timeout_ms); return net_text(n, strlen(n)); }
    return NULL;
}

int32_t net_tcpclient_get_int(void *obj, const char *prop) {
    NetTcpClient *c = (NetTcpClient *)obj;
    if (strcmp(prop, "port") == 0)       return c->port;
    if (strcmp(prop, "timeout_ms") == 0) return c->timeout_ms;
    if (strcmp(prop, "active") == 0)     return c->active;
    if (strcmp(prop, "connected") == 0)  return c->state == NET_CL_CONNECTED;
    return 0;
}

int32_t net_tcpclient_on(void *obj, const char *event, Kiln_HandlerFn fn) {
    NetTcpClient *c = (NetTcpClient *)obj;
    if (strcmp(event, "connect") == 0)    { c->on_connect = fn;    return 0; }
    if (strcmp(event, "disconnect") == 0) { c->on_disconnect = fn; return 0; }
    if (strcmp(event, "receive") == 0)    { c->on_receive = fn;    return 0; }
    if (strcmp(event, "receive_bytes") == 0) { c->on_receive_bytes = fn; return 0; }
    if (strcmp(event, "error") == 0)      { c->on_error = fn;      return 0; }
    return 1;
}

/* --- commands ------------------------------------------------------------ */

static NetTcpClient *net_tcpclient_named(const char *name) {
    name = net_nz(name);
    for (int32_t i = 0; *name && i < g_tcpclient_count; i++) {
        if (strcmp(g_tcpclients[i].name, name) == 0) return &g_tcpclients[i];
    }
    char msg[NET_TCP_NAME_MAX * 2 + 96];
    snprintf(msg, sizeof msg, "no tcpclient named \"%s\": set name = \"%s\" in the component",
             name, name);
    kn_error_set(KN_ERR_INVALID_ARG, msg);
    return NULL;
}

/* tcpclient_send(client, data) -> bool */
void tcpclient_send(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    NetTcpClient *c = net_tcpclient_named(kn_arg_text(argv, 0));
    if (!c) { kn_ret_bool(ret, 0); return; }
    if (c->state != NET_CL_CONNECTED) {
        kn_error_set(KN_ERR_INVALID_ARG, "not connected");
        kn_ret_bool(ret, 0);
        return;
    }
    const char *data = net_nz(kn_arg_text(argv, 1));
    kn_ret_bool(ret, net_peer_send(&c->peer, data, strlen(data)));
}

/* tcpclient_send_bytes(client, data) -> bool.  The byte-set half of
 * `tcpclient_send`, measured by the set's own length rather than by strlen. */
void tcpclient_send_bytes(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    NetTcpClient *c = net_tcpclient_named(kn_arg_text(argv, 0));
    if (!c || c->state != NET_CL_CONNECTED) {
        if (c) kn_error_set(KN_ERR_INVALID_ARG, "the client is not connected");
        kn_ret_bool(ret, 0);
        return;
    }
    Kiln_Bin *b = (Kiln_Bin *)kn_arg_ptr(argv, 1);
    if (!b) { kn_ret_bool(ret, 0); kn_error_set(KN_ERR_INVALID_ARG, "no bytes to send"); return; }
    kn_ret_bool(ret, net_peer_send(&c->peer, (const char *)(b + 1), (size_t)b->len));
}

/* tcpclient_connect(client) -> bool: `active = true` as a call.  True means
 * the attempt is under way (or the client is already connected); the outcome
 * arrives as `connect` or `error`. */
void tcpclient_connect(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    NetTcpClient *c = net_tcpclient_named(kn_arg_text(argv, 0));
    if (!c) { kn_ret_bool(ret, 0); return; }
    if (!c->active) net_tcpclient_start(c);
    if (!c->active) { kn_ret_bool(ret, 0); return; }    /* the slot says why */
    kn_error_clear();
    kn_ret_bool(ret, 1);
}

/* tcpclient_disconnect(client) -> bool: false with code 0 when there was
 * nothing to disconnect. */
void tcpclient_disconnect(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    NetTcpClient *c = net_tcpclient_named(kn_arg_text(argv, 0));
    if (!c) { kn_ret_bool(ret, 0); return; }
    kn_error_clear();
    if (!c->active) { kn_ret_bool(ret, 0); return; }
    net_tcpclient_stop(c);
    kn_error_clear();
    kn_ret_bool(ret, 1);
}

/* tcpclient_connected(client) -> bool */
void tcpclient_connected(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    NetTcpClient *c = net_tcpclient_named(kn_arg_text(argv, 0));
    if (!c) { kn_ret_bool(ret, 0); return; }
    kn_error_clear();
    kn_ret_bool(ret, c->state == NET_CL_CONNECTED);
}
