/* "hello" library metadata (design-time only; compiled into the introspection
 * .so, never a shipped program — same split as core_libinfo.c). */
#include "kiln_abi.h"

void hello_greet(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);

static const int32_t P_T[] = { KN_SDT_TEXT };

static const Kiln_CommandDesc HELLO_COMMANDS[] = {
    /* The last two fields are the documentation: one sentence, and one
     * example. They are optional — a row that omits them still works, and
     * `kiln commands` simply says nothing about it — but writing them is
     * what puts the command in the reference with a runnable sample, on the
     * language server's hover, and under F1 in Studio. The example is checked:
     * `tools/check-docs.sh` compiles every one of them. */
    { "greet", "hello_greet", KN_SDT_TEXT, 1, P_T,
      "Greet someone by name",
      "call print_text(greet(\"world\"))" },
};

static const Kiln_LibInfo HELLO_INFO = {
    KILN_ABI_VERSION,
    "hello",
    "kiln-hello-0000-0000-0000-000000000002",
    0, 1, 0,
    (int32_t)(sizeof(HELLO_COMMANDS) / sizeof(HELLO_COMMANDS[0])),
    HELLO_COMMANDS,
};

const Kiln_LibInfo *kiln_get_lib_info(void) {
    return &HELLO_INFO;
}
