// A small JSON reader and string escaper, enough for LSP traffic.
//
// The project has no JSON dependency and this needs only what the language
// server actually sends back, so a focused parser is cheaper than adding a
// library to the C++ side of a three-language build.
//
// The escaper matters more than the parser. Every keystroke in the editor is
// sent as a JSON string, and one unescaped quote or backslash corrupts the
// frame silently — the server sees malformed JSON and simply stops answering,
// which looks like the editor going dead rather than like a bug here.
#ifndef KILN_DESIGNER_JSON_H
#define KILN_DESIGNER_JSON_H

#include <cstdio>
#include <map>
#include <string>
#include <vector>

namespace kiln::json {

struct Value;
using Object = std::map<std::string, Value>;
using Array = std::vector<Value>;

struct Value {
    enum class Kind { Null, Bool, Number, String, Array, Object } kind = Kind::Null;
    bool boolean = false;
    double number = 0;
    std::string string;
    Array array;
    Object object;

    bool is_null() const { return kind == Kind::Null; }

    /// Member lookup that never throws and never inserts: a missing field is a
    /// null value, so a malformed message degrades instead of crashing.
    const Value& operator[](const std::string& key) const {
        static const Value none;
        if (kind != Kind::Object) return none;
        auto it = object.find(key);
        return it == object.end() ? none : it->second;
    }
    const Value& at(size_t i) const {
        static const Value none;
        return (kind == Kind::Array && i < array.size()) ? array[i] : none;
    }
    std::string str(const std::string& fallback = "") const {
        return kind == Kind::String ? string : fallback;
    }
    int num(int fallback = 0) const { return kind == Kind::Number ? (int)number : fallback; }
    size_t size() const { return kind == Kind::Array ? array.size() : 0; }
};

/// Escape `s` as a JSON string body (without the surrounding quotes).
inline std::string escape(const std::string& s) {
    std::string out;
    out.reserve(s.size() + 16);
    for (unsigned char c : s) {
        switch (c) {
        case '"': out += "\\\""; break;
        case '\\': out += "\\\\"; break;
        case '\n': out += "\\n"; break;
        case '\r': out += "\\r"; break;
        case '\t': out += "\\t"; break;
        case '\b': out += "\\b"; break;
        case '\f': out += "\\f"; break;
        default:
            if (c < 0x20) {
                // Other control characters must be \u-escaped; anything >= 0x20
                // is passed through, which keeps UTF-8 bytes intact.
                char buf[8];
                std::snprintf(buf, sizeof buf, "\\u%04x", c);
                out += buf;
            } else {
                out += (char)c;
            }
        }
    }
    return out;
}

namespace detail {

inline void skip_ws(const std::string& s, size_t& i) {
    while (i < s.size() && (s[i] == ' ' || s[i] == '\t' || s[i] == '\n' || s[i] == '\r')) i++;
}

inline bool parse_value(const std::string& s, size_t& i, Value& out);

inline bool parse_string(const std::string& s, size_t& i, std::string& out) {
    if (i >= s.size() || s[i] != '"') return false;
    i++;
    out.clear();
    while (i < s.size() && s[i] != '"') {
        if (s[i] == '\\' && i + 1 < s.size()) {
            i++;
            switch (s[i]) {
            case 'n': out += '\n'; break;
            case 't': out += '\t'; break;
            case 'r': out += '\r'; break;
            case 'b': out += '\b'; break;
            case 'f': out += '\f'; break;
            case 'u': {
                // Decode the code point and re-encode as UTF-8. Only the basic
                // plane is handled; surrogate pairs pass through as U+FFFD,
                // which is honest and never produces invalid UTF-8.
                if (i + 4 >= s.size()) return false;
                unsigned cp = 0;
                for (int k = 1; k <= 4; k++) {
                    const char h = s[i + k];
                    cp <<= 4;
                    if (h >= '0' && h <= '9') cp |= (unsigned)(h - '0');
                    else if (h >= 'a' && h <= 'f') cp |= (unsigned)(h - 'a' + 10);
                    else if (h >= 'A' && h <= 'F') cp |= (unsigned)(h - 'A' + 10);
                    else return false;
                }
                i += 4;
                if (cp >= 0xD800 && cp <= 0xDFFF) cp = 0xFFFD;
                if (cp < 0x80) {
                    out += (char)cp;
                } else if (cp < 0x800) {
                    out += (char)(0xC0 | (cp >> 6));
                    out += (char)(0x80 | (cp & 0x3F));
                } else {
                    out += (char)(0xE0 | (cp >> 12));
                    out += (char)(0x80 | ((cp >> 6) & 0x3F));
                    out += (char)(0x80 | (cp & 0x3F));
                }
                break;
            }
            default: out += s[i];
            }
            i++;
        } else {
            out += s[i++];
        }
    }
    if (i >= s.size()) return false;
    i++; // closing quote
    return true;
}

inline bool parse_value(const std::string& s, size_t& i, Value& out) {
    skip_ws(s, i);
    if (i >= s.size()) return false;
    const char c = s[i];
    if (c == '{') {
        i++;
        out.kind = Value::Kind::Object;
        skip_ws(s, i);
        if (i < s.size() && s[i] == '}') { i++; return true; }
        while (i < s.size()) {
            skip_ws(s, i);
            std::string key;
            if (!parse_string(s, i, key)) return false;
            skip_ws(s, i);
            if (i >= s.size() || s[i] != ':') return false;
            i++;
            Value v;
            if (!parse_value(s, i, v)) return false;
            out.object[key] = std::move(v);
            skip_ws(s, i);
            if (i < s.size() && s[i] == ',') { i++; continue; }
            if (i < s.size() && s[i] == '}') { i++; return true; }
            return false;
        }
        return false;
    }
    if (c == '[') {
        i++;
        out.kind = Value::Kind::Array;
        skip_ws(s, i);
        if (i < s.size() && s[i] == ']') { i++; return true; }
        while (i < s.size()) {
            Value v;
            if (!parse_value(s, i, v)) return false;
            out.array.push_back(std::move(v));
            skip_ws(s, i);
            if (i < s.size() && s[i] == ',') { i++; continue; }
            if (i < s.size() && s[i] == ']') { i++; return true; }
            return false;
        }
        return false;
    }
    if (c == '"') {
        out.kind = Value::Kind::String;
        return parse_string(s, i, out.string);
    }
    if (s.compare(i, 4, "true") == 0) {
        out.kind = Value::Kind::Bool;
        out.boolean = true;
        i += 4;
        return true;
    }
    if (s.compare(i, 5, "false") == 0) {
        out.kind = Value::Kind::Bool;
        out.boolean = false;
        i += 5;
        return true;
    }
    if (s.compare(i, 4, "null") == 0) {
        out.kind = Value::Kind::Null;
        i += 4;
        return true;
    }
    // number
    const size_t start = i;
    if (i < s.size() && (s[i] == '-' || s[i] == '+')) i++;
    while (i < s.size() && (isdigit((unsigned char)s[i]) || s[i] == '.' || s[i] == 'e' ||
                            s[i] == 'E' || s[i] == '-' || s[i] == '+')) {
        i++;
    }
    if (i == start) return false;
    out.kind = Value::Kind::Number;
    out.number = std::strtod(s.substr(start, i - start).c_str(), nullptr);
    return true;
}

} // namespace detail

/// Parse `text`. Returns a null value if it is not valid JSON.
inline Value parse(const std::string& text) {
    size_t i = 0;
    Value v;
    if (!detail::parse_value(text, i, v)) return Value{};
    return v;
}

} // namespace kiln::json

#endif
