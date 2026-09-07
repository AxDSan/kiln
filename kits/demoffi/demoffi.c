/* demoffi — a tiny, portable C library a declaration kit ships to prove the
 * `.kdecl` bundle end to end. It is NOT compiled into a program that `use`s the
 * kit: it is a runtime library the `dll` declarations load by name, the same
 * way a real program reaches user32 or libc. A test builds it into
 * `libdemoffi.so` (or `demoffi.dll`) beside the program. Plain C, no Kiln
 * headers, so it builds identically for Linux and Windows. */

#ifdef _WIN32
#define DEMO_EXPORT __declspec(dllexport)
#else
#define DEMO_EXPORT
#endif

/* Matches `record DemoPoint is c` in demoffi.kdecl: two 32-bit ints, 8 bytes. */
typedef struct {
    int x;
    int y;
} DemoPoint;

/* A value in, a value out. */
DEMO_EXPORT int demoffi_add(int a, int b) {
    return a + b;
}

/* A C string returned, copied into a managed text on the Kiln side. */
DEMO_EXPORT const char *demoffi_greeting(void) {
    return "demoffi says hello";
}

/* A struct mutated through the pointer the caller hands in — the c-record
 * path. The change is visible back in Kiln on the next line. */
DEMO_EXPORT void demoffi_move(DemoPoint *p, int dx, int dy) {
    p->x += dx;
    p->y += dy;
}
