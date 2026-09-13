/* The `xml` library's own structures — the document, and the flat pools it is
 * built from.
 *
 * Nothing here is ABI: a program sees a handle and small positive ints, never
 * one of these.  The point of the layout is that a whole document is THREE
 * realloc-grown arrays plus one copy of the source, so parsing a 134 KB table
 * with 3,430 rows allocates four times rather than once per node — and a node
 * id is an index into that array, which is what makes navigating a document
 * free of allocation entirely. */
#ifndef KILN_XML_INTERNAL_H
#define KILN_XML_INTERNAL_H

#include <stdint.h>

/* A run of the document's own bytes.  Values are NOT copied, NOT decoded and
 * NOT NUL-terminated: the source is kept whole for the document's life, and a
 * command copies out (decoding entity references) when the program asks for
 * one.  That is what lets a byte-set that is GBK be parsed without knowing
 * anything about GBK — the bytes travel through untouched. */
typedef struct { const char *p; int32_t len; } XmlSpan;

/* One attribute: a name and a value, both runs of the source. */
typedef struct { XmlSpan name, value; } XmlAttr;

typedef struct {
    XmlSpan  name;
    int32_t  line;              /* 1-based: where the start tag begins       */
    int32_t  parent;            /* 0 when the element is top level          */
    int32_t  first_child, last_child, next_sibling;
    int32_t  first_attr, nattr;
    int32_t  first_run,  nrun;  /* the element's own text, in document order */
} XmlNode;

typedef struct {
    char    *src;               /* the document, NUL-terminated, never edited */
    int32_t  srclen;
    /* nodes[0] is the DOCUMENT: no name, no attributes, no text, and its
     * children are the top-level elements.  It exists so that every navigation
     * command can take "the document" as a parent id without a second code
     * path, and so node id 0 means "nothing" everywhere else. */
    XmlNode *nodes; int32_t nnodes, ncap;
    XmlAttr *attrs; int32_t nattrs, acap;
    XmlSpan *runs;  int32_t nruns,  rcap;
    int32_t  first_top;         /* first top-level element, 0 when none      */
    int32_t  ntop;
} XmlDoc;

/* Where a parse failure happened, and what it was.  The message carries no
 * line: the command formats the one line of error text out of the two. */
typedef struct { int32_t line; char msg[112]; } XmlError;

/* Frees the document and everything under it.  Safe on NULL. */
void xml_doc_free(void *payload);

/* Parses a copy of `src` (len bytes; may contain NULs and any encoding).
 * Answers 1 and a document, or 0 with `err` filled in.  On failure nothing is
 * allocated that the caller has to free. */
int32_t xml_doc_build(const char *src, int32_t len, XmlDoc **out, XmlError *err);

/* The node with that id, or NULL for an id that is out of range.  Id 0 is the
 * document, which has no name, no attributes and no text — callers that accept
 * it say so; the ones that do not refuse it themselves. */
const XmlNode *xml_node_at(const XmlDoc *d, int32_t id);

#endif /* KILN_XML_INTERNAL_H */
