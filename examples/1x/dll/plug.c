/* A tiny plug-in, loaded at run time and called through its ADDRESS.
 *
 * Nothing here is linked against: the program that calls these functions never
 * names them to the linker. It opens this library while running, asks for an
 * address by string, and calls what comes back — which is the whole point of
 * `call through`. Three shapes cover it: a value in and out (`add`), a C string
 * out (`name`), and a pointer the callee writes through with nothing returned
 * (`bump`, the void case).
 */
int add(int a, int b) { return a + b; }

const char *name(void) { return "plug"; }

void bump(int *cell) { *cell += 1; }
