/* The host for the hook example: an ordinary program that calls a function,
 * then loads the Kiln library and calls the same function again.
 *
 * It links `hookrt` and calls `hookrt_call` directly — it is the "application"
 * whose function gets hooked. Loading `libhook.so` / `hook.dll` fires that
 * library's `dll_attach`, which installs a detour into the hookrt slot this
 * program is already using. So the second call returns a different value, in
 * this one process, with nothing patched by hand: the before/after is the hook.
 *
 * Build (Linux):
 *   clang -shared -fPIC examples/hook/hookrt.c -o libhookrt.so
 *   kiln build examples/hook/hook.kiln --target sharedlib -o libhook.so
 *   clang examples/hook/host.c -L. -lhookrt -Wl,-rpath,. -ldl -o host
 *   ./host            # prints "before 20" then "after 21"
 */
#include <stdio.h>

/* Provided by libhookrt. */
extern int hookrt_call(int x);

#ifdef _WIN32
#include <windows.h>
static int load_hook(void) { return LoadLibraryA("hook.dll") != NULL; }
#else
#include <dlfcn.h>
static int load_hook(void) { return dlopen("./libhook.so", RTLD_NOW | RTLD_LOCAL) != NULL; }
#endif

int main(void) {
    /* The original: hookrt doubles its argument. */
    printf("before %d\n", hookrt_call(10));

    /* Loading the library runs its dll_attach, which installs the detour. */
    if (!load_hook()) {
        fprintf(stderr, "could not load the hook library\n");
        return 1;
    }

    /* The same call, now routed through the detour: the original plus one. */
    printf("after %d\n", hookrt_call(10));
    return 0;
}
