//! The language server's Kiln 2 support: telling K2 from 1.x, diagnostics from
//! the real front end, and an outline.

use std::io::Write;
use std::process::{Command, Stdio};

/// Drive the server over stdio and return every message it wrote.
fn lsp(requests: &[serde_json::Value]) -> Vec<serde_json::Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .arg("lsp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("kiln lsp");
    {
        let stdin = child.stdin.as_mut().unwrap();
        for r in requests {
            let body = serde_json::to_string(r).unwrap();
            write!(stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        }
    }
    let out = child.wait_with_output().expect("server exits");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let mut msgs = Vec::new();
    let mut rest = text.as_str();
    while let Some(i) = rest.find("Content-Length: ") {
        let after = &rest[i + 16..];
        let Some(nl) = after.find("\r\n\r\n") else {
            break;
        };
        let len: usize = after[..nl].trim().parse().unwrap_or(0);
        let body = &after[nl + 4..];
        if body.len() < len {
            break;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body[..len]) {
            msgs.push(v);
        }
        rest = &body[len..];
    }
    msgs
}

fn open(uri: &str, text: &str) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0", "method": "textDocument/didOpen",
        "params": {"textDocument": {"uri": uri, "languageId": "kiln", "version": 1, "text": text}}
    })
}

fn init() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}),
        serde_json::json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
    ]
}

fn shutdown(v: &mut Vec<serde_json::Value>) {
    v.push(serde_json::json!({"jsonrpc":"2.0","id":99,"method":"shutdown","params":null}));
    v.push(serde_json::json!({"jsonrpc":"2.0","method":"exit","params":null}));
}

fn diagnostics_for(uri: &str, src: &str) -> Vec<serde_json::Value> {
    let mut reqs = init();
    reqs.push(open(uri, src));
    shutdown(&mut reqs);
    let msgs = lsp(&reqs);
    let note = msgs
        .iter()
        .find(|m| m["method"] == "textDocument/publishDiagnostics")
        .unwrap_or_else(|| panic!("no diagnostics published; got {msgs:?}"));
    note["params"]["diagnostics"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

#[test]
fn a_k2_error_is_reported_where_it_is() {
    let bad = "namespace D;\npublic static class P\n{\n    public static void Main()\n    {\n        var x = ;\n    }\n}\n";
    let list = diagnostics_for("file:///a.kiln", bad);
    assert!(!list.is_empty(), "expected an error");
    // On the line the mistake is on, not at the top of the file.
    assert_eq!(list[0]["range"]["start"]["line"], 5, "{list:?}");
}

#[test]
fn a_good_k2_file_reports_nothing() {
    let good = "namespace G;\npublic static class P\n{\n    public static void Main()\n    {\n        var x = 1;\n        Console.WriteLine($\"{x}\");\n    }\n}\n";
    assert_eq!(diagnostics_for("file:///b.kiln", good).len(), 0);
}

#[test]
fn a_lowering_error_is_reported_too() {
    // Parses, but does not compile: the name is unknown.
    let bad = "namespace L;\npublic static class P\n{\n    public static void Main()\n    {\n        Console.WriteLine($\"{nope}\");\n    }\n}\n";
    let list = diagnostics_for("file:///d.kiln", bad);
    assert!(!list.is_empty(), "expected a lowering error");
    assert!(
        list[0]["message"].as_str().unwrap().contains("nope"),
        "{list:?}"
    );
}

#[test]
fn a_form_outlines_its_components() {
    let form = "namespace O;\npublic form MainWindow\n{\n    Title = \"x\";\n    Label count { Text = \"0\"; }\n    Button add { Text = \"Add\"; }\n    int n;\n    void OnAdd() { n = n + 1; }\n}\n";
    let mut reqs = init();
    reqs.push(open("file:///c.kiln", form));
    reqs.push(
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"textDocument/documentSymbol",
        "params":{"textDocument":{"uri":"file:///c.kiln"}}}),
    );
    shutdown(&mut reqs);
    let msgs = lsp(&reqs);
    let reply = msgs.iter().find(|m| m["id"] == 2).expect("a symbol reply");
    let text = reply.to_string();
    for want in ["MainWindow", "count", "add", "OnAdd"] {
        assert!(text.contains(want), "outline missing {want}: {text}");
    }
}
