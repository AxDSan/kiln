/* "random" library metadata (design-time only; compiled into the introspection
 * .so, never a shipped program — same split as core_libinfo.c).
 *
 * Pseudo-random numbers for games and sampling. NOT for anything security
 * sensitive: the sequence is reproducible by design. */
#include "kiln_abi.h"

void random_seed(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void random_seed_now(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void random_int(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void random_between(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void random_double(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void random_double_between(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void random_bool(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void random_chance(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void random_hex(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);

static const int32_t P_I[]  = { KN_SDT_INT };
static const int32_t P_II[] = { KN_SDT_INT, KN_SDT_INT };
static const int32_t P_DD[] = { KN_SDT_DOUBLE, KN_SDT_DOUBLE };

static const Kiln_CommandDesc RANDOM_COMMANDS[] = {
    { "random_seed",           "random_seed",           KN_SDT_NULL,   1, P_I  },
    { "random_seed_now",       "random_seed_now",       KN_SDT_INT,    0, 0    },
    { "random_int",            "random_int",            KN_SDT_INT,    1, P_I  },
    { "random_between",        "random_between",        KN_SDT_INT,    2, P_II },
    { "random_double",         "random_double",         KN_SDT_DOUBLE, 0, 0    },
    { "random_double_between", "random_double_between", KN_SDT_DOUBLE, 2, P_DD },
    { "random_bool",           "random_bool",           KN_SDT_BOOL,   0, 0    },
    { "random_chance",         "random_chance",         KN_SDT_BOOL,   1, P_I  },
    { "random_hex",            "random_hex",            KN_SDT_TEXT,   1, P_I  },
};

static const Kiln_LibInfo RANDOM_INFO = {
    KILN_ABI_VERSION,
    "random",
    "kiln-random-0000-0000-0000-000000000011",
    0, 1, 0,
    (int32_t)(sizeof(RANDOM_COMMANDS) / sizeof(RANDOM_COMMANDS[0])),
    RANDOM_COMMANDS,
    0, 0,          /* no visual components */
};

const Kiln_LibInfo *kiln_get_lib_info(void) {
    return &RANDOM_INFO;
}
