/* The loader entry for a shared library that declares `dll_attach`/`dll_detach`.
 *
 * A `target sharedlib` module normally has no start-up moment — a host loads it
 * and calls its exports when it chooses (that is why the backend exports
 * `<module>_init` for the host to call rather than running it implicitly). But a
 * library that is *injected* or `LoadLibrary`'d for effect has no host willing
 * to make that call: it must run the instant it is mapped. That is what a
 * platform loader entry is for, and this file is it.
 *
 * A module that defines `sub dll_attach` (and/or `sub dll_detach`) gets this TU
 * compiled with KN_DLLMAIN defined; every other build compiles it to nothing,
 * so the ordinary sharedlib path — and every program and archive — is byte for
 * byte what it was. The CLI (cli/src/main.rs) sets the defines only for a
 * sharedlib that declares a hook:
 *
 *   -DOE_DLLMAIN                the switch that turns this file on
 *   -DOE_MODULE_INIT=<name>     the module's `<module>_init`, so vars are ready
 *   -DOE_HAS_ATTACH             the module defined `sub dll_attach`
 *   -DOE_HAS_DETACH             the module defined `sub dll_detach`
 *
 * `<module>_init` runs BEFORE `dll_attach`, always — a hook that touches a
 * module variable needs that variable initialised, and nothing else will do it
 * in time. The subs are the plain-named exports the backend emits for a library
 * (`define void @dll_attach()`), so a bare `extern void` prototype reaches them.
 *
 * On Windows the entry is a real `DllMain`, which the mingw C runtime's
 * `DllMainCRTStartup` calls with the reason code; on Linux it is an ELF
 * constructor/destructor, which the dynamic loader runs at `dlopen`/unload.
 * Both fire under the OS loader (on Windows, holding the loader lock), so
 * `dll_attach` should keep its own work short and hand anything heavy to a
 * thread it spawns — a mechanism, documented in docs-site/src/interop.md, not a
 * restriction this file can enforce.
 */
#ifdef KN_DLLMAIN

/* The module variable initialiser the backend emits for every library, named
 * here through the macro the CLI fills with the concrete `<module>_init`. */
extern void KN_MODULE_INIT(void);

#ifdef KN_HAS_ATTACH
extern void dll_attach(void);
#endif
#ifdef KN_HAS_DETACH
extern void dll_detach(void);
#endif

/* Run the attach side once: module variables first, then the user hook. Shared
 * by both platforms so the order can never drift between them. */
static void kn_dll_on_attach(void) {
    KN_MODULE_INIT();
#ifdef KN_HAS_ATTACH
    dll_attach();
#endif
}

#ifdef KN_HAS_DETACH
static void kn_dll_on_detach(void) { dll_detach(); }
#endif

#ifdef _WIN32
#include <windows.h>

/* The Windows loader calls this through the CRT for every process and thread
 * event; only the two process-wide reasons matter to a hook. Returning TRUE on
 * attach lets the load succeed. */
BOOL WINAPI DllMain(HINSTANCE instance, DWORD reason, LPVOID reserved) {
    (void)instance;
    (void)reserved;
    switch (reason) {
    case DLL_PROCESS_ATTACH:
        kn_dll_on_attach();
        break;
    case DLL_PROCESS_DETACH:
#ifdef KN_HAS_DETACH
        kn_dll_on_detach();
#endif
        break;
    default:
        break;
    }
    return TRUE;
}

#else /* the ELF and Mach-O loaders run marked functions at load and unload */

__attribute__((constructor)) static void kn_dll_ctor(void) { kn_dll_on_attach(); }

#ifdef KN_HAS_DETACH
__attribute__((destructor)) static void kn_dll_dtor(void) { kn_dll_on_detach(); }
#endif

#endif /* _WIN32 */

#endif /* KN_DLLMAIN */
