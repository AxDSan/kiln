//! `kiln install` — put this Kiln somewhere the shell can find it.
//!
//! The bundle is relocatable by construction: `kiln` walks up from its own
//! executable to find `runtime/`, so a whole tree copied anywhere works with
//! no environment variables. Installing is therefore a copy of that tree into
//! `<prefix>/lib/kiln`, plus a symlink per binary in `<prefix>/bin`.
//! `std::env::current_exe` resolves the symlink, so the walk-up lands in the
//! installed tree rather than in `<prefix>`.
//!
//! It is deliberately not a package manager. There is no uninstall database,
//! no version pinning and no post-install script — `--prefix` and a `rm -rf`
//! of `<prefix>/lib/kiln` are the whole model, and `--dry-run` prints exactly
//! what those two would touch.

use std::path::{Path, PathBuf};

/// What a bundle carries beside the binaries. Named rather than globbed: an
/// install that silently picked up `target/` or a developer's `dist/` would
/// copy gigabytes and would differ between machines.
const TREE: &[&str] = &[
    "runtime", "abi", "libs", "kits", "templates", "examples", "editors", "assets", "docs",
];

/// The binaries: the name to install as, whether the install is useless
/// without it, and where a *source checkout* keeps it when it is not beside
/// the other one. A release bundle has both under `bin/`; a checkout has the
/// compiler under `target/release` and the IDE under `designer/`, because
/// they are built by two different commands.
const BINARIES: &[(&str, bool, Option<&str>)] = &[
    ("kiln", true, None),
    ("kiln-studio", false, Some("designer/kiln-designer")),
];

pub fn cmd_install(root: &Path, args: &[String]) -> i32 {
    let mut prefix: Option<PathBuf> = None;
    let mut editors = false;
    let mut dry = false;
    let mut force = false;

    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--prefix" => match it.next() {
                Some(p) => prefix = Some(PathBuf::from(p)),
                None => {
                    eprintln!("kiln: --prefix needs a directory");
                    return 2;
                }
            },
            "--user" => prefix = Some(home().join(".local")),
            "--editors" => editors = true,
            "--dry-run" | "-n" => dry = true,
            "--force" | "-f" => force = true,
            "-h" | "--help" => {
                usage();
                return 0;
            }
            other => {
                eprintln!("kiln: unknown flag `{other}` for `install`\n");
                usage();
                return 2;
            }
        }
    }

    let prefix = prefix.unwrap_or_else(default_prefix);
    let libdir = prefix.join("lib").join("kiln");
    let bindir = prefix.join("bin");

    // Refusing to install a tree over itself: `--prefix` pointing at the
    // bundle already running is a copy of a directory into its own child,
    // which either loops or truncates the thing it is reading.
    if libdir.starts_with(root) || root.starts_with(&libdir) {
        eprintln!(
            "kiln: refusing to install into {} — it is inside the tree being installed \
             ({})",
            libdir.display(),
            root.display()
        );
        return 1;
    }

    if libdir.exists() && !force && !dry {
        eprintln!(
            "kiln: {} already exists — pass --force to replace it, or remove it first",
            libdir.display()
        );
        return 1;
    }

    println!("==> installing to {}", prefix.display());
    if dry {
        println!("    (dry run: nothing is written)");
    }

    // Everything is checked before anything is written, so a prefix that
    // cannot be created fails with one message rather than half a tree.
    for d in [&libdir, &bindir] {
        if !dry {
            if let Err(e) = std::fs::create_dir_all(d) {
                eprintln!("kiln: cannot create {}: {e}", d.display());
                if e.kind() == std::io::ErrorKind::PermissionDenied {
                    eprintln!(
                        "      {} needs write permission — run with sudo, or \
                         `kiln install --user` to install under ~/.local",
                        prefix.display()
                    );
                }
                return 1;
            }
        }
    }

    if !dry && force && libdir.exists() {
        let _ = std::fs::remove_dir_all(&libdir);
        if let Err(e) = std::fs::create_dir_all(&libdir) {
            eprintln!("kiln: cannot create {}: {e}", libdir.display());
            return 1;
        }
    }

    let mut copied = 0usize;
    for name in TREE {
        let from = root.join(name);
        if !from.is_dir() {
            continue; // a source checkout has no docs/book, and that is fine
        }
        let to = libdir.join(name);
        println!("    {name}/");
        if dry {
            continue;
        }
        if let Err(e) = copy_tree(&from, &to) {
            eprintln!("kiln: copying {}: {e}", from.display());
            return 1;
        }
        copied += 1;
    }
    if copied == 0 && !dry {
        eprintln!(
            "kiln: found nothing to install under {} — is this a Kiln tree?",
            root.display()
        );
        return 1;
    }

    // The binaries go under lib/ with the tree they belong to, and the names
    // on PATH are symlinks into it. That is what keeps the walk-up working
    // and what makes replacing an install one directory to delete.
    let src_bin = if root.join("bin").join("kiln").is_file() {
        root.join("bin") // a release bundle
    } else {
        root.join("target").join("release") // a source checkout
    };
    if !dry {
        if let Err(e) = std::fs::create_dir_all(libdir.join("bin")) {
            eprintln!("kiln: cannot create {}: {e}", libdir.join("bin").display());
            return 1;
        }
    }
    for (name, required, checkout_path) in BINARIES {
        let mut from = src_bin.join(name);
        if !from.is_file() {
            if let Some(alt) = checkout_path {
                let alt = root.join(alt);
                if alt.is_file() {
                    from = alt;
                }
            }
        }
        if !from.is_file() {
            if *required {
                eprintln!(
                    "kiln: no `{name}` in {} — build it first (cargo build --release)",
                    src_bin.display()
                );
                return 1;
            }
            println!("    bin/{name}  (not built; skipped)");
            continue;
        }
        let to = libdir.join("bin").join(name);
        let link = bindir.join(name);
        println!("    bin/{name} -> {}", link.display());
        if dry {
            continue;
        }
        if let Err(e) = std::fs::copy(&from, &to) {
            eprintln!("kiln: copying {}: {e}", from.display());
            return 1;
        }
        if let Err(e) = make_executable(&to) {
            eprintln!("kiln: chmod {}: {e}", to.display());
            return 1;
        }
        if let Err(e) = symlink(&to, &link) {
            eprintln!("kiln: linking {}: {e}", link.display());
            return 1;
        }
    }

    if editors {
        let code = install_editors(&libdir, dry);
        if code != 0 {
            return code;
        }
    }

    println!();
    if dry {
        println!("nothing was written. Run it again without --dry-run.");
        return 0;
    }
    println!("installed. `kiln version` should work once {} is on your PATH.", bindir.display());
    if !on_path(&bindir) {
        println!();
        println!("{} is not on your PATH. Add it:", bindir.display());
        println!("    export PATH=\"{}:$PATH\"", bindir.display());
    }
    0
}

/// The editor integration that can be installed without a package: Kate's
/// syntax definition, which is a file in a known directory.
///
/// Deliberately per-user and not per-prefix: these live under
/// `~/.local/share` and `~/.config` whatever the toolchain's prefix is,
/// because they belong to the person editing rather than to the machine.
fn install_editors(libdir: &Path, dry: bool) -> i32 {
    let src = libdir.join("editors").join("kate").join("kiln.xml");
    // In a dry run nothing has been copied yet, so read from the source tree.
    let src = if src.is_file() { src } else { PathBuf::from("editors/kate/kiln.xml") };

    let dest_dir = home()
        .join(".local")
        .join("share")
        .join("org.kde.syntax-highlighting")
        .join("syntax");
    let dest = dest_dir.join("kiln.xml");

    println!("    kate syntax -> {}", dest.display());
    if !dry {
        if let Err(e) = std::fs::create_dir_all(&dest_dir) {
            eprintln!("kiln: cannot create {}: {e}", dest_dir.display());
            return 1;
        }
        if let Err(e) = std::fs::copy(&src, &dest) {
            eprintln!("kiln: copying the Kate syntax definition: {e}");
            return 1;
        }
    }

    // The LSP entry is NOT written. Kate keeps its own allowlist of server
    // command lines and asks before running one it has not seen; a tool that
    // edited that file would be answering a security question on the user's
    // behalf. Printed instead, so the person pastes it and Kate asks them.
    println!();
    println!("Kate: paste this into Settings > Configure Kate > LSP Client >");
    println!("User Server Settings, merging it with what is already there:");
    println!();
    println!("{}", kate_lsp_json());
    println!("Kate will ask once before running `kiln lsp`. Say yes.");
    0
}

fn kate_lsp_json() -> &'static str {
    // Kept in step with editors/kate/lspclient.json by hand; it is nine lines
    // and a file read that can fail at the moment of printing advice is worse
    // than a copy.
    r#"    {
        "servers": {
            "kiln": {
                "command": ["kiln", "lsp"],
                "highlightingModeRegex": "^Kiln$",
                "documentLanguageId": "kiln",
                "rootIndicationFileNames": ["project.kproj"]
            }
        }
    }
"#
}

/// `/usr/local` when it can be written, `~/.local` otherwise. A default that
/// asks for a password it cannot get is a default that fails; a default that
/// silently installs somewhere not on PATH is worse, so the outcome is
/// printed either way.
fn default_prefix() -> PathBuf {
    let system = PathBuf::from("/usr/local");
    if writable(&system) {
        system
    } else {
        home().join(".local")
    }
}

fn writable(p: &Path) -> bool {
    // Asking the filesystem rather than checking for uid 0: a machine where
    // /usr/local is group-writable is one where a plain user should get it.
    let probe = p.join(".kiln-install-probe");
    match std::fs::create_dir(&probe) {
        Ok(()) => {
            let _ = std::fs::remove_dir(&probe);
            true
        }
        Err(_) => false,
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn on_path(dir: &Path) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d == dir))
        .unwrap_or(false)
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if ty.is_dir() {
            copy_tree(&src, &dst)?;
        } else if ty.is_file() {
            std::fs::copy(&src, &dst)?;
            // Preserve the executable bit: `libs/` and `tools/` carry scripts,
            // and a copy that dropped it would install something that cannot
            // run for a reason nothing explains.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(&src)?.permissions().mode();
                if mode & 0o111 != 0 {
                    let mut perms = std::fs::metadata(&dst)?.permissions();
                    perms.set_mode(mode);
                    std::fs::set_permissions(&dst, perms)?;
                }
            }
        }
        // Symlinks inside the tree are skipped rather than followed: nothing
        // a bundle ships needs one, and following one is how a copy leaves
        // the directory it was told to copy.
    }
    Ok(())
}

fn make_executable(p: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(p)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(p, perms)?;
    }
    #[cfg(not(unix))]
    let _ = p;
    Ok(())
}

fn symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    let _ = std::fs::remove_file(link);
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
    }
    #[cfg(windows)]
    {
        // Windows needs a privilege for a symlink and none for a copy, and a
        // copy of a 4MB binary is not worth an elevation prompt.
        std::fs::copy(target, link).map(|_| ())
    }
}

fn usage() {
    println!(
        "\
kiln install — copy this Kiln to a prefix and put it on your PATH

  kiln install                    install to /usr/local, or ~/.local when that
                                  is not writable
  kiln install --prefix <dir>     install to <dir>: <dir>/lib/kiln, <dir>/bin
  kiln install --user             the same as --prefix ~/.local
  kiln install --editors          also install Kate's syntax definition, and
                                  print Kate's language-server entry
  kiln install --dry-run          print what would be written, write nothing
  kiln install --force            replace an existing <prefix>/lib/kiln

The tree is relocatable: `kiln` finds its runtime by walking up from its own
executable, so <prefix>/lib/kiln is self-contained and <prefix>/bin holds
symlinks into it. To uninstall, delete <prefix>/lib/kiln and those links."
    );
}
