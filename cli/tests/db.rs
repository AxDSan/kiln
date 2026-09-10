//! `libs/db`, end to end in a built binary.
//!
//! Everything here runs against SQLite's `:memory:`, so the suite needs no
//! server, no file and no credentials — and every statement it runs is one of
//! the four the login slice runs, in the shape it runs them. MySQL shares the
//! surface and the parameter path; what it does not share is a machine with a
//! server on it, so it is exercised by hand rather than here.

use std::path::PathBuf;
use std::process::Command;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Build inline source and run it, answering its stdout. A tag per test, or
/// two tests race on one output path.
fn run(src: &str, tag: &str) -> String {
    let dir = std::env::temp_dir().join(format!("kiln_db_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let path = dir.join("main.kiln");
    std::fs::write(&path, src).expect("write source");
    let bin = dir.join("prog");

    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", path.to_str().unwrap(), "-o", bin.to_str().unwrap()])
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln build");
    assert!(
        out.status.success(),
        "kiln build {tag} failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let run = Command::new(&bin).output().expect("run the program");
    assert!(
        run.status.success(),
        "{tag} exited non-zero:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    String::from_utf8_lossy(&run.stdout).to_string()
}

/// The four statements the login slice runs, in the shapes it runs them: a
/// one-row SELECT with a bound name, an UPDATE, a DELETE and an INSERT.
#[test]
fn the_login_statements_run_with_bound_parameters() {
    let out = run(
        r#"module dbslice
target console
use db

sub main
  let h: int = db_open("sqlite::memory:")
  if h = 0
    call print_text("open failed: {last_error_text()}")
    return
  end

  call db_exec(h, "create table accounts (id int, username text, password_hash text)", [])
  call db_exec(h, "create table handoff_tickets (id int, expires_at int)", [])

  call print_int(db_exec(h, "insert into accounts values (?, ?, ?)", ["1", "ada", "hashed"]))
  call print_int(db_exec(h, "insert into handoff_tickets values (?, ?)", ["1", "10"]))
  call print_int(db_exec(h, "delete from handoff_tickets where expires_at < ?", ["99"]))
  call print_int(db_exec(h, "update accounts set password_hash = ? where id = ?", ["new", "1"]))

  let rows: int = db_query(h, "select id, username, password_hash from accounts where username = ? limit 1", ["ada"])
  call print_int(db_columns(rows))
  if db_next(rows)
    call print_int(db_int(rows, 1))
    call print_text(db_text(rows, 2))
    call print_text(db_text(rows, 3))
  end
  call print_text("more: {db_next(rows)}")
  call db_result_close(rows)
  call db_close(h)
end
"#,
        "slice",
    );
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        vec!["1", "1", "1", "1", "3", "1", "ada", "new", "more: false"],
        "{out}"
    );
}

/// The distinction the whole `_n` family exists for: a bound NULL is not an
/// empty string, and `db_is_null` is the only thing that can tell them apart
/// afterwards. `AccountRepository.RecordSuccessfulLoginAsync` writes NULL for
/// an IP the client did not send, so a port that wrote `""` would show a blank
/// address where the .NET server shows "never".
#[test]
fn a_bound_null_is_not_an_empty_string() {
    let out = run(
        r#"module dbnull
target console
use db

sub main
  let h: int = db_open("sqlite::memory:")
  call db_exec(h, "create table t (who text, ip text)", [])
  let a: int = db_exec_n(h, "insert into t values (?, ?)", ["null_one", ""], [false, true])
  let b: int = db_exec(h, "insert into t values (?, ?)", ["empty_one", ""])
  call print_int(a + b)

  let rows: int = db_query(h, "select who, ip from t order by who", [])
  while db_next(rows)
    call print_text("{db_text(rows, 1)} null={db_is_null(rows, 2)} text='{db_text(rows, 2)}'")
  end
  call db_result_close(rows)
  call db_close(h)
end
"#,
        "null",
    );
    assert!(out.contains("empty_one null=false text=''"), "{out}");
    assert!(out.contains("null_one null=true text=''"), "{out}");
}

/// What the game-server port needs beyond the login slice, in the shapes it
/// needs them: a transaction that undoes an INSERT, the id that INSERT
/// produced, and a double, a bool and a column name read back typed. The
/// MySQL half of each is `docs/gbo-port-probes/p9_transactions_typed.kiln`.
#[test]
fn transactions_insert_ids_and_typed_reads() {
    let out = run(
        r#"module dbtx
target console
use db

sub main
  let h: int = db_open("sqlite::memory:")
  call db_exec(h, "create table t (id integer primary key, name text, ratio real, banned int)", [])

  call db_begin(h)
  call db_exec(h, "insert into t (name, ratio, banned) values (?, ?, ?)", ["Ada", "0.75", "1"])
  call print_text("in tx id={db_last_insert_id(h)}")
  if db_begin(h)
    call print_text("nested begin accepted")
  else
    call print_text("nested begin refused")
  end
  call db_rollback(h)
  var rows: int = db_query(h, "select count(*) from t", [])
  if db_next(rows)
    call print_text("after rollback count={db_int(rows, 1)}")
  end
  call db_result_close(rows)

  call db_begin(h)
  call db_exec(h, "insert into t (name, ratio, banned) values (?, ?, ?)", ["Ada", "0.75", "1"])
  let id: int64 = db_last_insert_id(h)
  call db_commit(h)
  rows = db_query(h, "select name, ratio, banned as flag from t where id = ?", [int64_to_text(id)])
  if db_next(rows)
    call print_text("name={db_text(rows, 1)} ratio={db_double(rows, 2)} banned={db_bool(rows, 3)} col={db_column_name(rows, 3)}")
  end
  call db_result_close(rows)
  if db_commit(h)
    call print_text("stray commit accepted")
  else
    call print_text("stray commit refused")
  end
  call db_close(h)
end
"#,
        "tx",
    );
    assert!(out.contains("in tx id=1"), "{out}");
    assert!(out.contains("nested begin refused"), "{out}");
    assert!(out.contains("after rollback count=0"), "{out}");
    assert!(out.contains("name=Ada ratio=0.75 banned=true col=flag"), "{out}");
    assert!(out.contains("stray commit refused"), "{out}");
}

/// The reason the surface has no `db_exec(h, sql)` without a parameter list:
/// a value that looks like SQL must stay a value. This is the classic payload,
/// and it must match nothing.
#[test]
fn a_parameter_is_a_value_and_never_syntax() {
    let out = run(
        r#"module dbinject
target console
use db

sub main
  let h: int = db_open("sqlite::memory:")
  call db_exec(h, "create table accounts (username text)", [])
  call db_exec(h, "insert into accounts values (?)", ["ada"])

  let rows: int = db_query(h, "select username from accounts where username = ?", ["ada' or '1'='1"])
  if db_next(rows)
    call print_text("MATCHED")
  else
    call print_text("no match")
  end
  call db_result_close(rows)

  # And the same through the NULL-aware form, which builds its own statement
  # text for MySQL and must escape just as carefully.
  let rows2: int = db_query_n(h, "select username from accounts where username = ?", ["'; drop table accounts; --"], [false])
  if db_next(rows2)
    call print_text("MATCHED2")
  else
    call print_text("no match 2")
  end
  call db_result_close(rows2)

  # The table is still there.
  let rows3: int = db_query(h, "select username from accounts", [])
  if db_next(rows3)
    call print_text(db_text(rows3, 1))
  end
  call db_result_close(rows3)
  call db_close(h)
end
"#,
        "inject",
    );
    assert!(!out.contains("MATCHED"), "a parameter reached SQL as syntax:\n{out}");
    assert!(out.contains("no match\nno match 2\nada"), "{out}");
}

/// Handles are the runtime's, so the whole handle contract applies: a stale
/// one is rejected rather than reused, a connection handle is not a result
/// handle, and 0 is never valid. None of these may crash.
#[test]
fn bad_handles_are_rejected_not_dereferenced() {
    let out = run(
        r#"module dbhandles
target console
use db

sub main
  let h: int = db_open("sqlite::memory:")
  call db_exec(h, "create table t (n int)", [])
  let rows: int = db_query(h, "select n from t", [])
  call db_result_close(rows)

  # Stale: closed, and asked again.
  call print_text("stale next: {db_next(rows)} code={last_error_code()}")

  # Wrong kind: a connection handle where a result handle goes.
  call print_int(db_columns(h))
  call print_int(last_error_code())

  # Zero, and garbage.
  call print_int(db_columns(0))
  call print_int(db_columns(987654))

  # A column that is not there, on a row that is.
  call db_exec(h, "insert into t values (?)", ["1"])
  let r2: int = db_query(h, "select n from t", [])
  if db_next(r2)
    call print_text("col 9: '{db_text(r2, 9)}' code={last_error_code()}")
  end
  call db_result_close(r2)
  call db_close(h)
  call print_text("still here")
end
"#,
        "handles",
    );
    assert!(out.contains("stale next: false"), "{out}");
    assert!(out.contains("still here"), "the program crashed on a bad handle:\n{out}");
    // Every bad handle reports rather than answering as though it worked.
    assert!(out.contains("-1"), "a wrong-kind handle answered as a result:\n{out}");
}

/// A build with no database client still builds, and says so at run time
/// rather than at compile time — the whole point of the optional manifest.
/// Skipped when this machine has the clients, because then it is not what
/// would happen.
#[test]
fn without_a_client_the_commands_report_unsupported() {
    let have_sqlite = std::path::Path::new("/usr/include/sqlite3.h").exists();
    let have_mysql = std::path::Path::new("/usr/include/mysql/mysql.h").exists();
    if have_sqlite && have_mysql {
        eprintln!("skipped: this machine has both clients, so the unsupported path is unreachable");
        return;
    }
    let out = run(
        r#"module dbunsupported
target console
use db

sub main
  call print_int(db_open("sqlite::memory:"))
  call print_int(last_error_code())
end
"#,
        "unsupported",
    );
    assert!(out.starts_with("0\n10006"), "{out}");
}
