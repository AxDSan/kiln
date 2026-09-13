/* "xml" library metadata (design-time only; compiled into the introspection
 * .so, never a shipped program — same split as core_libinfo.c).
 *
 * The reader exists because a great deal of data lives in XML: item tables,
 * probabilities, quests and UI layouts, in files whose shape and encoding are
 * someone else's decision.  Reading them is cheaper than reimplementing tables
 * nobody can type out reliably.  Everything here reads
 * bytes as bytes, so a GB2312 declaration over GBK content is not a problem to
 * solve — it is a document to read (see libs/xml/xml_parse.c). */
#include "kiln_abi.h"

void xml_parse(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void xml_close(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void xml_close_all(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void xml_root(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void xml_parent(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void xml_line(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void xml_count(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void xml_child(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void xml_name(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void xml_attr(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void xml_has_attr(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void xml_attr_count(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void xml_attr_name(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void xml_attr_at(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void xml_text(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void xml_first(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void xml_sibling(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
void xml_descend(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);

static const int32_t P_BIN[]     = { KN_SDT_BIN };
static const int32_t P_H[]       = { KN_SDT_INT };
static const int32_t P_HN[]      = { KN_SDT_INT, KN_SDT_INT };
static const int32_t P_HNI[]     = { KN_SDT_INT, KN_SDT_INT, KN_SDT_INT };
static const int32_t P_HNT[]     = { KN_SDT_INT, KN_SDT_INT, KN_SDT_TEXT };

static const Kiln_CommandDesc XML_COMMANDS[] = {
    { "xml_parse", "xml_parse", KN_SDT_INT, 1, P_BIN,
      "Parse a document from a byte-set and answer its handle, or 0 with the line and reason in the error slot",
      "let h: int = xml_parse(bytes_from_text(r\"\"\"<items count=\"2\"/>\"\"\"))\ncall print_int(h)" },

    { "xml_close", "xml_close", KN_SDT_BOOL, 1, P_H,
      "Close a document and free it; false when the handle was not one",
      "let h: int = xml_parse(bytes_from_text(r\"\"\"<a/>\"\"\"))\nlet ok: bool = xml_close(h)\ncall print_text(\"closed: {ok}\")" },

    { "xml_close_all", "xml_close_all", KN_SDT_INT, 0, NULL,
      "Close every open document and answer how many there were",
      "call print_int(xml_close_all())" },

    { "xml_root", "xml_root", KN_SDT_INT, 1, P_H,
      "The first top-level element, or 0 for a document that holds none",
      "let h: int = xml_parse(bytes_from_text(r\"\"\"<items><row id=\"1\"/></items>\"\"\"))\ncall print_int(xml_root(h))" },

    { "xml_parent", "xml_parent", KN_SDT_INT, 2, P_HN,
      "The element a node sits inside, or 0 when it is top level",
      "let h: int = xml_parse(bytes_from_text(r\"\"\"<items><row id=\"1\"/></items>\"\"\"))\nlet r: int = xml_first(h, xml_root(h), \"row\")\ncall print_int(xml_parent(h, r))" },

    { "xml_line", "xml_line", KN_SDT_INT, 2, P_HN,
      "The 1-based line an element's start tag begins on, for diagnostics",
      "let h: int = xml_parse(bytes_from_text(r\"\"\"<items>\n<row/>\n</items>\"\"\"))\nlet r: int = xml_first(h, xml_root(h), \"row\")\ncall print_int(xml_line(h, r))" },

    { "xml_count", "xml_count", KN_SDT_INT, 2, P_HN,
      "How many child elements a node has; -1 on a bad handle or node id",
      "let h: int = xml_parse(bytes_from_text(r\"\"\"<items><row id=\"1\"/><row id=\"2\"/></items>\"\"\"))\ncall print_int(xml_count(h, xml_root(h)))" },

    { "xml_child", "xml_child", KN_SDT_INT, 3, P_HNI,
      "The i-th child element, counting from 1, or 0 when there is none",
      "let h: int = xml_parse(bytes_from_text(r\"\"\"<items><row id=\"1\"/></items>\"\"\"))\ncall print_int(xml_child(h, xml_root(h), 1))" },

    { "xml_name", "xml_name", KN_SDT_TEXT, 2, P_HN,
      "An element's own name, which is the tag it was written with",
      "let h: int = xml_parse(bytes_from_text(r\"\"\"<items><row id=\"1\"/></items>\"\"\"))\nlet r: int = xml_first(h, xml_root(h), \"row\")\ncall print_text(xml_name(h, r))" },

    { "xml_attr", "xml_attr", KN_SDT_TEXT, 3, P_HNT,
      "An attribute's value, or \"\" when it is not there — xml_has_attr tells those apart",
      "let h: int = xml_parse(bytes_from_text(r\"\"\"<row id=\"1007\" Attack=\"396,428\"/>\"\"\"))\ncall print_text(xml_attr(h, xml_root(h), \"Attack\"))" },

    { "xml_has_attr", "xml_has_attr", KN_SDT_BOOL, 3, P_HNT,
      "Whether an element carries an attribute, which an empty value cannot say",
      "let h: int = xml_parse(bytes_from_text(r\"\"\"<row id=\"1007\"/>\"\"\"))\nif xml_has_attr(h, xml_root(h), \"Attack\") = false\n  call print_text(\"no Attack\")\nend" },

    { "xml_attr_count", "xml_attr_count", KN_SDT_INT, 2, P_HN,
      "How many attributes an element carries",
      "let h: int = xml_parse(bytes_from_text(r\"\"\"<row id=\"1007\" Attack=\"1\"/>\"\"\"))\ncall print_int(xml_attr_count(h, xml_root(h)))" },

    { "xml_attr_name", "xml_attr_name", KN_SDT_TEXT, 3, P_HNI,
      "The name of an element's i-th attribute, counting from 1",
      "let h: int = xml_parse(bytes_from_text(r\"\"\"<row id=\"1007\"/>\"\"\"))\ncall print_text(xml_attr_name(h, xml_root(h), 1))" },

    { "xml_attr_at", "xml_attr_at", KN_SDT_TEXT, 3, P_HNI,
      "The value of an element's i-th attribute, counting from 1",
      "let h: int = xml_parse(bytes_from_text(r\"\"\"<row id=\"1007\"/>\"\"\"))\ncall print_text(xml_attr_at(h, xml_root(h), 1))" },

    { "xml_text", "xml_text", KN_SDT_TEXT, 2, P_HN,
      "An element's own text with the surrounding whitespace removed, or \"\" when it holds only elements",
      "let h: int = xml_parse(bytes_from_text(r\"\"\"<name>Kryss</name>\"\"\"))\ncall print_text(xml_text(h, xml_root(h)))" },

    { "xml_first", "xml_first", KN_SDT_INT, 3, P_HNT,
      "The first child of a node with that name — \"\" meaning any name — or 0 when there is none",
      "let h: int = xml_parse(bytes_from_text(r\"\"\"<items><row id=\"1\"/></items>\"\"\"))\ncall print_int(xml_first(h, xml_root(h), \"row\"))" },

    { "xml_sibling", "xml_sibling", KN_SDT_INT, 3, P_HNT,
      "The next element after a node with that name, which is how the next row is found",
      "let h: int = xml_parse(bytes_from_text(r\"\"\"<items><row id=\"1\"/><row id=\"2\"/></items>\"\"\"))\nvar r: int = xml_first(h, xml_root(h), \"row\")\nr = xml_sibling(h, r, \"row\")\ncall print_text(xml_attr(h, r, \"id\"))" },

    { "xml_descend", "xml_descend", KN_SDT_INT, 3, P_HNT,
      "The first element at any depth under a node with that name, or 0 when there is none",
      "let h: int = xml_parse(bytes_from_text(r\"\"\"<root><group><row id=\"7\"/></group></root>\"\"\"))\ncall print_int(xml_descend(h, xml_root(h), \"row\"))" },
};

static const Kiln_LibInfo XML_INFO = {
    KILN_ABI_VERSION,
    "xml",
    "kiln-xml-0000-0000-0000-000000000003",
    0, 1, 0,
    (int32_t)(sizeof(XML_COMMANDS) / sizeof(XML_COMMANDS[0])),
    XML_COMMANDS,
    0, NULL,
};

const Kiln_LibInfo *kiln_get_lib_info(void) {
    return &XML_INFO;
}
