//! `libs/xml` and `libs/encoding`, end to end in a built binary.
//!
//! Both exist for one job — reading data files, which are
//! XML with a GB2312 declaration over GBK bytes — so the fixtures here are
//! written to disk as BYTES rather than as source strings: a GBK byte pair that
//! survives a round trip through a temporary file is the property that matters,
//! and a Rust string literal cannot express it.
//!
//! Nothing here needs the client: the files are built by the test. The real
//! tables are read by hand against a checkout of the server tree, where the
//! parser agrees element-for-element and attribute-for-attribute with Python's
//! ElementTree and the decoder agrees byte-for-byte with iconv.

use std::path::PathBuf;
use std::process::Command;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Every test gets its own directory, so two of them never race on one file.
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kiln_datafiles_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// Build inline source and run it, answering its stdout.
fn run_in(dir: &PathBuf, src: &str, tag: &str) -> String {
    let path = dir.join("main.kiln");
    std::fs::write(&path, src).expect("write source");
    let bin = dir.join("prog");

    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", path.to_str().unwrap(), "-o", bin.to_str().unwrap()])
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln build");
    assert!(
        out.status.success(),
        "kiln build {tag} failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let run = Command::new(&bin).output().expect("run the program");
    assert!(
        run.status.success(),
        "{tag} exited non-zero:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    String::from_utf8_lossy(&run.stdout).to_string()
}

/// A table in the shape these files have: a GB2312 declaration over GBK bytes, a
/// comment block carrying Chinese, CRLF, and one self-closing row per item —
/// under a group element, because the real item table nests.
fn item_table(dir: &PathBuf) -> PathBuf {
    let mut bytes: Vec<u8> = Vec::new();
    bytes.extend_from_slice(b"<?xml version=\"1.0\" encoding=\"GB2312\"?>\r\n");
    // 武器 and 盔甲, GBK, inside a comment: the parser must not care.
    bytes
        .extend_from_slice(b"<!--\"weapon\",\xce\xe4\xc6\xf7\r\n\"armor\",\xbf\xf8\xbc\xd7-->\r\n");
    bytes.extend_from_slice(b"<ItemBaseAttribute>\r\n  <Weapons>\r\n    <Swords>\r\n");
    bytes.extend_from_slice(
        b"      <ShortSword ID=\"1000\" Attack=\"15,23,39,68,118\" Icon=\"36,144\" \
          BaseFraction=\"0,12,32,60,100\"/>\r\n",
    );
    bytes.extend_from_slice(b"      <Scimitar ID=\"1001\" Attack=\"65,78,102,141,208\"/>\r\n");
    bytes.extend_from_slice(b"    </Swords>\r\n  </Weapons>\r\n");
    bytes.extend_from_slice(
        b"  <Armors>\r\n    <Plate ID=\"2100\" Attack=\"9\"/>\r\n  </Armors>\r\n",
    );
    bytes.extend_from_slice(b"</ItemBaseAttribute>\r\n");

    let path = dir.join("ItemBaseAttribute.xml");
    std::fs::write(&path, &bytes).expect("write fixture");
    path
}

#[test]
fn the_client_tables_parse_and_walk() {
    let dir = scratch("walk");
    let table = item_table(&dir);

    let out = run_in(
        &dir,
        &format!(
            r#"module xmlwalk
target console
use file
use xml

sub main
  let h = xml_parse(file_read_bytes("{path}"))
  if h = 0
    call print_text("parse failed: {{last_error_text()}}")
    return
  end

  let root = xml_root(h)
  let rname = xml_name(h, root)
  call print_text("root {{rname}} line {{xml_line(h, root)}} children {{xml_count(h, root)}}")

  # a row two levels down: the item table nests, so the search descends
  let sword = xml_descend(h, root, "ShortSword")
  let name = xml_name(h, sword)
  let id = xml_attr(h, sword, "ID")
  let atk = xml_attr(h, sword, "Attack")
  call print_text("{{name}} id={{id}} attack={{atk}}")
  call print_text("parent is {{xml_name(h, xml_parent(h, sword))}}")

  # absent is not a failure; the predicate is what tells it apart
  let missing = xml_attr(h, sword, "MaterialProyAdd")
  let has = xml_has_attr(h, sword, "MaterialProyAdd")
  call print_text("missing '{{missing}}' has={{has}} code={{last_error_code()}}")

  # by position, and by walking every sibling whatever its name
  let weapons = xml_child(h, root, 1)
  let wname = xml_name(h, weapons)
  let wcount = xml_count(h, weapons)
  call print_text("child 1 is {{wname}} with {{wcount}} child")
  let swords = xml_first(h, weapons, "Swords")
  var row = xml_first(h, swords, "")
  var rows = 0
  while row <> 0
    rows += 1
    row = xml_sibling(h, row, "")
  end
  call print_text("{{rows}} row(s) under Swords")

  call print_int(xml_close_all())
end
"#,
            path = table.display()
        ),
        "walk",
    );

    let expected = "\
root ItemBaseAttribute line 4 children 2
ShortSword id=1000 attack=15,23,39,68,118
parent is Swords
missing '' has=false code=0
child 1 is Weapons with 1 child
2 row(s) under Swords
1
";
    assert_eq!(out, expected);
}

#[test]
fn a_malformed_document_is_refused_with_its_line() {
    let dir = scratch("bad");

    let cases: [(&str, &str, &str); 3] = [
        (
            "unclosed",
            "<a>\n  <b>\n",
            "the element <b> is never closed",
        ),
        (
            "mismatch",
            "<a>\n  <b></c>\n</a>\n",
            "expected </b>, found </c>",
        ),
        ("stray", "<a/>\ntext\n", "text outside the root element"),
    ];

    for (tag, body, want) in cases {
        let path = dir.join(format!("{tag}.xml"));
        std::fs::write(&path, body).expect("write fixture");
        let out = run_in(
            &dir,
            &format!(
                r#"module xmlbad
target console
use file
use xml

sub main
  let h = xml_parse(file_read_bytes("{path}"))
  call print_text("handle {{h}}")
  call print_text(last_error_text())
end
"#,
                path = path.display()
            ),
            tag,
        );
        let mut lines = out.lines();
        assert_eq!(lines.next().unwrap(), "handle 0", "{tag}");
        let msg = lines.next().unwrap();
        assert!(msg.contains(want), "{tag}: {msg}");
        assert!(msg.contains("line"), "{tag}: {msg}");
    }
}

#[test]
fn entities_are_decoded_and_bytes_are_not_touched() {
    let dir = scratch("entities");
    // An attribute holding &amp; and &#x41;, and a text element with a newline
    // and indentation around it.
    let path = dir.join("e.xml");
    std::fs::write(
        &path,
        "<root><a expr=\"x &amp; y\" code=\"&#x41;&#66;\"><b>  Kris &amp; Co  </b></a></root>",
    )
    .expect("write fixture");

    // A GBK pair in an attribute, written as bytes: 0xD6 0xD0 is 中.
    let gbk = dir.join("gbk.xml");
    let mut bytes: Vec<u8> = Vec::new();
    bytes.extend_from_slice(b"<row name=\"");
    bytes.extend_from_slice(b"\xd6\xd0");
    bytes.extend_from_slice(b"\"/>");
    std::fs::write(&gbk, &bytes).expect("write fixture");

    let out = run_in(
        &dir,
        &format!(
            r#"module xmle
target console
use file
use xml
use encoding

sub main
  let h = xml_parse(file_read_bytes("{p}"))
  let a = xml_descend(h, xml_root(h), "a")
  let expr = xml_attr(h, a, "expr")
  let code = xml_attr(h, a, "code")
  call print_text("{{expr}}|{{code}}")
  let b = xml_first(h, a, "b")
  let t = xml_text(h, b)
  call print_text("text '{{t}}' length {{length(t)}}")

  let g = xml_parse(file_read_bytes("{g}"))
  let row = xml_root(g)
  let raw = xml_attr(g, row, "name")
  let kept = bytes_from_text(raw)
  call print_int(bytes_count(kept))
  call print_int(bytes_at(kept, 1))
  call print_int(bytes_at(kept, 2))
  # the attribute reached the program as the file's own bytes, and decoding
  # them is what turns the name into text the language can print
  let wide = encoding_decode(kept, "gbk")
  call print_text("decoded '{{wide}}' length {{length(wide)}}")
  let again = encoding_encode(wide, "gbk")
  let back = encoding_decode(again, "gbk")
  let same = text_eq(back, wide)
  call print_text("encode/decode round trip {{same}}")
end
"#,
            p = path.display(),
            g = gbk.display()
        ),
        "entities",
    );

    let expected = "\
x & y|AB
text 'Kris & Co' length 9
2
214
208
decoded '中' length 1
encode/decode round trip true
";
    assert_eq!(out, expected);
}

#[test]
fn the_client_codepages_convert_both_ways() {
    let dir = scratch("encoding");
    let out = run_in(
        &dir,
        r#"module enc
target console
use encoding

sub main
  # 中文 as GBK bytes, and back to the codepage it came from
  var gbk: bytes = bytes_new(4)
  call bytes_set(gbk, 1, 214)
  call bytes_set(gbk, 2, 208)
  call bytes_set(gbk, 3, 206)
  call bytes_set(gbk, 4, 196)
  let utf = encoding_decode(gbk, "gbk")
  call print_text("gbk -> '{utf}' length {length(utf)}")

  let back = encoding_encode(utf, "gbk")
  call print_int(bytes_count(back))
  call print_int(bytes_at(back, 1))
  call print_int(bytes_at(back, 2))

  # aliases reach the same converter
  let alias = encoding_decode(gbk, "cp936")
  let same = text_eq(alias, utf)
  call print_text("cp936 is gbk: {same}")
  let known = encoding_known("gb2312")
  call print_text("gb2312 known: {known}")

  # a broken byte: strict names it, lossy replaces it, exactly as the C#
  # reader this replaces would have
  var bad: bytes = bytes_new(3)
  call bytes_set(bad, 1, 65)
  call bytes_set(bad, 2, 255)
  call bytes_set(bad, 3, 66)
  let strict = encoding_decode(bad, "gbk")
  call print_text("strict '{strict}' code {last_error_code()}")
  call print_text(last_error_text())
  let lossy = encoding_decode_lossy(bad, "gbk")
  call print_text("lossy '{lossy}' length {length(lossy)}")

  # UTF-16, both orders
  let le = encoding_encode("hi", "utf-16le")
  let be = encoding_encode("hi", "utf-16be")
  call print_text("utf-16le {bytes_at(le, 1)},{bytes_at(le, 2)} utf-16be {bytes_at(be, 1)},{bytes_at(be, 2)}")

  # an encoding nobody here implements is refused by name
  let none = encoding_known("koi8-r")
  call print_text("koi8-r known: {none}")
end
"#,
        "encoding",
    );

    let expected = "\
gbk -> '中文' length 2
4
214
208
cp936 is gbk: true
gb2312 known: true
strict '' code 10005
encoding_decode: not gbk: byte 2 does not begin a character
lossy 'A\u{fffd}B' length 3
utf-16le 104,0 utf-16be 0,104
koi8-r known: false
";
    assert_eq!(out, expected);
}
