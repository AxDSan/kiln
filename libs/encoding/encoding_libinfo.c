/* "encoding" library metadata (design-time only; compiled into the
 * introspection .so, never a shipped program — same split as core_libinfo.c).
 *
 * Four commands, because the port needs exactly four things: read a table that
 * is GBK, read one that is broken GBK without giving up on it, write a name
 * back in the client's own codepage, and ask whether this build can do any of
 * it — which a program wants to know once at start-up rather than at the moment
 * a player is waiting for a realm list. */
#include "kiln_abi.h"

void encoding_decode(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void encoding_decode_lossy(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void encoding_encode(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void encoding_known(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);

static const int32_t P_BT[] = { KN_SDT_BIN, KN_SDT_TEXT };
static const int32_t P_TT[] = { KN_SDT_TEXT, KN_SDT_TEXT };
static const int32_t P_T[]  = { KN_SDT_TEXT };

static const Kiln_CommandDesc ENCODING_COMMANDS[] = {
    { "encoding_decode", "encoding_decode", KN_SDT_TEXT, 2, P_BT,
      "Turn a byte-set in a named encoding into UTF-8 text, or answer \"\" and name the byte that is not of it",
      "let raw: bytes = bytes_new(2)\ncall bytes_set(raw, 1, 214)\ncall bytes_set(raw, 2, 208)\ncall print_text(encoding_decode(raw, \"gbk\"))" },

    { "encoding_decode_lossy", "encoding_decode_lossy", KN_SDT_TEXT, 2, P_BT,
      "The same, but a byte that is not of the encoding becomes U+FFFD instead of failing the whole read",
      "let raw: bytes = bytes_new(3)\ncall bytes_set(raw, 1, 65)\ncall bytes_set(raw, 2, 255)\ncall bytes_set(raw, 3, 66)\ncall print_text(encoding_decode_lossy(raw, \"gbk\"))" },

    { "encoding_encode", "encoding_encode", KN_SDT_BIN, 2, P_TT,
      "Write text back in a named encoding as a byte-set, for the client to read",
      "call print_int(bytes_count(encoding_encode(\"\u4e2d\u6587\", \"gbk\")))" },

    { "encoding_known", "encoding_known", KN_SDT_BOOL, 1, P_T,
      "Whether this build can convert a named encoding, so a start-up check can say so before a player asks",
      "let ok: bool = encoding_known(\"gbk\")\ncall print_text(\"gbk: {ok}\")" },
};

static const Kiln_LibInfo ENCODING_INFO = {
    KILN_ABI_VERSION,
    "encoding",
    "kiln-encoding-0000-0000-0000-000000000004",
    0, 1, 0,
    (int32_t)(sizeof(ENCODING_COMMANDS) / sizeof(ENCODING_COMMANDS[0])),
    ENCODING_COMMANDS,
    0, NULL,
};

const Kiln_LibInfo *kiln_get_lib_info(void) {
    return &ENCODING_INFO;
}
