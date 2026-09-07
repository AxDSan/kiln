/* Runtime lifecycle.
 *
 * Phase 2: E_Init prepares the runtime; E_DestroyRes releases all runtime-owned
 * allocations.  Per-library init dispatch (the BlackMoonInitAllElib analog that
 * hands each linked library the notify callback) is deferred — spike libraries
 * are stateless and reach the runtime through the global `kn_notify` symbol
 *. */
#include <stdio.h>
#include "kiln_core.h"

void kn_free_all(void); /* from kn_mem.c */

void E_Init(void) {
    /* Line-buffer stdout so a program's output appears as it happens, even when
     * piped. Without this, printf() is fully buffered to a pipe and a GUI app's
     * feedback only surfaces at exit — which looks exactly like nothing working.
     */
    setvbuf(stdout, NULL, _IOLBF, 0);
}

void E_DestroyRes(void) {
    /* Handles first, then blocks.  A handle's payload may itself be a runtime
     * allocation, so freeing blocks first would leave a close function reading
     * memory that is already gone.  Teardown runs in reverse of construction. */
    kn_handle_close_all();
    kn_free_all();
}
