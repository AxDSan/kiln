//! The Kiln 2 gaps the status note listed as parsed-but-not-lowered, and one
//! parser bug beside them. Every test either builds a K2 program and checks its
//! stdout, or builds one that must be refused and checks the diagnostic names
//! what is wrong — no panic, no wrong code.

use std::path::PathBuf;
use std::process::Command;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join("k2-gaps");
    std::fs::create_dir_all(&d).unwrap();
    d.join(name)
}

fn write(name: &str, src: &str) -> (PathBuf, PathBuf) {
    let path = tmp(&format!("{name}.kiln"));
    let exe = tmp(name);
    std::fs::write(&path, src).unwrap();
    (path, exe)
}

fn kiln(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(args)
        .output()
        .expect("kiln")
}

/// Build a K2 program and run it, returning stdout.
fn build_and_run(name: &str, src: &str) -> String {
    let (path, exe) = write(name, src);
    let out = kiln(&["k2", path.to_str().unwrap(), "-o", exe.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "build failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&exe).output().expect("the program runs");
    assert!(run.status.success(), "exited {:?}", run.status.code());
    String::from_utf8_lossy(&run.stdout).to_string()
}

/// Build a program that must be refused, returning its diagnostic.
fn build_error(name: &str, src: &str) -> String {
    let (path, exe) = write(name, src);
    let out = kiln(&["k2", path.to_str().unwrap(), "-o", exe.to_str().unwrap()]);
    assert!(
        !out.status.success(),
        "this was expected to be refused, but it built"
    );
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// Build a program that must compile, then run it, returning
/// `(succeeded, stdout, stderr)` — for a stop with a message rather than a
/// printed result.
fn build_and_run_stopped(name: &str, src: &str) -> (bool, String, String) {
    let (path, exe) = write(name, src);
    let out = kiln(&["k2", path.to_str().unwrap(), "-o", exe.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "build failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&exe).output().expect("the program runs");
    (
        run.status.success(),
        String::from_utf8_lossy(&run.stdout).to_string(),
        String::from_utf8_lossy(&run.stderr).to_string(),
    )
}

// ─── x! and x?. ─────────────────────────────────────────────────────────────

#[test]
fn null_forgiving_reads_through_an_optional() {
    // `x!` is the assertion that makes a `T?` read as a `T`. Without it the
    // member access below was a compile error, which is the whole point of the
    // operator.
    let out = build_and_run(
        "forgive",
        r#"
namespace Forgive;

public record Person(string Name);

public static class P
{
    public static void Main()
    {
        Person? ada = new Person("ada");
        Console.WriteLine(ada!.Name);
        // A value that is already a `T` comes back unchanged.
        var n = 5;
        Console.WriteLine($"{n!}");
    }
}
"#,
    );
    assert_eq!(out, "ada\n5\n");
}

#[test]
fn null_conditional_member_is_skipped_when_the_receiver_is_empty() {
    // Reading `.In.N` through an empty optional must not touch the memory the
    // optional does not have. `-1` falling out of the empty case is the proof
    // the access did not run.
    let out = build_and_run(
        "cond_member",
        r#"
namespace CondMember;

public record Inner(int N);
public record Outer(Inner In);

public static class P
{
    public static void Main()
    {
        Outer? some = new Outer(new Inner(5));
        Outer? none = null;
        Console.WriteLine($"{some?.In.N ?? -1}");
        Console.WriteLine($"{none?.In.N ?? -1}");
    }
}
"#,
    );
    assert_eq!(out, "5\n-1\n");
}

#[test]
fn null_conditional_chains_a_call_and_a_member_together() {
    // `x?.M()` and `x?.M.N` are one chain: the receiver is tested once and
    // everything after the `?` runs only when it holds a value.
    let out = build_and_run(
        "cond_chain",
        r#"
namespace CondChain;

public record Box(int N)
{
    public int Doubled() => N * 2;
}

public static class P
{
    public static void Main()
    {
        Box? b = new Box(4);
        Box? none = null;
        Console.WriteLine($"{b?.Doubled() ?? -1}");
        Console.WriteLine($"{none?.Doubled() ?? -1}");
        Console.WriteLine($"{b?.N ?? -1}");
    }
}
"#,
    );
    assert_eq!(out, "8\n-1\n4\n");
}

#[test]
fn null_conditional_index_yields_nothing_for_an_empty_list() {
    // `x?[i]` against a list that is not there, and against one that is. The
    // index is only checked when the list is present.
    let out = build_and_run(
        "cond_index",
        r#"
namespace CondIndex;

public static class P
{
    public static void Main()
    {
        var ys = new List<int>();
        ys.Add(7);
        List<int>? present = ys;
        List<int>? missing = null;
        Console.WriteLine($"{missing?[1] ?? -1}");
        Console.WriteLine($"{present?[1] ?? -1}");
    }
}
"#,
    );
    assert_eq!(out, "-1\n7\n");
}

#[test]
fn null_conditional_on_a_value_that_cannot_be_null_is_a_compile_error() {
    let err = build_error(
        "cond_bad",
        r#"
namespace CondBad;

public static class P
{
    public static void Main()
    {
        var n = 3;
        Console.WriteLine($"{n?.ToString()}");
    }
}
"#,
    );
    assert!(
        err.contains("reads through a value that may be null"),
        "the diagnostic does not say what is wrong: {err}"
    );
}

#[test]
fn null_forgiving_on_a_result_is_a_compile_error() {
    // `!` is not the way to unwrap a Result, and saying so beats a read of the
    // wrong half of the pair.
    let err = build_error(
        "forgive_bad",
        r#"
namespace ForgiveBad;

public static class P
{
    public static Result<int> Get() => 7;

    public static void Main()
    {
        var r = Get();
        Console.WriteLine($"{r!}");
    }
}
"#,
    );
    assert!(
        err.contains("`!` applies to a `T?`"),
        "the diagnostic does not name the operator: {err}"
    );
}

// ─── Any / First ────────────────────────────────────────────────────────────

#[test]
fn any_and_first_take_a_predicate() {
    // The two ends of a query: `Any` asks whether one exists, `First` answers
    // with it. Both follow the mechanism `Where`/`Select` already use.
    let out = build_and_run(
        "any_first",
        r#"
namespace AnyFirst;

public static class P
{
    public static void Main()
    {
        var xs = new List<int>();
        foreach (var i in 1..10)
            xs.Add(i);
        Console.WriteLine($"{xs.Any(x => x > 9)} {xs.Any(x => x > 10)}");
        Console.WriteLine($"{xs.First(x => x > 3)}");
        Console.WriteLine($"{xs.Where(x => x % 2 == 0).Any(x => x > 6)}");
    }
}
"#,
    );
    assert_eq!(out, "true false\n4\ntrue\n");
}

#[test]
fn first_with_no_match_stops_by_name() {
    // There is no value to answer with, so the program stops with a message
    // naming `First` rather than reading a zero that looks like a result.
    let (ok, stdout, stderr) = build_and_run_stopped(
        "first_missing",
        r#"
namespace FirstMissing;

public static class P
{
    public static void Main()
    {
        var xs = new List<int>();
        xs.Add(1);
        xs.Add(2);
        Console.WriteLine(xs.First(x => x > 10));
    }
}
"#,
    );
    assert!(!ok, "a First with no match should stop");
    assert_eq!(stdout, "");
    assert!(
        stderr.contains("First found no element"),
        "the stop message does not name First: {stderr}"
    );
}

#[test]
fn any_without_a_predicate_is_a_compile_error() {
    let err = build_error(
        "any_noarg",
        r#"
namespace AnyNoArg;

public static class P
{
    public static void Main()
    {
        var xs = new List<int>();
        xs.Add(1);
        Console.WriteLine($"{xs.Any()}");
    }
}
"#,
    );
    assert!(
        err.contains("List.Any takes one argument"),
        "the diagnostic does not name the method: {err}"
    );
}

#[test]
fn any_with_a_non_predicate_is_a_compile_error() {
    // A number where the predicate goes used to reach KIR as an integer stored
    // into a function slot, which clang then refused far from the source.
    let err = build_error(
        "any_notfn",
        r#"
namespace AnyNotFn;

public static class P
{
    public static void Main()
    {
        var xs = new List<int>();
        xs.Add(1);
        Console.WriteLine($"{xs.Any(3)}");
    }
}
"#,
    );
    assert!(
        err.contains("List.Any") && err.contains("wants a function"),
        "the diagnostic does not name the argument: {err}"
    );
}

#[test]
fn order_by_refuses_a_key_it_cannot_compare() {
    // There is no user comparator to call, so the key has to be something the
    // language itself orders. A record key is refused where it is written
    // rather than reaching clang as a `gt` it cannot emit.
    let err = build_error(
        "order_by_badkey",
        r#"
namespace OrderByBadKey;

public record Box(int V);

public static class P
{
    public static void Main()
    {
        var xs = new List<Box>();
        xs.Add(new Box(1));
        foreach (var b in xs.OrderBy(x => x))
            Console.WriteLine(b.V);
    }
}
"#,
    );
    assert!(
        err.contains("OrderBy") && err.contains("can order"),
        "the diagnostic does not name the key: {err}"
    );
}

#[test]
fn order_by_without_a_selector_is_a_compile_error() {
    let err = build_error(
        "order_by_noarg",
        r#"
namespace OrderByNoArg;

public static class P
{
    public static void Main()
    {
        var xs = new List<int>();
        xs.Add(1);
        foreach (var x in xs.OrderBy())
            Console.WriteLine(x);
    }
}
"#,
    );
    assert!(
        err.contains("List.OrderBy takes one key selector"),
        "the diagnostic does not name the method: {err}"
    );
}

// ─── generic constraints: class and new() ───────────────────────────────────

#[test]
fn class_and_new_constraints_admit_what_they_name() {
    // A record is a reference, so `where T : class` accepts it; a class with a
    // parameterless constructor satisfies `where T : new()` and `new T()`
    // inside the instance runs that constructor.
    let out = build_and_run(
        "constraints",
        r#"
namespace Constraints;

public record Box(int N);
public class Made
{
    public int N;
    public Made() { N = 7; }
}

public static class P
{
    public static string Kind<T>(T x) where T : class => "ref";
    public static T Fresh<T>(T seed) where T : new()
    {
        var made = new T();
        return made;
    }

    public static void Main()
    {
        Console.WriteLine(Kind(new Box(1)));
        Console.WriteLine(Kind("text"));
        Console.WriteLine($"{Fresh(new Made()).N}");
    }
}
"#,
    );
    assert_eq!(out, "ref\nref\n7\n");
}

#[test]
fn class_constraint_rejects_a_value_type() {
    let err = build_error(
        "class_bad",
        r#"
namespace ClassBad;

public static class P
{
    public static string Kind<T>(T x) where T : class => "ref";
    public static void Main() { Console.WriteLine(Kind(3)); }
}
"#,
    );
    assert!(
        err.contains("to be a reference type") && err.contains("where T : class"),
        "the diagnostic does not name the constraint: {err}"
    );
}

#[test]
fn new_constraint_rejects_a_type_without_a_parameterless_constructor() {
    let err = build_error(
        "new_bad",
        r#"
namespace NewBad;

public class Needs
{
    public int N;
    public Needs(int n) { N = n; }
}

public static class P
{
    public static T Fresh<T>(T seed) where T : new()
    {
        var made = new T();
        return made;
    }

    public static void Main() { Console.WriteLine($"{Fresh(new Needs(1)).N}"); }
}
"#,
    );
    assert!(
        err.contains("parameterless constructor") && err.contains("where T : new()"),
        "the diagnostic does not name the constraint: {err}"
    );
}

// ─── interpolation holes ────────────────────────────────────────────────────

#[test]
fn a_ternary_inside_an_interpolation_hole_keeps_its_colon() {
    // The parser used to split the hole at the first `:`, so a ternary lost its
    // else-arm. A hole holds exactly one expression, and a ternary is one.
    let out = build_and_run(
        "hole_ternary",
        r#"
namespace HoleTernary;

public static class P
{
    public static void Main()
    {
        var c = true;
        Console.WriteLine($"{c ? 1 : 2}");
        Console.WriteLine($"{c ? "yes" : "no"}");
        var d = false;
        Console.WriteLine($"{d ? 1 : 2}");
    }
}
"#,
    );
    assert_eq!(out, "1\nyes\n2\n");
}

#[test]
fn an_interpolation_format_specifier_is_refused_by_name() {
    // `$"{x:03}"` used to compile and print `7`, which is silent wrong output.
    // Refusing it and naming the specifier is the fix.
    let err = build_error(
        "hole_format",
        r#"
namespace HoleFormat;

public static class P
{
    public static void Main()
    {
        var x = 7;
        Console.WriteLine($"{x:03}");
    }
}
"#,
    );
    assert!(
        err.contains("format specifier is not supported") && err.contains("03"),
        "the diagnostic does not name the format specifier: {err}"
    );
}

#[test]
fn null_conditional_through_an_optional_member_stays_one_optional() {
    // `a?.Name` where `Name` is already a `string?` is a `string?`, as in C#.
    // It was an optional of an optional, which clang refused to zero.
    let out = build_and_run(
        "cond_flat",
        r#"
namespace CondFlat;

public record Inner(int N);
public record Mid(Inner? In, string? Name);

public static class P
{
    public static void Main()
    {
        Mid? a = new Mid(new Inner(5), "x");
        Mid? b = new Mid(null, null);
        Mid? c = null;
        Console.WriteLine(a?.Name ?? "none");
        Console.WriteLine(b?.Name ?? "none");
        Console.WriteLine(c?.Name ?? "none");
        Console.WriteLine($"{a?.In?.N ?? -1} {b?.In?.N ?? -1} {c?.In?.N ?? -1}");
    }
}
"#,
    );
    assert_eq!(out, "x\nnone\nnone\n5 -1 -1\n");
}

#[test]
fn order_by_a_bool_puts_false_first() {
    // An `i1` compared signed reads `true` as -1; the order must be C#'s.
    let out = build_and_run(
        "order_bool",
        r#"
namespace OrderBool;

public record Item(string Name, bool Done);

public static class P
{
    public static void Main()
    {
        var xs = new List<Item>();
        xs.Add(new Item("a", true));
        xs.Add(new Item("b", false));
        xs.Add(new Item("c", true));
        foreach (var x in xs.OrderBy(i => i.Done))
            Console.Write(x.Name);
        Console.WriteLine("");
    }
}
"#,
    );
    assert_eq!(out, "bac\n");
}

#[test]
fn optional_fields_and_elements_get_their_whole_size() {
    // A `T?` is `{ T, i1 }` and a function value is a pair: measured as one
    // pointer, a record of optionals was allocated half the bytes it wrote,
    // and a list of them overran its buffer. A number literal into a `long?`
    // is widened before it is wrapped.
    let out = build_and_run(
        "opt_size",
        r#"
namespace OptSize;

public record Inner(int N);
public record Mid(Inner? In, string? Name, long? L, bool? B);

public static class P
{
    public static void Main()
    {
        var xs = new List<int?>();
        for (int i = 1; i <= 50; i++) xs.Add(i);
        xs.Add(null);
        Mid? b = new Mid(null, null, 7, true);
        Func<int, int> f = x => x + 1;
        var fs = new List<Func<int, int>>();
        for (int i = 1; i <= 20; i++) fs.Add(f);
        var g = fs[20];
        long? n = 9;
        Console.WriteLine($"{xs[50] ?? -1} {xs[51] ?? -1} {b?.Name ?? "none"} {b?.L ?? 0} {g(4)} {n ?? 0}");
    }
}
"#,
    );
    assert_eq!(out, "50 -1 none 7 5 9\n");
}

#[test]
fn a_generic_method_takes_written_type_arguments() {
    // `Make<T>()` has no argument to infer `T` from, so it must be written;
    // written arguments also set the width a literal lowers to. `a < b` beside
    // them is still a comparison.
    let out = build_and_run(
        "type_args",
        r#"
namespace TypeArgs;

public record Point(int X, int Y);

public static class P
{
    public static T Make<T>() where T : new() => new T();
    public static T Pick<T>(T a, T b) => a;
    public static List<T> Two<T>(T a)
    {
        var xs = new List<T>();
        xs.Add(a);
        xs.Add(a);
        return xs;
    }

    public static void Main()
    {
        var xs = Make<List<int>>();
        xs.Add(3);
        var p = Make<Point>();
        long l = Pick<long>(5, 6);
        int a = 2;
        int b = 3;
        Console.WriteLine($"{xs.Count} {p.X} {l} {a < b} {a > b} {P.Two<string>("x").Count}");
    }
}
"#,
    );
    assert_eq!(out, "1 0 5 true false 2\n");
}

#[test]
fn an_optional_stored_where_a_plain_value_is_wanted_is_its_value_or_a_stop() {
    // The rule arguments always had, now for locals, assignments, returns and
    // fields too: they stored the `{value, present}` pair into a plain slot,
    // which clang refused.
    let out = build_and_run(
        "settle",
        r#"
namespace Settle;

public record Conn(int A);
public record Holder(Conn C);

public static class P
{
    static Dictionary<string, Conn> d = new Dictionary<string, Conn>();

    static Conn Find(string k) => d.Get(k);

    public static void Main()
    {
        d["x"] = new Conn(7);
        Conn c = d.Get("x");
        Conn e = new Conn(0);
        e = d.Get("x");
        var h = new Holder(d.Get("x"));
        Console.WriteLine($"{c.A} {e.A} {Find("x").A} {h.C.A}");
    }
}
"#,
    );
    assert_eq!(out, "7 7 7 7\n");
}
