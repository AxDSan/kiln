//! Optional dependencies: a `lib.json` key for a dependency that gates ONE
//! capability rather than the whole build.
//!
//! Both states are tested, because both ship. The absent one is what CI runs
//! and what every fresh checkout is, so it is the one that turns a build red;
//! the present one is the only proof that the include dirs, sources, defines
//! and link args actually reach the compiler when they are supposed to.
//!
//! Each test builds its own library rather than leaning on `net`, so that a
//! machine with a TLS stack vendored and one without run the same assertions.
//! `HOME` and the working directory are pinned for the reason `kits.rs` states:
//! both are inputs to kit resolution.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kiln_optional_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch");
    dir
}

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent");
    }
    std::fs::write(path, text).expect("write file");
}

/// A library whose one command answers differently depending on whether the
/// optional dependency was folded in.
///
/// The command exists in both states and has the same signature in both: an
/// optional dependency gates behaviour, never the command surface, or a program
/// would compile on one machine and fail to compile on another.
fn write_kit(dir: &Path, manifest: &str) {
    write(
        &dir.join("opt_libinfo.c"),
        r#"#include "kiln_abi.h"
void opt_answer(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
static const Kiln_CommandDesc C[] = {
    { "opt_answer", "opt_answer", KN_SDT_INT, 0, 0 },
};
static const Kiln_LibInfo I = {
    KILN_ABI_VERSION, "opt", "kiln-test-opt-0000-0000-6f707401", 1, 0, 0,
    (int32_t)(sizeof(C) / sizeof(C[0])), C, 0, 0,
};
const Kiln_LibInfo *kiln_get_lib_info(void) { return &I; }
"#,
    );
    // The `#ifdef` is the whole point: with the dependency absent this file
    // must compile with no sight of the header, the extra source or the
    // archive, all three of which only exist under the macro.
    write(
        &dir.join("opt_cmds.c"),
        r#"#include "kiln_abi.h"
#ifdef OPT_HAVE_DEP
#include "opt_dep.h"
/* A define that arrives with the dependency has no value to return, so it is
 * checked where a missing one is loudest. */
#ifndef OPT_DEFINE_REACHED
#error "optional_defines did not reach the compiler"
#endif
#endif
void opt_answer(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {
    (void)argc; (void)argv;
#ifdef OPT_HAVE_DEP
    kn_ret_int(ret, opt_dep_archive() + opt_dep_source());
#else
    kn_ret_int(ret, 0);
#endif
}
"#,
    );
    write(&dir.join("lib.json"), manifest);
}

/// The dependency the optional keys point at: a header, a source file and a
/// static archive, one per kind of contribution the manifest can make.
fn write_dependency(dep: &Path) {
    write(
        &dep.join("include/opt_dep.h"),
        "int opt_dep_archive(void);\nint opt_dep_source(void);\n",
    );
    write(&dep.join("src/opt_dep_source.c"), "int opt_dep_source(void) { return 4; }\n");
    write(&dep.join("archive/opt_dep_archive.c"), "int opt_dep_archive(void) { return 3; }\n");

    let obj = dep.join("archive/opt_dep_archive.o");
    let ok = Command::new("clang")
        .args(["-c", "-o"])
        .arg(&obj)
        .arg(dep.join("archive/opt_dep_archive.c"))
        .status()
        .expect("run clang")
        .success();
    assert!(ok, "compiling the test dependency failed");
    let ok = Command::new("ar")
        .arg("rcs")
        .arg(dep.join("archive/libopt_dep.a"))
        .arg(&obj)
        .status()
        .expect("run ar")
        .success();
    assert!(ok, "archiving the test dependency failed");
}

/// Absolute paths throughout: manifest paths are resolved against the root the
/// loader is handed, which for a kit outside `libs/` is a staged one. A test
/// that wrote into the repository's `vendor/` would also race every other test
/// doing the same.
fn manifest(dep: &Path, marker: &Path) -> String {
    format!(
        r#"{{
  "name": "opt",
  "optional_requires": ["{marker}"],
  "optional_feature": "OPT_HAVE_DEP",
  "optional_include_dirs": ["{inc}"],
  "optional_extra_sources": ["{src}"],
  "optional_defines": ["OPT_DEFINE_REACHED=1"],
  "optional_link_args": ["-L{lib}", "-lopt_dep"]
}}
"#,
        marker = marker.display(),
        inc = dep.join("include").display(),
        src = dep.join("src/opt_dep_source.c").display(),
        lib = dep.join("archive").display(),
    )
}

fn program(dir: &Path) -> PathBuf {
    let path = dir.join("prog.kiln");
    write(
        &path,
        "module prog\nuse opt\nsub main\n  call print_int(opt_answer())\nend\n",
    );
    path
}

fn kiln(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln")
}

/// Build and run, returning stdout.
fn build_and_run(root: &Path, tag: &str) -> String {
    let home = root.join("home");
    std::fs::create_dir_all(&home).expect("create home");
    let src = program(root);
    let bin = root.join(tag);
    let out = kiln(
        root,
        &home,
        &["build", src.to_str().unwrap(), "-o", bin.to_str().unwrap()],
    );
    assert!(
        out.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&bin).output().expect("run program");
    assert!(run.status.success(), "the built program did not run");
    String::from_utf8_lossy(&run.stdout).trim().to_string()
}

/// The state CI is in and every fresh checkout is in: the dependency is not
/// vendored, so none of the optional configuration exists, the feature macro is
/// undefined, and the build still succeeds.
#[test]
fn an_absent_optional_dependency_leaves_the_build_alone() {
    let root = scratch("absent");
    let dep = root.join("dep");
    write_kit(
        &root.join("kits/opt"),
        &manifest(&dep, &dep.join("archive/libopt_dep.a")),
    );
    // Nothing under `dep` was ever created: the header the `#ifdef` would
    // include, the extra source and the archive are all missing, which is
    // exactly what makes this a real test of the absent path.
    assert_eq!(build_and_run(&root, "absent"), "0");
}

/// Vendored: the header is on the include path, the extra source is compiled
/// in, the archive is on the link line, and the feature macro is defined.
#[test]
fn a_present_optional_dependency_contributes_everything_it_declares() {
    let root = scratch("present");
    let dep = root.join("dep");
    write_dependency(&dep);
    write_kit(
        &root.join("kits/opt"),
        &manifest(&dep, &dep.join("archive/libopt_dep.a")),
    );
    // 3 from the archive plus 4 from the extra source: a wrong answer names
    // which contribution did not arrive.
    assert_eq!(build_and_run(&root, "present"), "7");
}

/// The command surface may not move with the dependency. A program that
/// compiles against a library must compile against it on a colleague's machine,
/// whatever either of them has vendored.
#[test]
fn the_command_surface_is_the_same_in_both_states() {
    let absent = scratch("surface_absent");
    let dep_absent = absent.join("dep");
    write_kit(
        &absent.join("kits/opt"),
        &manifest(&dep_absent, &dep_absent.join("archive/libopt_dep.a")),
    );

    let present = scratch("surface_present");
    let dep_present = present.join("dep");
    write_dependency(&dep_present);
    write_kit(
        &present.join("kits/opt"),
        &manifest(&dep_present, &dep_present.join("archive/libopt_dep.a")),
    );

    let home = absent.join("home");
    std::fs::create_dir_all(&home).expect("create home");
    let listing = |cwd: &Path| {
        let out = kiln(cwd, &home, &["commands", "--use", "opt"]);
        assert!(
            out.status.success(),
            "commands failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let without = listing(&absent);
    assert!(without.contains("opt_answer"), "the command must exist without the dependency");
    assert_eq!(without, listing(&present));
}

/// `requires` keeps meaning what it always meant. `ui` without RmlUi is not a
/// build with one capability missing, it is no build at all, and it must keep
/// saying so in the library's own words.
#[test]
fn a_missing_hard_requirement_still_fails_loudly() {
    let root = scratch("hard");
    let dep = root.join("dep");
    write_kit(
        &root.join("kits/opt"),
        &format!(
            r#"{{
  "name": "opt",
  "requires": ["{missing}"],
  "requires_hint": "the widget stack is not vendored yet — run tools/fetch-widgets.sh"
}}
"#,
            missing = dep.join("libwidgets.a").display()
        ),
    );
    let home = root.join("home");
    std::fs::create_dir_all(&home).expect("create home");
    let src = program(&root);
    let out = kiln(
        &root,
        &home,
        &["build", src.to_str().unwrap(), "-o", root.join("hard").to_str().unwrap()],
    );
    assert!(!out.status.success(), "a missing `requires` path must fail the build");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("tools/fetch-widgets.sh") && err.contains("libwidgets.a"),
        "the failure must carry the library's hint and the missing path: {err}"
    );
}

/// Every `lib.json` in the tree predates these keys and has none of them. A
/// manifest without them must behave exactly as it did — no feature macro, no
/// extra flags, and above all no failure for a dependency it never declared.
#[test]
fn a_manifest_with_none_of_the_new_keys_is_unchanged() {
    let root = scratch("legacy");
    write_kit(
        &root.join("kits/opt"),
        "{ \"name\": \"opt\", \"display\": \"Opt\", \"version\": \"1.0.0\" }\n",
    );
    assert_eq!(build_and_run(&root, "legacy"), "0");
}

/// `net` is the library the optional mechanism was built for, and the one rule
/// it must never break: an `https://` URL is either served over TLS or refused.
/// It is never quietly turned into an `http://` request.
///
/// Both states are asserted from the same test, keyed off what is actually
/// vendored, so this passes on a CI machine that has never run
/// tools/fetch-mbedtls.sh and on a developer's machine that has. Nothing here
/// touches the network: port 1 on the loopback refuses instantly, and a refused
/// connection is proof that the scheme was accepted and the dial was attempted.
#[test]
fn https_is_served_or_refused_and_never_downgraded() {
    let root = scratch("net_tls");
    let home = root.join("home");
    std::fs::create_dir_all(&home).expect("create home");
    let src = root.join("tls.kiln");
    write(
        &src,
        "module tls\nuse net\nsub main\n  \
         let body: text = net_http_get(\"https://127.0.0.1:1/\")\n  \
         call print_int(last_error_code())\n  \
         call print_int(length(body))\nend\n",
    );
    let bin = root.join("tls");
    let out = kiln(
        &root,
        &home,
        &["build", src.to_str().unwrap(), "-o", bin.to_str().unwrap()],
    );
    assert!(
        out.status.success(),
        "a program using net must build whether or not TLS is vendored: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&bin).output().expect("run program");
    let printed = String::from_utf8_lossy(&run.stdout);
    let code: i64 = printed
        .lines()
        .next()
        .and_then(|l| l.trim().parse().ok())
        .expect("the program printed an error code");

    let vendored = repo().join("vendor/mbedtls/build/library/libmbedtls.a").exists();
    if vendored {
        // The dial happened, so the scheme was accepted. 10006 here would mean
        // the URL was refused on a build that can serve it.
        assert_ne!(code, 10006, "https must work when mbedTLS is vendored");
        assert_ne!(code, 0, "nothing listens on port 1, so this cannot succeed");
    } else {
        // KN_ERR_UNSUPPORTED, and an empty body: refused before a socket, not
        // fetched over http.
        assert_eq!(code, 10006, "https must be refused when there is no TLS");
        assert!(
            printed.lines().nth(1) == Some("0"),
            "a refused https request must return nothing: {printed}"
        );
    }
}

/// The refusal, proved on any machine.
///
/// The test above can only ever check one of net's two states — whichever the
/// machine it runs on happens to have vendored — and the state it skips on a
/// developer's machine is precisely the state CI ships. So this one stages a
/// copy of `net` as a project kit whose `optional_requires` names a path that
/// cannot exist, which makes the absent branch reachable with mbedTLS sitting
/// right there in `vendor/`. It is the same sources and the same `#ifdef`; only
/// the manifest differs.
///
/// What it asserts is the rule that must never bend: no TLS means the request
/// is REFUSED. Not fetched over http, not returned empty and successful —
/// refused, with KN_ERR_UNSUPPORTED and a message naming the fetch script.
#[test]
fn without_tls_an_https_url_is_refused_on_any_machine() {
    let root = scratch("net_notls");
    let home = root.join("home");
    std::fs::create_dir_all(&home).expect("create home");

    let kit = root.join("kits/net");
    std::fs::create_dir_all(&kit).expect("create kit");
    for entry in std::fs::read_dir(repo().join("libs/net")).expect("read libs/net") {
        let src = entry.expect("entry").path();
        if matches!(src.extension().and_then(|e| e.to_str()), Some("c") | Some("h")) {
            std::fs::copy(&src, kit.join(src.file_name().unwrap())).expect("copy net source");
        }
    }
    write(
        &kit.join("lib.json"),
        r#"{
  "name": "net",
  "optional_requires": ["vendor/kiln-test-no-tls-here/libmbedtls.a"],
  "optional_feature": "KILN_NET_TLS",
  "optional_include_dirs": ["vendor/kiln-test-no-tls-here/include"],
  "optional_link_args": ["-Lvendor/kiln-test-no-tls-here", "-lmbedtls"]
}
"#,
    );

    let src = root.join("notls.kiln");
    write(
        &src,
        "module notls\nuse net\nsub main\n  \
         let body: text = net_http_get(\"https://127.0.0.1:1/\")\n  \
         call print_int(last_error_code())\n  \
         call print_int(length(body))\n  \
         call print_text(last_error_text())\nend\n",
    );
    let bin = root.join("notls");
    let out = kiln(
        &root,
        &home,
        &["build", src.to_str().unwrap(), "-o", bin.to_str().unwrap()],
    );
    // The first half of the promise: an unvendored optional dependency costs
    // nobody a build. The `-lmbedtls` above would fail this link if any of the
    // optional configuration had leaked through.
    assert!(
        out.status.success(),
        "net must build with no TLS stack at all: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let run = Command::new(&bin).output().expect("run program");
    let printed = String::from_utf8_lossy(&run.stdout);
    let mut lines = printed.lines();
    assert_eq!(
        lines.next().map(str::trim),
        Some("10006"),
        "https without TLS must fail with KN_ERR_UNSUPPORTED: {printed}"
    );
    // Nothing came back, and nothing could have: a body here would mean the
    // request went out in the clear.
    assert_eq!(
        lines.next().map(str::trim),
        Some("0"),
        "a refused request must return nothing: {printed}"
    );
    let message = lines.collect::<Vec<_>>().join(" ");
    assert!(
        message.contains("tools/fetch-mbedtls.sh"),
        "the refusal must say how to fix it: {message}"
    );
    assert!(
        !message.contains("http://"),
        "the refusal must not suggest a plaintext request: {message}"
    );
}
