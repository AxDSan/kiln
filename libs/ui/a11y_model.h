/* Accessibility model — the thread boundary.
 *
 * AccessKit's adapter states plainly in its own header: *"All of the handlers
 * will always be called from another thread."* RmlUi is not thread-safe, so an
 * adapter callback must never touch a widget. This model is the barrier: the
 * main thread publishes a mutex-guarded snapshot of the widget tree, and the
 * adapter thread only ever reads that.
 *
 * It also holds the D10 line: no substrate types here, and no AccessKit types
 * either, so `a11y_accesskit.cpp` and `ui_rmlui.cpp` never need to see each
 * other's headers.
 */
#ifndef KILN_A11Y_MODEL_H
#define KILN_A11Y_MODEL_H

#include <cstdint>
#include <string>
#include <vector>

namespace kiln::a11y {

/* One accessible node, mirroring a widget. `id` IS the widget handle — the two
 * are both u64 and deliberately identical, so no mapping table can drift. */
struct Node {
    uint64_t id = 0;
    uint64_t parent = 0;         /* 0 = root */
    int32_t role = 0;            /* KN_ROLE_* (abi/kiln_abi.h) */
    std::string label;           /* accessible name; may be empty */
    float x = 0, y = 0, w = 0, h = 0;
    bool clickable = false;      /* exposes the default action */
};

/// Publish/replace a node (main thread only).
void put_node(const Node& n);

/// Update just the bounds of a node (main thread, per frame).
void set_bounds(uint64_t id, float x, float y, float w, float h);

/// Update a node's accessible name. Must be called whenever the user-visible
/// text changes, or an assistive technology announces stale content — the
/// accessible name is not a construction-time snapshot.
void set_label(uint64_t id, const std::string& label);

/// A consistent copy for the adapter thread to read.
std::vector<Node> snapshot();

/// The node that should be reported as focused; the root if nothing else.
uint64_t focus();
void set_focus(uint64_t id);

/// An assistive technology asked to activate a node. Called on the ADAPTER
/// thread — it only enqueues; the UI loop drains and dispatches on the main
/// thread, where touching widgets is safe.
void queue_action(uint64_t id);

/// Drain queued actions (main thread).
std::vector<uint64_t> take_actions();

/// Reset everything (shutdown).
void clear();

} // namespace kiln::a11y
#endif
