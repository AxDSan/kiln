/* Minimal syntax highlighter for the code editor pane.
 *
 * Emits RML spans with theme classes. Deliberately simple — a line-oriented
 * tokenizer, not a parser: the compiler owns real analysis, and the editor only
 * needs to make the shape of the code readable.
 */
#ifndef KILN_DESIGNER_HIGHLIGHT_H
#define KILN_DESIGNER_HIGHLIGHT_H

#include <cctype>
#include <string>
#include <vector>

namespace kiln::designer {

inline bool is_keyword(const std::string& w) {
    // `target`, `to`, `step`, `through` and the infix bitwise words are soft
    // keywords in the grammar — highlighted, but still usable as identifiers
    // elsewhere, which a line tokenizer cannot tell apart and does not try to.
    static const char* kw[] = {"module", "use",  "form", "sub",  "end",  "let",   "var",
                               "target", "sharedlib", "staticlib", "console", "gui",
                               "call",   "on",   "if",   "else", "while", "and",  "or",
                               "not",    "true", "false", "int", "int64", "double", "text", "bool",
                               "return", "for",  "break", "continue", "to", "step",
                               "through", "band", "bor", "bxor", "bnot", "shl", "shr", "ushr"};
    for (const char* k : kw) {
        if (w == k) return true;
    }
    return false;
}

inline std::string escape_rml(const std::string& s) {
    std::string o;
    for (char c : s) {
        if (c == '<') o += "&lt;";
        else if (c == '>') o += "&gt;";
        else if (c == '&') o += "&amp;";
        else o += c;
    }
    return o;
}

/// As `escape_rml`, but spaces become U+00A0 so indentation and inter-token
/// spacing survive. RmlUi collapses ordinary whitespace between inline spans,
/// which silently ran `call print_text` together as `callprint_text`.
inline std::string escape_code(const std::string& s) {
    std::string o;
    for (char c : s) {
        if (c == ' ') o += "\xC2\xA0";
        else if (c == '<') o += "&lt;";
        else if (c == '>') o += "&gt;";
        else if (c == '&') o += "&amp;";
        else o += c;
    }
    return o;
}

/// Highlight one line into RML markup.
inline std::string highlight_line(const std::string& line) {
    std::string out;
    size_t i = 0;
    while (i < line.size()) {
        const char c = line[i];
        if (c == '#') {                       // comment to end of line
            out += "<span class='c'>" + escape_code(line.substr(i)) + "</span>";
            break;
        }
        if (c == '"') {                        // string literal
            size_t j = i + 1;
            while (j < line.size() && line[j] != '"') {
                if (line[j] == '\\') j++;
                j++;
            }
            j = j < line.size() ? j + 1 : line.size();
            out += "<span class='s'>" + escape_code(line.substr(i, j - i)) + "</span>";
            i = j;
            continue;
        }
        if (std::isdigit((unsigned char)c)) {
            size_t j = i;
            // `0x...` and `0b...` first, or `0x8000_0000` paints as the number
            // `0` beside an identifier. Their digits may be grouped with `_`,
            // which a decimal literal may not be — the lexer draws the same
            // line.
            const bool bits = c == '0' && j + 1 < line.size() &&
                              (line[j + 1] == 'x' || line[j + 1] == 'X' ||
                               line[j + 1] == 'b' || line[j + 1] == 'B');
            if (bits) {
                j += 2;
                while (j < line.size() &&
                       (std::isalnum((unsigned char)line[j]) || line[j] == '_')) j++;
            } else {
                while (j < line.size() &&
                       (std::isdigit((unsigned char)line[j]) || line[j] == '.')) j++;
            }
            out += "<span class='n'>" + escape_code(line.substr(i, j - i)) + "</span>";
            i = j;
            continue;
        }
        if (std::isalpha((unsigned char)c) || c == '_') {
            size_t j = i;
            while (j < line.size() && (std::isalnum((unsigned char)line[j]) || line[j] == '_')) j++;
            const std::string word = line.substr(i, j - i);
            // A word followed by `(` is a command call; a word after `.` is a
            // property; otherwise keyword or plain identifier.
            const bool call = j < line.size() && line[j] == '(';
            const bool prop = i > 0 && line[i - 1] == '.';
            const char* cls = is_keyword(word) ? "k" : (call ? "m" : (prop ? "i" : nullptr));
            if (cls) {
                out += "<span class='" + std::string(cls) + "'>" + escape_code(word) + "</span>";
            } else {
                out += escape_code(word);
            }
            i = j;
            continue;
        }
        out += escape_code(std::string(1, c));
        i++;
    }
    return out;
}

} // namespace kiln::designer
#endif
