/* Bridge between the substrate-free a11y model and the platform adapter.
 * Kept separate so ui_rmlui.cpp never includes AccessKit headers. */
#ifndef KILN_A11Y_BRIDGE_H
#define KILN_A11Y_BRIDGE_H

namespace kiln::a11y {

void bridge_init();       /* create the platform adapter (no-op if unavailable) */

/// Tell the adapter where the window sits on screen, so assistive technologies
/// can map node bounds to screen coordinates. Node bounds are window-relative;
/// without this they are meaningless to an AT.
///
/// **Wayland cannot supply this** — a client may not learn its own window
/// position — so on Wayland this is a no-op and accessible coordinates remain
/// window-relative. Correct on X11. (AccessKit documents the same limitation.)
void bridge_set_window_bounds(float x, float y, float w, float h);
void bridge_publish();    /* push the current model; cheap when no AT is attached */
bool bridge_active();     /* has an assistive technology connected? */
void bridge_shutdown();

} // namespace kiln::a11y
#endif
