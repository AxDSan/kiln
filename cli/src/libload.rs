//! Build-time support-library introspection.
//!
//! For the implicit `core` library and each `use <name>`, the loader:
//!   1. builds an introspection shared object from the library's sources
//!      (impls + the `*_libinfo.c` metadata TU, minus the program entry),
//!   2. `dlopen`s it and calls `kiln_get_lib_info` to read the `LibInfo`
//!      metadata, populating a `Registry` (the authoritative signatures — no
//!      hard-coded Rust table), and
//!   3. records the library's *implementation* sources (everything except the
//!      `*_libinfo.c` metadata TU) for static-linking into the program.
//!
//! The metadata TU never enters the program link line, preserving `--gc-sections`
//! dead-stripping.
//!
//! `dlopen` runs the target library in-process, so this native path assumes
//! host == target (x86_64-linux). Cross-compilation needs a sidecar manifest
//! instead (Phase 4).

use std::collections::HashMap;
use std::ffi::{c_char, c_int, CStr, CString};
#[cfg(unix)]
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::process::Command;

use kiln_ir::registry::{CommandDoc, ComponentDesc, ComponentKind, DllSig, PropertyDesc};
use kiln_ir::validate::validate_decls;
use kiln_ir::{parse, Item, Module, Registry, Signature, Target, Ty};

#[cfg(unix)]
extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlclose(handle: *mut c_void) -> c_int;
    fn dlerror() -> *mut c_char;
}

// The same four calls over the Win32 loader, so the rest of this file reads
// one way. `dlerror` renders GetLastError as a number: the message text would
// need FormatMessage and a code is what a search engine wants anyway.
#[cfg(windows)]
mod win {
    use std::ffi::{c_char, c_int, c_void};
    #[link(name = "kernel32")]
    extern "system" {
        fn LoadLibraryA(name: *const c_char) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
        fn FreeLibrary(module: *mut c_void) -> c_int;
        fn GetLastError() -> u32;
    }
    static mut LAST: [u8; 32] = [0; 32];
    pub unsafe fn dlopen(filename: *const c_char, _flag: c_int) -> *mut c_void {
        LoadLibraryA(filename)
    }
    pub unsafe fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void {
        GetProcAddress(handle, symbol)
    }
    pub unsafe fn dlclose(handle: *mut c_void) -> c_int {
        FreeLibrary(handle)
    }
    pub unsafe fn dlerror() -> *mut c_char {
        let msg = format!("Win32 error {}\0", GetLastError());
        let n = msg.len().min(31);
        let buf = &mut *std::ptr::addr_of_mut!(LAST);
        buf[..n].copy_from_slice(&msg.as_bytes()[..n]);
        buf[n] = 0;
        std::ptr::addr_of_mut!(LAST) as *mut c_char
    }
}
#[cfg(windows)]
use win::{dlclose, dlerror, dlopen, dlsym};
const RTLD_LAZY: c_int = 1;

#[repr(C)]
struct CommandDescC {
    name: *const c_char,
    symbol: *const c_char,
    ret_tag: i32,
    argc: i32,
    arg_tags: *const i32,
    /// Both NULL for a command that has not been documented yet — which is
    /// most of them, and is why every consumer treats an absent doc as
    /// "signature only" rather than an error.
    doc: *const c_char,
    example: *const c_char,
}

#[repr(C)]
struct PropertyDescC {
    name: *const c_char,
    tag: i32,
    default_value: *const c_char,
    editor: *const c_char,
}

#[repr(C)]
struct EventDescC {
    name: *const c_char,
    // Appended in ABI v3, and appended is what makes them safe to read: a
    // descriptor written as `{ "click" }` zero-fills to a count of 0 and a null
    // table, which is the same "hands its handler nothing" every v2 event meant.
    param_count: i32,
    param_tags: *const i32,
}

#[repr(C)]
struct ComponentDescC {
    name: *const c_char,
    a11y_role: i32,
    property_count: i32,
    properties: *const PropertyDescC,
    event_count: i32,
    events: *const EventDescC,
    kind: i32,
}

#[repr(C)]
struct LibInfoC {
    abi_version: i32,
    name: *const c_char,
    guid: *const c_char,
    ver_major: i32,
    ver_minor: i32,
    ver_build: i32,
    command_count: i32,
    commands: *const CommandDescC,
    component_count: i32,
    components: *const ComponentDescC,
}

const KILN_ABI_VERSION: i32 = 4;

/// The result of resolving a module's libraries.
pub struct LibPlan {
    /// Combined command + component registry across all loaded libraries.
    pub registry: Registry,
    /// Implementation sources to static-link into the program.
    pub impl_sources: Vec<PathBuf>,
    /// Extra compiler/linker configuration contributed by libraries that need
    /// it (the UI stack, for example). Only populated for libraries actually
    /// used, so a console program never links the UI.
    pub build: BuildConfig,
    /// Kits that restrict themselves to a set of operating systems, as
    /// `(kit name, platforms)` — from a declaration kit's `lib.json`
    /// `"platforms"`. Empty for a portable kit. The *build* consults this to
    /// refuse a windows-only kit on a Linux target; listing (`kiln
    /// commands`, the language server) does not, so a Win32 kit still documents
    /// and completes on a machine that cannot build for Windows.
    pub gated: Vec<(String, Vec<String>)>,
}

/// Compiler/linker additions a library needs, from its `lib.json`.
#[derive(Debug, Default, Clone)]
pub struct BuildConfig {
    pub include_dirs: Vec<PathBuf>,
    pub extra_sources: Vec<PathBuf>,
    pub defines: Vec<String>,
    pub pkg_config: Vec<String>,
    pub link_args: Vec<String>,
    /// Any library needed C++, so the link step must use clang++.
    pub needs_cxx: bool,
    /// Windows only: DLLs a program must carry beside it that no import table
    /// names — loaded by hand at run time, so a scan of the image cannot find
    /// them (see `Manifest::windows_extra_dlls`).
    pub extra_dlls: Vec<String>,
}

/// Resolve `core` + each `use`d library under `repo_root`.
pub fn load(repo_root: &Path, uses: &[String]) -> Result<LibPlan, String> {
    load_with(repo_root, uses, true, false)
}

/// Resolve for a program that will be linked for ANOTHER operating system.
///
/// Two differences. The optional dependencies: `take_optional` decides by
/// whether the vendored files exist, and the files under `vendor/` were built
/// for this machine. A cross build that found them would hand ELF archives to
/// a PE linker and define the feature macro on top, so here they are treated
/// as absent and the library says at run time that the capability is missing —
/// which is the truth, on that platform, until someone vendors it there too.
/// And the link itself: a library whose `lib.json` carries `windows_*` keys
/// has a Windows build of its dependencies, and those keys replace the Linux
/// ones (`Manifest::take_windows`).
pub fn load_cross(repo_root: &Path, uses: &[String]) -> Result<LibPlan, String> {
    load_with(repo_root, uses, true, true)
}

/// Introspect libraries without requiring what only a *link* needs.
///
/// The metadata translation unit references command implementations by symbol
/// name rather than by pointer, so reading a library's descriptors never needs
/// its vendored dependencies. Asking what commands exist should not require the
/// ability to build a window — which is also what lets the documentation be
/// generated on a machine that has never fetched the UI stack.
pub fn load_metadata(repo_root: &Path, uses: &[String]) -> Result<LibPlan, String> {
    load_with(repo_root, uses, false, false)
}

fn load_with(
    repo_root: &Path,
    uses: &[String],
    require_impl: bool,
    cross: bool,
) -> Result<LibPlan, String> {
    let mut registry = Registry::new();
    let mut impl_sources = Vec::new();
    let mut build = BuildConfig::default();
    let mut gated: Vec<(String, Vec<String>)> = Vec::new();

    // (library name, directory) — core is implicit and first.
    let mut libs: Vec<(String, PathBuf)> = vec![("core".into(), repo_root.join("runtime"))];
    for u in uses {
        libs.push((u.clone(), repo_root.join("libs").join(u)));
    }

    for (name, dir) in &libs {
        if !dir.is_dir() {
            return Err(format!(
                "library `{name}`: directory not found: {}",
                dir.display()
            ));
        }
        let manifest = Manifest::load(dir, repo_root, !cross)?;
        if require_impl {
            manifest
                .check_requirements()
                .map_err(|e| format!("library `{name}`: {e}"))?;
        }

        let mut sources = c_sources(dir)?;
        sources.extend(manifest.extra_sources.iter().cloned());

        // Introspection .so: the metadata TU alone. It references command
        // implementations by symbol NAME, not by pointer, so it has no
        // dependency on them — which keeps this .so tiny, quick to build, and
        // free of the implementation's link requirements (a static RmlUi, for
        // instance, is not position-independent and could not go in a .so).
        let so_srcs: Vec<PathBuf> = sources
            .iter()
            .filter(|p| filename(p).ends_with("_libinfo.c"))
            .cloned()
            .collect();

        // A kit's declaration bundle: pure `dll`, `record` and `const` lines the
        // parser reads, merged into the registry as if the program had typed
        // them. A kit may ship one, C-implemented commands, or both.
        let decls = read_decls(dir, name)?;

        // A kit must carry SOMETHING the compiler can see: a metadata TU with
        // commands, or a declaration bundle. Neither, and there is nothing to
        // use — the same "not a kit" the resolver already refuses to list.
        if so_srcs.is_empty() && decls.is_none() {
            return Err(format!(
                "library `{name}`: no `{name}_libinfo.c` metadata source and no \
                 `*.kdecl` declaration bundle found in {}",
                dir.display()
            ));
        }

        // The declaration bundle. Validated on its own (as a sharedlib, so the
        // entry-point rule does not fire) against the registry so far, then its
        // dlls, records and constants are merged in. A collision here is a
        // kit-authoring fault, named with the kit.
        if let Some(mut decl_mod) = decls {
            decl_mod.target = Some(Target::SharedLib);
            // Validate the bundle in isolation against the registry so far, so
            // a malformed record or a `dll` taking a non-`is c` record fails
            // with the kit's own name rather than surfacing later in a program.
            if let Err(errs) = validate_decls(&decl_mod, &registry) {
                let joined = errs
                    .iter()
                    .map(|e| format!("    - {e}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                return Err(format!(
                    "kit `{name}`: its declaration bundle (`*.kdecl`) is invalid:\n{joined}"
                ));
            }
            for rec in decl_mod.records() {
                if !registry.insert_record(rec.clone()) {
                    return Err(format!(
                        "kit `{name}`: record `{}` collides with an already-registered name",
                        rec.name
                    ));
                }
            }
            for d in decl_mod.dlls() {
                let sig = DllSig {
                    sig: d.signature(),
                    library: d.library.clone(),
                    symbol: d.symbol_name().to_string(),
                    conv: d.conv,
                };
                if !registry.insert_dll(d.name.clone(), sig) {
                    return Err(format!(
                        "kit `{name}`: `dll {}` collides with an already-registered name",
                        d.name
                    ));
                }
            }
            for c in decl_mod.consts() {
                if !registry.insert_const(c.clone()) {
                    return Err(format!(
                        "kit `{name}`: constant `{}` collides with an already-registered name",
                        c.name
                    ));
                }
            }
        }

        // A kit that declares `"platforms"` is usable only on those; the build
        // checks it against the target OS. Recorded for every load path, acted
        // on only by the build — listing a kit's contents is allowed anywhere.
        if !manifest.platforms.is_empty() && name != "core" {
            gated.push((name.clone(), manifest.platforms.clone()));
        }

        // Impl sources and the introspection `.so` belong to the C-implemented
        // half of a kit, marked by a `_libinfo.c`. A declaration-only kit has
        // none: its `.c`, if any, is a runtime library a `dll` loads by name at
        // run time, not something static-linked into the program — harvesting
        // it here would put the symbols in the image AND have the loader open a
        // second copy.
        if !so_srcs.is_empty() {
            for p in &sources {
                if !filename(p).ends_with("_libinfo.c") {
                    impl_sources.push(p.clone());
                }
            }
            build.merge(&manifest);
            let so_path = build_introspection_so(repo_root, name, &so_srcs, &manifest)?;
            introspect_into(&so_path, name, &mut registry)?;
        }
    }

    Ok(LibPlan {
        registry,
        impl_sources,
        build,
        gated,
    })
}

/// Read a kit's declaration bundle — every `*.kdecl` in the kit directory,
/// merged — if it has one.
///
/// This is the ONE reader of an `.kdecl`: the loader merges what it returns into
/// the registry, and `kiln kits` lists it — two readers of one file is the
/// drift the CLI keeps a single parser for `.kiln` to avoid. A file is a module
/// body without the header: a run of `dll`, `record` and `const` declarations
/// and nothing else. It is parsed by prepending a synthetic `module <name>`
/// line and reusing the whole language parser, so a declaration in a kit reads
/// exactly as one in a program; a `sub`, a `form`, a component, a module
/// variable, a `target` or a `use` is refused, because a declaration bundle
/// declares, it does not define or build.
///
/// A kit may spread its declarations over as many `.kdecl` files as it likes —
/// `user32.kdecl`, `kernel32.kdecl`, `gdi32.kdecl` — and they are merged into one
/// bundle. Order across files does not matter: the merged bundle is validated
/// and registered as a unit, so a record declared in one file and used by a
/// `dll` in another resolves exactly as it would within one file. Two files
/// declaring the same name is a kit-authoring fault, reported with both.
pub fn read_decls(dir: &Path, name: &str) -> Result<Option<Module>, String> {
    let mut files: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().and_then(|s| s.to_str()) == Some("kdecl") && p.is_file() {
                files.push(p);
            }
        }
    }
    if files.is_empty() {
        return Ok(None);
    }
    // Sorted so a kit reads the same way twice, and so a collision names the
    // same two files whichever machine reports it.
    files.sort();

    let mut merged = Module {
        name: name.to_string(),
        target: None,
        uses: Vec::new(),
        items: Vec::new(),
    };
    // Every declared name, and the file that declared it. `dll`, `record` and
    // `const` share one namespace here because the registry gives them one:
    // two of them with one name is the same collision either way.
    let mut declared: HashMap<String, String> = HashMap::new();

    for path in &files {
        let body = std::fs::read_to_string(path)
            .map_err(|e| format!("read {}: {e}", path.display()))?;
        // The header is one prepended line, so a parse error's line number is
        // one past the `.kdecl`'s own — subtract it back before reporting.
        let src = format!("module {name}\n{body}");
        let module = parse(&src).map_err(|e| {
            format!(
                "{}:{}: {}",
                path.display(),
                e.line.saturating_sub(1).max(1),
                e.msg
            )
        })?;
        if !module.uses.is_empty() {
            return Err(format!(
                "{}: a declaration bundle may not `use` another kit",
                path.display()
            ));
        }
        let here = filename(path);
        for item in module.items {
            let declared_name = match &item {
                Item::Dll(d) => d.name.clone(),
                Item::UserType(r) => r.name.clone(),
                Item::Const(c) => c.name.clone(),
                Item::Sub(_) => return Err(bundle_holds_only(path, "a subroutine")),
                Item::Form(_) => return Err(bundle_holds_only(path, "a form")),
                Item::Component(_) => return Err(bundle_holds_only(path, "a component")),
                Item::Var(_) => return Err(bundle_holds_only(path, "a module variable")),
            };
            if let Some(first) = declared.get(&declared_name) {
                return Err(format!(
                    "kit `{name}`: `{declared_name}` is declared in both {first} and {here} — \
                     a kit's declarations share one namespace, so each name belongs to one file",
                ));
            }
            declared.insert(declared_name, here.clone());
            merged.items.push(item);
        }
    }
    Ok(Some(merged))
}

fn bundle_holds_only(path: &Path, what: &str) -> String {
    format!(
        "{}: a declaration bundle holds only `dll`, `record` and `const` — {what} \
         does not belong in one",
        path.display()
    )
}

fn filename(p: &Path) -> String {
    p.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string()
}

/// What to say when a toolchain program could not be started.
///
/// `NotFound` is the whole diagnosis, and on its own it prints as "program not
/// found" — which names neither the program that is missing nor anything to do
/// about it. Studio shows this same string in its PROBLEMS pane, where it was
/// the first thing a new user on Windows saw, so this is the one place either
/// of them gets to explain itself.
pub fn spawn_error(prog: &str, e: &std::io::Error) -> String {
    if e.kind() != std::io::ErrorKind::NotFound {
        return format!("invoke {prog}: {e}");
    }
    let install = if prog.contains("mingw32") {
        "install mingw-w64 and put its bin directory on PATH"
    } else {
        "install LLVM (which provides clang) and put its bin directory on PATH"
    };
    let guide = if cfg!(windows) {
        " — see SETUP-WINDOWS.md at the root of this bundle, beside README.md"
    } else {
        ""
    };
    format!("{prog} is not on PATH — {install}{guide}")
}

/// All C/C++ sources directly in `dir`, sorted for determinism.
fn c_sources(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| format!("read {}: {e}", dir.display()))? {
        let p = entry.map_err(|e| e.to_string())?.path();
        if matches!(
            p.extension().and_then(|s| s.to_str()),
            Some("c") | Some("cpp") | Some("cc") | Some("cxx")
        ) {
            out.push(p);
        }
    }
    out.sort();
    Ok(out)
}

/// Build `lib<name>.so` for introspection; returns its path.
fn build_introspection_so(
    repo_root: &Path,
    name: &str,
    sources: &[PathBuf],
    manifest: &Manifest,
) -> Result<PathBuf, String> {
    let build_dir = std::env::temp_dir().join("kiln-build");
    std::fs::create_dir_all(&build_dir).map_err(|e| e.to_string())?;
    let so_path = build_dir.join(format!("lib{name}_introspect_{}.so", std::process::id()));

    // Always plain C: the metadata TU has no implementation dependencies.
    let mut cmd = Command::new("clang");
    cmd.arg("-shared")
        .arg("-fPIC")
        .arg("-I")
        .arg(repo_root.join("abi"))
        .arg("-I")
        .arg(repo_root.join("runtime")); // for kiln_core.h
    for d in &manifest.include_dirs {
        cmd.arg("-I").arg(d);
    }
    for d in &manifest.defines {
        cmd.arg(format!("-D{d}"));
    }
    for s in sources {
        cmd.arg(s);
    }
    cmd.arg("-o").arg(&so_path);

    let status = cmd.status().map_err(|e| spawn_error("clang", &e))?;
    if !status.success() {
        return Err(format!("building introspection library `{name}` failed"));
    }
    Ok(so_path)
}

/// dlopen the `.so`, read its `LibInfo`, and register its commands.
fn introspect_into(so_path: &Path, name: &str, registry: &mut Registry) -> Result<(), String> {
    // SAFETY: we read a C `LibInfo` of the frozen ABI layout from a library we
    // just built; all pointers are copied into owned Rust values before dlclose.
    unsafe {
        let cpath = CString::new(so_path.to_string_lossy().as_bytes()).unwrap();
        let handle = dlopen(cpath.as_ptr(), RTLD_LAZY);
        if handle.is_null() {
            let e = CStr::from_ptr(dlerror()).to_string_lossy().into_owned();
            return Err(format!("dlopen `{name}`: {e}"));
        }
        let sym = CString::new("kiln_get_lib_info").unwrap();
        let getfn = dlsym(handle, sym.as_ptr());
        if getfn.is_null() {
            dlclose(handle);
            return Err(format!(
                "library `{name}` does not export kiln_get_lib_info"
            ));
        }
        let getfn: extern "C" fn() -> *const LibInfoC = std::mem::transmute(getfn);
        let info = getfn();
        if info.is_null() {
            dlclose(handle);
            return Err(format!("library `{name}` returned a null LibInfo"));
        }
        let info = &*info;
        // Copied before dlclose: the format argument would otherwise be read
        // back out of a library that is no longer mapped, so the one path that
        // exists to report a bad ABI would segfault instead of reporting it.
        let found = info.abi_version;
        if found != KILN_ABI_VERSION {
            dlclose(handle);
            return Err(format!(
                "library `{name}` ABI version {found} != {KILN_ABI_VERSION}"
            ));
        }

        let result = register_commands(info, name, registry);
        dlclose(handle);
        result
    }
}

/// An optional C string field. NULL is the descriptor's way of saying "none",
/// which is the empty string on the Rust side rather than an `Option` nobody
/// would ever match on.
///
/// SAFETY: `p` is NULL or a NUL-terminated string owned by the loaded library.
unsafe fn cstr_or_empty(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}

/// SAFETY: `info` is a valid `LibInfoC`; called only from `introspect_into`.
unsafe fn register_commands(
    info: &LibInfoC,
    lib: &str,
    registry: &mut Registry,
) -> Result<(), String> {
    let count = info.command_count.max(0) as isize;
    for i in 0..count {
        let desc = &*info.commands.offset(i);
        let cmd_name = CStr::from_ptr(desc.name).to_string_lossy().into_owned();
        let symbol = CStr::from_ptr(desc.symbol).to_string_lossy().into_owned();

        let ret = if desc.ret_tag == 0 {
            None
        } else {
            Some(Ty::from_sdt_tag(desc.ret_tag).ok_or_else(|| {
                format!(
                    "command `{cmd_name}` in `{lib}`: unsupported return tag {}",
                    desc.ret_tag
                )
            })?)
        };

        let argc = desc.argc.max(0) as isize;
        let mut params = Vec::new();
        for a in 0..argc {
            let tag = *desc.arg_tags.offset(a);
            params.push(Ty::from_sdt_tag(tag).ok_or_else(|| {
                format!("command `{cmd_name}` in `{lib}`: unsupported arg tag {tag}")
            })?);
        }

        if !registry.insert(cmd_name.clone(), Signature::simple(params, ret), symbol) {
            return Err(format!(
                "command `{cmd_name}` (from `{lib}`) collides with an already-registered command"
            ));
        }

        // Documentation, when the library carries any. Both pointers are NULL
        // for a row written before the fields existed, which is most of them.
        let text = |p: *const c_char| -> String {
            if p.is_null() {
                String::new()
            } else {
                CStr::from_ptr(p).to_string_lossy().into_owned()
            }
        };
        registry.set_doc(
            cmd_name,
            CommandDoc {
                summary: text(desc.doc),
                example: text(desc.example),
            },
        );
    }

    // Visual components come through the same LibInfo mechanism.
    let ccount = info.component_count.max(0) as isize;
    for i in 0..ccount {
        let cd = &*info.components.offset(i);
        let name = CStr::from_ptr(cd.name).to_string_lossy().into_owned();

        let mut properties = Vec::new();
        for pi in 0..cd.property_count.max(0) as isize {
            let pd = &*cd.properties.offset(pi);
            let pname = CStr::from_ptr(pd.name).to_string_lossy().into_owned();
            let ty = Ty::from_sdt_tag(pd.tag).ok_or_else(|| {
                format!(
                    "component `{name}` in `{lib}`: property `{pname}` has unsupported tag {}",
                    pd.tag
                )
            })?;
            properties.push(PropertyDesc {
                name: pname,
                ty,
                editor: cstr_or_empty(pd.editor),
            });
        }

        let mut events = Vec::new();
        // The parameters cannot be recorded yet: they are keyed by component
        // name, and the component is not in the registry until below.
        let mut event_params: Vec<(String, Vec<Ty>)> = Vec::new();
        for ei in 0..cd.event_count.max(0) as isize {
            let ed = &*cd.events.offset(ei);
            let ename = CStr::from_ptr(ed.name).to_string_lossy().into_owned();
            let mut params = Vec::new();
            for pi in 0..ed.param_count.max(0) as isize {
                let tag = *ed.param_tags.offset(pi);
                params.push(Ty::from_sdt_tag(tag).ok_or_else(|| {
                    format!(
                        "component `{name}` in `{lib}`: event `{ename}` has unsupported parameter tag {tag}"
                    )
                })?);
            }
            if !params.is_empty() {
                event_params.push((ename.clone(), params));
            }
            events.push(ename);
        }

        if !registry.insert_component(ComponentDesc {
            name: name.clone(),
            a11y_role: cd.a11y_role,
            kind: if cd.kind == 1 {
                ComponentKind::NonVisual
            } else {
                ComponentKind::Visual
            },
            library: lib.to_string(),
            properties,
            events,
        }) {
            return Err(format!(
                "component `{name}` (from `{lib}`) collides with an already-registered component"
            ));
        }
        for (event, params) in event_params {
            registry.set_event_params(&name, &event, params);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf()
    }

    /// The dlopen'd `core` LibInfo must match the hard-coded `Registry::core()`
    /// used by the ir/backend unit tests — so the fast in-memory table can never
    /// silently drift from the C source of truth.
    #[test]
    fn core_libinfo_matches_hardcoded_registry() {
        let plan = load(&repo_root(), &[]).expect("introspect core");
        let hard = Registry::core();
        assert_eq!(
            plan.registry.len(),
            hard.len(),
            "core command count drifted"
        );
        for (name, cmd) in hard.iter() {
            let loaded = plan
                .registry
                .get(name)
                .unwrap_or_else(|| panic!("core LibInfo missing command `{name}`"));
            assert_eq!(
                loaded, cmd,
                "command `{name}` signature/symbol drifted from LibInfo"
            );
        }
        // Components are deliberately absent from the hard-coded table: they
        // reach the compiler only through this dlopen path, so mirroring them
        // in Rust would create a second source of truth that nothing reads.
        // Core's own `timer` is covered end-to-end in `cli/tests/build.rs`.
        assert_eq!(
            hard.component_names().count(),
            0,
            "the hard-coded table must not grow components"
        );
    }
}

/// A library's `lib.json`: the extra build configuration it needs.
///
/// Parsed with a small purpose-built reader rather than pulling in a JSON crate
/// — the schema is fixed and tiny (flat string arrays plus one bool), and the
/// compiler has no other runtime dependencies.
#[derive(Debug, Default, Clone)]
pub struct Manifest {
    pub cxx: bool,
    pub include_dirs: Vec<PathBuf>,
    pub extra_sources: Vec<PathBuf>,
    pub defines: Vec<String>,
    pub pkg_config: Vec<String>,
    pub link_args: Vec<String>,
    pub requires: Vec<PathBuf>,
    pub requires_hint: String,
    /// Windows only: DLL names to ship beside a program in addition to what
    /// its import table says — for a dependency that `LoadLibrary`s another
    /// by hand (sdl2-compat's `SDL2.dll` loads `SDL3.dll` that way). Copied
    /// from the mingw sysroot when they are there, and skipped when not.
    pub extra_dlls: Vec<String>,
    /// The paths that decide whether the optional dependency is here.
    pub optional_requires: Vec<PathBuf>,
    /// Every `optional_requires` path exists, so the `optional_*` build
    /// configuration has been folded into the fields above and the feature
    /// macro is defined.
    pub optional_enabled: bool,
    /// The operating systems this kit may be used on (`"platforms"` in
    /// `lib.json`), lower-cased. Empty means portable — usable everywhere. A
    /// declaration kit that wraps a platform's own API (Win32) sets this so a
    /// build for another OS fails with a message rather than a link error.
    pub platforms: Vec<String>,
}

impl Manifest {
    /// Load `<dir>/lib.json` if present; an absent manifest is not an error
    /// (plain C libraries like `core` need no configuration).
    ///
    /// `host_deps` is false for a cross build, whose vendored dependencies
    /// are the wrong architecture by definition; see `load_cross`.
    fn load(dir: &Path, repo_root: &Path, host_deps: bool) -> Result<Manifest, String> {
        let path = dir.join("lib.json");
        if !path.is_file() {
            return Ok(Manifest::default());
        }
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;

        let mut m = Manifest {
            cxx: json_bool(&text, "cxx"),
            defines: json_array(&text, "defines"),
            pkg_config: json_array(&text, "pkg_config"),
            link_args: json_array(&text, "link_args"),
            requires_hint: json_string(&text, "requires_hint"),
            platforms: json_array(&text, "platforms")
                .iter()
                .map(|p| p.to_lowercase())
                .collect(),
            ..Default::default()
        };
        m.include_dirs = json_array(&text, "include_dirs")
            .iter()
            .map(|p| repo_root.join(p))
            .collect();
        m.extra_sources = json_array(&text, "extra_sources")
            .iter()
            .map(|p| repo_root.join(p))
            .collect();
        m.requires = json_array(&text, "requires")
            .iter()
            .map(|p| repo_root.join(p))
            .collect();
        m.link_args = m
            .link_args
            .iter()
            .map(|a| absolutise(repo_root, a))
            .collect();
        if host_deps {
            m.take_optional(&text, repo_root);
        } else {
            m.take_windows(&text, repo_root);
        }
        Ok(m)
    }

    /// A cross build reads the `windows_*` keys in place of the host ones.
    ///
    /// Each key REPLACES its counterpart rather than adding to it: the Linux
    /// `link_args` name `-lGL` and an ELF archive under `vendor/`, and neither
    /// has any business on a PE link line. A library that declares no
    /// `windows_*` key keeps what it had — `net`'s Winsock is named by the
    /// link itself, and a plain C library has nothing to replace. The
    /// `requires` swap is what turns "fetch RmlUi" into "build RmlUi for
    /// Windows" when that is the thing actually missing.
    fn take_windows(&mut self, text: &str, repo_root: &Path) {
        if json_has(text, "windows_pkg_config") {
            self.pkg_config = json_array(text, "windows_pkg_config");
        }
        if json_has(text, "windows_link_args") {
            self.link_args = json_array(text, "windows_link_args")
                .iter()
                .map(|a| absolutise(repo_root, a))
                .collect();
        }
        if json_has(text, "windows_requires") {
            self.requires = json_array(text, "windows_requires")
                .iter()
                .map(|p| repo_root.join(p))
                .collect();
        }
        if json_has(text, "windows_requires_hint") {
            self.requires_hint = json_string(text, "windows_requires_hint");
        }
        self.extra_dlls = json_array(text, "windows_extra_dlls");
    }

    /// Fold in the `optional_*` configuration when the dependency it names is
    /// actually there.
    ///
    /// `requires` is the right shape for a dependency the whole library is: no
    /// RmlUi means no window means no build, and failing with a hint beats
    /// failing with a wall of linker errors. It is the wrong shape for a
    /// dependency that gates ONE capability — declaring mbedTLS in `requires`
    /// would make every build of a program that merely mentions `net` fail on a
    /// machine that has never fetched a TLS stack, for a program that may not
    /// speak https at all.
    ///
    /// So: present, and the library compiles with the include dirs, sources,
    /// defines and link args it needs plus `optional_feature` defined, which is
    /// what the C tests with `#ifdef`. Absent, and none of it is there and the
    /// build proceeds — the capability fails at run time, loudly, in the
    /// library's own words.
    ///
    /// The fold happens here rather than at the link so that every consumer of
    /// a `Manifest` — the introspection `.so`, the program link, an archive —
    /// sees one library configuration and cannot disagree about it.
    fn take_optional(&mut self, text: &str, repo_root: &Path) {
        self.optional_requires = json_array(text, "optional_requires")
            .iter()
            .map(|p| repo_root.join(p))
            .collect();
        // An empty list is not "nothing is missing, so enable it": a library
        // with no optional dependency declared has no optional configuration to
        // fold, and a feature macro defined for a dependency nobody named would
        // compile code against headers that are not there.
        self.optional_enabled =
            !self.optional_requires.is_empty() && self.optional_requires.iter().all(|p| p.exists());
        if !self.optional_enabled {
            return;
        }

        self.include_dirs.extend(
            json_array(text, "optional_include_dirs")
                .iter()
                .map(|p| repo_root.join(p)),
        );
        self.extra_sources.extend(
            json_array(text, "optional_extra_sources")
                .iter()
                .map(|p| repo_root.join(p)),
        );
        self.defines.extend(json_array(text, "optional_defines"));
        self.link_args.extend(
            json_array(text, "optional_link_args")
                .iter()
                .map(|a| absolutise(repo_root, a)),
        );
        let feature = json_string(text, "optional_feature");
        if !feature.is_empty() {
            self.defines.push(feature);
        }
    }

    /// Fail with the library's own hint if a prerequisite (e.g. a vendored
    /// dependency) is missing — a clear message beats a wall of linker errors.
    fn check_requirements(&self) -> Result<(), String> {
        for r in &self.requires {
            if !r.exists() {
                return Err(if self.requires_hint.is_empty() {
                    format!("missing prerequisite: {}", r.display())
                } else {
                    format!("{} (missing: {})", self.requires_hint, r.display())
                });
            }
        }
        Ok(())
    }
}

impl BuildConfig {
    fn merge(&mut self, m: &Manifest) {
        self.needs_cxx |= m.cxx;
        self.include_dirs.extend(m.include_dirs.iter().cloned());
        self.extra_sources.extend(m.extra_sources.iter().cloned());
        self.defines.extend(m.defines.iter().cloned());
        self.pkg_config.extend(m.pkg_config.iter().cloned());
        self.link_args.extend(m.link_args.iter().cloned()); // already absolutised
        self.extra_dlls.extend(m.extra_dlls.iter().cloned());
    }
}

/// Resolve a repo-relative `-L` path in a link argument to an absolute one.
fn absolutise(repo_root: &Path, arg: &str) -> String {
    if let Some(rest) = arg.strip_prefix("-L") {
        if !rest.starts_with('/') {
            return format!("-L{}", repo_root.join(rest).display());
        }
    }
    arg.to_string()
}

/// Run `pkg-config <mode> <packages...>` and return the flags.
pub fn pkg_config_flags(packages: &[String], mode: &str) -> Result<Vec<String>, String> {
    pkg_config_flags_for(packages, mode, false)
}

/// The mingw-w64 sysroot's answer, for a Windows build.
///
/// The host's `pkg-config` describes the host's libraries, and its `-I` would
/// point a Windows compile at Linux headers. The cross answer comes from the
/// wrapper the distributions install beside the compiler
/// (`x86_64-w64-mingw32-pkg-config`, Fedora and Debian both), or — where only
/// the sysroot is there — from `pkg-config` itself pointed at the sysroot's
/// `.pc` files, which is all the wrapper does.
pub fn pkg_config_flags_cross(packages: &[String], mode: &str) -> Result<Vec<String>, String> {
    pkg_config_flags_for(packages, mode, true)
}

/// Where the mingw-w64 sysroot keeps its `.pc` files, on the layouts the
/// distributions use: `<gcc -print-sysroot>/mingw/lib/pkgconfig` on Fedora,
/// `/usr/x86_64-w64-mingw32/lib/pkgconfig` on Debian and Ubuntu.
fn mingw_pkgconfig_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(out) = Command::new("x86_64-w64-mingw32-gcc")
        .arg("-print-sysroot")
        .output()
    {
        let root = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !root.is_empty() && root != "/" {
            dirs.push(PathBuf::from(&root).join("mingw/lib/pkgconfig"));
            dirs.push(PathBuf::from(&root).join("lib/pkgconfig"));
        }
    }
    dirs.push(PathBuf::from("/usr/x86_64-w64-mingw32/sys-root/mingw/lib/pkgconfig"));
    dirs.push(PathBuf::from("/usr/x86_64-w64-mingw32/lib/pkgconfig"));
    dirs.retain(|d| d.is_dir());
    dirs
}

fn pkg_config_flags_for(packages: &[String], mode: &str, cross: bool) -> Result<Vec<String>, String> {
    if packages.is_empty() {
        return Ok(Vec::new());
    }
    let mut cmd = if cross {
        // The wrapper first; failing to start it is the one error that means
        // "use the sysroot directly", so it is told apart from a package the
        // wrapper could not find.
        let mut c = Command::new("x86_64-w64-mingw32-pkg-config");
        c.arg(mode).args(packages);
        match c.output() {
            Ok(out) if out.status.success() => {
                return Ok(String::from_utf8_lossy(&out.stdout)
                    .split_whitespace()
                    .map(str::to_string)
                    .collect());
            }
            Ok(out) => {
                return Err(format!(
                    "x86_64-w64-mingw32-pkg-config {mode} {} failed: {} (the mingw-w64 build \
                     of a library this program uses is not installed)",
                    packages.join(" "),
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
            Err(_) => {}
        }
        let dirs = mingw_pkgconfig_dirs();
        if dirs.is_empty() {
            return Err(format!(
                "no mingw-w64 pkg-config for {} — neither x86_64-w64-mingw32-pkg-config nor a \
                 sysroot with lib/pkgconfig is on this machine",
                packages.join(" ")
            ));
        }
        let libdir = dirs
            .iter()
            .map(|d| d.display().to_string())
            .collect::<Vec<_>>()
            .join(":");
        let mut c = Command::new("pkg-config");
        c.env("PKG_CONFIG_LIBDIR", libdir);
        c.env("PKG_CONFIG_SYSROOT_DIR", "");
        c
    } else {
        Command::new("pkg-config")
    };
    let out = cmd
        .arg(mode)
        .args(packages)
        .output()
        .map_err(|e| format!("pkg-config: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "pkg-config {mode} {} failed: {}",
            packages.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .map(str::to_string)
        .collect())
}

// --- minimal JSON readers for the fixed lib.json schema -------------------

fn json_field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{key}\"");
    let start = text.find(&needle)? + needle.len();
    let rest = text[start..].trim_start();
    rest.strip_prefix(':').map(|r| r.trim_start())
}

/// Is the key present at all? Distinguishes "declared empty" from "not
/// declared": an absent `windows_link_args` keeps the host list, an empty one
/// replaces it with nothing.
fn json_has(text: &str, key: &str) -> bool {
    json_field(text, key).is_some()
}

fn json_bool(text: &str, key: &str) -> bool {
    json_field(text, key).is_some_and(|r| r.starts_with("true"))
}

fn json_string(text: &str, key: &str) -> String {
    json_field(text, key)
        .and_then(|r| r.strip_prefix('"'))
        .and_then(|r| r.find('"').map(|e| r[..e].to_string()))
        .unwrap_or_default()
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
