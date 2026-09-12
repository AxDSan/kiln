//! `kiln` — the command-line toolchain.
//!
//! Subcommands:
//!   kiln build <in.kiln> [-o <out>]   parse -> lower -> clang -> native binary
//!   kiln run   <in.kiln> [-o <out>]   build, then execute it
//!     …either with --release            optimised, hardened and stripped
//!   kiln emit  <in.kiln>              print the generated LLVM IR to stdout
//!   kiln lsp                         language server (stdio) for editors
//!   kiln commands                    list available commands and components
//!   kiln inspect <in.kiln>            dump the form model, one fact per line
//!   kiln templates                   list project templates
//!   kiln new <tmpl> <dir>            create a project from a template
//!     [--name <module>] [--title <text>]  (the title defaults to "Untitled App")
//!   kiln kits                        list resolved kits and where they came from
//!   kiln kit add <path>              install a kit into ~/.kiln/kits
//!   kiln project <file-or-dir>       dump a project file's resolved fields
//!   kiln install                     copy this Kiln to a prefix, on your PATH
//!   kiln version                     the toolchain and ABI versions
//!
//! `build`, `run`, `emit` and `inspect` take a `project.kproj`, or a directory
//! holding one, in place of the `.kiln`; the entry file comes from the project.
//!
//! The pipeline lowers a module to LLVM IR, then has `clang` assemble it and
//! link the runtime sources, producing an ordinary native executable.

use std::path::{Path, PathBuf};
use std::process::{exit, Command};

mod dap;
mod debug;
mod header;
mod install;
mod kit;
mod libload;
mod lsp;
mod lsp_index;
mod project;
mod templates;

use std::collections::HashMap;

use kiln_backend::{lower_module_for, DebugFormat};
use kiln_ir::registry::Registry;
use kiln_ir::validate::{validate_with, Hints};
use kiln_ir::{parse_with, resolve_units, Module, ParseOptions, Target};

fn main() {
    // Die quietly when a reader goes away, the way every other command-line
    // tool does: `kiln commands | head` should not print a panic.
    #[cfg(unix)]
    unsafe {
        libc_signal_default();
    }
    let args: Vec<String> = std::env::args().collect();
    let code = run(&args[1..]);
    exit(code);
}

/// Restore the default SIGPIPE disposition, which Rust's runtime overrides.
#[cfg(unix)]
unsafe fn libc_signal_default() {
    // SIG_DFL for SIGPIPE (13). Declared here rather than adding a dependency
    // on `libc` for one constant.
    unsafe extern "C" {
        fn signal(sig: i32, handler: usize) -> usize;
    }
    unsafe { signal(13, 0) };
}

fn run(args: &[String]) -> i32 {
    let (cmd, rest) = match args.split_first() {
        Some(x) => x,
        None => {
            usage();
            return 2;
        }
    };
    match cmd.as_str() {
        "build" => cmd_build(rest, false),
        "run" => cmd_build(rest, true),
        "emit" => cmd_emit(rest),
        "inspect" => cmd_inspect(rest),
        "dap" => dap::run(),
        "debug" => debug::run(rest),
        "lsp" => lsp::run(),
        "commands" => match find_repo_root() {
            Some(root) => cmd_commands(&root, rest),
            None => {
                eprintln!("kiln: could not locate the Kiln runtime");
                1
            }
        },
        "kits" => match find_repo_root() {
            Some(root) => kit::cmd_list(&root),
            None => {
                eprintln!("kiln: could not locate the Kiln libraries");
                1
            }
        },
        "kit" => match rest.split_first() {
            Some((verb, kit_args)) if verb == "add" => kit::cmd_add(kit_args),
            Some((verb, _)) => {
                eprintln!("kiln: unknown `kit` verb `{verb}` — expected `add`");
                2
            }
            None => {
                eprintln!("kiln: usage: kiln kit add <path-or-tarball>");
                2
            }
        },
        "templates" => match find_repo_root() {
            Some(root) => templates::cmd_list(&root),
            None => {
                eprintln!("kiln: could not locate the Kiln templates directory");
                1
            }
        },
        "new" => match find_repo_root() {
            Some(root) => templates::cmd_new(&root, rest),
            None => {
                eprintln!("kiln: could not locate the Kiln templates directory");
                1
            }
        },
        "project" => project::cmd_project(rest),
        "install" => match find_repo_root() {
            Some(root) => install::cmd_install(&root, rest),
            None => {
                eprintln!(
                    "kiln: could not locate the Kiln tree to install — run this from a                      release bundle or a source checkout"
                );
                1
            }
        },
        "version" | "--version" | "-V" => {
            print!("{}", version_text());
            0
        }
        "-h" | "--help" | "help" => {
            usage();
            0
        }
        other => {
            eprintln!("kiln: unknown subcommand `{other}`\n");
            usage();
            2
        }
    }
}

fn usage() {
    eprintln!(
        "kiln — the Kiln toolchain\n\n\
         USAGE:\n  \
         kiln build <in.kiln> [-o <out>]   compile to a native binary\n  \
         kiln run   <in.kiln> [-o <out>]   compile and run\n  \
         kiln build|run --release         …optimised, hardened and stripped\n  \
         kiln build --emit-ir             …keeping the .ll it handed clang\n  \
         kiln build --os windows          …for Windows x86-64 (needs mingw-w64)\n  \
         kiln build --os windows --arch x86  …for Windows 32-bit (i686; needs mingw32)\n  \
         kiln build --target sharedlib    …a library, with its C header beside it\n  \
           [--header <path>]                 where the header goes (default <module>.h)\n  \
         kiln emit  <in.kiln>              print generated LLVM IR\n  \
         kiln debug <program>             read a built program's debug information\n  \
         kiln inspect <in.kiln>            dump the form model (for the designer)\n  \
         kiln lsp                         language server over stdio (see docs/editors.md)\n  \
         kiln commands [--use <lib>]      list the commands and components available\n  \
         kiln templates                   list the available project templates\n  \
         kiln new <template> <dir>        create a project from a template\n  \
           [--name <module>] [--title <text>]  the caption defaults to \"Untitled App\"\n  \
         kiln kits                        list the kits found, and from where\n  \
         kiln kit add <path>              install a kit into ~/.kiln/kits\n  \
         kiln project <file-or-dir>       dump a project file's resolved fields\n  \
         kiln project <file-or-dir> set <key>=<value>...\n  \
           the only writer of a project file: name, main, target, kits, version\n  \
         kiln install [--prefix <dir>]    copy this Kiln to a prefix, on your PATH\n  \
           [--user] [--editors] [--dry-run] [--force]   see `kiln install --help`\n  \
         kiln version                     print the toolchain and ABI versions\n\n\
         Wherever <in.kiln> is accepted, a project.kproj or its directory is too.\n"
    );
}

/// `kiln version` — two lines, each one fact, so Studio reads the first
/// and a library author checking compatibility reads the second.
///
/// The ABI number is read out of the header that defines it rather than
/// restated here: a version this command reports and a version the loader
/// checks have to be the same number, and the header is where both live.
fn version_text() -> String {
    const ABI_HEADER: &str = include_str!("../../abi/kiln_abi.h");
    let abi = ABI_HEADER
        .lines()
        .find_map(|l| l.strip_prefix("#define KILN_ABI_VERSION"))
        .map(str::trim)
        .unwrap_or("?");
    format!("kiln {}\nabi {abi}\n", env!("CARGO_PKG_VERSION"))
}

/// What a build/emit invocation was asked to do.
struct Io {
    input: PathBuf,
    output: Option<PathBuf>,
    /// Overrides the module's own `target` declaration when given.
    target: Option<Target>,
    /// Optimise, harden and strip the built program.
    release: bool,
    /// Keep the `.ll` the build handed clang. Off by default: it is an
    /// intermediate, and leaving one beside every binary litters a project
    /// directory with files nobody asked for. `kiln emit` prints the IR
    /// for anyone who wants to read it.
    emit_ir: bool,
    /// The operating system the output is for.
    os: Os,
    /// The CPU architecture the output is for.
    arch: Arch,
    /// Where a library build writes its C header; `None` is `<module>.h`
    /// beside the artifact.
    header: Option<PathBuf>,
    /// Where the output goes when the input came through a project file: the
    /// project's directory and name. Naming it after the entry would call
    /// every program `main`, and putting it in the working directory would
    /// collide with the project directory itself for `kiln build <dir>`.
    project_output: Option<PathBuf>,
}

/// The operating system a build is for.
///
/// Only the two the toolchain can actually produce: the host, and Windows
/// x86-64 through mingw-w64. Everything the target OS changes — the compiler
/// invocation, the artifact names, which hardening flags mean anything — is
/// decided by matching on this, so a third OS is a third arm in each match
/// and not a scattering of string comparisons.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Os {
    Linux,
    Windows,
}

impl Os {
    /// The machine this toolchain is running on: the default, and the only
    /// one whose output `kiln run` can execute.
    fn host() -> Os {
        Os::Linux
    }

    fn parse(s: &str) -> Option<Os> {
        match s {
            "linux" | "host" => Some(Os::Linux),
            "windows" | "win" | "win64" => Some(Os::Windows),
            _ => None,
        }
    }

    /// The name a kit's `lib.json` `"platforms"` uses for this OS, for the
    /// build's platform-gating check.
    fn as_platform(self) -> &'static str {
        match self {
            Os::Linux => "linux",
            Os::Windows => "windows",
        }
    }
}

/// The CPU architecture a build is for.
///
/// Two, because that is what the mingw-w64 cross toolchain gives us and what
/// the compiler now knows how to lay out for: 64-bit, and the 32-bit i386 that
/// a Windows program written for the 2000s still is. The language and the IR
/// are the same either way — only c-record pointer widths and the calling
/// conventions on 32-bit Windows differ, and both read from `TargetInfo`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Arch {
    X86_64,
    X86,
}

impl Arch {
    /// The architecture of the machine this toolchain runs on.
    fn host() -> Arch {
        Arch::X86_64
    }

    fn parse(s: &str) -> Option<Arch> {
        match s {
            "x86_64" | "x64" | "amd64" | "64" => Some(Arch::X86_64),
            "x86" | "i686" | "i386" | "win32" | "32" => Some(Arch::X86),
            _ => None,
        }
    }

    /// The machine model the IR layer lays out for and the backend names a
    /// calling convention from. `os` decides what `system` means on 32 bits.
    fn target_info(self, os: Os) -> kiln_ir::TargetInfo {
        match (self, os) {
            (Arch::X86_64, Os::Linux) => kiln_ir::TargetInfo::X86_64_LINUX,
            (Arch::X86_64, Os::Windows) => kiln_ir::TargetInfo::X86_64_WINDOWS,
            (Arch::X86, Os::Windows) => kiln_ir::TargetInfo::X86_WINDOWS,
            (Arch::X86, Os::Linux) => kiln_ir::TargetInfo::X86_LINUX,
        }
    }
}

/// The mingw-w64 cross toolchain, by the names the distributions install.
///
/// The IR goes through clang, which knows the target from a flag, and every C
/// source goes through the same clang so one set of flags compiles the whole
/// program. The link is gcc's, because that driver knows where the mingw
/// start files and import libraries live and clang's does not always.
///
/// The five names are per-architecture: Fedora installs `i686-w64-mingw32-*`
/// beside `x86_64-w64-mingw32-*`, and Debian's `gcc-mingw-w64-i686` the same.
struct Mingw {
    triple: &'static str,
    gcc: &'static str,
    ar: &'static str,
    gxx: &'static str,
    objdump: &'static str,
    /// Where a Debian/Fedora sysroot keeps its DLLs, for the fallback list
    /// `mingw_dll_dirs` builds when `-print-sysroot` says nothing useful.
    sysroot: &'static str,
}

const MINGW_X86_64: Mingw = Mingw {
    triple: "x86_64-w64-mingw32",
    gcc: "x86_64-w64-mingw32-gcc",
    ar: "x86_64-w64-mingw32-ar",
    // C++ is compiled by mingw's own g++ rather than by clang retargeted: the
    // vendored RmlUi archive is g++'s, and the two compilers disagree about
    // the size of C++ COMDAT type-info sections, which mingw's linker refuses
    // to merge. C and the IR stay with clang, whose objects have no such
    // sections.
    gxx: "x86_64-w64-mingw32-g++",
    objdump: "x86_64-w64-mingw32-objdump",
    sysroot: "/usr/x86_64-w64-mingw32",
};

const MINGW_X86: Mingw = Mingw {
    triple: "i686-w64-mingw32",
    gcc: "i686-w64-mingw32-gcc",
    ar: "i686-w64-mingw32-ar",
    gxx: "i686-w64-mingw32-g++",
    objdump: "i686-w64-mingw32-objdump",
    sysroot: "/usr/i686-w64-mingw32",
};

fn mingw(arch: Arch) -> &'static Mingw {
    match arch {
        Arch::X86_64 => &MINGW_X86_64,
        Arch::X86 => &MINGW_X86,
    }
}

/// The resource table a Windows program carries when it names no picture.
///
/// On Linux `libs/ui` declares the table weak and reads a null pointer as
/// empty. PE has no weak undefined symbol — a reference resolves or the link
/// fails — so a Windows program always defines the table, empty here, and the
/// library there declares an ordinary extern. Defined for every Windows
/// executable rather than only a form's: a console program that says `use ui`
/// references it too, and one nothing references is dead-stripped.
const EMPTY_RESOURCE_TABLE: &str = "\n; No resources to embed; the table is defined anyway because a PE link has no\n\
     ; weak undefined symbol for libs/ui to read as \"none\".\n\
     @kn_embedded_resources = constant [1 x { ptr, ptr, i64 }] [{ ptr, ptr, i64 } zeroinitializer]\n";

/// Parse `<in.kiln> [-o out] [--target kind] [--os name] [--release]` from an
/// argument slice.
///
/// The input may be a project file or its directory. Resolved here, in the one
/// place every subcommand parses its input, so that build, run, emit and
/// inspect cannot disagree about what a project is. The project's `target:`
/// stands in for `--target` only when none was given: a flag on the command
/// line is the more deliberate of the two.
fn parse_io(rest: &[String]) -> Result<Io, String> {
    let mut io = parse_io_args(rest)?;
    if project::is_project_path(&io.input) {
        let p = project::load(&io.input)?;
        io.input = p.main;
        io.target = io.target.or(p.target);
        let dir = p.file.parent().unwrap_or(Path::new(".")).to_path_buf();
        io.project_output = Some(dir.join(&p.name));
    }
    Ok(io)
}

fn parse_io_args(rest: &[String]) -> Result<Io, String> {
    let mut input: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut target: Option<Target> = None;
    let mut release = false;
    let mut emit_ir = false;
    let mut os = Os::host();
    let mut arch: Option<Arch> = None;
    let mut header: Option<PathBuf> = None;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "-o" | "--output" => {
                i += 1;
                let v = rest.get(i).ok_or("`-o` needs a path")?;
                output = Some(PathBuf::from(v));
            }
            "--target" | "-t" => {
                i += 1;
                let v = rest.get(i).ok_or("`--target` needs a kind")?;
                target = Some(Target::parse(v).ok_or_else(|| {
                    format!("unknown target `{v}` — expected console, gui, sharedlib or staticlib")
                })?);
            }
            "--release" => release = true,
            "--emit-ir" => emit_ir = true,
            "--header" => {
                i += 1;
                let v = rest.get(i).ok_or("`--header` needs a path")?;
                header = Some(PathBuf::from(v));
            }
            "--os" => {
                i += 1;
                let v = rest.get(i).ok_or("`--os` needs a name")?;
                os = Os::parse(v)
                    .ok_or_else(|| format!("unknown os `{v}` — expected linux or windows"))?;
            }
            "--arch" | "-a" => {
                i += 1;
                let v = rest.get(i).ok_or("`--arch` needs a name")?;
                arch = Some(Arch::parse(v).ok_or_else(|| {
                    format!("unknown arch `{v}` — expected x86_64 or x86")
                })?);
            }
            s if s.starts_with('-') => return Err(format!("unknown flag `{s}`")),
            s => {
                if input.is_some() {
                    return Err("multiple input files given".into());
                }
                input = Some(PathBuf::from(s));
            }
        }
        i += 1;
    }
    let input = input.ok_or("no input .kiln file given")?;
    Ok(Io {
        input,
        output,
        target,
        release,
        emit_ir,
        os,
        arch: arch.unwrap_or_else(Arch::host),
        header,
        project_output: None,
    })
}

fn cmd_emit(rest: &[String]) -> i32 {
    let io = match parse_io(rest) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("kiln: {e}");
            return 2;
        }
    };
    match compile_with(&io.input, io.target, io.os, io.arch, false, io.release) {
        Ok((ll, _plan, _t, _m)) => {
            print!("{ll}");
            0
        }
        Err(e) => {
            eprintln!("kiln: {e}");
            1
        }
    }
}

/// List everything a program can call or place: commands from the core runtime
/// plus any `use`d libraries, and the visual components they contribute.
///
/// Line-based like `inspect` and `templates`, so it can be read by a script as
/// easily as by a person — the documentation's reference pages are generated
/// from this rather than written by hand, which is the only way they stay true.
fn cmd_commands(repo_root: &Path, args: &[String]) -> i32 {
    let mut uses: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--use" | "-u" => {
                i += 1;
                match args.get(i) {
                    Some(v) => uses.push(v.clone()),
                    None => {
                        eprintln!("kiln: `--use` needs a library name");
                        return 2;
                    }
                }
            }
            s => {
                eprintln!("kiln: unexpected argument `{s}`");
                return 2;
            }
        }
        i += 1;
    }

    // Metadata only: listing what exists must not require the ability to link
    // it, or `kiln commands --use ui` would fail on any machine that has not
    // vendored the UI stack.
    let root = match kit::overlay_root(repo_root, &uses) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("kiln: {e}");
            return 1;
        }
    };
    let plan = match libload::load_metadata(&root, &uses, Arch::host()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("kiln: {e}");
            return 1;
        }
    };

    let mut names: Vec<&str> = plan.registry.names().collect();
    names.sort_unstable();
    for name in names {
        let Some(cmd) = plan.registry.get(name) else { continue };
        let params = cmd
            .sig
            .params
            .iter()
            .map(|t| t.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        match cmd.sig.ret {
            Some(r) => println!("command: {name}({params}) -> {}", r.as_str()),
            None => println!("command: {name}({params})"),
        }
        // ADDED lines, on the same rule the component listing states below: a
        // reader that predates them must still see exactly what it saw before.
        // Most commands carry neither, and print neither.
        if let Some(doc) = plan.registry.doc(name) {
            if !doc.summary.is_empty() {
                println!("doc: {name} {}", doc.summary);
            }
            // One line per line, because an example is several and this
            // listing is line-oriented. Order is the example's own.
            for line in doc.example.lines() {
                println!("example: {name} {line}");
            }
        }
    }

    let mut components: Vec<&str> = plan.registry.component_names().collect();
    components.sort_unstable();
    for type_name in components {
        let Some(desc) = plan.registry.component(type_name) else { continue };
        println!("component: {type_name}");
        // `kind:` and `editor:` are ADDED lines, never a change to the shape of
        // the existing ones: the designer's catalog, gen-docs.sh and
        // check-docs.sh all read this output by prefix, and a reader that
        // predates a line kind must still see exactly what it saw before. A
        // designer that only knows the kind of the components it was linked
        // against files a kit's visual control under the System tray, so the
        // kind has to travel with the listing.
        let kind = match desc.kind {
            kiln_ir::registry::ComponentKind::Visual => "visual",
            kiln_ir::registry::ComponentKind::NonVisual => "nonvisual",
        };
        println!("kind: {type_name} {kind}");
        for p in &desc.properties {
            println!("property: {type_name} {} {}", p.name, p.ty.as_str());
            // Absent means the plain editor the type implies, which is what the
            // descriptor's empty hint already means.
            if !p.editor.is_empty() {
                println!("editor: {type_name} {} {}", p.name, p.editor);
            }
        }
        for e in &desc.events {
            println!("event: {type_name} {e}");
        }
    }

    // Foreign declarations and constants a kit contributes through an `.kdecl`
    // bundle. `dll:` and `const:` are ADDED line kinds, read by prefix like the
    // others, so a reader that predates them is unaffected. A `dll` reads as a
    // signature the same way a command does; a `const` reports its type.
    let mut dlls: Vec<&str> = plan.registry.dll_names().collect();
    dlls.sort_unstable();
    for name in dlls {
        let Some(d) = plan.registry.dll(name) else { continue };
        let params = d
            .sig
            .params
            .iter()
            .map(|t| t.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        match d.sig.ret {
            Some(r) => println!("dll: {name}({params}) -> {} from {}", r.as_str(), d.library),
            None => println!("dll: {name}({params}) from {}", d.library),
        }
    }

    let mut crecords: Vec<&str> = plan.registry.record_names().collect();
    crecords.sort_unstable();
    for name in crecords {
        let Some(rec) = plan.registry.record(name) else { continue };
        // Only a c-record has a byte layout a kit ships for interop; a plain
        // heap record is a program's own business and is not listed here.
        if !rec.is_c {
            continue;
        }
        // The DECLARED type, not the surface one: this line is a description of
        // a memory layout, and a `word` field that printed as `int` would tell
        // a reader transcribing a C header the wrong width. What the field
        // reads and writes as is documented once, in the interop page, rather
        // than guessed at from here.
        let fields = rec
            .fields
            .iter()
            .map(|(n, t)| format!("{n}: {}", t.as_str()))
            .collect::<Vec<_>>()
            .join(", ");
        println!("crecord: {name} {fields}");
    }

    let mut consts: Vec<&str> = plan.registry.const_names().collect();
    consts.sort_unstable();
    for name in consts {
        let Some(c) = plan.registry.const_(name) else { continue };
        println!("const: {name} {}", c.ty.as_str());
    }
    0
}

/// Dump a module's form model as plain lines, for the designer to read.
///
/// This is the designer's ONLY way to learn a file's contents: the Rust parser
/// stays the single reader of `.kiln`. If the designer ever parsed the text
/// itself there would be two grammars to keep in step, and they would drift
///.
///
/// Line-based rather than JSON so neither side needs a serialisation library.
fn cmd_inspect(rest: &[String]) -> i32 {
    let io = match parse_io(rest) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("kiln: {e}");
            return 2;
        }
    };
    let input = io.input;
    let src = match std::fs::read_to_string(&input) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("kiln: cannot read {}: {e}", input.display());
            return 1;
        }
    };
    let module = match kiln_ir::parse(&src) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("kiln: {e}");
            return 1;
        }
    };

    println!("module: {}", module.name);
    for u in &module.uses {
        println!("use: {u}");
        // A `use` that names a file beside the program is a unit, and the
        // designer's list of subroutines should not pretend otherwise.
        if let Some(dir) = input.parent() {
            if let Some(p) = kiln_ir::units::unit_path(dir, u) {
                println!("unit: {u} {}", p.display());
            }
        }
    }
    for sub in module.subs() {
        // `sub:` stays the bare name — the designer reads the rest of the line
        // as a handler name and binds `on click: <that>`. Parameters and a
        // return type go on their own line, and only when there are any, so a
        // reader that predates them sees exactly what it saw before.
        println!("sub: {}", sub.name);
        if !sub.is_plain() {
            let params: Vec<String> = sub
                .params
                .iter()
                .map(|(n, t)| format!("{n}:{}", t.as_str()))
                .collect();
            println!(
                "subsig: {} ({}) {}",
                sub.name,
                params.join(", "),
                sub.ret.map_or("-", |t| t.as_str())
            );
        }
    }
    for form in module.forms() {
        println!(
            "form: {} span={}..{}",
            form.name, form.line_span.0, form.line_span.1
        );
        print_members(&form.name, &form.properties, &form.handlers);
        for c in &form.children {
            println!("component: {} {}", c.id, c.type_name);
            print_members(&c.id, &c.properties, &c.handlers);
        }
    }
    // A module-level component is a DISTINCT line kind. The designer folds
    // `component:` into the form's children, and a timer written back inside
    // the form is source the compiler refuses.
    let spans = module_component_spans(&src, &module);
    for (c, span) in module.components().zip(spans) {
        match span {
            Some((a, b)) => println!("modcomponent: {} {} span={a}..{b}", c.id, c.type_name),
            None => println!("modcomponent: {} {}", c.id, c.type_name),
        }
        print_members(&c.id, &c.properties, &c.handlers);
    }
    0
}

/// The `prop:` and `handler:` lines of a form or component, keyed by its id.
fn print_members(id: &str, props: &[(String, kiln_ir::Expr)], handlers: &[(String, String)]) {
    for (name, value) in props {
        println!("prop: {id} {name} {}", escape_value(&literal_text(value)));
    }
    for (event, handler) in handlers {
        println!("handler: {id} {event} {handler}");
    }
}

/// Where each module-level component sits in the file, in declaration order.
///
/// The parser records a span for a form and not for a component, so this
/// finds it again from the token stream: a header is `type id` at the start of
/// a line outside every form, and the first `end` after it closes the block,
/// because a component body holds only properties and bindings and nothing
/// that nests. Matching on the exact (type, id) pair is what keeps
/// `record point` — the same two tokens — from being taken for one.
///
/// The right home for this is a `line_span` on `ir::Component`. Until then a
/// component the walk cannot place gets no span — which cannot happen for a
/// file the parser accepted, since the header it looks for is the one the
/// parser consumed — and the designer would append it as new on save rather
/// than splice at a guess.
fn module_component_spans(src: &str, module: &Module) -> Vec<Option<(usize, usize)>> {
    use kiln_ir::lexer::{lex, Tok};
    let toks = match lex(src) {
        Ok(t) => t,
        Err(_) => return module.components().map(|_| None).collect(),
    };
    let form_spans: Vec<(usize, usize)> = module.forms().map(|f| f.line_span).collect();
    let mut cursor = 0;
    module
        .components()
        .map(|c| {
            let mut i = cursor;
            while i + 2 < toks.len() {
                let at_line_start = i == 0 || matches!(toks[i - 1].tok, Tok::Newline);
                let line = toks[i].line;
                let header = at_line_start
                    && matches!(&toks[i].tok, Tok::Ident(t) if *t == c.type_name)
                    && matches!(&toks[i + 1].tok, Tok::Ident(id) if *id == c.id)
                    && matches!(toks[i + 2].tok, Tok::Newline)
                    && !form_spans.iter().any(|(a, b)| (*a..=*b).contains(&line));
                if header {
                    let end = toks[i + 3..].iter().find(|t| matches!(t.tok, Tok::End))?;
                    cursor = i + 3;
                    return Some((line, end.line));
                }
                i += 1;
            }
            None
        })
        .collect()
}

/// Render a property literal as the designer should display and re-emit it.
fn literal_text(e: &kiln_ir::Expr) -> String {
    use kiln_ir::Expr;
    match e {
        Expr::TextLit(s) => s.clone(),
        Expr::IntLit(v) => v.to_string(),
        // A property written as a bit pattern (`0xFF`) is shown as the number
        // it is: the designer re-emits what it is shown, and it writes decimal.
        Expr::BitsLit(v) => kiln_ir::sema::bits_value(*v).to_string(),
        Expr::DoubleLit(v) => v.to_string(),
        Expr::BoolLit(b) => b.to_string(),
        _ => String::new(),
    }
}

/// Keep a `prop:` value on its one line.
///
/// The output is read a line at a time, so a raw newline in a memo's text
/// used to arrive as extra unlabelled lines the reader had to guess were
/// continuations. Backslash is escaped so the reversal is unambiguous, and NUL
/// because a C reader working in `char *` stops at one. Everything else,
/// including tab, cannot break a line and travels raw.
fn escape_value(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for ch in v.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\0' => out.push_str("\\0"),
            c => out.push(c),
        }
    }
    out
}

fn cmd_build(rest: &[String], then_run: bool) -> i32 {
    let io = match parse_io(rest) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("kiln: {e}");
            return 2;
        }
    };
    let input = io.input;
    if io.arch == Arch::X86 && io.os != Os::Windows {
        eprintln!(
            "kiln: the 32-bit backend targets Windows x86 only — build with \
             `--os windows --arch x86`"
        );
        return 2;
    }
    if io.os == Os::Windows {
        // Before any compiling: the one-line answer beats the same fact
        // arriving as a clang error after the IR has been generated.
        if let Err(e) = mingw_available(io.arch) {
            eprintln!("kiln: {e}");
            return 1;
        }
        if then_run {
            eprintln!(
                "kiln: cannot run a Windows program here — build it, then run it under \
                 wine or on Windows"
            );
            return 2;
        }
    }
    if io.arch == Arch::X86 && io.target == Some(Target::Gui) {
        eprintln!(
            "kiln: a GUI cannot be built for x86 yet — the vendored UI stack \
             (RmlUi, SDL2, freetype) is 64-bit only. Use a console or shared/static \
             library, or build for x86_64."
        );
        return 2;
    }
    let (mut ll, mut plan, target, module) = match compile(&input, io.target, io.os, io.arch, io.release) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("kiln: {e}");
            return 1;
        }
    };

    // A shared library that declares `dll_attach`/`dll_detach` gets a platform
    // loader entry — `DllMain` on Windows, an ELF constructor on Linux. The
    // shim lives in runtime/kn_dllmain.c, which compiles to nothing unless
    // KN_DLLMAIN is defined, so turning it on for this one target leaves every
    // other build (and the ordinary hook-less sharedlib) exactly as it was.
    // The macros carry the two facts the C cannot know for itself: the module's
    // `<module>_init` symbol, and which of the two hooks the module defined.
    if target == Target::SharedLib {
        let has_attach = module.subs().any(|s| s.name == "dll_attach");
        let has_detach = module.subs().any(|s| s.name == "dll_detach");
        if has_attach || has_detach {
            plan.build.defines.push("KN_DLLMAIN".into());
            plan.build
                .defines
                .push(format!("KN_MODULE_INIT={}_init", module.name));
            if has_attach {
                plan.build.defines.push("KN_HAS_ATTACH".into());
            }
            if has_detach {
                plan.build.defines.push("KN_HAS_DETACH".into());
            }
        }
    }

    let out_bin = match io.output {
        Some(p) => output_for_os(p, target, io.os),
        None => default_output(&input, io.project_output.as_deref(), target, io.os),
    };

    // Only a program builds a form, so only a program has pictures to carry.
    if target.is_executable() {
        match embed_resources(&module, &input) {
            Ok(Some(table)) => ll.push_str(&table),
            Ok(None) => {
                if io.os == Os::Windows {
                    ll.push_str(EMPTY_RESOURCE_TABLE);
                }
            }
            Err(e) => {
                eprintln!("kiln: {e}");
                return 1;
            }
        }
    }

    let ll_path = out_bin.with_extension("ll");
    if let Err(e) = std::fs::write(&ll_path, &ll) {
        eprintln!("kiln: cannot write {}: {e}", ll_path.display());
        return 1;
    }

    let repo_root = find_repo_root().expect("runtime located during compile()");
    let linked = clang_link(
        &ll_path, &repo_root, &plan, &out_bin, target, io.os, io.arch, io.release,
    );
    // The `.ll` was clang's input, not an output anyone asked for. It goes
    // whether the link succeeded or not — a failed build should not leave a
    // file behind either. Failure to remove it is not worth a message: the
    // build's own result is the news.
    if !io.emit_ir {
        let _ = std::fs::remove_file(&ll_path);
    }
    if let Err(code) = linked {
        return code;
    }
    eprintln!("kiln: wrote {}", out_bin.display());
    if io.emit_ir {
        eprintln!("kiln: wrote {}", ll_path.display());
    }

    // A library is only usable with its prototypes, so they come out of the
    // same IR the artifact did. Written after the link, so a failed build
    // leaves no header claiming exports that were never produced.
    if !target.is_executable() {
        let header_path = io
            .header
            .unwrap_or_else(|| header::default_path(&out_bin, &module.name));
        if let Err(e) = std::fs::write(&header_path, header::render(&module)) {
            eprintln!("kiln: cannot write {}: {e}", header_path.display());
            return 1;
        }
        eprintln!("kiln: wrote {}", header_path.display());
        if io.os == Os::Windows && target == Target::SharedLib {
            eprintln!("kiln: wrote {}", implib_path(&out_bin).display());
        }
    }

    if then_run {
        let status = Command::new(absolutize(&out_bin)).status();
        match status {
            Ok(s) => s.code().unwrap_or(1),
            Err(e) => {
                eprintln!("kiln: failed to run {}: {e}", out_bin.display());
                1
            }
        }
    } else {
        0
    }
}

/// Parse, introspect libraries, validate, and lower to LLVM IR.
/// Returns the `.ll` text and the implementation sources to static-link.
/// Parse a program and merge the units it `use`s — see `kiln_ir::units`.
///
/// Every reader of a program that goes on to validate or lower it comes
/// through here, so `build`, `run`, `emit` and the language server agree on
/// what a program *is*. A unit's own error is reported with the unit's file
/// and line, since the program's line numbers would point at the wrong text.
fn parse_program(
    src: &str,
    input: &Path,
    opts: ParseOptions,
) -> Result<kiln_ir::Resolved, String> {
    let module = parse_with(src, opts).map_err(|e| e.to_string())?;
    if module.is_unit {
        return Err(format!(
            "{} is a unit, not a program — build the program that says `use {}`",
            input.display(),
            module.name
        ));
    }
    let dir = input
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    resolve_units(module, &dir, opts).map_err(|e| e.to_string())
}

/// Say which unit file a diagnostic belongs to, when it names something a
/// unit declared. The validator reports positions in one merged module; the
/// name it mentions is what maps a line back to the file it is in.
fn annotate_origins(
    errs: Vec<kiln_ir::validate::ValidateError>,
    origins: &std::collections::HashMap<String, PathBuf>,
) -> Vec<kiln_ir::validate::ValidateError> {
    if origins.is_empty() {
        return errs;
    }
    errs.into_iter()
        .map(|mut e| {
            // "in `twice`: ..." is the validator's own anchor: the subroutine
            // the position is inside. Only that name says which file the line
            // number belongs to — a record or constant the message mentions
            // may well live in a unit while the error is in the program.
            let inside = e
                .msg
                .strip_prefix("in `")
                .and_then(|rest| rest.split_once('`'))
                .map(|(name, _)| name.to_string());
            if let Some(n) = inside {
                if let Some(file) = origins.get(&n) {
                    e.msg = format!("in unit {}: {}", file.display(), e.msg);
                }
            }
            e
        })
        .collect()
}

fn compile(
    input: &Path,
    target_override: Option<Target>,
    os: Os,
    arch: Arch,
    release: bool,
) -> Result<(String, libload::LibPlan, Target, Module), String> {
    compile_with(input, target_override, os, arch, true, release)
}

/// `require_impl` is false when the caller only wants the IR: emitting it
/// exercises parsing, validation and lowering, none of which need a library to
/// be linkable. Demanding a vendored UI stack to *print* IR would make the
/// check unavailable exactly where it is most useful — a fresh checkout.
fn compile_with(
    input: &Path,
    target_override: Option<Target>,
    os: Os,
    arch: Arch,
    require_impl: bool,
    release: bool,
) -> Result<(String, libload::LibPlan, Target, Module), String> {
    let src = std::fs::read_to_string(input)
        .map_err(|e| format!("cannot read {}: {e}", input.display()))?;
    // The module that is *checked* always carries its asserts, whatever the
    // build: a release build has to refuse every mistake a debug build refuses,
    // and an assert whose condition is nonsense is one of them. Only what is
    // *lowered* differs — see the second parse below.
    let resolved = parse_program(&src, input, ParseOptions::default())?;
    let origins = resolved.origins;
    let mut module = resolved.module;
    // An explicit --target wins over the module's declaration: the same source
    // should be buildable as a program or a library without editing it.
    if let Some(t) = target_override {
        module.target = Some(t);
    }
    let target = module.target();

    let repo_root = find_repo_root().ok_or_else(|| {
        "could not locate the Kiln runtime (runtime/kiln_core.h); \
         set KILN_RUNTIME_DIR or run from the repo root"
            .to_string()
    })?;

    // Introspect `core` + each `use`d library for command signatures (the
    // authoritative source — no hard-coded table).
    // A kit resolved outside `libs/` is presented to the loader through a
    // staged root, so listing a command and calling it agree by construction.
    let lib_root = kit::overlay_root(&repo_root, &module.uses)?;
    let plan = if !require_impl {
        libload::load_metadata(&lib_root, &module.uses, arch)?
    } else if os == Os::host() {
        libload::load(&lib_root, &module.uses)?
    } else {
        libload::load_cross(&lib_root, &module.uses, arch)?
    };

    // A kit that restricts itself to a set of operating systems (a Win32
    // declaration kit, say) cannot be built for one outside that set: the
    // symbols it names live in that platform's own libraries. Named here, at
    // the build, with the kit and the OS it needs — listing its contents stays
    // allowed anywhere, which is why the gate is not in the loader.
    for (kit, platforms) in &plan.gated {
        if !platforms.iter().any(|p| p == os.as_platform()) {
            return Err(format!(
                "kit `{kit}` supports {} — it cannot be built for {}. Build with `--os {}`.",
                platforms.join(", "),
                os.as_platform(),
                platforms.first().map(String::as_str).unwrap_or("<platform>")
            ));
        }
    }

    if let Err(errs) = validate_hinted(&module, &plan.registry, &repo_root) {
        let errs = annotate_origins(errs, &origins);
        let joined = errs
            .iter()
            .map(|e| format!("  - {e}"))
            .collect::<Vec<_>>()
            .join("\n");
        return Err(format!(
            "{} error(s) in {}:\n{joined}",
            errs.len(),
            input.display()
        ));
    }
    // `--release` is the one build flag the *language* sees: it drops each
    // `assert` where it is written, so a release binary carries no check, no
    // branch and no message. The dropping is done by parsing the source a
    // second time — parsing is text, and paying for it twice is far cheaper
    // than a release build that could compile something the debug build
    // rejected. Every other reader of a file — `inspect`, the language server,
    // the designer — never gets here: they read the program as written.
    if release {
        let mut stripped = parse_program(&src, input, ParseOptions { asserts: false })?.module;
        if let Some(t) = target_override {
            stripped.target = Some(t);
        }
        module = stripped;
    }
    // Debug information is on by default and off in a release build. On by
    // default because a program you are still writing is one you may want to
    // step through, and asking for a separate debug build is a mode switch
    // nobody remembers to make; off in release because `--release` already
    // means "strip what a user does not need", and this is the largest part
    // of it.
    let source = if release {
        None
    } else {
        input.to_str()
    };
    let ll = lower_module_for(
        &module,
        &plan.registry,
        source,
        debug_format(),
        arch.target_info(os),
    )
    .map_err(|e| e.to_string())?;
    Ok((ll, plan, target, module))
}

/// Validate, and when a command is unknown, say which library has it.
///
/// The same two passes the language server makes (`Server::diagnose`): the
/// cheap one first, and the map of every kit's commands only when an unknown
/// command is what went wrong — it costs an introspection build per kit, and
/// a program that validates cleanly should not pay for it. Without this the
/// editor said "add `use file`" and the terminal did not, and the terminal is
/// where a build fails.
fn validate_hinted(
    module: &Module,
    registry: &Registry,
    repo_root: &Path,
) -> Result<(), Vec<kiln_ir::validate::ValidateError>> {
    let Err(errs) = validate_with(module, registry, &Hints::default()) else {
        return Ok(());
    };
    if !errs.iter().any(|e| e.msg.contains("unknown command `")) {
        return Err(errs);
    }
    let hints = Hints {
        elsewhere: elsewhere(repo_root),
    };
    match validate_with(module, registry, &hints) {
        Err(better) => Err(better),
        Ok(()) => Err(errs),
    }
}

/// Every command of every kit the toolchain can see, and which kit it is in.
/// One kit at a time, as `Server::elsewhere` does: two kits may legitimately
/// export the same name, and a registry holding both would refuse to load.
fn elsewhere(repo_root: &Path) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for k in kit::resolve_all(repo_root) {
        let uses = vec![k.name.clone()];
        let Ok(root) = kit::overlay_root(repo_root, &uses) else { continue };
        if let Ok(plan) = libload::load_metadata(&root, &uses, Arch::host()) {
            for (name, _) in plan.registry.iter() {
                map.entry(name.to_string()).or_insert_with(|| k.name.clone());
            }
        }
    }
    for name in Registry::core().names() {
        map.remove(name);
    }
    map
}

/// Compile every picture a form names INTO the program.
///
/// `image.source = "logo.png"` is otherwise a promise about the machine the
/// program was built on, and "ship one file" is the claim the whole model rests
/// on. The bytes go into the program's own object as a table the `ui` library
/// reads instead of reaching for the filesystem (`libs/ui/ui_rmlui.cpp`).
///
/// A source that does not exist is a BUILD error. The alternative — an empty
/// picture in a running program — is the same missing file discovered by
/// whoever the program was shipped to.
///
/// Returns `None` when the module names no resources, so a program that has
/// none defines no table at all and the weak declaration on the other side sees
/// the empty one.
fn embed_resources(module: &Module, input: &Path) -> Result<Option<String>, String> {
    // Relative to the SOURCE, not to the working directory: a project is built
    // from wherever the person happens to be standing.
    let base = input.parent().unwrap_or(Path::new("."));
    let mut found: Vec<(String, Vec<u8>)> = Vec::new();
    for form in module.forms() {
        // The form's own icon rides the same path as an image's source: a
        // window icon that only exists on the author's disk is not shipped.
        let mut wanted: Vec<(&str, &kiln_ir::Expr)> = form
            .properties
            .iter()
            .filter(|(name, _)| name == "icon")
            .map(|(_, v)| (form.name.as_str(), v))
            .collect();
        for child in &form.children {
            if child.type_name != "image" {
                continue;
            }
            for (name, v) in &child.properties {
                if name == "source" {
                    wanted.push((child.id.as_str(), v));
                }
            }
        }
        for (owner, value) in wanted {
            {
                let src = literal_text(value);
                if src.is_empty() {
                    continue;
                }
                if found.iter().any(|(n, _)| *n == src) {
                    continue;
                }
                let path = base.join(&src);
                let bytes = std::fs::read(&path).map_err(|e| {
                    format!(
                        "{}: `{}` has source `{src}`, which cannot be read: {e}",
                        input.display(),
                        owner
                    )
                })?;
                if bytes.is_empty() {
                    // A zero-length picture is a file that exists and says
                    // nothing, which is the failure this check is for.
                    return Err(format!(
                        "{}: `{}` has source `{src}`, which is empty",
                        input.display(),
                        owner
                    ));
                }
                found.push((src, bytes));
            }
        }
    }
    if found.is_empty() {
        return Ok(None);
    }

    let mut out = String::from(
        "\n; Resources embedded at build time; read by libs/ui through a null-terminated\n\
         ; table, so a program that names none can leave the symbol undefined.\n",
    );
    for (i, (name, bytes)) in found.iter().enumerate() {
        out.push_str(&format!(
            "@.res.name{i} = private unnamed_addr constant [{} x i8] c\"{}\\00\"\n",
            name.len() + 1,
            llvm_bytes(name.as_bytes())
        ));
        out.push_str(&format!(
            "@.res.data{i} = private unnamed_addr constant [{} x i8] c\"{}\"\n",
            bytes.len(),
            llvm_bytes(bytes)
        ));
    }
    let mut rows: Vec<String> = found
        .iter()
        .enumerate()
        .map(|(i, (_, bytes))| {
            format!(
                "{{ ptr, ptr, i64 }} {{ ptr @.res.name{i}, ptr @.res.data{i}, i64 {} }}",
                bytes.len()
            )
        })
        .collect();
    rows.push("{ ptr, ptr, i64 } zeroinitializer".to_string());
    out.push_str(&format!(
        "@kn_embedded_resources = constant [{} x {{ ptr, ptr, i64 }}] [{}]\n",
        rows.len(),
        rows.join(", ")
    ));
    Ok(Some(out))
}

/// Bytes as an LLVM string body. Everything outside plain printable ASCII goes
/// as `\XX`, which is the only form that survives a byte a text editor would
/// otherwise eat.
fn llvm_bytes(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        if (0x20..0x7f).contains(&b) && b != b'"' && b != b'\\' {
            out.push(b as char);
        } else {
            out.push_str(&format!("\\{b:02X}"));
        }
    }
    out
}

fn default_output(input: &Path, project_output: Option<&Path>, target: Target, os: Os) -> PathBuf {
    let (dir, stem) = match project_output {
        Some(p) => (
            p.parent().unwrap_or(Path::new(".")).to_path_buf(),
            p.file_name().and_then(|s| s.to_str()).unwrap_or("a").to_string(),
        ),
        None => (
            PathBuf::new(),
            input.file_stem().and_then(|s| s.to_str()).unwrap_or("a").to_string(),
        ),
    };
    // Libraries follow the platform convention, so a host's linker finds them
    // by the name it expects (`-lgreet` wants `libgreet.so`, and on Windows
    // `greet.dll` beside a `libgreet.a` — mingw's archives keep the Unix name).
    dir.join(match (os, target) {
        (Os::Linux, Target::Console | Target::Gui) => stem,
        (Os::Linux, Target::SharedLib) => format!("lib{stem}.so"),
        (Os::Windows, Target::Console | Target::Gui) => format!("{stem}.exe"),
        (Os::Windows, Target::SharedLib) => format!("{stem}.dll"),
        (_, Target::StaticLib) => format!("lib{stem}.a"),
    })
}

/// An explicit `-o` for a Windows program gets `.exe` when it has no
/// extension: Windows will not run a file without one, and `-o hello` is
/// what everyone types. A name that carries an extension is left as given.
fn output_for_os(out: PathBuf, target: Target, os: Os) -> PathBuf {
    if os == Os::Windows && target.is_executable() && out.extension().is_none() {
        out.with_extension("exe")
    } else {
        out
    }
}

/// Which spelling of the debug-info declaration the installed clang can read.
///
/// LLVM 19 introduced debug records and LLVM 21 deleted the intrinsics they
/// replaced, so there is no form both ends of the supported range accept: a
/// module written for clang 21 does not parse under clang 18, which is what
/// Ubuntu 24.04 still ships. Since debug information is on by default, getting
/// this wrong is not a degraded build but a build that fails outright, so the
/// version is asked for rather than assumed.
///
/// An unreadable answer means records — the modern form, and the one every
/// clang from 19 on accepts. `KILN_CLANG_MAJOR` overrides the probe, which is
/// how the emitted IR is tested against a version that is not installed.
fn debug_format() -> DebugFormat {
    match clang_major() {
        Some(n) if n < 19 => DebugFormat::Intrinsics,
        _ => DebugFormat::Records,
    }
}

/// The major version of `clang`, from the first line of `clang --version`.
///
/// Vendors rewrite that line freely — "Ubuntu clang version 18.1.3", "Apple
/// clang version 17.0.0" — so rather than parse the prose this takes the first
/// dotted number after the word `version`. Anything it cannot read is `None`
/// rather than a guess.
fn clang_major() -> Option<u32> {
    if let Ok(forced) = std::env::var("KILN_CLANG_MAJOR") {
        return forced.trim().parse().ok();
    }
    let out = Command::new("clang").arg("--version").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().next()?;
    let after = line.split("version").nth(1)?;
    let num: String = after
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    num.parse().ok()
}

/// Is the mingw-w64 cross compiler installed? One line naming the package
/// when it is not, because the alternative is a screen of "cannot find
/// crt2.o" from a driver that was never there.
fn mingw_available(arch: Arch) -> Result<(), String> {
    let tc = mingw(arch);
    match Command::new(tc.gcc)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
    {
        Ok(s) if s.success() => Ok(()),
        _ => Err(format!(
            "building for windows needs the mingw-w64 cross compiler `{}` (package \
             mingw64-gcc on Fedora, gcc-mingw-w64-x86-64 on Debian and Ubuntu; for \
             --arch x86, mingw32-gcc / gcc-mingw-w64-i686)",
            tc.gcc
        )),
    }
}

/// Invoke clang to assemble the `.ll` and static-link the library implementation
/// sources into a native executable, dead-stripping unused commands.
fn clang_link(
    ll_path: &Path,
    repo_root: &Path,
    plan: &libload::LibPlan,
    out_bin: &Path,
    target: Target,
    os: Os,
    arch: Arch,
    release: bool,
) -> Result<(), i32> {
    let cfg = &plan.build;
    let driver = if cfg.needs_cxx { "clang++" } else { "clang" };
    if os == Os::Windows {
        return mingw_link(ll_path, repo_root, plan, out_bin, target, arch, release);
    }

    // Flags every invocation needs, whether we are linking a program or
    // compiling one object at a time for an archive.
    let mut common: Vec<String> = vec![
        "-ffunction-sections".into(),
        "-fdata-sections".into(),
        "-Wno-override-module".into(),
        "-I".into(),
        repo_root.join("abi").display().to_string(),
        "-I".into(),
        repo_root.join("runtime").display().to_string(),
    ];
    // A debug build is the default and stays exactly as it was: fast to
    // produce, and the shape a developer iterates on.
    if release {
        common.splice(0..0, release_cflags(driver, target.is_executable()));
    } else {
        common.insert(0, "-O0".into());
    }
    for d in &cfg.include_dirs {
        common.push("-I".into());
        common.push(d.display().to_string());
    }
    for d in &cfg.defines {
        common.push(format!("-D{d}"));
    }
    match libload::pkg_config_flags(&cfg.pkg_config, "--cflags") {
        Ok(flags) => common.extend(flags),
        Err(e) => {
            eprintln!("kiln: {e}");
            return Err(1);
        }
    }
    // Both library kinds must be position independent: a shared object requires
    // it, and a static archive is routinely linked into one.
    if !target.is_executable() {
        common.push("-fPIC".into());
    }

    // The inputs, each with the language it must be compiled as. When any
    // library needs C++ the driver is clang++, which would otherwise compile
    // our .c files as C++ and mangle their symbols, breaking the C ABI the
    // emitted IR calls.
    let mut inputs: Vec<(PathBuf, Option<&'static str>)> = vec![(ll_path.to_path_buf(), None)];
    for s in &plan.impl_sources {
        // The process-entry object provides `main`, which calls `ECodeStart`.
        // A library has no `ECodeStart`, so linking it in leaves an undefined
        // symbol and the `.so` fails to dlopen — a file with the right
        // extension that cannot actually be loaded. kn_start.c lives in its own
        // TU precisely so a build target can drop it.
        if !target.is_executable() && s.file_name().and_then(|f| f.to_str()) == Some("kn_start.c") {
            continue;
        }
        let is_cxx = matches!(
            s.extension().and_then(|e| e.to_str()),
            Some("cpp") | Some("cc") | Some("cxx")
        );
        let lang = if cfg.needs_cxx {
            Some(if is_cxx { "c++" } else { "c" })
        } else {
            None
        };
        inputs.push((s.clone(), lang));
    }

    if target == Target::StaticLib {
        return build_archive(driver, &[], "ar", &common, &inputs, out_bin);
    }

    // Everything after the inputs, in the order the link has always used it.
    let mut libs: Vec<String> = cfg.link_args.clone();
    match libload::pkg_config_flags(&cfg.pkg_config, "--libs") {
        Ok(flags) => libs.extend(flags),
        Err(e) => {
            eprintln!("kiln: {e}");
            return Err(1);
        }
    }
    libs.push("-lm".into()); // libm for the floating-point commands
    // libdl for the foreign-function loader (runtime/kn_dll.c's dlopen/dlsym).
    // Only on Linux, and only for a native build: glibc >= 2.34 folds these
    // into libc so the flag is a harmless no-op there, but an older host still
    // needs it — and macOS has no `libdl` to name (the calls live in libSystem)
    // while the Windows loader is kernel32, linked by the mingw path instead.
    if cfg!(target_os = "linux") {
        libs.push("-ldl".into());
    }
    if target == Target::SharedLib {
        libs.push("-shared".into());
    } else {
        // Dead-strip: the headline property of the BlackMoon model.
        // Only for programs — a library must keep exports no host has linked
        // yet, and --gc-sections would drop every one of them.
        libs.push("-Wl,--gc-sections".into());
    }

    let link = |common: &[String], ldflags: &[String], quiet: bool| -> Result<bool, i32> {
        let mut cmd = Command::new(driver);
        cmd.args(common);
        for (path, lang) in &inputs {
            if let Some(l) = lang {
                cmd.arg("-x").arg(l);
            }
            cmd.arg(path);
        }
        cmd.args(&libs);
        cmd.args(ldflags);
        cmd.arg("-o").arg(out_bin);
        if quiet {
            // Held back rather than dropped: a first attempt that succeeds
            // still has its warnings to say.
            match cmd.output() {
                Ok(o) => {
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    if o.status.success() {
                        eprint!("{stderr}");
                    }
                    if debug_info_discarded(&stderr) {
                        return Err(report_discarded_debug_info());
                    }
                    Ok(o.status.success())
                }
                Err(e) => {
                    eprintln!("kiln: {}", crate::libload::spawn_error(driver, &e));
                    Err(1)
                }
            }
        } else {
            match cmd.output() {
                Ok(o) => {
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    eprint!("{stderr}");
                    if debug_info_discarded(&stderr) {
                        return Err(report_discarded_debug_info());
                    }
                    if !o.status.success() {
                        eprintln!("kiln: clang failed with status {}", o.status);
                    }
                    Ok(o.status.success())
                }
                Err(e) => {
                    eprintln!("kiln: {}", crate::libload::spawn_error(driver, &e));
                    Err(1)
                }
            }
        }
    };

    /// Whether clang threw the module's debug information away.
    ///
    /// It says so in a warning and then exits 0, having produced a binary with
    /// no `.debug_*` sections at all — and `-w` and `-Wno-everything` do not
    /// suppress the warning, which is what makes it usable as a check. Without
    /// it, every mistake in the emitted metadata looks like a working build
    /// and a debugger that sees nothing.
    fn debug_info_discarded(stderr: &str) -> bool {
        stderr.contains("ignoring invalid debug info")
    }

    /// Refuse the build rather than ship a binary that cannot be debugged.
    /// This is a compiler bug when it happens, not anything a user did, so it
    /// says so and asks to be told.
    fn report_discarded_debug_info() -> i32 {
        eprintln!(
            "kiln: the debug information this build emitted was rejected by LLVM \
             and thrown away, so the binary could not be stepped through."
        );
        eprintln!(
            "kiln: this is a bug in the compiler, not in your program. Please \
             report it, and pass `--emit-ir` to keep the .ll named above for \
             the report. `--release` builds emit none and are unaffected."
        );
        1
    }

    let ldflags = if release {
        release_ldflags(driver, &common, target.is_executable())
    } else {
        Vec::new()
    };

    // A probe proves the driver accepts -pie; only the real link proves the
    // objects allow it. A vendored static library built without -fPIC — the UI
    // stack is one — cannot go into a position-independent program, and that
    // is a fact about the dependency, not a reason to fail the build.
    let pie = ldflags.iter().any(|f| f.ends_with("pie"));
    if pie {
        if link(&common, &ldflags, true)? {
            return Ok(());
        }
        eprintln!(
            "kiln: this program links a library that is not position-independent; \
             building the release without PIE"
        );
        let common: Vec<String> = common.iter().filter(|f| *f != "-fPIE").cloned().collect();
        let ldflags: Vec<String> = ldflags
            .iter()
            .filter(|f| !f.ends_with("pie"))
            .cloned()
            .collect();
        return if link(&common, &ldflags, false)? {
            Ok(())
        } else {
            Err(1)
        };
    }

    if link(&common, &ldflags, false)? {
        Ok(())
    } else {
        Err(1)
    }
}

/// Build for Windows x86-64: clang compiles the IR and every C source against
/// the mingw-w64 target, mingw's g++ compiles the C++, and mingw's gcc (g++
/// when there is C++ to carry) links them.
///
/// Separate from the host link on purpose. The host path is one command and a
/// PIE retry that both assume the compiler and the linker are the same driver;
/// here they are not, and PIE does not exist — a PE is relocated by the loader
/// whenever the image says it may be (`--dynamicbase`). Sharing the code would
/// mean threading the OS through every line of it; keeping it apart keeps the
/// Linux build exactly what it was.
fn mingw_link(
    ll_path: &Path,
    repo_root: &Path,
    plan: &libload::LibPlan,
    out_bin: &Path,
    target: Target,
    arch: Arch,
    release: bool,
) -> Result<(), i32> {
    let cfg = &plan.build;
    let tc = mingw(arch);
    let driver = "clang";
    let driver_args = vec![format!("--target={}", tc.triple)];

    let mut common: Vec<String> = vec![
        "-ffunction-sections".into(),
        "-fdata-sections".into(),
        "-I".into(),
        repo_root.join("abi").display().to_string(),
        "-I".into(),
        repo_root.join("runtime").display().to_string(),
    ];
    for d in &cfg.include_dirs {
        common.push("-I".into());
        common.push(d.display().to_string());
    }
    for d in &cfg.defines {
        common.push(format!("-D{d}"));
    }
    // The sysroot's pkg-config, never the host's: its `-I` names the mingw
    // SDL2 and freetype headers, which is what the ui library compiles
    // against for Windows (libs/ui/lib.json, `windows_pkg_config`).
    match libload::pkg_config_flags_cross(&cfg.pkg_config, "--cflags", arch) {
        Ok(flags) => common.extend(flags),
        Err(e) => {
            eprintln!("kiln: {e}");
            return Err(1);
        }
    }
    // No -fPIC: every PE image is relocatable already, and clang's mingw
    // target says so — with a warning — when asked for it.

    let ldflags = if release {
        let cflags = mingw_release_cflags(&driver_args);
        common.splice(0..0, cflags.iter().cloned());
        mingw_release_ldflags(arch, &driver_args, &cflags)
    } else {
        common.insert(0, "-O0".into());
        Vec::new()
    };

    // clang's flags, and g++'s: the same list minus what only clang knows.
    // The release flags are gcc's own vocabulary and both take them.
    let mut clang_common = common.clone();
    clang_common.push("-Wno-override-module".into());
    // One object per input means the `.ll` is compiled on its own, and an
    // include path is meaningless to it; clang says so for every build
    // otherwise.
    clang_common.push("-Wno-unused-command-line-argument".into());
    let mut gxx_common = common.clone();
    gxx_common.push("-std=gnu++17".into());

    let mut c_inputs: Vec<(PathBuf, Option<&'static str>)> = vec![(ll_path.to_path_buf(), None)];
    let mut cxx_inputs: Vec<(PathBuf, Option<&'static str>)> = Vec::new();
    for s in &plan.impl_sources {
        // As on the host: the entry object belongs to a program only.
        if !target.is_executable() && s.file_name().and_then(|f| f.to_str()) == Some("kn_start.c") {
            continue;
        }
        let is_cxx = matches!(
            s.extension().and_then(|e| e.to_str()),
            Some("cpp") | Some("cc") | Some("cxx")
        );
        if is_cxx {
            cxx_inputs.push((s.clone(), None));
        } else {
            c_inputs.push((s.clone(), Some("c")));
        }
    }

    if target == Target::StaticLib {
        if !cxx_inputs.is_empty() {
            // An archive is one compile per object and one `ar`; two
            // compilers would need two passes into one archive, and no
            // library target has asked for it — a library cannot declare a
            // form, and `use ui` without one is a program's mistake to make.
            eprintln!(
                "kiln: a static library for windows cannot carry C++ sources (this \
                 program uses a library that needs them)"
            );
            return Err(1);
        }
        return build_archive(driver, &driver_args, tc.ar, &clang_common, &c_inputs, out_bin);
    }

    let dir = std::env::temp_dir().join(format!("kiln_mingw_{}", std::process::id()));
    let c_dir = dir.join("c");
    let cxx_dir = dir.join("cxx");
    if let Err(e) = std::fs::create_dir_all(&c_dir).and_then(|_| std::fs::create_dir_all(&cxx_dir)) {
        eprintln!("kiln: cannot create {}: {e}", dir.display());
        return Err(1);
    }
    let mut objects = match compile_objects(driver, &driver_args, &clang_common, &c_inputs, &c_dir) {
        Ok(o) => o,
        Err(code) => {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(code);
        }
    };
    if !cxx_inputs.is_empty() {
        match compile_objects(tc.gxx, &[], &gxx_common, &cxx_inputs, &cxx_dir) {
            Ok(o) => objects.extend(o),
            Err(code) => {
                let _ = std::fs::remove_dir_all(&dir);
                return Err(code);
            }
        }
    }

    // The link driver follows the sources: g++ knows where libstdc++ is.
    let linker = if cxx_inputs.is_empty() { tc.gcc } else { tc.gxx };
    let mut cmd = Command::new(linker);
    cmd.args(&objects);
    cmd.args(&cfg.link_args);
    match libload::pkg_config_flags_cross(&cfg.pkg_config, "--libs", arch) {
        // `-mwindows` is a fact about the TARGET, said below for a form and
        // not for a console program that merely uses the library; and
        // `SDL2main` is the entry SDL offers a program without one, which a
        // program with `main` in runtime/kn_start.c does not want.
        Ok(flags) => cmd.args(
            flags
                .iter()
                .filter(|f| !matches!(f.as_str(), "-mwindows" | "-lmingw32" | "-lSDL2main")),
        ),
        Err(e) => {
            eprintln!("kiln: {e}");
            let _ = std::fs::remove_dir_all(&dir);
            return Err(1);
        }
    };
    cmd.arg("-lm");
    // Winsock is not among the libraries mingw links by default, and `net`
    // is the one library that needs it (libs/net/net_internal.h). Always
    // named rather than only then: an import library nothing references
    // costs the link nothing, and the link does not have to know which
    // library is which.
    cmd.arg("-lws2_32");
    // libgcc goes into the image. On 32-bit i386 a `long long` divide and
    // several other multi-word operations are libgcc calls, so a DLL-only C
    // program would still import `libgcc_s_dw2-1.dll` (and, through it,
    // `libwinpthread-1.dll`) — two files the machine it lands on has never
    // heard of. Static is what the 64-bit path already got for free.
    cmd.arg("-static-libgcc");
    if !cxx_inputs.is_empty() {
        // The C++ runtime goes in the same way, and for the same reason.
        cmd.arg("-static-libstdc++");
    }
    if target == Target::SharedLib {
        cmd.arg("-shared");
        // A Windows consumer links against `greet.lib`, never the DLL itself,
        // so the import library comes out beside it under the name a
        // `#pragma comment(lib, "greet.lib")` expects. ld writes it as part
        // of the same link, from the same export table.
        cmd.arg(format!("-Wl,--out-implib,{}", implib_path(out_bin).display()));
    } else {
        cmd.arg("-Wl,--gc-sections");
    }
    if target == Target::Gui {
        // The GUI subsystem: Windows opens no console for the program. The
        // entry stays `main` — mingw's CRT runs it under either subsystem.
        cmd.arg("-mwindows");
    }
    cmd.args(&ldflags);
    cmd.arg("-o").arg(out_bin);
    let status = cmd.status();
    let _ = std::fs::remove_dir_all(&dir);
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => {
            eprintln!("kiln: {linker} failed with status {s}");
            return Err(1);
        }
        Err(e) => {
            eprintln!("kiln: {}", crate::libload::spawn_error(linker, &e));
            return Err(1);
        }
    }

    // A program that links the sysroot's DLLs cannot start without them, and
    // the machine it is copied to has no mingw sysroot. So they go beside
    // it, transitively: what the image imports, what those import, and the
    // ones a library loads by hand and names in its manifest.
    if target.is_executable() {
        match copy_windows_dlls(arch, out_bin, &cfg.extra_dlls) {
            Ok(dlls) if dlls.is_empty() => {}
            Ok(dlls) => eprintln!(
                "kiln: copied beside it, because the program imports them: {}",
                dlls.join(" ")
            ),
            Err(e) => {
                eprintln!("kiln: {e}");
                return Err(1);
            }
        }
    }
    Ok(())
}

/// Where the mingw-w64 sysroot keeps its DLLs: `<gcc -print-sysroot>/mingw/bin`
/// on Fedora, `/usr/x86_64-w64-mingw32/bin` on Debian and Ubuntu (and `lib`
/// there, for the packages that put them beside the import libraries).
fn mingw_dll_dirs(arch: Arch) -> Vec<PathBuf> {
    let tc = mingw(arch);
    let mut dirs = Vec::new();
    if let Ok(out) = Command::new(tc.gcc).arg("-print-sysroot").output() {
        let root = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !root.is_empty() && root != "/" {
            dirs.push(PathBuf::from(&root).join("mingw/bin"));
            dirs.push(PathBuf::from(&root).join("bin"));
        }
    }
    dirs.push(PathBuf::from(format!("{}/sys-root/mingw/bin", tc.sysroot)));
    dirs.push(PathBuf::from(format!("{}/bin", tc.sysroot)));
    dirs.push(PathBuf::from(format!("{}/lib", tc.sysroot)));
    dirs.retain(|d| d.is_dir());
    dirs.dedup();
    dirs
}

/// The DLLs a PE image imports, by name, from its own import table.
fn pe_imports(arch: Arch, image: &Path) -> Result<Vec<String>, String> {
    let tc = mingw(arch);
    let out = Command::new(tc.objdump)
        .arg("-p")
        .arg(image)
        .output()
        .map_err(|e| format!("could not invoke {}: {e}", tc.objdump))?;
    if !out.status.success() {
        return Err(format!("{} could not read {}", tc.objdump, image.display()));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().strip_prefix("DLL Name:"))
        .map(|n| n.trim().to_string())
        .collect())
}

/// Copy beside `image` every DLL from the mingw sysroot it needs, following
/// each copied DLL's own imports in turn, plus `extra` when the sysroot has
/// them. Returns the names copied, in the order found. A DLL the sysroot does
/// not have is Windows' own (`KERNEL32.dll`) and is left alone; a name in
/// `extra` the sysroot does not have is skipped, not an error — it was a
/// courtesy for one distribution's packaging.
fn copy_windows_dlls(arch: Arch, image: &Path, extra: &[String]) -> Result<Vec<String>, String> {
    let dirs = mingw_dll_dirs(arch);
    // Windows resolves DLL names without regard to case; the sysroot spells
    // them one way and an import table may spell them another.
    let mut available: HashMap<String, PathBuf> = HashMap::new();
    for d in &dirs {
        if let Ok(rd) = std::fs::read_dir(d) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if name.to_ascii_lowercase().ends_with(".dll") {
                    available.entry(name.to_ascii_lowercase()).or_insert(e.path());
                }
            }
        }
    }
    let dest_dir = image.parent().unwrap_or(Path::new("."));
    let mut copied: Vec<String> = Vec::new();
    let mut queue: Vec<PathBuf> = vec![image.to_path_buf()];
    let mut wanted: Vec<String> = extra.to_vec();
    while let Some(next) = queue.pop() {
        wanted.extend(pe_imports(arch, &next)?);
        while let Some(name) = wanted.pop() {
            let key = name.to_ascii_lowercase();
            if copied.iter().any(|c| c.to_ascii_lowercase() == key) {
                continue;
            }
            let Some(src) = available.get(&key) else { continue };
            let real = src
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or(name);
            let dest = dest_dir.join(&real);
            std::fs::copy(src, &dest)
                .map_err(|e| format!("cannot copy {} to {}: {e}", src.display(), dest.display()))?;
            copied.push(real);
            queue.push(dest);
        }
    }
    Ok(copied)
}

/// The import library a Windows DLL is linked through: `greet.dll` → `greet.lib`.
fn implib_path(dll: &Path) -> PathBuf {
    dll.with_extension("lib")
}

/// The compile-time half of the Windows release profile: the same three the
/// host gets, minus `-fPIE`, which has no meaning for a PE.
///
/// Probed compile-only, against mingw's clang target, because the compile is
/// the only stage this driver runs here.
fn mingw_release_cflags(driver_args: &[String]) -> Vec<String> {
    let want = vec![
        req(&[&["-O2"]]),
        req(&[&["-U_FORTIFY_SOURCE", "-D_FORTIFY_SOURCE=2"]]),
        req(&[&["-fstack-protector-strong"]]),
    ];
    let dir = mingw_probe_dir();
    let Some(src) = mingw_probe_src(&dir) else { return Vec::new() };
    let obj = dir.join("probe.o");
    let taken = probe_each("clang", &want, |taken, alt| {
        Command::new("clang")
            .args(driver_args)
            .args(taken)
            .args(alt)
            .arg("-Werror")
            .arg("-c")
            .arg(&src)
            .arg("-o")
            .arg(&obj)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    });
    let _ = std::fs::remove_dir_all(&dir);
    taken
}

/// The link-time half for Windows. RELRO and `-z now` are ELF dynamic-loader
/// facts with no PE counterpart, so they are not offered — a flag dropped
/// with a warning on every build is noise, not honesty. What PE has instead:
/// `--dynamicbase` (ASLR, the role PIE plays on Linux), `--nxcompat` (DEP),
/// `--high-entropy-va` (64-bit ASLR), and the strip.
///
/// Probed by linking an object compiled with the cflags already taken, so a
/// stack protector whose support library is missing shows up here, at the
/// link, the way it would in the real build.
fn mingw_release_ldflags(arch: Arch, driver_args: &[String], cflags: &[String]) -> Vec<String> {
    let want: Vec<Requirement> = vec![
        req(&[&["-Wl,--dynamicbase"]]),
        req(&[&["-Wl,--nxcompat"]]),
        req(&[&["-Wl,--high-entropy-va"]]),
        req(&[&["-Wl,-s"]]),
    ];
    let dir = mingw_probe_dir();
    let Some(src) = mingw_probe_src(&dir) else { return Vec::new() };
    let obj = dir.join("probe.o");
    let compiled = Command::new("clang")
        .args(driver_args)
        .args(cflags)
        .arg("-c")
        .arg(&src)
        .arg("-o")
        .arg(&obj)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !compiled {
        eprintln!("kiln: cannot compile a probe program — linking the release unhardened");
        let _ = std::fs::remove_dir_all(&dir);
        return Vec::new();
    }
    let out = dir.join("probe.exe");
    let tc = mingw(arch);
    let taken = probe_each(tc.gcc, &want, |taken, alt| {
        Command::new(tc.gcc)
            .args(taken)
            .args(alt)
            .arg("-Wl,--fatal-warnings")
            .arg(&obj)
            .arg("-o")
            .arg(&out)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    });
    let _ = std::fs::remove_dir_all(&dir);
    taken
}

fn mingw_probe_dir() -> PathBuf {
    std::env::temp_dir().join(format!("kiln_probe_win_{}", std::process::id()))
}

/// The probe source, or nothing — with the same words the host probe uses —
/// when the scratch directory cannot be written.
fn mingw_probe_src(dir: &Path) -> Option<PathBuf> {
    let src = dir.join("probe.c");
    if std::fs::create_dir_all(dir).is_err()
        || std::fs::write(&src, "int main(void){return 0;}\n").is_err()
    {
        eprintln!("kiln: cannot write a probe program — building the release unhardened");
        return None;
    }
    Some(src)
}

/// One hardening requirement, as the argument lists that would satisfy it,
/// best first. `-pie` and `-Wl,-pie` ask the same thing of the driver and of
/// the linker, and which of them works is a property of the local install:
/// with GNU ld, `-Wl,-pie` links the non-PIE start files and fails.
type Requirement = Vec<Vec<String>>;

fn req(alternatives: &[&[&str]]) -> Requirement {
    alternatives
        .iter()
        .map(|alt| alt.iter().map(|s| s.to_string()).collect())
        .collect()
}

/// The compile-time half of the release profile, in the order it must be
/// probed: `_FORTIFY_SOURCE` is a no-op that warns unless optimisation is
/// already on, so `-O2` has to be accepted before it is offered.
fn release_cflags(driver: &str, executable: bool) -> Vec<String> {
    let mut want = vec![
        req(&[&["-O2"]]),
        // The distribution may have fortified the compiler already, and
        // redefining the macro is a warning — which the probe reads as a no.
        req(&[&["-U_FORTIFY_SOURCE", "-D_FORTIFY_SOURCE=2"]]),
        req(&[&["-fstack-protector-strong"]]),
    ];
    // Position independence for a program only: libraries are compiled -fPIC
    // already, and -fPIE would contradict it.
    if executable {
        want.push(req(&[&["-fPIE"]]));
    }
    probe(driver, &[], &want, &[])
}

/// The link-time half, probed on top of the compile flags that were accepted —
/// `-pie` is only meaningful over objects compiled `-fPIE`.
///
/// `-Wl,-s` is the strip: done in the link it needs no second tool and cannot
/// leave a half-stripped file behind when it fails.
fn release_ldflags(driver: &str, cflags: &[String], executable: bool) -> Vec<String> {
    let mut want: Vec<Requirement> = Vec::new();
    if executable {
        want.push(req(&[&["-pie"], &["-Wl,-pie"]]));
    }
    want.push(req(&[&["-Wl,-z,relro"]]));
    want.push(req(&[&["-Wl,-z,now"]]));
    want.push(req(&[&["-Wl,-s"]]));
    // A linker answers an option it does not know with a warning and carries
    // on, which would leave us believing in hardening that is not there.
    probe(driver, cflags, &want, &["-Wl,--fatal-warnings".to_string()])
}

/// Ask the local toolchain which of `want` it actually accepts, by building a
/// trivial program with each requirement in turn on top of the ones already
/// accepted.
///
/// A flag this compiler rejects is dropped and said out loud. Passing it
/// regardless would be worse than leaving it out: the build still succeeds and
/// the binary is not hardened, which is the failure nobody notices.
fn probe(driver: &str, base: &[String], want: &[Requirement], extra: &[String]) -> Vec<String> {
    let dir = std::env::temp_dir().join(format!("kiln_probe_{}", std::process::id()));
    // The extension picks the language: clang++ handed a .c file treats it as
    // C++ and says so as a deprecation warning, which -Werror turns into a
    // rejection of every flag we ask about.
    let src = dir.join(if driver.ends_with("++") {
        "probe.cpp"
    } else {
        "probe.c"
    });
    if std::fs::create_dir_all(&dir).is_err()
        || std::fs::write(&src, "int main(void){return 0;}\n").is_err()
    {
        eprintln!("kiln: cannot write a probe program — building the release unhardened");
        return Vec::new();
    }
    let out = dir.join("probe");

    let taken = probe_each(driver, want, |taken, alt| {
        Command::new(driver)
            .args(base)
            .args(taken)
            .args(alt)
            .args(extra)
            .arg("-Werror")
            .arg(&src)
            .arg("-o")
            .arg(&out)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    });
    let _ = std::fs::remove_dir_all(&dir);
    taken
}

/// The probing itself, over whatever "try these flags" means for the tool at
/// hand: one command on the host, two — a compile and a separate link — for
/// the cross toolchain. `accept` is handed the flags taken so far and the
/// alternative on offer, and says whether the tool took them together.
fn probe_each(
    driver: &str,
    want: &[Requirement],
    accept: impl Fn(&[String], &[String]) -> bool,
) -> Vec<String> {
    let mut taken: Vec<String> = Vec::new();
    for alternatives in want {
        let accepted = alternatives.iter().find(|alt| accept(&taken, alt));
        match accepted {
            Some(alt) => taken.extend(alt.iter().cloned()),
            None => eprintln!(
                "kiln: {driver} does not accept {} — building the release without it",
                alternatives[0].join(" ")
            ),
        }
    }
    taken
}

/// Compile each input to its own object in `dir`, in order.
///
/// `driver_args` come before everything else: the cross target flag, when
/// there is one, has to reach every compile the same way.
fn compile_objects(
    driver: &str,
    driver_args: &[String],
    common: &[String],
    inputs: &[(PathBuf, Option<&'static str>)],
    dir: &Path,
) -> Result<Vec<PathBuf>, i32> {
    let mut objects: Vec<PathBuf> = Vec::new();
    for (i, (path, lang)) in inputs.iter().enumerate() {
        let obj = dir.join(format!("{i}.o"));
        let mut cmd = Command::new(driver);
        cmd.args(driver_args).args(common).arg("-c");
        if let Some(l) = lang {
            cmd.arg("-x").arg(l);
        }
        cmd.arg(path).arg("-o").arg(&obj);
        match cmd.status() {
            Ok(s) if s.success() => objects.push(obj),
            Ok(s) => {
                eprintln!("kiln: clang failed with status {s} on {}", path.display());
                return Err(1);
            }
            Err(e) => {
                eprintln!("kiln: {}", crate::libload::spawn_error(driver, &e));
                return Err(1);
            }
        }
    }
    Ok(objects)
}

/// Compile each input to its own object and archive them.
///
/// `clang -c` refuses a single `-o` for several inputs, so an archive has to be
/// built one object at a time rather than in one command like a link.
fn build_archive(
    driver: &str,
    driver_args: &[String],
    ar_tool: &str,
    common: &[String],
    inputs: &[(PathBuf, Option<&'static str>)],
    out_lib: &Path,
) -> Result<(), i32> {
    let dir = std::env::temp_dir().join(format!("kiln_ar_{}", std::process::id()));
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("kiln: cannot create {}: {e}", dir.display());
        return Err(1);
    }

    let objects = match compile_objects(driver, driver_args, common, inputs, &dir) {
        Ok(o) => o,
        Err(code) => {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(code);
        }
    };

    // `ar rcs` replaces rather than appends, so a stale archive of the same name
    // cannot leave old objects behind.
    let _ = std::fs::remove_file(out_lib);
    let mut ar = Command::new(ar_tool);
    ar.arg("rcs").arg(out_lib).args(&objects);
    let status = ar.status();
    let _ = std::fs::remove_dir_all(&dir);
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => {
            eprintln!("kiln: ar failed with status {s}");
            Err(1)
        }
        Err(e) => {
            eprintln!("kiln: could not invoke {ar_tool}: {e}");
            Err(1)
        }
    }
}

/// Find the runtime directory: `$KILN_RUNTIME_DIR`, else walk up from cwd
/// looking for `runtime/kiln_core.h`.
fn find_runtime_dir() -> Option<PathBuf> {
    if let Ok(d) = std::env::var("KILN_RUNTIME_DIR") {
        let p = PathBuf::from(d);
        if p.join("kiln_core.h").is_file() {
            return Some(p);
        }
    }
    // Walking up from the working directory covers running inside the repo.
    if let Some(found) = std::env::current_dir().ok().and_then(walk_up_for_runtime) {
        return Some(found);
    }
    // …and walking up from the executable covers everything else: `kiln new`
    // is run from wherever the user's project will live, and the templates and
    // runtime are next to the binary, not next to them.
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .and_then(walk_up_for_runtime)
}

fn walk_up_for_runtime(start: PathBuf) -> Option<PathBuf> {
    let mut dir = start;
    loop {
        let cand = dir.join("runtime");
        if cand.join("kiln_core.h").is_file() {
            return Some(cand);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// The repository root (parent of the located `runtime/` directory).
fn find_repo_root() -> Option<PathBuf> {
    find_runtime_dir().and_then(|d| d.parent().map(|p| p.to_path_buf()))
}

/// Make a bare relative path executable-invokable (`./name`).
fn absolutize(p: &Path) -> PathBuf {
    if p.components().count() == 1 {
        let mut pb = PathBuf::from(".");
        pb.push(p);
        pb
    } else {
        p.to_path_buf()
    }
}
