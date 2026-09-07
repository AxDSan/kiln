#include "model.h"

#include <sys/stat.h>
#include <algorithm>
#include <cctype>
#include <cstdio>
#include <fstream>
#include <sstream>

namespace kiln::designer {
namespace {

std::vector<std::string> split_words(const std::string& line, int max_parts) {
    std::vector<std::string> out;
    size_t i = 0;
    while (i < line.size() && (int)out.size() < max_parts - 1) {
        size_t sp = line.find(' ', i);
        if (sp == std::string::npos) break;
        out.push_back(line.substr(i, sp - i));
        i = sp + 1;
    }
    out.push_back(line.substr(i));
    return out;
}

std::string quote(const std::string& v) {
    std::string out = "\"";
    for (char c : v) {
        // A raw newline ends a text literal as far as the lexer is concerned,
        // so a multi-line value written verbatim is a file that will not parse.
        // The escapes here are exactly the ones ir/src/lexer.rs accepts.
        switch (c) {
            case '"':  out += "\\\""; break;
            case '\\': out += "\\\\"; break;
            case '\n': out += "\\n";  break;
            case '\t': out += "\\t";  break;
            case '\0': out += "\\0";  break;
            default:   out += c;
        }
    }
    return out + "\"";
}

/// Undo what `inspect` does to keep a `prop:` value on one line: `\\n`,
/// `\\0` and `\\\\`. The inverse is exact, which is what makes a memo's text
/// a value rather than a guess about which lines belong to it. Any other
/// backslash pair is not one `inspect` writes and is passed through untouched.
std::string unescape(const std::string& v) {
    std::string out;
    out.reserve(v.size());
    for (size_t i = 0; i < v.size(); i++) {
        if (v[i] == '\\' && i + 1 < v.size()) {
            const char n = v[i + 1];
            if (n == 'n') { out += '\n'; i++; continue; }
            if (n == '0') { out += '\0'; i++; continue; }
            if (n == '\\') { out += '\\'; i++; continue; }
        }
        out += v[i];
    }
    return out;
}

/// Write a property value as the declared type requires: quoted for text,
/// bare for numbers and booleans.
std::string render_value(const std::string& type_name, const std::string& property,
                         const std::string& value, const NeedsQuotes& needs_quotes) {
    return needs_quotes(type_name, property) ? quote(value) : value;
}

} // namespace

/// Is `path` a project rather than a source file — a directory, or `.kproj`?
static bool is_project(const std::string& path) {
    struct stat st;
    if (::stat(path.c_str(), &st) == 0 && S_ISDIR(st.st_mode)) return true;
    const std::string ext = ".kproj";
    return path.size() > ext.size() && path.compare(path.size() - ext.size(), ext.size(), ext) == 0;
}

bool load_model(const std::string& kiln_bin, const std::string& given, Model& out,
                std::string& error) {
    std::string path = given;
    std::string project;
    if (is_project(given)) {
        const std::string cmd = kiln_bin + " project " + given + " 2>&1";
        FILE* pipe = popen(cmd.c_str(), "r");
        if (!pipe) { error = "could not run kiln project"; return false; }
        std::string text, line;
        char buf[4096];
        while (fgets(buf, sizeof buf, pipe)) text += buf;
        const int rc = pclose(pipe);
        std::istringstream lines(text);
        path.clear();
        while (std::getline(lines, line)) {
            if (line.rfind("main: ", 0) == 0) path = line.substr(6);
            else if (line.rfind("project: ", 0) == 0) project = line.substr(9);
        }
        if (rc != 0 || path.empty()) {
            error = text.empty() ? "kiln project failed" : text;
            return false;
        }
    }

    const std::string cmd = kiln_bin + " inspect " + path + " 2>&1";
    FILE* pipe = popen(cmd.c_str(), "r");
    if (!pipe) { error = "could not run kiln inspect"; return false; }

    out = Model{};
    out.path = path;
    out.project = project;
    std::string text;
    char buf[4096];
    while (fgets(buf, sizeof buf, pipe)) text += buf;
    const int rc = pclose(pipe);

    std::istringstream lines(text);
    std::string line;
    Component* current = nullptr;   // component most recently declared
    while (std::getline(lines, line)) {
        if (line.rfind("module: ", 0) == 0) {
            out.module_name = line.substr(8);
        } else if (line.rfind("use: ", 0) == 0) {
            out.uses.push_back(line.substr(5));
        } else if (line.rfind("sub: ", 0) == 0) {
            out.subs.push_back(line.substr(5));
        } else if (line.rfind("form: ", 0) == 0) {
            auto parts = split_words(line.substr(6), 2);
            out.form_name = parts[0];
            out.form.id = parts[0];
            out.form.type_name = "form";
            if (parts.size() > 1) {
                std::sscanf(parts[1].c_str(), "span=%d..%d", &out.form_first_line,
                            &out.form_last_line);
            }
            current = &out.form;
        } else if (line.rfind("modcomponent: ", 0) == 0) {
            // A DISTINCT line kind, not `component:`, because that one is
            // consumed into form children — a module-level timer written back
            // inside the form is source the compiler rejects.
            auto parts = split_words(line.substr(14), 3);
            Component c;
            c.id = parts[0];
            c.type_name = parts.size() > 1 ? parts[1] : "";
            if (parts.size() > 2) {
                std::sscanf(parts[2].c_str(), "span=%d..%d", &c.first_line, &c.last_line);
            }
            out.module_components.push_back(c);
            current = &out.module_components.back();
        } else if (line.rfind("component: ", 0) == 0) {
            auto parts = split_words(line.substr(11), 2);
            Component c;
            c.id = parts[0];
            c.type_name = parts.size() > 1 ? parts[1] : "";
            out.children.push_back(c);
            current = &out.children.back();
        } else if (line.rfind("prop: ", 0) == 0) {
            auto parts = split_words(line.substr(6), 3);
            if (parts.size() == 3) {
                Component* target =
                    (parts[0] == out.form_name) ? &out.form : out.find(parts[0]);
                if (target) target->set_property(parts[1], unescape(parts[2]));
            }
        } else if (line.rfind("handler: ", 0) == 0) {
            auto parts = split_words(line.substr(9), 3);
            if (parts.size() == 3) {
                Component* target =
                    (parts[0] == out.form_name) ? &out.form : out.find(parts[0]);
                if (target) target->set_handler(parts[1], parts[2]);
            }
        }
        (void)current;
    }

    if (rc != 0) {
        error = text.empty() ? "kiln inspect failed" : text;
        return false;
    }
    // A module with no form is still a project: a console program or a library
    // has code to edit even though it has nothing to lay out. Refusing to open
    // one would mean the welcome screen offers templates the IDE cannot then
    // open. The canvas simply has nothing to draw.
    return true;
}

std::string emit_form(const Model& m, const NeedsQuotes& needs_quotes) {
    std::ostringstream o;
    o << "form " << m.form_name << "\n";
    for (const auto& p : m.form.properties) {
        o << "  " << p.first << " = " << render_value("form", p.first, p.second, needs_quotes)
          << "\n";
    }
    for (const auto& h : m.form.handlers) {
        o << "  on " << h.first << ": " << h.second << "\n";
    }
    for (const auto& c : m.children) {
        o << "\n  " << c.type_name << " " << c.id << "\n";
        for (const auto& p : c.properties) {
            o << "    " << p.first << " = "
              << render_value(c.type_name, p.first, p.second, needs_quotes) << "\n";
        }
        for (const auto& h : c.handlers) {
            o << "    on " << h.first << ": " << h.second << "\n";
        }
        o << "  end\n";
    }
    o << "end\n";
    return o.str();
}

std::string emit_module_component(const Component& c, const NeedsQuotes& needs_quotes) {
    std::ostringstream o;
    o << c.type_name << " " << c.id << "\n";
    for (const auto& p : c.properties) {
        o << "  " << p.first << " = "
          << render_value(c.type_name, p.first, p.second, needs_quotes) << "\n";
    }
    for (const auto& h : c.handlers) {
        o << "  on " << h.first << ": " << h.second << "\n";
    }
    o << "end\n";
    return o.str();
}

bool is_identifier(const std::string& id) {
    if (id.empty()) return false;
    auto start = [](char c) { return c == '_' || std::isalpha((unsigned char)c); };
    auto cont = [](char c) { return c == '_' || std::isalnum((unsigned char)c); };
    if (!start(id[0])) return false;
    for (char c : id) {
        if (!cont(c)) return false;
    }
    // The words ir/src/lexer.rs turns into tokens rather than identifiers. A
    // component called `end` would parse as the end of its own form.
    // `bnot` is here and the other bitwise words are not: the infix ones are
    // soft keywords the lexer still reads as names, while a prefix operator
    // cannot be, so `bnot` is a token wherever it appears.
    static const char* KEYWORDS[] = {"module", "sub",   "end",  "let",   "var",  "if",   "else",
                                     "while",  "for",   "break", "continue", "and", "or",  "not",
                                     "bnot",
                                     "true",   "false", "call", "use",   "form", "on",   "return"};
    for (const char* k : KEYWORDS) {
        if (id == k) return false;
    }
    return true;
}

bool rename_id(Model& m, const std::string& old_id, const std::string& new_id, std::string& error) {
    if (old_id == new_id) return true;
    if (!is_identifier(new_id)) {
        error = new_id.empty() ? "a name is required" : new_id + " is not a valid name";
        return false;
    }
    if (m.has_id(new_id)) { error = new_id + " is already taken"; return false; }
    if (m.has_sub(new_id)) { error = new_id + " is a subroutine"; return false; }
    Component* c = m.find(old_id);
    if (!c) { error = "nothing is called " + old_id; return false; }
    c->id = new_id;
    if (c == &m.form) m.form_name = new_id;
    // Renaming what was itself renamed since the last save is one rename of
    // the file — a name typed a letter at a time is a chain of them.
    if (!m.renames.empty() && m.renames.back().second == old_id) {
        m.renames.back().second = new_id;
        if (m.renames.back().first == new_id) m.renames.pop_back();
    } else {
        m.renames.emplace_back(old_id, new_id);
    }
    return true;
}

std::string rename_references(const std::string& line, const std::string& old_id,
                              const std::string& new_id) {
    auto word = [](char c) { return c == '_' || std::isalnum((unsigned char)c); };
    std::string out;
    out.reserve(line.size());
    bool in_string = false;
    for (size_t i = 0; i < line.size();) {
        const char c = line[i];
        if (in_string) {
            // A backslash pair is one unit, so an escaped quote does not end
            // the literal — the same escapes ir/src/lexer.rs reads.
            if (c == '\\' && i + 1 < line.size()) { out += line.substr(i, 2); i += 2; continue; }
            if (c == '"') in_string = false;
            out += c;
            i++;
            continue;
        }
        if (c == '"') { in_string = true; out += c; i++; continue; }
        if (c == '#') { out += line.substr(i); break; }   // a comment runs to the end
        if (word(c) && (i == 0 || !word(line[i - 1])) &&
            line.compare(i, old_id.size(), old_id) == 0 && i + old_id.size() < line.size() &&
            line[i + old_id.size()] == '.') {
            out += new_id;
            i += old_id.size();
            continue;
        }
        out += c;
        i++;
    }
    return out;
}

namespace {

int count_lines(const std::string& block) {
    int n = 0;
    for (char ch : block) {
        if (ch == '\n') n++;
    }
    return n;
}

/// One stretch of the original file that a regenerated block replaces.
/// `which` is -1 for the form and otherwise an index into module_components.
struct Region {
    int first = 0, last = 0, which = -1;
};

} // namespace

bool save_model(Model& m, const std::vector<std::string>& new_subs,
                const NeedsQuotes& needs_quotes, std::string& error) {
    std::ifstream in(m.path);
    if (!in) { error = "cannot read " + m.path; return false; }
    std::vector<std::string> lines;
    std::string line;
    while (std::getline(in, line)) lines.push_back(line);
    in.close();

    // Every stretch of the file the designer owns, in file order.
    std::vector<Region> regions;
    if (!m.form_name.empty()) {
        regions.push_back({m.form_first_line, m.form_last_line, -1});
    }
    for (size_t i = 0; i < m.module_components.size(); i++) {
        if (m.module_components[i].last_line > 0) {
            regions.push_back({m.module_components[i].first_line,
                               m.module_components[i].last_line, (int)i});
        }
    }
    std::sort(regions.begin(), regions.end(),
              [](const Region& a, const Region& b) { return a.first < b.first; });

    // The header: `module`, then any mix of `target` and `use` lines, which is
    // what the parser accepts before the first item. A kit the model uses that
    // the file does not yet name — a `tcpserver` dropped into a module that
    // only says `use ui` — gets its `use` line here, after the last header
    // line, so the declaration it makes compiles. Decided against the file
    // rather than a flag on the model: an undo that took the component and its
    // kit back out of the model then writes nothing.
    std::vector<std::string> missing_uses;
    int header_end = 0;                  // 0-based index of the line new uses go after; -1 = top
    {
        std::vector<std::string> in_file;
        int last_header = -1;
        for (size_t i = 0; i < lines.size(); i++) {
            std::string t = lines[i];
            t.erase(0, t.find_first_not_of(" \t"));
            if (t.empty() || t[0] == '#') continue;
            const bool is_module = t.rfind("module", 0) == 0 && (t.size() == 6 || std::isspace((unsigned char)t[6]));
            const bool is_target = t.rfind("target", 0) == 0 && (t.size() == 6 || std::isspace((unsigned char)t[6]));
            const bool is_use = t.rfind("use", 0) == 0 && (t.size() == 3 || std::isspace((unsigned char)t[3]));
            if (!is_module && !is_target && !is_use) break;
            last_header = (int)i;
            if (is_use) {
                std::istringstream w(t.substr(3));
                std::string name;
                w >> name;
                if (!name.empty()) in_file.push_back(name);
            }
        }
        header_end = last_header;
        for (const auto& u : m.uses) {
            if (std::find(in_file.begin(), in_file.end(), u) != in_file.end()) continue;
            if (std::find(missing_uses.begin(), missing_uses.end(), u) != missing_uses.end()) continue;
            missing_uses.push_back(u);
        }
    }

    // Nothing to splice and nothing to add. A module with no form is still a
    // project: a console program or a library is edited entirely as text, and
    // reporting a form error for it is both wrong and alarming.
    bool anything_new = !new_subs.empty() || !m.renames.empty() || !missing_uses.empty();
    for (const auto& c : m.module_components) {
        if (c.last_line == 0 && !c.removed) anything_new = true;
    }
    if (regions.empty() && !anything_new) return true;

    // A span that does not describe the file cannot be spliced into it safely,
    // and overlapping spans would write one block over another. Refusing is the
    // only answer that cannot lose code.
    int prev_end = 0;
    for (const auto& r : regions) {
        if (r.first < 1 || r.last > (int)lines.size() || r.first > r.last || r.first <= prev_end) {
            error = "component line spans are out of range or overlap; refusing to save";
            return false;
        }
        prev_end = r.last;
    }

    std::ostringstream out;
    int out_lines = 0;                   // written so far, so spans can be recomputed
    int cursor = 0;                      // 0-based index of the next original line to copy
    // The lines copied verbatim are the hand-written ones, and they are
    // where a renamed component is referred to. The blocks the designer
    // emits already carry the new names, so the renames apply only here —
    // oldest first, since a later one may rename what an earlier one made.
    // The missing `use` lines go out once, right after the header's last line
    // (or at the very top of a file with no header). They land inside the
    // first copy, before any block the designer owns, and the spans below are
    // recomputed from `out_lines`, so the form and every module component
    // simply move down with them.
    bool uses_written = false;
    auto write_uses = [&]() {
        if (uses_written) return;
        uses_written = true;
        for (const auto& u : missing_uses) {
            out << "use " << u << "\n";
            out_lines++;
        }
    };
    auto copy_through = [&](int upto) {  // exclusive, 0-based
        if (header_end < 0) write_uses();
        for (int i = cursor; i < upto; i++) {
            std::string line = lines[i];
            for (const auto& r : m.renames) line = rename_references(line, r.first, r.second);
            out << line << "\n";
            out_lines++;
            if (i == header_end) write_uses();
        }
        cursor = upto;
    };

    // Where each block actually lands. Recomputing is not tidiness: a save that
    // changes the form's line count moves everything below it, and the next
    // save in the same session would splice over whatever now sits at the old
    // lines — which is somebody's subroutine.
    int form_first = m.form_first_line, form_last = m.form_last_line;
    std::vector<std::pair<int, int>> spans(m.module_components.size(), {0, 0});

    for (const auto& r : regions) {
        copy_through(r.first - 1);
        // A component deleted in the tray splices to nothing, which is how its
        // lines leave the file without disturbing anything around them.
        const std::string block =
            r.which < 0 ? emit_form(m, needs_quotes)
            : m.module_components[(size_t)r.which].removed
                ? std::string()
                : emit_module_component(m.module_components[(size_t)r.which], needs_quotes);
        const int start = out_lines + 1;
        out << block;
        out_lines += count_lines(block);
        if (r.which < 0) { form_first = start; form_last = out_lines; }
        else spans[(size_t)r.which] = {start, out_lines};
        cursor = r.last;                 // step over the block being replaced
    }
    // Everything after the last region, verbatim — this is what protects
    // hand-written subroutine bodies from being clobbered by a designer save.
    copy_through((int)lines.size());
    write_uses();                        // a file too short to have reached the header line

    // Components dropped in this session go at the END of the file, never above
    // the form. A module-level declaration is legal anywhere at top level, and
    // appending is the one position that moves no existing line — so nothing
    // this save has already placed can go stale behind it.
    for (size_t i = 0; i < m.module_components.size(); i++) {
        if (m.module_components[i].last_line > 0 || m.module_components[i].removed) continue;
        out << "\n";
        out_lines++;
        const std::string block = emit_module_component(m.module_components[i], needs_quotes);
        const int start = out_lines + 1;
        out << block;
        out_lines += count_lines(block);
        spans[i] = {start, out_lines};
    }
    // Stubs for handlers the user just wired up.
    for (const auto& name : new_subs) {
        out << "\nsub " << name << "\n"
            << "  # TODO: written by the designer; add your code here.\n"
            << "  call print_text(\"" << name << "\")\n"
            << "end\n";
        out_lines += 5;
    }

    std::ofstream os(m.path, std::ios::trunc);
    if (!os) { error = "cannot write " + m.path; return false; }
    os << out.str();
    os.close();

    // Only now, with the bytes on disk, do the spans describe reality.
    m.form_first_line = form_first;
    m.form_last_line = form_last;
    for (size_t i = 0; i < m.module_components.size(); i++) {
        m.module_components[i].first_line = spans[i].first;
        m.module_components[i].last_line = spans[i].second;
    }
    // A removed component's lines are gone from the file, so it is gone.
    std::vector<Component> kept;
    for (const auto& c : m.module_components) {
        if (!c.removed) kept.push_back(c);
    }
    m.module_components = kept;
    // The file now says the new names everywhere; nothing is left to rename.
    m.renames.clear();
    return true;
}

} // namespace kiln::designer
