/* The `xml` library's parser — one pass, no recursion, no dependency.
 *
 * What it must survive is the client's own tables, so the shape of those files
 * is the spec: a GB2312 declaration over bytes that are really GBK, a comment
 * block carrying Chinese text, CRLF, 523-character lines, and 3,430
 * self-closing rows whose attributes are the actual data.  Three consequences
 * are designed in rather than worked around:
 *
 *  - **Bytes are never interpreted.** No byte is decoded, validated as UTF-8
 *    or re-encoded; names and values are runs of the caller's buffer.  A GBK
 *    document parses as correctly as an ASCII one, and a name that is Chinese
 *    is still that name when <see cref="xml_attr"/> hands it back.
 *  - **Nothing is copied.** A node stores where its name, attributes and text
 *    are, not what they say, so a 134 KB table costs one copy of itself and
 *    three array growths.
 *  - **Nothing recurses.** Open elements are a stack of ids on the heap, so a
 *    deeply nested UI layout cannot overflow the machine stack the way a
 *    recursive parser would.
 *
 * It is a reader, not a validator: a bare attribute, an unknown entity or a
 * missing DOCTYPE are all tolerated because the files in hand contain none of
 * them and refusing a document over a construct it does not use would only
 * cost a caller a real file.  What IS reported is anything that would silently
 * lose data — an unclosed element, an unclosed comment, a mismatched end tag,
 * text outside the root — each with the line it happened on.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "xml_internal.h"

/* --- growth ------------------------------------------------------------ */

/* Grows a pool to at least `need` entries.  On failure the old block is left
 * alive and untouched — the caller aborts the parse and xml_doc_free() releases
 * it — because realloc's own contract cannot be honoured any other way. */
static void *xml_grow(void *p, int32_t *cap, int32_t need, size_t elem) {
    if (need <= *cap) return p;
    int32_t ncap = *cap > 0 ? *cap : 32;
    while (ncap < need) ncap *= 2;
    if (ncap < 0) return NULL;
    return realloc(p, (size_t)ncap * elem);
}

/* --- failure ------------------------------------------------------------ */

static void xml_fail(XmlError *e, int32_t line, const char *msg) {
    if (e->msg[0]) return;              /* the first failure is the one kept  */
    e->line = line;
    snprintf(e->msg, sizeof e->msg, "%s", msg);
}

/* "unclosed element <Kryss>" — a name is a span, not a string, and a very long
 * one is cut rather than skipped: a truncated name is a better diagnostic than
 * a wrong one or a buffer overrun. */
static void xml_fail_name(XmlError *e, int32_t line, const char *before,
                          XmlSpan name, const char *after) {
    if (e->msg[0]) return;
    char tmp[48];
    int32_t n = name.len < (int32_t)sizeof tmp - 1 ? name.len : (int32_t)sizeof tmp - 1;
    if (n > 0) memcpy(tmp, name.p, (size_t)n);
    tmp[n > 0 ? n : 0] = '\0';
    e->line = line;
    snprintf(e->msg, sizeof e->msg, "%s%s%s", before, tmp, after);
}

/* --- building the pools ------------------------------------------------- */

/* Index 0 is the document and is allocated before anything else, so a node id
 * is an index, 0 is the document rather than "nothing", and a failure to
 * allocate has to answer something 0 cannot: -1.  Every caller treats a
 * negative id as out of memory and abandons the parse. */
static int32_t xml_new_node(XmlDoc *d, XmlSpan name, int32_t parent, int32_t line) {
    XmlNode *g = (XmlNode *)xml_grow(d->nodes, &d->ncap, d->nnodes + 1, sizeof *d->nodes);
    if (!g) return -1;
    d->nodes = g;
    int32_t id = d->nnodes++;
    XmlNode *n = &d->nodes[id];
    memset(n, 0, sizeof *n);
    n->name = name;
    n->parent = parent;
    n->line = line;
    return id;
}

static int32_t xml_add_attr(XmlDoc *d, int32_t id, XmlSpan name, XmlSpan value) {
    XmlAttr *g = (XmlAttr *)xml_grow(d->attrs, &d->acap, d->nattrs + 1, sizeof *d->attrs);
    if (!g) return 0;
    d->attrs = g;
    d->attrs[d->nattrs].name = name;
    d->attrs[d->nattrs].value = value;
    XmlNode *n = &d->nodes[id];
    if (n->nattr == 0) n->first_attr = d->nattrs;
    n->nattr++;
    d->nattrs++;
    return 1;
}

static int32_t xml_add_run(XmlDoc *d, int32_t id, XmlSpan run) {
    XmlSpan *g = (XmlSpan *)xml_grow(d->runs, &d->rcap, d->nruns + 1, sizeof *d->runs);
    if (!g) return 0;
    d->runs = g;
    d->runs[d->nruns] = run;
    XmlNode *n = &d->nodes[id];
    if (n->nrun == 0) n->first_run = d->nruns;
    n->nrun++;
    d->nruns++;
    return 1;
}

static void xml_link(XmlDoc *d, int32_t parent, int32_t child) {
    XmlNode *p = &d->nodes[parent];
    if (p->last_child) d->nodes[p->last_child].next_sibling = child;
    else p->first_child = child;
    p->last_child = child;
}

/* --- lexical helpers ---------------------------------------------------- */

/* Whitespace as XML defines it, which is the four ASCII ones and nothing else:
 * a document encoded in GBK has no other byte that may be skipped. */
static int32_t xml_is_space(char c) {
    return c == ' ' || c == '\t' || c == '\r' || c == '\n';
}

static int32_t xml_all_space(const char *p, int32_t n) {
    for (int32_t i = 0; i < n; i++) if (!xml_is_space(p[i])) return 0;
    return 1;
}

/* A name may hold any byte at or above 0x80, uninterpreted: in a GBK document
 * a Chinese element name is legal, and there is no way to tell a continuation
 * byte from a letter without decoding — which is precisely what this parser
 * refuses to do. */
static int32_t xml_is_name_start(unsigned char c) {
    return (c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z') || c == '_' || c == ':' || c >= 0x80;
}

static int32_t xml_is_name_char(unsigned char c) {
    return xml_is_name_start(c) || (c >= '0' && c <= '9') || c == '-' || c == '.';
}

static int32_t xml_span_eq(XmlSpan a, XmlSpan b) {
    return a.len == b.len && (a.len == 0 || memcmp(a.p, b.p, (size_t)a.len) == 0);
}

/* Consumes up to and including `end`, counting lines.  Answers 0 when `end`
 * never occurs, leaving the cursor at the end of the document. */
static int32_t xml_skip_to(const char *s, int32_t n, int32_t *i, int32_t *line, const char *end) {
    int32_t el = (int32_t)strlen(end);
    while (*i + el <= n) {
        if (memcmp(s + *i, end, (size_t)el) == 0) { *i += el; return 1; }
        if (s[*i] == '\n') (*line)++;
        (*i)++;
    }
    return 0;
}

/* --- the parse --------------------------------------------------------- */

int32_t xml_doc_build(const char *src, int32_t len, XmlDoc **out, XmlError *err) {
    if (len <= 0) { xml_fail(err, 1, "the document is empty"); return 0; }

    XmlDoc *d = (XmlDoc *)calloc(1, sizeof *d);
    if (!d) { xml_fail(err, 0, "out of memory"); return 0; }

    d->src = (char *)malloc((size_t)len + 1);
    if (!d->src) { free(d); xml_fail(err, 0, "out of memory"); return 0; }
    memcpy(d->src, src, (size_t)len);
    d->src[len] = '\0';
    d->srclen = len;

    /* The document node. */
    if (xml_new_node(d, (XmlSpan){ d->src, 0 }, 0, 1) < 0) {
        xml_doc_free(d);
        xml_fail(err, 0, "out of memory");
        return 0;
    }

    const char *s = d->src;
    int32_t n = d->srclen, i = 0, line = 1;

    /* A UTF-8 byte-order mark is skipped rather than parsed.  Left in, it is
     * text outside the root element and every UTF-8 file would be refused —
     * which is exactly the kind of strictness that makes a parser useless. */
    if (n >= 3 && (unsigned char)s[0] == 0xEF && (unsigned char)s[1] == 0xBB &&
        (unsigned char)s[2] == 0xBF) {
        i = 3;
    }

    int32_t *open = NULL;         /* element ids, outermost first             */
    int32_t depth = 0, cap = 0;

    while (i < n) {
        if (s[i] != '<') {
            /* Text.  It belongs to the innermost open element; at the top level
             * only whitespace is allowed, and anything else is a document that
             * is not one document. */
            int32_t start = i, start_line = line;
            while (i < n && s[i] != '<') {
                if (s[i] == '\n') line++;
                i++;
            }
            if (depth > 0) {
                if (!xml_add_run(d, open[depth - 1], (XmlSpan){ s + start, i - start })) {
                    xml_fail(err, start_line, "out of memory");
                    goto fail;
                }
            } else if (!xml_all_space(s + start, i - start)) {
                xml_fail(err, start_line, "text outside the root element");
                goto fail;
            }
            continue;
        }

        if (i + 1 >= n) { xml_fail(err, line, "a '<' with nothing after it"); goto fail; }
        char c = s[i + 1];

        /* --- <?xml ... ?>, and any other processing instruction ---------- */
        if (c == '?') {
            int32_t start_line = line;
            i += 2;
            if (!xml_skip_to(s, n, &i, &line, "?>")) {
                xml_fail(err, start_line, "a processing instruction is never closed");
                goto fail;
            }
            continue;
        }

        /* --- <!-- ... -->, <![CDATA[ ... ]]>, and <!DOCTYPE ...> --------- */
        if (c == '!') {
            int32_t start_line = line;

            if (i + 3 < n && memcmp(s + i + 2, "--", 2) == 0) {
                i += 4;
                if (!xml_skip_to(s, n, &i, &line, "-->")) {
                    xml_fail(err, start_line, "a comment is never closed");
                    goto fail;
                }
                continue;
            }

            if (i + 8 < n && memcmp(s + i + 2, "[CDATA[", 7) == 0) {
                i += 9;
                int32_t text = i;
                if (!xml_skip_to(s, n, &i, &line, "]]>")) {
                    xml_fail(err, start_line, "a CDATA section is never closed");
                    goto fail;
                }
                int32_t textlen = i - 3 - text;
                if (depth > 0) {
                    if (!xml_add_run(d, open[depth - 1], (XmlSpan){ s + text, textlen })) {
                        xml_fail(err, start_line, "out of memory");
                        goto fail;
                    }
                } else if (!xml_all_space(s + text, textlen)) {
                    xml_fail(err, start_line, "text outside the root element");
                    goto fail;
                }
                continue;
            }

            /* A declaration, in practice a DOCTYPE.  Skipped whole, an internal
             * subset and all: the entities it declares are not read, so a
             * document that relies on them decodes them as written.  None of
             * the client's files carries one, which is why this is a skip
             * rather than a feature. */
            i += 2;
            int32_t bracket = 0;
            while (i < n) {
                if (s[i] == '\n') line++;
                if (s[i] == '[') bracket++;
                else if (s[i] == ']') bracket--;
                else if (s[i] == '>' && bracket <= 0) { i++; break; }
                i++;
            }
            continue;
        }

        /* --- </name> ----------------------------------------------------- */
        if (c == '/') {
            i += 2;
            while (i < n && xml_is_space(s[i])) {
                if (s[i] == '\n') line++;
                i++;
            }
            int32_t nstart = i;
            while (i < n && xml_is_name_char((unsigned char)s[i])) i++;
            XmlSpan cname = { s + nstart, i - nstart };
            while (i < n && xml_is_space(s[i])) {
                if (s[i] == '\n') line++;
                i++;
            }
            if (i >= n || s[i] != '>') {
                xml_fail(err, line, "a closing tag is not closed by '>'");
                goto fail;
            }
            i++;

            if (depth == 0) {
                xml_fail_name(err, line, "a closing tag </", cname, "> with nothing open");
                goto fail;
            }
            XmlNode *top = &d->nodes[open[depth - 1]];
            if (!xml_span_eq(top->name, cname)) {
                /* Both names, in the order a reader needs them: what was open,
                 * then what arrived. */
                if (!err->msg[0]) {
                    char want[48];
                    int32_t wn = top->name.len < (int32_t)sizeof want - 1
                               ? top->name.len : (int32_t)sizeof want - 1;
                    memcpy(want, top->name.p, (size_t)wn);
                    want[wn] = '\0';
                    xml_fail_name(err, line, "expected </", cname, ">");
                    snprintf(err->msg, sizeof err->msg, "expected </%s>, found </%.*s>",
                             want, (int)cname.len, cname.p);
                    err->line = line;
                }
                goto fail;
            }
            depth--;
            continue;
        }

        /* --- <name ...> or <name ... /> ---------------------------------- */
        if (!xml_is_name_start((unsigned char)c)) {
            xml_fail(err, line, "a '<' that does not begin a tag");
            goto fail;
        }

        int32_t start_line = line;
        i++;
        int32_t nstart = i;
        while (i < n && xml_is_name_char((unsigned char)s[i])) i++;
        XmlSpan name = { s + nstart, i - nstart };
        int32_t parent = depth > 0 ? open[depth - 1] : 0;
        int32_t id = xml_new_node(d, name, parent, start_line);
        if (id < 0) { xml_fail(err, start_line, "out of memory"); goto fail; }
        xml_link(d, parent, id);
        if (parent == 0) {
            if (d->first_top == 0) d->first_top = id;
            d->ntop++;
        }

        for (;;) {
            while (i < n && xml_is_space(s[i])) {
                if (s[i] == '\n') line++;
                i++;
            }
            if (i >= n) {
                xml_fail_name(err, start_line, "the start tag <", name, "> is never closed");
                goto fail;
            }

            if (s[i] == '/') {
                if (i + 1 >= n || s[i + 1] != '>') {
                    xml_fail_name(err, start_line, "a '/' inside <", name, "> is not '/>'");
                    goto fail;
                }
                i += 2;
                break;                          /* self-closing: nothing open */
            }

            if (s[i] == '>') {
                i++;
                /* Push, growing the stack rather than refusing a document that
                 * is merely deep. */
                if (depth == cap) {
                    int32_t *g = (int32_t *)xml_grow(open, &cap, depth + 1, sizeof *open);
                    if (!g) { xml_fail(err, start_line, "out of memory"); goto fail; }
                    open = g;
                }
                open[depth++] = id;
                break;
            }

            /* An attribute.  A bare name (no `=`) is tolerated with an empty
             * value: legal XML never writes one, so refusing it would only
             * reject a file that is not in hand. */
            if (!xml_is_name_start((unsigned char)s[i])) {
                xml_fail_name(err, line, "a malformed attribute inside <", name, ">");
                goto fail;
            }
            int32_t astart = i;
            while (i < n && xml_is_name_char((unsigned char)s[i])) i++;
            XmlSpan aname = { s + astart, i - astart };
            XmlSpan avalue = { s + i, 0 };

            int32_t save = i;
            while (i < n && xml_is_space(s[i])) {
                if (s[i] == '\n') line++;
                i++;
            }
            if (i < n && s[i] == '=') {
                i++;
                while (i < n && xml_is_space(s[i])) {
                    if (s[i] == '\n') line++;
                    i++;
                }
                char quote = i < n ? s[i] : '\0';
                if (quote != '"' && quote != '\'') {
                    xml_fail_name(err, line, "an attribute of <", name,
                                  "> has a value that is not quoted");
                    goto fail;
                }
                i++;
                int32_t vstart = i;
                while (i < n && s[i] != quote) {
                    if (s[i] == '\n') line++;
                    i++;
                }
                if (i >= n) {
                    xml_fail_name(err, line, "a quoted attribute value in <", name,
                                  "> is never closed");
                    goto fail;
                }
                avalue = (XmlSpan){ s + vstart, i - vstart };
                i++;
            } else {
                i = save;                       /* no `=`: an empty value      */
            }

            if (!xml_add_attr(d, id, aname, avalue)) {
                xml_fail(err, line, "out of memory");
                goto fail;
            }
        }
    }

    if (depth > 0) {
        XmlNode *top = &d->nodes[open[depth - 1]];
        xml_fail_name(err, top->line, "the element <", top->name, "> is never closed");
        goto fail;
    }

    free(open);
    *out = d;
    return 1;

fail:
    free(open);
    xml_doc_free(d);
    return 0;
}

const XmlNode *xml_node_at(const XmlDoc *d, int32_t id) {
    if (!d || id < 0 || id >= d->nnodes) return NULL;
    return &d->nodes[id];
}

void xml_doc_free(void *payload) {
    XmlDoc *d = (XmlDoc *)payload;
    if (!d) return;
    free(d->src);
    free(d->nodes);
    free(d->attrs);
    free(d->runs);
    free(d);
}
