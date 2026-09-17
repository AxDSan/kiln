/* A function-pointer detour runtime — the portable core of what a hooking
 * library like Detours does by rewriting a function's first bytes.
 *
 * Built to `libhookrt.so` (or `hookrt.dll`). It stands in for "the application"
 * in the worked hook: it owns a function everyone calls through a slot, and the
 * install call that redirects that slot. The Kiln library (`hook.kiln`) reaches
 * these three exports through `dll` declarations; the host program links this
 * library and makes the calls. Because there is one copy of the slot in the one
 * loaded `libhookrt`, a detour the Kiln library installs is seen by the host —
 * which is the whole point of an in-process hook.
 */

/* The original behaviour a caller sees until a hook is installed: double it. */
static int base_target(int x) { return x * 2; }

/* `slot` is what `hookrt_call` dispatches to; `trampoline` always reaches the
 * original. Installing a hook points `slot` at a detour and keeps the old `slot`
 * as `trampoline`, so the detour can still call what it replaced. */
static int (*slot)(int) = base_target;
static int (*trampoline)(int) = base_target;

/* The application's call site: every call goes through the slot, so redirecting
 * the slot redirects every future call — the host's included. */
int hookrt_call(int x) { return slot(x); }

/* How a detour reaches the un-hooked original: through the saved trampoline. */
int hookrt_original(int x) { return trampoline(x); }

/* Install a detour. The detour arrives as a `void *` — that is how Kiln's
 * `ptr` marshals, and `address of` hands over exactly a function pointer of this
 * shape. The current slot becomes the trampoline; the slot becomes the detour. */
void hookrt_install(void *detour) {
    trampoline = slot;
    slot = (int (*)(int))detour;
}
