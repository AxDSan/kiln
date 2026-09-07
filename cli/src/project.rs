//! The project file — `project.kproj` — and `kiln project`.
//!
//! A project is a directory with a `project.kproj` in it, and the file is the
//! same `key: value` lines as `template.meta`: the CLI is the only reader of a
//! project file, so Studio learns a project's shape through `kiln project`
//! rather than by parsing this itself, exactly as it learns a form's shape
//! through `kiln inspect`. A second parser in C++ would drift from this one
//! the first time a field was added.
//!
//! Fields:
//!
//!   name:    what the project is called; also the default output name
//!   main:    the entry `.kiln`, relative to the project file
//!   target:  console | gui | sharedlib | staticlib
//!   kits:    the `use` set, so a fresh checkout knows what to `kit add`
//!   version: the PROJECT's version — not the toolchain's, which is what
//!            `kiln version` reports. `new` writes `0.1.0`.
//!
//! `build`, `run`, `emit` and `inspect` accept the file or its directory in
//! place of an `.kiln`, and resolve `main` from it.

use std::path::{Path, PathBuf};

use kiln_ir::Target;

pub const FILE_NAME: &str = "project.kproj";

#[derive(Debug)]
pub struct Project {
    /// The project file itself, as given (not canonicalised — a message that
    /// names a path should name the one the person typed).
    pub file: PathBuf,
    pub name: String,
    /// Resolved: the project's directory joined with the `main:` line, so a
    /// caller standing anywhere opens the right file.
    pub main: PathBuf,
    pub target: Option<Target>,
    pub kits: Vec<String>,
    pub version: String,
}

/// Is this argument a project rather than a source file?
///
/// A directory always is: there is nothing else `kiln build <dir>` could
/// mean. A file is one by extension only, so a source file named oddly is
/// still handed to the parser.
pub fn is_project_path(p: &Path) -> bool {
    p.is_dir() || p.extension().and_then(|e| e.to_str()) == Some("kproj")
}

/// The project file for a path that names a project or its directory.
pub fn locate(p: &Path) -> Result<PathBuf, String> {
    if p.is_dir() {
        let file = p.join(FILE_NAME);
        if file.is_file() {
            Ok(file)
        } else {
            Err(format!(
                "{} has no {FILE_NAME} — run `kiln new` to create a project, or name the .kiln directly",
                p.display()
            ))
        }
    } else if p.is_file() {
        Ok(p.to_path_buf())
    } else {
        Err(format!("cannot read {}: no such file or directory", p.display()))
    }
}

pub fn load(path: &Path) -> Result<Project, String> {
    let file = locate(path)?;
    let text = std::fs::read_to_string(&file)
        .map_err(|e| format!("cannot read {}: {e}", file.display()))?;
    let dir = file.parent().unwrap_or(Path::new(".")).to_path_buf();

    let mut name = String::new();
    let mut main = String::new();
    let mut target = None;
    let mut kits = Vec::new();
    let mut version = String::new();
    for line in text.lines() {
        // Comments and blank lines, so a person can annotate the file by hand.
        if line.trim_start().starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "name" => name = value.to_string(),
            "main" => main = value.to_string(),
            "target" => {
                target = Some(Target::parse(value).ok_or_else(|| {
                    format!(
                        "{}: unknown target `{value}` — expected console, gui, sharedlib or staticlib",
                        file.display()
                    )
                })?)
            }
            // Either shape reads the same, so a file written by hand with
            // commas is not wrong.
            "kits" => kits.extend(
                value
                    .split(|c: char| c == ',' || c.is_whitespace())
                    .filter(|s| !s.is_empty())
                    .map(str::to_string),
            ),
            "version" => version = value.to_string(),
            _ => {}
        }
    }
    // The entry is the one field with no sensible default: a project that
    // does not say what to build cannot be built.
    if main.is_empty() {
        return Err(format!("{}: no `main:` line", file.display()));
    }
    if name.is_empty() {
        name = dir
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("app")
            .to_string();
    }
    Ok(Project {
        main: dir.join(&main),
        file,
        name,
        target,
        kits,
        version,
    })
}

/// The file's text, in the shape `load` reads.
pub fn render(name: &str, main: &str, target: Target, kits: &[String], version: &str) -> String {
    format!(
        "name: {name}\nmain: {main}\ntarget: {}\nkits: {}\nversion: {version}\n",
        target.as_str(),
        kits.join(" ")
    )
}

/// The keys `kiln project ... set` accepts, in the order `render` writes
/// them. A key not on this list is rejected rather than written: a project
/// file may carry a hand-added line the toolchain does not know, and silently
/// accepting `targt: gui` would look like it worked.
pub const SETTABLE: &[&str] = &["name", "main", "target", "kits", "version"];

/// Apply `key: value` edits to a project file's text, in place.
///
/// Lines are rewritten where they stand rather than the file being
/// re-rendered from a parsed `Project`, so a comment, a blank line, a hand-
/// added key and the file's own field order all survive an edit made from
/// Studio. A key the file does not have yet is appended.
pub fn apply_edits(text: &str, edits: &[(String, String)]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut written: Vec<&str> = Vec::new();
    let ends_with_newline = text.is_empty() || text.ends_with('\n');

    for line in text.lines() {
        let key = if line.trim_start().starts_with('#') {
            None
        } else {
            line.split_once(':').map(|(k, _)| k.trim().to_string())
        };
        match key.as_deref().and_then(|k| edits.iter().find(|(ek, _)| ek == k)) {
            Some((k, v)) => {
                // The key itself, not its entry in SETTABLE: a key that is
                // not on that list would record "" and then be appended a
                // second time by the loop below, leaving the file with the
                // line twice.
                written.push(k.as_str());
                out.push(format!("{k}: {v}"));
            }
            None => out.push(line.to_string()),
        }
    }
    for (k, v) in edits {
        if !written.contains(&k.as_str()) {
            out.push(format!("{k}: {v}"));
        }
    }
    let mut joined = out.join("\n");
    if ends_with_newline || !joined.is_empty() {
        joined.push('\n');
    }
    joined
}

/// `kiln project <file-or-dir> set <key>=<value>...`
///
/// Studio's only way to change a project file: by the house rule the CLI is
/// the single reader — and writer — of `.kproj`, so a second parser in C++
/// cannot drift from this one.
fn cmd_project_set(path: &str, args: &[String]) -> i32 {
    let file = match locate(Path::new(path)) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("kiln: {e}");
            return 1;
        }
    };
    if args.is_empty() {
        eprintln!("kiln: usage: kiln project <project.kproj | directory> set <key>=<value>...");
        eprintln!("kiln: keys: {}", SETTABLE.join(", "));
        return 2;
    }

    let mut edits: Vec<(String, String)> = Vec::new();
    for a in args {
        let Some((key, value)) = a.split_once('=') else {
            eprintln!("kiln: `{a}` is not <key>=<value>");
            return 2;
        };
        let key = key.trim();
        let value = value.trim();
        if !SETTABLE.contains(&key) {
            eprintln!("kiln: cannot set `{key}` — keys: {}", SETTABLE.join(", "));
            return 2;
        }
        // Validated here rather than at the next build: a project file that
        // names a target nothing can parse is a file Studio wrote and cannot
        // open again.
        if key == "target" && Target::parse(value).is_none() {
            eprintln!(
                "kiln: unknown target `{value}` — expected console, gui, sharedlib or staticlib"
            );
            return 2;
        }
        if key == "main" && value.is_empty() {
            eprintln!("kiln: `main` is the entry file — it cannot be empty");
            return 2;
        }
        // A value carrying a newline would split into a second line and read
        // back as a different key entirely.
        if value.contains('\n') || value.contains('\r') {
            eprintln!("kiln: a value cannot contain a newline");
            return 2;
        }
        edits.retain(|(k, _)| k != key);
        edits.push((key.to_string(), value.to_string()));
    }

    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("kiln: cannot read {}: {e}", file.display());
            return 1;
        }
    };
    let updated = apply_edits(&text, &edits);
    if let Err(e) = std::fs::write(&file, &updated) {
        eprintln!("kiln: cannot write {}: {e}", file.display());
        return 1;
    }
    // Read it back through the same loader the build uses, so `set` reports
    // the file it actually produced rather than the edit it intended.
    match load(&file) {
        Ok(_) => {
            for (k, v) in &edits {
                println!("{k}: {v}");
            }
            0
        }
        Err(e) => {
            eprintln!("kiln: wrote {} but it no longer loads: {e}", file.display());
            1
        }
    }
}

/// `kiln project <file-or-dir>` — the resolved fields, one per line.
///
/// `main:` is printed resolved rather than as written, because the reader is
/// Studio deciding what to open, and Studio is not standing in the project's
/// directory. `kit:` is one line per kit, like `use:` in `inspect`, so a
/// reader never splits a value.
pub fn cmd_project(args: &[String]) -> i32 {
    let Some(path) = args.first() else {
        eprintln!("kiln: usage: kiln project <project.kproj | directory> [set <key>=<value>...]");
        return 2;
    };
    if args.get(1).map(String::as_str) == Some("set") {
        return cmd_project_set(path, &args[2..]);
    }
    if args.len() > 1 {
        eprintln!("kiln: unexpected argument `{}`", args[1]);
        return 2;
    }
    match load(Path::new(path)) {
        Ok(p) => {
            println!("project: {}", p.file.display());
            println!("name: {}", p.name);
            println!("main: {}", p.main.display());
            if let Some(t) = p.target {
                println!("target: {}", t.as_str());
            }
            for k in &p.kits {
                println!("kit: {k}");
            }
            if !p.version.is_empty() {
                println!("version: {}", p.version);
            }
            0
        }
        Err(e) => {
            eprintln!("kiln: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_and_load_agree() {
        let dir = std::env::temp_dir().join("kiln_project_unit");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let text = render("demo", "src/main.kiln", Target::Gui, &["ui".into(), "file".into()], "0.1.0");
        std::fs::write(dir.join(FILE_NAME), text).unwrap();

        let p = load(&dir).unwrap();
        assert_eq!(p.name, "demo");
        assert_eq!(p.main, dir.join("src/main.kiln"));
        assert_eq!(p.target, Some(Target::Gui));
        assert_eq!(p.kits, vec!["ui", "file"]);
        assert_eq!(p.version, "0.1.0");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn set_rewrites_a_field_and_keeps_the_rest() {
        let text = "# my project\nname: demo\nmain: src/main.kiln\ntarget: console\nkits: ui\nversion: 0.1.0\nauthor: someone\n";
        let out = apply_edits(
            text,
            &[("target".into(), "gui".into()), ("version".into(), "0.2.0".into())],
        );
        assert!(out.contains("target: gui"), "{out}");
        assert!(out.contains("version: 0.2.0"), "{out}");
        // The comment, the field order and a key the toolchain does not know
        // all survive: a project file is a file a person may have edited.
        assert!(out.starts_with("# my project\n"), "{out}");
        assert!(out.contains("author: someone"), "{out}");
        assert!(!out.contains("target: console"), "{out}");
        assert_eq!(out.lines().count(), text.lines().count());
    }

    #[test]
    fn an_edited_field_is_never_written_twice() {
        // `apply_edits` is public and does not itself police the key list, so
        // a key outside SETTABLE must still be rewritten in place rather than
        // rewritten *and* appended.
        let out = apply_edits("author: old\n", &[("author".into(), "new".into())]);
        assert_eq!(out, "author: new\n");
    }

    #[test]
    fn set_appends_a_field_the_file_lacks() {
        let out = apply_edits("main: a.kiln\n", &[("version".into(), "1.0.0".into())]);
        assert_eq!(out, "main: a.kiln\nversion: 1.0.0\n");
    }

    #[test]
    fn a_directory_without_a_project_says_so() {
        let dir = std::env::temp_dir().join("kiln_project_unit_empty");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let err = load(&dir).unwrap_err();
        assert!(err.contains(FILE_NAME), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
