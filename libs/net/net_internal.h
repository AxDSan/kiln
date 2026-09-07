/* What the two halves of `net` — the client commands and the HTTP server
 * component — both need.
 *
 * The platform shim lives here rather than in net_cmds.c so that ONE place in
 * the library knows the platform, not one place per file: a second copy in the
 * server would be a second set of Winsock spellings to keep in step, and the
 * first divergence would only show up on Windows.
 */
#ifndef KILN_NET_INTERNAL_H
#define KILN_NET_INTERNAL_H

#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>
#include <time.h>

/* Winsock is the same BSD socket API with three differences that matter here:
 * it must be started before use, its errors live in WSAGetLastError() rather
 * than errno and are NOT errno values, and a socket is closed by closesocket
 * rather than close.  Everything below is written once and reads the same on
 * both platforms through the shims in this block. */
#ifdef _WIN32
#include <winsock2.h>
#include <ws2tcpip.h>
#include <stdint.h>

#define net_errno()      WSAGetLastError()
#define NET_EWOULDBLOCK  WSAEWOULDBLOCK
/* Winsock reports a connect that has not finished as WOULDBLOCK, where POSIX
 * says EINPROGRESS; the two spell the same state. */
#define NET_EINPROGRESS  WSAEWOULDBLOCK
#define NET_EINTR        WSAEINTR
#define NET_ETIMEDOUT    WSAETIMEDOUT
#define NET_ECONNREFUSED WSAECONNREFUSED
#define NET_EPIPE        WSAECONNRESET
#define close            closesocket
#define poll             WSAPoll
#define strcasecmp       _stricmp
#define strncasecmp      _strnicmp
#define MSG_NOSIGNAL     0
#ifdef _MSC_VER
typedef SSIZE_T ssize_t;
#endif
/* MSVC and clang-cl honour this; a mingw link needs -lws2_32 on the command
 * line, which no lib.json key can express today. */
#if defined(_MSC_VER)
#pragma comment(lib, "ws2_32.lib")
#endif
#else
#include <fcntl.h>
#include <netdb.h>
#include <poll.h>
#include <strings.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <netinet/in.h>
#include <arpa/inet.h>
#include <unistd.h>
#include <stdint.h>

#define net_errno()      errno
#define NET_EWOULDBLOCK  EWOULDBLOCK
#define NET_EINPROGRESS  EINPROGRESS
#define NET_EINTR        EINTR
#define NET_ETIMEDOUT    ETIMEDOUT
#define NET_ECONNREFUSED ECONNREFUSED
#define NET_EPIPE        EPIPE

#ifndef MSG_NOSIGNAL
#define MSG_NOSIGNAL 0
#endif
#endif

#include "kiln_abi.h"

/* Winsock refuses every call until WSAStartup has run; a POSIX build answers 1
 * with nothing to do.  Both halves of the library call it before their first
 * socket. */
int net_start(void);

/* A connected socket behind a handle.
 *
 * An HTTP request is a NetSock followed by its parsed request, and the request
 * handle carries the SAME kind as a client socket (KN_HK_SOCKET), because
 * handle kinds are assigned in abi/kiln_abi.h and a library may not invent
 * one.  Making this the first member is what keeps that safe: a request handle
 * passed to net_tcp_send reaches a real, live socket view of the same
 * connection rather than a struct reinterpreted as one. */
typedef struct {
    int  fd;
    int  eof;              /* the peer closed, or a read returned 0          */
    char buf[4096];        /* pushback, so receive_line can stop at a byte   */
    int  blen, bpos;
} NetSock;

void net_sock_close(void *payload);

/* A growable scratch buffer.  Plain malloc, deliberately: this is library
 * bookkeeping, not program data, so it must not sit in the runtime's tracked
 * block list where it would outlive the command that built it. */
typedef struct { char *p; size_t n, cap; } NetBuf;

int  net_buf_add(NetBuf *b, const char *s, size_t n);
void net_buf_free(NetBuf *b);

/* A socket failure carries a platform code, and a Winsock code is not an errno
 * value — comparing one against ECONNREFUSED would be meaningless — so it
 * reaches the error slot as itself. */
void net_fail(int code, const char *what);

const char *net_nz(const char *s);

/* Every text result is runtime-owned, including the "" failure sentinel, so a
 * program can hold a failed result with no special case. */
char *net_text(const char *p, size_t n);
char *net_empty(void);

/* Put a socket in non-blocking mode.  The server never blocks: the event loop
 * has to keep turning, or a window with a server in it stops repainting. */
int net_set_nonblocking(int fd);

/* Monotonic, so a clock adjustment cannot make a connection's idle deadline
 * expire an hour early or never. */
int64_t net_now_ms(void);

/* --- TLS (net_tls.c) --------------------------------------------------
 * Declared unconditionally and implemented twice — once on mbedTLS, once as
 * stubs — so net_cmds.c is the same code whether or not a TLS stack was
 * vendored.  The one thing a caller must branch on is net_tls_available():
 * with no TLS an https URL is REFUSED, never rewritten to http, because a
 * downgrade puts a password on the wire and looks like success. */
typedef struct NetTls NetTls;

int  net_tls_available(void);

/* Take over a connected socket.  NULL on failure with the error slot set —
 * including the failure that matters most, a certificate that does not verify.
 * `host` is the name to check the certificate against, so it must be the name
 * from the URL and never an address it resolved to. */
NetTls *net_tls_start(int fd, const char *host);

int  net_tls_send(NetTls *t, const char *p, size_t n);   /* 0 = failure     */
long net_tls_recv(NetTls *t, char *p, size_t n);         /* 0 = end, -1 err */
void net_tls_free(NetTls *t);                            /* not the socket  */

/* --- components (net_component.c) ---------------------------------------
 * The library's entry points — kn_net_component_create and its four siblings
 * — are ONE set for the whole library, and the backend numbers handles per
 * library in creation order whatever the type (backend/src/lib.rs,
 * `build_component`): a tcpserver declared after an httpserver is handle 2.
 * So the entry points live in one file and dispatch on type, and each type
 * provides these five hooks.  `create` answers NULL with the error slot set;
 * the rest are handed a non-NULL object, property and value, and answer 0 for
 * done and 1 for "not mine". */
typedef struct {
    const char  *type_name;
    void       *(*create)(void);
    int32_t     (*set)(void *obj, const char *prop, const char *value);
    const char *(*get)(void *obj, const char *prop);
    int32_t     (*get_int)(void *obj, const char *prop);
    int32_t     (*on)(void *obj, const char *event, Kiln_HandlerFn fn);
} NetComponentType;

void       *net_httpd_create(void);
int32_t     net_httpd_set(void *obj, const char *prop, const char *value);
const char *net_httpd_get(void *obj, const char *prop);
int32_t     net_httpd_get_int(void *obj, const char *prop);
int32_t     net_httpd_on(void *obj, const char *event, Kiln_HandlerFn fn);

void       *net_tcpserver_create(void);
int32_t     net_tcpserver_set(void *obj, const char *prop, const char *value);
const char *net_tcpserver_get(void *obj, const char *prop);
int32_t     net_tcpserver_get_int(void *obj, const char *prop);
int32_t     net_tcpserver_on(void *obj, const char *event, Kiln_HandlerFn fn);

void       *net_tcpclient_create(void);
int32_t     net_tcpclient_set(void *obj, const char *prop, const char *value);
const char *net_tcpclient_get(void *obj, const char *prop);
int32_t     net_tcpclient_get_int(void *obj, const char *prop);
int32_t     net_tcpclient_on(void *obj, const char *event, Kiln_HandlerFn fn);

/* A truth value crosses the property boundary as text, spelled the way the
 * source and every descriptor default spell it. */
int net_bool_of(const char *value);

#endif /* KILN_NET_INTERNAL_H */
