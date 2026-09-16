/* "encoding" library metadata (design-time only; compiled into the
 * introspection .so, never a shipped program — same split as core_libinfo.c).
 *
 * Four commands, covering what reading such a file needs: decode one that is
 * GBK, decode broken GBK without giving up on it, write text back in the
 * codepage it came from, and ask whether this build can do any of it — which a
 * program wants to know once at start-up rather than at the moment it
 * matters. */
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
      "Bytes raw = BytesNew(2);\n"
      "BytesSet(raw, 1, 214);\n"
      "BytesSet(raw, 2, 208);\n"
      "Console.WriteLine(EncodingDecode(raw, \"gbk\"));" },

    { "encoding_decode_lossy", "encoding_decode_lossy", KN_SDT_TEXT, 2, P_BT,
      "The same, but a byte that is not of the encoding becomes U+FFFD instead of failing the whole read",
      "Bytes raw = BytesNew(3);\n"
      "BytesSet(raw, 1, 65);\n"
      "BytesSet(raw, 2, 255);\n"
      "BytesSet(raw, 3, 66);\n"
      "Console.WriteLine(EncodingDecodeLossy(raw, \"gbk\"));" },

    { "encoding_encode", "encoding_encode", KN_SDT_BIN, 2, P_TT,
      "Write text back in a named encoding as a byte-set, for another program to read",
      "Console.WriteLine(BytesCount(EncodingEncode(\"中文\", \"gbk\")));" },

    { "encoding_known", "encoding_known", KN_SDT_BOOL, 1, P_T,
      "Whether this build can convert a named encoding, so a start-up check can say so before a program asks",
      "bool ok = EncodingKnown(\"gbk\");\n"
      "Console.WriteLine($\"gbk: {ok}\");" },
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
