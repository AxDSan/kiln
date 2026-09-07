/* AccessKit bridge.
 *
 * Turns the substrate-free a11y model into an AccessKit tree and serves it to
 * assistive technologies over AT-SPI. This file includes AccessKit and the
 * model — never RmlUi — so the substrate stays swappable (D10), and the model's
 * mutex is what makes the adapter's cross-thread callbacks safe.
 *
 * Degrades to nothing: with no accessibility bus, no D-Bus, or KILN_NO_A11Y=1,
 * the app runs identically. An app that breaks when accessibility infrastructure
 * is absent would fail the very requirement this exists to satisfy.
 */
#include <cstdio>
#include <cstdlib>

#include "a11y_bridge.h"
#include "a11y_model.h"

#if defined(__linux__) || defined(__FreeBSD__) || defined(__DragonFly__) || \
    defined(__NetBSD__) || defined(__OpenBSD__)
    #define KILN_A11Y_UNIX 1
    #include <accesskit.h>
#endif

namespace {

#ifdef KILN_A11Y_UNIX
accesskit_unix_adapter* g_adapter = nullptr;
bool g_activated = false;

accesskit_role map_role(int32_t role) {
    switch (role) {
        case 1:  return ACCESSKIT_ROLE_WINDOW;
        case 2:  return ACCESSKIT_ROLE_BUTTON;
        case 3:  return ACCESSKIT_ROLE_LABEL;
        case 7:  return ACCESSKIT_ROLE_GROUP;
        default: return ACCESSKIT_ROLE_UNKNOWN;
    }
}

/* Build a full tree update from the model snapshot. Safe on any thread: it only
 * reads the mutex-guarded model. */
accesskit_tree_update* build_update() {
    auto nodes = kiln::a11y::snapshot();
    if (nodes.empty()) return nullptr;

    const accesskit_node_id root = nodes.front().id;
    accesskit_tree_update* update =
        accesskit_tree_update_with_focus(kiln::a11y::focus());

    for (const auto& n : nodes) {
        accesskit_node* node = accesskit_node_new(map_role(n.role));
        if (!n.label.empty()) accesskit_node_set_label(node, n.label.c_str());
        accesskit_node_set_bounds(node, {n.x, n.y, n.x + n.w, n.y + n.h});
        if (n.clickable) {
            accesskit_node_add_action(node, ACCESSKIT_ACTION_CLICK);
            accesskit_node_add_action(node, ACCESSKIT_ACTION_FOCUS);
        }
        /* Children, by the parent link. */
        for (const auto& child : nodes) {
            if (child.parent == n.id && child.id != n.id) {
                accesskit_node_push_child(node, child.id);
            }
        }
        accesskit_tree_update_push_node(update, n.id, node);
    }

    accesskit_tree* tree = accesskit_tree_new(root);
    accesskit_tree_update_set_tree(update, tree);
    return update;
}

/* --- adapter callbacks: ALL of these run on the adapter's own thread ------ */

accesskit_tree_update* activation_handler(void*) {
    g_activated = true;
    std::fprintf(stderr, "kiln-a11y: assistive technology connected\n");
    return build_update();
}

void action_handler(accesskit_action_request* request, void*) {
    if (request) {
        if (std::getenv("KILN_A11Y_TRACE"))
            std::fprintf(stderr, "kiln-a11y: action=%d node=%llu\n",
                         (int)request->action, (unsigned long long)request->target_node);
        if (request->action == ACCESSKIT_ACTION_CLICK) {
            /* Do NOT touch widgets here — this is the adapter thread. Queue it;
             * the UI loop dispatches on the main thread next frame. */
            kiln::a11y::queue_action(request->target_node);
        }
        accesskit_action_request_free(request);
    }
}

void deactivation_handler(void*) { g_activated = false; }
#endif // KILN_A11Y_UNIX

bool disabled() {
    const char* v = std::getenv("KILN_NO_A11Y");
    return v && *v && v[0] != '0';
}

} // namespace

namespace kiln::a11y {

void bridge_init() {
#ifdef KILN_A11Y_UNIX
    if (disabled() || g_adapter) return;
    g_adapter = accesskit_unix_adapter_new(activation_handler, nullptr,
                                           action_handler, nullptr,
                                           deactivation_handler, nullptr);
    if (!g_adapter) {
        /* No accessibility bus, or D-Bus unavailable. Not an error. */
        std::fprintf(stderr, "kiln-a11y: no accessibility bus; continuing without\n");
    }
#endif
}

void bridge_publish() {
#ifdef KILN_A11Y_UNIX
    if (!g_adapter) return;
    /* update_if_active does nothing until an AT actually connects, so this is
     * cheap to call every frame. The factory is only invoked when needed. */
    accesskit_unix_adapter_update_if_active(g_adapter, [](void*) { return build_update(); }, nullptr);
#endif
}

void bridge_set_window_bounds(float x, float y, float w, float h) {
#ifdef KILN_A11Y_UNIX
    if (!g_adapter) return;
    const accesskit_rect bounds = {x, y, x + w, y + h};
    /* Outer and inner are the same here: we have no window decorations to
     * account for, and on Wayland the caller passes 0,0 anyway. */
    accesskit_unix_adapter_set_root_window_bounds(g_adapter, bounds, bounds);
#else
    (void)x; (void)y; (void)w; (void)h;
#endif
}

bool bridge_active() {
#ifdef KILN_A11Y_UNIX
    return g_activated;
#else
    return false;
#endif
}

void bridge_shutdown() {
#ifdef KILN_A11Y_UNIX
    if (g_adapter) {
        accesskit_unix_adapter_free(g_adapter);
        g_adapter = nullptr;
    }
    g_activated = false;
#endif
}

} // namespace kiln::a11y
