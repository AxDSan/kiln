//! Project templates — `kiln templates` and `kiln new`.
//!
//! Templates live in `templates/<id>/`, each with a `template.meta` describing
//! it and one or more source files. `__MODULE__` in any file is replaced with
//! the module name and `__TITLE__` with the title — what a window's caption
//! shows, "Untitled App" until someone says otherwise — so a template's
//! caption is a project's name, not a module identifier; the rest replaced with
//! the new project's module name.
//!
//! They are exposed through the CLI rather than read directly by the designer,
//! for the same reason the designer never parses `.kiln` itself: one
//! reader, one format, no drift. Studio builds its New Project tiles from
//! `kiln templates` output, so adding a template adds a tile with no IDE
//! change.

use std::path::{Path, PathBuf};

use kiln_ir::Target;

/// One template, as described by its `template.meta`.
pub struct Template {
    pub id: String,
    pub name: String,
    pub desc: String,
    pub target: Target,
    /// The file a newly created project should open first.
    pub entry: String,
    pub dir: PathBuf,
}

/// Read every template under `<repo_root>/templates` plus every template
/// shipped by a resolved kit, sorted by id so the listing (and therefore the
/// New Project dialog) is stable.
///
/// The bundled directory is read first, so a kit cannot quietly replace a
/// built-in template with something else under the same name. Among kits the
/// winner is the one resolution already chose, since a shadowed kit is never
/// resolved at all.
pub fn load_all(repo_root: &Path) -> Result<Vec<Template>, String> {
    let root = repo_root.join("templates");
    let entries = std::fs::read_dir(&root)
        .map_err(|e| format!("cannot read {}: {e}", root.display()))?;

    let mut out: Vec<Template> = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        if let Some(t) = read_template(&dir)? {
            out.push(t);
        }
    }
    for k in crate::kit::resolve_all(repo_root) {
        for name in &k.templates {
            let dir = k.dir.join(name);
            let Some(t) = read_template(&dir)? else {
                return Err(format!(
                    "kit `{}` names a template `{name}` with no template.meta in {}",
                    k.name,
                    dir.display()
                ));
            };
            if !out.iter().any(|e| e.id == t.id) {
                out.push(t);
            }
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

/// A directory is a template when it has a `template.meta`; anything else in
/// `templates/` is not one, and is skipped rather than reported.
fn read_template(dir: &Path) -> Result<Option<Template>, String> {
    if !dir.is_dir() {
        return Ok(None);
    }
    let meta = dir.join("template.meta");
    if !meta.is_file() {
        return Ok(None);
    }
    let id = dir
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or_default()
        .to_string();
    parse_meta(&id, dir, &meta).map(Some)
}

fn parse_meta(id: &str, dir: &Path, meta: &Path) -> Result<Template, String> {
    let text = std::fs::read_to_string(meta)
        .map_err(|e| format!("cannot read {}: {e}", meta.display()))?;
    let mut name = String::new();
    let mut desc = String::new();
    let mut target = None;
    let mut entry = String::from("main.kiln");

    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().to_string();
        match key.trim() {
            "name" => name = value,
            "desc" => desc = value,
            "entry" => entry = value,
            "target" => {
                target = Some(Target::parse(&value).ok_or_else(|| {
                    format!("{}: unknown target `{value}`", meta.display())
                })?)
            }
            _ => {}
        }
    }

    Ok(Template {
        id: id.to_string(),
        name: if name.is_empty() { id.to_string() } else { name },
        desc,
        // A template without a declared target is a template whose New Project
        // tile would lie about what it builds.
        target: target.ok_or_else(|| format!("{}: no `target:` line", meta.display()))?,
        entry,
        dir: dir.to_path_buf(),
    })
}

/// `kiln templates` — list what can be created.
///
/// Line-based, like `inspect`, so a consumer needs no JSON parser. Each line
/// repeats the id so the fields can be read in any order.
pub fn cmd_list(repo_root: &Path) -> i32 {
    let templates = match load_all(repo_root) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("kiln: {e}");
            return 1;
        }
    };
    for t in &templates {
        println!("template: {} {}", t.id, t.target.as_str());
        println!("name: {} {}", t.id, t.name);
        println!("desc: {} {}", t.id, t.desc);
        println!("entry: {} {}", t.id, t.entry);
    }
    0
}

/// `kiln new <template> <dir> [--name <module>] [--title <text>]`.
pub fn cmd_new(repo_root: &Path, args: &[String]) -> i32 {
    let mut template_id: Option<String> = None;
    let mut dest: Option<PathBuf> = None;
    let mut module: Option<String> = None;
    let mut title: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--name" | "-n" => {
                i += 1;
                match args.get(i) {
                    Some(v) => module = Some(v.clone()),
                    None => {
                        eprintln!("kiln: `--name` needs a module name");
                        return 2;
                    }
                }
            }
            "--title" | "-t" => {
                i += 1;
                match args.get(i) {
                    Some(v) if !v.trim().is_empty() => title = Some(v.trim().to_string()),
                    _ => {
                        eprintln!("kiln: `--title` needs a title");
                        return 2;
                    }
                }
            }
            s if s.starts_with('-') => {
                eprintln!("kiln: unknown flag `{s}`");
                return 2;
            }
            s if template_id.is_none() => template_id = Some(s.to_string()),
            s if dest.is_none() => dest = Some(PathBuf::from(s)),
            s => {
                eprintln!("kiln: unexpected argument `{s}`");
                return 2;
            }
        }
        i += 1;
    }

    let (Some(template_id), Some(dest)) = (template_id, dest) else {
        eprintln!("kiln: usage: kiln new <template> <dir> [--name <module>] [--title <text>]");
        eprintln!("kiln: run `kiln templates` to see what is available");
        return 2;
    };

    let templates = match load_all(repo_root) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("kiln: {e}");
            return 1;
        }
    };
    let Some(t) = templates.iter().find(|t| t.id == template_id) else {
        eprintln!("kiln: no template `{template_id}`");
        eprintln!(
            "kiln: available: {}",
            templates
                .iter()
                .map(|t| t.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        return 1;
    };

    // The module name defaults to the directory name, sanitised: it becomes an
    // identifier in the source, so it cannot contain `-` or start with a digit.
    let module = module.unwrap_or_else(|| {
        let stem = dest
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("app")
            .to_string();
        sanitise_module(&stem)
    });

    // The title is what a person reads — a window's caption, a project's name
    // on a welcome screen — so it is not derived from the directory the way
    // the module is: `my-app` is a fine folder and a poor caption. Every
    // project starts as "Untitled App", as documents do in every editor,
    // until the person names it.
    let title = title.unwrap_or_else(|| "Untitled App".to_string());

    // Never write over someone's work: an existing non-empty directory is an
    // error, not something to merge into.
    if dest.exists() && std::fs::read_dir(&dest).map(|d| d.count()).unwrap_or(0) > 0 {
        eprintln!("kiln: {} already exists and is not empty", dest.display());
        return 1;
    }
    if let Err(e) = std::fs::create_dir_all(&dest) {
        eprintln!("kiln: cannot create {}: {e}", dest.display());
        return 1;
    }

    let files = match std::fs::read_dir(&t.dir) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("kiln: cannot read {}: {e}", t.dir.display());
            return 1;
        }
    };
    for f in files.flatten() {
        let from = f.path();
        let Some(name) = from.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name == "template.meta" || !from.is_file() {
            continue;
        }
        let bytes = match std::fs::read(&from) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("kiln: cannot read {}: {e}", from.display());
                return 1;
            }
        };
        // Only text gets the module name substituted. An icon or any other
        // binary a template ships is copied byte for byte — running a text
        // replace over a PNG corrupts it, and reading it as UTF-8 refuses it.
        let out: Vec<u8> = match std::str::from_utf8(&bytes) {
            Ok(text) => text
                .replace("__MODULE__", &module)
                .replace("__TITLE__", &title)
                .into_bytes(),
            Err(_) => bytes,
        };
        let to = dest.join(name);
        if let Err(e) = std::fs::write(&to, out) {
            eprintln!("kiln: cannot write {}: {e}", to.display());
            return 1;
        }
    }

    // The project file, from what the template already knows plus the entry's
    // own `use` lines — so `kits:` is what the source actually asks for, not
    // a second list in template.meta that could drift from it.
    let entry_path = dest.join(&t.entry);
    let kits = std::fs::read_to_string(&entry_path)
        .ok()
        .and_then(|src| kiln_ir::parse(&src).ok())
        .map(|m| m.uses.clone())
        .unwrap_or_default();
    let proj = dest.join(crate::project::FILE_NAME);
    let text = crate::project::render(&module, &t.entry, t.target, &kits, "0.1.0");
    if let Err(e) = std::fs::write(&proj, text) {
        eprintln!("kiln: cannot write {}: {e}", proj.display());
        return 1;
    }

    // What a build leaves behind, ignored from the first commit rather than
    // after someone commits a binary. Written here rather than shipped in each
    // template because every template wants exactly the same three lines, and
    // four copies of a file is four chances for one to fall behind. A template
    // that ships its own is left alone.
    let ignore = dest.join(".gitignore");
    if !ignore.exists() {
        let text = "# Studio's throwaway build, made by Run\n.kiln/\n\n# Where Build puts your program\nbuild/\n";
        if let Err(e) = std::fs::write(&ignore, text) {
            eprintln!("kiln: cannot write {}: {e}", ignore.display());
            return 1;
        }
    }

    // Printed in the same line-based shape as the listing, so Studio can read
    // where to open without guessing. `project:` is an ADDED line: a reader
    // that only knows `open:` still sees exactly what it saw before.
    println!("created: {} {}", t.id, dest.display());
    println!("module: {module}");
    println!("title: {title}");
    println!("target: {}", t.target.as_str());
    println!("open: {}", entry_path.display());
    println!("project: {}", proj.display());
    0
}

/// Turn a directory name into a legal module identifier.
fn sanitise_module(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect();
    if out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    if out.is_empty() {
        out.push_str("app");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_names_are_legal_identifiers() {
        assert_eq!(sanitise_module("my-app"), "my_app");
        assert_eq!(sanitise_module("2048"), "_2048");
        assert_eq!(sanitise_module("Hello World"), "Hello_World");
    }
}
