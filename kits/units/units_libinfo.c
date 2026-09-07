/* "units" kit metadata (design-time only; compiled into the introspection .so,
 * never a shipped program — the same split every library uses). This kit lives
 * outside libs/, so it is also the proof that resolution reaches a directory
 * the compiler was not built knowing about. */
#include "kiln_abi.h"

void units_c_to_f(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void units_f_to_c(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);

static const int32_t P_D[] = { KN_SDT_DOUBLE };

static const Kiln_CommandDesc UNITS_COMMANDS[] = {
    { "units_c_to_f", "units_c_to_f", KN_SDT_DOUBLE, 1, P_D },
    { "units_f_to_c", "units_f_to_c", KN_SDT_DOUBLE, 1, P_D },
};

static const Kiln_LibInfo UNITS_INFO = {
    KILN_ABI_VERSION,
    "units",
    "kiln-units-0000-0000-0000-000000000010",
    1, 0, 0,
    (int32_t)(sizeof(UNITS_COMMANDS) / sizeof(UNITS_COMMANDS[0])),
    UNITS_COMMANDS,
};

const Kiln_LibInfo *kiln_get_lib_info(void) {
    return &UNITS_INFO;
}
