//! The collector.
//!
//! Two properties, and they pull against each other. A collector that frees
//! nothing passes every correctness test here; a collector that frees too much
//! passes the memory test and corrupts the program. So each test asserts one
//! side, and neither is meaningful without the other.
//!
//! Every program below is written to a temp file rather than added to
//! `examples/`: they are shaped to stress the collector — millions of
//! iterations, values reachable only one way — not to teach anything, and the
//! examples directory is documentation.

use std::path::PathBuf;
use std::process::Command;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Compile `src` and run it, answering stdout. `tag` must be unique per test:
/// tests run in parallel and two writing one path race.
fn build_and_run(tag: &str, src: &str) -> String {
    let repo = repo();
    let dir = std::env::temp_dir();
    let file = dir.join(format!("kiln_gc_{tag}.kiln"));
    let bin = dir.join(format!("kiln_gc_{tag}"));
    std::fs::write(&file, src).expect("write source");

    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args([
            "build",
            file.to_str().unwrap(),
            "-o",
            bin.to_str().unwrap(),
        ])
        .env("KILN_RUNTIME_DIR", repo.join("runtime"))
        .output()
        .expect("run kiln");
    assert!(
        out.status.success(),
        "build failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let run = Command::new(&bin).output().expect("run the built program");
    assert!(
        run.status.success(),
        "the program exited non-zero:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    String::from_utf8_lossy(&run.stdout).into_owned()
}

/// The reason the collector exists: a loop that builds a value per iteration
/// used to hold every value it had ever built. Two million short texts came to
/// 220 MB. The assertion is on `memory_in_use`, not on resident set size —
/// the allocator does not return freed pages to the operating system promptly
/// and RSS would make this flaky — but the two were measured together.
#[test]
fn a_loop_that_discards_what_it_builds_does_not_grow() {
    let out = build_and_run(
        "loop",
        "module gcloop\ntarget console\n\n\
         sub main\n\
        \x20 var n: int = 0\n\
        \x20 for i = 1 to 2000000\n\
        \x20   let s: text = \"row {i} of the tick loop\"\n\
        \x20   n = n + length(s)\n\
        \x20 end\n\
        \x20 call print_int(n)\n\
        \x20 call print_int64(memory_in_use())\n\
         end\n",
    );
    let mut lines = out.lines();
    assert_eq!(lines.next().unwrap().trim(), "54888896", "wrong answer: {out}");
    let held: i64 = lines.next().unwrap().trim().parse().expect("a number");
    assert!(
        held < 8 * 1024 * 1024,
        "held {held} bytes at the end of a loop whose live set is one text"
    );
}

/// A module variable is the case a stack scan cannot cover: between the
/// subroutines that touch it, nothing on the stack refers to its value. It is
/// reachable only through the root table the backend emits, and this is the
/// test that the table is emitted, handed over, and read.
#[test]
fn a_value_held_only_by_a_module_variable_survives() {
    let out = build_and_run(
        "global",
        "module gcglobal\ntarget console\n\n\
         var kept: text = \"\"\n\
         var list: text[] = []\n\n\
         sub churn\n\
        \x20 for i = 1 to 400000\n\
        \x20   let junk: text = \"throwaway {i} with padding to make it real\"\n\
        \x20 end\n\
         end\n\n\
         sub main\n\
        \x20 kept = \"built \" + \"at run time\"\n\
        \x20 list = [\"one\", \"two\", \"three\"]\n\
        \x20 call churn()\n\
        \x20 call print_int64(collect_garbage())\n\
        \x20 call print_text(kept)\n\
        \x20 call print_text(\"{list[1]},{list[2]},{list[3]}\")\n\
         end\n",
    );
    assert!(out.contains("built at run time"), "the global was lost: {out}");
    assert!(out.contains("one,two,three"), "the global list was lost: {out}");
}

/// Two names for one array are two names for the same array — the promise that
/// ruled out copying values into per-call arenas. A collection must not quietly
/// break it, and a write through one name must still show through the other.
#[test]
fn aliasing_survives_a_collection() {
    let out = build_and_run(
        "alias",
        "module gcalias\ntarget console\n\n\
         sub main\n\
        \x20 var a: text[] = [\"x\", \"y\"]\n\
        \x20 var b: text[] = a\n\
        \x20 for i = 1 to 400000\n\
        \x20   let junk: text = \"throwaway {i} with padding to make it real\"\n\
        \x20 end\n\
        \x20 call print_int64(collect_garbage())\n\
        \x20 b[1] = \"mutated\"\n\
        \x20 call print_text(\"{a[1]},{a[2]}\")\n\
         end\n",
    );
    assert!(
        out.contains("mutated,y"),
        "the two names came apart across a collection: {out}"
    );
}

/// Reachability through structure, not just through a name: a text inside an
/// array inside a record inside a dictionary. Each hop is a different piece of
/// the tracer, and a program that only ever held flat values would not exercise
/// any of them.
#[test]
fn a_value_nested_three_deep_survives() {
    let out = build_and_run(
        "nested",
        "module gcnested\ntarget console\n\n\
         record Player\n\
        \x20 name: text\n\
        \x20 inventory: text[]\n\
         end\n\n\
         sub main\n\
        \x20 var roster: Player{} = {}\n\
        \x20 roster[\"ad\" + \"a\"] = Player{name: \"Ada \" + \"Lovelace\", \
         inventory: [\"a \" + \"note\", \"an \" + \"engine\"]}\n\
        \x20 for i = 1 to 400000\n\
        \x20   let junk: text = \"throwaway {i} with padding to make it real\"\n\
        \x20 end\n\
        \x20 call print_int64(collect_garbage())\n\
        \x20 let p: Player = roster[\"ada\"]\n\
        \x20 call print_text(\"{p.name} / {p.inventory[1]} / {p.inventory[2]}\")\n\
         end\n",
    );
    assert!(
        out.contains("Ada Lovelace / a note / an engine"),
        "something three levels down was collected: {out}"
    );
}

/// `KILN_GC=0` restores the behaviour every program had before the collector,
/// which is the escape hatch if it ever gets something wrong in the field. The
/// point is that the program still runs and still answers correctly.
#[test]
fn the_collector_can_be_turned_off() {
    let repo = repo();
    let dir = std::env::temp_dir();
    let file = dir.join("kiln_gc_off.kiln");
    let bin = dir.join("kiln_gc_off");
    std::fs::write(
        &file,
        "module gcoff\ntarget console\n\n\
         sub main\n\
        \x20 var n: int = 0\n\
        \x20 for i = 1 to 200000\n\
        \x20   let s: text = \"row {i}\"\n\
        \x20   n = n + length(s)\n\
        \x20 end\n\
        \x20 call print_int(n)\n\
         end\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", file.to_str().unwrap(), "-o", bin.to_str().unwrap()])
        .env("KILN_RUNTIME_DIR", repo.join("runtime"))
        .output()
        .expect("run kiln");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let on = Command::new(&bin).output().expect("run");
    let off = Command::new(&bin).env("KILN_GC", "0").output().expect("run");
    assert_eq!(
        String::from_utf8_lossy(&on.stdout),
        String::from_utf8_lossy(&off.stdout),
        "collection changed the answer"
    );
}
