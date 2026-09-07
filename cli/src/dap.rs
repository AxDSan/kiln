//! `kiln dap` — the Debug Adapter Protocol, spoken over stdio.
//!
//! This is the surface every editor debugs Kiln through. Studio drives it
//! as a subprocess on a pipe, exactly as it already drives `kiln lsp`, and
//! VS Code's generic DAP client speaks it without a line of adapter code — one
//! server, many editors, which is the whole reason the protocol was chosen
//! over inventing one.
//!
//! The layer is deliberately thin: it translates requests into calls on
//! [`kiln_debug::session::Session`] and translates the answers back. Every
//! decision about *what* a stop means, which frames are the user's, and how a
//! value reads lives in the engine below. What lives here is the protocol, and
//! the protocol has four ways to fail silently that are worth naming, because
//! each produces a session that looks alive and is not:
//!
//! * **DAP is not JSON-RPC.** A response carries the request's sequence number
//!   in `request_seq` and its *own* `seq`, drawn from the adapter's own
//!   counter. Echoing `seq` as `seq` is the mistake anyone arriving from an
//!   LSP server makes, because there echoing the id is exactly right.
//! * **The launch response is held back until `configurationDone`.** The
//!   client sends no breakpoints until it has seen `initialized`, and no
//!   `configurationDone` until it has sent them; answering `launch` straight
//!   away means the client considers configuration over before it began. The
//!   capability that unlocks this (`supportsConfigurationDoneRequest`) has to
//!   be advertised or the request never arrives and the session hangs on a
//!   reply that will never be sent.
//! * **`setBreakpoints` replaces a file's whole set and its answer must keep
//!   the order it was asked in.** The client pairs the two lists up by
//!   position, so returning them sorted by resolved address silently moves
//!   every breakpoint onto a different line in the gutter.
//! * **The debuggee's output never touches this stream.** One stray byte on
//!   the adapter's stdout desynchronises the framing permanently, and no
//!   participant can tell that is what happened. The build's output is
//!   forwarded as `output` events instead, and the program's own pipes belong
//!   to the session.
//!
//! Everything the adapter needs from below sits behind [`Backend`] and
//! [`Debuggee`], so the protocol can be tested end to end with no compiler run
//! and no traced process — which matters here, because the interesting bugs
//! are in the message shapes rather than in the debugging.

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value as Json};

use kiln_debug::session::{Breakpoint, Session, Stopped};
use kiln_debug::step::Step;
use kiln_debug::target::Interrupt;
use kiln_debug::unwind::Frame;
use kiln_debug::value::Value;

/// The one thread the protocol reports.
///
/// A single-threaded program still has a thread as far as DAP is concerned,
/// and a `threads` answer with an empty array leaves the client nothing to
/// attach a stop to — no call stack, no variables, no stepping, and no error
/// to explain any of it. The dummy is mandatory rather than a placeholder.
const THREAD_ID: i64 = 1;

/// Entry point for the `dap` subcommand. Returns a process exit code.
pub fn run() -> i32 {
    // Everything diagnostic goes to stderr. stdout is the protocol channel and
    // a stray `println!` on it corrupts the stream for good.
    eprintln!("kiln-dap: starting on stdio");
    let mut out = Out::new(io::stdout());
    let mut adapter = Adapter::new(Box::new(Cli));

    // Requests are read on their own thread.
    //
    // Handling one means running the program, and running the program means
    // blocking until it stops. Reading on this thread would mean nothing is
    // read while the program runs — so a pause, a stop or a disconnect sent to
    // a program that never stops on its own would never be seen at all, and a
    // form idling in its event loop would wedge the session for good.
    //
    // The thread does one thing besides forwarding: a request that means
    // "stop what you are doing" fires the interrupt handle as it passes, which
    // is what turns the blocked wait on the other thread into a stop.
    let (sender, requests) = std::sync::mpsc::channel();
    let reading = adapter.interrupt_shared();
    std::thread::spawn(move || {
        let stdin = io::stdin();
        let mut input = BufReader::new(stdin.lock());
        loop {
            match read_message(&mut input) {
                Ok(Some(message)) => {
                    if let Some(request) = Request::parse(&message) {
                        if matches!(request.command.as_str(), "pause" | "terminate" | "disconnect")
                        {
                            let handle: Option<Interrupt> =
                                *reading.lock().expect("interrupt handle");
                            if let Some(handle) = handle {
                                handle.stop();
                            }
                        }
                        if sender.send(request).is_err() {
                            break;
                        }
                    }
                }
                // The client closed the pipe. That is how a session normally
                // ends when the editor is shut down rather than stopped, so it
                // is not an error.
                Ok(None) => break,
                Err(e) => {
                    eprintln!("kiln-dap: {e}");
                    break;
                }
            }
        }
    });

    for request in requests {
        adapter.handle(&request, &mut out);
        if adapter.finished || out.broken {
            break;
        }
    }
    // A traced child outlives its tracer unless it is told not to, so the exit
    // path has to stop it even when the client vanished without disconnecting.
    adapter.shutdown();
    eprintln!("kiln-dap: shutting down");
    0
}

// --- Framing ----------------------------------------------------------------

/// Read one `Content-Length`-framed message, or `None` at end of input.
///
/// The framing is LSP's, byte for byte, which is what lets Studio's debug
/// client be `lspclient.h` again rather than something new.
fn read_message(input: &mut impl BufRead) -> io::Result<Option<Json>> {
    let mut length: Option<usize> = None;
    loop {
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        // Header names are case-insensitive, and clients differ on the casing
        // they send.
        let lower = line.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            length = v.trim().parse().ok();
        }
    }
    let Some(length) = length else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "a message arrived with no Content-Length header",
        ));
    };
    // Read exactly the declared number of bytes. Reading to a delimiter would
    // work until the first message containing one.
    let mut body = vec![0u8; length];
    input.read_exact(&mut body)?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Wrap a message in its header.
fn frame(message: &Json) -> Vec<u8> {
    let body = serde_json::to_vec(message).unwrap_or_else(|_| b"{}".to_vec());
    let mut bytes = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    bytes.extend_from_slice(&body);
    bytes
}

// --- Messages ---------------------------------------------------------------

/// A request from the client.
#[derive(Debug, Clone, PartialEq)]
struct Request {
    /// The client's sequence number, which the response echoes in
    /// `request_seq` and nowhere else.
    seq: u64,
    command: String,
    arguments: Json,
}

impl Request {
    /// Recognise a request. Responses and events from the client are not
    /// answers to anything we asked for, so they are ignored rather than
    /// rejected.
    fn parse(message: &Json) -> Option<Request> {
        if message.get("type").and_then(Json::as_str) != Some("request") {
            return None;
        }
        Some(Request {
            seq: message.get("seq").and_then(Json::as_u64).unwrap_or(0),
            command: message.get("command").and_then(Json::as_str)?.to_string(),
            arguments: message
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({})),
        })
    }

    /// One argument, by name.
    fn arg(&self, name: &str) -> Option<&Json> {
        self.arguments.get(name)
    }

    fn flag(&self, name: &str, default: bool) -> bool {
        self.arg(name).and_then(Json::as_bool).unwrap_or(default)
    }
}

/// Where outgoing messages go, and what stamps their sequence numbers.
///
/// The counter lives here rather than on the adapter so that `seq` always
/// reflects the order messages actually left, including the ones a long build
/// emits from inside a request that has not been answered yet.
trait Sink {
    /// Stamp `message` with the next outgoing sequence number and deliver it.
    fn send(&mut self, message: Json);
}

/// The real sink: framed messages on the adapter's stdout.
struct Out<W: Write> {
    writer: W,
    seq: u64,
    /// Set when the client's end of the pipe has gone. There is nobody left to
    /// report an error to, so the loop simply stops.
    broken: bool,
}

impl<W: Write> Out<W> {
    fn new(writer: W) -> Out<W> {
        Out {
            writer,
            // DAP numbers the first message either side sends 1, not 0.
            seq: 1,
            broken: false,
        }
    }
}

impl<W: Write> Sink for Out<W> {
    fn send(&mut self, mut message: Json) {
        if self.broken {
            return;
        }
        message["seq"] = json!(self.seq);
        self.seq += 1;
        // Header and body go out together: a client reading the header and
        // then blocking for a body that is still in our buffer would stall
        // every time.
        if self.writer.write_all(&frame(&message)).is_err() || self.writer.flush().is_err() {
            self.broken = true;
        }
    }
}

/// Build a response, less the `seq` the sink will stamp on it.
fn response(request: &Request, success: bool, body: Option<Json>) -> Json {
    let mut message = json!({
        "type": "response",
        // The request's number goes here and only here.
        "request_seq": request.seq,
        "success": success,
        "command": request.command,
    });
    if let Some(body) = body {
        message["body"] = body;
    }
    message
}

/// Build a failure response, with the reason in the place clients actually
/// display: `body.error.format`. A bare `message` is shown by some clients and
/// swallowed by others, so both are filled in.
fn failure(request: &Request, short: &str, detail: &str) -> Json {
    let mut message = response(request, false, None);
    message["message"] = json!(short);
    message["body"] = json!({
        "error": {
            "id": 1,
            "format": detail,
            "showUser": true,
        }
    });
    message
}

/// Build an event, less its `seq`.
fn event(name: &str, body: Option<Json>) -> Json {
    let mut message = json!({ "type": "event", "event": name });
    if let Some(body) = body {
        message["body"] = body;
    }
    message
}

/// An `output` event. `category` is `"console"` for the adapter's own words
/// and the build's, `"stdout"`/`"stderr"` for the program's — a client renders
/// them differently, and conflating them makes a compiler error look like
/// something the program printed.
fn output(category: &str, text: &str) -> Json {
    event("output", Some(json!({ "category": category, "output": text })))
}

// --- The seam to the engine -------------------------------------------------

/// Where an address is, in the user's source.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Location {
    /// The subroutine's name as the user wrote it, not its mangled symbol.
    function: String,
    /// Absolute where it can be worked out, so the client can open the file.
    source: Option<String>,
    line: u32,
    column: u32,
}

/// A program under control, as the protocol layer needs to see it.
///
/// This is [`kiln_debug::session::Session`] with two differences, and both
/// exist so the protocol can be exercised without a traced process: errors are
/// already rendered to strings, because they end up in a DAP message either
/// way, and address-to-line lookup is a method here rather than a second copy
/// of the symbol table in the adapter.
trait Debuggee {
    /// Replace the breakpoint set. The engine may return these in any order;
    /// putting them back in the order the client asked for is the caller's
    /// job, and it is not optional.
    fn set_breakpoints(&mut self, lines: &[u32]) -> Result<Vec<Breakpoint>, String>;

    fn resume(&mut self) -> Result<Stopped, String>;

    fn step(&mut self, kind: Step) -> Result<Stopped, String>;

    /// The user's frames, innermost first.
    fn stack(&mut self) -> Result<Vec<Frame>, String>;

    fn locals(&mut self, frame: usize) -> Result<Vec<(String, Value)>, String>;

    /// Whatever the program has written since this was last called.
    ///
    /// Drained rather than copied: it is forwarded to the client, and
    /// forwarding the same bytes twice is worse than losing them. It never
    /// reaches the adapter's own stdout — one stray byte there desynchronises
    /// the protocol permanently — which is the whole reason the session holds
    /// the pipe rather than letting the program inherit ours.
    fn program_output(&mut self) -> Vec<u8>;

    /// A way to stop the program from another thread, so a pause that arrives
    /// while it is running is acted on rather than queued behind it.
    fn interrupt_handle(&self) -> Interrupt;

    fn stop(&mut self) -> Result<(), String>;

    /// Which line of which source an address belongs to.
    fn describe(&self, address: u64) -> Option<Location>;

    /// The source the program was built from, for deciding whether a
    /// breakpoint request is about this program at all.
    fn source(&self) -> Option<String>;
}

/// How a program gets built and put under control.
///
/// Separated from [`Debuggee`] because the two happen at different times and
/// fail differently: a build failure is a diagnostic the user must read, and a
/// launch failure is the debugger's own problem.
trait Backend {
    /// Compile `program` to `binary`, forwarding the compiler's output as it
    /// arrives. A build that takes several seconds behind a silent adapter is
    /// indistinguishable from one that has hung.
    ///
    /// The error is the diagnostic itself, to be shown to the user verbatim.
    fn build(
        &mut self,
        program: &Path,
        binary: &Path,
        cwd: Option<&Path>,
        out: &mut dyn Sink,
    ) -> Result<(), String>;

    fn launch(&mut self, binary: &Path, args: &[String]) -> Result<Box<dyn Debuggee>, String>;
}

/// The real backend: this same executable for the build, and the debug engine
/// for the session.
struct Cli;

impl Backend for Cli {
    fn build(
        &mut self,
        program: &Path,
        binary: &Path,
        cwd: Option<&Path>,
        out: &mut dyn Sink,
    ) -> Result<(), String> {
        let exe = std::env::current_exe()
            .map_err(|e| format!("cannot find the Kiln compiler to build with: {e}"))?;
        let mut command = Command::new(exe);
        command
            .arg("build")
            .arg(program)
            .arg("-o")
            .arg(binary)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        let mut child = command
            .spawn()
            .map_err(|e| format!("cannot run the Kiln compiler: {e}"))?;

        let (tx, rx) = std::sync::mpsc::channel::<(bool, String)>();
        let mut readers = Vec::new();
        if let Some(pipe) = child.stdout.take() {
            readers.push(pump(pipe, false, tx.clone()));
        }
        if let Some(pipe) = child.stderr.take() {
            readers.push(pump(pipe, true, tx.clone()));
        }
        // The loop below ends when every sender has gone, so the one held here
        // has to go first or it waits for itself.
        drop(tx);

        // The diagnostic is kept as well as forwarded: the `output` events
        // scroll past, and the launch failure has to carry the reason itself.
        let mut diagnostic = String::new();
        for (is_error, line) in rx {
            if is_error {
                diagnostic.push_str(&line);
                diagnostic.push('\n');
            }
            out.send(output("console", &format!("{line}\n")));
        }
        for reader in readers {
            let _ = reader.join();
        }

        let status = child
            .wait()
            .map_err(|e| format!("the Kiln compiler could not be waited for: {e}"))?;
        if status.success() {
            Ok(())
        } else if diagnostic.trim().is_empty() {
            Err(format!("the build of {} failed", program.display()))
        } else {
            Err(diagnostic.trim_end().to_string())
        }
    }

    fn launch(&mut self, binary: &Path, args: &[String]) -> Result<Box<dyn Debuggee>, String> {
        // The symbol table is loaded here rather than taken from the session,
        // which keeps its own copy but does not expose it. Reading a mapped
        // file twice is cheap; guessing at line numbers is not.
        let program = kiln_debug::load(binary).map_err(|e| e.to_string())?;
        let session = Session::launch(binary, args).map_err(|e| e.to_string())?;
        Ok(Box::new(Live { session, program }))
    }
}

/// Forward one of the compiler's pipes, a line at a time, until it closes.
///
/// Both pipes are drained at once and by separate threads. Reading one to the
/// end and then the other deadlocks the moment the compiler fills the pipe
/// that is not being read, which for a real diagnostic is immediately.
fn pump<R: std::io::Read + Send + 'static>(
    pipe: R,
    is_error: bool,
    tx: std::sync::mpsc::Sender<(bool, String)>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        for line in BufReader::new(pipe).lines().map_while(Result::ok) {
            if tx.send((is_error, line)).is_err() {
                break;
            }
        }
    })
}

/// A live session, plus the symbols needed to describe where it is.
struct Live {
    session: Session,
    program: kiln_debug::Program,
}

impl Debuggee for Live {
    fn set_breakpoints(&mut self, lines: &[u32]) -> Result<Vec<Breakpoint>, String> {
        self.session.set_breakpoints(lines).map_err(|e| e.to_string())
    }

    fn resume(&mut self) -> Result<Stopped, String> {
        self.session.resume().map_err(|e| e.to_string())
    }

    fn step(&mut self, kind: Step) -> Result<Stopped, String> {
        self.session.step(kind).map_err(|e| e.to_string())
    }

    fn stack(&mut self) -> Result<Vec<Frame>, String> {
        self.session.stack().map_err(|e| e.to_string())
    }

    fn locals(&mut self, frame: usize) -> Result<Vec<(String, Value)>, String> {
        self.session.locals(frame).map_err(|e| e.to_string())
    }

    fn stop(&mut self) -> Result<(), String> {
        self.session.stop().map_err(|e| e.to_string())
    }

    fn program_output(&mut self) -> Vec<u8> {
        self.session.output()
    }

    fn interrupt_handle(&self) -> Interrupt {
        self.session.interrupt_handle()
    }

    fn describe(&self, address: u64) -> Option<Location> {
        let row = self.program.line_for(address)?;
        Some(Location {
            function: self
                .program
                .subprogram_for(address)
                .map(|s| s.name.clone())
                .unwrap_or_else(|| "<unknown>".to_string()),
            source: self.source(),
            line: row.line,
            column: row.column,
        })
    }

    fn source(&self) -> Option<String> {
        let source = Path::new(&self.program.source);
        if source.is_absolute() {
            Some(self.program.source.clone())
        } else {
            Some(
                Path::new(&self.program.directory)
                    .join(source)
                    .to_string_lossy()
                    .into_owned(),
            )
        }
    }
}

// --- Values -----------------------------------------------------------------

/// One line describing a value, for the row the client shows collapsed.
///
/// Deliberately short for anything with parts: the parts are separate rows,
/// reached through `variablesReference`, so spelling them out here would say
/// everything twice and make a long array unreadable. That is a different job
/// from `Value`'s own `Display`, which renders a value whole for a terminal —
/// and which this must not call while it is still unimplemented.
fn summary(value: &Value) -> String {
    match value {
        Value::Int(v) => v.to_string(),
        Value::Int64(v) => v.to_string(),
        Value::Double(v) => v.to_string(),
        Value::Bool(v) => if *v { "true" } else { "false" }.to_string(),
        // Quoted, so an empty text and an absent one do not look alike.
        Value::Text(v) => format!("{v:?}"),
        Value::Nothing => "nothing".to_string(),
        Value::Array(items) => match items.len() {
            0 => "[]".to_string(),
            1 => "[1 item]".to_string(),
            n => format!("[{n} items]"),
        },
        Value::Record { name, fields } => {
            if fields.is_empty() {
                format!("{name} {{}}")
            } else {
                format!("{name} {{ … }}")
            }
        }
        Value::Dict(pairs) => match pairs.len() {
            0 => "{}".to_string(),
            1 => "{1 entry}".to_string(),
            n => format!("{{{n} entries}}"),
        },
        // Shown rather than hidden: a blank row cannot be told from a bug.
        Value::Unreadable(why) => format!("<unreadable: {why}>"),
    }
}

/// The rows a value expands into, or `None` when it is a leaf.
///
/// Array indices are 1-based, because that is what an index means in this
/// language. A debugger that shows `[0]` for the first element is describing a
/// different program from the one the user wrote.
fn children(value: &Value) -> Option<Vec<(String, Value)>> {
    match value {
        Value::Array(items) if !items.is_empty() => Some(
            items
                .iter()
                .enumerate()
                .map(|(i, v)| (format!("[{}]", i + 1), v.clone()))
                .collect(),
        ),
        Value::Record { fields, .. } if !fields.is_empty() => Some(fields.clone()),
        Value::Dict(pairs) if !pairs.is_empty() => Some(pairs.clone()),
        _ => None,
    }
}

/// What a `variablesReference` points at.
#[derive(Debug, Clone)]
enum Node {
    /// A frame's locals, read only when the client expands the scope.
    Locals(usize),
    /// The parts of a value that has already been read.
    Parts(Vec<(String, Value)>),
}

// --- The adapter ------------------------------------------------------------

/// The protocol state machine.
struct Adapter {
    backend: Box<dyn Backend>,
    debuggee: Option<Box<dyn Debuggee>>,
    /// Whether the client counts the first line 1 or 0. It says so at
    /// `initialize`, and every line crossing the boundary is converted there
    /// rather than passed through — the same discipline the language server
    /// applies to UTF-16 columns.
    lines_start_at_1: bool,
    columns_start_at_1: bool,
    /// The sequence number of a `launch` that has been accepted and not yet
    /// answered. It is answered when `configurationDone` arrives.
    pending_launch: Option<u64>,
    /// Held back until the program is configured, because starting it before
    /// the client has sent its breakpoints runs straight past all of them.
    stop_on_entry: bool,
    /// Live `variablesReference`s. Cleared at every stop: the protocol says a
    /// reference is only valid until the program moves, and holding stale ones
    /// would show the user the previous stop's values.
    variables: HashMap<i64, Node>,
    next_reference: i64,
    /// Ids for breakpoints the engine could not place. Started well above
    /// anything the engine hands out, so the two ranges cannot meet.
    next_unbound: i64,
    /// Shared with whoever reads requests, so a pause arriving while the
    /// program runs can stop it. Published the moment there is a program,
    /// which must be before anything blocks waiting for one.
    interrupt: Arc<Mutex<Option<Interrupt>>>,
    /// Set once the program has exited, so `disconnect` does not try to stop a
    /// process that is already gone.
    exited: bool,
    /// Set by `disconnect`, which is the one request that ends the loop.
    finished: bool,
}

impl Adapter {
    fn new(backend: Box<dyn Backend>) -> Adapter {
        Adapter {
            backend,
            debuggee: None,
            // The protocol's defaults, used until `initialize` says otherwise.
            lines_start_at_1: true,
            columns_start_at_1: true,
            pending_launch: None,
            stop_on_entry: false,
            variables: HashMap::new(),
            next_reference: 1,
            next_unbound: 1 << 20,
            interrupt: Arc::new(Mutex::new(None)),
            exited: false,
            finished: false,
        }
    }

    /// Handle one request, emitting everything it produces through `out`.
    fn handle(&mut self, request: &Request, out: &mut dyn Sink) {
        match request.command.as_str() {
            "initialize" => self.on_initialize(request, out),
            "launch" => self.on_launch(request, out),
            "setBreakpoints" => self.on_set_breakpoints(request, out),
            "configurationDone" => self.on_configuration_done(request, out),
            "threads" => self.on_threads(request, out),
            "stackTrace" => self.on_stack_trace(request, out),
            "scopes" => self.on_scopes(request, out),
            "variables" => self.on_variables(request, out),
            "continue" => self.on_continue(request, out),
            "next" => self.on_step(request, Step::Over, out),
            "stepIn" => self.on_step(request, Step::In, out),
            "stepOut" => self.on_step(request, Step::Out, out),
            "pause" => self.on_pause(request, out),
            "evaluate" => self.on_evaluate(request, out),
            "terminate" => self.on_terminate(request, out),
            "disconnect" => self.on_disconnect(request, out),
            other => {
                let detail = format!("`{other}` is not something this debugger can do");
                out.send(failure(request, "unsupported request", &detail));
            }
        }
    }

    /// Stop the program if it is still running. Called on every exit path,
    /// including the one where the client vanished.
    /// The handle for stopping the program, when there is one to stop.
    /// An id for a breakpoint the engine could not place.
    ///
    /// Drawn from above everything the engine hands out rather than from the
    /// same 1..n space: two breakpoints alive at once would otherwise share an
    /// id, and `hitBreakpointIds` is the client's only way of saying which one
    /// fired.
    fn unbound_id(&mut self) -> i64 {
        self.next_unbound += 1;
        self.next_unbound
    }

    /// The shared slot holding the way to stop the program, for whoever reads
    /// requests on another thread.
    fn interrupt_shared(&self) -> Arc<Mutex<Option<Interrupt>>> {
        Arc::clone(&self.interrupt)
    }

    fn shutdown(&mut self) {
        if let Some(debuggee) = self.debuggee.as_mut() {
            if !self.exited {
                let _ = debuggee.stop();
            }
        }
        self.debuggee = None;
    }

    // --- Line and column conversion ---------------------------------------

    /// A line as the client counts them.
    fn client_line(&self, line: u32) -> i64 {
        if self.lines_start_at_1 {
            line as i64
        } else {
            line as i64 - 1
        }
    }

    fn client_column(&self, column: u32) -> i64 {
        if self.columns_start_at_1 {
            column as i64
        } else {
            column as i64 - 1
        }
    }

    /// A line as the debug information counts them, which is always from 1.
    fn engine_line(&self, line: i64) -> u32 {
        let line = if self.lines_start_at_1 { line } else { line + 1 };
        line.max(0) as u32
    }

    // --- Requests -----------------------------------------------------------

    fn on_initialize(&mut self, request: &Request, out: &mut dyn Sink) {
        self.lines_start_at_1 = request.flag("linesStartAt1", true);
        self.columns_start_at_1 = request.flag("columnsStartAt1", true);
        let body = json!({
            // Without this the client never sends `configurationDone`, and the
            // launch response that waits for it is never sent either. It is
            // the one capability this adapter cannot work without.
            "supportsConfigurationDoneRequest": true,
            // Studio's hover tips and VS Code's are the same request with
            // `context: "hover"`, and a client will not send it unasked.
            "supportsEvaluateForHovers": true,
            "supportsTerminateRequest": true,
            // Named so the client stops offering what the engine cannot do,
            // rather than offering it and failing.
            "supportsStepBack": false,
            "supportsSetVariable": false,
            "supportsRestartRequest": false,
            "supportsFunctionBreakpoints": false,
            "supportsConditionalBreakpoints": false,
            "exceptionBreakpointFilters": [],
        });
        out.send(response(request, true, Some(body)));
    }

    fn on_launch(&mut self, request: &Request, out: &mut dyn Sink) {
        // A second launch is refused rather than allowed to replace the first.
        // Taking it would overwrite the outstanding request's sequence number
        // — leaving the client waiting on a reply that can never be sent — and
        // drop a traced program without stopping it.
        if self.pending_launch.is_some() || self.debuggee.is_some() {
            out.send(failure(
                request,
                "already debugging",
                "this session already has a program. Disconnect before launching another.",
            ));
            return;
        }
        let Some(program) = request.arg("program").and_then(Json::as_str) else {
            out.send(failure(
                request,
                "nothing to debug",
                "`launch` needs a `program`: the path of the .kiln file to debug",
            ));
            out.send(event("terminated", None));
            return;
        };
        let program = PathBuf::from(program);
        let cwd = request
            .arg("cwd")
            .and_then(Json::as_str)
            .map(PathBuf::from);
        let binary = match request.arg("binary").and_then(Json::as_str) {
            Some(path) => PathBuf::from(path),
            None => default_binary(&program),
        };
        let args: Vec<String> = request
            .arg("args")
            .and_then(Json::as_array)
            .map(|a| {
                a.iter()
                    .map(|v| match v.as_str() {
                        Some(s) => s.to_string(),
                        None => v.to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.stop_on_entry = request.flag("stopOnEntry", false);

        // Re-running an unchanged program should not pay for a compile, so the
        // client can skip it. Everything else about the launch is the same.
        if !request.flag("noBuild", false) {
            out.send(output(
                "console",
                &format!("Building {}…\n", program.display()),
            ));
            if let Err(diagnostic) = self
                .backend
                .build(&program, &binary, cwd.as_deref(), out)
            {
                out.send(failure(request, "the build failed", &diagnostic));
                // The session is over before it began, and a client that is
                // not told so leaves its debug toolbar enabled for good.
                out.send(event("terminated", None));
                return;
            }
        }

        match self.backend.launch(&binary, &args) {
            Ok(debuggee) => {
                // Published before anything can block on the program: the
                // resume that follows does not return until it stops, and a
                // pause handle that arrives after that is a pause that can
                // never be acted on.
                *self.interrupt.lock().expect("interrupt handle") =
                    Some(debuggee.interrupt_handle());
                self.debuggee = Some(debuggee);
            }
            Err(e) => {
                out.send(failure(request, "the program could not be started", &e));
                out.send(event("terminated", None));
                return;
            }
        }

        // `initialized` says the adapter is ready to be configured, and it is
        // sent here rather than after `initialize` so that every breakpoint
        // the client sends arrives with a session to bind it to. Sent earlier,
        // breakpoints would be answered unverified and would need a later
        // `breakpoint` event to correct — and a gutter full of hollow dots is
        // exactly the lie the design set out to avoid.
        out.send(event("initialized", None));
        self.pending_launch = Some(request.seq);
    }

    fn on_set_breakpoints(&mut self, request: &Request, out: &mut dyn Sink) {
        let path = request
            .arg("source")
            .and_then(|s| s.get("path"))
            .and_then(Json::as_str)
            .map(PathBuf::from);
        // `breakpoints` is what every current client sends; `lines` is the
        // deprecated form, still emitted by older ones.
        let requested: Vec<u32> = match request.arg("breakpoints").and_then(Json::as_array) {
            Some(list) => list
                .iter()
                .map(|b| self.engine_line(b.get("line").and_then(Json::as_i64).unwrap_or(0)))
                .collect(),
            None => request
                .arg("lines")
                .and_then(Json::as_array)
                .map(|list| {
                    list.iter()
                        .map(|l| self.engine_line(l.as_i64().unwrap_or(0)))
                        .collect()
                })
                .unwrap_or_default(),
        };

        // A breakpoint that cannot bind is reported unverified rather than
        // dropped — the client draws the difference — and it is told which of
        // the two reasons applies, because a breakpoint that explains itself
        // wrongly is only marginally better than one that says nothing.
        let unbound = match self.debuggee.as_ref() {
            None => Some("there is no program yet, so this has not been placed"),
            // The engine compiles one source, so a breakpoint anywhere else
            // has nothing to attach to.
            Some(debuggee) if !source_matches(debuggee.source().as_deref(), path.as_deref()) => {
                Some("this program has no code from this file")
            }
            Some(_) => None,
        };
        let bound = match (unbound, self.debuggee.as_mut()) {
            (None, Some(debuggee)) => match debuggee.set_breakpoints(&requested) {
                Ok(bound) => bound,
                Err(e) => {
                    out.send(failure(request, "the breakpoints were refused", &e));
                    return;
                }
            },
            _ => Vec::new(),
        };

        // The client pairs its request up with this answer by position, so the
        // answer is rebuilt from the *request* and the engine's list is only
        // consulted for what each line resolved to. Handing back the engine's
        // own ordering — which is by address, and for good reason — would move
        // every breakpoint onto somebody else's line.
        let mut taken = vec![false; bound.len()];
        let mut answers = Vec::with_capacity(requested.len());
        for (_index, line) in requested.iter().enumerate() {
            let matched = bound
                .iter()
                .enumerate()
                .find(|(i, b)| !taken[*i] && b.requested_line == *line);
            match matched {
                Some((i, b)) => {
                    taken[i] = true;
                    let mut answer = json!({
                        "id": b.id,
                        "verified": b.address.is_some(),
                        "line": self.client_line(b.line),
                    });
                    if let Some(path) = path.as_deref() {
                        answer["source"] = json!({ "path": path.to_string_lossy() });
                    }
                    if b.address.is_none() {
                        answer["message"] =
                            json!("nothing runs on this line, so no breakpoint could be placed");
                    } else if b.line != b.requested_line {
                        // A breakpoint that quietly moved is a breakpoint that
                        // lies about where the program will stop.
                        answer["message"] = json!(format!(
                            "line {} runs nothing; the breakpoint moved to line {}",
                            b.requested_line, b.line
                        ));
                    }
                    answers.push(answer);
                }
                None => answers.push(json!({
                    // From the adapter's own range, never the engine's: a
                    // breakpoint that did not bind is still a distinct
                    // breakpoint as far as the client is concerned.
                    "id": self.unbound_id(),
                    "verified": false,
                    "line": self.client_line(*line),
                    "message": unbound.unwrap_or("this breakpoint could not be placed"),
                })),
            }
        }
        out.send(response(
            request,
            true,
            Some(json!({ "breakpoints": answers })),
        ));
    }

    fn on_configuration_done(&mut self, request: &Request, out: &mut dyn Sink) {
        out.send(response(request, true, None));
        // Only now is the launch complete, as far as the client is concerned.
        if let Some(seq) = self.pending_launch.take() {
            out.send(json!({
                "type": "response",
                "request_seq": seq,
                "success": true,
                "command": "launch",
            }));
        }
        if self.debuggee.is_none() {
            return;
        }
        if self.stop_on_entry {
            out.send(self.stopped_event("entry", None, None));
            return;
        }
        // The program starts running of the adapter's own accord rather than
        // in answer to a request, which is the one case the protocol asks for
        // a `continued` event.
        out.send(event(
            "continued",
            Some(json!({ "threadId": THREAD_ID, "allThreadsContinued": true })),
        ));
        self.run(None, out);
    }

    fn on_threads(&mut self, request: &Request, out: &mut dyn Sink) {
        // Always exactly one, even before a launch and even after an exit. An
        // empty array leaves the client with no thread to hang a stop on, and
        // the symptom is a debugger that stops and shows nothing.
        out.send(response(
            request,
            true,
            Some(json!({ "threads": [{ "id": THREAD_ID, "name": "main" }] })),
        ));
    }

    fn on_stack_trace(&mut self, request: &Request, out: &mut dyn Sink) {
        let Some(debuggee) = self.debuggee.as_mut() else {
            out.send(failure(
                request,
                "not running",
                "there is no program to take a stack from",
            ));
            return;
        };
        let frames = match debuggee.stack() {
            Ok(frames) => frames,
            Err(e) => {
                out.send(failure(request, "the stack could not be read", &e));
                return;
            }
        };
        let described: Vec<Option<Location>> = frames
            .iter()
            .enumerate()
            .map(|(index, frame)| {
                // Above the innermost frame the saved program counter is the
                // *return* address, which is the instruction after the call
                // and can belong to the following line — or, for a call in
                // tail position, to the following function. One byte back is
                // inside the call itself.
                let address = if index == 0 {
                    frame.registers.pc
                } else {
                    frame.registers.pc.saturating_sub(1)
                };
                debuggee.describe(address)
            })
            .collect();

        let start = request
            .arg("startFrame")
            .and_then(Json::as_u64)
            .unwrap_or(0) as usize;
        let levels = request.arg("levels").and_then(Json::as_u64).unwrap_or(0) as usize;
        let end = if levels == 0 {
            described.len()
        } else {
            (start + levels).min(described.len())
        };

        let mut rendered = Vec::new();
        let first = start.min(described.len());
        for (offset, described) in described[first..end].iter().enumerate() {
            let index = first + offset;
            let location = described.as_ref();
            let mut entry = json!({
                // Frame ids are 1-based so that none of them is 0, which some
                // clients use to mean "no frame".
                "id": index as i64 + 1,
                "name": location.map_or("<unknown>", |l| l.function.as_str()),
                "line": location.map_or(0, |l| self.client_line(l.line)),
                "column": location.map_or(0, |l| self.client_column(l.column)),
            });
            if let Some(path) = location.and_then(|l| l.source.clone()) {
                let name = Path::new(&path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.clone());
                entry["source"] = json!({ "name": name, "path": path });
            }
            rendered.push(entry);
        }
        out.send(response(
            request,
            true,
            Some(json!({
                "stackFrames": rendered,
                // The whole depth, not the slice's — the client uses it to
                // decide whether to ask for more.
                "totalFrames": described.len(),
            })),
        ));
    }

    fn on_scopes(&mut self, request: &Request, out: &mut dyn Sink) {
        let frame_id = request.arg("frameId").and_then(Json::as_i64).unwrap_or(1);
        let index = (frame_id.max(1) - 1) as usize;
        let reference = self.reference_for(Node::Locals(index));
        out.send(response(
            request,
            true,
            Some(json!({
                "scopes": [{
                    "name": "Locals",
                    // The hint is what makes an editor open this scope by
                    // default instead of leaving it folded.
                    "presentationHint": "locals",
                    "variablesReference": reference,
                    // Reading a frame's locals costs a few memory reads, so
                    // there is no reason for the client to wait to be asked.
                    "expensive": false,
                }]
            })),
        ));
    }

    fn on_variables(&mut self, request: &Request, out: &mut dyn Sink) {
        let reference = request
            .arg("variablesReference")
            .and_then(Json::as_i64)
            .unwrap_or(0);
        let Some(node) = self.variables.get(&reference).cloned() else {
            out.send(failure(
                request,
                "no such variables",
                "that reference belongs to an earlier stop and is no longer valid",
            ));
            return;
        };
        let entries = match node {
            Node::Parts(parts) => parts,
            Node::Locals(index) => {
                let Some(debuggee) = self.debuggee.as_mut() else {
                    out.send(failure(
                        request,
                        "not running",
                        "there is no program to read variables from",
                    ));
                    return;
                };
                match debuggee.locals(index) {
                    Ok(locals) => locals,
                    Err(e) => {
                        out.send(failure(request, "the variables could not be read", &e));
                        return;
                    }
                }
            }
        };
        let rendered: Vec<Json> = entries
            .iter()
            .map(|(name, value)| self.render_variable(name, value))
            .collect();
        out.send(response(
            request,
            true,
            Some(json!({ "variables": rendered })),
        ));
    }

    fn on_continue(&mut self, request: &Request, out: &mut dyn Sink) {
        if self.debuggee.is_none() {
            out.send(failure(
                request,
                "not running",
                "there is no program to continue",
            ));
            return;
        }
        // Answered before the program is let go, because the stop that follows
        // must not reach the client ahead of the answer to the request that
        // caused it: a client seeing them out of order attributes the stop to
        // whatever it thought was happening before.
        out.send(response(
            request,
            true,
            Some(json!({ "allThreadsContinued": true })),
        ));
        self.run(None, out);
    }

    fn on_step(&mut self, request: &Request, kind: Step, out: &mut dyn Sink) {
        if self.debuggee.is_none() {
            out.send(failure(request, "not running", "there is no program to step"));
            return;
        }
        out.send(response(request, true, None));
        self.run(Some(kind), out);
    }

    fn on_pause(&mut self, request: &Request, out: &mut dyn Sink) {
        if self.debuggee.is_none() {
            out.send(failure(request, "not running", "there is no program to pause"));
            return;
        }
        // Answered, and nothing more. The thread that reads requests has
        // already stopped the program — that is what makes a pause work while
        // it is running — and the run that was blocked has already reported
        // the stop it came back with. Announcing a second one here would tell
        // the client the program stopped twice, and it would draw the second
        // stop over whatever the user had already begun looking at.
        out.send(response(request, true, None));
    }

    fn on_evaluate(&mut self, request: &Request, out: &mut dyn Sink) {
        let expression = request
            .arg("expression")
            .and_then(Json::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let frame_id = request.arg("frameId").and_then(Json::as_i64).unwrap_or(1);
        let index = (frame_id.max(1) - 1) as usize;
        let Some(debuggee) = self.debuggee.as_mut() else {
            out.send(failure(
                request,
                "not running",
                "there is nothing to evaluate against until the program stops",
            ));
            return;
        };
        let locals = match debuggee.locals(index) {
            Ok(locals) => locals,
            Err(e) => {
                out.send(failure(request, "the variables could not be read", &e));
                return;
            }
        };
        // Only a name is understood. That is enough for hovering a variable,
        // which is what this is for and the most-asked-for behaviour in the
        // material this language comes from; anything else is refused rather
        // than guessed at, because a wrong value is worse than none.
        match locals.iter().find(|(name, _)| *name == expression) {
            Some((name, value)) => {
                let rendered = self.render_variable(name, value);
                out.send(response(
                    request,
                    true,
                    Some(json!({
                        "result": rendered["value"],
                        "variablesReference": rendered["variablesReference"],
                    })),
                ));
            }
            None => {
                let detail = format!("there is no `{expression}` here");
                out.send(failure(request, "not a name in this frame", &detail));
            }
        }
    }

    fn on_terminate(&mut self, request: &Request, out: &mut dyn Sink) {
        // This ends the program, not the conversation: the client sends
        // `disconnect` afterwards, and ending the loop here would take the
        // pipe out from under that request.
        self.shutdown();
        out.send(response(request, true, None));
        if !self.exited {
            self.exited = true;
            out.send(event("terminated", None));
        }
    }

    fn on_disconnect(&mut self, request: &Request, out: &mut dyn Sink) {
        // A launch still waiting on `configurationDone` has to be answered
        // first, or the client sits on a reply that is never coming.
        if let Some(seq) = self.pending_launch.take() {
            out.send(json!({
                "type": "response",
                "request_seq": seq,
                "success": false,
                "command": "launch",
                "message": "cancelled",
                "body": { "error": {
                    "id": 2,
                    "format": "the session was disconnected before it was configured",
                    "showUser": false,
                }},
            }));
        }
        self.shutdown();
        out.send(response(request, true, None));
        self.finished = true;
    }

    // --- Running ------------------------------------------------------------

    /// Let the program go — resuming, or taking one step — and report where it
    /// ended up.
    fn run(&mut self, step: Option<Step>, out: &mut dyn Sink) {
        let Some(debuggee) = self.debuggee.as_mut() else {
            return;
        };
        let outcome = match step {
            Some(kind) => debuggee.step(kind),
            None => debuggee.resume(),
        };
        // References describe where the program was, so they stop meaning
        // anything the moment it moves.
        self.variables.clear();
        // What the program printed while it ran, before the stop is announced:
        // a client that draws its console on a stop should already have it.
        if let Some(d) = self.debuggee.as_mut() {
            let written = d.program_output();
            if !written.is_empty() {
                out.send(output("stdout", &String::from_utf8_lossy(&written)));
            }
        }
        match outcome {
            Ok(Stopped::Breakpoint(id)) => {
                out.send(self.stopped_event("breakpoint", None, Some(id)))
            }
            Ok(Stopped::Step) => out.send(self.stopped_event("step", None, None)),
            Ok(Stopped::Pause) => out.send(self.stopped_event("pause", None, None)),
            Ok(Stopped::RuntimeError(message)) => {
                // Caught before the runtime exits, so the stack is still whole
                // and the user can look at it.
                out.send(self.stopped_event("exception", Some(&message), None))
            }
            Ok(Stopped::Exited(code)) => {
                self.exited = true;
                self.debuggee = None;
                out.send(event("exited", Some(json!({ "exitCode": code }))));
                out.send(event("terminated", None));
            }
            Err(e) => {
                out.send(output("important", &format!("{e}\n")));
                self.exited = true;
                self.debuggee = None;
                out.send(event("terminated", None));
            }
        }
    }

    fn stopped_event(&self, reason: &str, description: Option<&str>, hit: Option<u32>) -> Json {
        let mut body = json!({
            "reason": reason,
            "threadId": THREAD_ID,
            // Every thread is stopped whenever any of them is: reading a local
            // while another thread writes it gives a torn value reported as a
            // fact, so the engine stops them all and this says so truthfully.
            "allThreadsStopped": true,
        });
        if let Some(description) = description {
            body["description"] = json!(description);
            // `text` is what a client shows in the exception pop-up;
            // `description` is what it puts in the call-stack row. Clients
            // differ about which they use, so both carry the message.
            body["text"] = json!(description);
        }
        if let Some(id) = hit {
            body["hitBreakpointIds"] = json!([id]);
        }
        event("stopped", Some(body))
    }

    // --- Variables ----------------------------------------------------------

    /// Hand out a reference for something the client can expand.
    fn reference_for(&mut self, node: Node) -> i64 {
        let reference = self.next_reference;
        self.next_reference += 1;
        self.variables.insert(reference, node);
        reference
    }

    /// One `variables` row.
    fn render_variable(&mut self, name: &str, value: &Value) -> Json {
        // A leaf must say 0, not omit the field: a non-zero reference is the
        // client's signal that there is something to expand, and an expandable
        // row that expands to nothing looks like a broken debugger.
        let reference = match children(value) {
            Some(parts) => self.reference_for(Node::Parts(parts)),
            None => 0,
        };
        json!({
            "name": name,
            "value": summary(value),
            "variablesReference": reference,
        })
    }
}

/// Where a build goes when the client did not say.
///
/// The same place `kiln build` would put it: the source with its extension
/// taken off. A name that already has no extension gets one rather than
/// overwriting the source.
fn default_binary(program: &Path) -> PathBuf {
    if program.extension().is_some() {
        program.with_extension("")
    } else {
        program.with_extension("bin")
    }
}

/// Whether a breakpoint request is about the source this program was built
/// from.
///
/// Compared by file name as well as by whole path, because the client sends
/// the path it opened the file by and the debug information carries the one
/// the compiler was given, and the two agree only by luck.
fn source_matches(program: Option<&str>, requested: Option<&Path>) -> bool {
    let (Some(program), Some(requested)) = (program, requested) else {
        // With nothing to compare, the program's own source is the only one
        // there is, so the request is taken at face value.
        return true;
    };
    let program = Path::new(program);
    if program == requested {
        return true;
    }
    match (program.file_name(), requested.file_name()) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiln_debug::unwind::Registers;
    use std::collections::VecDeque;

    /// A sink that keeps what it was sent, so a test can look at the exact
    /// JSON that would have gone down the pipe.
    #[derive(Default)]
    struct Recorder {
        messages: Vec<Json>,
        seq: u64,
    }

    impl Recorder {
        fn new() -> Recorder {
            Recorder {
                messages: Vec::new(),
                seq: 1,
            }
        }

        /// The first response to `command`, or a panic naming what did arrive.
        fn response(&self, command: &str) -> &Json {
            self.messages
                .iter()
                .find(|m| m["type"] == "response" && m["command"] == command)
                .unwrap_or_else(|| panic!("no response to `{command}` in {:#?}", self.messages))
        }

        fn find_event(&self, name: &str) -> Option<&Json> {
            self.messages
                .iter()
                .find(|m| m["type"] == "event" && m["event"] == name)
        }

        /// Where a message sits in the stream, for asserting on ordering.
        fn position_of_response(&self, command: &str) -> usize {
            self.messages
                .iter()
                .position(|m| m["type"] == "response" && m["command"] == command)
                .unwrap_or_else(|| panic!("no response to `{command}`"))
        }
    }

    impl Sink for Recorder {
        fn send(&mut self, mut message: Json) {
            message["seq"] = json!(self.seq);
            self.seq += 1;
            self.messages.push(message);
        }
    }

    /// A program under control that never was one.
    #[derive(Default)]
    struct FakeDebuggee {
        /// What each resume or step reports, in order.
        outcomes: VecDeque<Result<Stopped, String>>,
        frames: Vec<Frame>,
        locals: Vec<(String, Value)>,
        source: Option<String>,
        /// Every line the adapter asked for, for checking what was passed
        /// through the line-base conversion. Shared rather than owned, because
        /// the fake goes into a box the test cannot reach back into — and a
        /// field the test cannot read is scaffolding that implies coverage
        /// which does not exist.
        asked: Arc<Mutex<Vec<u32>>>,
        /// What the program has written and the adapter has not yet forwarded.
        printed: Vec<u8>,
    }

    impl Debuggee for FakeDebuggee {
        fn interrupt_handle(&self) -> Interrupt {
            Interrupt::none()
        }

        fn program_output(&mut self) -> Vec<u8> {
            std::mem::take(&mut self.printed)
        }

        fn set_breakpoints(&mut self, lines: &[u32]) -> Result<Vec<Breakpoint>, String> {
            *self.asked.lock().expect("asked") = lines.to_vec();
            let mut bound: Vec<Breakpoint> = lines
                .iter()
                .enumerate()
                .map(|(i, line)| Breakpoint {
                    id: i as u32 + 1,
                    requested_line: *line,
                    line: *line,
                    address: Some(0x400000 + u64::from(*line) * 4),
                })
                .collect();
            // Deliberately not the order they were asked in. A real engine
            // resolves addresses and has every reason to hand them back
            // sorted; the adapter is what must undo it.
            bound.sort_by_key(|b| b.address);
            Ok(bound)
        }

        fn resume(&mut self) -> Result<Stopped, String> {
            self.outcomes
                .pop_front()
                .unwrap_or(Ok(Stopped::Exited(0)))
        }

        fn step(&mut self, _kind: Step) -> Result<Stopped, String> {
            self.outcomes.pop_front().unwrap_or(Ok(Stopped::Step))
        }

        fn stack(&mut self) -> Result<Vec<Frame>, String> {
            Ok(self.frames.clone())
        }

        fn locals(&mut self, _frame: usize) -> Result<Vec<(String, Value)>, String> {
            Ok(self.locals.clone())
        }

        fn stop(&mut self) -> Result<(), String> {
            Ok(())
        }

        fn describe(&self, address: u64) -> Option<Location> {
            Some(Location {
                function: format!("f{address:#x}"),
                source: self.source.clone(),
                line: (address & 0xff) as u32,
                column: 3,
            })
        }

        fn source(&self) -> Option<String> {
            self.source.clone()
        }
    }

    /// A backend whose build and launch do what the test tells them to.
    struct FakeBackend {
        build_error: Option<String>,
        debuggee: Option<FakeDebuggee>,
        built: bool,
    }

    impl FakeBackend {
        fn with(debuggee: FakeDebuggee) -> FakeBackend {
            FakeBackend {
                build_error: None,
                debuggee: Some(debuggee),
                built: false,
            }
        }

        fn failing(diagnostic: &str) -> FakeBackend {
            FakeBackend {
                build_error: Some(diagnostic.to_string()),
                debuggee: None,
                built: false,
            }
        }
    }

    impl Backend for FakeBackend {
        fn build(
            &mut self,
            _program: &Path,
            _binary: &Path,
            _cwd: Option<&Path>,
            out: &mut dyn Sink,
        ) -> Result<(), String> {
            self.built = true;
            out.send(output("console", "kiln: compiling\n"));
            match &self.build_error {
                Some(e) => Err(e.clone()),
                None => Ok(()),
            }
        }

        fn launch(&mut self, _binary: &Path, _args: &[String]) -> Result<Box<dyn Debuggee>, String> {
            match self.debuggee.take() {
                Some(d) => Ok(Box::new(d)),
                None => Err("no program".to_string()),
            }
        }
    }

    fn request(seq: u64, command: &str, arguments: Json) -> Request {
        Request {
            seq,
            command: command.to_string(),
            arguments,
        }
    }

    /// An adapter already past `initialize` and `launch`, stopped and waiting.
    fn started(debuggee: FakeDebuggee) -> (Adapter, Recorder) {
        let mut adapter = Adapter::new(Box::new(FakeBackend::with(debuggee)));
        let mut out = Recorder::new();
        adapter.handle(&request(1, "initialize", json!({})), &mut out);
        adapter.handle(
            &request(2, "launch", json!({ "program": "/tmp/x.kiln", "stopOnEntry": true })),
            &mut out,
        );
        adapter.handle(&request(3, "configurationDone", json!({})), &mut out);
        out.messages.clear();
        (adapter, out)
    }

    #[test]
    fn a_response_echoes_the_request_seq_and_carries_its_own() {
        let mut adapter = Adapter::new(Box::new(FakeBackend::with(FakeDebuggee::default())));
        let mut out = Recorder::new();
        adapter.handle(&request(42, "initialize", json!({})), &mut out);

        let reply = out.response("initialize");
        assert_eq!(reply["type"], "response");
        assert_eq!(reply["request_seq"], 42, "the request's number goes here");
        assert_eq!(reply["seq"], 1, "and the adapter's own counter here");
        assert_eq!(reply["success"], true);
        assert_eq!(reply["command"], "initialize");
        // Without this the client never sends configurationDone and the
        // deferred launch response is never sent either.
        assert_eq!(reply["body"]["supportsConfigurationDoneRequest"], true);
    }

    #[test]
    fn seq_counts_responses_and_events_together() {
        let mut adapter = Adapter::new(Box::new(FakeBackend::with(FakeDebuggee::default())));
        let mut out = Recorder::new();
        adapter.handle(&request(1, "initialize", json!({})), &mut out);
        adapter.handle(&request(2, "threads", json!({})), &mut out);
        adapter.handle(&request(3, "threads", json!({})), &mut out);

        let seqs: Vec<u64> = out
            .messages
            .iter()
            .map(|m| m["seq"].as_u64().unwrap())
            .collect();
        assert_eq!(seqs, vec![1, 2, 3]);
    }

    #[test]
    fn launch_is_not_answered_until_configuration_done() {
        let mut adapter = Adapter::new(Box::new(FakeBackend::with(FakeDebuggee::default())));
        let mut out = Recorder::new();
        adapter.handle(&request(1, "initialize", json!({})), &mut out);
        adapter.handle(
            &request(2, "launch", json!({ "program": "/tmp/x.kiln", "stopOnEntry": true })),
            &mut out,
        );

        assert!(
            !out.messages
                .iter()
                .any(|m| m["type"] == "response" && m["command"] == "launch"),
            "launch must wait: {:#?}",
            out.messages
        );
        assert!(
            out.find_event("initialized").is_some(),
            "the client is told to send its configuration"
        );

        adapter.handle(&request(3, "configurationDone", json!({})), &mut out);
        let launch = out.response("launch");
        assert_eq!(launch["request_seq"], 2, "answered by number, out of band");
        assert_eq!(launch["success"], true);
        assert!(
            out.position_of_response("configurationDone") < out.position_of_response("launch"),
            "configurationDone is answered first"
        );
    }

    #[test]
    fn set_breakpoints_answers_in_the_order_it_was_asked() {
        let (mut adapter, mut out) = started(FakeDebuggee {
            source: Some("/tmp/x.kiln".to_string()),
            ..Default::default()
        });
        adapter.handle(
            &request(
                10,
                "setBreakpoints",
                json!({
                    "source": { "path": "/tmp/x.kiln" },
                    "breakpoints": [{ "line": 30 }, { "line": 10 }, { "line": 20 }],
                }),
            ),
            &mut out,
        );

        let reply = out.response("setBreakpoints");
        let bound = reply["body"]["breakpoints"].as_array().unwrap();
        let lines: Vec<i64> = bound.iter().map(|b| b["line"].as_i64().unwrap()).collect();
        assert_eq!(
            lines,
            vec![30, 10, 20],
            "the client matches these up by position, not by line"
        );
        let ids: Vec<i64> = bound.iter().map(|b| b["id"].as_i64().unwrap()).collect();
        assert_eq!(ids, vec![1, 2, 3], "each answer keeps its own breakpoint");
        assert!(bound.iter().all(|b| b["verified"] == true));
    }

    #[test]
    fn set_breakpoints_replaces_rather_than_adds() {
        let (mut adapter, mut out) = started(FakeDebuggee {
            source: Some("/tmp/x.kiln".to_string()),
            ..Default::default()
        });
        adapter.handle(
            &request(
                10,
                "setBreakpoints",
                json!({ "source": { "path": "/tmp/x.kiln" }, "breakpoints": [{ "line": 4 }] }),
            ),
            &mut out,
        );
        adapter.handle(
            &request(
                11,
                "setBreakpoints",
                json!({ "source": { "path": "/tmp/x.kiln" }, "breakpoints": [{ "line": 7 }] }),
            ),
            &mut out,
        );

        let last = out
            .messages
            .iter()
            .rfind(|m| m["command"] == "setBreakpoints")
            .unwrap();
        let bound = last["body"]["breakpoints"].as_array().unwrap();
        assert_eq!(bound.len(), 1, "the second request replaced the first");
        assert_eq!(bound[0]["line"], 7);
    }

    #[test]
    fn breakpoints_in_another_file_come_back_unverified_and_in_order() {
        let (mut adapter, mut out) = started(FakeDebuggee {
            source: Some("/tmp/x.kiln".to_string()),
            ..Default::default()
        });
        adapter.handle(
            &request(
                10,
                "setBreakpoints",
                json!({
                    "source": { "path": "/tmp/other.kiln" },
                    "breakpoints": [{ "line": 9 }, { "line": 2 }],
                }),
            ),
            &mut out,
        );

        let bound = out.response("setBreakpoints")["body"]["breakpoints"]
            .as_array()
            .unwrap()
            .clone();
        let lines: Vec<i64> = bound.iter().map(|b| b["line"].as_i64().unwrap()).collect();
        assert_eq!(lines, vec![9, 2]);
        assert!(bound.iter().all(|b| b["verified"] == false));
        assert_eq!(bound[0]["message"], "this program has no code from this file");
    }

    #[test]
    fn a_breakpoint_sent_before_a_launch_says_why_it_did_not_bind() {
        // A conforming client waits for `initialized`, which is sent from
        // `launch`, so this should not happen — and if it does the reason it
        // is given must still be the true one.
        let mut adapter = Adapter::new(Box::new(FakeBackend::with(FakeDebuggee::default())));
        let mut out = Recorder::new();
        adapter.handle(
            &request(
                1,
                "setBreakpoints",
                json!({ "source": { "path": "/tmp/x.kiln" }, "breakpoints": [{ "line": 3 }] }),
            ),
            &mut out,
        );

        let bound = out.response("setBreakpoints")["body"]["breakpoints"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(bound[0]["verified"], false);
        assert_eq!(
            bound[0]["message"],
            "there is no program yet, so this has not been placed"
        );
    }

    #[test]
    fn zero_based_clients_get_their_own_line_numbers_back() {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let mut adapter = Adapter::new(Box::new(FakeBackend::with(FakeDebuggee {
            source: Some("/tmp/x.kiln".to_string()),
            asked: Arc::clone(&asked),
            ..Default::default()
        })));
        let mut out = Recorder::new();
        adapter.handle(
            &request(1, "initialize", json!({ "linesStartAt1": false })),
            &mut out,
        );
        adapter.handle(
            &request(2, "launch", json!({ "program": "/tmp/x.kiln", "stopOnEntry": true })),
            &mut out,
        );
        adapter.handle(&request(3, "configurationDone", json!({})), &mut out);
        out.messages.clear();
        adapter.handle(
            &request(
                4,
                "setBreakpoints",
                json!({ "source": { "path": "/tmp/x.kiln" }, "breakpoints": [{ "line": 11 }] }),
            ),
            &mut out,
        );

        let bound = out.response("setBreakpoints")["body"]["breakpoints"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(
            bound[0]["line"], 11,
            "it goes back out in the base it came in"
        );
        // And it went *in* converted. Asserting only the round trip would pass
        // just as well with no conversion at all, and a zero-based client's
        // breakpoints would then be planted one line off, silently.
        assert_eq!(
            *asked.lock().expect("asked"),
            vec![12],
            "the engine was asked for the wrong line"
        );
    }

    #[test]
    fn threads_is_never_empty() {
        // Before a launch, after a launch, and after the program has gone: the
        // answer is the same one thread every time.
        let mut adapter = Adapter::new(Box::new(FakeBackend::with(FakeDebuggee::default())));
        let mut out = Recorder::new();
        adapter.handle(&request(1, "threads", json!({})), &mut out);
        let threads = out.response("threads")["body"]["threads"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0]["id"], 1);
        assert!(threads[0]["name"].as_str().is_some_and(|n| !n.is_empty()));

        adapter.exited = true;
        adapter.debuggee = None;
        let mut after = Recorder::new();
        adapter.handle(&request(2, "threads", json!({})), &mut after);
        assert_eq!(
            after.response("threads")["body"]["threads"]
                .as_array()
                .unwrap()
                .len(),
            1,
            "still one after the program has exited"
        );
    }

    #[test]
    fn a_build_failure_is_reported_and_the_session_ends() {
        let mut adapter = Adapter::new(Box::new(FakeBackend::failing(
            "kiln: x.kiln:4: `foo` is not a command",
        )));
        let mut out = Recorder::new();
        adapter.handle(&request(1, "initialize", json!({})), &mut out);
        adapter.handle(
            &request(2, "launch", json!({ "program": "/tmp/x.kiln" })),
            &mut out,
        );

        let launch = out.response("launch");
        assert_eq!(launch["request_seq"], 2);
        assert_eq!(launch["success"], false);
        assert_eq!(
            launch["body"]["error"]["format"],
            "kiln: x.kiln:4: `foo` is not a command"
        );
        assert_eq!(launch["body"]["error"]["showUser"], true);
        assert!(
            out.find_event("terminated").is_some(),
            "the client must be told the session is over"
        );
        assert!(
            out.find_event("initialized").is_none(),
            "nothing to configure when there is no program"
        );
        // The compiler's words reached the user while it was still working.
        assert!(out
            .messages
            .iter()
            .any(|m| m["event"] == "output" && m["body"]["output"] == "kiln: compiling\n"));
    }

    #[test]
    fn a_run_reports_the_breakpoint_it_stopped_on() {
        let (mut adapter, mut out) = started(FakeDebuggee {
            outcomes: VecDeque::from(vec![Ok(Stopped::Breakpoint(7))]),
            ..Default::default()
        });
        adapter.handle(&request(20, "continue", json!({ "threadId": 1 })), &mut out);

        let reply = out.response("continue");
        assert_eq!(reply["body"]["allThreadsContinued"], true);
        let stopped = out.find_event("stopped").expect("a stop is reported");
        assert_eq!(stopped["body"]["reason"], "breakpoint");
        assert_eq!(stopped["body"]["threadId"], 1);
        assert_eq!(stopped["body"]["allThreadsStopped"], true);
        assert_eq!(stopped["body"]["hitBreakpointIds"], json!([7]));
        // The answer to the request comes first, or the client credits the
        // stop to whatever it thought was happening before.
        let response_at = out.position_of_response("continue");
        let stopped_at = out
            .messages
            .iter()
            .position(|m| m["event"] == "stopped")
            .unwrap();
        assert!(response_at < stopped_at);
    }

    #[test]
    fn an_exit_is_reported_as_exited_then_terminated() {
        let (mut adapter, mut out) = started(FakeDebuggee {
            outcomes: VecDeque::from(vec![Ok(Stopped::Exited(3))]),
            ..Default::default()
        });
        adapter.handle(&request(20, "continue", json!({})), &mut out);

        let exited = out.find_event("exited").expect("an exit is reported");
        assert_eq!(exited["body"]["exitCode"], 3);
        let exited_at = out
            .messages
            .iter()
            .position(|m| m["event"] == "exited")
            .unwrap();
        let terminated_at = out
            .messages
            .iter()
            .position(|m| m["event"] == "terminated")
            .unwrap();
        assert!(exited_at < terminated_at, "exited then terminated");
    }

    #[test]
    fn a_runtime_error_stops_with_the_message_attached() {
        let (mut adapter, mut out) = started(FakeDebuggee {
            outcomes: VecDeque::from(vec![Ok(Stopped::RuntimeError(
                "index 4 is outside the array".to_string(),
            ))]),
            ..Default::default()
        });
        adapter.handle(&request(20, "next", json!({})), &mut out);

        let stopped = out.find_event("stopped").unwrap();
        assert_eq!(stopped["body"]["reason"], "exception");
        assert_eq!(stopped["body"]["description"], "index 4 is outside the array");
        assert_eq!(stopped["body"]["text"], "index 4 is outside the array");
    }

    #[test]
    fn a_stack_names_its_frames_and_never_numbers_one_zero() {
        let frame = |pc: u64, cfa: u64| Frame {
            registers: Registers { pc, sp: cfa - 16, bp: cfa - 8 },
            cfa,
        };
        let (mut adapter, mut out) = started(FakeDebuggee {
            // The stack grows down, so the caller's CFA is the larger one.
            frames: vec![frame(0x40_0012, 0x7fff_0100), frame(0x40_0024, 0x7fff_0200)],
            source: Some("/tmp/x.kiln".to_string()),
            ..Default::default()
        });
        adapter.handle(&request(20, "stackTrace", json!({ "threadId": 1 })), &mut out);

        let body = &out.response("stackTrace")["body"];
        assert_eq!(body["totalFrames"], 2);
        let frames = body["stackFrames"].as_array().unwrap();
        assert_eq!(frames[0]["id"], 1);
        assert_eq!(frames[1]["id"], 2);
        assert_eq!(frames[0]["line"], 0x12);
        // A caller's saved program counter is the return address, so the frame
        // is described one byte back, inside the call itself.
        assert_eq!(frames[1]["line"], 0x23);
        assert_eq!(frames[0]["source"]["path"], "/tmp/x.kiln");
        assert_eq!(frames[0]["source"]["name"], "x.kiln");
    }

    #[test]
    fn a_stack_honours_start_frame_and_levels() {
        let frame = |pc: u64| Frame {
            registers: Registers { pc, sp: 0, bp: 0 },
            cfa: pc,
        };
        let (mut adapter, mut out) = started(FakeDebuggee {
            frames: vec![frame(0x11), frame(0x22), frame(0x33)],
            ..Default::default()
        });
        adapter.handle(
            &request(20, "stackTrace", json!({ "startFrame": 1, "levels": 1 })),
            &mut out,
        );

        let body = &out.response("stackTrace")["body"];
        assert_eq!(body["totalFrames"], 3, "the whole depth, not the slice's");
        let frames = body["stackFrames"].as_array().unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0]["id"], 2);
    }

    #[test]
    fn locals_expand_one_based_and_leaves_say_zero() {
        let (mut adapter, mut out) = started(FakeDebuggee {
            locals: vec![
                ("count".to_string(), Value::Int(3)),
                (
                    "names".to_string(),
                    Value::Array(vec![
                        Value::Text("ada".to_string()),
                        Value::Text("grace".to_string()),
                    ]),
                ),
            ],
            frames: vec![Frame {
                registers: Registers::default(),
                cfa: 0x100,
            }],
            ..Default::default()
        });
        adapter.handle(&request(20, "stackTrace", json!({})), &mut out);
        adapter.handle(&request(21, "scopes", json!({ "frameId": 1 })), &mut out);

        let scope = &out.response("scopes")["body"]["scopes"][0];
        assert_eq!(scope["name"], "Locals");
        assert_eq!(scope["expensive"], false);
        let reference = scope["variablesReference"].as_i64().unwrap();
        assert!(reference > 0, "a scope must be expandable");

        adapter.handle(
            &request(22, "variables", json!({ "variablesReference": reference })),
            &mut out,
        );
        let variables = out.response("variables")["body"]["variables"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(variables[0]["name"], "count");
        assert_eq!(variables[0]["value"], "3");
        assert_eq!(
            variables[0]["variablesReference"], 0,
            "a leaf must say 0, not omit it"
        );
        assert_eq!(variables[1]["value"], "[2 items]");

        let array = variables[1]["variablesReference"].as_i64().unwrap();
        assert!(array > 0);
        adapter.handle(
            &request(23, "variables", json!({ "variablesReference": array })),
            &mut out,
        );
        let items = out
            .messages
            .iter()
            .rfind(|m| m["command"] == "variables")
            .unwrap()["body"]["variables"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(items[0]["name"], "[1]", "Kiln indexes from 1");
        assert_eq!(items[1]["name"], "[2]");
        assert_eq!(items[0]["value"], "\"ada\"");
    }

    #[test]
    fn references_do_not_survive_the_program_moving() {
        let (mut adapter, mut out) = started(FakeDebuggee {
            locals: vec![("count".to_string(), Value::Int(1))],
            frames: vec![Frame {
                registers: Registers::default(),
                cfa: 0x100,
            }],
            outcomes: VecDeque::from(vec![Ok(Stopped::Step)]),
            ..Default::default()
        });
        adapter.handle(&request(20, "scopes", json!({ "frameId": 1 })), &mut out);
        let reference = out.response("scopes")["body"]["scopes"][0]["variablesReference"]
            .as_i64()
            .unwrap();

        adapter.handle(&request(21, "next", json!({})), &mut out);
        out.messages.clear();
        adapter.handle(
            &request(22, "variables", json!({ "variablesReference": reference })),
            &mut out,
        );
        assert_eq!(
            out.response("variables")["success"],
            false,
            "the old stop's values would be a lie"
        );
    }

    #[test]
    fn evaluate_answers_a_name_and_refuses_anything_else() {
        let (mut adapter, mut out) = started(FakeDebuggee {
            locals: vec![("total".to_string(), Value::Int64(9))],
            ..Default::default()
        });
        adapter.handle(
            &request(
                20,
                "evaluate",
                json!({ "expression": "total", "frameId": 1, "context": "hover" }),
            ),
            &mut out,
        );
        assert_eq!(out.response("evaluate")["body"]["result"], "9");

        out.messages.clear();
        adapter.handle(
            &request(21, "evaluate", json!({ "expression": "total + 1" })),
            &mut out,
        );
        let reply = out.response("evaluate");
        assert_eq!(reply["success"], false);
        assert_eq!(
            reply["body"]["error"]["format"],
            "there is no `total + 1` here"
        );
    }

    #[test]
    fn disconnect_answers_a_launch_that_was_still_waiting() {
        let mut adapter = Adapter::new(Box::new(FakeBackend::with(FakeDebuggee::default())));
        let mut out = Recorder::new();
        adapter.handle(&request(1, "initialize", json!({})), &mut out);
        adapter.handle(
            &request(2, "launch", json!({ "program": "/tmp/x.kiln" })),
            &mut out,
        );
        out.messages.clear();
        adapter.handle(&request(3, "disconnect", json!({})), &mut out);

        let launch = out.response("launch");
        assert_eq!(launch["request_seq"], 2);
        assert_eq!(launch["success"], false);
        assert!(
            out.position_of_response("launch") < out.position_of_response("disconnect"),
            "the pending reply goes out before the one that cancelled it"
        );
        assert_eq!(out.response("disconnect")["success"], true);
        assert!(adapter.finished, "the loop must end");
    }

    #[test]
    fn an_unknown_request_is_refused_rather_than_ignored() {
        let mut adapter = Adapter::new(Box::new(FakeBackend::with(FakeDebuggee::default())));
        let mut out = Recorder::new();
        adapter.handle(&request(5, "goto", json!({})), &mut out);

        let reply = out.response("goto");
        assert_eq!(reply["request_seq"], 5);
        assert_eq!(reply["success"], false);
    }

    #[test]
    fn requests_that_need_a_program_fail_without_one() {
        let mut adapter = Adapter::new(Box::new(FakeBackend::with(FakeDebuggee::default())));
        let mut out = Recorder::new();
        for (seq, command) in [(1, "continue"), (2, "next"), (3, "pause"), (4, "stackTrace")] {
            adapter.handle(&request(seq, command, json!({})), &mut out);
            assert_eq!(
                out.response(command)["success"],
                false,
                "`{command}` cannot work before a launch"
            );
        }
    }

    #[test]
    fn stop_on_entry_holds_the_program_at_its_first_line() {
        let mut adapter = Adapter::new(Box::new(FakeBackend::with(FakeDebuggee {
            outcomes: VecDeque::from(vec![Ok(Stopped::Exited(0))]),
            ..Default::default()
        })));
        let mut out = Recorder::new();
        adapter.handle(&request(1, "initialize", json!({})), &mut out);
        adapter.handle(
            &request(2, "launch", json!({ "program": "/tmp/x.kiln", "stopOnEntry": true })),
            &mut out,
        );
        adapter.handle(&request(3, "configurationDone", json!({})), &mut out);

        assert_eq!(out.find_event("stopped").unwrap()["body"]["reason"], "entry");
        assert!(
            out.find_event("exited").is_none(),
            "it must not have been let go"
        );
    }

    #[test]
    fn without_stop_on_entry_the_program_is_let_go() {
        let mut adapter = Adapter::new(Box::new(FakeBackend::with(FakeDebuggee {
            outcomes: VecDeque::from(vec![Ok(Stopped::Breakpoint(1))]),
            ..Default::default()
        })));
        let mut out = Recorder::new();
        adapter.handle(&request(1, "initialize", json!({})), &mut out);
        adapter.handle(
            &request(2, "launch", json!({ "program": "/tmp/x.kiln" })),
            &mut out,
        );
        adapter.handle(&request(3, "configurationDone", json!({})), &mut out);

        // Nothing asked for this run, so the client is told about it.
        assert!(out.find_event("continued").is_some());
        assert_eq!(
            out.find_event("stopped").unwrap()["body"]["reason"],
            "breakpoint"
        );
    }

    #[test]
    fn terminate_ends_the_program_but_not_the_conversation() {
        let (mut adapter, mut out) = started(FakeDebuggee::default());
        adapter.handle(&request(20, "terminate", json!({})), &mut out);

        assert_eq!(out.response("terminate")["success"], true);
        assert!(out.find_event("terminated").is_some());
        assert!(
            !adapter.finished,
            "the client still has a `disconnect` to send"
        );
    }

    #[test]
    fn a_whole_handshake_survives_the_wire() {
        // The same path `run` takes, minus the process's own stdio: framed
        // bytes in, framed bytes out. A message shape that is right in a unit
        // test and wrong once framed is the failure this catches.
        let requests = [
            json!({ "type": "request", "seq": 1, "command": "initialize",
                    "arguments": { "linesStartAt1": true } }),
            json!({ "type": "request", "seq": 2, "command": "launch",
                    "arguments": { "program": "/tmp/x.kiln", "stopOnEntry": true } }),
            json!({ "type": "request", "seq": 3, "command": "setBreakpoints",
                    "arguments": { "source": { "path": "/tmp/x.kiln" },
                                   "breakpoints": [{ "line": 12 }] } }),
            json!({ "type": "request", "seq": 4, "command": "configurationDone" }),
            json!({ "type": "request", "seq": 5, "command": "threads" }),
            json!({ "type": "request", "seq": 6, "command": "disconnect" }),
        ];
        let mut wire = Vec::new();
        for request in &requests {
            wire.extend_from_slice(&frame(request));
        }

        let mut adapter = Adapter::new(Box::new(FakeBackend::with(FakeDebuggee {
            source: Some("/tmp/x.kiln".to_string()),
            ..Default::default()
        })));
        let mut out = Out::new(Vec::new());
        let mut input = BufReader::new(wire.as_slice());
        while let Some(message) = read_message(&mut input).unwrap() {
            let request = Request::parse(&message).unwrap();
            adapter.handle(&request, &mut out);
            if adapter.finished {
                break;
            }
        }
        assert!(adapter.finished, "`disconnect` ends the loop");

        let sent = out.writer.clone();
        let mut replies = BufReader::new(sent.as_slice());
        let mut received = Vec::new();
        while let Some(message) = read_message(&mut replies).unwrap() {
            received.push(message);
        }

        let seqs: Vec<u64> = received
            .iter()
            .map(|m| m["seq"].as_u64().unwrap())
            .collect();
        assert_eq!(
            seqs,
            (1..=seqs.len() as u64).collect::<Vec<_>>(),
            "one counter, no gaps, in send order"
        );
        let answered: Vec<u64> = received
            .iter()
            .filter(|m| m["type"] == "response")
            .map(|m| m["request_seq"].as_u64().unwrap())
            .collect();
        // Every request answered, and `launch` answered after the
        // `configurationDone` it was waiting for.
        assert_eq!(answered, vec![1, 3, 4, 2, 5, 6]);
        assert_eq!(
            received
                .iter()
                .find(|m| m["command"] == "setBreakpoints")
                .unwrap()["body"]["breakpoints"][0]["line"],
            12
        );
    }

    #[test]
    fn framing_survives_a_round_trip() {
        let mut out = Out::new(Vec::new());
        out.send(event("initialized", None));
        out.send(json!({ "type": "response", "request_seq": 1, "success": true, "command": "x" }));

        let written = out.writer.clone();
        let mut input = BufReader::new(written.as_slice());
        let first = read_message(&mut input).unwrap().unwrap();
        let second = read_message(&mut input).unwrap().unwrap();
        assert_eq!(first["event"], "initialized");
        assert_eq!(first["seq"], 1);
        assert_eq!(second["seq"], 2);
        assert!(
            read_message(&mut input).unwrap().is_none(),
            "end of input is not an error"
        );
    }

    #[test]
    fn a_message_is_read_whatever_the_header_casing() {
        let body = br#"{"type":"request","seq":8,"command":"threads"}"#;
        let mut raw = format!("content-length: {}\r\n\r\n", body.len()).into_bytes();
        raw.extend_from_slice(body);
        let mut input = BufReader::new(raw.as_slice());
        let message = read_message(&mut input).unwrap().unwrap();
        assert_eq!(Request::parse(&message).unwrap().command, "threads");
    }

    #[test]
    fn responses_and_events_from_the_client_are_not_requests() {
        assert!(Request::parse(&json!({ "type": "event", "event": "output" })).is_none());
        assert!(Request::parse(&json!({ "type": "response", "command": "x" })).is_none());
    }

    #[test]
    fn a_binary_defaults_to_the_source_without_its_extension() {
        assert_eq!(default_binary(Path::new("/tmp/x.kiln")), Path::new("/tmp/x"));
        // Never over the source itself.
        assert_eq!(default_binary(Path::new("/tmp/x")), Path::new("/tmp/x.bin"));
    }

    #[test]
    fn a_value_summarises_without_repeating_its_parts() {
        assert_eq!(summary(&Value::Nothing), "nothing");
        assert_eq!(summary(&Value::Bool(true)), "true");
        assert_eq!(summary(&Value::Text(String::new())), "\"\"");
        assert_eq!(summary(&Value::Array(vec![])), "[]");
        assert_eq!(summary(&Value::Array(vec![Value::Int(1)])), "[1 item]");
        assert_eq!(
            summary(&Value::Record {
                name: "Point".to_string(),
                fields: vec![("x".to_string(), Value::Int(1))],
            }),
            "Point { … }"
        );
        assert_eq!(
            summary(&Value::Unreadable("no such address".to_string())),
            "<unreadable: no such address>"
        );
    }
}
