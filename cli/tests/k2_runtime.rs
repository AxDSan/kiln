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
