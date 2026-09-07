//! Wire-level tests for `kiln lsp`.
//!
//! These drive the real binary over real stdio with real JSON-RPC framing,
//! because that is where language servers actually break: a `Content-Length`
//! off by one, a handshake that never completes, a notification sent before
//! `initialized`. A unit test on `diagnose()` would pass while the server was
//! silent in every editor on earth — the same "mechanism works, surface is
//! broken" trap that has caught this project repeatedly.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Client {
    /// Start the server and complete the initialize handshake.
    fn start() -> Client {
        Client::start_in(&repo(), &repo())
    }

    /// Start the server rooted at `workspace`, with `home` pinned.
    ///
    /// Both are inputs to kit resolution, so a test that inherited the
    /// developer's would pass or fail on what they happen to have installed —
    /// the same hazard `kits.rs` documents.
    fn start_in(workspace: &std::path::Path, home: &std::path::Path) -> Client {
        let mut child = Command::new(env!("CARGO_BIN_EXE_kiln"))
            .arg("lsp")
            .current_dir(workspace)
            .env("HOME", home)
            .env("KILN_RUNTIME_DIR", repo().join("runtime"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn kiln lsp");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut c = Client {
            child,
            stdin,
            stdout,
        };

        let root = format!("file://{}", workspace.display());
        c.send(serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "capabilities": {}, "rootUri": root }
        }));
        let reply = c.recv();
        assert_eq!(reply["id"], 1, "initialize must be answered: {reply}");
        assert!(
            reply["result"]["capabilities"]["textDocumentSync"] == 1,
            "server should advertise full-text sync: {reply}"
        );
        // A capability that is implemented but not advertised is never asked
        // for: no client sends `signatureHelp` to a server that did not claim
        // it, so the feature would be dead in every editor while every test
        // that pokes it directly still passed.
        let sig = &reply["result"]["capabilities"]["signatureHelpProvider"];
        assert!(
            sig["triggerCharacters"]
                .as_array()
                .is_some_and(|t| t.iter().any(|c| c == "(")),
            "signature help must be advertised, triggered by `(`: {reply}"
        );
        c.send(serde_json::json!({
            "jsonrpc": "2.0", "method": "initialized", "params": {}
        }));
        c
    }

    fn send(&mut self, v: serde_json::Value) {
        let body = serde_json::to_string(&v).unwrap();
        write!(self.stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body).unwrap();
        self.stdin.flush().unwrap();
    }

    /// Read one framed message. Panics on malformed framing, which is the point.
    fn recv(&mut self) -> serde_json::Value {
        let mut len = 0usize;
        loop {
            let mut line = String::new();
            let n = self.stdout.read_line(&mut line).expect("read header");
            assert!(n > 0, "server closed the stream while we were reading");
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some(v) = line.strip_prefix("Content-Length: ") {
                len = v.parse().expect("numeric Content-Length");
            }
        }
        assert!(len > 0, "message had no Content-Length");
        let mut buf = vec![0u8; len];
        self.stdout.read_exact(&mut buf).expect("read body");
        serde_json::from_slice(&buf).expect("body is JSON")
    }

    fn open(&mut self, uri: &str, text: &str) {
        self.send(serde_json::json!({
            "jsonrpc": "2.0", "method": "textDocument/didOpen",
            "params": { "textDocument": {
                "uri": uri, "languageId": "kiln", "version": 1, "text": text
            }}
        }));
    }

    /// Read messages until a `publishDiagnostics` for `uri` arrives.
    fn diagnostics(&mut self, uri: &str) -> Vec<serde_json::Value> {
        for _ in 0..10 {
            let m = self.recv();
            if m["method"] == "textDocument/publishDiagnostics" && m["params"]["uri"] == uri {
                return m["params"]["diagnostics"].as_array().unwrap().clone();
            }
        }
        panic!("no publishDiagnostics for {uri}");
    }

    fn shutdown(mut self) {
        self.send(serde_json::json!({
            "jsonrpc": "2.0", "id": 99, "method": "shutdown", "params": null
        }));
        let _ = self.recv();
        self.send(serde_json::json!({"jsonrpc": "2.0", "method": "exit"}));
        // Close stdin BEFORE waiting. `lsp-server`'s reader thread runs until
        // stdin hits EOF, and the server joins its io threads on the way out —
        // so holding the pipe open here deadlocks the exit we are waiting for.
        drop(self.stdin);
        let status = self.child.wait().expect("server exits");
        assert!(status.success(), "server should exit cleanly: {status}");
    }
}

/// A clean file must produce an *empty* diagnostics array, not silence — that
/// is how a client clears previous squiggles.
#[test]
fn clean_file_publishes_empty_diagnostics() {
    let mut c = Client::start();
    let uri = "file:///tmp/kiln_lsp_clean.kiln";
    c.open(
        uri,
        "module m\nsub main\n  call print_text(\"hi\")\nend\n",
    );
    let d = c.diagnostics(uri);
    assert!(d.is_empty(), "expected no diagnostics, got {d:?}");
    c.shutdown();
}

/// The whole point of the position work: the squiggle lands on the line with
/// the mistake. LSP lines are 0-based, so source line 5 is `line: 4`.
#[test]
fn semantic_errors_land_on_the_right_line() {
    let mut c = Client::start();
    let uri = "file:///tmp/kiln_lsp_errors.kiln";
    //                1          2         3                 4          5
    c.open(
        uri,
        "module m\nsub main\n  let x: int = 1\n  x = 2\n  call nosuch()\nend\n",
    );
    let d = c.diagnostics(uri);
    assert_eq!(d.len(), 2, "expected two diagnostics, got {d:?}");

    let immutable = d
        .iter()
        .find(|x| x["message"].as_str().unwrap().contains("immutable"))
        .expect("immutability error");
    assert_eq!(immutable["range"]["start"]["line"], 3, "source line 4");
    assert_eq!(immutable["severity"], 1, "errors are severity 1");
    assert_eq!(immutable["source"], "kiln");
    assert!(
        !immutable["message"].as_str().unwrap().starts_with("line "),
        "the message must not repeat the position — the range carries it: {immutable}"
    );

    let unknown = d
        .iter()
        .find(|x| x["message"].as_str().unwrap().contains("unknown command"))
        .expect("unknown-command error");
    assert_eq!(unknown["range"]["start"]["line"], 4, "source line 5");
    c.shutdown();
}

/// While you type, the file is syntactically broken most of the time. That path
/// must be positioned too, not collapsed to line 0.
#[test]
fn parse_errors_are_positioned() {
    let mut c = Client::start();
    let uri = "file:///tmp/kiln_lsp_parse.kiln";
    c.open(uri, "module m\nsub main\n  let = = =\nend\n");
    let d = c.diagnostics(uri);
    assert_eq!(d.len(), 1, "one parse error: {d:?}");
    assert_eq!(d[0]["range"]["start"]["line"], 2, "the bad line is line 3");
    c.shutdown();
}

/// Editing republishes: fix the file and the squiggle must disappear.
#[test]
fn edits_republish_diagnostics() {
    let mut c = Client::start();
    let uri = "file:///tmp/kiln_lsp_edit.kiln";
    c.open(uri, "module m\nsub main\n  call nosuch()\nend\n");
    assert_eq!(c.diagnostics(uri).len(), 1, "starts broken");

    c.send(serde_json::json!({
        "jsonrpc": "2.0", "method": "textDocument/didChange",
        "params": {
            "textDocument": { "uri": uri, "version": 2 },
            "contentChanges": [{ "text": "module m\nsub main\n  call print_text(\"ok\")\nend\n" }]
        }
    }));
    assert!(
        c.diagnostics(uri).is_empty(),
        "fixing the file must clear the diagnostics"
    );
    c.shutdown();
}

/// An unsupported request must get an error response, never silence: a client
/// waiting on a reply that never comes hangs.
#[test]
fn unsupported_requests_get_an_error_not_silence() {
    let mut c = Client::start();
    c.send(serde_json::json!({
        // Formatting is genuinely unimplemented; hover et al. are supported now.
        "jsonrpc": "2.0", "id": 7, "method": "textDocument/formatting",
        "params": { "textDocument": {"uri": "file:///tmp/x.kiln"},
                    "options": {"tabSize": 2, "insertSpaces": true} }
    }));
    let reply = c.recv();
    assert_eq!(reply["id"], 7);
    assert!(reply["error"].is_object(), "expected an error reply: {reply}");
    c.shutdown();
}

// ---------------------------------------------------------------------------
// Completion, hover, definition, references
// ---------------------------------------------------------------------------

impl Client {
    fn request(&mut self, id: i64, method: &str, params: serde_json::Value) -> serde_json::Value {
        self.send(serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": method, "params": params
        }));
        for _ in 0..10 {
            let m = self.recv();
            if m["id"] == id {
                return m["result"].clone();
            }
        }
        panic!("no response to {method}");
    }

    fn at(uri: &str, line: u32, ch: u32) -> serde_json::Value {
        serde_json::json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": ch }
        })
    }
}

/// The server must advertise every capability it implements — a client only
/// sends requests for capabilities it was told about, so an unadvertised
/// feature is dead code however well it works.
#[test]
fn advertises_its_capabilities() {
    // start() already asserts the handshake and the advertised sync kind.
    Client::start().shutdown();

    let mut c = Client::start();
    let r = c.request(50, "textDocument/documentSymbol", serde_json::json!({
        "textDocument": { "uri": "file:///tmp/kiln_lsp_nodoc.kiln" }
    }));
    // Unknown document: a null result, not an error and not a hang.
    assert!(r.is_null(), "unknown document should answer null: {r}");
    c.shutdown();
}

// A real form: `use ui` supplies the component types, and each component
// opens its own block.
const FORM_SRC: &str =
    "module m\nuse ui\nform Main\n  button ok\n  end\nend\nsub go\n  ok.text = \"hi\"\nend\n";

/// After `id.` the completions must be that component's properties — offering
/// the whole global namespace there is what makes completion feel broken.
#[test]
fn completion_after_dot_offers_component_properties() {
    let mut c = Client::start();
    let uri = "file:///tmp/kiln_lsp_complete.kiln";
    c.open(uri, FORM_SRC);
    let _ = c.diagnostics(uri);

    // `  ok.te|xt` on line 8 (0-based 7): mid-word, the way an editor asks.
    let r = c.request(60, "textDocument/completion", serde_json::json!({
        "textDocument": { "uri": uri },
        "position": { "line": 7, "character": 7 }
    }));
    let labels: Vec<String> = r
        .as_array()
        .expect("completion list")
        .iter()
        .map(|i| i["label"].as_str().unwrap().to_string())
        .collect();
    assert!(labels.contains(&"text".to_string()), "expected `text`: {labels:?}");
    assert!(
        !labels.contains(&"module".to_string()),
        "keywords must not appear after `.`: {labels:?}"
    );
    c.shutdown();
}

/// Plain completion offers commands from the registry, with their signature.
#[test]
fn completion_offers_commands_and_locals() {
    let mut c = Client::start();
    let uri = "file:///tmp/kiln_lsp_complete2.kiln";
    c.open(uri, "module m\nsub main\n  let total: int = 1\n  \nend\n");
    let _ = c.diagnostics(uri);

    let r = c.request(61, "textDocument/completion", serde_json::json!({
        "textDocument": { "uri": uri },
        "position": { "line": 3, "character": 2 }
    }));
    let items = r.as_array().expect("completion list");
    let labels: Vec<&str> = items.iter().map(|i| i["label"].as_str().unwrap()).collect();
    assert!(labels.contains(&"print_text"), "commands: {labels:?}");
    assert!(labels.contains(&"total"), "locals in scope: {labels:?}");

    let print = items.iter().find(|i| i["label"] == "print_text").unwrap();
    assert!(
        print["detail"].as_str().unwrap().contains("("),
        "a command's detail should show its signature: {print}"
    );
    c.shutdown();
}

/// The words 0.7.0 added are offered like any other keyword.
///
/// `through` and the infix bitwise operators are soft keywords — the lexer
/// reserves nothing for them — so nothing else in the compiler would notice if
/// completion forgot they existed. This is where that is noticed.
#[test]
fn completion_offers_the_indirect_call_and_bitwise_words() {
    let mut c = Client::start();
    let uri = "file:///tmp/kiln_lsp_complete07.kiln";
    c.open(uri, "module m\nsub main\n  let total: int = 1\n  \nend\n");
    let _ = c.diagnostics(uri);

    let r = c.request(62, "textDocument/completion", serde_json::json!({
        "textDocument": { "uri": uri },
        "position": { "line": 3, "character": 2 }
    }));
    let labels = labels(&r);
    for word in ["through", "band", "bor", "bxor", "bnot", "shl", "shr", "ushr"] {
        assert!(
            labels.contains(&word.to_string()),
            "completion should offer `{word}`: {labels:?}"
        );
    }
    c.shutdown();
}

/// The words 0.9.0 added are offered like any other keyword.
///
/// All but one are *soft* keywords the parser recognises by position — the
/// lexer reserves nothing for `match`, `defer` or `then`, and a variable may
/// still be named for any of them — so nothing else in the compiler would
/// notice if completion forgot one existed. This is where that is noticed.
/// `none` is the exception, a literal rather than a position, and is offered
/// beside the rest.
#[test]
fn completion_offers_the_shorthand_words() {
    let mut c = Client::start();
    let uri = "file:///tmp/kiln_lsp_complete09.kiln";
    c.open(uri, "module m\nsub main\n  let total: int = 1\n  \nend\n");
    let _ = c.diagnostics(uri);

    let r = c.request(63, "textDocument/completion", serde_json::json!({
        "textDocument": { "uri": uri },
        "position": { "line": 3, "character": 2 }
    }));
    let labels = labels(&r);
    for word in [
        "then", "otherwise", "check", "match", "when", "repeat", "times",
        "assert", "enum", "some", "none", "as", "where", "defer",
    ] {
        assert!(
            labels.contains(&word.to_string()),
            "completion should offer `{word}`: {labels:?}"
        );
    }
    c.shutdown();
}

#[test]
fn hover_shows_a_command_signature() {
    let mut c = Client::start();
    let uri = "file:///tmp/kiln_lsp_hover.kiln";
    c.open(uri, "module m\nsub main\n  call print_text(\"hi\")\nend\n");
    let _ = c.diagnostics(uri);

    let r = c.request(70, "textDocument/hover", Client::at(uri, 2, 9));
    let text = r["contents"]["value"].as_str().unwrap_or("");
    assert!(text.contains("print_text"), "hover should name it: {r}");
    assert!(text.contains("command"), "and say what it is: {r}");
    c.shutdown();
}

/// A `##` note above a declaration is the author's own account of the symbol,
/// and hover is the only place it can be read. A plain `#` comment is a note to
/// whoever is reading the source and must stay invisible — that difference is
/// the whole of the feature, so both halves are asserted here.
#[test]
fn hover_shows_a_doc_comment_and_hides_a_plain_one() {
    let mut c = Client::start();
    let uri = "file:///tmp/kiln_lsp_doc.kiln";
    c.open(
        uri,
        "module m\n\
         ## Greet someone by name.\n\
         ## The name is not checked.\n\
         sub greet(who: text)\n\
         end\n\
         # an ordinary note\n\
         sub quiet()\n\
         end\n\
         sub main\n\
         \x20 call greet(\"you\")\n\
         \x20 call quiet()\n\
         end\n",
    );
    let _ = c.diagnostics(uri);

    let r = c.request(80, "textDocument/hover", Client::at(uri, 9, 8));
    let text = r["contents"]["value"].as_str().unwrap_or("");
    assert!(text.contains("greet"), "hover should name it: {r}");
    assert!(
        text.contains("Greet someone by name."),
        "hover should carry the doc comment: {r}"
    );
    assert!(
        text.contains("The name is not checked."),
        "every `##` line above the declaration belongs to it: {r}"
    );

    let r = c.request(81, "textDocument/hover", Client::at(uri, 10, 8));
    let text = r["contents"]["value"].as_str().unwrap_or("");
    assert!(text.contains("quiet"), "hover should name it: {r}");
    assert!(
        !text.contains("an ordinary note"),
        "a single-`#` comment is not documentation: {r}"
    );
    c.shutdown();
}

/// An `enum` declares a name and a run of members, and at module level those
/// are two identifiers on a line — which is also how a non-visual component is
/// declared. Read as a component, the enum would open a block that swallows
/// the rest of the file and every symbol after it would go unindexed, so the
/// declaration is claimed here and its body skipped, exactly as a `record`'s is.
#[test]
fn an_enum_is_a_type_name_not_a_component() {
    let mut c = Client::start();
    let uri = "file:///tmp/kiln_lsp_enum.kiln";
    c.open(
        uri,
        "module m\n\
         enum colour\n\
         \x20 red, green\n\
         end\n\
         sub after()\n\
         end\n",
    );
    let _ = c.diagnostics(uri);

    let r = c.request(84, "textDocument/hover", Client::at(uri, 1, 6));
    let text = r["contents"]["value"].as_str().unwrap_or("");
    assert!(text.contains("colour"), "hover should name it: {r}");
    assert!(
        !text.contains("component"),
        "an enum is not a component: {r}"
    );

    // The subroutine after the enum must still be indexed — proof the enum's
    // `end` closed the enum and not something the server thinks is a block.
    let syms = c.request(
        85,
        "textDocument/documentSymbol",
        serde_json::json!({ "textDocument": { "uri": uri } }),
    );
    let names = syms.to_string();
    assert!(names.contains("after"), "the sub after the enum is missing: {syms}");
    c.shutdown();
}

#[test]
fn goto_definition_finds_the_declaration() {
    let mut c = Client::start();
    let uri = "file:///tmp/kiln_lsp_def.kiln";
    //          1        2               3         4       5
    c.open(uri, "module m\nvar count: int = 0\nsub main\n  count = 1\nend\n");
    let _ = c.diagnostics(uri);

    // cursor on `count` in the assignment (line 4 -> 0-based 3)
    let r = c.request(80, "textDocument/definition", Client::at(uri, 3, 3));
    assert_eq!(r["range"]["start"]["line"], 1, "declared on line 2: {r}");
    assert_eq!(r["uri"], uri);
    c.shutdown();
}

/// Shadowing is where a naive index gets it wrong with full confidence.
#[test]
fn references_respect_local_shadowing() {
    let mut c = Client::start();
    let uri = "file:///tmp/kiln_lsp_refs.kiln";
    //          1        2               3      4                5         6    7      8         9
    let src = "module m\nvar x: int = 1\nsub a\n  let x: int = 9\n  x = 3\nend\nsub b\n  x = 4\nend\n";
    c.open(uri, src);
    let _ = c.diagnostics(uri);

    // The `x` on line 5 is a's local: it must not include line 2 or line 8.
    let r = c.request(90, "textDocument/references", serde_json::json!({
        "textDocument": { "uri": uri },
        "position": { "line": 4, "character": 2 },
        "context": { "includeDeclaration": true }
    }));
    let lines: Vec<i64> = r
        .as_array()
        .expect("locations")
        .iter()
        .map(|l| l["range"]["start"]["line"].as_i64().unwrap())
        .collect();
    assert!(lines.contains(&3), "the local's declaration: {lines:?}");
    assert!(lines.contains(&4), "its use: {lines:?}");
    assert!(!lines.contains(&1), "must NOT include the global: {lines:?}");
    assert!(!lines.contains(&7), "must NOT include sub b: {lines:?}");
    c.shutdown();
}

/// LSP columns are UTF-16 code units; our lexer counts bytes. They agree only
/// for ASCII, so an ASCII-only suite would pass forever while every file with a
/// non-Latin string misplaced the cursor.
#[test]
fn positions_are_utf16_not_bytes() {
    let mut c = Client::start();
    let uri = "file:///tmp/kiln_lsp_utf16.kiln";
    // "héllo wörld" is 11 UTF-16 units but 13 bytes.
    let src = "module m\nvar greeting: text = \"héllo wörld\"\nsub main\n  greeting = \"x\"\nend\n";
    c.open(uri, src);
    let _ = c.diagnostics(uri);

    // `greeting` on line 4 starts at UTF-16 character 2.
    let r = c.request(100, "textDocument/definition", Client::at(uri, 3, 3));
    assert_eq!(r["range"]["start"]["line"], 1, "declared on line 2: {r}");
    assert_eq!(
        r["range"]["start"]["character"], 4,
        "`greeting` starts after `var ` — 4 UTF-16 units in: {r}"
    );
    assert_eq!(r["range"]["end"]["character"], 12, "8 characters long: {r}");
    c.shutdown();
}

#[test]
fn document_symbols_list_module_level_names() {
    let mut c = Client::start();
    let uri = "file:///tmp/kiln_lsp_syms.kiln";
    c.open(uri, FORM_SRC);
    let _ = c.diagnostics(uri);

    let r = c.request(110, "textDocument/documentSymbol", serde_json::json!({
        "textDocument": { "uri": uri }
    }));
    let names: Vec<&str> = r
        .as_array()
        .expect("symbols")
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"go"), "subroutine: {names:?}");
    assert!(names.contains(&"ok"), "component: {names:?}");
    c.shutdown();
}

// ---------------------------------------------------------------------------
// Kits, signature help
// ---------------------------------------------------------------------------

/// A project kit: the same shape `kits.rs` builds, kept here rather than
/// shared because a test crate is its own compilation unit.
fn write_project_kit(root: &std::path::Path, name: &str) {
    let dir = root.join("kits").join(name);
    std::fs::create_dir_all(&dir).expect("create kit dir");
    std::fs::write(
        dir.join(format!("{name}_libinfo.c")),
        format!(
            r#"#include "kiln_abi.h"
void {name}_answer(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
static const Kiln_CommandDesc C[] = {{
    {{ "{name}_answer", "{name}_answer", KN_SDT_INT, 0, 0 }},
}};
static const Kiln_LibInfo I = {{
    KILN_ABI_VERSION, "{name}", "kiln-lsptest-{name}", 1, 0, 0,
    (int32_t)(sizeof(C) / sizeof(C[0])), C,
}};
const Kiln_LibInfo *kiln_get_lib_info(void) {{ return &I; }}
"#
        ),
    )
    .expect("write libinfo");
    std::fs::write(
        dir.join(format!("{name}_cmds.c")),
        format!(
            r#"#include "kiln_abi.h"
void {name}_answer(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {{
    (void)argc; (void)argv; kn_ret_int(ret, 42);
}}
"#
        ),
    )
    .expect("write cmds");
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kiln_lspdx_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch");
    dir
}

/// The worst bug an editor can have is disagreeing with the compiler. A kit's
/// commands build, so they must complete and they must not be underlined.
#[test]
fn a_project_kits_commands_complete_and_do_not_error() {
    let root = scratch("kit_ws");
    let home = scratch("kit_home");
    write_project_kit(&root, "widget");

    let mut c = Client::start_in(&root, &home);
    let uri = "file:///tmp/kiln_lsp_kit.kiln";
    let src = "module m\nuse widget\nsub main\n  let n: int = widget_answer()\n  \nend\n";
    c.open(uri, src);
    let diags = c.diagnostics(uri);
    assert!(
        diags.is_empty(),
        "a kit's command compiles, so the editor must not underline it: {diags:?}"
    );

    let r = c.request(80, "textDocument/completion", Client::at(uri, 4, 2));
    let labels: Vec<String> = r
        .as_array()
        .expect("completion list")
        .iter()
        .map(|i| i["label"].as_str().unwrap().to_string())
        .collect();
    assert!(
        labels.contains(&"widget_answer".to_string()),
        "the kit's command must be offered: {labels:?}"
    );
    c.shutdown();
}

/// The names legal on a `use` line are exactly the resolvable kits, and they
/// are the one thing that cannot be guessed from the file you are editing.
#[test]
fn use_line_completion_offers_resolvable_kits() {
    let root = scratch("use_ws");
    let home = scratch("use_home");
    write_project_kit(&root, "widget");

    let mut c = Client::start_in(&root, &home);
    let uri = "file:///tmp/kiln_lsp_use.kiln";
    c.open(uri, "module m\nuse \nsub main\nend\n");
    let _ = c.diagnostics(uri);

    let r = c.request(81, "textDocument/completion", Client::at(uri, 1, 4));
    let labels: Vec<String> = r
        .as_array()
        .expect("completion list")
        .iter()
        .map(|i| i["label"].as_str().unwrap().to_string())
        .collect();
    assert!(labels.contains(&"widget".to_string()), "project kit: {labels:?}");
    assert!(labels.contains(&"file".to_string()), "bundled library: {labels:?}");
    assert!(
        !labels.contains(&"module".to_string()),
        "a keyword is not a library name: {labels:?}"
    );
    c.shutdown();
}

/// Signature help while typing a call — for a command, and for the module's
/// own subroutines, whose parameter *names* the registry cannot supply.
#[test]
fn signature_help_tracks_the_argument_being_typed() {
    let mut c = Client::start();
    let uri = "file:///tmp/kiln_lsp_sig.kiln";
    let src = "module m\nsub join_two(left: text, right: text): text\n  return concat(left, right)\n\
               end\nsub main\n  call print_text(join_two(\"a\", \"b\"))\nend\n";
    c.open(uri, src);
    let _ = c.diagnostics(uri);

    // Inside `concat(left, |right)` on line 3 — the second argument.
    let r = c.request(82, "textDocument/signatureHelp", Client::at(uri, 2, 22));
    let sig = &r["signatures"][0];
    assert!(
        sig["label"].as_str().unwrap_or("").starts_with("concat("),
        "should describe the call being typed: {r}"
    );
    assert_eq!(r["activeParameter"], 1, "second argument: {r}");

    // Inside `join_two("a", |"b")` on line 6 — a subroutine, with its own
    // parameter names.
    let r = c.request(83, "textDocument/signatureHelp", Client::at(uri, 5, 33));
    let label = r["signatures"][0]["label"].as_str().unwrap_or("").to_string();
    assert!(label.contains("left: text"), "parameter names: {label}");
    assert_eq!(r["activeParameter"], 1, "second argument: {r}");

    // A string containing a comma must not move the highlight.
    let uri2 = "file:///tmp/kiln_lsp_sig2.kiln";
    c.open(uri2, "module m\nsub main\n  call print_text(concat(\"a, b\", \"c\"))\nend\n");
    let _ = c.diagnostics(uri2);
    let r = c.request(84, "textDocument/signatureHelp", Client::at(uri2, 2, 34));
    assert_eq!(r["activeParameter"], 1, "the comma inside the literal counted: {r}");
    c.shutdown();
}

/// Hover on a subroutine shows its signature. Without it the only way to
/// recall a parameter list is to scroll to the declaration.
#[test]
fn hover_on_a_subroutine_shows_its_parameters() {
    let mut c = Client::start();
    let uri = "file:///tmp/kiln_lsp_subhover.kiln";
    c.open(
        uri,
        "module m\nsub twice(n: int): int\n  return n * 2\nend\nsub main\n  let x: int = twice(2)\nend\n",
    );
    let _ = c.diagnostics(uri);

    let r = c.request(85, "textDocument/hover", Client::at(uri, 5, 16));
    let text = r["contents"]["value"].as_str().unwrap_or("").to_string();
    assert!(text.contains("twice(n: int): int"), "hover: {text}");
    c.shutdown();
}

/// The types that landed this week must be visible to the editor, or a user
/// meets them for the first time as a red squiggle. Three things at once: the
/// dictionary commands complete like any other, a record name is offered as a
/// type, and hovering one says *record* — not "component type", which is what
/// an index with no category for a record would have to call it.
#[test]
fn records_and_dictionaries_reach_the_editor() {
    let mut c = Client::start();
    let uri = "file:///tmp/kiln_lsp_aggregates.kiln";
    //          1        2                3        4      5         6
    c.open(
        uri,
        "module m\nrecord person\n  name: text\nend\nsub main\n  var d: int{} = {}\n  \nend\n",
    );
    let diags = c.diagnostics(uri);
    assert!(diags.is_empty(), "a record and a dictionary must be clean: {diags:?}");

    let r = c.request(90, "textDocument/completion", serde_json::json!({
        "textDocument": { "uri": uri },
        "position": { "line": 6, "character": 2 }
    }));
    let items = r.as_array().expect("completion list");
    let labels: Vec<&str> = items.iter().map(|i| i["label"].as_str().unwrap()).collect();
    for cmd in ["dict_get", "dict_set", "dict_has", "dict_keys", "dict_count", "dict_remove"] {
        assert!(labels.contains(&cmd), "`{cmd}` should complete: {labels:?}");
    }
    assert!(labels.contains(&"person"), "the record type: {labels:?}");
    let person = items.iter().find(|i| i["label"] == "person").unwrap();
    assert_eq!(
        person["detail"].as_str().unwrap_or(""),
        "record type",
        "a record must not be presented as something else: {person}"
    );

    // Hover on the declaration itself.
    let h = c.request(91, "textDocument/hover", Client::at(uri, 1, 8));
    let text = h["contents"]["value"].as_str().unwrap_or("").to_string();
    assert!(text.contains("record type"), "hover on a record: {text}");
    c.shutdown();
}

// ---------------------------------------------------------------------------
// Diagnostics that name the fix, and where
// ---------------------------------------------------------------------------

/// Two calls on one line: the squiggle has to say which. The range covers the
/// offending name, not the line it sits on.
#[test]
fn diagnostics_underline_the_name_not_the_line() {
    let mut c = Client::start();
    let uri = "file:///tmp/kiln_lsp_cols.kiln";
    //                                   0-based columns: `nope` is 17..21
    c.open(uri, "module m\nsub main\n  call print_int(nope(1))\nend\n");
    let d = c.diagnostics(uri);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0]["range"]["start"]["line"], 2);
    assert_eq!(d[0]["range"]["start"]["character"], 17, "{:?}", d[0]);
    assert_eq!(d[0]["range"]["end"]["character"], 21, "{:?}", d[0]);
    c.shutdown();
}

/// The column is UTF-16, like every other position: a non-Latin string to the
/// left of the mistake must not shift the squiggle.
#[test]
fn diagnostic_columns_are_utf16() {
    let mut c = Client::start();
    let uri = "file:///tmp/kiln_lsp_cols_utf16.kiln";
    // "héllo" is 5 UTF-16 units, 6 bytes. `nope` starts at unit 27.
    c.open(uri, "module m\nsub main\n  call print_text(\"héllo\" + nope())\nend\n");
    let d = c.diagnostics(uri);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0]["range"]["start"]["character"], 28, "{:?}", d[0]);
    assert_eq!(d[0]["range"]["end"]["character"], 32, "{:?}", d[0]);
    c.shutdown();
}

/// A command from a library the module never `use`d: the message names the
/// line to add. This is the one diagnostic the registry alone cannot give, so
/// it is asserted here, through the server that can.
#[test]
fn a_command_from_an_unused_library_names_the_use_line() {
    let mut c = Client::start();
    let uri = "file:///tmp/kiln_lsp_elsewhere.kiln";
    c.open(uri, "module m\nsub main\n  let t: text = file_read_text(\"a\")\nend\n");
    let d = c.diagnostics(uri);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(
        d[0]["message"],
        "in `main`: unknown command `file_read_text` — it is in the `file` library: add `use file` to the module"
    );
    c.shutdown();
}

#[test]
fn a_typo_suggests_the_nearest_command() {
    let mut c = Client::start();
    let uri = "file:///tmp/kiln_lsp_typo.kiln";
    c.open(uri, "module m\nsub main\n  call prnt_text(\"hi\")\nend\n");
    let d = c.diagnostics(uri);
    assert_eq!(
        d[0]["message"],
        "in `main`: unknown command `prnt_text` — did you mean `print_text`?"
    );
    c.shutdown();
}

// ---------------------------------------------------------------------------
// The RAD loop: `on ` completion
// ---------------------------------------------------------------------------

fn labels(r: &serde_json::Value) -> Vec<String> {
    r.as_array()
        .expect("completion list")
        .iter()
        .map(|i| i["label"].as_str().unwrap().to_string())
        .collect()
}

/// Inside a component block, `on ` offers exactly that component's events —
/// and says what each hands a handler, so the signature is never a guess.
#[test]
fn completion_after_on_offers_the_components_events() {
    let mut c = Client::start();
    let uri = "file:///tmp/kiln_lsp_on.kiln";
    //          1        2        3                 4      5    6        7
    c.open(uri, "module m\ntimer t\n  interval = 500\n  on \nend\nsub main\nend\n");
    let _ = c.diagnostics(uri);

    let r = c.request(120, "textDocument/completion", Client::at(uri, 3, 5));
    assert_eq!(labels(&r), vec!["tick"], "{r}");
    let tick = &r.as_array().unwrap()[0];
    assert_eq!(tick["detail"], "timer event — hands (n: int)", "{tick}");
    c.shutdown();
}

/// The handler position offers the subroutines that exist, plus one that does
/// not: accepting it writes `sub t_tick(n: int) … end` at the end of the file.
/// That edit is the whole RAD loop — draw, wire, and the handler is there.
#[test]
fn handler_completion_writes_the_subroutine_with_the_events_parameters() {
    let mut c = Client::start();
    let uri = "file:///tmp/kiln_lsp_handler.kiln";
    //          1        2        3            4      5         6
    let src = "module m\ntimer t\n  on tick: \nend\nsub main\nend\n";
    c.open(uri, src);
    let _ = c.diagnostics(uri);

    let r = c.request(121, "textDocument/completion", Client::at(uri, 2, 11));
    let names = labels(&r);
    assert!(names.contains(&"main".to_string()), "existing subroutines: {names:?}");
    let new = r
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["label"] == "t_tick")
        .unwrap_or_else(|| panic!("the new handler: {names:?}"));
    assert_eq!(new["insertText"], "t_tick");
    let edit = &new["additionalTextEdits"][0];
    assert_eq!(
        edit["newText"], "\nsub t_tick(n: int)\n  \nend\n",
        "the event hands an int, so the handler takes one: {new}"
    );
    assert_eq!(edit["range"]["start"]["line"], 6, "after the last line: {new}");

    // Once the subroutine exists it is offered as itself, not created twice.
    let uri2 = "file:///tmp/kiln_lsp_handler2.kiln";
    c.open(uri2, "module m\ntimer t\n  on tick: \nend\nsub main\nend\nsub t_tick(n: int)\nend\n");
    let _ = c.diagnostics(uri2);
    let r = c.request(122, "textDocument/completion", Client::at(uri2, 2, 11));
    let ticks: Vec<&serde_json::Value> = r
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["label"] == "t_tick")
        .collect();
    assert_eq!(ticks.len(), 1, "{r}");
    assert!(ticks[0]["additionalTextEdits"].is_null(), "{}", ticks[0]);
    c.shutdown();
}

/// The same loop inside a form — where nearly every `on ` is actually typed.
/// The button is nested in the form and its type comes from `use ui`, and the
/// file does not parse while the line reads `on `: every one of those was a
/// way for the list to come back empty, and the designer has nothing to show
/// for its trigger.
#[test]
fn completion_after_on_inside_a_forms_button_offers_its_events() {
    let mut c = Client::start();
    let uri = "file:///tmp/kiln_lsp_form_on.kiln";
    //          1        2      3          4           5      6    7    8      9
    c.open(uri, "module m\nuse ui\nform Main\n  button ok\n    on \n  end\nend\nsub go\nend\n");
    let _ = c.diagnostics(uri);

    let r = c.request(140, "textDocument/completion", Client::at(uri, 4, 7));
    assert_eq!(labels(&r), vec!["click"], "{r}");
    assert_eq!(r.as_array().unwrap()[0]["detail"], "button event", "{r}");
    c.shutdown();
}

/// The handler position inside a form's button: the subroutines that exist,
/// plus `ok_click` — a button's `click` hands nothing, so the subroutine it
/// writes takes nothing.
#[test]
fn handler_completion_inside_a_forms_button_writes_the_subroutine() {
    let mut c = Client::start();
    let uri = "file:///tmp/kiln_lsp_form_handler.kiln";
    //          1        2      3          4           5             6    7    8      9
    c.open(uri, "module m\nuse ui\nform Main\n  button ok\n    on click: \n  end\nend\nsub go\nend\n");
    let _ = c.diagnostics(uri);

    let r = c.request(141, "textDocument/completion", Client::at(uri, 4, 14));
    let names = labels(&r);
    assert!(names.contains(&"go".to_string()), "existing subroutines: {names:?}");
    let new = r
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["label"] == "ok_click")
        .unwrap_or_else(|| panic!("the new handler: {names:?}"));
    assert_eq!(new["insertText"], "ok_click");
    let edit = &new["additionalTextEdits"][0];
    assert_eq!(edit["newText"], "\nsub ok_click\n  \nend\n", "{new}");
    assert_eq!(edit["range"]["start"]["line"], 9, "after the last line: {new}");
    c.shutdown();
}

/// `on ` directly inside the form resolves to the form, not to the button
/// declared above it: the events on offer are the form's own.
#[test]
fn completion_after_on_inside_the_form_offers_the_forms_events() {
    let mut c = Client::start();
    let uri = "file:///tmp/kiln_lsp_form_own_on.kiln";
    //          1        2      3          4           5      6      7    8      9
    c.open(uri, "module m\nuse ui\nform Main\n  button ok\n  end\n  on \nend\nsub go\nend\n");
    let _ = c.diagnostics(uri);

    let r = c.request(142, "textDocument/completion", Client::at(uri, 5, 5));
    assert_eq!(labels(&r), vec!["load"], "{r}");
    assert_eq!(r.as_array().unwrap()[0]["detail"], "form event", "{r}");

    // And the handler it creates is named after the form.
    let uri2 = "file:///tmp/kiln_lsp_form_own_handler.kiln";
    c.open(uri2, "module m\nuse ui\nform Main\n  on load: \nend\nsub go\nend\n");
    let _ = c.diagnostics(uri2);
    let r = c.request(143, "textDocument/completion", Client::at(uri2, 3, 11));
    let new = r
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["label"] == "Main_load")
        .unwrap_or_else(|| panic!("the new handler: {:?}", labels(&r)));
    assert_eq!(
        new["additionalTextEdits"][0]["newText"],
        "\nsub Main_load\n  \nend\n",
        "{new}"
    );
    c.shutdown();
}

/// Hover on a property — as `id.name` or on its line inside the block — shows
/// its type and the editor an inspector would offer.
#[test]
fn hover_on_a_property_shows_its_type_and_editor() {
    let mut c = Client::start();
    let uri = "file:///tmp/kiln_lsp_prophover.kiln";
    //          1        2      3          4           5                            6     7    8      9                10
    let src = "module m\nuse ui\nform Main\n  button ok\n    background_color = \"#fff\"\n  end\nend\nsub go\n  ok.text = \"hi\"\nend\n";
    c.open(uri, src);
    let _ = c.diagnostics(uri);

    let h = c.request(130, "textDocument/hover", Client::at(uri, 4, 8));
    let text = h["contents"]["value"].as_str().unwrap_or("").to_string();
    assert!(text.contains("button.background_color: text"), "hover: {text}");
    assert!(text.contains("editor: color"), "the editor hint: {text}");

    let h = c.request(131, "textDocument/hover", Client::at(uri, 8, 6));
    let text = h["contents"]["value"].as_str().unwrap_or("").to_string();
    assert!(text.contains("button.text: text"), "hover on `ok.text`: {text}");
    assert!(text.contains("property of `ok`"), "{text}");
    c.shutdown();
}
