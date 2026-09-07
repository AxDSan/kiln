/* TLS for the HTTP client, on mbedTLS, compiled only when mbedTLS is vendored.
 *
 * The whole file is one `#ifdef KILN_NET_TLS`, and the macro comes from
 * `optional_requires` in lib.json matching the archives tools/fetch-mbedtls.sh
 * builds.  Nobody has to fetch it: without it this translation unit is the
 * three stubs at the bottom, `net_tls_available()` answers 0, and net_cmds.c
 * refuses https rather than downgrading it.
 *
 * Two rules govern everything here, and neither is negotiable:
 *
 *   Certificates are verified, always.  An https client that skips
 *   verification offers the appearance of security and none of it — anyone on
 *   the path can present their own certificate and read the traffic — which is
 *   worse than the honest refusal it replaced, because the refusal is visible
 *   and this is not.  So the authmode is REQUIRED, the hostname is set (which
 *   is both SNI and the name the certificate is checked against), and a
 *   handshake that cannot be verified fails.
 *
 *   No CA store means no connection.  Falling back to "trust everything"
 *   when the trust store cannot be found is the same hole arriving by a
 *   different door, so a machine with no bundle gets KN_ERR_UNSUPPORTED and a
 *   message naming KILN_CA_BUNDLE.
 */
#include "net_internal.h"

#ifdef KILN_NET_TLS

#include <mbedtls/ssl.h>
#include <mbedtls/entropy.h>
#include <mbedtls/ctr_drbg.h>
#include <mbedtls/x509_crt.h>
#include <mbedtls/error.h>
#include <mbedtls/net_sockets.h>

struct NetTls {
    mbedtls_ssl_context      ssl;
    mbedtls_ssl_config       conf;
    mbedtls_entropy_context  entropy;
    mbedtls_ctr_drbg_context drbg;
    int                      fd;
};

/* The trust store, parsed once: it is a few hundred certificates, and every
 * request would otherwise re-read and re-parse the lot. */
static mbedtls_x509_crt g_ca;
static int g_ca_state = 0;             /* 0 unread, 1 loaded, -1 unavailable */
static char g_ca_where[512];

/* Where a Linux or BSD keeps the trust store.  There is no standard location,
 * only a list of the ones distributions actually use, so the list is the
 * mechanism.  KILN_CA_BUNDLE comes first because a container, a corporate
 * proxy or a test needs to say "this one" and be believed. */
static const char *const CA_FILES[] = {
    "/etc/ssl/certs/ca-certificates.crt",       /* Debian, Ubuntu, Alpine    */
    "/etc/pki/tls/certs/ca-bundle.crt",         /* Fedora, RHEL              */
    "/etc/ssl/ca-bundle.pem",                   /* openSUSE                  */
    "/etc/ssl/cert.pem",                        /* macOS ports, FreeBSD      */
    "/usr/local/share/certs/ca-root-nss.crt",   /* FreeBSD                   */
};

/* mbedTLS turns its negative codes into a sentence; without this every failure
 * here would read as a bare hexadecimal number. */
static void net_tls_fail(int rc, const char *what) {
    char detail[192];
    mbedtls_strerror(rc, detail, sizeof detail);
    char msg[320];
    snprintf(msg, sizeof msg, "%s: %s", what, detail);
    kn_error_set(KN_ERR_INVALID_ARG, msg);
}

/* Load the trust store, or decide there is none.  1 = loaded. */
static int net_tls_load_ca(void) {
    if (g_ca_state) return g_ca_state == 1;
    g_ca_state = -1;
    mbedtls_x509_crt_init(&g_ca);

    const char *env = getenv("KILN_CA_BUNDLE");
    if (env && *env) {
        /* A directory of certificates and a single bundle file are both common
         * spellings of the same thing, and a caller should not have to know
         * which one they have. */
        int rc = mbedtls_x509_crt_parse_file(&g_ca, env);
        if (rc != 0) rc = mbedtls_x509_crt_parse_path(&g_ca, env);
        /* A POSITIVE result means some certificates in the bundle were skipped
         * and the rest parsed, which is the normal state of a system store —
         * treating it as failure would reject nearly every machine. */
        if (rc >= 0) {
            snprintf(g_ca_where, sizeof g_ca_where, "%s", env);
            g_ca_state = 1;
            return 1;
        }
        /* Named explicitly and unusable: say so rather than quietly searching
         * elsewhere and verifying against a store nobody asked for. */
        char detail[192];
        mbedtls_strerror(rc, detail, sizeof detail);
        char msg[400];
        snprintf(msg, sizeof msg,
                 "KILN_CA_BUNDLE names %s, which could not be read as "
                 "certificates: %s", env, detail);
        kn_error_set(KN_ERR_INVALID_ARG, msg);
        mbedtls_x509_crt_free(&g_ca);
        g_ca_state = -2;                     /* the slot is already set */
        return 0;
    }

    for (size_t i = 0; i < sizeof CA_FILES / sizeof CA_FILES[0]; i++) {
        if (mbedtls_x509_crt_parse_file(&g_ca, CA_FILES[i]) >= 0) {
            snprintf(g_ca_where, sizeof g_ca_where, "%s", CA_FILES[i]);
            g_ca_state = 1;
            return 1;
        }
    }
    mbedtls_x509_crt_free(&g_ca);
    return 0;
}

/* Read and write the socket for mbedTLS.  The fd already carries the send and
 * receive timeouts net_dial set, so a stalled peer fails the handshake instead
 * of hanging a program that looks merely slow. */
static int net_tls_bio_send(void *ctx, const unsigned char *buf, size_t len) {
    int fd = (int)(intptr_t)ctx;
    ssize_t w = send(fd, (const char *)buf, (int)len, MSG_NOSIGNAL);
    if (w >= 0) return (int)w;
    return MBEDTLS_ERR_NET_SEND_FAILED;
}

static int net_tls_bio_recv(void *ctx, unsigned char *buf, size_t len) {
    int fd = (int)(intptr_t)ctx;
    ssize_t r = recv(fd, (char *)buf, (int)len, 0);
    if (r >= 0) return (int)r;
    return MBEDTLS_ERR_NET_RECV_FAILED;
}

int net_tls_available(void) { return 1; }

NetTls *net_tls_start(int fd, const char *host) {
    if (!net_tls_load_ca()) {
        if (g_ca_state == -2) return NULL;         /* slot already set */
        kn_error_set(KN_ERR_UNSUPPORTED,
                     "https needs a certificate authority store and none was "
                     "found on this machine: install the system CA bundle, or "
                     "set KILN_CA_BUNDLE to one. Certificates are never "
                     "left unverified");
        return NULL;
    }

    NetTls *t = (NetTls *)calloc(1, sizeof *t);
    if (!t) { kn_error_set_errno(ENOMEM, "tls"); return NULL; }
    t->fd = fd;
    mbedtls_ssl_init(&t->ssl);
    mbedtls_ssl_config_init(&t->conf);
    mbedtls_entropy_init(&t->entropy);
    mbedtls_ctr_drbg_init(&t->drbg);

    const char *pers = "kiln-net";
    int rc = mbedtls_ctr_drbg_seed(&t->drbg, mbedtls_entropy_func, &t->entropy,
                                   (const unsigned char *)pers, strlen(pers));
    if (rc != 0) { net_tls_fail(rc, "seed the random generator"); goto fail; }

    rc = mbedtls_ssl_config_defaults(&t->conf, MBEDTLS_SSL_IS_CLIENT,
                                     MBEDTLS_SSL_TRANSPORT_STREAM,
                                     MBEDTLS_SSL_PRESET_DEFAULT);
    if (rc != 0) { net_tls_fail(rc, "configure tls"); goto fail; }

    /* REQUIRED, and set before the handshake: with OPTIONAL, mbedTLS completes
     * a handshake against any certificate at all and leaves the verdict for a
     * caller to remember to ask for. */
    mbedtls_ssl_conf_authmode(&t->conf, MBEDTLS_SSL_VERIFY_REQUIRED);
    mbedtls_ssl_conf_ca_chain(&t->conf, &g_ca, NULL);
    mbedtls_ssl_conf_rng(&t->conf, mbedtls_ctr_drbg_random, &t->drbg);

    rc = mbedtls_ssl_setup(&t->ssl, &t->conf);
    if (rc != 0) { net_tls_fail(rc, "set up tls"); goto fail; }

    /* Both the SNI name and the name the certificate must match.  Omitting it
     * would leave the connection encrypted but unauthenticated — any valid
     * certificate for any host would pass. */
    rc = mbedtls_ssl_set_hostname(&t->ssl, host);
    if (rc != 0) { net_tls_fail(rc, "set the tls hostname"); goto fail; }

    mbedtls_ssl_set_bio(&t->ssl, (void *)(intptr_t)fd,
                        net_tls_bio_send, net_tls_bio_recv, NULL);

    while ((rc = mbedtls_ssl_handshake(&t->ssl)) != 0) {
        if (rc == MBEDTLS_ERR_SSL_WANT_READ || rc == MBEDTLS_ERR_SSL_WANT_WRITE)
            continue;
        if (rc == MBEDTLS_ERR_X509_CERT_VERIFY_FAILED) {
            /* The failure a user most needs the detail of: expired, wrong
             * name, unknown issuer.  A bare "handshake failed" here sends
             * people looking for a network problem they do not have. */
            char why[512];
            uint32_t flags = mbedtls_ssl_get_verify_result(&t->ssl);
            if (mbedtls_x509_crt_verify_info(why, sizeof why, "", flags) <= 0)
                snprintf(why, sizeof why, "unknown reason\n");
            for (char *p = why; *p; p++) if (*p == '\n') *p = ' ';
            char msg[700];
            snprintf(msg, sizeof msg,
                     "the certificate for %s could not be verified against %s: "
                     "%s", host, g_ca_where, why);
            kn_error_set(KN_ERR_INVALID_ARG, msg);
            goto fail;
        }
        net_tls_fail(rc, "tls handshake");
        goto fail;
    }
    return t;

fail:
    mbedtls_ssl_free(&t->ssl);
    mbedtls_ssl_config_free(&t->conf);
    mbedtls_ctr_drbg_free(&t->drbg);
    mbedtls_entropy_free(&t->entropy);
    free(t);
    return NULL;
}

int net_tls_send(NetTls *t, const char *p, size_t n) {
    while (n > 0) {
        int w = mbedtls_ssl_write(&t->ssl, (const unsigned char *)p, n);
        if (w == MBEDTLS_ERR_SSL_WANT_READ || w == MBEDTLS_ERR_SSL_WANT_WRITE)
            continue;
        if (w <= 0) { net_tls_fail(w, "send over tls"); return 0; }
        p += w;
        n -= (size_t)w;
    }
    return 1;
}

long net_tls_recv(NetTls *t, char *p, size_t n) {
    for (;;) {
        int r = mbedtls_ssl_read(&t->ssl, (unsigned char *)p, n);
        if (r == MBEDTLS_ERR_SSL_WANT_READ || r == MBEDTLS_ERR_SSL_WANT_WRITE)
            continue;
        /* Both of these are the end of the response, not a failure: the first
         * is a peer that closed the session politely, the second one that shut
         * the socket after its last byte, which is what `Connection: close`
         * asks for. */
        if (r == MBEDTLS_ERR_SSL_PEER_CLOSE_NOTIFY) return 0;
        if (r == MBEDTLS_ERR_NET_RECV_FAILED) return 0;
        if (r < 0) { net_tls_fail(r, "receive over tls"); return -1; }
        return r;
    }
}

void net_tls_free(NetTls *t) {
    if (!t) return;
    mbedtls_ssl_close_notify(&t->ssl);
    mbedtls_ssl_free(&t->ssl);
    mbedtls_ssl_config_free(&t->conf);
    mbedtls_ctr_drbg_free(&t->drbg);
    mbedtls_entropy_free(&t->entropy);
    free(t);
}

#else /* no mbedTLS vendored */

/* The stubs exist so that net_cmds.c has one shape to compile against in both
 * states, and so this file is never an empty translation unit — which ISO C
 * does not allow and -Wall reports. `net_tls_available()` answering 0 is what
 * turns every https URL into a loud refusal at the point it is asked for. */
int net_tls_available(void) { return 0; }

NetTls *net_tls_start(int fd, const char *host) {
    (void)fd; (void)host;
    kn_error_set(KN_ERR_UNSUPPORTED,
                 "https is not available: this build has no TLS. Run "
                 "tools/fetch-mbedtls.sh and rebuild. The request is never "
                 "downgraded to http");
    return NULL;
}

int net_tls_send(NetTls *t, const char *p, size_t n) {
    (void)t; (void)p; (void)n;
    return 0;
}

long net_tls_recv(NetTls *t, char *p, size_t n) {
    (void)t; (void)p; (void)n;
    return -1;
}

void net_tls_free(NetTls *t) { (void)t; }

#endif
