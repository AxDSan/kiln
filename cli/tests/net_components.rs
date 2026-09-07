//! The `tcpserver` and `tcpclient` components, end to end in built binaries.
//!
//! Every program here is built through the real `kiln` and spoken to over
//! a real socket — from Rust, or from the other component in the same
//! program — because a component whose events fire in a unit test and not in
//! a binary is the failure the suite exists to catch. The port is always one
//! the test just found free, passed in as `sys_arg(1)`: a property value in
//! the source must be a literal, so the program sets it in `main`.
//!
//! Every program exits on its own, by `quit()` or by switching its last
//! component off. A piped stdout is fully buffered, so a program the test
//! killed would have printed nothing at all — and a program that cannot end
//! is a bug this suite should report rather than hide behind a timeout.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Build one source file to a binary in a scratch directory unique to `tag`.
fn build(src_path: &Path, tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kiln_netc_{tag}"));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let bin = dir.join("prog");
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", src_path.to_str().unwrap(), "-o", bin.to_str().unwrap()])
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln");
    assert!(
        out.status.success(),
        "kiln build {tag} failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    bin
}

/// Build inline source.
fn build_src(src: &str, tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kiln_netc_{tag}"));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let path = dir.join("main.kiln");
    std::fs::write(&path, src).expect("write source");
    build(&path, tag)
}

/// A port nobody is listening on right now.
fn free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
    l.local_addr().unwrap().port()
}

fn start(bin: &Path, port: u16) -> Child {
    Command::new(bin)
        .arg(port.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start program")
}

/// Wait for the program to end by itself, within reason.
fn finish(mut child: Child, what: &str) -> (String, String) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while child.try_wait().expect("poll").is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            let out = child.wait_with_output().expect("collect");
            panic!(
                "{what} did not end on its own; stderr:\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let out = child.wait_with_output().expect("collect");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Connect once the listener is up. It opens on the first turn of the loop,
/// so the first attempt may beat it; retry rather than sleep a guessed amount.
fn connect(port: u16) -> TcpStream {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(s) = TcpStream::connect(("127.0.0.1", port)) {
            s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            return s;
        }
        assert!(Instant::now() < deadline, "nothing ever listened on {port}");
        std::thread::sleep(Duration::from_millis(30));
    }
}

/// Read until the reply holds `until`, or the peer closes.
fn read_until(s: &mut TcpStream, until: &str) -> String {
    let mut got = String::new();
    let mut buf = [0u8; 1024];
    while !got.contains(until) {
        match s.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => got.push_str(&String::from_utf8_lossy(&buf[..n])),
            Err(e) => panic!("read failed waiting for {until:?}: {e}; had {got:?}"),
        }
    }
    got
}

/// A listen failure is somebody else's machine, not a product failure: the
/// program says so and stops, and the test declines to invent a verdict.
///
/// Only the tests that wait for the program BEFORE speaking to it can ask
/// this. One that connects first would hang on its read instead, and with the
/// port found free an instant earlier that is a failure worth seeing, not a
/// case worth a skip.
fn port_was_busy(stdout: &str, stderr: &str) -> bool {
    stdout.contains("error: listen on") || stderr.contains("cannot listen on")
}

/// (a) The shipped echo example: two lines in, two echoes out, and the
/// program reports the client arriving and leaving by number.
#[test]
fn the_echo_example_echoes_lines_and_counts_clients_from_one() {
    let bin = build(&repo().join("examples/tcpecho.kiln"), "echo");
    let port = free_port();
    let child = start(&bin, port);

    let mut s = connect(port);
    s.write_all(b"hello\nworld\n").unwrap();
    let got = read_until(&mut s, "echo: world\n");
    assert_eq!(got, "echo: hello\necho: world\n");
    drop(s);

    // A second client is 2, never 1 again — and `quit` ends the program.
    let mut s = connect(port);
    s.write_all(b"quit\n").unwrap();
    let got = read_until(&mut s, "bye\n");
    assert_eq!(got, "bye\n");

    let (stdout, _) = finish(child, "tcpecho");
    assert!(stdout.contains("connect 1\n"), "no `connect 1`:\n{stdout}");
    assert!(stdout.contains("  from 127.0.0.1:"), "no client address:\n{stdout}");
    assert!(stdout.contains("disconnect 1\n"), "no `disconnect 1`:\n{stdout}");
    assert!(stdout.contains("connect 2\n"), "the second client must be 2:\n{stdout}");
}

/// (b) A server and a client in ONE program, talking over loopback: two pumps
/// on the runtime's loop, each event dispatched from its own. The client
/// dials `localhost`, which commonly resolves to ::1 first — a server bound
/// to 127.0.0.1 refuses there, and the client must move on to the next
/// address rather than report a refusal. The program ends by switching both
/// components off: no `quit()`, no source left, loop returns.
#[test]
fn a_server_and_a_client_in_one_program_ping_pong() {
    let bin = build_src(
        r#"module pingpong
use net
use system

tcpserver srv
  name = "srv"
  address = "127.0.0.1"
  on connect: srv_connect
  on disconnect: srv_disconnect
  on receive: srv_receive
  on error: srv_error
end

tcpclient cli
  name = "cli"
  host = "localhost"
  on connect: cli_connect
  on disconnect: cli_disconnect
  on receive: cli_receive
  on error: cli_error
end

sub main
  let port: int = text_to_int(sys_arg(1))
  srv.port = port
  cli.port = port
  srv.active = true
  cli.active = true
  call print_text("main returned")
end

sub srv_connect(client: int)
  call print_text(concat("server: client ", concat(int_to_text(client), " connected")))
  call print_text(concat("server: clients ", int_to_text(tcpserver_client_count("srv"))))
  call print_text(concat("server: first is ", int_to_text(tcpserver_client("srv", 1))))
end

sub srv_receive(client: int, data: text)
  call print_text(concat("server got ", data))
  call tcpserver_send("srv", client, "pong\n")
end

sub srv_disconnect(client: int)
  call print_text(concat("server: client ", concat(int_to_text(client), " left")))
  srv.active = false
end

sub srv_error(message: text)
  call print_text(concat("error: ", message))
  call quit()
end

sub cli_connect
  call print_text("client connected")
  call tcpclient_send("cli", "ping\n")
end

sub cli_receive(data: text)
  call print_text(concat("client got ", data))
  cli.active = false
end

sub cli_disconnect
  call print_text("client disconnected")
end

sub cli_error(message: text)
  call print_text(concat("client error: ", message))
  call quit()
end
"#,
        "pingpong",
    );
    let port = free_port();
    let (stdout, stderr) = finish(start(&bin, port), "pingpong");
    if port_was_busy(&stdout, &stderr) {
        eprintln!("port {port} was busy; skipping");
        return;
    }
    let expected = "main returned\n\
                    server: client 1 connected\n\
                    server: clients 1\n\
                    server: first is 1\n\
                    client connected\n\
                    server got ping\n\
                    client got pong\n\
                    client disconnected\n\
                    server: client 1 left\n";
    assert_eq!(stdout, expected, "stderr:\n{stderr}");
}

/// (c) A refused connection is an `error` with a message, the error slot set
/// for the handler to read, and a client that switched itself off — so the
/// program, with nothing else to wait for, ends.
#[test]
fn a_client_told_no_fires_error_with_the_slot_set() {
    let bin = build_src(
        r#"module refused
use net
use system

tcpclient cli
  name = "cli"
  host = "127.0.0.1"
  timeout_ms = 3000
  on connect: on_connect
  on error: on_error
end

sub main
  cli.port = text_to_int(sys_arg(1))
  cli.active = true
end

sub on_connect
  call print_text("connected?!")
end

sub on_error(message: text)
  call print_text(concat("error: ", message))
  if last_error_code() <> 0
    call print_text("code set")
  end
  if cli.active
    call print_text("still active")
  else
    call print_text("inactive")
  end
  if tcpclient_connected("cli")
    call print_text("connected?!")
  end
end
"#,
        "refused",
    );
    // Found free and then released: nothing is listening there.
    let port = free_port();
    let (stdout, stderr) = finish(start(&bin, port), "refused");
    assert!(
        stdout.contains(&format!("error: connect 127.0.0.1:{port}: ")),
        "the message must name the failure and the address:\n{stdout}\n{stderr}"
    );
    assert!(stdout.contains("code set\n"), "last_error_code() was 0:\n{stdout}");
    assert!(stdout.contains("inactive\n"), "active should have dropped:\n{stdout}");
    assert!(!stdout.contains("connected?!"), "it connected to nothing:\n{stdout}");
}

/// (d) An empty delimiter delivers what arrived, as it arrived.
#[test]
fn an_empty_delimiter_delivers_raw_chunks() {
    let bin = build_src(
        r#"module rawchunks
use net
use system

tcpserver raw
  name = "raw"
  address = "127.0.0.1"
  delimiter = ""
  on receive: on_receive
  on error: on_error
end

sub main
  raw.port = text_to_int(sys_arg(1))
  raw.active = true
end

sub on_receive(client: int, data: text)
  call print_text(concat("chunk: ", data))
  call tcpserver_send("raw", client, concat("[", concat(data, "]")))
  if data = "stop"
    call quit()
  end
end

sub on_error(message: text)
  call print_text(concat("error: ", message))
  call quit()
end
"#,
        "rawchunks",
    );
    let port = free_port();
    let child = start(&bin, port);
    let mut s = connect(port);
    // No newline anywhere: with a delimiter these would never be delivered.
    s.write_all(b"abc").unwrap();
    assert_eq!(read_until(&mut s, "]"), "[abc]");
    s.write_all(b"stop").unwrap();
    assert_eq!(read_until(&mut s, "]"), "[stop]");
    let (stdout, _) = finish(child, "rawchunks");
    assert_eq!(stdout, "chunk: abc\nchunk: stop\n");
}

/// A client that leaves is reported once, a stale id is refused as stale, and
/// `active = false` from inside a handler tells every remaining client.
#[test]
fn disconnects_are_reported_once_and_stale_ids_are_refused() {
    let bin = build_src(
        r#"module leaving
use net
use system

tcpserver s
  name = "s"
  address = "127.0.0.1"
  on connect: on_connect
  on disconnect: on_disconnect
  on receive: on_receive
  on error: on_error
end

sub main
  s.port = text_to_int(sys_arg(1))
  s.active = true
end

sub on_connect(client: int)
  call print_text(concat("connect ", int_to_text(client)))
end

sub on_disconnect(client: int)
  call print_text(concat("disconnect ", int_to_text(client)))
end

sub on_receive(client: int, data: text)
  if data = "kick"
    call tcpserver_disconnect("s", client)
    if tcpserver_send("s", client, "x")
      call print_text("sent to a dead client?!")
    else
      call print_text(concat("stale code ", int_to_text(last_error_code())))
    end
  end
  if data = "stop"
    call tcpserver_send("s", client, "stopping\n")
    s.active = false
    call print_text(concat("clients after stop: ", int_to_text(tcpserver_client_count("s"))))
    call quit()
  end
end

sub on_error(message: text)
  call print_text(concat("error: ", message))
  call quit()
end
"#,
        "leaving",
    );
    let port = free_port();
    let child = start(&bin, port);
    let mut a = connect(port);
    let mut b = connect(port);
    a.write_all(b"kick\n").unwrap();
    // The server closed `a`: the read ends rather than waits.
    assert_eq!(read_until(&mut a, "never"), "");
    b.write_all(b"stop\n").unwrap();
    assert_eq!(read_until(&mut b, "stopping\n"), "stopping\n");
    let (stdout, stderr) = finish(child, "leaving");
    if port_was_busy(&stdout, &stderr) {
        eprintln!("port {port} was busy; skipping");
        return;
    }
    let expected = "connect 1\n\
                    connect 2\n\
                    disconnect 1\n\
                    stale code 10002\n\
                    disconnect 2\n\
                    clients after stop: 0\n";
    assert_eq!(stdout, expected, "stderr:\n{stderr}");
}

/// (e) The listing the language server and the docs are generated from.
#[test]
fn the_listing_shows_the_components_and_their_commands() {
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["commands", "--use", "net"])
        .current_dir(repo())
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln");
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    for line in [
        "command: tcpserver_send(text, int, text) -> bool",
        "command: tcpserver_send_all(text, text) -> int",
        "command: tcpserver_disconnect(text, int) -> bool",
        "command: tcpserver_client_count(text) -> int",
        "command: tcpserver_client_address(text, int) -> text",
        "command: tcpserver_client(text, int) -> int",
        "command: tcpclient_send(text, text) -> bool",
        "command: tcpclient_connect(text) -> bool",
        "command: tcpclient_disconnect(text) -> bool",
        "command: tcpclient_connected(text) -> bool",
        "kind: tcpserver nonvisual",
        "property: tcpserver port int",
        "property: tcpserver active bool",
        "property: tcpserver delimiter text",
        "event: tcpserver connect",
        "event: tcpserver receive",
        "kind: tcpclient nonvisual",
        "property: tcpclient connected bool",
        "property: tcpclient timeout_ms int",
        "event: tcpclient receive",
        "event: tcpclient error",
    ] {
        assert!(text.lines().any(|l| l == line), "missing `{line}` in:\n{text}");
    }
}

/// The chat example is a form and a client together; it must at least build.
#[test]
fn the_chat_example_builds() {
    if !repo().join("vendor/RmlUi/build/librmlui.a").exists() {
        eprintln!("RmlUi not vendored; skipping");
        return;
    }
    build(&repo().join("examples/tcpchat.kiln"), "chat");
}
