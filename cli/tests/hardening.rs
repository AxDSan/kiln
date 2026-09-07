//! The release profile, checked against the binary rather than against the
//! flags we believe we passed. A hardening flag the local clang quietly drops
//! leaves a build that still succeeds and a program that is not hardened, so
//! every assertion here reads the ELF that came out.
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Build `examples/hello.kiln`, once per mode for the whole test binary.
///
/// Once, because tests run in parallel and two of them writing one output path
/// race — one truncating the file the other is reading.
fn hello(release: bool) -> &'static Path {
    static DEBUG: OnceLock<PathBuf> = OnceLock::new();
    static RELEASE: OnceLock<PathBuf> = OnceLock::new();
    let cell = if release { &RELEASE } else { &DEBUG };
    cell.get_or_init(|| {
        let repo = repo();
        let example = repo.join("examples").join("hello.kiln");
        let mode = if release { "release" } else { "debug" };
        let out = std::env::temp_dir().join(format!("kiln_hardening_{mode}"));
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_kiln"));
        cmd.args(["build", example.to_str().unwrap()]);
        if release {
            cmd.arg("--release");
        }
        cmd.args(["-o", out.to_str().unwrap()])
            .env("KILN_RUNTIME_DIR", repo.join("runtime"));
        let status = cmd.status().expect("run kiln");
        assert!(status.success(), "kiln build --{mode} failed");
        out
    })
}

/// `tool` applied to the binary, or `None` when the tool is not installed —
/// which skips the check rather than failing a machine without binutils.
fn inspect(tool: &str, args: &[&str], bin: &Path) -> Option<String> {
    let out = Command::new(tool).args(args).arg(bin).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn bytes(bin: &Path) -> Vec<u8> {
    std::fs::read(bin).expect("read the built binary")
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|w| w == needle.as_bytes())
}

#[test]
fn release_program_still_produces_the_same_output() {
    let debug = Command::new(hello(false))
        .output()
        .expect("run debug build");
    let release = Command::new(hello(true))
        .output()
        .expect("run release build");
    assert!(
        release.status.success(),
        "the release build exited non-zero"
    );
    assert_eq!(
        String::from_utf8_lossy(&debug.stdout),
        String::from_utf8_lossy(&release.stdout),
        "optimising and hardening changed what the program prints"
    );
}

#[test]
fn release_is_position_independent() {
    let Some(header) = inspect("readelf", &["-h"], hello(true)) else {
        eprintln!("readelf unavailable; skipping the PIE check");
        return;
    };
    let kind = header
        .lines()
        .find(|l| l.trim_start().starts_with("Type:"))
        .unwrap_or_default();
    assert!(
        kind.contains("DYN"),
        "expected a position-independent executable, got `{}`",
        kind.trim()
    );
}

#[test]
fn release_binds_now_behind_read_only_relocations() {
    let Some(dynamic) = inspect("readelf", &["-dW"], hello(true)) else {
        eprintln!("readelf unavailable; skipping the RELRO check");
        return;
    };
    assert!(
        dynamic.contains("BIND_NOW") || dynamic.contains("Flags: NOW"),
        "the release binary resolves symbols lazily:\n{dynamic}"
    );
    let segments = inspect("readelf", &["-lW"], hello(true)).expect("readelf ran a moment ago");
    assert!(
        segments.contains("GNU_RELRO"),
        "the release binary has no RELRO segment:\n{segments}"
    );
}

/// The symbol table is the source shape: subroutine names, module variables,
/// which runtime commands were linked. A release build has none of it, and a
/// debug build keeps all of it — that difference is the whole point of the flag.
#[test]
fn release_is_stripped_and_debug_is_not() {
    let Some(debug_symbols) = inspect("nm", &[], hello(false)) else {
        eprintln!("nm unavailable; skipping the symbol check");
        return;
    };
    assert!(
        debug_symbols.contains("kn_print_text"),
        "a debug build should keep its symbol table"
    );

    // GNU nm answers a stripped file with an empty listing and a note on
    // stderr; some builds make that note an error. Either is the pass.
    let stripped = match Command::new("nm").arg(hello(true)).output() {
        Ok(o) => {
            o.stdout.iter().all(|b| b.is_ascii_whitespace())
                || String::from_utf8_lossy(&o.stderr).contains("no symbols")
        }
        Err(_) => {
            eprintln!("nm unavailable; skipping the symbol check");
            return;
        }
    };
    assert!(stripped, "the release binary still carries a symbol table");
}

/// Dead-stripping has to survive the release profile — it is the property the
/// whole compilation model exists for.
///
/// A stripped binary has no symbol names left to ask `nm` about, so the witness
/// is data instead: the strftime format inside `format_time`, a command
/// `hello.kiln` never calls. `-fdata-sections` and `--gc-sections` drop it;
/// stripping would not, which is what makes it a test of the right thing. The
/// program's own literal is there to prove the search can find anything at all.
#[test]
fn release_still_drops_unused_commands() {
    let unused = "%Y-%m-%d %H:%M:%S";
    let used = "Kiln — arithmetic demo";
    // Absence proves nothing about a string that no longer exists: if the
    // format ever moves or is reworded, this test must fail rather than pass
    // for the wrong reason.
    let source = std::fs::read_to_string(repo().join("runtime").join("kn_datetime.c"))
        .expect("read the datetime commands");
    assert!(
        source.contains(unused),
        "`format_time` no longer holds {unused}; pick a new witness"
    );
    for release in [false, true] {
        let image = bytes(hello(release));
        assert!(
            contains(&image, used),
            "the program's own text is missing (release: {release})"
        );
        assert!(
            !contains(&image, unused),
            "`format_time` survived into a binary that never calls it (release: {release})"
        );
    }
}
