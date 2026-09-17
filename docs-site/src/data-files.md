# Reading data files

Two kits exist for the data files a program reads: `Kiln.Xml` for the tables,
and `Kiln.Encoding` for the codepages they are written in. They are separate because the
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

`using Kiln.Xml;` reads it as bytes and hands bytes back. **Nothing is decoded**
— which is what makes a declaration that lies a non-event, and what lets an
element name in Chinese come back as that name. Decoding is a separate,
deliberate step.

`Xml.Parse` takes a **byte-set** rather than a path: `File.ReadBytes` opens the
file, so there is one place in the language that opens files.

```k2
namespace Items;

using Kiln.File;
using Kiln.Xml;

public static class P
{
    public static void Main()
    {
        int doc = Xml.Parse(File.ReadBytes("ItemBaseAttribute.xml"));
        if (doc == 0)
        {
            Console.WriteLine($"could not read it: {LastErrorText()}");
            return;
        }

        int root = Xml.Root(doc);
        Console.WriteLine($"{Xml.Count(doc, root)} group(s) under {Xml.Name(doc, root)}");

        // Rows sit below the groups, so the search descends; then siblings walk them.
        int sword = Xml.Descend(doc, root, "ShortSword");
        string id = Xml.Attr(doc, sword, "ID");
        string attack = Xml.Attr(doc, sword, "Attack");
        Console.WriteLine($"first sword {id} hits for {attack}");

        Console.WriteLine($"on line {Xml.Line(doc, sword)}");
        Xml.Close(doc);
    }
}
```

The navigation is small on purpose:

| | |
|---|---|
| `Xml.Root(doc)` | the first top-level element |
| `Xml.Count(doc, node)` / `Xml.Child(doc, node, i)` | children, positions counting from 1 |
| `Xml.First(doc, node, name)` / `Xml.Sibling(doc, node, name)` | the first child with that name, then the next |
| `Xml.Descend(doc, node, name)` | the first match at any depth |
| `Xml.Name`, `Xml.Attr`, `Xml.Text`, `Xml.AttrCount`, `Xml.AttrName`, `Xml.AttrAt` | what is in a node |
| `Xml.Parent`, `Xml.Line` | where it sits, and where to complain about it |

**An empty name means any element.** That is not a quirk: a client names every
row of its item table its own template, so a walk over rows has to be able to say
"the next one, whatever it is called" — `Xml.First(doc, parent, "")` and
`Xml.Sibling(doc, node, "")`.

**A missing attribute is not a failure.** `Xml.Attr` answers `""` and leaves the
error slot clear, because most attributes are optional; `Xml.HasAttr` is the
predicate that separates "absent" from "present and empty", the same way
`Db.IsNull` separates a NULL column from an empty one. A bad handle or a node id
from another document is a failure, and sets a code.

A document that cannot be read is refused with the line:

```
line 41: expected </Swords>, found </Weapons>
```

## The codepages

`using Kiln.Encoding;` converts. It ships **no conversion table** — GBK is
23,940 mappings and hand-copying them is how a decoder ends up 99% right and
silently wrong on a name — so it asks the platform: iconv on POSIX, the Win32
codepage API on Windows. `gbk`, `gb2312`, `cp936`, `gb18030`, `big5`,
`shift-jis`, `latin1`, `utf-16le`, `utf-16be` and `utf-8` all resolve, and
`Encoding.Known` answers whether this build can do one *before* a program
depends on it.

```k2
namespace Names;

using Kiln.File;
using Kiln.Encoding;

public static class P
{
    public static void Main()
    {
        var raw = File.ReadBytes("EquipName.dat");
        string names = Encoding.Decode(raw, "gbk");
        Console.WriteLine($"{names.Length} character(s)");

        // and back again, for anything the client has to read
        var forTheClient = Encoding.Encode("短剑", "gbk");
        Console.WriteLine($"{Bytes.Count(forTheClient)} byte(s)");
    }
}
```

**Two decoders, and the difference is the point.** `Encoding.Decode` refuses
input that is not of the encoding and names the byte that failed — what a
program wants when it is checking a file. `Encoding.DecodeLossy` replaces a
malformed byte with U+FFFD and carries on, which is what a `StreamReader` in C#
does by default and therefore what a faithful port of one needs.
`Encoding.Encode` is strict in both directions: it refuses a `string` that is
not UTF-8 (GBK bytes held in one, most likely) rather than sending a client a
name that is quietly wrong.

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
