/* "file" library metadata (design-time only; compiled into the introspection
 * .so, never a shipped program — the same split as core_libinfo.c).
 *
 * Commands are referenced by symbol NAME, so this translation unit has no
 * dependency on the implementations and needs nothing but the ABI header. */
#include "kiln_abi.h"

/* --- implementations (in file_cmds.c) --------------------------------- */
#define D(sym) void sym(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
D(file_read_text)   D(file_write_text)  D(file_append_text) D(file_exists)
D(file_size)        D(file_delete)      D(file_copy)        D(file_move)
D(file_modified)    D(file_line_count)
D(file_read_bytes)  D(file_write_bytes) D(file_append_bytes)
D(file_open)        D(file_read_line)   D(file_at_end)      D(file_write_line)
D(file_close)       D(file_close_all)
D(dir_exists)       D(dir_create)       D(dir_delete)       D(dir_current)
D(dir_set_current)  D(dir_entry_count)  D(dir_entry)
D(path_join)        D(path_name)        D(path_parent)      D(path_extension)
D(path_absolute)
#undef D

static const int32_t P_T[]   = { KN_SDT_TEXT };
static const int32_t P_TT[]  = { KN_SDT_TEXT, KN_SDT_TEXT };
static const int32_t P_TI[]  = { KN_SDT_TEXT, KN_SDT_INT };
static const int32_t P_I[]   = { KN_SDT_INT };
static const int32_t P_IT[]  = { KN_SDT_INT, KN_SDT_TEXT };
static const int32_t P_TB[]  = { KN_SDT_TEXT, KN_SDT_BIN };

static const Kiln_CommandDesc FILE_COMMANDS[] = {
    /* --- one-shot, path-level: the documented surface ------------------ */
    { "file_read_text",   "file_read_text",   KN_SDT_TEXT,  1, P_T ,
      "Read a whole text file, or \"\" if it could not be read",
      "let notes: text = file_read_text(\"notes.txt\")\ncall print_text(notes)" },
    { "file_write_text",  "file_write_text",  KN_SDT_BOOL,  2, P_TT,
      "Write text to a file, replacing what was there; false on failure",
      "if file_write_text(\"notes.txt\", \"hello\") = false\n  call print_text(last_error_text())\nend" },
    { "file_append_text", "file_append_text", KN_SDT_BOOL,  2, P_TT,
      "Add text to the end of a file, creating it if absent; false on failure",
      "call file_append_text(\"log.txt\", \"started\")" },
    { "file_exists",      "file_exists",      KN_SDT_BOOL,  1, P_T ,
      "Whether a file exists and can be read",
      "if file_exists(\"notes.txt\")\n  call print_text(\"found it\")\nend" },
    { "file_size",        "file_size",        KN_SDT_INT64, 1, P_T ,
      "How many bytes a file holds, or -1 if it could not be measured",
      "call print_int64(file_size(\"notes.txt\"))" },
    { "file_delete",      "file_delete",      KN_SDT_BOOL,  1, P_T ,
      "Remove a file; false if it was not there or could not be removed",
      "call file_delete(\"scratch.txt\")" },
    { "file_copy",        "file_copy",        KN_SDT_BOOL,  2, P_TT,
      "Copy a file, replacing the destination; false on failure",
      "call file_copy(\"notes.txt\", \"notes.bak\")" },
    { "file_move",        "file_move",        KN_SDT_BOOL,  2, P_TT,
      "Move or rename a file; false on failure",
      "call file_move(\"notes.txt\", \"archive/notes.txt\")" },
    { "file_modified",    "file_modified",    KN_SDT_INT64, 1, P_T ,
      "When a file was last written, in Unix seconds, or -1 on failure",
      "call print_int64(file_modified(\"notes.txt\"))" },
    { "file_line_count",  "file_line_count",  KN_SDT_INT,   1, P_T ,
      "How many lines a text file holds, or -1 on failure",
      "call print_int(file_line_count(\"notes.txt\"))" },

    /* --- the same three, in bytes: what a PNG needs and text cannot do -- */
    { "file_read_bytes",   "file_read_bytes",   KN_SDT_BIN,  1, P_T ,
      "Read a whole file as raw bytes, which is what a picture or an archive needs",
      "let raw: bytes = file_read_bytes(\"logo.png\")\ncall print_int(bytes_count(raw))" },
    { "file_write_bytes",  "file_write_bytes",  KN_SDT_BOOL, 2, P_TB,
      "Write raw bytes to a file, replacing what was there; false on failure",
      "call file_write_bytes(\"copy.png\", file_read_bytes(\"logo.png\"))" },
    { "file_append_bytes", "file_append_bytes", KN_SDT_BOOL, 2, P_TB,
      "Add raw bytes to the end of a file; false on failure",
      "call file_append_bytes(\"out.bin\", bytes_from_text(\"tail\"))" },

    /* --- handles: the escape hatch for what does not fit in memory ----- */
    { "file_open",        "file_open",        KN_SDT_INT,   2, P_TT,
      "Open a file for streaming in mode \"read\", \"write\" or \"append\"; 0 on failure",
      "let h: int = file_open(\"big.txt\", \"read\")\ncall file_close(h)" },
    { "file_read_line",   "file_read_line",   KN_SDT_TEXT,  1, P_I ,
      "Read the next line from an open file, without its newline",
      "let h: int = file_open(\"big.txt\", \"read\")\ncall print_text(file_read_line(h))\ncall file_close(h)" },
    { "file_at_end",      "file_at_end",      KN_SDT_BOOL,  1, P_I ,
      "Whether an open file has no more lines, which is the predicate a blank line needs",
      "let h: int = file_open(\"big.txt\", \"read\")\nwhile file_at_end(h) = false\n  call print_text(file_read_line(h))\nend\ncall file_close(h)" },
    { "file_write_line",  "file_write_line",  KN_SDT_BOOL,  2, P_IT,
      "Write one line to an open file, adding the newline; false on failure",
      "let h: int = file_open(\"out.txt\", \"write\")\ncall file_write_line(h, \"first\")\ncall file_close(h)" },
    { "file_close",       "file_close",       KN_SDT_BOOL,  1, P_I ,
      "Close an open file; false if the handle was already closed or never valid",
      "let h: int = file_open(\"out.txt\", \"write\")\ncall file_close(h)" },
    { "file_close_all",   "file_close_all",   KN_SDT_INT,   0, 0   ,
      "Close every file this program still has open, and say how many that was",
      "call print_int(file_close_all())" },

    /* --- directories --------------------------------------------------- */
    { "dir_exists",       "dir_exists",       KN_SDT_BOOL,  1, P_T ,
      "Whether a directory exists",
      "if dir_exists(\"reports\")\n  call print_text(\"ready\")\nend" },
    { "dir_create",       "dir_create",       KN_SDT_BOOL,  1, P_T ,
      "Create a directory and any parent it needs; false on failure",
      "call dir_create(\"reports/2026\")" },
    { "dir_delete",       "dir_delete",       KN_SDT_BOOL,  1, P_T ,
      "Remove an empty directory; false if it was not empty or not there",
      "call dir_delete(\"reports/2026\")" },
    { "dir_current",      "dir_current",      KN_SDT_TEXT,  0, 0   ,
      "The directory the program is running in",
      "call print_text(dir_current())" },
    { "dir_set_current",  "dir_set_current",  KN_SDT_BOOL,  1, P_T ,
      "Change the directory the program is running in; false on failure",
      "call dir_set_current(\"reports\")" },
    { "dir_entry_count",  "dir_entry_count",  KN_SDT_INT,   1, P_T ,
      "How many entries a directory holds, or -1 on failure. It re-reads the directory and snapshots it, which is what makes a loop over dir_entry stable",
      "call print_int(dir_entry_count(\"reports\"))" },
    { "dir_entry",        "dir_entry",        KN_SDT_TEXT,  2, P_TI,
      "One entry from the snapshot dir_entry_count took, counting from 1",
      "let n: int = dir_entry_count(\"reports\")\nfor i in 1..n\n  call print_text(dir_entry(\"reports\", i))\nend" },

    /* --- paths: pure text, and so infallible --------------------------- */
    { "path_join",        "path_join",        KN_SDT_TEXT,  2, P_TT,
      "Join two path pieces with the separator this platform uses",
      "call print_text(path_join(\"reports\", \"june.txt\"))" },
    { "path_name",        "path_name",        KN_SDT_TEXT,  1, P_T ,
      "The last piece of a path, which is the file name",
      "call print_text(path_name(\"reports/june.txt\"))" },
    { "path_parent",      "path_parent",      KN_SDT_TEXT,  1, P_T ,
      "Everything before the last piece of a path",
      "call print_text(path_parent(\"reports/june.txt\"))" },
    { "path_extension",   "path_extension",   KN_SDT_TEXT,  1, P_T ,
      "A file name's extension, without the dot, or \"\" if it has none",
      "call print_text(path_extension(\"reports/june.txt\"))" },
    { "path_absolute",    "path_absolute",    KN_SDT_TEXT,  1, P_T ,
      "A path resolved against the current directory",
      "call print_text(path_absolute(\"june.txt\"))" },
};

static const Kiln_LibInfo FILE_INFO = {
    KILN_ABI_VERSION,
    "file",
    "kiln-file-0000-0000-0000-000000000004",
    0, 1, 0,
    (int32_t)(sizeof(FILE_COMMANDS) / sizeof(FILE_COMMANDS[0])),
    FILE_COMMANDS,
    0, 0,
};

const Kiln_LibInfo *kiln_get_lib_info(void) {
    return &FILE_INFO;
}
