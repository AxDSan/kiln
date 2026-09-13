//! Kiln 2 against the real runtime: `kiln k2 --runtime` links the runtime and
//! the collector, so printing goes through the slot ABI and what a program
//! allocates is reclaimed.

use std::path::PathBuf;
use std::process::Command;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("k2-rt-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.join(name)
}

/// Build a K2 source against the runtime and run it, returning stdout.
fn build_and_run(name: &str, src: &str) -> String {
    let path = tmp(&format!("{name}.kiln"));
    let exe = tmp(name);
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args([
            "k2",
            path.to_str().unwrap(),
            "--runtime",
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("kiln k2");
    assert!(
        out.status.success(),
        "build failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&exe).output().expect("the program runs");
    assert!(run.status.success(), "exited {:?}", run.status.code());
    String::from_utf8_lossy(&run.stdout).to_string()
}

#[test]
fn printing_goes_through_the_runtime() {
    let out = build_and_run(
        "print",
        "namespace RtP;\npublic static class P\n{\n    public static void Main()\n    {\n        Console.WriteLine(\"through the runtime\");\n        var n = 6 * 7;\n        Console.WriteLine($\"answer {n}\");\n    }\n}\n",
    );
    assert_eq!(out, "through the runtime\nanswer 42\n");
}

#[test]
fn what_a_program_allocates_is_collected() {
    // Each round allocates a list that grows to 500 elements and then goes out
    // of reach. After a forced collection the reachable heap must be the same
    // whether the program did the work forty times or four hundred.
    let src = "\
namespace RtGc;
public static class P
{
    [Dll(\"runtime\", Entry = \"kn_gc_live_bytes\")]
    public static extern long LiveBytes();

    [Dll(\"runtime\", Entry = \"kn_gc_collect\")]
    public static extern long Collect();

    static int Rounds(int rounds)
    {
        var last = 0;
        foreach (var round in 1..rounds)
        {
            var xs = new List<int>();
            foreach (var i in 1..500)
                xs.Add(i);
            last = xs.Count;
        }
        return last;
    }

    public static void Main()
    {
        // Measure what is *reachable*, by collecting first: between
        // collections the heap also holds garbage that has not been swept yet.
        Console.WriteLine($\"{Rounds(40)}\");
        Collect();
        var small = LiveBytes();
        Console.WriteLine($\"{Rounds(400)}\");
        Collect();
        var big = LiveBytes();
        // Ten times the work, and nothing kept: what survives is a few bytes
        // either way, not a heap that grew with the work.
        Console.WriteLine($\"{small} {big}\");
    }
}
";
    let out = build_and_run("gc", src);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "500", "{out}");
    assert_eq!(lines[1], "500", "{out}");
    let mut live = lines[2].split_whitespace();
    let small: i64 = live.next().unwrap().parse().unwrap();
    let big: i64 = live.next().unwrap().parse().unwrap();
    // Each round allocates roughly 8 KiB, so 400 rounds move megabytes. What
    // remains reachable afterwards is a few bytes, and barely differs between
    // the two workloads — the collector is doing its job.
    assert!(
        big < 4096,
        "after collecting, {big} bytes are still held — 400 rounds were not reclaimed:\n{out}"
    );
    assert!(
        small < 4096,
        "after collecting, {small} bytes are still held:\n{out}"
    );
}

#[test]
fn the_standard_library_is_reachable() {
    // `using Kiln.File;` loads the file library, and `File.ReadText(p)` resolves
    // to its `file_read_text` command — the spec's naming rule, reversed at the
    // call site. A library whose commands are unprefixed (`uppercase`) is found
    // by the bare name.
    let path = tmp("stdlib-demo.txt");
    let p = path.to_str().unwrap();
    let src = format!(
        "namespace StdLib;\n\
         using Kiln.File;\n\
         using Kiln.Text;\n\
         public static class P\n\
         {{\n\
         \x20   public static void Main()\n\
         \x20   {{\n\
         \x20       File.WriteText(\"{p}\", \"written by Kiln 2\");\n\
         \x20       Console.WriteLine(File.ReadText(\"{p}\"));\n\
         \x20       Console.WriteLine($\"exists: {{File.Exists(\"{p}\")}}\");\n\
         \x20       Console.WriteLine(Text.Uppercase(\"kiln two\"));\n\
         \x20   }}\n\
         }}\n"
    );
    let out = build_and_run("stdlib", &src);
    assert_eq!(out, "written by Kiln 2\nexists: 1\nKILN TWO\n", "{out}");
}

#[test]
fn a_command_can_be_reached_as_an_instance_member() {
    // `length(s)` is written `s.Length`, and `uppercase(s)` as `s.Uppercase()`
    // — the receiver becomes the command's first argument. A name in scope is
    // a value, so `s.Uppercase()` is not read as a static `uppercase`.
    let src = "namespace Inst;\n\
               using Kiln.Text;\n\
               public static class P\n\
               {\n\
               \x20   public static void Main()\n\
               \x20   {\n\
               \x20       var s = \"kiln two\";\n\
               \x20       Console.WriteLine($\"{s.Length}\");\n\
               \x20       Console.WriteLine(s.Uppercase());\n\
               \x20   }\n\
               }\n";
    assert_eq!(build_and_run("instance", src), "8\nKILN TWO\n");
}

#[test]
fn a_command_failure_becomes_a_result() {
    // A library command reports failure through the error slot. Asking for a
    // Result turns that into a value; `??` does the same inline, which is what
    // 1.x spelled `otherwise`.
    let ok = tmp("result-ok.txt");
    let p = ok.to_str().unwrap();
    let src = format!(
        "namespace Res;\n\
         using Kiln.File;\n\
         public static class P\n\
         {{\n\
         \x20   public static void Main()\n\
         \x20   {{\n\
         \x20       File.WriteText(\"{p}\", \"here\");\n\
         \x20       Result<string> good = File.ReadText(\"{p}\");\n\
         \x20       Console.WriteLine($\"ok={{good.IsOk}} value={{good.Value}}\");\n\
         \x20       Result<string> bad = File.ReadText(\"/tmp/definitely-not-here-xyz\");\n\
         \x20       Console.WriteLine($\"ok={{bad.IsOk}}\");\n\
         \x20       Console.WriteLine(File.ReadText(\"/tmp/definitely-not-here-xyz\") ?? \"(missing)\");\n\
         \x20   }}\n\
         }}\n"
    );
    let out = build_and_run("result", &src);
    assert_eq!(out, "ok=1 value=here\nok=0\n(missing)\n", "{out}");
}

#[test]
fn a_failure_propagates_with_question_mark() {
    // `?` straight off a command call: the error slot becomes a Result and the
    // failure is handed back to the caller.
    let a = tmp("try-a.txt");
    let b = tmp("try-b.txt");
    let (pa, pb) = (a.to_str().unwrap(), b.to_str().unwrap());
    let src = format!(
        "namespace Try;\n\
         using Kiln.File;\n\
         public static class P\n\
         {{\n\
         \x20   static Result<string> Both(string x, string y)\n\
         \x20   {{\n\
         \x20       var first = File.ReadText(x)?;\n\
         \x20       var second = File.ReadText(y)?;\n\
         \x20       return first + second;\n\
         \x20   }}\n\
         \x20   public static void Main()\n\
         \x20   {{\n\
         \x20       File.WriteText(\"{pa}\", \"A\");\n\
         \x20       File.WriteText(\"{pb}\", \"B\");\n\
         \x20       var ok = Both(\"{pa}\", \"{pb}\");\n\
         \x20       Console.WriteLine($\"{{ok.IsOk}} {{ok.Value}}\");\n\
         \x20       var bad = Both(\"{pa}\", \"/tmp/not-here-xyz\");\n\
         \x20       Console.WriteLine($\"{{bad.IsOk}}\");\n\
         \x20   }}\n\
         }}\n"
    );
    assert_eq!(build_and_run("try", &src), "1 AB\n0\n");
}

#[test]
fn a_command_links_the_runtime_without_being_asked() {
    // `s.Length` is core's `length` command, reached with no `using` at all —
    // so a program that never says `--runtime` still needs the runtime linked.
    // The reader should not have to know which members are commands, and this
    // is the third configuration: libc printing with the runtime linked.
    let path = tmp("autolink.kiln");
    let exe = tmp("autolink");
    std::fs::write(
        &path,
        "namespace Auto;\npublic static class P\n{\n    public static void Main()\n    {\n        var s = \"kiln\";\n        Console.WriteLine($\"{s.Length}\");\n    }\n}\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["k2", path.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .expect("kiln k2");
    assert!(
        out.status.success(),
        "a command should link the runtime on its own:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&exe).output().expect("runs");
    assert_eq!(String::from_utf8_lossy(&run.stdout), "4\n");
}

#[test]
fn a_k2_binary_carries_a_line_table() {
    // Debug information is what makes a binary steppable. The module names the
    // file it came from, each statement emits a line marker, and each function
    // gets a subprogram — so a debugger can stop on a line and name the frame.
    let path = tmp("dbg.kiln");
    let exe = tmp("dbgbin");
    std::fs::write(
        &path,
        "namespace Dbg;\npublic static class P\n{\n    static int Twice(int n)\n    {\n        var doubled = n * 2;\n        return doubled;\n    }\n\n    public static void Main()\n    {\n        var a = 20;\n        Console.WriteLine($\"{Twice(a)}\");\n    }\n}\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["k2", path.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .expect("kiln k2");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The IR carries the metadata a debugger reads.
    let ir = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["k2", path.to_str().unwrap(), "--emit-ir"])
        .output()
        .expect("kiln k2 --emit-ir");
    let ll = String::from_utf8_lossy(&ir.stdout);
    assert!(ll.contains("DICompileUnit"), "no compile unit:\n{ll}");
    assert!(
        ll.contains("DISubprogram(name: \"P_Twice\""),
        "no subprogram:\n{ll}"
    );
    assert!(
        ll.contains("DILocation(line: 6"),
        "no location for line 6:\n{ll}"
    );
    assert!(
        ll.contains("!dbg !"),
        "instructions carry no location:\n{ll}"
    );

    // And the program still runs.
    let run = Command::new(&exe).output().expect("runs");
    assert_eq!(String::from_utf8_lossy(&run.stdout), "40\n");
}

#[test]
fn a_record_is_described_by_its_fields() {
    // A record is held by pointer, so a debugger needs both the struct type and
    // a pointer to it before it can print `{W = 3, H = 4}` instead of an address.
    let path = tmp("recdbg.kiln");
    std::fs::write(
        &path,
        "namespace RecDbg;\npublic record Rect(int W, int H);\npublic static class P\n{\n    static int Area(Rect r)\n    {\n        var a = r.W * r.H;\n        return a;\n    }\n    public static void Main()\n    {\n        Console.WriteLine($\"{Area(new Rect(3, 4))}\");\n    }\n}\n",
    )
    .unwrap();
    let ir = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["k2", path.to_str().unwrap(), "--emit-ir"])
        .output()
        .expect("kiln k2 --emit-ir");
    let ll = String::from_utf8_lossy(&ir.stdout);
    assert!(
        ll.contains("DICompositeType(tag: DW_TAG_structure_type, name: \"Rect\""),
        "no struct type:\n{ll}"
    );
    for field in ["name: \"W\"", "name: \"H\""] {
        assert!(
            ll.contains(&format!("DW_TAG_member, {field}")),
            "no member {field}:\n{ll}"
        );
    }
    // The second field sits one int along.
    assert!(ll.contains("offset: 32"), "member offsets are wrong:\n{ll}");
    assert!(
        ll.contains("DW_TAG_pointer_type"),
        "a record is held by pointer:\n{ll}"
    );
}

#[test]
fn console_write_interleaves_with_the_runtimes_own_printing() {
    // `print_text` writes a whole line, so `Console.Write` cannot use it. The
    // runtime prints through libc on the same stream, so a `%s` with no newline
    // lands in the right place between two `WriteLine`s — which is the only
    // thing that could go wrong here.
    let out = build_and_run(
        "write",
        "namespace Wr;\npublic static class P\n{\n    public static void Main()\n    {\n        Console.WriteLine(\"start\");\n        foreach (var i in 1..3)\n            Console.Write($\"{i} \");\n        Console.WriteLine(\"end\");\n        Console.Write(\"a\");\n        Console.Write(\"b\");\n        Console.WriteLine(\"\");\n    }\n}\n",
    );
    assert_eq!(out, "start\n1 2 3 end\nab\n", "{out}");
}

#[test]
fn a_table_record_inserts_and_queries() {
    // Phase 7's point: the statement is built while compiling — no reflection
    // reaches the binary — and then actually runs. A captured value is bound as
    // a parameter, never pasted into the SQL, and each column is read back with
    // the reader its declared type asks for.
    let src = "\
namespace DbRt;
using Kiln.Db;

[Table(\"items\")]
public record Item(
    [Auto] long Id,
    int OwnerId,
    string Name);

public static class P
{
    public static void Main()
    {
        var h = Db.Open(\"sqlite::memory:\");
        Db.Exec(h, \"create table items (id integer primary key, owner_id int, name text)\", []);

        Item.Insert(h, new Item(0, 7, \"sword\"));
        Item.Insert(h, new Item(0, 7, \"shield\"));
        Item.Insert(h, new Item(0, 9, \"hat\"));

        var wanted = 7;
        var mine = Item.Select(h, i => i.OwnerId == wanted);
        Console.WriteLine($\"{mine.Count} rows\");
        foreach (var it in mine)
            Console.WriteLine($\"{it.Id} {it.OwnerId} {it.Name}\");

        // No predicate: every row.
        Console.WriteLine($\"{Item.Select(h).Count} total\");
    }
}
";
    let out = build_and_run("table", src);
    assert_eq!(
        out, "2 rows\n1 7 sword\n2 7 shield\n3 total\n",
        "{out}"
    );
}

#[test]
fn a_quoted_value_is_bound_not_pasted() {
    // The reason a query is parameterised at all: a value that looks like SQL
    // is data. Pasted in, this ends the statement and drops the table.
    let src = "\
namespace DbInj;
using Kiln.Db;

[Table(\"notes\")]
public record Note(int Id, string Body);

public static class P
{
    public static void Main()
    {
        var h = Db.Open(\"sqlite::memory:\");
        Db.Exec(h, \"create table notes (id int, body text)\", []);
        Db.Exec(h, \"insert into notes values (1, 'safe')\", []);
        Db.Exec(h, \"insert into notes values (2, ?)\", [\"'; drop table notes; --\"]);

        var nasty = \"'; drop table notes; --\";
        var found = Note.Select(h, n => n.Body == nasty);
        Console.WriteLine($\"{found.Count} matched\");
        Console.WriteLine($\"{Note.Select(h).Count} rows survive\");
    }
}
";
    assert_eq!(build_and_run("inject", src), "1 matched\n2 rows survive\n");
}

/// Build a K2 source that is expected NOT to compile, and return the message.
fn build_error(name: &str, src: &str) -> String {
    let path = tmp(&format!("{name}.kiln"));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["k2", path.to_str().unwrap(), "--runtime", "-o", &tmp(name).to_string_lossy()])
        .output()
        .expect("kiln k2");
    assert!(
        !out.status.success(),
        "this was expected not to compile, but it did"
    );
    String::from_utf8_lossy(&out.stderr).to_string()
}

#[test]
fn a_wrong_argument_to_a_command_is_reported_not_crashed() {
    // A command's parameters are ABI tags, not Kiln types, and nothing used to
    // compare the two. A `List<T>` where an array is declared reached the
    // library as a record pointer, was read as an array, and segfaulted inside
    // it — with nothing in the message to say which argument was wrong.
    // A `List<string>` where a list of text belongs is converted, so the wrong
    // argument here is one that cannot be: a list of numbers.
    let src = "\
namespace Mism;
using Kiln.Db;
public static class P
{
    public static void Main()
    {
        var h = Db.Open(\"sqlite::memory:\");
        var ns = new List<int>();
        ns.Add(1);
        Db.Exec(h, \"insert into t values (?)\", ns);
    }
}
";
    let err = build_error("mismatch", src);
    assert!(err.contains("Db.Exec"), "the command is not named: {err}");
    assert!(err.contains("argument 3"), "the position is not named: {err}");
    assert!(
        err.contains("a list of text") && err.contains("a list of a whole number"),
        "expected and got are not both named: {err}"
    );
}

#[test]
fn a_list_converts_to_a_command_list_and_back() {
    // `List<T>` is the only list a K2 program sees, and a command's list is a
    // runtime array. One conversion each way at the boundary — which is what
    // crossing an ABI costs — rather than two list types in the language.
    let src = "\
namespace Conv;
using Kiln.Db;
using Kiln.Text;
public static class P
{
    public static void Main()
    {
        // Out: a List<string> where the command declares a list of text.
        var h = Db.Open(\"sqlite::memory:\");
        Db.Exec(h, \"create table t (n text)\", []);
        var ps = new List<string>();
        ps.Add(\"first\");
        Console.WriteLine($\"{Db.Exec(h, \"insert into t values (?)\", ps)}\");

        // Back: a command that answers with a list gives a List<string>, so it
        // counts and iterates like any other.
        var parts = Text.Split(\"a,b,c\", \",\");
        Console.WriteLine($\"{parts.Count}\");
        foreach (var p in parts)
            Console.Write($\"{p} \");
        Console.WriteLine(\"\");

        // And the round trip: what came back can go out again.
        Console.WriteLine($\"{Db.Exec(h, \"insert into t values (?)\", Text.Split(\"solo\", \",\"))}\");
    }
}
";
    assert_eq!(
        build_and_run("convert", src),
        "1\n3\na b c \n1\n",
    );
}
