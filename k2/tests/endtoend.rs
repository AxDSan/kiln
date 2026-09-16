//! End-to-end: K2 source → KIR → LLVM → clang → run, asserting stdout.
//! This is the proof that the K2 front end and the KIR backend meet.

use std::process::Command as Proc;

fn run_k2(src: &str) -> String {
    let ll = kiln_k2::compile_to_llvm(src).unwrap_or_else(|e| panic!("compile failed: {e}"));
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let uniq = N.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("k2-e2e-{}-{uniq}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ll_path = dir.join("m.ll");
    let exe = dir.join("m");
    std::fs::write(&ll_path, &ll).unwrap();
    let shim = dir.join("shim.c");
    std::fs::write(
        &shim,
        "extern int ECodeStart(void); int main(void){return ECodeStart();}\n",
    )
    .unwrap();
    let out = Proc::new("clang")
        .args([
            ll_path.to_str().unwrap(),
            shim.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("clang");
    assert!(
        out.status.success(),
        "clang failed:\n{}\n--- IR ---\n{ll}",
        String::from_utf8_lossy(&out.stderr)
    );
    let r = Proc::new(&exe).output().expect("run");
    String::from_utf8(r.stdout).unwrap()
}

#[test]
fn hello_world() {
    let src = r#"
namespace Hello;
Console.WriteLine("Hello from K2!");
"#;
    assert_eq!(run_k2(src), "Hello from K2!\n");
}

#[test]
fn arithmetic_and_interpolation() {
    let src = r#"
namespace Calc;
var a = 6;
var b = 7;
Console.WriteLine($"{a} * {b} = {a * b}");
"#;
    assert_eq!(run_k2(src), "6 * 7 = 42\n");
}

#[test]
fn a_static_method_and_if() {
    let src = r#"
namespace M;

public static class Program
{
    public static int Max(int a, int b) => a >= b ? a : b;

    public static void Main()
    {
        Console.WriteLine($"max = {Max(3, 9)}");
        if (Max(3, 9) == 9)
            Console.WriteLine("ok");
    }
}
"#;
    assert_eq!(run_k2(src), "max = 9\nok\n");
}

#[test]
fn fizzbuzz() {
    let src = r#"
namespace FizzBuzz;
foreach (var i in 1..15)
{
    if (i % 15 == 0)
        Console.WriteLine("FizzBuzz");
    else if (i % 3 == 0)
        Console.WriteLine("Fizz");
    else if (i % 5 == 0)
        Console.WriteLine("Buzz");
    else
        Console.WriteLine($"{i}");
}
"#;
    let expected = "1\n2\nFizz\n4\nBuzz\nFizz\n7\n8\nFizz\nBuzz\n11\nFizz\n13\n14\nFizzBuzz\n";
    assert_eq!(run_k2(src), expected);
}

#[test]
fn a_record_with_a_method() {
    let src = r#"
namespace Geo;

public record Rect(int W, int H)
{
    public int Area() => W * H;
}

public static class Program
{
    public static void Main()
    {
        var r = new Rect(3, 4);
        Console.WriteLine($"area = {r.Area()}");
    }
}
"#;
    assert_eq!(run_k2(src), "area = 12\n");
}

#[test]
fn unsigned_and_while() {
    let src = r#"
namespace U;
uint x = 4000000000;
var count = 0;
while (x > 1000000000)
{
    x = x / 2;
    count = count + 1;
}
Console.WriteLine($"{count}");
"#;
    // 4e9 -> 2e9 -> 1e9 (stop when not > 1e9): 4000000000/2=2000000000 (>1e9),
    // /2=1000000000 (not >1e9) → 2 iterations. Unsigned matters: 4e9 > 2^31.
    assert_eq!(run_k2(src), "2\n");
}

#[test]
fn generic_method_monomorphises() {
    let src = r#"
namespace G;

public static class Program
{
    public static T Max<T>(T a, T b) where T : IComparable<T> => a >= b ? a : b;

    public static void Main()
    {
        Console.WriteLine($"{Max(3, 9)}");
        Console.WriteLine($"{Max(2.5, 1.5)}");
        Console.WriteLine($"{Max(10, 4)}");
    }
}
"#;
    // int and double instances; the two int calls share one instantiation.
    assert_eq!(run_k2(src), "9\n2.5\n10\n");
}

#[test]
fn generic_instances_get_distinct_symbols() {
    let src = r#"
namespace G2;
public static class P
{
    public static T Id<T>(T x) => x;
    public static void Main()
    {
        Console.WriteLine($"{Id(7)}");
        Console.WriteLine($"{Id(1.5)}");
    }
}
"#;
    let ll = kiln_k2::compile_to_llvm(src).unwrap();
    let n = ll.matches("define internal").filter(|_| true).count();
    assert!(n >= 3, "expected Main + two Id instances, got {n}:\n{ll}");
    assert!(
        ll.contains("@P_Id$"),
        "mangled instance symbols missing:\n{ll}"
    );
    assert_eq!(run_k2(src), "7\n1.5\n");
}

#[test]
fn lambdas_are_first_class() {
    let src = r#"
namespace L;
public static class P
{
    public static int Apply(Func<int, int> f, int x) => f(x);

    public static void Main()
    {
        Func<int, int> twice = x => x * 2;
        Console.WriteLine($"{twice(21)}");
        Console.WriteLine($"{Apply(n => n + 1, 41)}");

        Func<int, int, int> add = (a, b) => a + b;
        Console.WriteLine($"{add(20, 22)}");

        Action<int> shout = n => { Console.WriteLine($"n={n}"); };
        shout(7);
    }
}
"#;
    assert_eq!(run_k2(src), "42\n42\n42\nn=7\n");
}

#[test]
fn a_loop_variable_is_captured_like_any_other() {
    // A lambda made inside a loop and called inside the same turn reads that
    // turn's value — the everyday case, and the one that used to be refused.
    let src = r#"
namespace C;
public static class P
{
    public static void Main()
    {
        foreach (var i in 1..3)
        {
            Func<int, int> f = x => x + i;
            Console.WriteLine($"{f(1)}");
        }
    }
}
"#;
    assert_eq!(run_k2(src), "2\n3\n4\n");
}

#[test]
fn generics_and_lambdas_work_in_top_level_code() {
    // Regression: instance bodies are queued during lowering and drained at the
    // end — top-level code must be drained too, or the instance is emitted empty.
    let src = r#"
namespace T;
public static class P
{
    public static T Pick<T>(T a, T b) => a;
}
Func<int, int> triple = x => x * 3;
Console.WriteLine($"{triple(14)}");
Console.WriteLine($"{P.Pick(5, 9)}");
"#;
    assert_eq!(run_k2(src), "42\n5\n");
}

#[test]
fn capturing_lambda_sees_later_mutation() {
    // Capture is BY REFERENCE (spec §8): the closure must observe a write made
    // after it was created, and its own writes must be visible outside.
    let src = r#"
namespace Cap;
public static class P
{
    public static void Main()
    {
        var n = 10;
        Func<int, int> addN = x => x + n;
        n = 32;
        Console.WriteLine($"{addN(10)}");

        var count = 0;
        Action<int> bump = k => { count = count + k; };
        bump(7);
        bump(35);
        Console.WriteLine($"{count}");
    }
}
"#;
    assert_eq!(run_k2(src), "42\n42\n");
}

#[test]
fn capturing_lambda_captures_a_parameter() {
    let src = r#"
namespace Cap2;
public static class P
{
    public static int Make(int seed)
    {
        Func<int, int> f = x => x * seed;
        seed = 6;
        return f(7);
    }
    public static void Main()
    {
        Console.WriteLine($"{Make(2)}");
    }
}
"#;
    // `seed` is captured by reference, so the call sees 6, not 2.
    assert_eq!(run_k2(src), "42\n");
}

#[test]
fn result_propagates_and_falls_back() {
    let src = r#"
namespace R;
public static class P
{
    public static Result<int> Half(int n)
    {
        if (n % 2 != 0) return Error("not even");
        return n / 2;
    }

    // `?` propagates a failure out of this method.
    public static Result<int> QuarterOf(int n)
    {
        var half = Half(n)?;
        var quarter = Half(half)?;
        return quarter;
    }

    public static void Main()
    {
        Console.WriteLine($"{Half(84).Value}");
        Console.WriteLine($"{Half(7).IsOk}");
        Console.WriteLine($"{Half(7).Error}");

        // `??` supplies the value a failure did not.
        Console.WriteLine($"{Half(7) ?? -1}");
        Console.WriteLine($"{Half(10) ?? -1}");

        var q = QuarterOf(40);
        Console.WriteLine($"{q.Value}");
        var bad = QuarterOf(6);
        Console.WriteLine($"{bad.IsOk} {bad.Error}");
    }
}
"#;
    // 84/2=42; 7 is odd -> failure; 40 -> 20 -> 10; 6 -> 3 -> odd, propagated.
    assert_eq!(run_k2(src), "42\nfalse\nnot even\n-1\n5\n10\nfalse not even\n");
}

#[test]
fn defer_runs_on_every_exit() {
    let src = r#"
namespace D;
public static class P
{
    public static int Attempt(int n)
    {
        Console.WriteLine("open");
        defer Console.WriteLine("close");
        if (n == 1)
            return 10;
        Console.WriteLine("work");
        return 20;
    }

    public static void Main()
    {
        Console.WriteLine($"{Attempt(1)}");
        Console.WriteLine($"{Attempt(2)}");

        // Several defers in one block unwind in reverse order.
        Console.WriteLine("a");
        defer Console.WriteLine("first-declared-runs-last");
        defer Console.WriteLine("last-declared-runs-first");
        Console.WriteLine("b");
    }
}
"#;
    assert_eq!(
        run_k2(src),
        "open\nclose\n10\nopen\nwork\nclose\n20\na\nb\nlast-declared-runs-first\nfirst-declared-runs-last\n"
    );
}

#[test]
fn defer_runs_when_a_loop_body_is_left() {
    let src = r#"
namespace D2;
public static class P
{
    public static void Main()
    {
        foreach (var i in 1..3)
        {
            defer Console.WriteLine($"leave {i}");
            if (i == 2)
                continue;
            Console.WriteLine($"body {i}");
        }
    }
}
"#;
    assert_eq!(run_k2(src), "body 1\nleave 1\nleave 2\nbody 3\nleave 3\n");
}

#[test]
fn continue_advances_a_counted_loop() {
    // Regression: `continue` jumps to the loop top, so the step has to run on
    // that edge too — otherwise the loop never advances and spins forever.
    let src = r#"
namespace K;
public static class P
{
    public static void Main()
    {
        foreach (var i in 1..5)
        {
            if (i % 2 == 0)
                continue;
            Console.WriteLine($"{i}");
        }
        for (var j = 1; j <= 5; j++)
        {
            if (j == 3)
                continue;
            Console.WriteLine($"j{j}");
        }
    }
}
"#;
    assert_eq!(run_k2(src), "1\n3\n5\nj1\nj2\nj4\nj5\n");
}

#[test]
fn optionals_narrow_on_a_null_check() {
    let src = r#"
namespace O;
public static class P
{
    public static int? Halve(int n)
    {
        if (n % 2 != 0)
            return null;
        return n / 2;
    }

    public static void Main()
    {
        var a = Halve(84);
        if (a != null)
            Console.WriteLine($"{a}");          // narrowed: reads as int

        var b = Halve(7);
        if (b == null)
            Console.WriteLine("no value");

        Console.WriteLine($"{Halve(7) ?? -1}");  // fallback
        Console.WriteLine($"{Halve(10) ?? -1}");
        Console.WriteLine($"{Halve(9).HasValue}");
    }
}
"#;
    assert_eq!(run_k2(src), "42\nno value\n-1\n5\nfalse\n");
}

#[test]
fn a_ternary_deals_with_an_optional_the_way_an_if_does() {
    // Two halves of the same rule. An arm that runs when a null test proved the
    // value present reads it as its plain type; and when the arms differ only
    // in optionality the result is the optional, with the plain arm wrapped.
    // Without the first, `s == null ? "none" : s` stored the `{value, present}`
    // pair into a slot typed from the other arm; without the second,
    // `f() == null ? "none" : f()` did the same, since a call is not a name the
    // proof can follow. Both are valid Kiln that clang refused at the IR.
    let src = r#"
namespace TN;
public static class P
{
    public static string? Name(int n)
    {
        if (n == 1)
            return "one";
        return null;
    }

    public static void Main()
    {
        var s = Name(1);
        Console.WriteLine(s == null ? "none" : s);
        Console.WriteLine(Name(2) == null ? "none" : Name(2));

        var t = Name(2) == null ? "none" : Name(2);
        Console.WriteLine(t == null ? "gone" : "here");
    }
}
"#;
    assert_eq!(run_k2(src), "one\nnone\nhere\n");
}

#[test]
fn switch_expressions() {
    let src = r#"
namespace S;

public enum Profession { Warrior, Champion, Mage, Priest }

public static class P
{
    public static uint WeaponFor(int p) => p switch
    {
        0 => 1000,
        1 => 1400,
        2 => 1700,
        _ => 1800,
    };

    public static string Size(int n) => n switch
    {
        <= 0 => "none",
        < 10 => "small",
        < 100 => "medium",
        _ => "large",
    };

    public static void Main()
    {
        Console.WriteLine($"{WeaponFor(2)}");
        Console.WriteLine($"{WeaponFor(9)}");
        Console.WriteLine(Size(0));
        Console.WriteLine(Size(5));
        Console.WriteLine(Size(50));
        Console.WriteLine(Size(500));
    }
}
"#;
    assert_eq!(run_k2(src), "1700\n1800\nnone\nsmall\nmedium\nlarge\n");
}

#[test]
fn strings_build_as_values() {
    let src = r#"
namespace Str;
public static class P
{
    public static string Describe(string name, int n) => $"{name} has {n}";

    public static void Main()
    {
        string greeting = $"Hello, {"world"}!";
        Console.WriteLine(greeting);

        var s = Describe("kiln", 42);
        Console.WriteLine(s);

        string a = "con";
        string b = "cat";
        Console.WriteLine(a + b);

        // a value built once and used twice
        var twice = $"[{s}]";
        Console.WriteLine(twice);
        Console.WriteLine(twice);
    }
}
"#;
    assert_eq!(
        run_k2(src),
        "Hello, world!\nkiln has 42\nconcat\n[kiln has 42]\n[kiln has 42]\n"
    );
}

#[test]
fn lists_grow_and_iterate() {
    let src = r#"
namespace L2;
public static class P
{
    public static void Main()
    {
        var xs = new List<int>();
        foreach (var i in 1..10)
            xs.Add(i * i);

        Console.WriteLine($"count {xs.Count}");
        Console.WriteLine($"first {xs[1]} last {xs[xs.Count]}");

        var total = 0;
        foreach (var x in xs)
            total = total + x;
        Console.WriteLine($"sum {total}");

        var names = new List<string>();
        names.Add("ada");
        names.Add("grace");
        foreach (var n in names)
            Console.WriteLine(n);
    }
}
"#;
    // squares 1..100 sum to 385; growth crosses 4 -> 8 -> 16.
    assert_eq!(
        run_k2(src),
        "count 10\nfirst 1 last 100\nsum 385\nada\ngrace\n"
    );
}

#[test]
fn where_and_select_over_a_list() {
    let src = r#"
namespace Q;
public static class P
{
    public static void Main()
    {
        var xs = new List<int>();
        foreach (var i in 1..10)
            xs.Add(i);

        var evens = xs.Where(x => x % 2 == 0);
        Console.WriteLine($"{evens.Count}");
        foreach (var e in evens)
            Console.Write($"{e} ");
        Console.WriteLine("");

        var squares = xs.Select(x => x * x);
        Console.WriteLine($"{squares[1]} {squares[10]}");

        // chained, and a Select that changes the element type
        var labels = xs.Where(x => x > 8).Select(x => $"n{x}");
        foreach (var l in labels)
            Console.WriteLine(l);
    }
}
"#;
    assert_eq!(run_k2(src), "5\n2 4 6 8 10 \n1 100\nn9\nn10\n");
}

#[test]
fn order_by_over_a_list() {
    let src = r#"
namespace QO;
public record Point(int X, int Y);
public static class P
{
    public static void Main()
    {
        var xs = new List<int>();
        xs.Add(3);
        xs.Add(1);
        xs.Add(2);
        xs.Add(2);

        foreach (var n in xs.OrderBy(x => x))
            Console.Write($"{n} ");
        Console.WriteLine("");

        // The source is a copy, so ordering it leaves the original alone —
        // where Sort is in place.
        foreach (var n in xs)
            Console.Write($"{n} ");
        Console.WriteLine("");

        // A key that is not the element, and a chained Where after it.
        var desc = xs.OrderBy(x => -x).Where(x => x > 1);
        Console.WriteLine($"{desc[1]} {desc[3]}");

        // Strings order by content.
        var names = new List<string>();
        names.Add("grace");
        names.Add("ada");
        names.Add("bob");
        foreach (var n in names.OrderBy(s => s))
            Console.Write($"{n} ");
        Console.WriteLine("");

        // Records order by a selected field, whatever the element is.
        var pts = new List<Point>();
        pts.Add(new Point(3, 9));
        pts.Add(new Point(1, 8));
        pts.Add(new Point(2, 7));
        foreach (var p in pts.OrderBy(q => q.X))
            Console.Write($"{p.X}:{p.Y} ");
        Console.WriteLine("");
    }
}
"#;
    assert_eq!(
        run_k2(src),
        "1 2 2 3 \n3 1 2 2 \n3 2\nada bob grace \n1:8 2:7 3:9 \n"
    );
}

#[test]
fn the_starter_kit_example_compiles_and_runs() {
    // examples/k2/starter.kiln — the program that motivated K2. Kept as a test
    // so the features it leans on (records, List, Select, switch over an enum,
    // hex literals, unsigned fields) cannot regress together.
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("examples/k2/starter.kiln"),
    )
    .expect("examples/k2/starter.kiln");
    assert_eq!(
        run_k2(&src),
        "13 items for character 7\n\
         slot 0: template 10157 flags 16843015\n\
         slot 2: template 4199 flags 50397441\n\
         5 consumables\n\
         a mage wields 1700\n\
         dressed in 3 pieces\n"
    );
}

#[test]
fn dictionaries_set_get_and_update() {
    let src = r#"
namespace D3;
public static class P
{
    public static void Main()
    {
        var ages = new Dictionary<string, int>();
        ages["ada"] = 36;
        ages["grace"] = 45;
        ages["alan"] = 41;
        Console.WriteLine($"{ages.Count}");

        // Updating an existing key replaces rather than appends.
        ages["ada"] = 37;
        Console.WriteLine($"{ages.Count}");

        Console.WriteLine($"{ages.Get("ada") ?? -1}");
        Console.WriteLine($"{ages.Get("nobody") ?? -1}");
        Console.WriteLine($"{ages.ContainsKey("grace")}");
        Console.WriteLine($"{ages.ContainsKey("nobody")}");

        // Integer keys work the same way; growth crosses the initial capacity.
        var squares = new Dictionary<int, int>();
        foreach (var i in 1..10)
            squares[i] = i * i;
        Console.WriteLine($"{squares.Count} {squares.Get(9) ?? 0}");
    }
}
"#;
    assert_eq!(run_k2(src), "3\n3\n37\n-1\ntrue\nfalse\n10 81\n");
}

#[test]
fn dll_extern_calls_a_c_function() {
    // [Dll] + extern is how K2 reaches C. Tested against libc so it needs no
    // platform library; the same shape declares a Win32 import.
    let src = r#"
namespace I;

public static class Libc
{
    [Dll("c")]
    public static extern int abs(int n);

    [Dll("c", Entry = "strlen")]
    public static extern long Length(string s);

    [Dll("c")]
    public static extern int puts(string s);
}

public static class P
{
    public static void Main()
    {
        Console.WriteLine($"{Libc.abs(-42)}");
        Console.WriteLine($"{Libc.Length("kiln")}");
        Libc.puts("straight through libc");
    }
}
"#;
    assert_eq!(run_k2(src), "42\n4\nstraight through libc\n");
}

#[test]
fn packed_records_read_and_write_bytes() {
    // [Packed] is what protocol work needs: an exact byte layout with no
    // padding, written into and read back out of a buffer.
    let src = r#"
namespace Pk;

[Packed]
public record Header(ushort Opcode, ushort Length, uint Sequence);

public static class P
{
    public static void Main()
    {
        Console.WriteLine($"size {Header.Size}");

        var buf = Bytes.Alloc(64);
        var h = new Header(0x1234, 40, 7);
        h.Write(buf, 0);

        // A second header further along the same buffer.
        var h2 = new Header(0x5678, 12, 9);
        h2.Write(buf, Header.Size);

        var back = Header.Read(buf, 0);
        Console.WriteLine($"{back.Opcode} {back.Length} {back.Sequence}");
        var back2 = Header.Read(buf, Header.Size);
        Console.WriteLine($"{back2.Opcode} {back2.Length} {back2.Sequence}");

        // Little-endian on this target: the low byte of 0x1234 comes first.
        Console.WriteLine($"{buf[0]} {buf[1]}");
    }
}
"#;
    // 2 + 2 + 4 = 8 bytes, no padding. 0x1234 = 4660, low byte 0x34 = 52.
    assert_eq!(run_k2(src), "size 8\n4660 40 7\n22136 12 9\n52 18\n");
}

#[test]
fn table_attribute_builds_sql_at_compile_time() {
    // The feature the original starter kit argued for: the SQL is generated
    // from the record and a predicate lambda, at compile time. No reflection
    // reaches the binary — only the string.
    let src = r#"
namespace Db;

[Table("character_items")]
public record CharacterItem(
    [Auto] ulong Id,
    uint CharacterId,
    int Slot,
    uint TemplateId,
    [Column("flags")] uint Flags);

public static class P
{
    public static void Main()
    {
        Console.WriteLine(CharacterItem.InsertSql());
        Console.WriteLine(CharacterItem.SelectSql());

        var wanted = 7;
        Console.WriteLine(CharacterItem.SelectSql(i => i.CharacterId == wanted));
        Console.WriteLine(CharacterItem.SelectSql(i => i.CharacterId == wanted && i.Slot < 10));
    }
}
"#;
    assert_eq!(
        run_k2(src),
        "insert into character_items (character_id, slot, template_id, flags) values (?, ?, ?, ?)\n\
         select id, character_id, slot, template_id, flags from character_items\n\
         select id, character_id, slot, template_id, flags from character_items where (character_id = ?)\n\
         select id, character_id, slot, template_id, flags from character_items where ((character_id = ?) and (slot < 10))\n"
    );
}

#[test]
fn an_untranslatable_query_is_a_compile_error() {
    let src = r#"
namespace Db2;
[Table("t")]
public record Row(int A);
public static class P
{
    public static int Side() => 1;
    public static void Main()
    {
        Console.WriteLine(Row.SelectSql(r => r.A == Side()));
    }
}
"#;
    let err = kiln_k2::compile_to_llvm(src).unwrap_err();
    assert!(err.contains("cannot be translated to SQL"), "got: {err}");
}

#[test]
fn the_kiln_runtime_lowering_emits_slot_abi_commands() {
    // With the Kiln runtime, printing is the `print_text` command over the slot
    // ABI rather than printf. Linking it needs the runtime sources, which the
    // CLI does (`kiln k2 --runtime`); here we check the IR takes that path.
    let src = r#"
namespace Rt;
public static class P
{
    public static void Main()
    {
        Console.WriteLine("hello");
        var n = 6 * 7;
        Console.WriteLine($"answer {n}");
    }
}
"#;
    let ll = kiln_k2::compile_to_llvm_with(src, kiln_k2::Runtime::Kiln).unwrap();
    assert!(
        ll.contains("declare void @kn_print_text(ptr, i32, ptr)"),
        "the runtime command should be declared:\n{ll}"
    );
    assert!(
        ll.contains("call void @kn_print_text(ptr %s"),
        "printing should go through the slot ABI:\n{ll}"
    );
    assert!(
        !ll.contains("@printf"),
        "printf should not be used with the Kiln runtime:\n{ll}"
    );
    // The interpolated line is built once and printed once.
    assert_eq!(ll.matches("@kn_print_text(ptr %s").count(), 2, "{ll}");

    // The libc lowering of the same source still uses printf.
    let libc = kiln_k2::compile_to_llvm(src).unwrap();
    assert!(libc.contains("@printf"));
}

#[test]
fn interfaces_dispatch_dynamically() {
    // Interfaces, no inheritance (spec §4.2). An interface-typed value carries
    // the object and the implementation's methods, so two unrelated types can
    // be used through the same interface — including inside a List.
    let src = r#"
namespace If;

public interface IShape
{
    int Area();
    string Name();
}

public record Rect(int W, int H) : IShape
{
    public int Area() => W * H;
    public string Name() => "rect";
}

public record Square(int Side) : IShape
{
    public int Area() => Side * Side;
    public string Name() => "square";
}

public static class P
{
    // Takes any implementation — dispatch happens through the value.
    public static string Describe(IShape s) => $"{s.Name()} has area {s.Area()}";

    public static void Main()
    {
        Console.WriteLine(Describe(new Rect(3, 4)));
        Console.WriteLine(Describe(new Square(5)));

        var shapes = new List<IShape>();
        shapes.Add(new Rect(2, 3));
        shapes.Add(new Square(4));
        var total = 0;
        foreach (var s in shapes)
            total = total + s.Area();
        Console.WriteLine($"total {total}");
    }
}
"#;
    assert_eq!(
        run_k2(src),
        "rect has area 12\nsquare has area 25\ntotal 22\n"
    );
}

#[test]
fn a_missing_interface_method_is_a_compile_error() {
    let src = r#"
namespace If2;
public interface IShape { int Area(); }
public record Dot() : IShape { public int Size() => 0; }
public static class P { public static void Main() { } }
"#;
    let err = kiln_k2::compile_to_llvm(src).unwrap_err();
    assert!(err.contains("does not implement"), "got: {err}");
}

#[test]
fn generic_types_instantiate_per_type_argument() {
    let src = r#"
namespace GT;

/// A generic pair, instantiated per type argument.
public record Pair<A, B>(A First, B Second)
{
    public string Show() => $"({First}, {Second})";
}

/// A generic class with state and methods.
public class Box<T>
{
    T value;
    public Box(T v) { value = v; }
    public T Get() => value;
    public void Set(T v) { value = v; }
}

public static class P
{
    public static void Main()
    {
        var a = new Pair<int, int>(3, 4);
        Console.WriteLine(a.Show());

        var b = new Pair<int, string>(7, "seven");
        Console.WriteLine(b.Show());

        var box = new Box<int>(41);
        box.Set(box.Get() + 1);
        Console.WriteLine($"{box.Get()}");

        var words = new Box<string>("kiln");
        Console.WriteLine(words.Get());
    }
}
"#;
    assert_eq!(run_k2(src), "(3, 4)\n(7, seven)\n42\nkiln\n");
}

#[test]
fn a_form_lowers_to_the_ui_interface() {
    // The RAD half: a `form` becomes the UI build sequence — init, create each
    // component, set its properties, bind its handlers, run the loop. Rendering
    // it needs the UI runtime (see examples/k2/counter.kiln, built with
    // `kiln k2`); here we check the module it lowers to.
    let src = r#"
namespace C;
public form MainWindow
{
    Title = "Counter";
    Width = 320;
    Height = 200;

    Label count { Text = "0"; Left = 40; Top = 40; }
    Button add { Text = "Add one"; Click += OnAdd; }

    int n;

    void OnAdd()
    {
        n = n + 1;
        count.Text = $"{n}";
    }
}
"#;
    let m = kiln_k2::compile(src).unwrap();
    assert_eq!(m.kind, kiln_k2::ModuleKind::Gui, "a form is a GUI program");
    let ll = kiln_kir::emit::emit(&m);
    for expected in [
        "@kn_ui_init",
        "@kn_ui_create",
        "@kn_ui_set",
        "@kn_ui_on",
        "@kn_ui_set_a11y",
        "@kn_ui_run",
        "@kn_ui_shutdown",
    ] {
        assert!(ll.contains(expected), "missing {expected}:\n{ll}");
    }
    // The handler is bound by address, and component state lives in globals.
    assert!(ll.contains("ptr @MainWindow_OnAdd"), "{ll}");
    assert!(ll.contains("@MainWindow__count"), "{ll}");
    assert!(ll.contains("@MainWindow__state_n"), "{ll}");
    // Identifiers must not reach the binary as data; the window is named by its
    // title, and the label by its text.
    assert!(ll.contains("Counter"), "{ll}");
}

#[test]
fn a_partial_form_merges_its_halves() {
    // The designer owns one block and your code the other, as the spec has it.
    let src = r#"
namespace C2;

public partial form MainWindow
{
    Title = "Counter";
    Label count { Text = "0"; }
    Button add { Text = "Add"; Click += OnAdd; }
}

public partial form MainWindow
{
    int n;
    void OnAdd()
    {
        n = n + 1;
        count.Text = $"{n}";
    }
}
"#;
    let m = kiln_k2::compile(src).unwrap();
    assert_eq!(m.kind, kiln_k2::ModuleKind::Gui);
    let ll = kiln_kir::emit::emit(&m);
    // Components from the first block, state and handler from the second.
    assert!(ll.contains("@MainWindow__count"), "{ll}");
    assert!(ll.contains("@MainWindow__add"), "{ll}");
    assert!(ll.contains("@MainWindow__state_n"), "{ll}");
    assert!(ll.contains("ptr @MainWindow_OnAdd"), "{ll}");
}

#[test]
fn two_differently_named_forms_are_rejected() {
    let src = r#"
namespace C3;
public form A { Title = "a"; }
public form B { Title = "b"; }
"#;
    let err = match kiln_k2::compile(src) {
        Ok(_) => panic!("two forms should be rejected"),
        Err(e) => e,
    };
    assert!(err.contains("one form"), "got: {err}");
}

#[test]
fn an_event_can_be_wired_to_a_lambda() {
    // A form's state lives in globals, so a handler lambda that touches it
    // needs no environment pointer — it binds on the ABI as it stands.
    let src = r#"
namespace LH;
public partial form MainWindow
{
    Title = "Lambdas";
    Label count { Text = "0"; }
    Button add { Text = "Add"; Click += () => { n = n + 1; count.Text = $"{n}"; }; }
}
public partial form MainWindow { int n; }
"#;
    let m = kiln_k2::compile(src).unwrap();
    let ll = kiln_kir::emit::emit(&m);
    // The lambda became its own handler, bound by address.
    assert!(ll.contains("ptr @MainWindow_add_click"), "{ll}");
    // It reaches the form's state and the component directly.
    assert!(ll.contains("@MainWindow__state_n"), "{ll}");
    assert!(ll.contains("@MainWindow__count"), "{ll}");
}

#[test]
fn generic_instance_methods_and_interface_constraints() {
    let src = r#"
namespace GI;

public interface INamed { string Name(); }

public record Tagged(string Label) : INamed
{
    public string Name() => Label;
    // A generic method on an instance: the receiver comes through as `this`.
    public string With<T>(T extra) => $"{Label}/{extra}";
}

public static class P
{
    // The constraint is enforced when the type argument is chosen.
    public static string Show<T>(T x) where T : INamed => $"[{x.Name()}]";

    public static void Main()
    {
        var t = new Tagged("core");
        Console.WriteLine(t.With(7));
        Console.WriteLine(t.With("extra"));
        Console.WriteLine(Show(t));
    }
}
"#;
    assert_eq!(run_k2(src), "core/7\ncore/extra\n[core]\n");
}

#[test]
fn an_unsatisfied_constraint_is_a_compile_error() {
    let src = r#"
namespace GI2;
public interface INamed { string Name(); }
public record Plain(int N);
public static class P
{
    public static string Show<T>(T x) where T : INamed => "x";
    public static void Main() { Console.WriteLine(Show(new Plain(1))); }
}
"#;
    let err = kiln_k2::compile_to_llvm(src).unwrap_err();
    assert!(err.contains("to implement `INamed`"), "got: {err}");
}

/// Printing must be a fixed point: format(format(x)) == format(x), and the
/// reprinted source must still compile to the same thing. This is what lets
/// Studio and `kiln migrate` write source through the tree.
fn assert_format_roundtrips(src: &str, label: &str) {
    let once = kiln_k2::format(src).unwrap_or_else(|e| panic!("{label}: format failed: {e}"));
    let twice = kiln_k2::format(&once)
        .unwrap_or_else(|e| panic!("{label}: reformatting failed: {e}\n--- once ---\n{once}"));
    assert_eq!(once, twice, "{label}: formatting is not a fixed point");
}

#[test]
fn formatting_is_a_fixed_point() {
    for (label, src) in [
        (
            "kitchen sink",
            r#"
namespace F;
using Kiln.IO;

public enum Colour : byte { Red, Green = 4, Blue }

public interface INamed { string Name(); }

[Table("rows")]
public record Row([Auto] ulong Id, [Column("n")] int N) : INamed
{
    public string Name() => $"row {N}";
}

public static class P
{
    public const int Limit = 10;
    private int state;

    public static T Pick<T>(T a, T b) where T : INamed => a;

    [Dll("c")]
    public static extern int abs(int n);

    public static void Main()
    {
        var xs = new List<int>();
        foreach (var i in 1..Limit)
        {
            if (i % 2 == 0)
                continue;
            xs.Add(i * i);
        }
        var evens = xs.Where(x => x > 4).Select(x => $"n{x}");
        defer Console.WriteLine("done");
        int? maybe = null;
        Console.WriteLine($"{maybe ?? -1} {evens.Count}");
        var c = 3 switch { 1 => "one", < 5 => "few", _ => "many" };
        Console.WriteLine(c);
    }
}
"#,
        ),
        (
            "form",
            r#"
namespace G;
public partial form MainWindow
{
    Title = "Counter";
    Width = 320;
    Label count { Text = "0"; }
    Button add { Text = "Add"; Click += OnAdd; }
    Button two { Text = "Two"; Click += () => { n = 0; }; }
    int n;
    void OnAdd() { n = n + 1; count.Text = $"{n}"; }
}
"#,
        ),
    ] {
        assert_format_roundtrips(src, label);
    }
}

#[test]
fn formatted_source_still_compiles_the_same() {
    // Reformatting must not change meaning: the same program, reformatted,
    // must produce byte-identical LLVM.
    let src = r#"
namespace Fmt;

public record Rect(int W, int H)
{
    public int Area() => W * H;
}

public static class P
{
    public static void Main()
    {
        var r = new Rect(3, 4);
        var xs = new List<int>();
        foreach (var i in 1..5)
            xs.Add(i * i + 1);
        var big = xs.Where(x => x > 4);
        Console.WriteLine($"{r.Area()} {big.Count}");
    }
}
"#;
    let before = kiln_k2::compile_to_llvm(src).unwrap();
    let formatted = kiln_k2::format(src).unwrap();
    let after = kiln_k2::compile_to_llvm(&formatted)
        .unwrap_or_else(|e| panic!("reformatted: {e}\n{formatted}"));
    assert_eq!(before, after, "reformatting changed the program");
}

#[test]
fn formatting_keeps_comments() {
    let src = "namespace C;\n// a note worth keeping\npublic static class P\n{\n    // inside, too\n    public static void Main()\n    {\n        // and here\n        var x = 1;\n    }\n}\n";
    let out = kiln_k2::format(src).unwrap();
    for c in ["// a note worth keeping", "// inside, too", "// and here"] {
        assert!(out.contains(c), "lost {c}:\n{out}");
    }
    // Doc comments are kept too, and formatting stays a fixed point.
    let ok = "namespace D;\n/// kept\npublic static class P { public static void Main() { } }\n";
    let out = kiln_k2::format(ok).unwrap();
    assert!(out.contains("/// kept"), "{out}");
    assert_eq!(kiln_k2::format(&out).unwrap(), out);
}

#[test]
fn the_examples_format_without_losing_anything() {
    // The real test: the shipped examples are comment-heavy. Formatting them
    // must keep every comment, stay a fixed point, and not change the program.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    for name in ["examples/k2/demo.kiln", "examples/k2/starter.kiln"] {
        let src = std::fs::read_to_string(root.join(name)).unwrap();
        let once = kiln_k2::format(&src).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            kiln_k2::format(&once).unwrap(),
            once,
            "{name}: not a fixed point"
        );
        // Every comment in the source survives.
        for line in src.lines() {
            let t = line.trim();
            if let Some(c) = t.strip_prefix("//") {
                if !c.starts_with('/') && !c.trim().is_empty() {
                    assert!(
                        once.contains(c.trim()),
                        "{name}: lost comment `{}`",
                        c.trim()
                    );
                }
            }
        }
        let before = kiln_k2::compile_to_llvm(&src).unwrap();
        let after = kiln_k2::compile_to_llvm(&once).unwrap();
        assert_eq!(before, after, "{name}: reformatting changed the program");
    }
}

#[test]
fn every_1x_example_migrates_and_reparses() {
    // `kiln migrate` converts the mechanical part and leaves TODO comments for
    // the rest. The invariant that matters: whatever it emits must be valid K2
    // source, so the file you get back is one you can open and work on.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let dir = root.join("examples").join("1x");
    let mut seen = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let p = entry.unwrap().path();
        if p.extension().and_then(|e| e.to_str()) != Some("kiln") {
            continue;
        }
        let src = std::fs::read_to_string(&p).unwrap();
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        let migrated = match kiln_k2::migrate(&src) {
            Ok(m) => m,
            // A 1.x program this build cannot parse is not migrate's failure.
            Err(_) => continue,
        };
        seen += 1;
        kiln_k2::format(&migrated).unwrap_or_else(|e| {
            panic!("{name}: migrated output is not valid K2: {e}\n--- output ---\n{migrated}")
        });
    }
    assert!(seen > 20, "expected the 1.x examples, saw {seen}");
}

#[test]
fn migration_converts_the_mechanical_part() {
    let src = "\
module starter
use text

const ITEM_FLAGS = 16843009

record item_spec
  slot: int
  template: int
end

sub starter_weapon(profession: int): int
  if profession = 0
    return 1000
  end
  return 1800
end

sub main
  let n: int = starter_weapon(0)
  call print_text(\"weapon {n}\")
end
";
    let out = kiln_k2::migrate(src).unwrap();
    // Naming: module and subs to PascalCase, parameters to camelCase.
    assert!(out.contains("namespace Starter;"), "{out}");
    assert!(out.contains("public static class Starter"), "{out}");
    assert!(out.contains("StarterWeapon(int profession)"), "{out}");
    assert!(out.contains("public static void Main()"), "{out}");
    // A 1.x record becomes a record; a SCREAMING constant becomes PascalCase.
    assert!(
        out.contains("public record ItemSpec(int Slot, int Template)"),
        "{out}"
    );
    assert!(out.contains("ItemFlags = 16843009"), "{out}");
    // `use text` becomes a using; printing becomes Console.WriteLine.
    assert!(out.contains("using Kiln.Text;"), "{out}");
    assert!(out.contains("Console.WriteLine"), "{out}");
    // 1.x interpolates every string; K2 only a `$` one.
    assert!(out.contains("$\"weapon {n}\""), "{out}");
    // And it is valid K2.
    kiln_k2::format(&out).unwrap();
}

#[test]
fn kiln_edit_changes_a_form_through_the_tree() {
    // What Studio calls instead of splicing text. Everything the edit did not
    // touch — comments included — must come back unchanged.
    let src = r#"// a file comment
namespace E;

// the designer's half
public partial form MainWindow
{
    Title = "Before";
    Label count { Text = "0"; }
}

public partial form MainWindow
{
    // your half, untouched by edits
    int n;
    void OnAdd() { n = n + 1; }
}
"#;
    // Add a component, give it properties, wire it, retitle the window.
    let s = kiln_k2::edit(
        src,
        &kiln_k2::Edit::AddComponent {
            type_name: "Button".into(),
            id: "add".into(),
        },
    )
    .unwrap();
    let s = kiln_k2::edit(
        &s,
        &kiln_k2::Edit::SetProperty {
            target: "add".into(),
            name: "Text".into(),
            value: "Add one".into(),
        },
    )
    .unwrap();
    let s = kiln_k2::edit(
        &s,
        &kiln_k2::Edit::AddHandler {
            target: "add".into(),
            event: "Click".into(),
            method: "OnAdd".into(),
        },
    )
    .unwrap();
    let s = kiln_k2::edit(
        &s,
        &kiln_k2::Edit::SetProperty {
            target: String::new(),
            name: "Title".into(),
            value: "After".into(),
        },
    )
    .unwrap();

    assert!(s.contains("Button add"), "{s}");
    assert!(s.contains("Text = \"Add one\";"), "{s}");
    assert!(s.contains("Click += OnAdd;"), "{s}");
    assert!(s.contains("Title = \"After\";"), "{s}");
    // Nothing else moved.
    for kept in [
        "// a file comment",
        "// the designer's half",
        "// your half, untouched by edits",
        "Label count",
        "void OnAdd()",
    ] {
        assert!(s.contains(kept), "lost {kept}:\n{s}");
    }
    // Setting an existing property replaces rather than duplicates it.
    assert_eq!(s.matches("Title =").count(), 1, "{s}");
    // And the result is a GUI program that still lowers.
    let m = kiln_k2::compile(&s).unwrap();
    assert_eq!(m.kind, kiln_k2::ModuleKind::Gui);

    // Removing puts it back the way it was.
    let s = kiln_k2::edit(&s, &kiln_k2::Edit::RemoveComponent { id: "add".into() }).unwrap();
    assert!(!s.contains("Button add"), "{s}");
}

#[test]
fn hashsets_refuse_duplicates() {
    let src = r#"
namespace HS;
public static class P
{
    public static void Main()
    {
        var seen = new HashSet<int>();
        foreach (var i in 1..10)
            seen.Add(i % 4);
        Console.WriteLine($"{seen.Count}");
        Console.WriteLine($"{seen.Contains(3)} {seen.Contains(9)}");

        var names = new HashSet<string>();
        names.Add("ada");
        names.Add("ada");
        names.Add("grace");
        Console.WriteLine($"{names.Count}");
        foreach (var n in names)
            Console.WriteLine(n);
    }
}
"#;
    // i % 4 over 1..10 yields {1,2,3,0} — four distinct values.
    assert_eq!(run_k2(src), "4\ntrue false\n2\nada\ngrace\n");
}

#[test]
fn a_dictionary_stays_correct_through_rehashing() {
    // The index is open-addressed over a power-of-two capacity and rebuilt on
    // every growth. Two thousand keys cross several rebuilds, and every one
    // must still find its own value.
    let src = r#"
namespace Rehash;
public static class P
{
    public static void Main()
    {
        var d = new Dictionary<int, int>();
        foreach (var i in 1..2000)
            d[i] = i * 3;
        Console.WriteLine($"{d.Count}");

        var wrong = 0;
        foreach (var i in 1..2000)
            if ((d.Get(i) ?? -1) != i * 3)
                wrong = wrong + 1;
        Console.WriteLine($"wrong {wrong}");

        // Overwriting replaces rather than appending.
        d[7] = 999;
        Console.WriteLine($"{d.Count} {d.Get(7) ?? 0}");

        // String keys hash by content.
        var s = new Dictionary<string, int>();
        s["ada"] = 1;
        s["grace"] = 2;
        s["ada"] = 4;
        Console.WriteLine($"{s.Count} {s.Get("ada") ?? 0} {s.ContainsKey("nobody")}");
    }
}
"#;
    assert_eq!(run_k2(src), "2000\nwrong 0\n2000 999\n2 4 false\n");
}

#[test]
fn a_set_stays_correct_through_rehashing() {
    let src = r#"
namespace SetRehash;
public static class P
{
    public static void Main()
    {
        var s = new HashSet<int>();
        foreach (var i in 1..5000)
            s.Add(i % 1500);
        Console.WriteLine($"{s.Count}");

        var missing = 0;
        foreach (var i in 0..1499)
            if (!s.Contains(i))
                missing = missing + 1;
        Console.WriteLine($"missing {missing}");
        Console.WriteLine($"{s.Contains(9999)}");

        var n = new HashSet<string>();
        n.Add("ada");
        n.Add("ada");
        n.Add("grace");
        Console.WriteLine($"{n.Count} {n.Contains("grace")} {n.Contains("nope")}");
    }
}
"#;
    assert_eq!(run_k2(src), "1500\nmissing 0\nfalse\n2 true false\n");
}

#[test]
fn each_turn_of_a_loop_closes_over_its_own_variable() {
    // C# 5 changed this rule because the old one was always a bug: with one
    // shared variable every closure sees the last value. Each turn gets a cell
    // of its own, so the list prints 1 2 3 and not 3 3 3 — over a range and
    // over a list, and the loop variable still reads normally inside the body.
    let src = r#"
namespace LoopCapture;
public static class P
{
    public static void Main()
    {
        var fs = new List<Action>();
        foreach (var i in 1..3)
        {
            Console.WriteLine($"at {i}");
            fs.Add(() => { Console.Write($"{i} "); });
        }
        foreach (var f in fs)
            f();
        Console.WriteLine("");

        var names = new List<string>();
        names.Add("ada");
        names.Add("grace");
        var gs = new List<Action>();
        foreach (var n in names)
            gs.Add(() => { Console.Write($"{n} "); });
        foreach (var g in gs)
            g();
        Console.WriteLine("");
    }
}
"#;
    assert_eq!(
        run_k2(src),
        "at 1\nat 2\nat 3\n1 2 3 \nada grace \n"
    );
}

#[test]
fn a_loop_variable_is_still_captured_by_reference_within_one_turn() {
    // Per-iteration does not mean by-value: a closure made in one turn still
    // sees a write made later in that same turn.
    let src = r#"
namespace TurnRef;
public static class P
{
    public static void Main()
    {
        var fs = new List<Action>();
        foreach (var i in 1..2)
        {
            fs.Add(() => { Console.Write($"{i} "); });
            i = i * 10;
        }
        foreach (var f in fs)
            f();
        Console.WriteLine("");
    }
}
"#;
    assert_eq!(run_k2(src), "10 20 \n");
}

#[test]
fn a_designers_save_goes_through_the_tree() {
    // What Studio's save has to be: it holds a whole form and has no record of
    // which single edit got it there. `sync` takes back exactly what
    // `kiln inspect` prints, so the CLI stays the only reader *and* writer.
    let src = "\
namespace Counter;

// Studio owns this block.
public partial form MainWindow
{
    Title = \"Counter\";

    // The number, which is text and must stay text.
    Label count
    {
        Text = \"0\";
        Left = 40;
    }
}

public partial form MainWindow
{
    int n;

    void OnAdd()
    {
        n = n + 1;      // a comment of mine
        count.Text = $\"{n}\";
    }
}
";
    // A save that moves the label and wires a button that was just dropped.
    let spec = "\
form: main_window span=4..15
prop: main_window title Counter
component: count label
prop: count text 0
prop: count left 80
component: add button
prop: add text Add one
handler: add click on_add
";
    let out = kiln_k2::edit(
        src,
        &kiln_k2::Edit::Sync(kiln_k2::edit::parse_spec(spec).unwrap()),
    )
    .unwrap();

    // The designer's half is what it was told.
    assert!(out.contains("Left = 80;"), "the move was not saved:\n{out}");
    assert!(out.contains("Button add"), "the new button is missing:\n{out}");
    assert!(out.contains("Click += OnAdd;"), "the wiring is missing:\n{out}");
    // Names come back in K2's casing, not the designer's.
    assert!(!out.contains("background_color"), "snake case leaked:\n{out}");

    // A value that is text stays text: `Text = "0"` must not become `Text = 0`,
    // which is what guessing the type from the printed value would do.
    assert!(out.contains("Text = \"0\";"), "a string became a number:\n{out}");

    // And none of this touches the half that is not the designer's.
    assert!(out.contains("int n;"), "the code half lost a field:\n{out}");
    assert!(
        out.contains("// a comment of mine"),
        "a comment in the code half was lost:\n{out}"
    );
    assert!(
        out.contains("// The number, which is text and must stay text."),
        "a comment in the designer's block was lost:\n{out}"
    );

    // What it produces still compiles.
    kiln_k2::compile(&out).expect("a saved form still compiles");
}

#[test]
fn a_save_round_trips_through_inspect_unchanged() {
    // Saving a form nobody edited must change nothing at all — otherwise every
    // open-and-close rewrites the file, and the diff is noise forever.
    let src = "\
namespace RT;

public partial form MainWindow
{
    Title = \"Hi\";

    Label count
    {
        Text = \"0\";
        Left = 40;
    }

    Button go
    {
        Text = \"Go\";
        Click += OnGo;
    }
}

public partial form MainWindow
{
    void OnGo()
    {
        count.Text = \"1\";
    }
}
";
    let spec = "\
form: main_window span=3..18
prop: main_window title Hi
component: count label
prop: count text 0
prop: count left 40
component: go button
prop: go text Go
handler: go click on_go
";
    let out = kiln_k2::edit(
        src,
        &kiln_k2::Edit::Sync(kiln_k2::edit::parse_spec(spec).unwrap()),
    )
    .unwrap();
    assert_eq!(out, src, "an untouched save rewrote the file");
}

#[test]
fn a_wired_handler_lands_in_the_code_half() {
    // The designer wires an event and leaves an empty handler behind. It
    // belongs in the half of the form that holds code — putting it beside the
    // components would be the designer rewriting the user's half. The name
    // arrives in the designer's spelling and is written in K2's.
    let src = "\
namespace W;

public partial form MainWindow
{
    Title = \"x\";
    Button go { Text = \"Go\"; }
}

public partial form MainWindow
{
    int n;
}
";
    let out = kiln_k2::edit(
        src,
        &kiln_k2::Edit::AddMethod {
            name: "button1_click".into(),
            params: vec![],
        },
    )
    .unwrap();
    assert!(out.contains("void Button1Click()"), "{out}");
    // In the code half: after the field, not among the components.
    let code_half = out.rsplit("public partial form MainWindow").next().unwrap();
    assert!(code_half.contains("Button1Click"), "wrong half:\n{out}");
    assert!(!code_half.contains("Button go"), "wrong half:\n{out}");
    kiln_k2::compile(&out).expect("still compiles");

    // An event that hands something over gets a parameter for it.
    let with_params = kiln_k2::edit(
        src,
        &kiln_k2::Edit::AddMethod {
            name: "grid1_select".into(),
            params: vec!["int".into()],
        },
    )
    .unwrap();
    assert!(with_params.contains("void Grid1Select(int a1)"), "{with_params}");
}

#[test]
fn an_event_can_be_wired_in_code() {
    // With ABI v5 a handler can be bound at run time, capturing what it needs.
    // What it cannot be is anything other than a method of the form or a
    // lambda written where it is wired — that is refused, and says so.
    let ok = r#"
namespace Wiring;

public partial form MainWindow
{
    Title = "x";
    Button go { Text = "Go"; }
}

public partial form MainWindow
{
    void Setup()
    {
        var row = 3;
        go.Click += () => { Console.WriteLine($"{row}"); };
        go.Click += OnGo;
        go.Click -= OnGo;
    }
    void OnGo() { }
}
"#;
    kiln_k2::compile(ok).expect("run-time wiring compiles");

    let bad = ok.replace("go.Click += OnGo;", "go.Click += 42;");
    let err = kiln_k2::compile(&bad).err().expect("a number is not a handler");
    assert!(err.contains("method of the form or a lambda"), "unhelpful error: {err}");
}

#[test]
fn a_migrated_constant_keeps_one_name() {
    // A 1.x constant is SCREAMING_CASE and a K2 one is PascalCase. The
    // declaration took the right name and every use took `camel`, which turned
    // `TIMES` into `tIMES` — so the migrated program named something that did
    // not exist, and did not compile. The two must agree.
    let src = "module m\n\
               const TIMES = 3\n\
               sub main\n\
               \x20 var i: int = 0\n\
               \x20 while i < TIMES\n\
               \x20   call print_int(i)\n\
               \x20   i = i + 1\n\
               \x20 end\n\
               end\n";
    let out = kiln_k2::migrate(src).expect("migrates");
    assert!(out.contains("Times = 3"), "{out}");
    assert!(out.contains("i < Times"), "the use site disagrees:\n{out}");
    assert!(!out.contains("tIMES"), "{out}");
    // And the whole point: what comes out compiles.
    kiln_k2::compile(&out).expect("a migrated program compiles");
}

#[test]
fn a_static_field_is_one_variable_with_its_initial_value() {
    // A static class's field is a module global, set before Main runs. It was
    // not a variable at all — "cannot assign to unknown" — which is how every
    // migrated 1.x module with state failed to build.
    let src = r#"
namespace Statics;
public static class P
{
    static int count = 5;
    static string label = "bumps";
    static void Bump() { count = count + 1; }
    public static void Main()
    {
        Bump();
        Bump();
        Console.WriteLine($"{count} {label}");
        Console.WriteLine($"{P.count}");
    }
}
"#;
    assert_eq!(run_k2(src), "7 bumps\n7\n");
}

#[test]
fn a_list_position_is_stored_into_and_checked() {
    // `xs[2] = 20` crashed the compiler: the store handed the emitter the list
    // record as if it were an array. And a position outside the list read
    // whatever lay past the buffer, silently. Both are fixed; the index is
    // evaluated once, so a call in it runs once.
    let src = r#"
namespace ListStore;
public static class P
{
    static int calls = 0;
    static int Two() { calls = calls + 1; return 2; }
    public static void Main()
    {
        var xs = new List<int>();
        xs.Add(1);
        xs.Add(2);
        xs[Two()] = 20;
        Console.WriteLine($"{xs[1]} {xs[2]} calls={calls}");
        var b = Bytes.Alloc(2);
        b[0] = 65;
        b[1] = 66;
        Console.WriteLine($"{b[0]} {b[1]}");
    }
}
"#;
    assert_eq!(run_k2(src), "1 20 calls=1\n65 66\n");
}

#[test]
fn a_position_outside_a_list_stops_the_program_and_says_so() {
    let src = r#"
namespace Outside;
var xs = new List<int>();
xs.Add(1);
Console.WriteLine($"{xs[9]}");
Console.WriteLine("never");
"#;
    let ll = kiln_k2::compile_to_llvm(src).unwrap();
    let dir = std::env::temp_dir().join(format!("k2-oob-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("m.ll"), &ll).unwrap();
    std::fs::write(
        dir.join("shim.c"),
        "extern int ECodeStart(void); int main(void){return ECodeStart();}\n",
    )
    .unwrap();
    let built = Proc::new("clang")
        .args(["m.ll", "shim.c", "-o", "m"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let r = Proc::new(dir.join("m")).output().unwrap();
    assert_eq!(r.status.code(), Some(1));
    assert_eq!(String::from_utf8_lossy(&r.stdout), "", "it carried on past the bad read");
    assert!(
        String::from_utf8_lossy(&r.stderr).contains("index 9 is outside a list of 1 element(s)"),
        "{}",
        String::from_utf8_lossy(&r.stderr)
    );
}

#[test]
fn a_dictionary_stays_correct_through_removal_and_growth() {
    // Removal moves the last entry into the gap and rebuilds the index, so
    // the index never holds a tombstone and a probe may still stop at the
    // first empty slot. This checks that promise against a model: remove every
    // third key while the table keeps growing, then look every key up.
    let src = r#"
namespace DictRemove;
public static class P
{
    public static void Main()
    {
        var d = new Dictionary<int, int>();
        foreach (var i in 1..3000)
        {
            d[i] = i * 10;
            if (i % 3 == 0)
                d.Remove(i - 1);
        }
        var wrong = 0;
        var present = 0;
        foreach (var i in 1..3000)
        {
            var should = (i + 1) % 3 != 0 || i == 3000;
            if (d.ContainsKey(i) != should)
                wrong = wrong + 1;
            if (d.ContainsKey(i))
            {
                present = present + 1;
                if ((d.Get(i) ?? -1) != i * 10)
                    wrong = wrong + 1;
            }
        }
        Console.WriteLine($"{d.Count} {present} wrong {wrong}");
        Console.WriteLine($"{d.Remove(1)} {d.Remove(1)} {d.ContainsKey(1)}");

        var s = new Dictionary<string, string>();
        s["ada"] = "x";
        s["grace"] = "y";
        s["linus"] = "z";
        s.Remove("grace");
        Console.WriteLine($"{s.Count} {s.ContainsKey("ada")} {s.ContainsKey("grace")} {s.Get("linus") ?? "?"}");
    }
}
"#;
    // Keys 2, 5, 8, … (every i ≡ 2 mod 3 below 3000) are removed: 1000 of them.
    assert_eq!(run_k2(src), "2000 2000 wrong 0\ntrue false false\n2 true false z\n");
}

#[test]
fn a_list_finds_and_removes_by_value() {
    let src = r#"
namespace ListRemove;
var xs = [5, 7, 9, 7];
Console.WriteLine($"{xs.Contains(7)} {xs.Contains(8)}");
Console.WriteLine($"{xs.Remove(7)} {xs.Count} {xs[1]} {xs[2]} {xs[3]}");
Console.WriteLine($"{xs.Remove(42)} {xs.Count}");
var names = ["ada", "grace"];
Console.WriteLine($"{names.Contains("grace")} {names.Remove("ada")} {names[1]}");
"#;
    assert_eq!(run_k2(src), "true false\ntrue 3 5 9 7\nfalse 3\ntrue true grace\n");
}

#[test]
fn a_dictionary_is_walked_in_the_order_its_keys_were_added() {
    // `foreach (var (key, value) in d)`, and the order survives a removal: the
    // later entries shift down rather than the last one jumping into the gap,
    // which is what 1.x promised and what makes iterating one reproducible.
    let src = r#"
namespace DictWalk;
var ages = new Dictionary<string, int>();
ages["Ada"] = 36;
ages["Alan"] = 41;
ages["Grace"] = 45;
ages["Linus"] = 29;
ages.Remove("Alan");
foreach (var (who, age) in ages)
    Console.WriteLine($"{who} -> {age}");
var n = 7;
Console.WriteLine(n.ToString() + "/" + true.ToString());
var xs = [10, 20, 30, 40];
xs.RemoveAt(2);
Console.WriteLine($"{xs.Count} {xs[1]} {xs[2]} {xs[3]}");
"#;
    assert_eq!(
        run_k2(src),
        "Ada -> 36\nGrace -> 45\nLinus -> 29\n7/true\n3 10 30 40\n"
    );
}

#[test]
fn text_compares_by_content_and_and_really_short_circuits() {
    // `==` on two strings compared the pointers, so a built string never
    // equalled a literal — `"fizz" + "buzz" == "fizzbuzz"` was false. And the
    // right side of `&&` was only half short-circuited: its value waited for
    // the `if`, but the statements it needed ran first — so the bounds check in
    // `ready && xs[9] > 0` stopped the program though `ready` was false.
    let src = r#"
namespace EqSc;
Console.WriteLine($"{"fizz" + "buzz" == "fizzbuzz"} {"a" != "b"} {"abc" < "abd"} {"b" < "a"}");
var xs = new List<int>();
var ready = false;
if (ready && xs[9] > 0)
    Console.WriteLine("no");
var done = true;
if (done || xs[9] > 0)
    Console.WriteLine("or held too");
"#;
    assert_eq!(run_k2(src), "true true true false\nor held too\n");
}

#[test]
fn a_default_argument_is_used_and_a_wrong_count_is_refused() {
    // The default was parsed and thrown away, and arguments were paired with
    // parameters by `zip` — so `Shout("ada")` passed nothing where `mark`
    // belonged and the program segfaulted reading it.
    let src = r#"
namespace Defaults;
public static class P
{
    static string Shout(string who, string mark = "!") => $"{who}{mark}";
    public static void Main()
    {
        Console.WriteLine(Shout("ada"));
        Console.WriteLine(Shout("ada", "?"));
    }
}
"#;
    assert_eq!(run_k2(src), "ada!\nada?\n");

    let short = src.replace("Shout(\"ada\")", "Shout()");
    let err = kiln_k2::compile(&short).err().expect("too few arguments");
    assert!(err.contains("`Shout` takes 1 to 2 argument(s)"), "{err}");
    let long = src.replace("Shout(\"ada\", \"?\")", "Shout(\"a\", \"b\", \"c\")");
    let err = kiln_k2::compile(&long).err().expect("too many arguments");
    assert!(err.contains("but this call passes 3"), "{err}");
}

#[test]
fn a_condition_must_be_true_or_false_and_a_statement_must_do_something() {
    // A string used as a condition was emitted as a branch on a pointer, which
    // clang rejected far from the cause; a bare value as a statement was
    // silently dropped. Both are errors a reader can act on.
    let cond = "namespace C1;\nif (\"yes\")\n    Console.WriteLine(\"x\");\n";
    let err = kiln_k2::compile(cond).err().expect("a string is not a condition");
    assert!(err.contains("a condition must be true or false, but this is text"), "{err}");
    let bare = "namespace C2;\n\"nothing\";\n";
    let err = kiln_k2::compile(bare).err().expect("a bare value does nothing");
    assert!(err.contains("does nothing"), "{err}");
}

#[test]
fn a_dictionary_is_initialised_in_place_and_formats_back() {
    // C#'s index initialiser — the only way to write a dictionary's entries
    // where it is made. It prints back in the same spelling, so formatting a
    // file that uses one does not lose it.
    let src = r#"
namespace DictInit;
var ages = new Dictionary<string, int> { ["Ada"] = 36, ["Alan"] = 41 };
var empty = new Dictionary<string, string>();
foreach (var (who, age) in ages)
    Console.WriteLine($"{who} {age}");
Console.WriteLine($"{empty.Count}");
"#;
    assert_eq!(run_k2(src), "Ada 36\nAlan 41\n0\n");
    let formatted = kiln_k2::format(src).unwrap();
    assert!(formatted.contains(r#"new Dictionary<string, int> { ["Ada"] = 36, ["Alan"] = 41 }"#), "{formatted}");
    assert_eq!(run_k2(&formatted), "Ada 36\nAlan 41\n0\n");
}

#[test]
fn a_switch_statement_selects_a_section() {
    let src = r#"
namespace Sw;

public enum Colour { Red, Green, Blue }

public static class P
{
    static string Kind(int n)
    {
        switch (n)
        {
            case 0:
                return "zero";
            case 1:
            case 2:
                return "small";
            case >= 100:
                return "huge";
            default:
                return "other";
        }
    }

    public static void Main()
    {
        for (int i = 0; i < 6; i++)
        {
            switch (i % 3)
            {
                case 0:
                    if (i == 3)
                    {
                        Console.WriteLine("three, skipping");
                        continue;
                    }
                    Console.WriteLine($"{i} fizz");
                    break;
                case 1:
                    Console.WriteLine($"{i} one");
                    break;
                default:
                    Console.WriteLine($"{i} two");
                    break;
            }
            Console.WriteLine($"after {i}");
        }
        Console.WriteLine($"{Kind(0)} {Kind(2)} {Kind(500)} {Kind(7)}");
        var name = "bob";
        switch (name)
        {
            case "alice":
                Console.WriteLine("hi alice");
                break;
            case "bob":
                Console.WriteLine("hi bob");
                break;
            default:
                break;
        }
        var c = Colour.Green;
        switch (c)
        {
            case Colour.Red: Console.WriteLine("red"); break;
            case Colour.Green: Console.WriteLine("green"); break;
            default: Console.WriteLine("blue"); break;
        }
    }
}
"#;
    assert_eq!(
        run_k2(src),
        "0 fizz\nafter 0\n1 one\nafter 1\n2 two\nafter 2\nthree, skipping\n4 one\nafter 4\n5 two\nafter 5\nzero small huge other\nhi bob\ngreen\n"
    );
}

#[test]
fn a_switch_section_that_falls_through_is_a_compile_error() {
    let fall = r#"
namespace Sw2;
public static class P { public static void Main() {
    switch (1) { case 1: Console.WriteLine("a"); case 2: break; }
} }
"#;
    let err = kiln_k2::compile_to_llvm(fall).unwrap_err();
    assert!(err.contains("fall through"), "got: {err}");
    let cont = r#"
namespace Sw3;
public static class P { public static void Main() {
    switch (1) { case 1: continue; default: break; }
} }
"#;
    let err = kiln_k2::compile_to_llvm(cont).unwrap_err();
    assert!(err.contains("inside a loop"), "got: {err}");
}

#[test]
fn a_function_pointer_calls_a_c_address_and_a_struct_holds_arrays_and_structs_in_place() {
    let src = r#"
namespace FnPtr;

[CLayout]
public record Point(int X, int Y);

[CLayout]
public record Box(int Tag, Point Corner, int[3] Values, delegate* unmanaged<int, int, int>[2] Ops);

public static class P
{
    static int Add(int a, int b) => a + b;
    static int Mul(int a, int b) => a * b;

    public static void Main()
    {
        delegate* unmanaged<int, int, int> f = Add;
        Console.WriteLine(f(2, 3));

        var b = new Box();
        b.Tag = 7;
        b.Corner.X = 10;
        b.Corner.Y = 20;
        foreach (var i in 1..3)
            b.Values[i] = i * 100;
        b.Ops[1] = Add;
        b.Ops[2] = Mul;
        Console.WriteLine($"{b.Tag} {b.Corner.X} {b.Corner.Y} {b.Values[1]} {b.Values[3]}");
        Console.WriteLine($"{b.Ops[1](6, 7)} {b.Ops[2](6, 7)}");
        Console.WriteLine($"{Box.Size} {Box.OffsetOf("Values")} {Box.OffsetOf("Ops")}");
        long big = 12;
        Console.WriteLine(Box.OffsetOf("Values") == big);
    }
}
"#;
    // Box: Tag 4, Corner 8 at 4, Values 12 at 12, Ops 16 at 24 → 40.
    assert_eq!(run_k2(src), "5\n7 10 20 100 300\n13 42\n40 12 24\ntrue\n");
}

#[test]
fn a_function_pointer_with_the_wrong_signature_is_a_compile_error() {
    let src = r#"
namespace FnPtr2;
public static class P
{
    static int Add(int a, int b) => a + b;
    public static void Main()
    {
        delegate* unmanaged<int, int> f = Add;
    }
}
"#;
    let err = kiln_k2::compile_to_llvm(src).unwrap_err();
    assert!(err.contains("signature"), "got: {err}");
}
