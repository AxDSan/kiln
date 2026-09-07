/* The library's component entry points (abi/kiln_abi.h), for every
 * non-visual component `net` declares.
 *
 * There is one `kn_net_component_create` for the whole library, and the
 * backend hands out handles per LIBRARY in creation order, not per type: a
 * program with an httpserver and then a tcpserver addresses them as 1 and 2.
 * So a handle here names a row in one table, and the row remembers which type
 * it is; each type's own file supplies the hooks in its NetComponentType.  A
 * second counter per type would have handed the compiler numbers it did not
 * assign.
 */
#include "net_internal.h"

#define NET_COMPONENTS_MAX 32

static const NetComponentType NET_TYPES[] = {
    { "httpserver",
      net_httpd_create, net_httpd_set, net_httpd_get,
      net_httpd_get_int, net_httpd_on },
    { "tcpserver",
      net_tcpserver_create, net_tcpserver_set, net_tcpserver_get,
      net_tcpserver_get_int, net_tcpserver_on },
    { "tcpclient",
      net_tcpclient_create, net_tcpclient_set, net_tcpclient_get,
      net_tcpclient_get_int, net_tcpclient_on },
};

typedef struct {
    const NetComponentType *type;
    void                   *obj;
} NetComponent;

static NetComponent g_components[NET_COMPONENTS_MAX];
static int32_t      g_next_handle;

static NetComponent *net_component_of(int64_t h) {
    if (h < 1 || h > g_next_handle) return NULL;
    return &g_components[h - 1];
}

int net_bool_of(const char *value) {
    return strcmp(value, "true") == 0 || strcmp(value, "1") == 0;
}

int64_t kn_net_component_create(const char *type_name) {
    const NetComponentType *t = NULL;
    for (size_t i = 0; type_name && i < sizeof NET_TYPES / sizeof NET_TYPES[0]; i++) {
        if (strcmp(NET_TYPES[i].type_name, type_name) == 0) { t = &NET_TYPES[i]; break; }
    }
    if (!t) {
        kn_error_set(KN_ERR_INVALID_ARG, "net declares no such component type");
        return 0;
    }
    if (g_next_handle >= NET_COMPONENTS_MAX) {
        kn_error_set(KN_ERR_TABLE_FULL, "too many net components");
        return 0;
    }
    void *obj = t->create();
    if (!obj) return 0;                    /* the hook set the error slot   */
    NetComponent *c = &g_components[g_next_handle++];
    c->type = t;
    c->obj = obj;
    kn_error_clear();
    return g_next_handle;
}

int32_t kn_net_component_set(int64_t h, const char *prop, const char *value) {
    NetComponent *c = net_component_of(h);
    if (!c || !prop || !value) return 1;
    return c->type->set(c->obj, prop, value);
}

const char *kn_net_component_get(int64_t h, const char *prop) {
    NetComponent *c = net_component_of(h);
    if (!c || !prop) return NULL;
    return c->type->get(c->obj, prop);
}

int32_t kn_net_component_get_int(int64_t h, const char *prop) {
    NetComponent *c = net_component_of(h);
    if (!c || !prop) return 0;
    return c->type->get_int(c->obj, prop);
}

int32_t kn_net_component_on(int64_t h, const char *event, Kiln_HandlerFn handler) {
    NetComponent *c = net_component_of(h);
    if (!c || !event || !handler) return 1;
    return c->type->on(c->obj, event, handler);
}
