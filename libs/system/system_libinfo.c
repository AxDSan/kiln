/* "system" library metadata (design-time only; compiled into the introspection
 * .so, never a shipped program — the same split as core_libinfo.c).
 *
 * Prefixes owned by this library: env_, os_, sys_. */
#include "kiln_abi.h"

#define SYS_CMD(n) void n(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv)
SYS_CMD(system_env_get);      SYS_CMD(system_env_has);      SYS_CMD(system_env_set);
SYS_CMD(system_env_unset);    SYS_CMD(system_env_count);    SYS_CMD(system_env_name_at);
SYS_CMD(system_os_name);      SYS_CMD(system_os_arch);      SYS_CMD(system_os_host_name);
SYS_CMD(system_os_user_name); SYS_CMD(system_os_home_dir);  SYS_CMD(system_os_temp_dir);
SYS_CMD(system_sys_arg_count);    SYS_CMD(system_sys_arg);
SYS_CMD(system_sys_program_path); SYS_CMD(system_sys_program_dir);
SYS_CMD(system_sys_process_id);   SYS_CMD(system_sys_tick_count);
SYS_CMD(system_sys_sleep_ms);     SYS_CMD(system_sys_quit);

static const int32_t P_T[]  = { KN_SDT_TEXT };
static const int32_t P_TT[] = { KN_SDT_TEXT, KN_SDT_TEXT };
static const int32_t P_I[]  = { KN_SDT_INT };

#define CMD(name, sym, ret, argc, args) { name, #sym, ret, argc, args }

static const Kiln_CommandDesc SYSTEM_COMMANDS[] = {
    /* the environment block */
    CMD("env_get",     system_env_get,     KN_SDT_TEXT, 1, P_T),
    CMD("env_has",     system_env_has,     KN_SDT_BOOL, 1, P_T),
    CMD("env_set",     system_env_set,     KN_SDT_BOOL, 2, P_TT),
    CMD("env_unset",   system_env_unset,   KN_SDT_BOOL, 1, P_T),
    CMD("env_count",   system_env_count,   KN_SDT_INT,  0, NULL),
    CMD("env_name_at", system_env_name_at, KN_SDT_TEXT, 1, P_I),
    /* the machine and the account */
    CMD("os_name",      system_os_name,      KN_SDT_TEXT, 0, NULL),
    CMD("os_arch",      system_os_arch,      KN_SDT_TEXT, 0, NULL),
    CMD("os_host_name", system_os_host_name, KN_SDT_TEXT, 0, NULL),
    CMD("os_user_name", system_os_user_name, KN_SDT_TEXT, 0, NULL),
    CMD("os_home_dir",  system_os_home_dir,  KN_SDT_TEXT, 0, NULL),
    CMD("os_temp_dir",  system_os_temp_dir,  KN_SDT_TEXT, 0, NULL),
    /* this process */
    CMD("sys_arg_count",    system_sys_arg_count,    KN_SDT_INT,   0, NULL),
    CMD("sys_arg",          system_sys_arg,          KN_SDT_TEXT,  1, P_I),
    CMD("sys_program_path", system_sys_program_path, KN_SDT_TEXT,  0, NULL),
    CMD("sys_program_dir",  system_sys_program_dir,  KN_SDT_TEXT,  0, NULL),
    CMD("sys_process_id",   system_sys_process_id,   KN_SDT_INT,   0, NULL),
    CMD("sys_tick_count",   system_sys_tick_count,   KN_SDT_INT64, 0, NULL),
    CMD("sys_sleep_ms",     system_sys_sleep_ms,     KN_SDT_NULL,  1, P_I),
    CMD("sys_quit",         system_sys_quit,         KN_SDT_NULL,  1, P_I),
};

static const Kiln_LibInfo SYSTEM_INFO = {
    KILN_ABI_VERSION,
    "system",
    "kiln-system-0000-0000-0000-73797374656d",
    0, 1, 0,
    (int32_t)(sizeof(SYSTEM_COMMANDS) / sizeof(SYSTEM_COMMANDS[0])),
    SYSTEM_COMMANDS,
    0, NULL,   /* the system library contributes no visual components */
};

const Kiln_LibInfo *kiln_get_lib_info(void) {
    return &SYSTEM_INFO;
}
