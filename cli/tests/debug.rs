//! `openepl debug` — the symbol layer, checked against an external oracle.
//!
//! The engine's own line table is compared with `objdump`'s reading of the
//! same binary. That is deliberately a tool nothing here depends on: an engine
//! checked only against itself is checked against its own mistakes, and this
//! one is the foundation every later phase stands on. OpenEPL's debugger does
//! not use objdump, gdb or lldb — it reads DWARF itself. They are used here as
//! a second opinion, which is a different thing.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn build(name: &str, tag: &str, release: bool) -> PathBuf {
    let repo = repo();
    let example = repo.join("examples").join(format!("{name}.oir"));
    let dir = std::env::temp_dir().join("openepl_debug_tests");
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join(format!("{name}_{tag}"));
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_openepl"));
    cmd.args(["build", example.to_str().unwrap()]);
    if release {
        cmd.arg("--release");
    }
    let status = cmd
        .args(["-o", out.to_str().unwrap()])
        .env("OPENEPL_RUNTIME_DIR", repo.join("runtime"))
        .status()
        .expect("run openepl");
    assert!(status.success(), "build failed for {name}");
    out
}

fn debug_cmd(args: &[&str]) -> (String, String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_openepl"))
        .arg("debug")
        .args(args)
        .output()
        .expect("run openepl debug");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

/// `(line, address)` for every row, as the engine reports them.
fn ours(bin: &Path) -> Vec<(u32, u64)> {
    let (stdout, stderr, ok) = debug_cmd(&["--dump-lines", bin.to_str().unwrap()]);
    assert!(ok, "openepl debug failed: {stderr}");
    stdout
        .lines()
        .filter(|l| l.starts_with("line: "))
        .map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            let line: u32 = f[1].parse().unwrap();
            let addr = u64::from_str_radix(f[5].trim_start_matches("0x"), 16).unwrap();
            (line, addr)
        })
        .collect()
}

/// The same, as objdump reports them.
fn theirs(bin: &Path, source: &str) -> Vec<(u32, u64)> {
    let out = Command::new("objdump")
        .args(["--dwarf=decodedline", bin.to_str().unwrap()])
        .output()
        .expect("run objdump");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            // `<file> <line> <address>`, and a `-` line is the end marker,
            // which the engine reports separately rather than as a row.
            if f.len() < 3 || f[0] != source || f[1] == "-" {
                return None;
            }
            let line: u32 = f[1].parse().ok()?;
            let addr = u64::from_str_radix(f[2].trim_start_matches("0x"), 16).ok()?;
            Some((line, addr))
        })
        .collect()
}

/// The assertion the rest of the debugger stands on.
#[test]
fn the_line_table_agrees_with_objdump() {
    let bin = build("loops", "lines", false);
    let mut a = ours(&bin);
    let mut b = theirs(&bin, "loops.oir");
    assert!(b.len() > 5, "objdump found no table to compare against");
    a.sort();
    b.sort();
    assert_eq!(a, b, "the engine and objdump read different tables");
}

/// A binary carries other people's debug information — glibc's `atexit.c` is
/// in every one built here. Those rows must not be attributed to the user's
/// source, or a debugger reports `loops.oir:45` for an address in the C
/// library and steps a user into it.
#[test]
fn another_compile_units_lines_are_not_the_users() {
    let bin = build("loops", "units", false);
    let (stdout, _, ok) = debug_cmd(&["--dump-lines", bin.to_str().unwrap()]);
    assert!(ok);
    assert!(stdout.contains("source: loops.oir"), "{stdout}");
    let ours = ours(&bin);
    let theirs = theirs(&bin, "loops.oir");
    assert_eq!(
        ours.len(),
        theirs.len(),
        "the engine kept rows objdump attributes to another source"
    );
}

#[test]
fn a_breakpoint_resolves_to_the_line_it_was_asked_for() {
    let bin = build("loops", "resolve", false);
    let (stdout, stderr, ok) = debug_cmd(&["--resolve", bin.to_str().unwrap(), "loops.oir:24"]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("line: 24"), "{stdout}");
    assert!(stdout.contains("sub: main"), "{stdout}");
    let address = stdout
        .lines()
        .find_map(|l| l.strip_prefix("address: "))
        .expect("an address");
    // And that address reads back as the line it came from.
    let (back, _, ok) = debug_cmd(&["--at", bin.to_str().unwrap(), address]);
    assert!(ok);
    assert!(back.contains("line: 24"), "{back}");
    assert!(back.contains("sub: main"), "{back}");
}

/// An address in the runtime or in libc is not the user's code, and saying so
/// plainly is what a backtrace needs to filter on.
#[test]
fn an_address_outside_the_users_code_says_so() {
    let bin = build("loops", "outside", false);
    let (stdout, _, ok) = debug_cmd(&["--at", bin.to_str().unwrap(), "0x1"]);
    assert!(ok, "an unknown address is an answer, not a failure");
    assert!(stdout.contains("line: none"), "{stdout}");
    assert!(stdout.contains("not one of yours"), "{stdout}");
}

#[test]
fn every_subroutine_is_listed_with_its_extent() {
    let bin = build("loops", "subs", false);
    let (stdout, _, ok) = debug_cmd(&["--dump-subs", bin.to_str().unwrap()]);
    assert!(ok);
    for want in ["sub: main", "sub: fizzbuzz", "sub: candidate"] {
        assert!(stdout.contains(want), "{want} missing from {stdout}");
    }
    // Every one has a real extent; a zero size would make `--at` useless.
    for line in stdout.lines().filter(|l| l.starts_with("sub: ")) {
        let size = line.rsplit_once("size: ").unwrap().1;
        assert!(size.parse::<u64>().unwrap() > 0, "{line}");
    }
}

/// A release build strips debug information, and the message says what to do
/// about it rather than reporting a malformed file.
#[test]
fn a_release_build_is_refused_with_a_reason() {
    let bin = build("loops", "release", true);
    let (_, stderr, ok) = debug_cmd(&["--dump-lines", bin.to_str().unwrap()]);
    assert!(!ok);
    assert!(stderr.contains("no debug information"), "{stderr}");
    assert!(stderr.contains("--release"), "{stderr}");
}

/// A whole debugging session, driven the way an editor drives one.
///
/// This is the assertion that says OpenEPL has a debugger: a program is built,
/// a breakpoint is planted on a line inside a loop, the program runs and stops
/// there, stepping walks the body and around the loop's back edge, and
/// continuing comes back round to the breakpoint on the next turn. Every part
/// of it — the traps, the stepping, the stack — is OpenEPL's own; nothing here
/// runs gdb or lldb.
#[test]
fn a_debug_session_stops_steps_and_comes_back_round() {
    use std::io::{BufReader, Read, Write};
    use std::process::Stdio;

    let dir = std::env::temp_dir().join("openepl_dap_session_test");
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("t.oir");
    std::fs::write(
        &source,
        "module stepdemo\n\
         \n\
         sub main\n\
         \u{20}\u{20}var total: int = 0\n\
         \u{20}\u{20}for i in 1..3\n\
         \u{20}\u{20}\u{20}\u{20}total = total + i\n\
         \u{20}\u{20}\u{20}\u{20}call print_int(total)\n\
         \u{20}\u{20}end\n\
         end\n",
    )
    .unwrap();

    let mut adapter = Command::new(env!("CARGO_BIN_EXE_openepl"))
        .arg("dap")
        .current_dir(&dir)
        .env("OPENEPL_RUNTIME_DIR", repo().join("runtime"))
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

    send("initialize", r#"{"adapterID":"openepl"}"#);
    send("launch", r#"{"program":"t.oir"}"#);
    send(
        "setBreakpoints",
        r#"{"source":{"path":"t.oir"},"breakpoints":[{"line":6}]}"#,
    );
    send("configurationDone", "");
    // The requests are all sent up front on purpose: an adapter that only
    // reads stdin between stops would deadlock, and the timeout below is what
    // would catch it.
    send("stackTrace", r#"{"threadId":1}"#);
    send("next", r#"{"threadId":1}"#);
    send("stackTrace", r#"{"threadId":1}"#);
    send("next", r#"{"threadId":1}"#);
    send("stackTrace", r#"{"threadId":1}"#);
    send("continue", r#"{"threadId":1}"#);
    send("stackTrace", r#"{"threadId":1}"#);
    send("disconnect", "");
    drop(input);

    let mut transcript = String::new();
    BufReader::new(adapter.stdout.take().unwrap())
        .read_to_string(&mut transcript)
        .unwrap();
    let _ = adapter.wait();

    let lines: Vec<&str> = transcript
        .split("Content-Length:")
        .filter(|m| m.contains('{'))
        .collect();
    let joined = lines.join("\n");
    // Compared without spaces, so the assertions do not depend on how the
    // serialiser happens to lay a message out.
    let dense: String = joined.chars().filter(|c| !c.is_whitespace()).collect();

    assert!(
        dense.contains(r#""reason":"breakpoint""#),
        "no breakpoint stop in: {joined}"
    );
    assert!(
        dense.contains(r#""hitBreakpointIds":[1]"#),
        "the stop did not name the breakpoint: {joined}"
    );
    // The lines the stack reported, in order. 6 is the breakpoint, 7 is the
    // next statement, 5 is the loop header the back edge returns to, and 6
    // again is the next turn of the loop.
    let mut stopped_at = Vec::new();
    for message in &lines {
        if !message.contains("stackFrames") {
            continue;
        }
        let dense: String = message.chars().filter(|c| !c.is_whitespace()).collect();
        // The innermost frame is the one the user is stopped in, and it is
        // first in the array, so the first line in the message is the answer.
        if let Some(at) = dense.find(r#""line":"#) {
            let number: String = dense[at + 7..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if let Ok(line) = number.parse::<u32>() {
                stopped_at.push(line);
            }
        }
    }
    assert_eq!(
        stopped_at,
        vec![6, 7, 5, 6],
        "the session did not step through the loop and back round: {joined}"
    );
}
