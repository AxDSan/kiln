# Reading data files

Two kits exist for the data files a program reads: `xml` for the tables, and
`encoding` for the codepages they are written in. They are separate because the
two problems are separate — a document can be UTF-8 XML that needs no decoding,
and a name can be GBK in a `.dat` with no XML anywhere near it.

## The tables are XML, and the declaration lies

A real client table declares `encoding="GB2312"` over bytes that are really GBK,
opens with a comment block carrying Chinese, ends its lines with CRLF, and holds
one self-closing element per row:

```
<?xml version="1.0" encoding="GB2312"?>
<!--"weapon", 武器 ... -->
<ItemBaseAttribute>
  <Weapons>
    <Swords>
      <ShortSword ID="1000" Attack="15,23,39,68,118" Icon="36,144" .../>
```

`use xml` reads it as bytes and hands bytes back. **Nothing is decoded** — which
is what makes a declaration that lies a non-event, and what lets an element name
in Chinese come back as that name. Decoding is a separate, deliberate step.

`xml_parse` takes a **byte-set** rather than a path: `file_read_bytes` opens the
file, so there is one place in the language that opens files.

```
module items
target console
use file
use xml

sub main
  let doc: int = xml_parse(file_read_bytes("ItemBaseAttribute.xml"))
  if doc = 0
    call print_text("could not read it: {last_error_text()}")
    return
  end

  let root: int = xml_root(doc)
  let rows: int = xml_count(doc, root)
  call print_text("{rows} group(s) under {xml_name(doc, root)}")

  # Rows sit below the groups, so the search descends; then siblings walk them.
  let sword: int = xml_descend(doc, root, "ShortSword")
  let id: text = xml_attr(doc, sword, "ID")
  let attack: text = xml_attr(doc, sword, "Attack")
  call print_text("first sword {id} hits for {attack}")

  call print_text("on line {xml_line(doc, sword)}")
  call xml_close(doc)
end
```

The navigation is small on purpose:

| | |
|---|---|
| `xml_root(doc)` | the first top-level element |
| `xml_count(doc, node)` / `xml_child(doc, node, i)` | children, positions counting from 1 |
| `xml_first(doc, node, name)` / `xml_sibling(doc, node, name)` | the first child with that name, then the next |
| `xml_descend(doc, node, name)` | the first match at any depth |
| `xml_name`, `xml_attr`, `xml_text`, `xml_attr_count`, `xml_attr_name`, `xml_attr_at` | what is in a node |
| `xml_parent`, `xml_line` | where it sits, and where to complain about it |

**An empty name means any element.** That is not a quirk: a client names every
row of its item table its own template, so a walk over rows has to be able to say
"the next one, whatever it is called" — `xml_first(doc, parent, "")` and
`xml_sibling(doc, node, "")`.

**A missing attribute is not a failure.** `xml_attr` answers `""` and leaves the
error slot clear, because most attributes are optional; `xml_has_attr` is the
predicate that separates "absent" from "present and empty", the same way
`db_is_null` separates a NULL column from an empty one. A bad handle or a node id
from another document is a failure, and sets a code.

A document that cannot be read is refused with the line:

```
line 41: expected </Swords>, found </Weapons>
```

## The codepages

`use encoding` converts. It ships **no conversion table** — GBK is 23,940
mappings and hand-copying them is how a decoder ends up 99% right and silently
wrong on a name — so it asks the platform: iconv on POSIX, the Win32 codepage API
on Windows. `gbk`, `gb2312`, `cp936`, `gb18030`, `big5`, `shift-jis`, `latin1`,
`utf-16le`, `utf-16be` and `utf-8` all resolve, and `encoding_known` answers
whether this build can do one *before* a program depends on it.

```
module names
target console
use file
use encoding

sub main
  let raw = file_read_bytes("EquipName.dat")
  let names: text = encoding_decode(raw, "gbk")
  call print_text("{length(names)} character(s)")

  # and back again, for anything the client has to read
  let fortheclient: bytes = encoding_encode("短剑", "gbk")
  call print_text("{bytes_count(fortheclient)} byte(s)")
end
```

**Two decoders, and the difference is the point.** `encoding_decode` refuses input
that is not of the encoding and names the byte that failed — what a program wants
when it is checking a file. `encoding_decode_lossy` replaces a malformed byte with
U+FFFD and carries on, which is what a `StreamReader` in C# does by default and
therefore what a faithful port of one needs. `encoding_encode` is strict in both
directions: it refuses text that is not UTF-8 (GBK bytes held in a `text`, most
likely) rather than sending a client a name that is quietly wrong.

A round trip is exact — decoding a 134 KB table and re-encoding it returns the
original byte count, and `iconv` over the same file produces the same UTF-8, byte
for byte.

## What is not here

No XPath, no namespaces, no DTD: a DOCTYPE is skipped whole and the entities it
would have declared are not known, so a document relying on them reads them as
written. No XML *writer* — these files are read, and a table that needs editing is
edited by the program that owns it. No image decoding, so the item icons a client
keeps in its texture atlases are still unread; that is the management console's
problem rather than the server's.
