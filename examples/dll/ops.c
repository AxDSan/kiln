/* Five bit operations, one shape: (int, int) -> int.
 *
 * A uniform signature is what makes a table of these callable in a loop — the
 * program picks a slot and calls it, and every slot means the same prototype.
 * Each function is C's own operator, so an Kiln program that computes the
 * same thing with `band`, `bor`, `bxor`, `shl` and `ushr` has something real to
 * check itself against.
 *
 * The shifts go through `unsigned` because a signed left shift that runs off
 * the top, and a signed right shift, are C's business rather than the
 * machine's; the cast pins them to the wrap and the zero-fill that Kiln's
 * `shl` and `ushr` do.
 */
int op_and(int a, int b) { return a & b; }

int op_or(int a, int b) { return a | b; }

int op_xor(int a, int b) { return a ^ b; }

int op_shl(int a, int b) { return (int)((unsigned int)a << b); }

int op_ushr(int a, int b) { return (int)((unsigned int)a >> b); }
