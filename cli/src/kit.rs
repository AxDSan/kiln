//! Kits — a support library plus the design-time material that goes with it.
//!
//! A kit is the directory a support library already is: `<name>_libinfo.c`
//! builds the metadata `.so` the compiler dlopens and never ships, and the
//! other sources static-link and dead-strip. Delphi split that into a
//! design-time package and a runtime package, and paid for it with a DLL
//! beside every executable; here the runtime half disappears into the binary
//! and only the design-time half is ever a file on disk.
//!
//! What a kit adds over a bare library is the material an IDE needs and a
//! compiler does not: a display name, the toolbox section to file it under,
//! an icon per component, a version, and the project templates it ships. All
//! of it lives in the same `lib.json` the build flags do, under keys the
//! loader never reads — so every existing library, which has no such keys and
//! mostly no manifest at all, keeps working untouched.
//!
//! A kit is found in one of three places, and the first match wins:
//!
//!   1. `kits/` in the project (walking up from the working directory),
//!   2. `~/.kiln/kits/`, where `kiln kit add` unpacks,
//!   3. the bundled `libs/`.
//!
//! `kiln kits` prints which one won and from where. A resolution people
//! cannot inspect is a resolution they cannot debug, and "it worked on my
//! machine" is nearly always a shadowing question.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Where a kit was found. The order is the resolution order.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Tier {
    Project,
    User,
    Bundled,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Project => "project",
            Tier::User => "user",
            Tier::Bundled => "bundled",
        }
    }
}

/// One resolved kit.
pub struct Kit {
    /// The directory name, which is what `use` says and what a command's
    /// prefix belongs to.
    pub name: String,
    pub display: String,
    pub section: String,
    pub version: String,
    /// Where the kit sorts in a toolbox. Equal values fall back to the name,
    /// so a listing never depends on directory order.
    pub order: i64,
    /// `component=path` pairs, the path relative to the kit directory.
    pub icons: Vec<(String, String)>,
    /// Template subdirectories the kit ships, each with a `template.meta`.
    pub templates: Vec<String>,
    pub dir: PathBuf,
    pub tier: Tier,
}

/// Every kit visible from `repo_root` and the working directory, resolved.
///
/// Keyed by name, so a project kit shadowing a bundled one is simply the entry
/// that got there first — the same rule a `PATH` lookup uses, for the same
/// reason.
pub fn resolve_all(repo_root: &Path) -> Vec<Kit> {
    let mut by_name: BTreeMap<String, Kit> = BTreeMap::new();
    for (tier, root) in search_roots(repo_root) {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        dirs.sort();
        for dir in dirs {
            if !is_kit_dir(&dir) {
                continue;
            }
            let Some(name) = dir.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            by_name
                .entry(name.to_string())
                .or_insert_with(|| read_kit(name, &dir, tier));
        }
    }
    let mut out: Vec<Kit> = by_name.into_values().collect();
    out.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.name.cmp(&b.name)));
    out
}

/// Resolve one kit by name.
pub fn resolve(repo_root: &Path, name: &str) -> Option<Kit> {
    for (tier, root) in search_roots(repo_root) {
        let dir = root.join(name);
        if is_kit_dir(&dir) {
            return Some(read_kit(name, &dir, tier));
        }
    }
    None
}

/// The three search roots, in resolution order. A root that does not exist is
/// still listed; the caller's `read_dir` simply finds nothing.
fn search_roots(repo_root: &Path) -> Vec<(Tier, PathBuf)> {
    let mut roots = Vec::new();
    if let Some(p) = project_kits_dir() {
        roots.push((Tier::Project, p));
    }
    if let Some(p) = user_kits_dir() {
        roots.push((Tier::User, p));
    }
    roots.push((Tier::Bundled, repo_root.join("libs")));
    roots
}

/// `kits/` in the project, found by walking up from the working directory.
///
/// The walk only accepts a `kits` directory that actually contains a kit, so a
/// directory that happens to be called `kits` several levels up cannot silently
/// become the project's.
fn project_kits_dir() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let cand = dir.join("kits");
        if cand.is_dir()
            && std::fs::read_dir(&cand)
                .map(|d| d.flatten().any(|e| is_kit_dir(&e.path())))
                .unwrap_or(false)
        {
            return Some(cand);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// `~/.kiln/kits`. An unset `HOME` skips the tier rather than guessing.
pub fn user_kits_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".kiln").join("kits"))
}

/// A directory is a kit when it holds the metadata TU the compiler dlopens
/// (`*_libinfo.c`) or any declaration bundle (`*.kdecl`). That is the same test
/// `use` and the loader apply, so nothing can be listed that cannot then be
/// used, and a declaration-only kit — one that ships `dll`/`record`/`const`
/// lines and no C — is a first-class kit. A kit that splits its declarations
/// over several `.kdecl` files counts on the first one found.
fn is_kit_dir(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|e| {
        e.file_name()
            .to_str()
            .is_some_and(|n| n.ends_with(".kdecl") || n.ends_with("_libinfo.c"))
    })
}

fn read_kit(name: &str, dir: &Path, tier: Tier) -> Kit {
    let text = std::fs::read_to_string(dir.join("lib.json")).unwrap_or_default();
    let display = non_empty(json_string(&text, "display")).unwrap_or_else(|| name.to_string());
    let icons = json_array(&text, "icons")
        .into_iter()
        .filter_map(|s| {
            s.split_once('=')
                .map(|(c, p)| (c.to_string(), p.to_string()))
        })
        .collect();
    Kit {
        name: name.to_string(),
        display,
        section: non_empty(json_string(&text, "section")).unwrap_or_else(|| "Libraries".into()),
        version: non_empty(json_string(&text, "version")).unwrap_or_else(|| "0.0.0".into()),
        order: json_int(&text, "order").unwrap_or(1000),
        icons,
        templates: json_array(&text, "templates"),
        dir: dir.to_path_buf(),
        tier,
    }
}

fn non_empty(s: String) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// `kiln kits` — what is installed, at which version, and from where.
///
/// Line-based like `inspect` and `templates`, with the kit name repeated on
/// every line so the fields can be read in any order and an unknown line kind
/// can be skipped by a reader that predates it.
pub fn cmd_list(repo_root: &Path) -> i32 {
    for k in resolve_all(repo_root) {
        println!("kit: {} {} {}", k.name, k.version, k.tier.as_str());
        println!("path: {} {}", k.name, k.dir.display());
        println!("name: {} {}", k.name, k.display);
        println!("section: {} {}", k.name, k.section);
        for t in &k.templates {
            println!("template: {} {}", k.name, t);
        }
        for (component, icon) in &k.icons {
            println!(
                "icon: {} {} {}",
                k.name,
                component,
                k.dir.join(icon).display()
            );
        }
        // A kit's declaration bundle, read through the ONE `.kdecl` reader the
        // loader uses — so what `kiln kits` says a kit declares and what a
        // program gets from `use` can never disagree. A bundle that will not
        // parse is simply not listed here; the build is where that error is
        // worth stopping for.
        if let Ok(Some(decls)) = crate::libload::read_decls(&k.dir, &k.name) {
            for d in decls.dlls() {
                println!("dll: {} {} {}", k.name, d.name, d.library);
            }
            for rec in decls.records() {
                if rec.is_c {
                    println!("crecord: {} {}", k.name, rec.name);
                }
            }
            for c in decls.consts() {
                println!("const: {} {} {}", k.name, c.name, c.ty.as_str());
            }
        }
    }
    0
}

/// `kiln kit add <path-or-tarball>` — unpack into `~/.kiln/kits/`.
pub fn cmd_add(args: &[String]) -> i32 {
    let mut src: Option<PathBuf> = None;
    for a in args {
        if a.starts_with('-') {
            eprintln!("kiln: unknown flag `{a}`");
            return 2;
        }
        if src.is_some() {
            eprintln!("kiln: unexpected argument `{a}`");
            return 2;
        }
        src = Some(PathBuf::from(a));
    }
    let Some(src) = src else {
        eprintln!("kiln: usage: kiln kit add <path-or-tarball>");
        return 2;
    };
    if !src.exists() {
        eprintln!("kiln: {} does not exist", src.display());
        return 1;
    }
    let Some(dest_root) = user_kits_dir() else {
        eprintln!("kiln: HOME is not set, so there is nowhere to install a kit");
        return 1;
    };

    // A tarball is unpacked to a scratch directory first, because only after
    // unpacking is it known what is inside — the kit may sit at the top level
    // or one directory down, and refusing to guess before looking is what makes
    // both layouts work.
    let scratch = std::env::temp_dir().join(format!("kiln-kit-add-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    let found = if src.is_dir() {
        find_kit_root(&src)
    } else {
        if let Err(e) = std::fs::create_dir_all(&scratch) {
            eprintln!("kiln: cannot create {}: {e}", scratch.display());
            return 1;
        }
        let status = Command::new("tar")
            .arg("-xf")
            .arg(&src)
            .arg("-C")
            .arg(&scratch)
            .status();
        match status {
            Ok(s) if s.success() => {}
            Ok(_) => {
                eprintln!("kiln: tar failed to unpack {}", src.display());
                let _ = std::fs::remove_dir_all(&scratch);
                return 1;
            }
            Err(e) => {
                eprintln!("kiln: cannot run tar: {e}");
                let _ = std::fs::remove_dir_all(&scratch);
                return 1;
            }
        }
        find_kit_root(&scratch)
    };

    let Some(kit_dir) = found else {
        eprintln!(
            "kiln: {} contains no kit — a kit directory holds a *_libinfo.c metadata source",
            src.display()
        );
        let _ = std::fs::remove_dir_all(&scratch);
        return 1;
    };
    let Some(name) = kit_dir
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_string)
    else {
        eprintln!("kiln: cannot name the kit in {}", src.display());
        let _ = std::fs::remove_dir_all(&scratch);
        return 1;
    };

    let dest = dest_root.join(&name);
    let replaced = dest.exists();
    if replaced {
        if let Err(e) = std::fs::remove_dir_all(&dest) {
            eprintln!("kiln: cannot replace {}: {e}", dest.display());
            let _ = std::fs::remove_dir_all(&scratch);
            return 1;
        }
    }
    let result = std::fs::create_dir_all(&dest_root).and_then(|_| copy_tree(&kit_dir, &dest));
    let _ = std::fs::remove_dir_all(&scratch);
    if let Err(e) = result {
        eprintln!("kiln: cannot install into {}: {e}", dest.display());
        return 1;
    }

    let version = read_kit(&name, &dest, Tier::User).version;
    println!("installed: {name} {version}");
    println!("path: {name} {}", dest.display());
    println!(
        "action: {name} {}",
        if replaced { "replaced" } else { "added" }
    );
    // A project kit of the same name still wins, and saying so here is cheaper
    // than the user discovering it from a command that did not change.
    println!("use: {name}");
    0
}

/// The kit inside `root`: `root` itself if it is one, else its first kit child.
fn find_kit_root(root: &Path) -> Option<PathBuf> {
    if is_kit_dir(root) {
        return Some(root.to_path_buf());
    }
    let mut children: Vec<PathBuf> = std::fs::read_dir(root)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .collect();
    children.sort();
    children.into_iter().find(|p| p.is_dir() && is_kit_dir(p))
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

/// The root the library loader should be handed so that `use <name>` finds the
/// kits resolution chose rather than only `libs/`.
///
/// The loader looks a library up at `<root>/libs/<name>`, which is exactly the
/// right rule when every library is bundled. When one is not, this stages a
/// root that says the same thing about a different set: every top-level entry
/// of the repository is symlinked through unchanged, and `libs/` is rebuilt as
/// one symlink per resolved kit. Manifest paths, include directories and `-L`
/// arguments are all relative to that root and so keep resolving.
///
/// When nothing is shadowed the real root is returned untouched, so an ordinary
/// build does not go anywhere near this.
pub fn overlay_root(repo_root: &Path, uses: &[String]) -> Result<PathBuf, String> {
    let resolved: Vec<(String, PathBuf)> = uses
        .iter()
        .filter_map(|u| resolve(repo_root, u).map(|k| (u.clone(), k.dir)))
        .collect();
    let shadowed = resolved
        .iter()
        .any(|(name, dir)| *dir != repo_root.join("libs").join(name));
    if !shadowed {
        return Ok(repo_root.to_path_buf());
    }

    let stage = std::env::temp_dir().join(format!("kiln-kits-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(stage.join("libs"))
        .map_err(|e| format!("cannot stage kit root {}: {e}", stage.display()))?;

    for entry in std::fs::read_dir(repo_root).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.file_name() == std::ffi::OsStr::new("libs") {
            continue;
        }
        link(&entry.path(), &stage.join(entry.file_name()))?;
    }
    // The bundled libraries stay reachable: only a name a kit claims is
    // redirected, so `use file` alongside `use mykit` still works.
    if let Ok(entries) = std::fs::read_dir(repo_root.join("libs")) {
        for entry in entries.flatten() {
            link(&entry.path(), &stage.join("libs").join(entry.file_name()))?;
        }
    }
    for (name, dir) in &resolved {
        let dest = stage.join("libs").join(name);
        let _ = std::fs::remove_file(&dest);
        link(dir, &dest)?;
    }
    Ok(stage)
}

fn link(from: &Path, to: &Path) -> Result<(), String> {
    // Every entry staged here is a directory; on Windows a directory link
    // is its own call, and creating one needs Developer Mode or elevation —
    // the error names the pair so that is diagnosable.
    #[cfg(unix)]
    let r = std::os::unix::fs::symlink(from, to);
    #[cfg(windows)]
    let r = std::os::windows::fs::symlink_dir(from, to);
    r.map_err(|e| format!("cannot link {} -> {}: {e}", to.display(), from.display()))
}

// --- lib.json readers, for the keys the loader does not read ---------------
//
// A second tiny reader rather than a JSON crate, for the reason the loader's
// own is one: the schema is flat and fixed, and the compiler has no runtime
// dependencies to spend here.

fn json_field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{key}\"");
    let start = text.find(&needle)? + needle.len();
    let rest = text[start..].trim_start();
    rest.strip_prefix(':').map(|r| r.trim_start())
}

fn json_string(text: &str, key: &str) -> String {
    json_field(text, key)
        .and_then(|r| r.strip_prefix('"'))
        .and_then(|r| r.find('"').map(|e| r[..e].to_string()))
        .unwrap_or_default()
}

fn json_int(text: &str, key: &str) -> Option<i64> {
    let rest = json_field(text, key)?;
    let digits: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-')
        .collect();
    digits.parse().ok()
}

fn json_array(text: &str, key: &str) -> Vec<String> {
    let Some(rest) = json_field(text, key) else {
        return Vec::new();
    };
    let Some(rest) = rest.strip_prefix('[') else {
        return Vec::new();
    };
    let Some(end) = rest.find(']') else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut chars = rest[..end].chars().peekable();
    while let Some(c) = chars.next() {
        if c != '"' {
            continue;
        }
        let mut item = String::new();
        for c in chars.by_ref() {
            if c == '"' {
                break;
            }
            item.push(c);
        }
        out.push(item);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_keys_default_when_absent() {
        // Every existing library has no kit keys, so the defaults are what
        // they will be listed with.
        let k = read_kit("plain", Path::new("/nowhere"), Tier::Bundled);
        assert_eq!(k.display, "plain");
        assert_eq!(k.version, "0.0.0");
        assert_eq!(k.section, "Libraries");
        assert!(k.icons.is_empty() && k.templates.is_empty());
    }

    #[test]
    fn json_readers_take_the_named_field() {
        let text = r#"{ "order": 20, "icons": ["Gauge=a.svg", "Dial=b.svg"] }"#;
        assert_eq!(json_int(text, "order"), Some(20));
        assert_eq!(json_int(text, "missing"), None);
        assert_eq!(json_array(text, "icons").len(), 2);
    }
}
