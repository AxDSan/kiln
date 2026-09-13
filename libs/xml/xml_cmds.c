/* The `xml` library — reading XML data files.
 *
 * A parsed document is a handle (kind KN_HK_XML).  Inside it, an ELEMENT is a
 * small positive int, and 0 means "nothing" — the same numbering the rest of
 * the language uses.  Those ints are the whole navigation surface, so nothing
 * is allocated by walking a document and a program cannot hold a node that has
 * been freed: the ids die with the handle that issued them.
 *
 *     h    = xml_parse(file_read_bytes("ItemBaseAttribute.xml"))
 *     node = xml_first(h, xml_root(h), "Kryss")
 *     call print_text(xml_attr(node, "Attack"))
 *
 * Three decisions worth stating, because a reader will otherwise expect the
 * other behaviour:
 *
 *  - **Values are byte runs, not decoded text.**  A GBK name comes back as GBK
 *    bytes, because decoding is the wrong thing to do by default in a document
 *    whose declared encoding lies about itself.  `use encoding` turns one into
 *    UTF-8 when the program wants that.
 *  - **A missing attribute answers `""` and CLEARS the error slot.**  Absent is
 *    not a failure — most attributes are optional — so `xml_has_attr` is the
 *    predicate beside the ambiguous sentinel, exactly as libs/README.md asks.
 *    A bad handle or a bad node id is a failure, and sets a code.
 *  - **An empty name means any element.**  `xml_first(h, node, "")` is the
 *    first child of any name and `xml_sibling(h, node, "")` walks them all,
 *    which is how a program iterates rows whose element names differ — the
 *    client's item table names every row its own template.
 *
 * Failure follows the house rule: a handle command returns 0, a count or an id
 * -1 where it is a count and 0 where it is "nothing", text "", a yes/no false,
 * and the reason is left in the error slot for `last_error_code` /
 * `last_error_text`.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "kiln_abi.h"
#include "xml_internal.h"

/* --- handles and nodes ------------------------------------------------- */

static XmlDoc *xml_doc(int32_t h) {
    return (XmlDoc *)kn_handle_resolve(h, KN_HK_XML);   /* sets the slot */
}

/* The node `id`, refused when it is out of range, or when it is the document
 * and the caller cannot use one (the document has no name, attributes or
 * text).  Every command that takes a node goes through here, so all of them
 * report a bad id identically, and the message names the command that asked —
 * which is the difference between a diagnosable line and a shrug. */
static const XmlNode *xml_at(XmlDoc *d, int32_t id, int32_t allow_doc, const char *cmd) {
    const XmlNode *n = xml_node_at(d, id);
    char msg[128];
    if (!n) {
        snprintf(msg, sizeof msg, "%s: no node with that id in this document", cmd);
        kn_error_set(KN_ERR_OUT_OF_RANGE, msg);
        return NULL;
    }
    if (id == 0 && !allow_doc) {
        snprintf(msg, sizeof msg, "%s: the document has no name, attributes or text", cmd);
        kn_error_set(KN_ERR_INVALID_ARG, msg);
        return NULL;
    }
    return n;
}

/* --- text out ---------------------------------------------------------- */

/* Entity and character references, decoded as they are copied.  Answers how
 * many bytes were written, or -1 when this is not a reference the parser knows,
 * in which case the caller copies the '&' literally and carries on: a document
 * with an undeclared entity keeps its text rather than losing it. */
static int32_t xml_reference(const char *p, int32_t avail, char out[4]) {
    if (avail < 3 || p[0] != '&') return -1;

    if (p[1] == '#') {
        long code = 0;
        int32_t i = 2;
        int32_t hex = 0;
        if (i < avail && (p[i] == 'x' || p[i] == 'X')) { hex = 1; i++; }
        int32_t digits = 0;
        while (i < avail && p[i] != ';' && digits < 8) {
            char c = p[i];
            int32_t v;
            if (c >= '0' && c <= '9') v = c - '0';
            else if (hex && c >= 'a' && c <= 'f') v = c - 'a' + 10;
            else if (hex && c >= 'A' && c <= 'F') v = c - 'A' + 10;
            else break;
            code = code * (hex ? 16 : 10) + v;
            i++;
            digits++;
        }
        if (digits == 0 || i >= avail || p[i] != ';') return -1;

        /* UTF-8, hand-rolled: the one place this library produces bytes rather
         * than passing them through.  Out of range or a surrogate is not a
         * character; it stays literal like any other unknown reference. */
        if (code <= 0 || code > 0x10FFFF || (code >= 0xD800 && code <= 0xDFFF)) return -1;
        if (code < 0x80) { out[0] = (char)code; return 1; }
        if (code < 0x800) {
            out[0] = (char)(0xC0 | (code >> 6));
            out[1] = (char)(0x80 | (code & 0x3F));
            return 2;
        }
        if (code < 0x10000) {
            out[0] = (char)(0xE0 | (code >> 12));
            out[1] = (char)(0x80 | ((code >> 6) & 0x3F));
            out[2] = (char)(0x80 | (code & 0x3F));
            return 3;
        }
        out[0] = (char)(0xF0 | (code >> 18));
        out[1] = (char)(0x80 | ((code >> 12) & 0x3F));
        out[2] = (char)(0x80 | ((code >> 6) & 0x3F));
        out[3] = (char)(0x80 | (code & 0x3F));
        return 4;
    }

    struct { const char *name; int32_t len; char value; } named[] = {
        { "amp;",  4, '&' }, { "lt;", 3, '<' }, { "gt;", 3, '>' },
        { "quot;", 5, '"' }, { "apos;", 5, '\'' },
    };
    for (size_t k = 0; k < sizeof named / sizeof named[0]; k++) {
        int32_t len = named[k].len;
        if (avail < len + 1) continue;
        if (memcmp(p + 1, named[k].name, (size_t)len - 1) == 0 && p[len] == ';') {
            out[0] = named[k].value;
            return 1;
        }
    }
    return -1;
}

/* Copies a run out, decoding references.  Answers a runtime-owned string, or
 * NULL when there was nothing to allocate. */
static char *xml_copy(const char *p, int32_t len) {
    char *out = (char *)kn_malloc((long)len + 1);
    if (!out) return NULL;
    int32_t w = 0;
    for (int32_t i = 0; i < len; i++) {
        if (p[i] == '&') {
            char decoded[4];
            int32_t got = xml_reference(p + i, len - i, decoded);
            if (got > 0) {
                memcpy(out + w, decoded, (size_t)got);
                w += got;
                /* find the ';' this reference ended at */
                int32_t j = i + 1;
                while (j < len && p[j] != ';') j++;
                i = j < len ? j : i;
                continue;
            }
        }
        out[w++] = p[i];
    }
    out[w] = '\0';
    return out;
}

static void xml_ret_run(Kiln_Slot *ret, const char *p, int32_t len) {
    char *out = xml_copy(p, len);
    if (!out) { kn_error_set(KN_ERR_INVALID_ARG, "xml: out of memory"); kn_ret_text(ret, NULL); return; }
    kn_error_clear();
    kn_ret_text(ret, out);
}

static void xml_ret_empty(Kiln_Slot *ret) {
    kn_error_clear();
    kn_ret_text(ret, NULL);      /* the ABI's empty text */
}

static void xml_fail_text(Kiln_Slot *ret, int32_t code, const char *msg) {
    kn_error_set(code, msg);
    kn_ret_text(ret, NULL);
}

/* --- opening and closing ------------------------------------------------ */

void xml_parse(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    const Kiln_Bin *b = (const Kiln_Bin *)kn_arg_ptr(argv, 0);
    if (!b || b->len < 0) {
        kn_error_set(KN_ERR_INVALID_ARG, "xml_parse: expected a byte-set");
        kn_ret_int(ret, 0);
        return;
    }

    XmlError err;
    err.line = 0;
    err.msg[0] = '\0';

    XmlDoc *d = NULL;
    if (!xml_doc_build((const char *)(b + 1), b->len, &d, &err)) {
        char msg[160];
        snprintf(msg, sizeof msg, "xml_parse: line %d: %s", err.line ? err.line : 1, err.msg);
        kn_error_set(KN_ERR_INVALID_ARG, msg);
        kn_ret_int(ret, 0);
        return;
    }

    int32_t h = kn_handle_new(KN_HK_XML, d, xml_doc_free);
    if (h == 0) { xml_doc_free(d); kn_ret_int(ret, 0); return; }   /* slot is set */

    kn_error_clear();
    kn_ret_int(ret, h);
}

void xml_close(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    kn_ret_bool(ret, kn_handle_close(kn_arg_int(argv, 0), KN_HK_XML));
}

void xml_close_all(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; (void)argv;
    kn_ret_int(ret, kn_handle_close_kind(KN_HK_XML));
}

/* --- walking ------------------------------------------------------------ */

/* The first top-level element, or 0 for a document that holds none — which is
 * not a failure: a file of nothing but comments parses, it simply has no
 * root. */
void xml_root(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    XmlDoc *d = xml_doc(kn_arg_int(argv, 0));
    if (!d) { kn_ret_int(ret, 0); return; }
    kn_error_clear();
    kn_ret_int(ret, d->first_top);
}

void xml_line(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    XmlDoc *d = xml_doc(kn_arg_int(argv, 0));
    if (!d) { kn_ret_int(ret, -1); return; }
    const XmlNode *n = xml_at(d, kn_arg_int(argv, 1), 1, "xml_line");
    if (!n) { kn_ret_int(ret, -1); return; }
    kn_error_clear();
    kn_ret_int(ret, n->line);
}

void xml_parent(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    XmlDoc *d = xml_doc(kn_arg_int(argv, 0));
    if (!d) { kn_ret_int(ret, -1); return; }
    const XmlNode *n = xml_at(d, kn_arg_int(argv, 1), 1, "xml_parent");
    if (!n) { kn_ret_int(ret, -1); return; }
    kn_error_clear();
    kn_ret_int(ret, n->parent);
}

/* Counted by walking: the children are a linked list of ids, and a stored
 * count would be a fifth field to keep true for one caller. */
static int32_t xml_child_count(const XmlDoc *d, const XmlNode *n) {
    int32_t count = 0;
    for (int32_t id = n->first_child; id; id = d->nodes[id].next_sibling) count++;
    return count;
}

void xml_count(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    XmlDoc *d = xml_doc(kn_arg_int(argv, 0));
    if (!d) { kn_ret_int(ret, -1); return; }
    const XmlNode *n = xml_at(d, kn_arg_int(argv, 1), 1, "xml_count");
    if (!n) { kn_ret_int(ret, -1); return; }
    kn_error_clear();
    kn_ret_int(ret, xml_child_count(d, n));
}

void xml_child(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    XmlDoc *d = xml_doc(kn_arg_int(argv, 0));
    if (!d) { kn_ret_int(ret, 0); return; }
    const XmlNode *n = xml_at(d, kn_arg_int(argv, 1), 1, "xml_child");
    if (!n) { kn_ret_int(ret, 0); return; }

    int32_t want = kn_arg_int(argv, 2);
    if (want < 1) {
        kn_error_set(KN_ERR_OUT_OF_RANGE, "xml_child: children are numbered from 1");
        kn_ret_int(ret, 0);
        return;
    }
    int32_t id = n->first_child;
    while (id && --want > 0) id = d->nodes[id].next_sibling;
    if (!id) {
        kn_error_set(KN_ERR_OUT_OF_RANGE, "xml_child: no child at that position");
        kn_ret_int(ret, 0);
        return;
    }
    kn_error_clear();
    kn_ret_int(ret, id);
}

/* --- names and attributes ----------------------------------------------- */

void xml_name(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    XmlDoc *d = xml_doc(kn_arg_int(argv, 0));
    if (!d) { kn_ret_text(ret, NULL); return; }
    const XmlNode *n = xml_at(d, kn_arg_int(argv, 1), 0, "xml_name");
    if (!n) { kn_ret_text(ret, NULL); return; }
    xml_ret_run(ret, n->name.p, n->name.len);
}

/* The attribute whose name is `name`, or NULL. */
static const XmlAttr *xml_attr_find(const XmlDoc *d, const XmlNode *n, const char *name) {
    if (!name) return NULL;
    size_t len = strlen(name);
    for (int32_t i = 0; i < n->nattr; i++) {
        const XmlAttr *a = &d->attrs[n->first_attr + i];
        if ((size_t)a->name.len == len && memcmp(a->name.p, name, len) == 0) return a;
    }
    return NULL;
}

/* An attribute that is not there is not a failure: the slot is cleared and the
 * text is empty, and `xml_has_attr` is what tells that apart from an attribute
 * whose value is empty. */
void xml_attr(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    XmlDoc *d = xml_doc(kn_arg_int(argv, 0));
    if (!d) { kn_ret_text(ret, NULL); return; }
    const XmlNode *n = xml_at(d, kn_arg_int(argv, 1), 0, "xml_attr");
    if (!n) { kn_ret_text(ret, NULL); return; }
    const XmlAttr *a = xml_attr_find(d, n, kn_arg_text(argv, 2));
    if (!a) { xml_ret_empty(ret); return; }
    xml_ret_run(ret, a->value.p, a->value.len);
}

void xml_has_attr(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    XmlDoc *d = xml_doc(kn_arg_int(argv, 0));
    if (!d) { kn_ret_bool(ret, 0); return; }
    const XmlNode *n = xml_at(d, kn_arg_int(argv, 1), 0, "xml_has_attr");
    if (!n) { kn_ret_bool(ret, 0); return; }
    kn_error_clear();
    kn_ret_bool(ret, xml_attr_find(d, n, kn_arg_text(argv, 2)) != NULL);
}

void xml_attr_count(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    XmlDoc *d = xml_doc(kn_arg_int(argv, 0));
    if (!d) { kn_ret_int(ret, -1); return; }
    const XmlNode *n = xml_at(d, kn_arg_int(argv, 1), 0, "xml_attr_count");
    if (!n) { kn_ret_int(ret, -1); return; }
    kn_error_clear();
    kn_ret_int(ret, n->nattr);
}

static void xml_attr_by_position(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv,
                                 const char *cmd, int32_t want_value) {
    (void)argc;
    XmlDoc *d = xml_doc(kn_arg_int(argv, 0));
    if (!d) { kn_ret_text(ret, NULL); return; }
    const XmlNode *n = xml_at(d, kn_arg_int(argv, 1), 0, cmd);
    if (!n) { kn_ret_text(ret, NULL); return; }

    int32_t want = kn_arg_int(argv, 2);
    if (want < 1 || want > n->nattr) {
        xml_fail_text(ret, KN_ERR_OUT_OF_RANGE, "xml: attributes are numbered from 1");
        return;
    }
    const XmlAttr *a = &d->attrs[n->first_attr + want - 1];
    if (want_value) xml_ret_run(ret, a->value.p, a->value.len);
    else            xml_ret_run(ret, a->name.p, a->name.len);
}

void xml_attr_name(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    xml_attr_by_position(ret, argc, argv, "xml_attr_name", 0);
}

void xml_attr_at(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    xml_attr_by_position(ret, argc, argv, "xml_attr_at", 1);
}

/* --- text --------------------------------------------------------------- */

/* The element's own text: its direct runs, concatenated, with the whitespace
 * at either end taken off.  Whitespace-only content answers "" — an element
 * holding nothing but the newline and indent before its children is the normal
 * case in every one of these files, and handing that back as text would
 * make `xml_text` useless for the question it is asked. */
void xml_text(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    XmlDoc *d = xml_doc(kn_arg_int(argv, 0));
    if (!d) { kn_ret_text(ret, NULL); return; }
    const XmlNode *n = xml_at(d, kn_arg_int(argv, 1), 0, "xml_text");
    if (!n) { kn_ret_text(ret, NULL); return; }

    int32_t total = 0;
    for (int32_t i = 0; i < n->nrun; i++) {
        XmlSpan r = d->runs[n->first_run + i];
        int32_t a = 0, b = r.len;
        if (i == 0) while (a < b && (r.p[a] == ' ' || r.p[a] == '\t' ||
                                     r.p[a] == '\r' || r.p[a] == '\n')) a++;
        if (i == n->nrun - 1) while (b > a && (r.p[b-1] == ' ' || r.p[b-1] == '\t' ||
                                               r.p[b-1] == '\r' || r.p[b-1] == '\n')) b--;
        if (b > a) total += b - a;
    }
    if (total == 0) { xml_ret_empty(ret); return; }

    char *out = (char *)kn_malloc((long)total + 1);
    if (!out) { xml_fail_text(ret, KN_ERR_INVALID_ARG, "xml_text: out of memory"); return; }

    int32_t w = 0;
    for (int32_t i = 0; i < n->nrun; i++) {
        XmlSpan r = d->runs[n->first_run + i];
        int32_t a = 0, b = r.len;
        if (i == 0) while (a < b && (r.p[a] == ' ' || r.p[a] == '\t' ||
                                     r.p[a] == '\r' || r.p[a] == '\n')) a++;
        if (i == n->nrun - 1) while (b > a && (r.p[b-1] == ' ' || r.p[b-1] == '\t' ||
                                               r.p[b-1] == '\r' || r.p[b-1] == '\n')) b--;
        for (int32_t k = a; k < b; k++) {
            if (r.p[k] == '&') {
                char decoded[4];
                int32_t got = xml_reference(r.p + k, b - k, decoded);
                if (got > 0) {
                    memcpy(out + w, decoded, (size_t)got);
                    w += got;
                    int32_t j = k + 1;
                    while (j < b && r.p[j] != ';') j++;
                    k = j < b ? j : k;
                    continue;
                }
            }
            out[w++] = r.p[k];
        }
    }
    out[w] = '\0';
    kn_error_clear();
    kn_ret_text(ret, out);
}

/* --- searching ---------------------------------------------------------- */

static int32_t xml_name_matches(const XmlDoc *d, int32_t id, const char *name) {
    if (!name || name[0] == '\0') return 1;             /* "" = any element   */
    size_t len = strlen(name);
    const XmlNode *n = &d->nodes[id];
    return (size_t)n->name.len == len && memcmp(n->name.p, name, len) == 0;
}

/* The first child of `parent` named `name`, 0 when there is none.  Nothing
 * found is not a failure — the walk simply ends — so the slot is cleared. */
void xml_first(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    XmlDoc *d = xml_doc(kn_arg_int(argv, 0));
    if (!d) { kn_ret_int(ret, 0); return; }
    const XmlNode *n = xml_at(d, kn_arg_int(argv, 1), 1, "xml_first");
    if (!n) { kn_ret_int(ret, 0); return; }
    const char *name = kn_arg_text(argv, 2);

    for (int32_t id = n->first_child; id; id = d->nodes[id].next_sibling) {
        if (xml_name_matches(d, id, name)) { kn_error_clear(); kn_ret_int(ret, id); return; }
    }
    kn_error_clear();
    kn_ret_int(ret, 0);
}

void xml_sibling(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    XmlDoc *d = xml_doc(kn_arg_int(argv, 0));
    if (!d) { kn_ret_int(ret, 0); return; }
    const XmlNode *n = xml_at(d, kn_arg_int(argv, 1), 1, "xml_sibling");
    if (!n) { kn_ret_int(ret, 0); return; }
    const char *name = kn_arg_text(argv, 2);

    for (int32_t id = n->next_sibling; id; id = d->nodes[id].next_sibling) {
        if (xml_name_matches(d, id, name)) { kn_error_clear(); kn_ret_int(ret, id); return; }
    }
    kn_error_clear();
    kn_ret_int(ret, 0);
}

/* The next node in document order after `id`, or 0 past the end.  This is what
 * lets the search below be a loop rather than a recursion: a subtree is a
 * contiguous run of nodes in document order. */
static int32_t xml_next_in_order(const XmlDoc *d, int32_t id) {
    if (d->nodes[id].first_child) return d->nodes[id].first_child;
    while (id) {
        if (d->nodes[id].next_sibling) return d->nodes[id].next_sibling;
        id = d->nodes[id].parent;
    }
    return 0;
}

static int32_t xml_last_descendant(const XmlDoc *d, int32_t id) {
    while (d->nodes[id].last_child) id = d->nodes[id].last_child;
    return id;
}

/* The first element at any depth under `node` (not `node` itself) with that
 * name, in document order.  This is what a nested file needs — Quest.xml and
 * the UI layouts nest, where the item and forge tables are flat. */
void xml_descend(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc;
    XmlDoc *d = xml_doc(kn_arg_int(argv, 0));
    if (!d) { kn_ret_int(ret, 0); return; }
    const XmlNode *n = xml_at(d, kn_arg_int(argv, 1), 1, "xml_descend");
    if (!n) { kn_ret_int(ret, 0); return; }
    const char *name = kn_arg_text(argv, 2);

    int32_t start = kn_arg_int(argv, 1);
    int32_t last = n->first_child ? xml_last_descendant(d, start) : 0;

    for (int32_t id = n->first_child; id; ) {
        if (xml_name_matches(d, id, name)) { kn_error_clear(); kn_ret_int(ret, id); return; }
        if (id == last) break;
        id = xml_next_in_order(d, id);
    }
    kn_error_clear();
    kn_ret_int(ret, 0);
}
