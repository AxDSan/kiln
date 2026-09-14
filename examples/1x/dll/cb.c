/* A tiny C library that calls BACK into its caller through a function pointer —
 * the other half of `dll`. `dll` lets Kiln call C; this lets C call Kiln,
 * which is what every hook, thread entry and enumeration callback needs.
 *
 * Built to `libcb.so` (or `cb.dll` for the Windows case) beside the program
 * that passes it a subroutine's address. Three shapes cover the ground: a
 * function pointer that returns a value (`apply`), one called for its side
 * effect several times over (`each`), and one handed a C string (`greet`) to
 * prove a `text` parameter survives the crossing. `each` counts from 1,
 * matching how the program's own loop would, so the printed sequence reads the
 * obvious way.
 */
int apply(int (*fn)(int, int), int a, int b) {
    return fn(a, b);
}

void each(void (*fn)(int), int n) {
    for (int i = 1; i <= n; i++) {
        fn(i);
    }
}

/* A C string literal handed straight to the callback: the callee receives it as
 * a `text` it borrows for the call. */
void greet(void (*fn)(const char *)) {
    fn("from C");
}
