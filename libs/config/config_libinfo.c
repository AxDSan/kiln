/* "config" library metadata (design-time only; compiled into the introspection
 * .so, never a shipped program — same split as core_libinfo.c). Commands are
 * referenced by SYMBOL name, so this table needs none of the implementations. */
#include "kiln_abi.h"

void config_open(Kiln_Slot *, int32_t, Kiln_Slot *);
void config_create(Kiln_Slot *, int32_t, Kiln_Slot *);
void config_close(Kiln_Slot *, int32_t, Kiln_Slot *);
void config_close_all(Kiln_Slot *, int32_t, Kiln_Slot *);
void config_path(Kiln_Slot *, int32_t, Kiln_Slot *);
void config_save(Kiln_Slot *, int32_t, Kiln_Slot *);
void config_get(Kiln_Slot *, int32_t, Kiln_Slot *);
void config_get_int(Kiln_Slot *, int32_t, Kiln_Slot *);
void config_get_double(Kiln_Slot *, int32_t, Kiln_Slot *);
void config_get_bool(Kiln_Slot *, int32_t, Kiln_Slot *);
void config_has(Kiln_Slot *, int32_t, Kiln_Slot *);
void config_has_section(Kiln_Slot *, int32_t, Kiln_Slot *);
void config_set(Kiln_Slot *, int32_t, Kiln_Slot *);
void config_set_int(Kiln_Slot *, int32_t, Kiln_Slot *);
void config_set_double(Kiln_Slot *, int32_t, Kiln_Slot *);
void config_set_bool(Kiln_Slot *, int32_t, Kiln_Slot *);
void config_remove(Kiln_Slot *, int32_t, Kiln_Slot *);
void config_remove_section(Kiln_Slot *, int32_t, Kiln_Slot *);
void config_section_count(Kiln_Slot *, int32_t, Kiln_Slot *);
void config_section_at(Kiln_Slot *, int32_t, Kiln_Slot *);
void config_key_count(Kiln_Slot *, int32_t, Kiln_Slot *);
void config_key_at(Kiln_Slot *, int32_t, Kiln_Slot *);

static const int32_t P_T[]    = { KN_SDT_TEXT };
static const int32_t P_I[]    = { KN_SDT_INT };
static const int32_t P_IT[]   = { KN_SDT_INT, KN_SDT_TEXT };
static const int32_t P_II[]   = { KN_SDT_INT, KN_SDT_INT };
static const int32_t P_ITT[]  = { KN_SDT_INT, KN_SDT_TEXT, KN_SDT_TEXT };
static const int32_t P_ITI[]  = { KN_SDT_INT, KN_SDT_TEXT, KN_SDT_INT };
static const int32_t P_ITTT[] = { KN_SDT_INT, KN_SDT_TEXT, KN_SDT_TEXT, KN_SDT_TEXT };
static const int32_t P_ITTI[] = { KN_SDT_INT, KN_SDT_TEXT, KN_SDT_TEXT, KN_SDT_INT };
static const int32_t P_ITTD[] = { KN_SDT_INT, KN_SDT_TEXT, KN_SDT_TEXT, KN_SDT_DOUBLE };
static const int32_t P_ITTB[] = { KN_SDT_INT, KN_SDT_TEXT, KN_SDT_TEXT, KN_SDT_BOOL };

static const Kiln_CommandDesc CONFIG_COMMANDS[] = {
    /* opening and closing */
    { "config_open",           "config_open",           KN_SDT_INT,    1, P_T    },
    { "config_create",         "config_create",         KN_SDT_INT,    1, P_T    },
    { "config_close",          "config_close",          KN_SDT_BOOL,   1, P_I    },
    { "config_close_all",      "config_close_all",      KN_SDT_INT,    0, 0      },
    { "config_save",           "config_save",           KN_SDT_BOOL,   1, P_I    },
    { "config_path",           "config_path",           KN_SDT_TEXT,   1, P_I    },
    /* reading */
    { "config_get",            "config_get",            KN_SDT_TEXT,   3, P_ITT  },
    { "config_get_int",        "config_get_int",        KN_SDT_INT,    4, P_ITTI },
    { "config_get_double",     "config_get_double",     KN_SDT_DOUBLE, 4, P_ITTD },
    { "config_get_bool",       "config_get_bool",       KN_SDT_BOOL,   4, P_ITTB },
    { "config_has",            "config_has",            KN_SDT_BOOL,   3, P_ITT  },
    { "config_has_section",    "config_has_section",    KN_SDT_BOOL,   2, P_IT   },
    /* writing */
    { "config_set",            "config_set",            KN_SDT_BOOL,   4, P_ITTT },
    { "config_set_int",        "config_set_int",        KN_SDT_BOOL,   4, P_ITTI },
    { "config_set_double",     "config_set_double",     KN_SDT_BOOL,   4, P_ITTD },
    { "config_set_bool",       "config_set_bool",       KN_SDT_BOOL,   4, P_ITTB },
    { "config_remove",         "config_remove",         KN_SDT_BOOL,   3, P_ITT  },
    { "config_remove_section", "config_remove_section", KN_SDT_BOOL,   2, P_IT   },
    /* collections: count + indexed accessor */
    { "config_section_count",  "config_section_count",  KN_SDT_INT,    1, P_I    },
    { "config_section_at",     "config_section_at",     KN_SDT_TEXT,   2, P_II   },
    { "config_key_count",      "config_key_count",      KN_SDT_INT,    2, P_IT   },
    { "config_key_at",         "config_key_at",         KN_SDT_TEXT,   3, P_ITI  },
};

static const Kiln_LibInfo CONFIG_INFO = {
    KILN_ABI_VERSION,
    "config",
    "kiln-config-0000-0000-0000-000000000005",
    0, 1, 0,
    (int32_t)(sizeof(CONFIG_COMMANDS) / sizeof(CONFIG_COMMANDS[0])),
    CONFIG_COMMANDS,
    0, 0,
};

const Kiln_LibInfo *kiln_get_lib_info(void) {
    return &CONFIG_INFO;
}
