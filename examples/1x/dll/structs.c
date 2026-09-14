/* The C side of the second wave of c-struct record tests: the shapes a real
 * Win32 header is full of — a struct nested by value, 16-bit `WORD` fields, a
 * fixed byte array, a `float`.
 *
 * Each type here is the C spelling of a `record ... is c` in `cstruct2.kiln`,
 * and each `*_sizeof` / `*_offset_*` helper reports what this C compiler
 * computes for it. The test compares those numbers to Kiln's own `size of`
 * and to what Kiln reads at its own offsets, so the layout is held to the C
 * compiler's rather than to a table written by hand. */
#include <stddef.h>
#include <stdint.h>
#include <string.h>

typedef struct {
    int x;
    int y;
} SPoint;

/* The shape of a Win32 `MSG`: a pointer, an int, two pointer-width values, an
 * int, and a `POINT` **by value** — the nesting this wave exists for. */
typedef struct {
    void *hwnd;
    int32_t message;
    int64_t wparam;
    int64_t lparam;
    int32_t time;
    SPoint pt;
} SMsg;

/* The shape a `WNDCLASSEX` has around its `WORD` members: 16-bit fields between
 * a 32-bit one and a pointer, so the padding is what a wrong width would move. */
typedef struct {
    int32_t style;
    uint16_t cls_extra;
    uint16_t wnd_extra;
    const char *name;
} SWndClass;

/* A `PAINTSTRUCT`-shaped fixed array member. */
typedef struct {
    int32_t n;
    uint8_t bytes[16];
} SBlob;

typedef struct {
    float value;
} SFloatBox;

/* Fill every field of a `MSG` through the pointer Kiln hands over, so the
 * Kiln side reading `m.pt.x` back proves the nested offset agrees. */
void structs_fill_msg(SMsg *m) {
    m->hwnd = (void *)(intptr_t)0x1234;
    m->message = 15;
    m->wparam = 1000000000000LL;
    m->lparam = -7;
    m->time = 42;
    m->pt.x = 300;
    m->pt.y = 400;
}

/* memset the array member alone, leaving `n` untouched: the Kiln side reads
 * every element back and checks `n` survived, which a wrong offset or a wrong
 * stride would break. */
void structs_paint_blob(SBlob *b, int value) {
    memset(b->bytes, value & 0xff, sizeof b->bytes);
}

/* Read the array member back the way C sees it, so a value Kiln wrote at
 * position `k` (counting from 1) is proved to land at C's index `k - 1`. */
int structs_blob_byte(const SBlob *b, int index0) {
    return (int)b->bytes[index0];
}

/* Takes a pointer to the NESTED struct alone — what a `POINT *` parameter is —
 * so passing `msg.pt` to a `dll` declared with a `SPoint` parameter is proved to
 * hand over the address of the nested member and not of the whole `MSG`. */
void structs_move_point(SPoint *p, int dx, int dy) {
    p->x += dx;
    p->y += dy;
}

void structs_set_float(SFloatBox *f, double value) { f->value = (float)value; }
double structs_get_float(const SFloatBox *f) { return (double)f->value; }

/* Fill the `WORD` fields, so Kiln reading them back unsigned is proved
 * against a C compiler that stored them as `uint16_t`. */
void structs_fill_wndclass(SWndClass *w) {
    w->style = 3;
    w->cls_extra = 65535;
    w->wnd_extra = 258;
    w->name = "window";
}

/* The reference numbers. */
long long structs_msg_sizeof(void) { return (long long)sizeof(SMsg); }
long long structs_msg_offset_pt(void) { return (long long)offsetof(SMsg, pt); }
long long structs_point_sizeof(void) { return (long long)sizeof(SPoint); }
long long structs_wndclass_sizeof(void) { return (long long)sizeof(SWndClass); }
long long structs_wndclass_offset_name(void) {
    return (long long)offsetof(SWndClass, name);
}
long long structs_blob_sizeof(void) { return (long long)sizeof(SBlob); }
long long structs_blob_offset_bytes(void) { return (long long)offsetof(SBlob, bytes); }
long long structs_floatbox_sizeof(void) { return (long long)sizeof(SFloatBox); }
