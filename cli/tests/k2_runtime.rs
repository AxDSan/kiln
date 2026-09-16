//! Kiln 2 against the real runtime: `kiln k2 --runtime` links the runtime and
//! the collector, so printing goes through the slot ABI and what a program
//! allocates is reclaimed.

use std::path::PathBuf;
use std::process::Command;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join("k2-rt");
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
fn a_collection_held_in_a_module_variable_survives_a_collection() {
    // A module variable is the one place a value is reachable from nowhere
    // else: no stack frame holds it between the methods that touch it, so the
    // collector only keeps it if the program hands over its address — the root
    // table. Without it the list below is swept and what is read afterwards is
    // a freed block.
    //
    // The deep recursion is the point of the test rather than decoration: the
    // collector also scans the stack conservatively, and a stale copy of the
    // pointer left in a frame or a callee-saved register would keep the list
    // alive by luck and hide the bug. Recursing afterwards overwrites that
    // region, so what survives survives because it was rooted.
    let src = "\
namespace RootGc;
public static class P
{
    [Dll(\"runtime\", Entry = \"kn_gc_collect\")]
    public static extern long Collect();

    static List<int> kept;

    static void Fill()
    {
        var xs = new List<int>();
        foreach (var i in 1..200)
            xs.Add(i * 3);
        kept = xs;
    }

    static long Churn(int depth)
    {
        var junk = new List<int>();
        foreach (var j in 1..400)
            junk.Add(j);
        if (depth > 1)
            return Churn(depth - 1) + junk.Count;
        return junk.Count;
    }

    public static void Main()
    {
        Fill();

        // Overwrite whatever the stack still held from `Fill`, then collect.
        Churn(80);
        Collect();
        Collect();

        // And again, so no copy from before the collection can be read back.
        Churn(80);
        Collect();

        Console.WriteLine($\"{kept.Count} {kept[1]} {kept[200]}\");
    }
}
";
    let out = build_and_run("rootgc", src);
    assert_eq!(
        out, "200 3 600\n",
        "a collection held in a module variable did not survive collection:\n{out}"
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
    assert_eq!(out, "written by Kiln 2\nexists: true\nKILN TWO\n", "{out}");
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
    assert_eq!(out, "ok=true value=here\nok=false\n(missing)\n", "{out}");
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
    assert_eq!(build_and_run("try", &src), "true AB\nfalse\n");
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

#[test]
fn every_call_carries_a_location_so_debug_info_survives() {
    // LLVM discards a module's debug information *entirely* if a call inside a
    // function that has some carries no `!dbg`. So one untagged instruction —
    // a prologue, an initialiser emitted before the first line marker — costs
    // the whole program its debuggability, silently. The only symptom is a
    // warning from clang that a build otherwise ignores.
    let path = tmp("dbgall.kiln");
    std::fs::write(
        &path,
        "namespace DbgAll;\npublic static class P\n{\n    static int Inner() => 7;\n    static int Outer() { return Inner() + 1; }\n    public static void Main()\n    {\n        Console.WriteLine($\"{Outer()}\");\n    }\n}\n",
    )
    .unwrap();
    let ir = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["k2", path.to_str().unwrap(), "--emit-ir"])
        .output()
        .expect("kiln k2 --emit-ir");
    let ll = String::from_utf8_lossy(&ir.stdout);

    // Every `call` in a function that has a subprogram must carry a location.
    let mut untagged = Vec::new();
    for line in ll.lines() {
        let t = line.trim();
        if t.contains(" call ") && !t.contains("!dbg") && !t.starts_with("declare") {
            untagged.push(t.to_string());
        }
    }
    assert!(
        untagged.is_empty(),
        "these calls carry no location, which costs the module its debug info:\n{}",
        untagged.join("\n")
    );

    // And clang agrees: no complaint about invalid debug info.
    let ll_path = tmp("dbgall.ll");
    std::fs::write(&ll_path, ll.as_bytes()).unwrap();
    let cc = Command::new("clang")
        .args(["-c", ll_path.to_str().unwrap(), "-o", "/dev/null"])
        .output()
        .expect("clang");
    let err = String::from_utf8_lossy(&cc.stderr);
    assert!(
        !err.contains("invalid debug info"),
        "clang discarded the debug info:\n{err}"
    );
}

#[test]
fn a_held_handler_environment_survives_a_collection() {
    // The whole reason `kn_handler_hold` exists. A handler's environment is
    // allocated by the collector and then stored in a UI library's own tables,
    // which are C++ memory the collector does not scan — so nothing on the
    // Kiln side refers to it and the next collection frees it. The next click
    // then calls a function whose captured variables are gone.
    //
    // Held, it survives: the table is collector-allocated and rooted, and the
    // collector traces a marked block's contents. Proved by reading the
    // environment's own bytes back afterwards, not merely by finding a
    // non-null pointer where one was left.
    let src = "\
namespace HoldGc;

[Packed]
public record Captured(int Row, int Flags);

public static class P
{
    [Dll(\"runtime\", Entry = \"kn_handler_hold\")]
    public static extern int Hold(Bytes env);

    [Dll(\"runtime\", Entry = \"kn_handler_release\")]
    public static extern void Release(int token);

    [Dll(\"runtime\", Entry = \"kn_handler_count\")]
    public static extern int Held();

    [Dll(\"runtime\", Entry = \"kn_gc_collect\")]
    public static extern long Collect();

    public static void Main()
    {
        // An environment the collector owns, holding real values, reachable
        // from nowhere on the Kiln side once this method stops naming it.
        var env = Bytes.Alloc(64);
        var c = new Captured(7, 42);
        c.Write(env, 0);
        var token = Hold(env);
        Console.WriteLine($\"held {Held()}\");

        // Enough allocation to force real collections.
        foreach (var i in 1..200)
        {
            var junk = new List<int>();
            foreach (var j in 1..200)
                junk.Add(j);
        }
        Collect();
        Collect();

        // The bytes are still what was written into them.
        var back = Captured.Read(env, 0);
        Console.WriteLine($\"{back.Row} {back.Flags}\");

        Release(token);
        Console.WriteLine($\"held {Held()}\");
    }
}
";
    let out = build_and_run("holdgc", src);
    assert_eq!(
        out, "held 1\n7 42\nheld 0\n",
        "a held environment did not survive collection:\n{out}"
    );
}

/// Build a K2 form program and run it headless, clicking the given widget
/// handles in order. Returns stdout.
fn run_form_clicks(name: &str, src: &str, clicks: &str) -> String {
    let path = tmp(&format!("{name}.kiln"));
    let exe = tmp(name);
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["k2", path.to_str().unwrap(), "--runtime", "-o", exe.to_str().unwrap()])
        .output()
        .expect("kiln k2");
    assert!(
        out.status.success(),
        "build failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&exe)
        .env("KILN_UI_SYNTH_CLICK", clicks)
        .env("KILN_UI_EXIT_AFTER_FRAMES", "2")
        .env("SDL_VIDEODRIVER", "offscreen")
        // Poison freed memory, so a handler reading a collected environment
        // reads garbage or crashes instead of the stale values still there.
        .env("MALLOC_PERTURB_", "165")
        .output()
        .expect("the form runs");
    assert!(
        run.status.success(),
        "the form exited {:?} — a crash here is a handler reaching freed memory:\n{}",
        run.status.code(),
        String::from_utf8_lossy(&run.stdout)
    );
    String::from_utf8_lossy(&run.stdout)
        .lines()
        .filter(|l| !l.starts_with("Loaded font") && !l.contains("a11y"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn an_extern_in_a_form_calls_the_function() {
    // A `[Dll]` extern declared in a form compiled to an empty body returning
    // 0 — the call to C silently never happened. Here it has to report a real
    // collection over a heap that has real bytes in it.
    let src = "\
namespace FormDll;

public partial form MainWindow
{
    Title = \"x\";
    Button go { Text = \"Go\"; Click += OnGo; }
}

public partial form MainWindow
{
    [Dll(\"runtime\", Entry = \"kn_gc_live_bytes\")]
    static extern long Live();

    void OnGo()
    {
        var xs = new List<string>();
        foreach (var i in 1..200)
            xs.Add($\"item {i}\");
        Console.WriteLine($\"{Live() > 0}\");
    }
}
";
    // Handles: the form is 1, `go` is 2.
    assert_eq!(run_form_clicks("formdll", src, "2"), "true");
}

#[test]
fn a_handler_wired_at_run_time_keeps_its_environment_through_a_collection() {
    // ABI v5's reason to exist. Three buttons are wired in a loop, each
    // capturing its own row; a separate click then churns the heap and forces
    // a collection, when nothing on the stack refers to those environments any
    // more. Only the runtime's handler table keeps them alive.
    //
    // This test was checked against a build with the hold removed: that build
    // segfaults on the first row click. A version with the collection inside
    // the wiring method passed either way, because the environments were still
    // on the stack — which is why the churn is its own click.
    let src = ROWS_FIXTURE;
    // Handles: form 1, shown 2, r1 3, r2 4, r3 5, wire 6, churn 7.
    let out = run_form_clicks("rowsgc", src, "6;7;5;3;4");
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].starts_with("live before "), "{out}");
    let freed: i64 = lines[1]
        .trim_start_matches("collect freed ")
        .split(',')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert!(freed > 100_000, "the churn did not force a real collection:\n{out}");
    assert_eq!(&lines[2..], ["gamma #3", "alpha #1", "beta #2"], "{out}");
}

const ROWS_FIXTURE: &str = include_str!("fixtures/k2_rows_gc.kiln");

#[test]
fn a_method_wired_at_run_time_can_be_unwired() {
    // `+=` binds, `-=` unbinds by the same (function, environment) pair a
    // delegate compares — so removing `OnGo` leaves the lambda alone.
    let src = "\
namespace Unwire;

public partial form MainWindow
{
    Title = \"x\";
    Label shown { Text = \"none\"; }
    Button go { Text = \"Go\"; }
    Button wire { Text = \"Wire\"; Click += OnWire; }
    Button unwire { Text = \"Unwire\"; Click += OnUnwire; }
}

public partial form MainWindow
{
    void OnWire()
    {
        var row = 7;
        go.Click += () => { Console.WriteLine($\"lambda {row}\"); };
        go.Click += OnGo;
    }

    void OnGo() { Console.WriteLine(\"method\"); }

    void OnUnwire() { go.Click -= OnGo; }
}
";
    // Handles: form 1, shown 2, go 3, wire 4, unwire 5.
    // Wire, click go, unwire, click go.
    assert_eq!(
        run_form_clicks("unwire", src, "4;3;5;3"),
        "lambda 7\nmethod\nlambda 7"
    );
}

#[test]
fn kiln_build_builds_kiln_2_by_default() {
    // The flip: `kiln build` is how a Kiln 2 program is built, with the
    // runtime and its collector linked — not a separate `kiln k2` command. The
    // file says which language it is, by the same first-line rule the language
    // server and Studio use, so the build cannot disagree with the editor.
    let path = tmp("flip.kiln");
    let exe = tmp("flip");
    std::fs::write(
        &path,
        "namespace Flip;\npublic static class P\n{\n    [Dll(\"runtime\", Entry = \"kn_gc_live_bytes\")]\n    static extern long Live();\n    public static void Main()\n    {\n        var xs = new List<string>();\n        xs.Add(\"collected\");\n        Console.WriteLine($\"{xs[1]} {Live() > 0}\");\n    }\n}\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", path.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .expect("kiln build");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!err.contains("deprecated"), "a K2 build was called 1.x:\n{err}");
    // The runtime is linked: allocation goes through the collector.
    let run = Command::new(&exe).output().expect("runs");
    assert_eq!(String::from_utf8_lossy(&run.stdout), "collected true\n");
}

#[test]
fn a_1x_build_is_deprecated_but_still_works() {
    // The 1.x engine stays, behind a note: a program that has not moved yet
    // still builds, and says how to move it. `--1x` builds it quietly.
    let path = tmp("old.kiln");
    let exe = tmp("old");
    std::fs::write(
        &path,
        "module old\ntarget console\n\nsub main\n  call print_text(\"still here\")\nend\n",
    )
    .unwrap();
    let noisy = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", path.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .expect("kiln build");
    assert!(noisy.status.success(), "{}", String::from_utf8_lossy(&noisy.stderr));
    let err = String::from_utf8_lossy(&noisy.stderr);
    assert!(err.contains("deprecated") && err.contains("kiln migrate"), "{err}");
    let run = Command::new(&exe).output().expect("runs");
    assert_eq!(String::from_utf8_lossy(&run.stdout), "still here\n");

    let quiet = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", "--1x", path.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .expect("kiln build --1x");
    assert!(quiet.status.success());
    assert!(
        !String::from_utf8_lossy(&quiet.stderr).contains("deprecated"),
        "--1x should build without the note"
    );

    // And `--1x` on a Kiln 2 file is a mistake worth naming.
    let k2 = tmp("notold.kiln");
    std::fs::write(&k2, "namespace N;\nConsole.WriteLine(\"x\");\n").unwrap();
    let wrong = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", "--1x", k2.to_str().unwrap()])
        .output()
        .expect("kiln build --1x on K2");
    assert!(!wrong.status.success());
    assert!(String::from_utf8_lossy(&wrong.stderr).contains("is a Kiln 2 program"));
}

#[test]
fn a_form_field_starts_with_the_value_it_was_given() {
    // `string label = "points";` in a form used to be dropped: the field began
    // as null, and the forms guide's own sample showed "7 (null)" on screen
    // while compiling cleanly — a docs gate that only compiles cannot see it.
    let src = "\
namespace Defaults;

public partial form MainWindow
{
    Title = \"x\";
    Button hit { Text = \"+7\"; Click += OnHit; }
}

public partial form MainWindow
{
    int points = 0;
    string label = \"points\";

    void OnHit()
    {
        points = points + 7;
        Console.WriteLine($\"{points} {label}\");
    }
}
";
    // Handles: form 1, hit 2.
    assert_eq!(run_form_clicks("formdefaults", src, "2"), "7 points");
}

#[test]
fn bytes_cross_the_command_boundary_both_ways() {
    // A `Bytes` is the runtime's byte-set, header and all. Kiln 2 used to treat
    // the pointer as the data: bytes a command returned were read from their
    // header, and bytes Kiln 2 made were handed to commands without one.
    let path = tmp("bytes-roundtrip.bin");
    let p = path.to_str().unwrap();
    let src = format!(
        "namespace BytesRt;\n\
         using Kiln.File;\n\
         public static class P\n\
         {{\n\
         \x20   public static void Main()\n\
         \x20   {{\n\
         \x20       var raw = Bytes.Alloc(3);\n\
         \x20       raw[0] = 65;\n\
         \x20       raw[1] = 0;\n\
         \x20       raw[2] = 66;\n\
         \x20       File.WriteBytes(\"{p}\", raw);\n\
         \x20       var back = File.ReadBytes(\"{p}\");\n\
         \x20       Console.WriteLine($\"{{back[0]}} {{back[1]}} {{back[2]}}\");\n\
         \x20   }}\n\
         }}\n"
    );
    assert_eq!(build_and_run("bytesrt", &src), "65 0 66\n");
    assert_eq!(std::fs::read(&path).unwrap(), b"A\0B");
}

#[test]
fn every_shipped_example_is_kiln_2_and_builds() {
    // The examples beside the toolchain are the first programs anyone runs.
    // Every one at the top of `examples/` must be Kiln 2 and must build with
    // `kiln build` — the 1.x originals live under `examples/1x/`.
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
    let mut failures = Vec::new();
    let mut seen = 0;
    for dir in ["examples", "examples/dll", "examples/k2"] {
        for entry in std::fs::read_dir(root.join(dir)).unwrap() {
            let p = entry.unwrap().path();
            if p.extension().and_then(|e| e.to_str()) != Some("kiln") {
                continue;
            }
            seen += 1;
            let src = std::fs::read_to_string(&p).unwrap();
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            if src.lines().any(|l| l.trim_start().starts_with("module ")) {
                failures.push(format!("{dir}/{name} is still a 1.x program"));
                continue;
            }
            let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
                .args(["k2", p.to_str().unwrap(), "--runtime", "--emit-ir"])
                .current_dir(&root)
                .output()
                .expect("kiln k2");
            if !out.status.success() {
                failures.push(format!(
                    "{dir}/{name}: {}",
                    String::from_utf8_lossy(&out.stderr)
                ));
            }
        }
    }
    assert!(seen >= 45, "expected the shipped examples, found {seen}");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn compound_assignment_on_a_property_updates_it_and_on_an_event_wires_it() {
    let src = "\
namespace PropPlus;

public partial form MainWindow
{
    Title = \"x\";
    Label note { Text = \"a\"; }
    Button hit { Text = \"go\"; }
}

public partial form MainWindow
{
    public static void Main()
    {
        hit.Click += OnHit;
    }

    void OnHit()
    {
        note.Text += \"b\";
        Console.WriteLine(note.Text);
    }
}
";
    // Handles: form 1, note 2, hit 3.
    assert_eq!(run_form_clicks("propplus", src, "3;3"), "ab\nabb");
}

#[test]
fn string_append_ternary_arms_chained_commands_and_division_by_zero() {
    let src = concat!(
        "using Kiln.Hello;\n",
        "namespace Fixes;\n",
        "public static class P\n",
        "{\n",
        "    public static void Main()\n",
        "    {\n",
        "        var s = \"x\";\n",
        "        s += \"y\";\n",
        "        var xs = new List<int>();\n",
        "        int v = xs.Count > 5 ? xs[99] : 7;\n",
        "        Console.WriteLine($\"{s} {v} {Greet(\"bob\").Uppercase().Lowercase()}\");\n",
        "        int zero = xs.Count;\n",
        "        Console.WriteLine($\"{10 / zero}\");\n",
        "    }\n",
        "}\n",
    );
    let exe = tmp("fixes-bin");
    let file = tmp("fixes.kiln");
    std::fs::write(&file, src).unwrap();
    let b = std::process::Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", file.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(b.status.success(), "{}", String::from_utf8_lossy(&b.stderr));
    let r = std::process::Command::new(&exe).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&r.stdout), "xy 7 hello, bob!\n");
    assert!(String::from_utf8_lossy(&r.stderr).contains("division by zero"));
    assert_eq!(r.status.code(), Some(1));
}

/// Phase 7's exit: the starter sketch's inventory — `Outfit` stocks a
/// character from the kit, `Inventory` reads the bag back — as a `[Table]`
/// record, run against SQLite always and against MariaDB when a server is named.
/// The same program answers the same way on both: the SQL is built while
/// compiling, the captured values are bound, and the columns come back through
/// the readers their declared types ask for.
const INVENTORY_FIXTURE: &str = include_str!("fixtures/k2_inventory.kiln.tmpl");
const INVENTORY_OUT: &str = "stocked 11 for 7, 11 for 9\n7 holds 11\n\
slot 2: template 4199 flags 50397441\n7 has 5 consumables\n22 rows, the last id 11\n";

#[test]
fn the_starter_inventory_runs_against_sqlite() {
    let src = INVENTORY_FIXTURE.replace("@DSN@", "sqlite::memory:").replace(
        "@CREATE@",
        "create table character_items (id integer primary key, character_id int, slot int, \
         template_id int, class_value int, flags int)",
    );
    assert_eq!(build_and_run("inventory_sqlite", &src), INVENTORY_OUT);
}

/// `KILN_TEST_MYSQL_DSN=mysql://user:pass@host:port/database` runs it against a
/// real MariaDB or MySQL; the table is dropped and made again. Unset, the test
/// says so and passes, because most machines have no server to give it.
#[test]
fn the_starter_inventory_runs_against_mariadb() {
    let Ok(dsn) = std::env::var("KILN_TEST_MYSQL_DSN") else {
        eprintln!("skipped: set KILN_TEST_MYSQL_DSN to run the inventory against MariaDB");
        return;
    };
    let src = INVENTORY_FIXTURE.replace("@DSN@", &dsn).replace(
        "@CREATE@",
        "create table character_items (id bigint auto_increment primary key, \
         character_id int unsigned, slot int, template_id int unsigned, \
         class_value int unsigned, flags int unsigned)",
    );
    assert_eq!(build_and_run("inventory_mariadb", &src), INVENTORY_OUT);
}

/// A Kiln 2 program spread over files: `using Accounts;` names `accounts.kiln`
/// beside the entry file, a unit may name further units, a library a unit asks
/// for is linked, and a `using` that names no file is left alone.
#[test]
fn a_program_is_assembled_from_the_units_it_names() {
    let dir = std::env::temp_dir().join("k2-units");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("main.kiln"),
        "namespace App;\nusing Accounts;\nusing Kiln.Text;\n\npublic static class Program\n{\n    public static void Main()\n    {\n        Console.WriteLine(Accounts.Greeting(\"ada\"));\n        Console.WriteLine($\"{Accounts.Total(new Account(\"grace\", 2))}\");\n    }\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("accounts.kiln"),
        "namespace Accounts;\nusing Wording;\n\npublic record Account(string Name, int Logins);\n\npublic static class Accounts\n{\n    public static string Greeting(string who) => Wording.Hello(who);\n    public static int Total(Account a) => a.Logins + 40;\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("wording.kiln"),
        "namespace Wording;\nusing Kiln.Text;\n\npublic static class Wording\n{\n    public static string Hello(string who) => $\"hello, {Uppercase(who)}\";\n}\n",
    )
    .unwrap();
    let exe = dir.join("app");
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", dir.join("main.kiln").to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .expect("kiln build");
    assert!(out.status.success(), "build failed:\n{}", String::from_utf8_lossy(&out.stderr));
    let run = Command::new(&exe).output().expect("the program runs");
    assert_eq!(String::from_utf8_lossy(&run.stdout), "hello, ADA\n42\n");
}

/// `kiln migrate` on a 1.x unit program: a `use` naming a file beside it stays
/// the unit's own name, a c-record's `byte[32]` stays thirty-two bytes in place,
/// and an empty `{}` module variable is created with its declared value type.
#[test]
fn migrate_keeps_units_inline_arrays_and_dictionary_types() {
    let dir = std::env::temp_dir().join("k2-migrate");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("helpers.kiln"), "unit helpers\n\nsub twice(n: int): int\n  return n * 2\nend\n").unwrap();
    let main = dir.join("main.kiln");
    std::fs::write(
        &main,
        "module main\nuse helpers\nuse text\n\nrecord frame is c\n  id: byte\n  name: byte[32]\n  port: int\nend\n\nrecord conn\n  n: int\nend\n\nvar conns: conn{} = {}\n\nsub main\n  call print_text(\"{twice(2)}\")\nend\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["migrate", main.to_str().unwrap()])
        .output()
        .expect("kiln migrate");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(text.contains("using Helpers;"), "a unit became a library:\n{text}");
    assert!(text.contains("using Kiln.Text;"), "a library lost its prefix:\n{text}");
    assert!(text.contains("byte[32] Name"), "the inline array became a pointer:\n{text}");
    assert!(
        text.contains("Dictionary<string, Conn> conns = new Dictionary<string, Conn>()"),
        "the empty dictionary took the wrong value type:\n{text}"
    );
}

/// Phase 6: a handler wired at run time takes what the event hands it. A grid's
/// `select` gives its row to a lambda and to a method alike; `-=` finds the
/// method again, because it is bound through one remembered thunk; `activate`
/// gives its row too. Handles: form 1, grid 2, wire 3, unwire 4; row N of the
/// grid is `2.1.N`.
#[test]
fn a_handler_wired_at_run_time_takes_the_grid_row() {
    let src = include_str!("fixtures/k2_grid_env.kiln");
    assert_eq!(
        run_form_clicks("gridenv", src, "3;2.1.2;4;2.1.3"),
        "lambda select 2\nmethod select Linus\nlambda select 3"
    );
}
