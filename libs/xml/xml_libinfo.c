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
      "int h = XmlParse(BytesFromText(\"<items count=\\\"2\\\"/>\"));\n"
      "Console.WriteLine(h);" },

    { "xml_close", "xml_close", KN_SDT_BOOL, 1, P_H,
      "Close a document and free it; false when the handle was not one",
      "int h = XmlParse(BytesFromText(\"<a/>\"));\n"
      "bool ok = XmlClose(h);\n"
      "Console.WriteLine($\"closed: {ok}\");" },

    { "xml_close_all", "xml_close_all", KN_SDT_INT, 0, NULL,
      "Close every open document and answer how many there were",
      "Console.WriteLine(XmlCloseAll());" },

    { "xml_root", "xml_root", KN_SDT_INT, 1, P_H,
      "The first top-level element, or 0 for a document that holds none",
      "int h = XmlParse(BytesFromText(\"<items><row id=\\\"1\\\"/></items>\"));\n"
      "Console.WriteLine(XmlRoot(h));" },

    { "xml_parent", "xml_parent", KN_SDT_INT, 2, P_HN,
      "The element a node sits inside, or 0 when it is top level",
      "int h = XmlParse(BytesFromText(\"<items><row id=\\\"1\\\"/></items>\"));\n"
      "int r = XmlFirst(h, XmlRoot(h), \"row\");\n"
      "Console.WriteLine(XmlParent(h, r));" },

    { "xml_line", "xml_line", KN_SDT_INT, 2, P_HN,
      "The 1-based line an element's start tag begins on, for diagnostics",
      "int h = XmlParse(BytesFromText(\"<items>\\n  <row/>\\n  </items>\"));\n"
      "int r = XmlFirst(h, XmlRoot(h), \"row\");\n"
      "Console.WriteLine(XmlLine(h, r));" },

    { "xml_count", "xml_count", KN_SDT_INT, 2, P_HN,
      "How many child elements a node has; -1 on a bad handle or node id",
      "int h = XmlParse(BytesFromText(\"<items><row id=\\\"1\\\"/><row id=\\\"2\\\"/></items>\"));\n"
      "Console.WriteLine(XmlCount(h, XmlRoot(h)));" },

    { "xml_child", "xml_child", KN_SDT_INT, 3, P_HNI,
      "The i-th child element, counting from 1, or 0 when there is none",
      "int h = XmlParse(BytesFromText(\"<items><row id=\\\"1\\\"/></items>\"));\n"
      "Console.WriteLine(XmlChild(h, XmlRoot(h), 1));" },

    { "xml_name", "xml_name", KN_SDT_TEXT, 2, P_HN,
      "An element's own name, which is the tag it was written with",
      "int h = XmlParse(BytesFromText(\"<items><row id=\\\"1\\\"/></items>\"));\n"
      "int r = XmlFirst(h, XmlRoot(h), \"row\");\n"
      "Console.WriteLine(XmlName(h, r));" },

    { "xml_attr", "xml_attr", KN_SDT_TEXT, 3, P_HNT,
      "An attribute's value, or \"\" when it is not there — xml_has_attr tells those apart",
      "int h = XmlParse(BytesFromText(\"<row id=\\\"1007\\\" Attack=\\\"396,428\\\"/>\"));\n"
      "Console.WriteLine(XmlAttr(h, XmlRoot(h), \"Attack\"));" },

    { "xml_has_attr", "xml_has_attr", KN_SDT_BOOL, 3, P_HNT,
      "Whether an element carries an attribute, which an empty value cannot say",
      "int h = XmlParse(BytesFromText(\"<row id=\\\"1007\\\"/>\"));\n"
      "if (XmlHasAttr(h, XmlRoot(h), \"Attack\") == false)\n"
      "{\n"
      "    Console.WriteLine(\"no Attack\");\n"
      "}" },

    { "xml_attr_count", "xml_attr_count", KN_SDT_INT, 2, P_HN,
      "How many attributes an element carries",
      "int h = XmlParse(BytesFromText(\"<row id=\\\"1007\\\" Attack=\\\"1\\\"/>\"));\n"
      "Console.WriteLine(XmlAttrCount(h, XmlRoot(h)));" },

    { "xml_attr_name", "xml_attr_name", KN_SDT_TEXT, 3, P_HNI,
      "The name of an element's i-th attribute, counting from 1",
      "int h = XmlParse(BytesFromText(\"<row id=\\\"1007\\\"/>\"));\n"
      "Console.WriteLine(XmlAttrName(h, XmlRoot(h), 1));" },

    { "xml_attr_at", "xml_attr_at", KN_SDT_TEXT, 3, P_HNI,
      "The value of an element's i-th attribute, counting from 1",
      "int h = XmlParse(BytesFromText(\"<row id=\\\"1007\\\"/>\"));\n"
      "Console.WriteLine(XmlAttrAt(h, XmlRoot(h), 1));" },

    { "xml_text", "xml_text", KN_SDT_TEXT, 2, P_HN,
      "An element's own text with the surrounding whitespace removed, or \"\" when it holds only elements",
      "int h = XmlParse(BytesFromText(\"<name>Kryss</name>\"));\n"
      "Console.WriteLine(XmlText(h, XmlRoot(h)));" },

    { "xml_first", "xml_first", KN_SDT_INT, 3, P_HNT,
      "The first child of a node with that name — \"\" meaning any name — or 0 when there is none",
      "int h = XmlParse(BytesFromText(\"<items><row id=\\\"1\\\"/></items>\"));\n"
      "Console.WriteLine(XmlFirst(h, XmlRoot(h), \"row\"));" },

    { "xml_sibling", "xml_sibling", KN_SDT_INT, 3, P_HNT,
      "The next element after a node with that name, which is how the next row is found",
      "int h = XmlParse(BytesFromText(\"<items><row id=\\\"1\\\"/><row id=\\\"2\\\"/></items>\"));\n"
      "int r = XmlFirst(h, XmlRoot(h), \"row\");\n"
      "r = XmlSibling(h, r, \"row\");\n"
      "Console.WriteLine(XmlAttr(h, r, \"id\"));" },

    { "xml_descend", "xml_descend", KN_SDT_INT, 3, P_HNT,
      "The first element at any depth under a node with that name, or 0 when there is none",
      "int h = XmlParse(BytesFromText(\"<root><group><row id=\\\"7\\\"/></group></root>\"));\n"
      "Console.WriteLine(XmlDescend(h, XmlRoot(h), \"row\"));" },
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
