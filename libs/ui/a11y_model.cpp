#include "a11y_model.h"

#include <algorithm>
#include <mutex>

namespace kiln::a11y {
namespace {
std::mutex g_mutex;
std::vector<Node> g_nodes;
std::vector<uint64_t> g_actions;
uint64_t g_focus = 0;

Node* find_locked(uint64_t id) {
    auto it = std::find_if(g_nodes.begin(), g_nodes.end(), [id](const Node& n) { return n.id == id; });
    return it == g_nodes.end() ? nullptr : &*it;
}
} // namespace

void put_node(const Node& n) {
    std::lock_guard<std::mutex> lock(g_mutex);
    if (Node* existing = find_locked(n.id)) {
        // Preserve bounds already measured this frame.
        Node merged = n;
        if (merged.w == 0 && merged.h == 0) {
            merged.x = existing->x; merged.y = existing->y;
            merged.w = existing->w; merged.h = existing->h;
        }
        *existing = merged;
    } else {
        g_nodes.push_back(n);
    }
    if (g_focus == 0) g_focus = n.id;
}

void set_bounds(uint64_t id, float x, float y, float w, float h) {
    std::lock_guard<std::mutex> lock(g_mutex);
    if (Node* n = find_locked(id)) { n->x = x; n->y = y; n->w = w; n->h = h; }
}

void set_label(uint64_t id, const std::string& label) {
    std::lock_guard<std::mutex> lock(g_mutex);
    if (Node* n = find_locked(id)) n->label = label;
}

std::vector<Node> snapshot() {
    std::lock_guard<std::mutex> lock(g_mutex);
    return g_nodes;
}

uint64_t focus() {
    std::lock_guard<std::mutex> lock(g_mutex);
    return g_focus;
}

void set_focus(uint64_t id) {
    std::lock_guard<std::mutex> lock(g_mutex);
    g_focus = id;
}

void queue_action(uint64_t id) {
    std::lock_guard<std::mutex> lock(g_mutex);
    g_actions.push_back(id);
}

std::vector<uint64_t> take_actions() {
    std::lock_guard<std::mutex> lock(g_mutex);
    std::vector<uint64_t> out;
    out.swap(g_actions);
    return out;
}

void clear() {
    std::lock_guard<std::mutex> lock(g_mutex);
    g_nodes.clear();
    g_actions.clear();
    g_focus = 0;
}

} // namespace kiln::a11y
