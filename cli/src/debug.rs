//! `kiln debug` — what the debugger knows about a built program.
//!
//! At this stage it reads and reports; it does not run anything. That is the
//! order deliberately: the code that understands debug information is what
//! everything above it depends on, and it can be checked completely against a
//! binary sitting on disk, with no process to control and nothing to go wrong
//! asynchronously.
//!
//! The output is line-based, like `inspect` and `commands`, so a script and a
//! person can read it equally well.

use std::path::Path;

pub fn usage() {
    eprintln!(
        "kiln debug — read the debug information in a built program\n\n\
         USAGE:\n  \
         kiln debug --dump-lines <program>       the line table, one row per line\n  \
         kiln debug --dump-subs <program>        the subroutines and their extents\n  \
         kiln debug --resolve <program> <line>   where a breakpoint on that line goes\n  \
         kiln debug --at <program> <address>     which line an address is in\n\n\
         The program must have been built without `--release`, which strips\n\
         the debug information."
    );
}

pub fn run(args: &[String]) -> i32 {
    let (flag, rest) = match args.split_first() {
        Some(x) => x,
        None => {
            usage();
            return 2;
        }
    };
    let Some(bin) = rest.first() else {
        eprintln!("kiln: `{flag}` needs the path of a built program");
        return 2;
    };
    let program = match kiln_debug::load(Path::new(bin)) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("kiln: {bin}: {e}");
            return 1;
        }
    };

    match flag.as_str() {
        "--dump-lines" => {
            println!("source: {}", program.source);
            println!("directory: {}", program.directory);
            for row in program.rows() {
                if row.end_sequence {
                    println!("end: {:#x}", row.address);
                } else {
                    println!(
                        "line: {} column: {} address: {:#x} stmt: {}",
                        row.line,
                        row.column,
                        row.address,
                        if row.is_stmt { "yes" } else { "no" }
                    );
                }
            }
            0
        }
        "--dump-subs" => {
            for s in program.subprograms() {
                println!(
                    "sub: {} symbol: {} address: {:#x} size: {}",
                    s.name, s.symbol, s.low_pc, s.size
                );
            }
            0
        }
        "--resolve" => {
            let Some(want) = rest.get(1) else {
                eprintln!("kiln: `--resolve` needs a line number");
                return 2;
            };
            // `file:line` is accepted as well as a bare line, because that is
            // how a user says it and how an editor sends it. There is one
            // compile unit for now, so the file is checked rather than used to
            // choose between several.
            let (file, line) = match want.rsplit_once(':') {
                Some((f, l)) => (Some(f), l),
                None => (None, want.as_str()),
            };
            if let Some(f) = file {
                if !program.source.ends_with(f) && !f.ends_with(&program.source) {
                    eprintln!(
                        "kiln: this program was built from {}, not {f}",
                        program.source
                    );
                    return 1;
                }
            }
            let Ok(line) = line.parse::<u32>() else {
                eprintln!("kiln: `{line}` is not a line number");
                return 2;
            };
            match program.breakpoint_for(line) {
                Some(row) => {
                    println!("address: {:#x}", row.address);
                    println!("line: {}", row.line);
                    println!("column: {}", row.column);
                    if let Some(s) = program.subprogram_for(row.address) {
                        println!("sub: {}", s.name);
                    }
                    if row.line != line {
                        println!("moved: {line} does not run; the next line that does is used");
                    }
                    0
                }
                None => {
                    eprintln!("kiln: nothing runs at or after line {line}");
                    1
                }
            }
        }
        "--at" => {
            let Some(want) = rest.get(1) else {
                eprintln!("kiln: `--at` needs an address");
                return 2;
            };
            let text = want.strip_prefix("0x").unwrap_or(want);
            let Ok(address) = u64::from_str_radix(text, 16) else {
                eprintln!("kiln: `{want}` is not an address");
                return 2;
            };
            match program.line_for(address) {
                Some(row) => {
                    println!("line: {}", row.line);
                    println!("column: {}", row.column);
                    println!("source: {}", program.source);
                    match program.subprogram_for(address) {
                        Some(s) => println!("sub: {}", s.name),
                        None => println!("sub: (not one of yours)"),
                    }
                    0
                }
                None => {
                    // Not an error: most of a program's addresses are the
                    // runtime and libc, and "not your code" is the answer a
                    // backtrace filter wants.
                    println!("line: none");
                    println!("sub: (not one of yours)");
                    0
                }
            }
        }
        other => {
            eprintln!("kiln: unknown flag `{other}`");
            usage();
            2
        }
    }
}
