/* The "hello" support library — a minimal third-party Kiln library (.4,
 * M5). It includes ONLY the public SDK header (abi/kiln_abi.h) — not any
 * runtime-internal header — and allocates its result through the notification
 * channel (kn_malloc -> kn_notify), proving the ABI is a real extension point. */
#include <string.h>
#include "kiln_abi.h"

/* greet(text name) -> text : "Hello, <name>!" */
void hello_greet(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const char *name = kn_arg_text(argv, 0);
    if (!name) name = "";
    const char *pre = "Hello, ", *post = "!";
    long n = (long)(strlen(pre) + strlen(name) + strlen(post) + 1);
    char *out = (char *)kn_malloc(n);
    strcpy(out, pre);
    strcat(out, name);
    strcat(out, post);
    kn_ret_text(ret, out);
}
