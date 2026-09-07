//! The `time` support library, cross-built for Windows, says what it says on
//! Linux — including for instants before 1970.
//!
//! Windows' `gmtime_s` refuses a negative `time_t`, so a library that asked it
//! for the calendar answered "" and 0 for the first moonwalk there and a date
//! everywhere else. The library computes the civil date itself now; these
//! tests build the same program for both platforms, run the Windows image
//! under wine, and hold the two outputs to be the same lines. Line *endings*
//! differ by platform convention — a Windows console program writes CRLF —
//! and `lines()` strips both, so the comparison is of what was said.
//!
//! Each test skips itself with a line saying why when mingw or wine is not on
//! this machine, so a green run there is not mistaken for proof.
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn on_path(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn mingw_present() -> bool {
    if on_path("x86_64-w64-mingw32-gcc") {
        return true;
    }
    eprintln!("x86_64-w64-mingw32-gcc is not installed; skipping the Windows time-library test");
    false
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kiln_time_windows_{tag}_test"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Build `source` into `out`; `os` is `None` for this machine.
fn build(source: &Path, out: &Path, os: Option<&str>) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_kiln"));
    cmd.args(["build", source.to_str().unwrap()]);
    if let Some(os) = os {
        cmd.args(["--os", os]);
    }
    let status = cmd
        .arg("-o")
        .arg(out)
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .status()
        .expect("run kiln");
    assert!(
        status.success(),
        "kiln build {} failed for {}",
        os.map(|o| format!("--os {o}")).unwrap_or_default(),
        source.display()
    );
}

fn lines_of(out: std::process::Output, what: &str) -> Vec<String> {
    assert!(
        out.status.success(),
        "{what} exited non-zero:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

fn linux_lines(image: &Path, cwd: &Path) -> Vec<String> {
    let out = Command::new(image).current_dir(cwd).output().expect("run the Linux build");
    lines_of(out, "the Linux program")
}

/// Run the image under wine when it is here; `None` when it is not.
fn wine_lines(image: &Path, cwd: &Path) -> Option<Vec<String>> {
    if !on_path("wine") {
        eprintln!("wine is not installed; the Windows image was built but not run");
        return None;
    }
    let out = Command::new("wine")
        .arg(image)
        .current_dir(cwd)
        .env("WINEDEBUG", "-all")
        .output()
        .expect("run wine");
    Some(lines_of(out, "the Windows program under wine"))
}

/// Build `source` for both platforms and hold the Windows program to the
/// Linux program's every line. `must_say` guards against an empty-vs-empty
/// pass: it is a line the Linux build is known to print.
fn same_on_both(tag: &str, source: &Path, must_say: &[&str]) {
    if !mingw_present() {
        return;
    }
    let dir = scratch(tag);
    build(source, &dir.join("linux_build"), None);
    build(source, &dir.join("win_build"), Some("windows"));
    let image = dir.join("win_build.exe");
    assert!(image.is_file(), "expected {} to be written", image.display());

    let linux = linux_lines(&dir.join("linux_build"), &dir);
    for line in must_say {
        assert!(
            linux.iter().any(|l| l == line),
            "the Linux build did not print {line:?}:\n{}",
            linux.join("\n")
        );
    }
    if let Some(win) = wine_lines(&image, &dir) {
        assert_eq!(
            win, linux,
            "the Windows build (left) disagrees with the Linux build (right)"
        );
    }
}

/// The shipped example: Apollo 11's moonwalk in 1969, the epoch, a round trip,
/// and the error paths. Before the fix the Windows image printed "" for the
/// moonwalk's ISO text and 0 for its month, day, hour, minute, second and
/// day-of-year, and skipped "iso round trip ok".
#[test]
fn timelib_example_says_the_same_under_wine() {
    same_on_both(
        "example",
        &repo().join("examples/timelib.kiln"),
        &["1969-07-21T02:56:15Z", "iso round trip ok"],
    );
}

/// The paths the example does not reach: the weekday and day-of-year of
/// pre-1970 dates, the second before the epoch (a negative remainder), a
/// far-back leap day, and a far-forward one.
#[test]
fn pre_1970_calendar_fields_say_the_same_under_wine() {
    let dir = scratch("src");
    let source = dir.join("before1970.kiln");
    std::fs::write(
        &source,
        r#"module before1970
use time

sub show(t: int64)
  call print_text(time_format_iso(t))
  call print_int(time_weekday(t))
  call print_int(time_day_of_year(t))
  call print_int(time_month(t))
  call print_int(time_day(t))
  call print_int(time_hour(t))
  call print_int(time_minute(t))
  call print_int(time_second(t))
  call print_text(format_time(t, "%Y-%m-%d %H:%M:%S"))
end

sub main
  call show(time_from_parts(1906, 1, 1, 0, 0, 0))
  call show(time_from_parts(1969, 7, 20, 20, 17, 40))
  call show(time_parse_iso("1969-12-31T23:59:59Z"))
  call show(time_from_parts(1904, 2, 29, 12, 30, 45))
  call show(time_from_parts(1900, 3, 1, 0, 0, 0))
  call show(time_from_parts(2000, 2, 29, 23, 59, 59))
  call show(time_from_parts(2033, 5, 18, 3, 33, 20))
  call show(time_from_parts(1600, 12, 31, 0, 0, 0))
end
"#,
    )
    .expect("write the test program");
    same_on_both(
        "before1970",
        &source,
        &["1906-01-01T00:00:00Z", "1969-07-20T20:17:40Z", "1969-12-31T23:59:59Z"],
    );
}
