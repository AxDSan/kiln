/* A tiny C library the FFI tests call across the `dll` boundary.
 *
 * Built to `libmathdll.so` (or `mathdll.dll` for the Windows case) beside the
 * program that calls it. Three shapes cover what a `dll` has to carry: a value
 * in and out (`add_ints`), a C string out (`banner`), and a pointer through
 * which the callee writes (`bump`). `times_ten` exists to be reached under a
 * different Kiln name with `as "times_ten"`.
 */
int add_ints(int a, int b) { return a + b; }

const char *banner(void) { return "Kiln <-> C"; }

void bump(int *p) { *p += 1; }

int times_ten(int x) { return x * 10; }

/* The remaining scalar shapes, so every marshalled type crosses the boundary:
 * a 64-bit value larger than an int, a double, and a truth returned as C's int
 * (which Kiln normalises to a 0/1 bool). */
long long add_bignums(long long a, long long b) { return a + b; }

double halve(double x) { return x / 2.0; }

int is_positive(int x) { return x > 0; } /* returns 1 or 0, read back as bool */
