//! Kiln 2 values in the debugger.
//!
//! Kiln 2 lays a record out as C lays out a struct and describes it field by
//! field in DWARF; 1.x's heap `record` prefixes its fields with a
//! `{count, pad}` header and reaches them through the runtime. Both are
//! described as a *pointer to a structure*, so a reader that goes by the
//! pointer alone reads one layout's bytes under the other's rule. The first
//! half of this file pins the reader down on each shape without needing a
//! program to run; the last test drives the whole chain — compiler, DWARF,
//! reader, adapter — through a real K2 binary.
//!
//! The direct tests stand in for a session because the shapes are the whole
//! question here: the values come from memory this file builds, so a failure
//! names the reader rather than a compiler, a linker or `ptrace`.

use kiln_debug::unwind::Memory;
use kiln_debug::value::{self, Field, Local, RecordShape};
use std::path::PathBuf;

/// The base of the fake stack every fixture is built at, eight-byte aligned
/// because a traced process only offers whole words.
const BASE: u64 = 0x1000;

/// A block of a stopped program's memory, with nothing else mapped.
struct Ram {
    bytes: Vec<u8>,
}

impl Ram {
    fn new() -> Ram {
        Ram { bytes: Vec::new() }
    }

    /// Write raw bytes at an offset from the base, growing to fit. Growth is
    /// rounded to a word so the last value in a fixture is never unreadable
    /// for a reason no program has.
    fn put(&mut self, offset: u64, bytes: &[u8]) -> &mut Ram {
        let at = offset as usize;
        let end = at + bytes.len();
        if self.bytes.len() < end {
            self.bytes.resize(end.next_multiple_of(8), 0);
        }
        self.bytes[at..end].copy_from_slice(bytes);
        self
    }

    fn put_i32(&mut self, offset: u64, v: i32) -> &mut Ram {
        self.put(offset, &v.to_le_bytes())
    }

    fn put_u64(&mut self, offset: u64, v: u64) -> &mut Ram {
        self.put(offset, &v.to_le_bytes())
    }

    /// A NUL-terminated string, as a K2 `string` points at one.
    fn put_text(&mut self, offset: u64, s: &str) -> &mut Ram {
        self.put(offset, s.as_bytes());
        self.put(offset + s.len() as u64, &[0])
    }
}

impl Memory for Ram {
    fn read_u64(&self, address: u64) -> Option<u64> {
        let offset = address.checked_sub(BASE)? as usize;
        let end = offset.checked_add(8)?;
        if end > self.bytes.len() {
            return None;
        }
        Some(u64::from_le_bytes(
            self.bytes[offset..end].try_into().unwrap(),
        ))
    }
}

fn local(name: &str, type_name: &str, offset: u64) -> Local {
    Local {
        name: name.to_string(),
        frame_offset: offset as i64,
        type_name: type_name.to_string(),
    }
}

fn field(name: &str, byte_offset: u64, type_name: &str) -> Field {
    Field {
        name: name.to_string(),
        byte_offset,
        type_name: type_name.to_string(),
    }
}

/// A Kiln 2 record is a C-layout struct reached through its slot's pointer, and
/// its first field sits at zero. Reading it as a 1.x heap record — a header and
/// then cells — shows the pointer's own bytes as the first field, or refuses
/// the record for claiming a field count it does not have.
#[test]
fn a_kiln2_record_reads_its_fields_at_their_own_offsets() {
    let mut ram = Ram::new();
    ram.put_u64(0, BASE + 16) // the local's slot points at the struct
        .put_i32(16, 3) // X at offset 0
        .put_i32(20, 4); // Y at offset 4

    let fields = vec![field("X", 0, "int"), field("Y", 4, "int")];
    let value = value::read_record(
        &local("p", "Point", 0),
        RecordShape::Pointer,
        &fields,
        BASE,
        &ram,
    );
    assert_eq!(value.to_string(), "Point { X: 3, Y: 4 }");
}

/// A K2 `class` is the same shape: one allocation of the struct, reached
/// through the reference. Its fields are named the same way and read the same
/// way, which is the point of the shape living in the debug information.
#[test]
fn a_kiln2_class_reads_its_fields_too() {
    let mut ram = Ram::new();
    ram.put_u64(0, BASE + 16)
        .put_i32(16, 7) // Value at offset 0
        .put_u64(24, BASE + 48) // Label, a string held at offset 8
        .put_text(48, "ada");

    let fields = vec![field("Value", 0, "int"), field("Label", 8, "")];
    let value = value::read_record(
        &local("b", "Box", 0),
        RecordShape::Pointer,
        &fields,
        BASE,
        &ram,
    );
    assert_eq!(value.to_string(), "Box { Value: 7, Label: \"ada\" }");
}

/// A K2 `string` reaches the reader as a pointer with no name: the front end
/// spells it `string`, and DWARF has no such name to carry. The characters are
/// the only true reading, and `null` is the empty string rather than an error.
#[test]
fn a_kiln2_string_reads_through_its_unnamed_pointer() {
    let mut ram = Ram::new();
    ram.put_u64(0, BASE + 16).put_text(16, "hello");
    assert_eq!(
        value::read(&local("s", "", 0), BASE, &ram)
            .unwrap()
            .to_string(),
        "\"hello\""
    );

    let mut empty = Ram::new();
    empty.put_u64(0, 0);
    assert_eq!(
        value::read(&local("s", "", 0), BASE, &empty)
            .unwrap()
            .to_string(),
        "\"\""
    );
}

/// A `List<T>` is the compiler's own record, `{len, cap, data}`, and `data`
/// points at the elements themselves — not at a string and not at a runtime
/// array. Reading that buffer as text walks element bytes until a zero happens
/// to appear, so the address is what is shown and the length is still true.
#[test]
fn a_kiln2_list_shows_its_length_and_not_a_walk_through_its_elements() {
    let mut ram = Ram::new();
    ram.put_u64(0, BASE + 16) // the slot points at `$List3`
        .put_i32(16, 2) // len
        .put_i32(20, 4) // cap
        .put_u64(24, BASE + 48) // data, a raw `int` buffer
        .put_i32(48, 10)
        .put_i32(52, 20);

    let fields = vec![
        field("len", 0, "int"),
        field("cap", 4, "int"),
        field("data", 8, ""),
    ];
    let value = value::read_record(
        &local("nums", "$List3", 0),
        RecordShape::Pointer,
        &fields,
        BASE,
        &ram,
    );
    // The compiler's instance number is not the name of anything the user
    // wrote, so the type is shown under the language's own spelling.
    assert_eq!(
        value.to_string(),
        format!("List {{ len: 2, cap: 4, data: {} }}", BASE + 48)
    );
}

/// `Result<T>` is the compiler's `{ok, value, err}` record, and every field is
/// one the reader already knows: a bool, the value, and a string.
#[test]
fn a_kiln2_result_shows_its_value_and_its_error() {
    let mut ram = Ram::new();
    ram.put_u64(0, BASE + 16)
        .put_i32(16, 1) // ok at offset 0, a C int in a C-layout record
        .put_i32(20, 2) // value at offset 4
        .put_u64(24, BASE + 48) // err at offset 8
        .put_text(48, "");

    let fields = vec![
        field("ok", 0, "bool"),
        field("value", 4, "int"),
        field("err", 8, ""),
    ];
    let value = value::read_record(
        &local("r", "$Result2", 0),
        RecordShape::Pointer,
        &fields,
        BASE,
        &ram,
    );
    assert_eq!(
        value.to_string(),
        "Result { ok: true, value: 2, err: \"\" }"
    );
}

/// A null reference is a value, not a read that failed: `null` is what the
/// language calls absence, and the reader shows the same word for it.
#[test]
fn a_null_kiln2_reference_reads_as_nothing() {
    let mut ram = Ram::new();
    ram.put_u64(0, 0);
    let fields = vec![field("X", 0, "int")];
    let value = value::read_record(
        &local("p", "Point", 0),
        RecordShape::Pointer,
        &fields,
        BASE,
        &ram,
    );
    assert_eq!(value.to_string(), "nothing");
}

// --- The whole chain --------------------------------------------------------

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Build a program the way the CLI does, so its functions can be read back out
/// of the binary.
fn build(source: &std::path::Path, name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("kiln_debug_k2_builds");
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join(name);
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_kiln"))
        .arg("build")
        .arg(source)
        .arg("-o")
        .arg(&out)
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .status()
        .expect("run kiln");
    assert!(status.success(), "build failed for {}", source.display());
    out
}

/// Every address `kiln debug --dump-subs` reports, in the order it reports
/// them.
fn subprogram_addresses(bin: &std::path::Path) -> Vec<String> {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["debug", "--dump-subs", bin.to_str().unwrap()])
        .output()
        .expect("run kiln debug");
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let (_, rest) = line.rsplit_once(" address: ")?;
            Some(rest.split_whitespace().next()?.to_string())
        })
        .collect()
}

/// A 1.x compile unit names itself `Kiln 2.0.0` — the release's version, not
/// the language's — which is one space away from the `Kiln 2 ` a Kiln 2 unit
/// writes. Confusing the two reads a 1.x function from the symbol table *and*
/// from its own DWARF, and every frame is listed twice.
#[test]
fn a_kiln1_binary_is_not_mistaken_for_kiln2() {
    let bin = build(&repo().join("examples/1x/loops.kiln"), "loops_debug_k2");
    let addresses = subprogram_addresses(&bin);
    assert!(!addresses.is_empty(), "no subprograms were listed");
    let mut unique = addresses.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        addresses.len(),
        "a 1.x function was listed twice: {addresses:?}"
    );
}

/// Drive the Debug Adapter Protocol the way an editor does, and return the
/// transcript it wrote.
///
/// Every request is sent up front on purpose. The adapter reads requests while
/// the program runs, and an adapter that only reads between stops would
/// deadlock here instead of answering — which is exactly what the first test
/// that drove one this way was written to catch.
fn session(dir: &std::path::Path, source: &str, line: u32, requests: &[(&str, &str)]) -> String {
    use std::io::{BufReader, Read, Write};
    use std::process::{Command, Stdio};

    let mut adapter = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .arg("dap")
        .current_dir(dir)
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("start the adapter");

    let mut input = adapter.stdin.take().unwrap();
    let mut seq = 0;
    let mut send = |command: &str, arguments: &str| {
        seq += 1;
        let body = if arguments.is_empty() {
            format!(r#"{{"seq":{seq},"type":"request","command":"{command}"}}"#)
        } else {
            format!(
                r#"{{"seq":{seq},"type":"request","command":"{command}","arguments":{arguments}}}"#
            )
        };
        input
            .write_all(format!("Content-Length: {}\r\n\r\n{body}", body.len()).as_bytes())
            .unwrap();
        input.flush().unwrap();
    };

    send("initialize", r#"{"adapterID":"kiln"}"#);
    send("launch", &format!(r#"{{"program":"{source}"}}"#));
    send(
        "setBreakpoints",
        &format!(r#"{{"source":{{"path":"{source}"}},"breakpoints":[{{"line":{line}}}]}}"#),
    );
    send("configurationDone", "");
    for (command, arguments) in requests {
        send(command, arguments);
    }
    send("disconnect", "");
    drop(input);

    let mut transcript = String::new();
    BufReader::new(adapter.stdout.take().unwrap())
        .read_to_string(&mut transcript)
        .unwrap();
    let _ = adapter.wait();
    transcript
}

/// A Kiln 2 program stopped under the debugger shows what its own values hold.
///
/// This is the assertion the direct tests cannot make: that the compiler wrote
/// a Kiln 2 record into DWARF in the shape the reader now expects, that the
/// function was found at all without a `kn_user_` name to look for, and that
/// the frame base the compiler named is the register the session used.
#[test]
fn a_stopped_kiln2_program_shows_its_values() {
    let dir = std::env::temp_dir().join("kiln_dap_k2_values_test");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("k2.kiln"),
        "namespace DbgK2;\n\
         \n\
         public record Point(int X, int Y);\n\
         \n\
         public static class P\n\
         {\n\
         \x20   public static void Main()\n\
         \x20   {\n\
         \x20       var p = new Point(3, 4);\n\
         \x20       var s = \"hello\";\n\
         \x20       var nums = new List<int>();\n\
         \x20       nums.Add(10);\n\
         \x20       nums.Add(20);\n\
         \x20       Console.WriteLine($\"{p.X} {p.Y} {s.Length} {nums.Count}\");\n\
         \x20   }\n\
         }\n",
    )
    .unwrap();

    // The locals expand in declaration order, so `p` and `nums` are the two
    // records the client can open; expanding a range rather than a fixed pair
    // of references keeps the assertion about the values and not about the
    // order the compiler happened to write them in.
    let transcript = session(
        &dir,
        "k2.kiln",
        14,
        &[
            ("scopes", r#"{"frameId":1}"#),
            ("variables", r#"{"variablesReference":1}"#),
            ("variables", r#"{"variablesReference":2}"#),
            ("variables", r#"{"variablesReference":3}"#),
            ("variables", r#"{"variablesReference":4}"#),
        ],
    );
    let dense: String = transcript.chars().filter(|c| !c.is_whitespace()).collect();

    // The record's own fields, read out of the heap struct it points at.
    assert!(
        dense.contains(r#""name":"X","value":"3""#),
        "the record's first field did not read: {transcript}"
    );
    assert!(
        dense.contains(r#""name":"Y","value":"4""#),
        "the record's second field did not read: {transcript}"
    );
    // The string, read through a pointer the debug information does not name.
    assert!(
        dense.contains(r#""name":"s","value":"\"hello\"""#),
        "the string did not read as its characters: {transcript}"
    );
    // The list's own header, which is true even though its element buffer is
    // raw storage the debug information does not describe.
    assert!(
        dense.contains(r#""name":"len","value":"2""#),
        "the list's length did not read: {transcript}"
    );
    assert!(
        dense.contains("\"value\":\"List{\u{2026}}\""),
        "the list was not named as a List: {transcript}"
    );
}
