// Splash and Welcome screens for Kiln Studio.
//
// Two separate documents shown before the IDE chrome exists, for two reasons.
// The splash has to be on screen *during* the expensive startup work — loading
// the runtime registry through `kiln inspect` — so it must render before
// that work begins, which means before the model exists. And the welcome screen
// has no project yet, so a half-initialised IDE behind it would have nothing
// coherent to draw.
//
// The template tiles are generated from `kiln templates` rather than from a
// list here: the CLI is the single reader of the templates directory, so adding
// a template adds a tile with no change to this file.
#ifndef KILN_DESIGNER_WELCOME_H
#define KILN_DESIGNER_WELCOME_H

#include <dirent.h>
#include <cstdio>
#include <cstdlib>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>
#include <algorithm>
#include <string>
#include <vector>

#include "portable.h"
#include "theme.h"

namespace kiln::welcome {

/// Strip the line ending `fgets` leaves on.
inline void chomp(std::string& line) {
    while (!line.empty() && (line.back() == '\n' || line.back() == '\r')) line.pop_back();
}

/// What the toolchain says it is — the first line of `kiln version`, e.g.
/// `kiln 0.9.0` — or "" when it cannot be asked. Shown on the welcome
/// screen so the product says somewhere what it is; before this it did not.
inline std::string version_string(const std::string& kiln_bin) {
    if (kiln_bin.empty()) return "";
    std::string text;
    kiln::sys::capture_output(kiln_bin + " version", false, text);
    std::string line = text.substr(0, text.find('\n'));
    chomp(line);
    return line;
}

/// Is this path a project — a `.kproj`, or a directory holding one?
inline bool is_project_path(const std::string& path) {
    struct stat st;
    if (::stat(path.c_str(), &st) == 0 && S_ISDIR(st.st_mode)) return true;
    return kiln::sys::has_ext(path, ".kproj");
}

/// The file Studio should open for `path`: the `main:` of a project, as
/// `kiln project` resolves it, or the path itself when it is already an
/// `.kiln`. Studio never reads a project file — the CLI is its only reader —
/// so this is one subprocess, and "" when the project cannot be read.
inline std::string resolve_open(const std::string& kiln_bin, const std::string& path) {
    if (!is_project_path(path)) return path;
    if (kiln_bin.empty()) return "";
    std::string text;
    kiln::sys::capture_output(kiln_bin + " project " + path, false, text);
    std::string main, line;
    std::istringstream lines(text);
    while (std::getline(lines, line)) {
        chomp(line);
        if (line.rfind("main: ", 0) == 0) main = line.substr(6);
    }
    return main;
}

/// One project template, as reported by `kiln templates`.
struct TemplateInfo {
    std::string id;
    std::string target;
    std::string name;
    std::string desc;
    std::string entry;
};

/// Run `kiln templates` and parse its line-based output.
///
/// Same shape as the designer's use of `kiln inspect`: the CLI reads the
/// templates directory, this only renders what it reports.
inline std::vector<TemplateInfo> load_templates(const std::string& kiln_bin) {
    std::vector<TemplateInfo> out;
    std::string text;
    kiln::sys::capture_output(kiln_bin + " templates", false, text);
    std::istringstream lines(text);

    auto find = [&out](const std::string& id) -> TemplateInfo& {
        for (auto& t : out) {
            if (t.id == id) return t;
        }
        out.push_back(TemplateInfo{id, "", id, "", "main.kiln"});
        return out.back();
    };

    std::string line;
    while (std::getline(lines, line)) {
        while (!line.empty() && (line.back() == '\n' || line.back() == '\r')) line.pop_back();
        const size_t colon = line.find(':');
        if (colon == std::string::npos) continue;
        const std::string key = line.substr(0, colon);
        std::string rest = line.substr(colon + 1);
        if (!rest.empty() && rest[0] == ' ') rest.erase(0, 1);
        const size_t sp = rest.find(' ');
        const std::string id = rest.substr(0, sp);
        const std::string value = sp == std::string::npos ? "" : rest.substr(sp + 1);
        if (id.empty()) continue;
        TemplateInfo& t = find(id);
        if (key == "template") t.target = value;
        else if (key == "name") t.name = value;
        else if (key == "desc") t.desc = value;
        else if (key == "entry") t.entry = value;
    }
    return out;
}

/// Shared styling for both screens: the same palette as the IDE, so startup
/// does not flash a different-looking product.
inline std::string base_styles() {
    using namespace kiln::designer::theme;
    std::string s;
    // RmlUi has no HTML-like default stylesheet: every element is `inline`
    // unless told otherwise. Without this the tiles flow into one another
    // instead of stacking, which is exactly what the first render looked like.
    s += "div{display:block}";
    s += "body{background-color:";
    s += CANVAS;
    s += ";color:";
    s += TEXT;
    s += ";font-size:14px}";
    s += "#mark{font-size:34px;font-weight:bold;color:";
    s += TEXT;
    s += "}";
    s += ".accent{color:";
    s += ACCENT;
    s += "}";
    s += "#tag{font-size:13px;color:";
    s += TEXT_MUTED;
    s += ";margin-top:6px}";
    // RmlUi ships no default scrollbar size, and an unstyled slider is laid
    // out over the whole of the element that owns it — where it swallows every
    // click that should have reached the content underneath. The IDE's
    // stylesheet has said so since the code editor could not be focused; the
    // welcome screen and the path browser load their own documents and never
    // got the same rules, so the browser listed files that could not be
    // opened. Sizing them is not cosmetic.
    s += "scrollbarvertical{width:10px;background-color:";
    s += CANVAS;
    s += "}";
    s += "scrollbarhorizontal{height:10px;background-color:";
    s += CANVAS;
    s += "}";
    s += "scrollbarvertical slidertrack,scrollbarhorizontal slidertrack{background-color:";
    s += CANVAS;
    s += "}";
    s += "scrollbarvertical sliderbar{width:10px;min-height:24px;border-radius:5px;"
         "background-color:#c9d1d9}";
    s += "scrollbarhorizontal sliderbar{height:10px;min-width:24px;border-radius:5px;"
         "background-color:#c9d1d9}";
    s += "scrollbarvertical sliderbar:hover,scrollbarhorizontal sliderbar:hover{"
         "background-color:";
    s += TEXT_MUTED;
    s += "}";
    // No stepper arrows: undersized ones are the other way a scrollbar becomes
    // a click trap.
    s += "sliderarrowdec,sliderarrowinc{width:0px;height:0px}";
    return s;
}

/// The window controls, drawn by Studio because the window has no frame of
/// its own: minimise, maximise, close, in that order, at the top right. One
/// definition for every screen, so the click path that reads `oe-win` and the
/// tests that press these by id find the same elements on all of them.
inline std::string window_controls_styles() {
    return ".wc{position:absolute;top:8px;width:16px;height:16px;border-radius:8px;"
           "cursor:pointer}.wc:hover{opacity:0.7}";
}
inline std::string window_controls_markup() {
    return "<div class='wc' id='wc-min' oe-win='min' style='right:66px;background-color:#febc2e'/>"
           "<div class='wc' id='wc-max' oe-win='max' style='right:42px;background-color:#28c840'/>"
           "<div class='wc' id='wc-close' oe-win='close' style='right:18px;background-color:#ff5f57'/>";
}

/// The splash: shown while the runtime registry loads.
inline std::string splash_markup(const std::string& family, int w, int h,
                                 const std::string& wordmark) {
    std::string s = "<rml><head><style>";
    s += "body{width:" + std::to_string(w) + "px;height:" + std::to_string(h) +
         "px;font-family:'" + family + "'}";
    s += base_styles();
    s += "#box{position:absolute;left:0;top:0;width:" + std::to_string(w) + "px;height:" +
         std::to_string(h) + "px}";
    s += "#inner{position:absolute;left:0;top:" + std::to_string(h / 2 - 60) +
         "px;width:" + std::to_string(w) + "px;text-align:center}";
    // The wordmark, with the text mark as the fallback: a missing asset should
    // cost the logo, not the splash.
    s += "#logo{display:block;width:420px;margin-left:auto;margin-right:auto}";
    s += "</style></head><body><div id='box'><div id='inner'>";
    if (wordmark.empty()) {
        s += "<div id='mark'>Kiln <span class='accent'>Studio</span></div>";
    } else {
        s += "<img id='logo' src='" + wordmark + "'/>";
    }
    s += "<div id='tag'>Studio</div>";
    // Literal UTF-8, not entities: RmlUi decodes only a small set (lt, gt, amp,
    // quot, nbsp) and prints anything else verbatim, so `&middot;` would show
    // up on screen exactly as written.
    s += "<div id='tag'>RAD is the identity \u00b7 English-first \u00b7 Cross-platform</div>";
    s += "<div id='tag'>Loading the component library\u2026</div>";
    s += "</div></div></body></rml>";
    return s;
}

/// The welcome screen: pick a project kind, open a project or a file, or
/// reopen something recent.
///
/// `kiln_bin` is what lets the screen say which toolchain it is and turn a
/// recent PROJECT into the file to open: a recent entry is recorded as its
/// `project.kproj` when it has one, and `oe-open` must still carry an `.kiln`,
/// because the caller reads that path into the code editor. With no binary the
/// screen shows no version and hands the recent path over as recorded.
///
/// Attributes the caller's click listener reads:
///   oe-new='<template id>'   create from a template
///   oe-open='<path.kiln>'     open that file
///   oe-browse='project'|'file'   show the path browser (`browse_markup`)
inline std::string welcome_markup(const std::string& family, int w, int h,
                                  const std::vector<TemplateInfo>& templates,
                                  const std::vector<std::string>& recent,
                                  const std::string& wordmark,
                                  const std::string& kiln_bin) {
    using namespace kiln::designer::theme;
    const std::string version = version_string(kiln_bin);
    std::string s = "<rml><head><style>";
    s += "body{width:" + std::to_string(w) + "px;height:" + std::to_string(h) +
         "px;font-family:'" + family + "'}";
    s += base_styles();
    s += "#head{position:absolute;left:56px;top:48px;width:" + std::to_string(w - 112) + "px}";
    s += "#cols{position:absolute;left:56px;top:150px;width:" + std::to_string(w - 112) + "px}";
    s += ".colhead{font-size:11px;font-weight:bold;color:" + std::string(TEXT_MUTED) +
         ";margin-bottom:10px}";
    // Tiles are buttons: give them a hover state so they read as clickable.
    s += ".tile{background-color:" + std::string(PANEL) + ";border:1px " + BORDER +
         ";border-radius:8px;padding:14px 16px;margin-bottom:10px;cursor:pointer}";
    s += ".tile:hover{border:1px " + std::string(ACCENT) + ";background-color:#f2f6ff}";
    s += ".tname{font-size:15px;font-weight:bold;color:" + std::string(TEXT) + "}";
    s += ".tdesc{font-size:12px;color:" + std::string(TEXT_MUTED) + ";margin-top:4px}";
    s += ".ttarget{font-size:10px;color:" + std::string(ACCENT) + ";margin-top:6px}";
    s += ".recent{padding:8px 10px;border-radius:6px;cursor:pointer;font-size:13px;color:" +
         std::string(TEXT) + "}";
    s += ".recent:hover{background-color:" + std::string(PANEL) + "}";
    s += ".rpath{font-size:11px;color:" + std::string(TEXT_MUTED) + "}";
    s += ".empty{font-size:12px;color:" + std::string(TEXT_MUTED) + ";font-style:italic}";
    s += "#left{position:absolute;left:0;top:0;width:520px}";
    s += "#right{position:absolute;left:560px;top:0;width:420px}";
    s += ".open{display:inline-block;width:190px;margin-right:12px;margin-bottom:18px;"
         "background-color:" + std::string(PANEL) + ";border:1px " + BORDER +
         ";border-radius:8px;padding:12px 14px;cursor:pointer}";
    s += ".open:hover{border:1px " + std::string(ACCENT) + ";background-color:#f2f6ff}";
    s += "#foot{position:absolute;left:56px;top:" + std::to_string(h - 56) +
         "px;font-size:11px;color:" + TEXT_MUTED + "}";
    // Top right, beside the wordmark, rather than in the footer: the window
    // opens taller than some screens, and a line at the bottom edge is the
    // one line that would then not be seen.
    s += "#version{position:absolute;right:56px;top:56px;font-size:12px;color:" + std::string(TEXT_MUTED) + "}";
    // An explicit backdrop rather than relying on the body's own background:
    // the body paints its box, but a full-window rectangle is what actually
    // guarantees no unpainted region shows through as black.
    s += "#bg{position:absolute;left:0;top:0;width:" + std::to_string(w) + "px;height:" +
         std::to_string(h) + "px;background-color:" + CANVAS + "}";
    s += "#logo{display:block;width:300px}";
    s += window_controls_styles();
    s += "</style></head><body><div id='bg'/>" + window_controls_markup();

    s += "<div id='head'>";
    s += wordmark.empty() ? "<div id='mark'>Kiln <span class='accent'>Studio</span></div>"
                          : "<img id='logo' src='" + wordmark + "'/>";
    s += ""
         "<div id='tag'>Start something. Every template below compiles to a clean native "
         "binary.</div></div>";

    s += "<div id='cols'><div id='left'><div class='colhead'>NEW PROJECT</div>";
    for (const auto& t : templates) {
        s += "<div class='tile' oe-new='" + t.id + "'>";
        s += "<div class='tname'>" + t.name + "</div>";
        s += "<div class='tdesc'>" + t.desc + "</div>";
        s += "<div class='ttarget'>target: " + t.target + "</div>";
        s += "</div>";
    }
    if (templates.empty()) {
        s += "<div class='empty'>No templates found — is the templates/ directory present?</div>";
    }
    s += "</div>";

    s += "<div id='right'><div class='colhead'>OPEN</div>";
    s += "<div class='open' oe-browse='project'><div class='tname'>Open Project\u2026</div>"
         "<div class='tdesc'>A project.kproj, or its folder</div></div>";
    s += "<div class='open' oe-browse='file'><div class='tname'>Open File\u2026</div>"
         "<div class='tdesc'>Any .kiln</div></div>";

    s += "<div class='colhead'>RECENT</div>";
    for (const auto& r : recent) {
        // A project is shown by its folder, which is its name; a file by its
        // own name. Either way the path underneath says where it is.
        std::string shown = r;
        if (is_project_path(r)) {
            const size_t slash = shown.find_last_of('/');
            if (slash != std::string::npos) shown = shown.substr(0, slash);
        }
        const size_t slash = shown.find_last_of('/');
        const std::string base = slash == std::string::npos ? shown : shown.substr(slash + 1);
        const std::string open = kiln_bin.empty() ? r : resolve_open(kiln_bin, r);
        if (open.empty()) continue;   // a project that no longer reads
        s += "<div class='recent' oe-open='" + open + "'><div>" + base + "</div>";
        s += "<div class='rpath'>" + r + "</div></div>";
    }
    if (recent.empty()) {
        s += "<div class='empty'>Nothing yet. Pick a template to begin.</div>";
    }
    s += "</div></div>";

    s += "<div id='foot'>kiln-designer &lt;project.kiln&gt; opens a file directly</div>";
    if (!version.empty()) s += "<div id='version'>" + version + "</div>";
    s += "</body></rml>";
    return s;
}

/// The signature the IDE called before the screen could open anything: no
/// toolchain to ask, so no version line and recent entries handed over as
/// recorded. Kept so the caller compiles unchanged; it should move to the
/// overload above.
inline std::string welcome_markup(const std::string& family, int w, int h,
                                  const std::vector<TemplateInfo>& templates,
                                  const std::vector<std::string>& recent,
                                  const std::string& wordmark) {
    return welcome_markup(family, w, h, templates, recent, wordmark, "");
}

/// One row of the path browser.
struct DirEntry {
    std::string name;
    std::string path;
    bool is_dir = false;
    /// A `project.kproj` (browsing for a project) or an `.kiln` (for a file).
    bool is_openable = false;
};

/// What `dir` holds that a person choosing a `mode` — "project" or "file" —
/// could want: directories to descend into, and the one kind of file the mode
/// is for. Everything else is noise in a file dialog and is left out.
/// Directories first, then files, each sorted, and `..` on top unless at `/`.
inline std::vector<DirEntry> list_dir(const std::string& dir, const std::string& mode) {
    std::vector<DirEntry> dirs, files;
    DIR* d = ::opendir(dir.c_str());
    if (!d) return {};
    while (dirent* e = ::readdir(d)) {
        const std::string name = e->d_name;
        if (name == "." || name == ".." || name[0] == '.') continue;
        const std::string path = kiln::sys::is_root_dir(dir) ? dir + name : dir + "/" + name;
        struct stat st;
        if (::stat(path.c_str(), &st) != 0) continue;
        if (S_ISDIR(st.st_mode)) {
            dirs.push_back(DirEntry{name, path, true, false});
        } else if (mode == "project" ? name == "project.kproj"
                                     : kiln::sys::has_ext(name, ".kiln")) {
            files.push_back(DirEntry{name, path, false, true});
        }
    }
    ::closedir(d);
    auto by_name = [](const DirEntry& a, const DirEntry& b) { return a.name < b.name; };
    std::sort(dirs.begin(), dirs.end(), by_name);
    std::sort(files.begin(), files.end(), by_name);

    std::vector<DirEntry> out;
    if (!kiln::sys::is_root_dir(dir)) {
        out.push_back(DirEntry{"..", kiln::sys::parent_dir(dir), true, false});
    }
    out.insert(out.end(), dirs.begin(), dirs.end());
    out.insert(out.end(), files.begin(), files.end());
    return out;
}

/// The path browser: RmlUi has no native file dialog, so this is a document
/// listing `dir` for the caller to show in place of the welcome screen.
///
/// Attributes the caller's click listener reads:
///   oe-browse-dir='<path>'   relist that directory (`list_dir` + this again)
///   oe-open='<path.kiln>'     open that file — for a project entry it is
///                            already the project's `main`, resolved here
///   oe-browse-cancel=''      back to the welcome screen
inline std::string browse_markup(const std::string& family, int w, int h, const std::string& dir,
                                 const std::string& mode, const std::string& kiln_bin) {
    using namespace kiln::designer::theme;
    std::string s = "<rml><head><style>";
    s += "body{width:" + std::to_string(w) + "px;height:" + std::to_string(h) +
         "px;font-family:'" + family + "'}";
    s += base_styles();
    s += "#bg{position:absolute;left:0;top:0;width:" + std::to_string(w) + "px;height:" +
         std::to_string(h) + "px;background-color:" + CANVAS + "}";
    s += "#head{position:absolute;left:56px;top:40px;width:" + std::to_string(w - 112) + "px}";
    s += "#title{font-size:22px;font-weight:bold;color:" + std::string(TEXT) + "}";
    s += "#dir{font-size:12px;color:" + std::string(TEXT_MUTED) + ";margin-top:6px}";
    s += "#list{position:absolute;left:56px;top:120px;width:" + std::to_string(w - 112) +
         "px;height:" + std::to_string(h - 200) + "px;overflow-y:auto;background-color:" +
         PANEL + ";border:1px " + BORDER + ";border-radius:8px;padding:6px}";
    s += ".row{padding:7px 10px;border-radius:5px;cursor:pointer;font-size:13px;color:" +
         std::string(TEXT) + "}";
    s += ".row:hover{background-color:#f2f6ff}";
    s += ".dir{color:" + std::string(ACCENT) + "}";
    s += ".empty{font-size:12px;color:" + std::string(TEXT_MUTED) + ";font-style:italic;padding:8px}";
    s += "#cancel{position:absolute;left:56px;top:" + std::to_string(h - 60) +
         "px;padding:8px 16px;border-radius:6px;border:1px " + BORDER + ";cursor:pointer;font-size:13px}";
    s += "#cancel:hover{background-color:" + std::string(PANEL) + "}";
    s += window_controls_styles();
    s += "</style></head><body><div id='bg'/>" + window_controls_markup();
    s += "<div id='head'><div id='title'>";
    s += mode == "project" ? "Open Project" : "Open File";
    s += "</div><div id='dir'>" + dir + "</div></div>";

    s += "<div id='list'>";
    const auto entries = list_dir(dir, mode);
    for (const auto& e : entries) {
        if (e.is_dir) {
            s += "<div class='row dir' oe-browse-dir='" + e.path + "'>" + e.name + "/</div>";
            continue;
        }
        const std::string open = mode == "project" ? resolve_open(kiln_bin, e.path) : e.path;
        if (open.empty()) continue;   // a project file the CLI refuses is not offered
        s += "<div class='row' oe-open='" + open + "'>" + e.name + "</div>";
    }
    if (entries.empty()) s += "<div class='empty'>Nothing here that can be opened.</div>";
    s += "</div>";
    s += "<div id='cancel' oe-browse-cancel=''>Back</div>";
    s += "</body></rml>";
    return s;
}

/// Where the recent-projects list lives.
inline std::string recent_path() {
    // XDG_DATA_HOME first: it is how a test run keeps its scratch files out of
    // the list a person sees on their next real start. Then the platform's
    // per-user data directory — %APPDATA% on Windows.
    const std::string dir = kiln::sys::data_dir();
    return dir.empty() ? "" : dir + "/recent";
}

inline std::vector<std::string> load_recent(size_t limit = 8) {
    std::vector<std::string> out;
    const std::string path = recent_path();
    if (path.empty()) return out;
    FILE* f = std::fopen(path.c_str(), "r");
    if (!f) return out;
    char buf[1024];
    while (out.size() < limit && fgets(buf, sizeof buf, f)) {
        std::string line(buf);
        while (!line.empty() && (line.back() == '\n' || line.back() == '\r')) line.pop_back();
        // Skip files that have since been deleted or moved: offering a dead
        // path is worse than offering nothing.
        if (!line.empty() && ::access(line.c_str(), R_OK) == 0) out.push_back(line);
    }
    std::fclose(f);
    return out;
}

/// Record `path` as the most recent project, de-duplicated, newest first.
///
/// Stored absolute: a relative path is meaningless from the next session's
/// working directory, and the entry would silently vanish from the list.
///
/// A file with a `project.kproj` beside it is recorded as that project, not
/// as the file: the list is of things a person worked on, and what they
/// worked on was the project. A loose `.kiln` is still recorded as itself.
inline void remember_recent(const std::string& relative_or_absolute, size_t limit = 8) {
    std::string path = relative_or_absolute;
    if (const std::string real = kiln::sys::real_path(relative_or_absolute); !real.empty()) path = real;
    if (!is_project_path(path)) {
        const size_t slash = path.find_last_of('/');
        const std::string sibling =
            (slash == std::string::npos ? std::string() : path.substr(0, slash + 1)) + "project.kproj";
        if (::access(sibling.c_str(), R_OK) == 0) path = sibling;
    }
    const std::string file = recent_path();
    if (file.empty()) return;
    const size_t slash = file.find_last_of('/');
    if (slash != std::string::npos) kiln::sys::make_dirs(file.substr(0, slash));
    std::vector<std::string> keep{path};
    for (const auto& r : load_recent(limit * 2)) {
        if (r != path && keep.size() < limit) keep.push_back(r);
    }
    FILE* f = std::fopen(file.c_str(), "w");
    if (!f) return;
    for (const auto& r : keep) std::fprintf(f, "%s\n", r.c_str());
    std::fclose(f);
}

} // namespace kiln::welcome

#endif
