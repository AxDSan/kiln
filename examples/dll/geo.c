/* A tiny C library for the c-struct record tests: a real C struct passed by
 * pointer across the `dll` boundary and mutated in place.
 *
 * `Point` is the C side of an Kiln `record Point is c (x: int, y: int)` —
 * two `int`s, natural layout, `sizeof` 8. `move_point` takes a pointer to one
 * and shifts it, the plainest proof that Kiln handed C the address of a real
 * struct: C reads and writes the same bytes Kiln laid out.
 *
 * `Mixed` matches `record Mixed is c (a: byte, b: int, c: byte, d: int64)`.
 * The reference helpers report what this C compiler computes for its size and
 * field offsets, so the test can hold Kiln's own layout to clang's. */
#include <stddef.h>
#include <stdint.h>

typedef struct {
    int x;
    int y;
} Point;

void move_point(Point *p, int dx, int dy) {
    p->x += dx;
    p->y += dy;
}

typedef struct {
    uint8_t a;
    int32_t b;
    uint8_t c;
    int64_t d;
} Mixed;

/* A `BOOL`-style field: a C API writes truth as any non-zero int, not always 1,
 * so `raise_flag` sets 7 on purpose. Reading it back as a c-record `bool` must
 * normalise to true, the way a Win32 `BOOL` field would. */
typedef struct {
    int on;
} Flags;

void raise_flag(Flags *f) { f->on = 7; }

/* The reference numbers the test compares Kiln's `size of` / offsets to. */
long long geo_mixed_sizeof(void) { return (long long)sizeof(Mixed); }
long long geo_mixed_offset_a(void) { return (long long)offsetof(Mixed, a); }
long long geo_mixed_offset_b(void) { return (long long)offsetof(Mixed, b); }
long long geo_mixed_offset_c(void) { return (long long)offsetof(Mixed, c); }
long long geo_mixed_offset_d(void) { return (long long)offsetof(Mixed, d); }
