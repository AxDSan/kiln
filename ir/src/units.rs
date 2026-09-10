//! Units: a program split across files.
//!
//! A whole program is one module, and that stays true — what this adds is a
//! way to *assemble* that module from more than one file. A file that starts
//! `unit <name>` holds subroutines, records, constants, `dll` declarations
//! and module variables, and nothing that runs: no `main`, no form, no
//! `target`. A program (or another unit) pulls it in with the same `use <name>`
//! it uses for a support library, and `<name>.kiln` beside the file that says
//! `use` is what resolves. A name that has no such file beside it is a library,
//! exactly as before, so every existing program means what it meant.
//!
//! Resolution is a merge: the unit's items are appended to the program's,
//! the libraries the unit itself `use`s are added to the program's, and the
//! unit's own name leaves the `use` list — the compiler introspects libraries,
//! not units. Everything downstream (validation, lowering, the header writer)
//! sees one module and does not know a unit was involved.
//!
//! Two things are refused rather than guessed at. A name declared in two
//! places — the program and a unit, or two units — is an error naming both
//! files, because a silent "first one wins" is how the wrong subroutine gets
//! called. And a unit is not a program: `main` or a form in one is an error,
//! since a unit that could also run is two things with one name.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::parser::{parse_with, ParseOptions};
use crate::{Item, Module};

/// A program with its units merged in, and where each merged name came from.
#[derive(Debug)]
pub struct Resolved {
    pub module: Module,
    /// Every subroutine, record, constant, `dll` and variable a unit
    /// contributed, and the unit file that declared it. A diagnostic that
    /// names one of these can say which file to open.
    pub origins: HashMap<String, PathBuf>,
    /// The unit files that were merged, in the order they were reached.
    pub units: Vec<PathBuf>,
}

/// Why a unit could not be merged: which file, where in it, and what.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitError {
    pub file: PathBuf,
    /// 1-based line in `file`; 0 when the error is about the file as a whole.
    pub line: usize,
    pub msg: String,
}

impl std::fmt::Display for UnitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.line > 0 {
            write!(f, "{}:{}: {}", self.file.display(), self.line, self.msg)
        } else {
            write!(f, "{}: {}", self.file.display(), self.msg)
        }
    }
}

impl std::error::Error for UnitError {}

/// The unit file a `use <name>` from a file in `dir` would resolve to, if
/// there is one. `None` means "a library": either there is no such file, or
/// the file is a program — `examples/hello.kiln` beside `examples/hellolib.kiln`
/// must not capture `use hello`, which names the support library. Only a file
/// whose header says `unit` is a unit; the header is sniffed, not parsed, so
/// a unit with a mistake further down is still found and then reported.
pub fn unit_path(dir: &Path, name: &str) -> Option<PathBuf> {
    let p = dir.join(format!("{name}.kiln"));
    let src = std::fs::read_to_string(&p).ok()?;
    let header = src
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))?;
    // `unit` alone, not `unit <name>`: a header that names the wrong unit is
    // the resolver's error to report, with the file and both names.
    if header.split_whitespace().next() == Some("unit") {
        Some(p)
    } else {
        None
    }
}

/// Merge every unit `program` uses — and every unit those use — into it.
///
/// `dir` is the directory of the program's own file, which is where its
/// `use` names are looked up; a unit's `use` names are looked up beside the
/// unit. `opts` is applied to every unit parsed, so a release build strips a
/// unit's asserts the same way it strips the program's.
pub fn resolve_units(
    program: Module,
    dir: &Path,
    opts: ParseOptions,
) -> Result<Resolved, UnitError> {
    let mut names: HashMap<String, PathBuf> = HashMap::new();
    let program_file = dir.join(format!("{}.kiln", program.name));
    for name in declared(&program) {
        names.insert(name, program_file.clone());
    }

    let mut merged = program;
    let mut origins = HashMap::new();
    let mut units: Vec<PathBuf> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut libs: Vec<String> = Vec::new();
    // (name as written, directory to look beside)
    let mut pending: Vec<(String, PathBuf)> =
        merged.uses.drain(..).map(|u| (u, dir.to_path_buf())).collect();

    while !pending.is_empty() {
        let (name, from) = pending.remove(0);
        let Some(path) = unit_path(&from, &name) else {
            if !libs.contains(&name) {
                libs.push(name);
            }
            continue;
        };
        let key = path.canonicalize().unwrap_or_else(|_| path.clone());
        if !seen.insert(key) {
            continue;
        }

        let src = std::fs::read_to_string(&path).map_err(|e| UnitError {
            file: path.clone(),
            line: 0,
            msg: format!("cannot read: {e}"),
        })?;
        let unit = parse_with(&src, opts).map_err(|e| UnitError {
            file: path.clone(),
            line: e.line,
            msg: e.msg,
        })?;
        if !unit.is_unit {
            return Err(UnitError {
                file: path.clone(),
                line: 1,
                msg: format!(
                    "`use {name}` found this file, but it is a program (`module`), not a \
                     unit — a file meant to be used starts with `unit {name}`"
                ),
            });
        }
        if unit.name != name {
            return Err(UnitError {
                file: path.clone(),
                line: 1,
                msg: format!(
                    "the file is `{name}.kiln` but declares `unit {}` — the two must agree",
                    unit.name
                ),
            });
        }
        if let Some(form) = unit.forms().next() {
            return Err(UnitError {
                file: path.clone(),
                line: 0,
                msg: format!(
                    "a unit cannot declare a form (`{}`) — a form belongs to the program",
                    form.name
                ),
            });
        }
        if let Some(main) = unit.subs().find(|s| s.name == "main") {
            return Err(UnitError {
                file: path.clone(),
                line: main.name_span.line,
                msg: "a unit has no `main` — the program that uses it is what runs".into(),
            });
        }

        for n in declared(&unit) {
            if let Some(other) = names.get(&n) {
                return Err(UnitError {
                    file: path.clone(),
                    line: 0,
                    msg: format!(
                        "`{n}` is declared here and in {} — one program, one `{n}`",
                        other.display()
                    ),
                });
            }
            names.insert(n.clone(), path.clone());
            origins.insert(n, path.clone());
        }

        let unit_dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        for u in unit.uses {
            pending.push((u, unit_dir.clone()));
        }
        merged.items.extend(unit.items);
        units.push(path);
    }

    merged.uses = libs;
    Ok(Resolved {
        module: merged,
        origins,
        units,
    })
}

/// Every name a module declares at its top level. One namespace on purpose:
/// a record and a subroutine with one name would be a collision the
/// validator reports anyway, and reporting it here names the file.
fn declared(m: &Module) -> Vec<String> {
    m.items
        .iter()
        .filter_map(|i| match i {
            Item::Sub(s) => Some(s.name.clone()),
            Item::UserType(r) => Some(r.name.clone()),
            Item::Const(c) => Some(c.name.clone()),
            Item::Dll(d) => Some(d.name.clone()),
            Item::Var(v) => Some(v.name.clone()),
            Item::Form(_) | Item::Component(_) => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("kiln_units_{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_unit_beside_the_program_is_merged_and_a_library_is_not() {
        let d = scratch("merge");
        std::fs::write(
            d.join("maths.kiln"),
            "unit maths\nuse db\nconst TWO = 2\nsub twice(n: int): int\n  return n * TWO\nend\n",
        )
        .unwrap();
        let prog = parse("module app\nuse maths\nuse system\nsub main\n  call print_int(twice(3))\nend\n")
            .unwrap();
        let r = resolve_units(prog, &d, ParseOptions::default()).unwrap();
        assert_eq!(r.module.uses, vec!["system", "db"]);
        assert_eq!(r.units.len(), 1);
        assert!(r.module.subs().any(|s| s.name == "twice"));
        assert_eq!(r.origins["twice"], d.join("maths.kiln"));
        assert_eq!(r.origins["TWO"], d.join("maths.kiln"));
    }

    #[test]
    fn a_unit_may_use_another_unit_and_each_is_merged_once() {
        let d = scratch("chain");
        std::fs::write(d.join("a.kiln"), "unit a\nuse b\nsub fa(): int\n  return fb()\nend\n").unwrap();
        std::fs::write(d.join("b.kiln"), "unit b\nsub fb(): int\n  return 1\nend\n").unwrap();
        let prog = parse("module app\nuse a\nuse b\nsub main\n  call print_int(fa())\nend\n").unwrap();
        let r = resolve_units(prog, &d, ParseOptions::default()).unwrap();
        assert_eq!(r.units.len(), 2);
        assert_eq!(r.module.subs().filter(|s| s.name == "fb").count(), 1);
    }

    #[test]
    fn a_name_in_two_files_is_refused_naming_both() {
        let d = scratch("dup");
        std::fs::write(d.join("u.kiln"), "unit u\nsub helper()\nend\n").unwrap();
        let prog = parse("module app\nuse u\nsub helper()\nend\nsub main\nend\n").unwrap();
        let e = resolve_units(prog, &d, ParseOptions::default()).unwrap_err();
        assert!(e.msg.contains("`helper` is declared here and in"), "{e}");
        assert!(e.msg.contains("app.kiln"), "{e}");
    }

    #[test]
    fn a_program_beside_the_program_is_not_a_unit_and_a_unit_is_not_a_program() {
        let d = scratch("kinds");
        // `p.kiln` is a program, so `use p` is a library named p — left to the
        // loader, exactly as before units existed.
        std::fs::write(d.join("p.kiln"), "module p\nsub main\nend\n").unwrap();
        let prog = parse("module app\nuse p\nsub main\nend\n").unwrap();
        let r = resolve_units(prog, &d, ParseOptions::default()).unwrap();
        assert_eq!(r.module.uses, vec!["p"]);
        assert!(r.units.is_empty());

        std::fs::write(d.join("q.kiln"), "unit q\nsub main\nend\n").unwrap();
        let prog = parse("module app\nuse q\nsub main\nend\n").unwrap();
        let e = resolve_units(prog, &d, ParseOptions::default()).unwrap_err();
        assert!(e.msg.contains("a unit has no `main`"), "{e}");
        assert_eq!(e.line, 2);
    }

    #[test]
    fn a_unit_whose_header_names_another_unit_is_refused() {
        let d = scratch("misnamed");
        std::fs::write(d.join("maths.kiln"), "unit math\nsub f()\nend\n").unwrap();
        let prog = parse("module app\nuse maths\nsub main\nend\n").unwrap();
        let e = resolve_units(prog, &d, ParseOptions::default()).unwrap_err();
        assert!(e.msg.contains("declares `unit math`"), "{e}");
    }

    #[test]
    fn a_unit_refuses_a_target() {
        let e = parse("unit u\ntarget console\n").unwrap_err();
        assert!(e.msg.contains("a unit has no `target`"), "{e}");
    }
}
