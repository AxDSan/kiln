/* "process" library metadata (design-time only; compiled into the
 * introspection .so, never a shipped program — the same split as
 * core_libinfo.c). */
#include "kiln_abi.h"

void process_run(Kiln_Slot *, int32_t, Kiln_Slot *);
void process_run_capture(Kiln_Slot *, int32_t, Kiln_Slot *);
void process_start(Kiln_Slot *, int32_t, Kiln_Slot *);
void process_read_line(Kiln_Slot *, int32_t, Kiln_Slot *);
void process_at_end(Kiln_Slot *, int32_t, Kiln_Slot *);
void process_write_line(Kiln_Slot *, int32_t, Kiln_Slot *);
void process_is_running(Kiln_Slot *, int32_t, Kiln_Slot *);
void process_wait(Kiln_Slot *, int32_t, Kiln_Slot *);
void process_kill(Kiln_Slot *, int32_t, Kiln_Slot *);
void process_close(Kiln_Slot *, int32_t, Kiln_Slot *);
void process_close_all(Kiln_Slot *, int32_t, Kiln_Slot *);

static const int32_t P_T[]  = { KN_SDT_TEXT };
static const int32_t P_I[]  = { KN_SDT_INT };
static const int32_t P_IT[] = { KN_SDT_INT, KN_SDT_TEXT };

static const Kiln_CommandDesc PROCESS_COMMANDS[] = {
    { "process_run",         "process_run",         KN_SDT_INT,  1, P_T  },
    { "process_run_capture", "process_run_capture", KN_SDT_TEXT, 1, P_T  },
    { "process_start",       "process_start",       KN_SDT_INT,  1, P_T  },
    { "process_read_line",   "process_read_line",   KN_SDT_TEXT, 1, P_I  },
    { "process_at_end",      "process_at_end",      KN_SDT_BOOL, 1, P_I  },
    { "process_write_line",  "process_write_line",  KN_SDT_BOOL, 2, P_IT },
    { "process_is_running",  "process_is_running",  KN_SDT_BOOL, 1, P_I  },
    { "process_wait",        "process_wait",        KN_SDT_INT,  1, P_I  },
    { "process_kill",        "process_kill",        KN_SDT_BOOL, 1, P_I  },
    { "process_close",       "process_close",       KN_SDT_BOOL, 1, P_I  },
    { "process_close_all",   "process_close_all",   KN_SDT_INT,  0, 0    },
};

static const Kiln_LibInfo PROCESS_INFO = {
    KILN_ABI_VERSION,
    "process",
    "kiln-process-0000-0000-0000-000000000008",
    0, 1, 0,
    (int32_t)(sizeof(PROCESS_COMMANDS) / sizeof(PROCESS_COMMANDS[0])),
    PROCESS_COMMANDS,
    0, 0,
};

const Kiln_LibInfo *kiln_get_lib_info(void) {
    return &PROCESS_INFO;
}
