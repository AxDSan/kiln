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
