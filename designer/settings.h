/* Kiln Studio's settings: the schema, and the file it lives in.
 *
 * Two rules shape this file.
 *
 * **Every setting does something.** The menu bar's rule — "an entry that only
 * prints 'not implemented' is worse than no entry at all" — applies with more
 * force here, because a control that persists a value nothing reads looks
 * exactly like one that works. Each row below names the code it changes. A
 * setting whose mechanism does not exist yet is not listed greyed out; it is
 * not listed.
 *
 * **The schema is the single source.** The file format, the defaults, the
 * dialog's rows and its categories are all generated from `schema()`, so
 * adding a setting is one row here plus the code that reads it. There is no
 * second list to forget.
 *
 * The file is `key: value` lines in the user's data directory, the same shape
 * as `project.kproj`, `template.meta` and the recent list — one format for
 * everything a person might open in an editor. Unknown keys are preserved on
 * save: a settings file written by a newer Studio must survive being opened by
 * an older one.
 */
#ifndef KILN_DESIGNER_SETTINGS_H
#define KILN_DESIGNER_SETTINGS_H

#include <algorithm>
#include <cctype>
#include <cstdio>
#include <cstdlib>
#include <map>
#include <string>
#include <vector>

#include "portable.h"

namespace kiln::settings {

enum class Kind { Bool, Int, Choice, Text, Path, Shortcut };

/// One setting. `min`/`max` are meaningful for `Int` only, and they are not
/// decoration: `snap()` divides by the grid size and the editor divides by the
/// character width, so a zero typed into either is a crash rather than a bad
/// look. A row without a sane floor is a row that has not been finished.
struct Row {
    const char* key;
    const char* category;
    const char* label;
    Kind kind;
    const char* def;
    int min, max;
    /// Takes effect only on the next start. Said on the row, because a control
    /// that silently does nothing until relaunch is indistinguishable from one
    /// that does not work.
    bool restart;
    const char* hint;
    /// For `Choice`: the values, in order. Empty otherwise.
    std::vector<const char*> choices;
    /// Remembered state rather than a preference — the window geometry. It
    /// lives in the same file because it belongs to the same person and the
    /// same machine, but it is not a row anyone should be shown: nobody sets
    /// their window size by typing it.
    bool hidden = false;
};

/// The whole schema, in the order the dialog shows it.
inline const std::vector<Row>& schema() {
    static const std::vector<Row> rows = {
        {"appearance.theme", "Appearance", "Theme", Kind::Choice, "light", 0, 0, false,
         "Repaints the IDE straight away.", {"light", "dark"}},
        {"appearance.remember_window", "Appearance", "Remember the window size", Kind::Bool,
         "true", 0, 0, false, "Reopen at the size you left it.", {}},
        {"appearance.splash_ms", "Appearance", "Splash screen (ms)", Kind::Int, "1200", 0, 5000,
         false, "0 shows it only as long as loading takes.", {}},

        {"editor.font_size", "Editor", "Font size", Kind::Int, "13", 8, 32, true,
         "The code editor's text and its line height.", {}},
        {"editor.indent_size", "Editor", "Indent width", Kind::Int, "2", 1, 8, false,
         "What Tab inserts, and what Enter copies after a block opener.", {}},
        {"editor.scroll_lines", "Editor", "Lines per wheel notch", Kind::Int, "3", 1, 10, false,
         "", {}},
        {"editor.start_view", "Editor", "Open files in", Kind::Choice, "designer", 0, 0, false,
         "Which tab a project opens on.", {"designer", "code"}},

        {"designer.show_grid", "Designer", "Show the grid", Kind::Bool, "true", 0, 0, false,
         "The dots behind the form.", {}},
        {"designer.snap_to_grid", "Designer", "Snap to the grid", Kind::Bool, "true", 0, 0, false,
         "Alignment guides still pull to other components.", {}},
        {"designer.grid_size", "Designer", "Grid size (px)", Kind::Int, "10", 2, 64, true,
         "Also the distance an arrow key nudges with Shift.", {}},

        {"build.output_dir", "Build", "Put built binaries in", Kind::Path, "", 0, 0, false,
         "Where Build puts your program. Empty means the project's build/. Run always uses .kiln/run/ inside the project.", {}},
        {"build.release", "Build", "Build optimised and stripped", Kind::Bool, "false", 0, 0,
         false, "Passes --release. Slower to build, smaller to ship.", {}},

        {"startup.on_exit", "Files and startup", "On exit", Kind::Choice, "save", 0, 0, false,
         "`ask` is not offered: there is nowhere yet to ask.", {"save", "discard"}},
        {"startup.reopen_last", "Files and startup", "Reopen the last project", Kind::Bool,
         "false", 0, 0, true, "Skips the welcome screen when the file still exists.", {}},
        {"startup.recent_limit", "Files and startup", "Recent projects kept", Kind::Int, "8", 1,
         30, false, "", {}},

        {"toolchain.kiln", "Toolchain", "kiln binary", Kind::Path, "", 0, 0, true,
         "Empty uses the one beside Studio.", {}},

        // Keyboard shortcuts. The key is `keys.<action>`, the action being the
        // one a menu entry fires, so a binding and its menu entry cannot name
        // different things. A value may hold several combinations separated by
        // spaces; an empty one unbinds the action. The dialog records a binding
        // by pressing it, and `menus()` shows whatever is bound here.
        {"keys.new-project", "Keyboard shortcuts", "New project", Kind::Shortcut, "Ctrl+N", 0, 0, false, "", {}},
        {"keys.open-project", "Keyboard shortcuts", "Open project", Kind::Shortcut, "Ctrl+Shift+O", 0, 0, false, "", {}},
        {"keys.open-file", "Keyboard shortcuts", "Open file", Kind::Shortcut, "Ctrl+O", 0, 0, false, "", {}},
        {"keys.close-project", "Keyboard shortcuts", "Close project", Kind::Shortcut, "Ctrl+Shift+W", 0, 0, false, "", {}},
        {"keys.save", "Keyboard shortcuts", "Save", Kind::Shortcut, "Ctrl+S", 0, 0, false, "", {}},
        {"keys.undo", "Keyboard shortcuts", "Undo", Kind::Shortcut, "Ctrl+Z", 0, 0, false, "In the designer; the code editor keeps its own.", {}},
        {"keys.redo", "Keyboard shortcuts", "Redo", Kind::Shortcut, "Ctrl+Shift+Z Ctrl+Y", 0, 0, false, "In the designer; the code editor keeps its own.", {}},
        {"keys.copy", "Keyboard shortcuts", "Copy", Kind::Shortcut, "Ctrl+C", 0, 0, false, "Components, in the designer.", {}},
        {"keys.paste", "Keyboard shortcuts", "Paste", Kind::Shortcut, "Ctrl+V", 0, 0, false, "Components, in the designer.", {}},
        {"keys.delete", "Keyboard shortcuts", "Delete", Kind::Shortcut, "Delete", 0, 0, false, "Components, in the designer.", {}},
        {"keys.view-designer", "Keyboard shortcuts", "Show the designer", Kind::Shortcut, "Ctrl+1", 0, 0, false, "", {}},
        {"keys.view-code", "Keyboard shortcuts", "Show the code", Kind::Shortcut, "Ctrl+2", 0, 0, false, "", {}},
        {"keys.run", "Keyboard shortcuts", "Run", Kind::Shortcut, "Ctrl+F5", 0, 0, false, "", {}},
        {"keys.build", "Keyboard shortcuts", "Build binary", Kind::Shortcut, "Ctrl+B", 0, 0, false, "", {}},
        {"keys.stop", "Keyboard shortcuts", "Stop", Kind::Shortcut, "Ctrl+Shift+F5", 0, 0, false, "", {}},
        {"keys.debug", "Keyboard shortcuts", "Debug / continue", Kind::Shortcut, "F5", 0, 0, false, "Starts a session, or continues the one that is paused.", {}},
        {"keys.togglebp", "Keyboard shortcuts", "Toggle breakpoint", Kind::Shortcut, "F9", 0, 0, false, "", {}},
        {"keys.dbgstepover", "Keyboard shortcuts", "Step over", Kind::Shortcut, "F10", 0, 0, false, "", {}},
        {"keys.dbgstepin", "Keyboard shortcuts", "Step in", Kind::Shortcut, "F11", 0, 0, false, "", {}},
        {"keys.dbgstepout", "Keyboard shortcuts", "Step out", Kind::Shortcut, "Shift+F11", 0, 0, false, "", {}},
        {"keys.dbgstop", "Keyboard shortcuts", "Stop debugging", Kind::Shortcut, "Shift+F5", 0, 0, false, "", {}},
        {"keys.gotodef", "Keyboard shortcuts", "Go to definition", Kind::Shortcut, "F12", 0, 0, false, "", {}},
        {"keys.findrefs", "Keyboard shortcuts", "Find references", Kind::Shortcut, "Shift+F12", 0, 0, false, "", {}},
        {"keys.complete", "Keyboard shortcuts", "Complete the word", Kind::Shortcut, "Ctrl+Space", 0, 0, false, "", {}},
        {"keys.help", "Keyboard shortcuts", "Help for the word at the caret", Kind::Shortcut, "F1", 0, 0, false, "", {}},
        {"keys.helpsearch", "Keyboard shortcuts", "Search the handbook", Kind::Shortcut, "Shift+F1", 0, 0, false, "", {}},
        {"keys.settings", "Keyboard shortcuts", "Settings", Kind::Shortcut, "Ctrl+Comma", 0, 0, false, "", {}},

        // Not shown: state, not preference. See Row::hidden.
        {"window.width", "", "", Kind::Int, "1440", 480, 16384, false, "", {}, true},
        {"window.height", "", "", Kind::Int, "900", 360, 16384, false, "", {}, true},
    };
    return rows;
}

inline const Row* find(const std::string& key) {
    for (const auto& r : schema()) {
        if (key == r.key) return &r;
    }
    return nullptr;
}

/// Where the settings file lives — beside the recent list, and under
/// `XDG_DATA_HOME` first, which is what keeps a test run out of the file a
/// person sees on their next real start.
inline std::string path() {
    const std::string dir = kiln::sys::data_dir();
    return dir.empty() ? "" : dir + "/settings";
}

/// The values, and the keys we did not recognise.
///
/// `unknown` exists so that a file written by a newer Studio survives being
/// opened and saved by an older one: dropping a key we do not understand would
/// silently delete the newer version's settings.
struct Store {
    std::map<std::string, std::string> values;
    std::vector<std::string> unknown;
};

inline Store& store() {
    static Store s;
    return s;
}

inline std::string trim(std::string v) {
    const size_t a = v.find_first_not_of(" \t\r\n");
    if (a == std::string::npos) return "";
    const size_t b = v.find_last_not_of(" \t\r\n");
    return v.substr(a, b - a + 1);
}

/// Read the file. Missing, unreadable or empty all mean "every default", which
/// is why nothing here reports an error: a first start has no settings file and
/// that is not a problem to tell anyone about.
inline void load() {
    Store& s = store();
    s.values.clear();
    s.unknown.clear();
    const std::string file = path();
    if (file.empty()) return;
    FILE* f = std::fopen(file.c_str(), "r");
    if (!f) return;
    char buf[1024];
    while (std::fgets(buf, sizeof buf, f)) {
        std::string line(buf);
        if (trim(line).empty() || trim(line)[0] == '#') continue;
        const size_t colon = line.find(':');
        if (colon == std::string::npos) continue;
        const std::string key = trim(line.substr(0, colon));
        const std::string val = trim(line.substr(colon + 1));
        if (find(key)) {
            s.values[key] = val;
        } else {
            s.unknown.push_back(key + ": " + val);
        }
    }
    std::fclose(f);
}

/// Write the file, schema order first and unrecognised keys after.
///
/// Only values that differ from their default are written, so the file stays
/// readable and a default that changes in a later Studio reaches a user who
/// never touched that row.
inline void save() {
    const std::string file = path();
    if (file.empty()) return;
    const size_t slash = file.find_last_of('/');
    if (slash != std::string::npos) kiln::sys::make_dirs(file.substr(0, slash));
    FILE* f = std::fopen(file.c_str(), "w");
    if (!f) return;
    std::fprintf(f, "# Kiln Studio settings. Delete a line to return it to its default.\n");
    for (const auto& r : schema()) {
        auto it = store().values.find(r.key);
        if (it == store().values.end() || it->second == r.def) continue;
        std::fprintf(f, "%s: %s\n", r.key, it->second.c_str());
    }
    for (const auto& u : store().unknown) std::fprintf(f, "%s\n", u.c_str());
    std::fclose(f);
}

/// The value as written, or the schema's default. Never fails: a key not in the
/// schema is a programming error, and returning "" for it is quieter than a
/// crash in a settings dialog.
inline std::string text(const std::string& key) {
    auto it = store().values.find(key);
    if (it != store().values.end()) return it->second;
    const Row* r = find(key);
    return r ? r->def : "";
}

inline bool boolean(const std::string& key) { return text(key) == "true"; }

/// An int, clamped to the row's range.
///
/// Clamping here rather than at the point of use is deliberate: `grid_size`
/// reaches an integer division and `font_size` reaches another, so a 0 that
/// escaped this function would be a SIGFPE somewhere far from the field that
/// produced it. A hand-edited file is exactly as likely to hold one as a
/// half-typed text box.
inline int number(const std::string& key) {
    const Row* r = find(key);
    const int fallback = r ? std::atoi(r->def) : 0;
    const std::string v = text(key);
    if (v.empty()) return fallback;
    char* end = nullptr;
    const long n = std::strtol(v.c_str(), &end, 10);
    if (end == v.c_str()) return fallback;
    if (!r) return (int)n;
    return (int)std::max((long)r->min, std::min((long)r->max, n));
}

/// One key combination in its canonical spelling — `Ctrl+Alt+Shift+Key`, in that
/// order — or "" when it is not one.
///
/// A key with no Ctrl or Alt must be a function key or Delete. A plain letter or
/// a Shift+letter is typing, and binding one would make the code editor
/// untypeable, so it is refused here rather than discovered there.
inline std::string normalize_combo(const std::string& combo) {
    bool ctrl = false, alt = false, shift = false;
    std::string key;
    size_t start = 0;
    while (start <= combo.size()) {
        size_t plus = combo.find('+', start);
        // A trailing `+` names the plus key itself: `Ctrl++`.
        if (plus == combo.size() - 1 && plus == start) plus = std::string::npos;
        std::string part = trim(combo.substr(start, plus == std::string::npos ? std::string::npos
                                                                            : plus - start));
        std::string lower = part;
        for (char& c : lower) c = (char)std::tolower((unsigned char)c);
        if (lower == "ctrl" || lower == "control" || lower == "cmd") ctrl = true;
        else if (lower == "alt" || lower == "option") alt = true;
        else if (lower == "shift") shift = true;
        else if (!part.empty()) {
            if (!key.empty()) return "";
            if (part.size() == 1) {
                const char c = (char)std::toupper((unsigned char)part[0]);
                if (c == ',') key = "Comma";
                else if (c == '.') key = "Period";
                else if (c == '+' || c == '=') key = "Plus";
                else if (c == '-') key = "Minus";
                else if ((c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9')) key = std::string(1, c);
                else return "";
            } else if ((lower[0] == 'f') && lower.size() <= 3 &&
                       std::all_of(lower.begin() + 1, lower.end(), ::isdigit)) {
                const int n = std::atoi(lower.c_str() + 1);
                if (n < 1 || n > 12) return "";
                key = "F" + std::to_string(n);
            } else {
                static const char* named[] = {"Space", "Comma", "Period", "Plus", "Minus",
                                              "Delete", "Tab", "Enter", "Escape", "Backspace"};
                for (const char* n : named) {
                    std::string ln = n;
                    for (char& c : ln) c = (char)std::tolower((unsigned char)c);
                    if (lower == ln) key = n;
                }
                if (key.empty()) return "";
            }
        }
        if (plus == std::string::npos) break;
        start = plus + 1;
    }
    if (key.empty()) return "";
    const bool function = key[0] == 'F' && key.size() > 1 && std::isdigit((unsigned char)key[1]);
    if (!ctrl && !alt && !function && key != "Delete") return "";
    std::string out;
    if (ctrl) out += "Ctrl+";
    if (alt) out += "Alt+";
    if (shift) out += "Shift+";
    return out + key;
}

/// Every combination a shortcut value holds, canonical, invalid ones dropped.
inline std::vector<std::string> combos(const std::string& value) {
    std::vector<std::string> out;
    size_t i = 0;
    while (i < value.size()) {
        while (i < value.size() && value[i] == ' ') i++;
        size_t j = value.find(' ', i);
        if (j == std::string::npos) j = value.size();
        if (j > i) {
            const std::string c = normalize_combo(value.substr(i, j - i));
            if (!c.empty()) out.push_back(c);
        }
        i = j;
    }
    return out;
}

/// The action bound to a canonical combination, or "".
inline std::string action_for(const std::string& combo) {
    if (combo.empty()) return "";
    for (const auto& r : schema()) {
        if (r.kind != Kind::Shortcut) continue;
        for (const auto& c : combos(text(r.key))) {
            if (c == combo) return std::string(r.key).substr(5);
        }
    }
    return "";
}

/// What a menu shows for an action: its first binding, or "".
inline std::string shortcut_for(const std::string& action) {
    const Row* r = find("keys." + action);
    if (!r) return "";
    const auto c = combos(text(r->key));
    return c.empty() ? "" : c.front();
}

/// Record a value. A `Choice` that is not one of its choices, and an `Int`
/// outside its range, are refused rather than stored — the caller is a text
/// field, and a text field can produce anything.
inline bool set(const std::string& key, const std::string& value) {
    const Row* r = find(key);
    if (!r) return false;
    if (r->kind == Kind::Choice) {
        bool ok = false;
        for (const char* c : r->choices) ok = ok || value == c;
        if (!ok) return false;
    }
    if (r->kind == Kind::Bool && value != "true" && value != "false") return false;
    if (r->kind == Kind::Int) {
        if (value.empty()) return false;
        char* end = nullptr;
        const long n = std::strtol(value.c_str(), &end, 10);
        if (*end || n < r->min || n > r->max) return false;
    }
    if (r->kind == Kind::Shortcut) {
        // Stored canonical, so the file and the dialog say the same thing.
        std::string joined;
        for (const auto& c : combos(value)) joined += (joined.empty() ? "" : " ") + c;
        if (joined.empty() && !trim(value).empty()) return false;
        store().values[key] = joined;
        return true;
    }
    store().values[key] = value;
    return true;
}

/// Has this row been changed from its default? The dialog marks those, so a
/// user can see at a glance what they have done — VS Code's blue bar, which is
/// the one affordance every survey of a settings page agreed on.
inline bool modified(const std::string& key) {
    auto it = store().values.find(key);
    const Row* r = find(key);
    return r && it != store().values.end() && it->second != r->def;
}

inline void reset(const std::string& key) { store().values.erase(key); }

/// The categories, in schema order, without duplicates.
inline std::vector<std::string> categories() {
    std::vector<std::string> out;
    for (const auto& r : schema()) {
        if (r.hidden) continue;
        if (std::find(out.begin(), out.end(), r.category) == out.end()) out.push_back(r.category);
    }
    return out;
}

} // namespace kiln::settings

#endif
