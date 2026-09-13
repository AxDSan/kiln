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
fn capturing_lambda_is_reported_not_miscompiled() {
    let src = r#"
namespace C;
public static class P
{
    public static void Main()
    {
        var k = 10;
        Func<int, int> f = x => x + k;
        Console.WriteLine($"{f(1)}");
    }
}
"#;
    let err = kiln_k2::compile_to_llvm(src).unwrap_err();
    assert!(err.contains("captures `k`"), "unexpected error: {err}");
}
