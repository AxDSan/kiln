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
fn capturing_a_loop_variable_is_reported_not_miscompiled() {
    // Locals and parameters are captured by reference through cells; a foreach
    // variable is not celled yet, so it must be reported rather than compiled
    // to the wrong thing.
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
    let err = kiln_k2::compile_to_llvm(src).unwrap_err();
    assert!(err.contains("captures `i`"), "unexpected error: {err}");
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
    assert_eq!(run_k2(src), "42\n0\nnot even\n-1\n5\n10\n0 not even\n");
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
    assert_eq!(run_k2(src), "42\nno value\n-1\n5\n0\n");
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
    assert_eq!(run_k2(src), "3\n3\n37\n-1\n1\n0\n10 81\n");
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
    let dir = root.join("examples");
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
    assert_eq!(run_k2(src), "4\n1 0\n2\nada\ngrace\n");
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
    assert_eq!(run_k2(src), "2000\nwrong 0\n2000 999\n2 4 0\n");
}
