/* "json" library metadata (design-time only; compiled into the introspection
 * .so, never a shipped program — the same split as core_libinfo.c).
 *
 * The path grammar every command shares is documented at the top of
 * json_cmds.c, because the path IS the interface: with no record type, a
 * dotted path is how a program names a place inside a document. */
#include "kiln_abi.h"

void json_parse(Kiln_Slot *, int32_t, Kiln_Slot *);
void json_parse_file(Kiln_Slot *, int32_t, Kiln_Slot *);
void json_new_object(Kiln_Slot *, int32_t, Kiln_Slot *);
void json_new_array(Kiln_Slot *, int32_t, Kiln_Slot *);
void json_close(Kiln_Slot *, int32_t, Kiln_Slot *);
void json_close_all(Kiln_Slot *, int32_t, Kiln_Slot *);
void json_stringify(Kiln_Slot *, int32_t, Kiln_Slot *);
void json_stringify_pretty(Kiln_Slot *, int32_t, Kiln_Slot *);
void json_save(Kiln_Slot *, int32_t, Kiln_Slot *);
void json_type(Kiln_Slot *, int32_t, Kiln_Slot *);
void json_has(Kiln_Slot *, int32_t, Kiln_Slot *);
void json_count(Kiln_Slot *, int32_t, Kiln_Slot *);
void json_key_at(Kiln_Slot *, int32_t, Kiln_Slot *);
void json_get_text(Kiln_Slot *, int32_t, Kiln_Slot *);
void json_get_int(Kiln_Slot *, int32_t, Kiln_Slot *);
void json_get_double(Kiln_Slot *, int32_t, Kiln_Slot *);
void json_get_bool(Kiln_Slot *, int32_t, Kiln_Slot *);
void json_set_text(Kiln_Slot *, int32_t, Kiln_Slot *);
void json_set_int(Kiln_Slot *, int32_t, Kiln_Slot *);
void json_set_double(Kiln_Slot *, int32_t, Kiln_Slot *);
void json_set_bool(Kiln_Slot *, int32_t, Kiln_Slot *);
void json_set_null(Kiln_Slot *, int32_t, Kiln_Slot *);
void json_set_object(Kiln_Slot *, int32_t, Kiln_Slot *);
void json_set_array(Kiln_Slot *, int32_t, Kiln_Slot *);
void json_remove(Kiln_Slot *, int32_t, Kiln_Slot *);

static const int32_t P_T[]   = { KN_SDT_TEXT };
static const int32_t P_I[]   = { KN_SDT_INT };
static const int32_t P_IT[]  = { KN_SDT_INT, KN_SDT_TEXT };
static const int32_t P_ITT[] = { KN_SDT_INT, KN_SDT_TEXT, KN_SDT_TEXT };
static const int32_t P_ITI[] = { KN_SDT_INT, KN_SDT_TEXT, KN_SDT_INT };
static const int32_t P_ITD[] = { KN_SDT_INT, KN_SDT_TEXT, KN_SDT_DOUBLE };
static const int32_t P_ITB[] = { KN_SDT_INT, KN_SDT_TEXT, KN_SDT_BOOL };

static const Kiln_CommandDesc JSON_COMMANDS[] = {
    /* opening and closing a document */
    { "json_parse",             "json_parse",             KN_SDT_INT,    1, P_T   },
    { "json_parse_file",        "json_parse_file",        KN_SDT_INT,    1, P_T   },
    { "json_new_object",        "json_new_object",        KN_SDT_INT,    0, 0     },
    { "json_new_array",         "json_new_array",         KN_SDT_INT,    0, 0     },
    { "json_close",             "json_close",             KN_SDT_BOOL,   1, P_I   },
    { "json_close_all",         "json_close_all",         KN_SDT_INT,    0, 0     },

    /* writing it back out */
    { "json_stringify",         "json_stringify",         KN_SDT_TEXT,   1, P_I   },
    { "json_stringify_pretty",  "json_stringify_pretty",  KN_SDT_TEXT,   1, P_I   },
    { "json_save",              "json_save",              KN_SDT_BOOL,   2, P_IT  },

    /* asking about a place */
    { "json_type",              "json_type",              KN_SDT_TEXT,   2, P_IT  },
    { "json_has",               "json_has",               KN_SDT_BOOL,   2, P_IT  },
    { "json_count",             "json_count",             KN_SDT_INT,    2, P_IT  },
    { "json_key_at",            "json_key_at",            KN_SDT_TEXT,   3, P_ITI },

    /* reading a value */
    { "json_get_text",          "json_get_text",          KN_SDT_TEXT,   2, P_IT  },
    { "json_get_int",           "json_get_int",           KN_SDT_INT,    2, P_IT  },
    { "json_get_double",        "json_get_double",        KN_SDT_DOUBLE, 2, P_IT  },
    { "json_get_bool",          "json_get_bool",          KN_SDT_BOOL,   2, P_IT  },

    /* writing a value */
    { "json_set_text",          "json_set_text",          KN_SDT_BOOL,   3, P_ITT },
    { "json_set_int",           "json_set_int",           KN_SDT_BOOL,   3, P_ITI },
    { "json_set_double",        "json_set_double",        KN_SDT_BOOL,   3, P_ITD },
    { "json_set_bool",          "json_set_bool",          KN_SDT_BOOL,   3, P_ITB },
    { "json_set_null",          "json_set_null",          KN_SDT_BOOL,   2, P_IT  },
    { "json_set_object",        "json_set_object",        KN_SDT_BOOL,   2, P_IT  },
    { "json_set_array",         "json_set_array",         KN_SDT_BOOL,   2, P_IT  },
    { "json_remove",            "json_remove",            KN_SDT_BOOL,   2, P_IT  },
};

static const Kiln_LibInfo JSON_INFO = {
    KILN_ABI_VERSION,
    "json",
    "kiln-json-4f2b-8c17-9ae3-6a5d1c07b3f2",
    0, 1, 0,
    (int32_t)(sizeof(JSON_COMMANDS) / sizeof(JSON_COMMANDS[0])),
    JSON_COMMANDS,
    0, 0,
};

const Kiln_LibInfo *kiln_get_lib_info(void) {
    return &JSON_INFO;
}
