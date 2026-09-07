/* "text" library metadata (design-time only; compiled into the introspection
 * .so, never a shipped program — same split as core_libinfo.c).
 *
 * Core already owns text_eq, text_to_int and text_to_double inside this
 * prefix, and owns length/uppercase/lowercase/trim/substr/find/replace/
 * concat/repeat/reverse outside it. None of them appear here. */
#include "kiln_abi.h"

void text_starts_with(Kiln_Slot *, int32_t, Kiln_Slot *);
void text_ends_with(Kiln_Slot *, int32_t, Kiln_Slot *);
void text_contains(Kiln_Slot *, int32_t, Kiln_Slot *);
void text_equals_ignore_case(Kiln_Slot *, int32_t, Kiln_Slot *);
void text_index_of(Kiln_Slot *, int32_t, Kiln_Slot *);
void text_last_index_of(Kiln_Slot *, int32_t, Kiln_Slot *);
void text_count(Kiln_Slot *, int32_t, Kiln_Slot *);
void text_compare(Kiln_Slot *, int32_t, Kiln_Slot *);
void text_trim_start(Kiln_Slot *, int32_t, Kiln_Slot *);
void text_trim_end(Kiln_Slot *, int32_t, Kiln_Slot *);
void text_pad_left(Kiln_Slot *, int32_t, Kiln_Slot *);
void text_pad_right(Kiln_Slot *, int32_t, Kiln_Slot *);
void text_title_case(Kiln_Slot *, int32_t, Kiln_Slot *);
void text_insert(Kiln_Slot *, int32_t, Kiln_Slot *);
void text_remove(Kiln_Slot *, int32_t, Kiln_Slot *);
void text_char_at(Kiln_Slot *, int32_t, Kiln_Slot *);
void text_char_code(Kiln_Slot *, int32_t, Kiln_Slot *);
void text_from_code(Kiln_Slot *, int32_t, Kiln_Slot *);
void text_split_count(Kiln_Slot *, int32_t, Kiln_Slot *);
void text_split_at(Kiln_Slot *, int32_t, Kiln_Slot *);

static const int32_t P_T[]   = { KN_SDT_TEXT };
static const int32_t P_TT[]  = { KN_SDT_TEXT, KN_SDT_TEXT };
static const int32_t P_TI[]  = { KN_SDT_TEXT, KN_SDT_INT };
static const int32_t P_TIT[] = { KN_SDT_TEXT, KN_SDT_INT, KN_SDT_TEXT };
static const int32_t P_TII[] = { KN_SDT_TEXT, KN_SDT_INT, KN_SDT_INT };
static const int32_t P_TTI[] = { KN_SDT_TEXT, KN_SDT_TEXT, KN_SDT_INT };
static const int32_t P_I[]   = { KN_SDT_INT };

static const Kiln_CommandDesc TEXT_COMMANDS[] = {
    { "text_starts_with",        "text_starts_with",        KN_SDT_BOOL, 2, P_TT  },
    { "text_ends_with",          "text_ends_with",          KN_SDT_BOOL, 2, P_TT  },
    { "text_contains",           "text_contains",           KN_SDT_BOOL, 2, P_TT  },
    { "text_equals_ignore_case", "text_equals_ignore_case", KN_SDT_BOOL, 2, P_TT  },
    { "text_index_of",           "text_index_of",           KN_SDT_INT,  2, P_TT  },
    { "text_last_index_of",      "text_last_index_of",      KN_SDT_INT,  2, P_TT  },
    { "text_count",              "text_count",              KN_SDT_INT,  2, P_TT  },
    { "text_compare",            "text_compare",            KN_SDT_INT,  2, P_TT  },
    { "text_trim_start",         "text_trim_start",         KN_SDT_TEXT, 1, P_T   },
    { "text_trim_end",           "text_trim_end",           KN_SDT_TEXT, 1, P_T   },
    { "text_pad_left",           "text_pad_left",           KN_SDT_TEXT, 3, P_TIT },
    { "text_pad_right",          "text_pad_right",          KN_SDT_TEXT, 3, P_TIT },
    { "text_title_case",         "text_title_case",         KN_SDT_TEXT, 1, P_T   },
    { "text_insert",             "text_insert",             KN_SDT_TEXT, 3, P_TIT },
    { "text_remove",             "text_remove",             KN_SDT_TEXT, 3, P_TII },
    { "text_char_at",            "text_char_at",            KN_SDT_TEXT, 2, P_TI  },
    { "text_char_code",          "text_char_code",          KN_SDT_INT,  2, P_TI  },
    { "text_from_code",          "text_from_code",          KN_SDT_TEXT, 1, P_I   },
    { "text_split_count",        "text_split_count",        KN_SDT_INT,  2, P_TT  },
    { "text_split_at",           "text_split_at",           KN_SDT_TEXT, 3, P_TTI },
};

static const Kiln_LibInfo TEXT_INFO = {
    KILN_ABI_VERSION,
    "text",
    "kiln-text-0000-0000-0000-746578740001",
    0, 1, 0,
    (int32_t)(sizeof(TEXT_COMMANDS) / sizeof(TEXT_COMMANDS[0])),
    TEXT_COMMANDS,
    0, 0,
};

const Kiln_LibInfo *kiln_get_lib_info(void) {
    return &TEXT_INFO;
}
