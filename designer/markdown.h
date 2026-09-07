#pragma once
/* Markdown to RML, for Studio's built-in handbook.
 *
 * The bundle ships the documentation twice: as HTML for a browser, and as the
 * Markdown it was built from, which this file renders inside Studio. RmlUi is
 * not a browser and cannot open the HTML, and a second hand-written copy of
 * the prose would drift from the first within a release. So: one source, two
 * renderings.
 *
 * The subset implemented is exactly what `docs-site/src/*.md` uses, which was
 * measured rather than guessed — headings, fenced code, two- and three-column
 * tables, lists, images, rules, and inline code / bold / italic / links, plus
 * mdBook's `{{#include}}`. Anything outside that is passed through as text
 * rather than mangled, and `tools/check-docs.sh` is what keeps the docs inside
 * the subset.
 *
 * Two decisions worth stating:
 *
 * **Tables are laid out in pixels, not percentages.** The viewer knows its own
 * width, so the converter is told it. RmlUi collapses the whitespace between
 * inline-block cells into a real space, so percentage columns summing to 100
 * overflow and wrap every row; pixel columns and no whitespace between cells
 * cannot.
 *
 * **A table row whose first cell is a code span gets an id.** That is what
 * makes F1 work: `abs_int` in the editor scrolls the reference to `cmd-abs_int`
 * rather than to the top of a 483-line page. The reference is generated, so
 * this is the only place that knows the convention.
 */

#include <string>
#include <vector>
#include <sstream>
#include <fstream>
#include <cctype>

#include "highlight.h"
#include "theme.h"

namespace kiln::designer::md {

/// A rendered page: its title, its markup, and every anchor it defines.
struct Doc {
    std::string title;
    std::string rml;
    std::vector<std::string> anchors;
    /// Every fenced block on the page, in order. The copy button carries its
    /// index rather than the code itself: an attribute holding a program would
    /// have to survive RML escaping intact, and an index cannot be mangled.
    std::vector<std::string> code;
};

/// GitHub's heading-anchor rules, which mdBook also follows: lowercase, spaces
/// to dashes, everything else that is not alphanumeric dropped. Links in the
/// docs are written against these, so they have to match.
inline std::string slug(const std::string& s) {
    std::string o;
    for (char c : s) {
        if (std::isalnum(static_cast<unsigned char>(c))) o += static_cast<char>(std::tolower(c));
        // mdBook keeps underscores. Folding them to dashes would make Studio's
        // anchor for a heading named after a command disagree with the book's.
        else if (c == '_') o += '_';
        else if (c == ' ' || c == '-') o += '-';
    }
    while (!o.empty() && o.back() == '-') o.pop_back();
    return o;
}

/// Strip the markup from a span of Markdown, for search and for titles.
inline std::string plain(const std::string& s) {
    std::string o;
    for (size_t i = 0; i < s.size(); ++i) {
        // Not '_': every other command in this language has one in its name,
        // and dropping them turned the heading `file_read_text` into the
        // anchor `filereadtext`, which nothing links to. A genuinely
        // emphasised _word_ keeping its underscores in a snippet is the far
        // cheaper mistake.
        if (s[i] == '`' || s[i] == '*') continue;
        if (s[i] == '[') { continue; }
        if (s[i] == ']' && i + 1 < s.size() && s[i + 1] == '(') {
            while (i < s.size() && s[i] != ')') ++i;
            continue;
        }
        o += s[i];
    }
    return o;
}

/// Inline markup: `code`, **bold**, _italic_, [text](url).
///
/// Code spans are matched first and their contents are not re-scanned, so
/// `**` inside a code span stays literal — which matters, because the language
/// reference is full of operators.
inline std::string inlines(const std::string& s) {
    std::ostringstream o;
    size_t i = 0;
    auto emit = [&](const std::string& t) { o << escape_rml(t); };
    while (i < s.size()) {
        // `code`
        if (s[i] == '`') {
            size_t e = s.find('`', i + 1);
            if (e != std::string::npos) {
                o << "<span class='mdcode'>" << escape_code(s.substr(i + 1, e - i - 1)) << "</span>";
                i = e + 1;
                continue;
            }
        }
        // [text](url)
        if (s[i] == '[') {
            size_t close = s.find(']', i);
            if (close != std::string::npos && close + 1 < s.size() && s[close + 1] == '(') {
                size_t end = s.find(')', close);
                if (end != std::string::npos) {
                    const std::string text = s.substr(i + 1, close - i - 1);
                    const std::string url = s.substr(close + 2, end - close - 2);
                    if (url.rfind("http", 0) == 0) {
                        o << "<span class='mdlink' oe-url='" << escape_rml(url) << "'>"
                          << inlines(text) << "</span>";
                    } else {
                        // ./page.md#anchor, page.md, or a bare #anchor.
                        std::string page = url, anchor;
                        const size_t h = page.find('#');
                        if (h != std::string::npos) { anchor = page.substr(h + 1); page = page.substr(0, h); }
                        if (page.rfind("./", 0) == 0) page = page.substr(2);
                        const size_t dot = page.rfind(".md");
                        if (dot != std::string::npos) page = page.substr(0, dot);
                        o << "<span class='mdlink' oe-help-page='" << escape_rml(page)
                          << "' oe-help-anchor='" << escape_rml(anchor) << "'>" << inlines(text) << "</span>";
                    }
                    i = end + 1;
                    continue;
                }
            }
        }
        // **bold**
        if (s.compare(i, 2, "**") == 0) {
            const size_t e = s.find("**", i + 2);
            if (e != std::string::npos) {
                o << "<span class='mdb'>" << inlines(s.substr(i + 2, e - i - 2)) << "</span>";
                i = e + 2;
                continue;
            }
        }
        // _italic_ — only when it opens and closes a word, so that a name like
        // print_text is not read as emphasis around "text".
        if (s[i] == '_' && (i == 0 || s[i - 1] == ' ' || s[i - 1] == '(')) {
            const size_t e = s.find('_', i + 1);
            if (e != std::string::npos && (e + 1 == s.size() || s[e + 1] == ' ' ||
                                           s[e + 1] == '.' || s[e + 1] == ',' || s[e + 1] == ')')) {
                o << "<span class='mdi'>" << inlines(s.substr(i + 1, e - i - 1)) << "</span>";
                i = e + 1;
                continue;
            }
        }
        emit(std::string(1, s[i]));
        ++i;
    }
    return o.str();
}

/// Split a table row on its pipes, trimming each cell.
inline std::vector<std::string> cells(const std::string& line) {
    std::vector<std::string> out;
    size_t i = 0;
    if (!line.empty() && line[0] == '|') i = 1;
    std::string cur;
    for (; i < line.size(); ++i) {
        if (line[i] == '|') { out.push_back(cur); cur.clear(); }
        else cur += line[i];
    }
    if (!cur.empty()) out.push_back(cur);
    for (auto& c : out) {
        while (!c.empty() && c.front() == ' ') c.erase(c.begin());
        while (!c.empty() && c.back() == ' ') c.pop_back();
    }
    if (!out.empty() && out.back().empty()) out.pop_back();
    return out;
}

inline bool is_separator_row(const std::string& line) {
    bool dash = false;
    for (char c : line) {
        if (c == '-') dash = true;
        else if (c != '|' && c != ' ' && c != ':') return false;
    }
    return dash;
}

/// Read a file whole, or "" if it is not there.
inline std::string read_file(const std::string& path) {
    std::ifstream f(path, std::ios::binary);
    if (!f) return "";
    std::ostringstream s;
    s << f.rdbuf();
    return s.str();
}

/// Expand mdBook's `{{#include path}}`, relative to the including page.
inline std::string expand_includes(const std::string& src, const std::string& dir) {
    std::string out;
    std::istringstream in(src);
    std::string line;
    while (std::getline(in, line)) {
        const size_t a = line.find("{{#include ");
        if (a != std::string::npos) {
            const size_t b = line.find("}}", a);
            if (b != std::string::npos) {
                std::string rel = line.substr(a + 11, b - a - 11);
                while (!rel.empty() && rel.back() == ' ') rel.pop_back();
                // The path as written first. Trying the flattened basename
                // first makes `{{#include ../../docs/editors.md}}` read
                // editors.md — the page doing the including — and the include
                // survives into the output unexpanded.
                std::string body = read_file(dir + "/" + rel);
                if (body.empty()) {
                    // The bundle flattens includes beside the page.
                    const size_t slash = rel.rfind('/');
                    if (slash != std::string::npos) body = read_file(dir + "/" + rel.substr(slash + 1));
                }
                // A file that includes itself would otherwise expand forever.
                if (body.find("{{#include ") != std::string::npos) body.clear();
                out += body;
                out += "\n";
                continue;
            }
        }
        out += line;
        out += "\n";
    }
    return out;
}

/// Render a page. `width` is the pixel width of the content column; `dir` is
/// the directory the page was read from, used for includes and images.
inline Doc render(const std::string& source, int width, const std::string& dir) {
    Doc doc;
    std::ostringstream o;
    const std::string expanded = expand_includes(source, dir);

    std::vector<std::string> lines;
    {
        std::istringstream in(expanded);
        std::string line;
        while (std::getline(in, line)) {
            if (!line.empty() && line.back() == '\r') line.pop_back();
            lines.push_back(line);
        }
    }

    std::string para;
    auto flush_para = [&]() {
        if (para.empty()) return;
        o << "<div class='mdp'>" << inlines(para) << "</div>";
        para.clear();
    };

    bool in_comment = false;
    for (size_t i = 0; i < lines.size(); ++i) {
        std::string line = lines[i];

        // HTML comments — the generated pages open with one.
        if (in_comment) {
            if (line.find("-->") != std::string::npos) in_comment = false;
            continue;
        }
        if (line.find("<!--") != std::string::npos) {
            if (line.find("-->") == std::string::npos) in_comment = true;
            continue;
        }

        // Fenced code.
        if (line.rfind("```", 0) == 0) {
            flush_para();
            const std::string lang = line.substr(3);
            const bool epl = lang.empty() || lang == "kiln";
            std::string raw;
            std::ostringstream body;
            for (++i; i < lines.size() && lines[i].rfind("```", 0) != 0; ++i) {
                body << "<div class='mdcodeline'>"
                     << (epl ? highlight_line(lines[i]) : escape_code(lines[i]))
                     << "</div>";
                raw += lines[i];
                raw += "\n";
            }
            // The button is in normal flow, in its own right-aligned row.
            // Absolutely positioning it inside the block would be neater and
            // wrong: RmlUi does not clip a positioned element against an
            // ancestor's overflow, so it would paint over the header as the
            // page scrolled.
            o << "<div class='mdpre'><div class='mdcopyrow'><span class='mdcopy' oe-help-copy='"
              << doc.code.size() << "'>copy</span></div>" << body.str() << "</div>";
            doc.code.push_back(raw);
            continue;
        }

        // Tables.
        if (!line.empty() && line[0] == '|' && i + 1 < lines.size() && is_separator_row(lines[i + 1])) {
            flush_para();
            const std::vector<std::string> head = cells(line);
            const int n = static_cast<int>(head.size());
            // Two shapes appear in the docs and only two: name/description and
            // name/parameters/returns. The first column carries the identifier,
            // so it gets the room it needs and the rest split what is left.
            std::vector<int> w;
            const int inner = width - 24;
            if (n == 2) w = {inner * 34 / 100, inner - inner * 34 / 100};
            else if (n == 3) w = {inner * 30 / 100, inner * 40 / 100, inner - inner * 30 / 100 - inner * 40 / 100};
            else { w.assign(n, inner / (n ? n : 1)); }

            auto row = [&](const std::vector<std::string>& cs, const char* cls, const std::string& id) {
                o << "<div class='mdrow " << cls << "'";
                if (!id.empty()) o << " id='" << id << "'";
                o << ">";
                for (int c = 0; c < n; ++c) {
                    // No whitespace between cells: RmlUi turns it into a space
                    // and the row wraps.
                    o << "<span class='mdcell' style='width:" << w[c] << "px'>"
                      << (c < static_cast<int>(cs.size()) ? inlines(cs[c]) : "") << "</span>";
                }
                o << "</div>";
            };

            row(head, "mdhead", "");
            i += 2;
            for (; i < lines.size() && !lines[i].empty() && lines[i][0] == '|'; ++i) {
                const std::vector<std::string> cs = cells(lines[i]);
                // A row naming a command or a component is a jump target.
                std::string id;
                if (!cs.empty() && cs[0].size() > 2 && cs[0].front() == '`' && cs[0].back() == '`') {
                    id = "cmd-" + cs[0].substr(1, cs[0].size() - 2);
                    doc.anchors.push_back(id);
                }
                row(cs, "", id);
            }
            --i;
            continue;
        }

        // Headings.
        if (!line.empty() && line[0] == '#') {
            flush_para();
            int level = 0;
            while (level < static_cast<int>(line.size()) && line[level] == '#') ++level;
            const std::string text = line.substr(level == 0 ? 0 : level + (line[level] == ' ' ? 1 : 0));
            const std::string id = slug(plain(text));
            doc.anchors.push_back(id);
            if (doc.title.empty() && level == 1) doc.title = plain(text);
            o << "<div class='mdh" << (level > 4 ? 4 : level) << "' id='" << escape_rml(id) << "'>"
              << inlines(text) << "</div>";
            continue;
        }

        // Images.
        if (line.rfind("![", 0) == 0) {
            flush_para();
            const size_t close = line.find("](");
            const size_t end = line.rfind(')');
            if (close != std::string::npos && end != std::string::npos) {
                std::string src = line.substr(close + 2, end - close - 2);
                if (src.rfind("./", 0) == 0) src = src.substr(2);
                o << "<img class='mdimg' src='" << escape_rml(dir + "/" + src) << "'/>";
                continue;
            }
        }

        // Horizontal rule.
        if (line == "---" || line == "***") { flush_para(); o << "<div class='mdrule'/>"; continue; }

        // Lists. Nesting is by indent, and two levels is all SUMMARY.md uses.
        size_t indent = 0;
        while (indent < line.size() && line[indent] == ' ') ++indent;
        const std::string body = line.substr(indent);
        if (body.rfind("- ", 0) == 0 || body.rfind("* ", 0) == 0) {
            flush_para();
            o << "<div class='mdli' style='margin-left:" << (12 + static_cast<int>(indent) * 8)
              << "px'><span class='mdbullet'>\xE2\x80\xA2</span><span class='mditem'>"
              << inlines(body.substr(2)) << "</span></div>";
            continue;
        }

        // Blank line ends a paragraph; anything else joins it.
        if (line.find_first_not_of(" \t") == std::string::npos) { flush_para(); continue; }
        if (!para.empty()) para += " ";
        para += body;
    }
    flush_para();

    doc.rml = o.str();
    return doc;
}

} // namespace kiln::designer::md
