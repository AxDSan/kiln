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

/// Ask for one request against an open K2 file and return its reply.
fn ask(src: &str, id: i64, method: &str, extra: serde_json::Value) -> serde_json::Value {
    let uri = "file:///ask.kiln";
    let mut reqs = init();
    reqs.push(open(uri, src));
    let mut params = serde_json::json!({"textDocument": {"uri": uri}});
    for (k, v) in extra.as_object().unwrap() {
        params[k] = v.clone();
    }
    reqs.push(serde_json::json!({
        "jsonrpc": "2.0", "id": id, "method": method, "params": params
    }));
    shutdown(&mut reqs);
    lsp(&reqs)
        .into_iter()
        .find(|m| m["id"] == id)
        .unwrap_or_else(|| panic!("no reply to {method}"))
}

/// A whole-line string, written without `\`-continuations: those strip the
/// leading whitespace of the next line, which silently shifts every column.
const SAMPLE: &str = concat!(
    "namespace H;\n",
    "public record Rect(int W, int H)\n",
    "{\n",
    "    public int Area() => W * H;\n",
    "}\n",
    "public static class P\n",
    "{\n",
    "    public static void Main()\n",
    "    {\n",
    "        var r = new Rect(3, 4);\n",
    "        Console.WriteLine($\"{r.Area()}\");\n",
    "    }\n",
    "}\n",
);

#[test]
fn hovering_a_k2_type_describes_it() {
    // The 1.x index is a token scanner for another language: pointed at K2 it
    // would answer confidently and wrongly. This comes from the parse tree.
    let reply = ask(
        SAMPLE,
        11,
        "textDocument/hover",
        serde_json::json!({"position": {"line": 1, "character": 15}}),
    );
    let text = reply["result"]["contents"]["value"].as_str().unwrap_or("");
    assert!(text.contains("record Rect"), "unexpected hover: {reply}");
}

#[test]
fn hovering_a_method_shows_its_signature() {
    let reply = ask(
        SAMPLE,
        12,
        "textDocument/hover",
        serde_json::json!({"position": {"line": 3, "character": 16}}),
    );
    let text = reply["result"]["contents"]["value"].as_str().unwrap_or("");
    assert!(text.contains("int Area()"), "unexpected hover: {reply}");
    assert!(text.contains("method"), "unexpected hover: {reply}");
}

#[test]
fn hovering_a_keyword_explains_it() {
    // `defer` has no declaration to point at, so without this the editor says
    // nothing about exactly the words a newcomer is most likely to hover.
    let src = "namespace K;\npublic static class P\n{\n    public static void Main()\n    {\n        defer Console.WriteLine(\"bye\");\n    }\n}\n";
    let reply = ask(
        src,
        13,
        "textDocument/hover",
        serde_json::json!({"position": {"line": 5, "character": 10}}),
    );
    let text = reply["result"]["contents"]["value"].as_str().unwrap_or("");
    assert!(text.contains("block is left"), "unexpected hover: {reply}");
}

#[test]
fn completion_offers_what_the_file_declares() {
    let reply = ask(
        SAMPLE,
        14,
        "textDocument/completion",
        serde_json::json!({"position": {"line": 10, "character": 8}}),
    );
    let items = reply["result"].as_array().expect("items");
    let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();
    assert!(labels.contains(&"Rect"), "no declared type: {labels:?}");
    assert!(labels.contains(&"Area"), "no declared method: {labels:?}");
    assert!(labels.contains(&"foreach"), "no keywords: {labels:?}");
}

#[test]
fn completion_after_a_dot_offers_only_that_types_members() {
    // `Rect.` is not everything in the file — a list that ignores the dot is
    // the same list you would get by not asking.
    let src = "namespace D2;\npublic record Rect(int W, int H)\n{\n    public int Area() => W * H;\n}\npublic static class P\n{\n    public static void Main()\n    {\n        Rect.\n    }\n}\n";
    let reply = ask(
        src,
        15,
        "textDocument/completion",
        serde_json::json!({"position": {"line": 9, "character": 13}}),
    );
    let items = reply["result"].as_array().expect("items");
    let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();
    assert!(labels.contains(&"Area"), "no member: {labels:?}");
    assert!(labels.contains(&"W"), "no field: {labels:?}");
    assert!(!labels.contains(&"foreach"), "keywords leaked in: {labels:?}");
}

#[test]
fn a_forms_component_is_offered_and_described() {
    let form = "namespace F2;\npublic form MainWindow\n{\n    Label count { Text = \"0\"; }\n    void OnAdd() { count.Text = \"1\"; }\n}\n";
    let reply = ask(
        form,
        16,
        "textDocument/hover",
        serde_json::json!({"position": {"line": 3, "character": 11}}),
    );
    let text = reply["result"]["contents"]["value"].as_str().unwrap_or("");
    assert!(text.contains("Label count"), "unexpected hover: {reply}");

    // And after the dot, its properties.
    let reply = ask(
        form,
        17,
        "textDocument/completion",
        serde_json::json!({"position": {"line": 4, "character": 25}}),
    );
    let labels: Vec<String> = reply["result"]
        .as_array()
        .expect("items")
        .iter()
        .filter_map(|i| i["label"].as_str().map(str::to_string))
        .collect();
    assert!(labels.iter().any(|l| l == "Text"), "no property: {labels:?}");
}

const NAV: &str = concat!(
    "namespace N;\n",                                     // 0
    "public static class P\n",                            // 1
    "{\n",                                                // 2
    "    static int Add(int a, int b) => a + b;\n",       // 3
    "    public static void Main()\n",                    // 4
    "    {\n",                                            // 5
    "        int total = Add(1, 2);\n",                   // 6
    "        // total in a comment is not a use\n",       // 7
    "        if (total > 2) total = total + 1;\n",        // 8
    "        Console.WriteLine($\"{total} \\\"total\\\"\");\n", // 9
    "    }\n",
    "}\n",
);

#[test]
fn go_to_definition_finds_a_k2_local_and_a_method() {
    let reply = ask(NAV, 21, "textDocument/definition",
        serde_json::json!({"position": {"line": 9, "character": 29}}));
    assert_eq!(reply["result"]["range"]["start"], serde_json::json!({"line": 6, "character": 12}), "{reply}");
    let reply = ask(NAV, 22, "textDocument/definition",
        serde_json::json!({"position": {"line": 6, "character": 21}}));
    assert_eq!(reply["result"]["range"]["start"], serde_json::json!({"line": 3, "character": 15}), "{reply}");
}

#[test]
fn references_skip_comments_and_strings_but_see_interpolation() {
    let reply = ask(NAV, 23, "textDocument/references",
        serde_json::json!({"position": {"line": 6, "character": 13}, "context": {"includeDeclaration": true}}));
    let lines: Vec<u64> = reply["result"].as_array().unwrap_or(&vec![]).iter()
        .map(|l| l["range"]["start"]["line"].as_u64().unwrap()).collect();
    // The declaration, three on line 8, the hole on line 9 — not the comment
    // on 7 or the quoted word on 9.
    assert_eq!(lines, vec![6, 8, 8, 8, 9], "{reply}");
    let reply = ask(NAV, 24, "textDocument/references",
        serde_json::json!({"position": {"line": 6, "character": 13}, "context": {"includeDeclaration": false}}));
    assert_eq!(reply["result"].as_array().map(|a| a.len()), Some(4), "{reply}");
}

#[test]
fn signature_help_shows_a_k2_method() {
    let src = NAV.replace("Add(1, 2);", "Add(1, ");
    let reply = ask(&src, 25, "textDocument/signatureHelp",
        serde_json::json!({"position": {"line": 6, "character": 27}}));
    let sig = &reply["result"]["signatures"][0];
    assert!(sig["label"].as_str().unwrap_or("").contains("int Add(int a, int b)"), "{reply}");
    assert_eq!(reply["result"]["activeParameter"], 1, "{reply}");
}
