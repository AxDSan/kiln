/* Headless test for the part of the designer that can destroy user code.
 * Build/run via designer/build.sh test. */
#include <cstdio>
#include <cstdlib>
#include <fstream>
#include <sstream>
#include "json.h"
#include "model.h"

using namespace kiln::designer;

static int failures = 0;
static void check(const char* what, bool ok) {
    std::printf("  %-56s %s\n", what, ok ? "PASS" : "*** FAIL ***");
    if (!ok) failures++;
}

static std::string slurp(const std::string& p) {
    std::ifstream f(p);
    std::ostringstream o;
    o << f.rdbuf();
    return o.str();
}

static void test_json();
static void test_module_components(const std::string& kiln, const NeedsQuotes& quoted);
static void test_multiline(const std::string& kiln);
static void test_round_trip(const std::string& kiln, const NeedsQuotes& quoted);
static void test_rename(const std::string& kiln, const NeedsQuotes& quoted);

int main(int argc, char** argv) {
    const std::string kiln = argc > 1 ? argv[1] : "./target/debug/kiln";
    const std::string fixture = "/tmp/kiln_designer_fixture.kiln";

    // A file with a hand-written subroutine body that MUST survive a save.
    const char* source =
        "module fixture\n"
        "use ui\n"
        "\n"
        "var tally: int = 0\n"
        "\n"
        "form win\n"
        "  title = \"Fixture\"\n"
        "  width = 300\n"
        "  height = 200\n"
        "\n"
        "  button go\n"
        "    text = \"Go\"\n"
        "    left = 10\n"
        "    top = 10\n"
        "    on click: on_go\n"
        "  end\n"
        "end\n"
        "\n"
        "sub on_go\n"
        "  # A comment the designer must not touch.\n"
        "  tally = tally + 7\n"
        "  call print_int(tally)\n"
        "end\n";
    { std::ofstream f(fixture, std::ios::trunc); f << source; }
    const std::string original = slurp(fixture);

    Model m;
    std::string err;
    check("load via `kiln inspect`", load_model(kiln, fixture, m, err));
    if (!err.empty()) std::printf("    error: %s\n", err.c_str());
    check("module name", m.module_name == "fixture");
    check("form span found", m.form_first_line == 6 && m.form_last_line == 17);
    check("one component", m.children.size() == 1 && m.children[0].id == "go");
    check("handler read back", m.children[0].handler("click") &&
                                   *m.children[0].handler("click") == "on_go");
    check("existing sub listed", m.has_sub("on_go"));

    // Edit one property, exactly as the designer would.
    m.find("go")->set_property("left", "42");
    // Stands in for the descriptor table: only the declared type can say which
    // properties are text, and a form `title` written unquoted does not parse.
    auto quoted = [](const std::string&, const std::string& p) {
        return p == "text" || p == "title";
    };
    check("save", save_model(m, {}, quoted, err));

    const std::string saved = slurp(fixture);
    // THE test that matters: the hand-written body survives byte-identical.
    const char* body =
        "sub on_go\n"
        "  # A comment the designer must not touch.\n"
        "  tally = tally + 7\n"
        "  call print_int(tally)\n"
        "end\n";
    check("hand-written subroutine body is byte-identical",
          saved.find(body) != std::string::npos);
    check("module header preserved", saved.find("var tally: int = 0") != std::string::npos);
    check("edit applied", saved.find("left = 42") != std::string::npos);
    check("file actually changed", saved != original);

    // Reload: the saved file must parse and report the edit.
    Model m2;
    check("saved file re-inspects", load_model(kiln, fixture, m2, err));
    check("reloaded value", m2.find("go") && *m2.find("go")->property("left") == "42");

    // Adding a component + wiring a new handler appends a stub.
    Component c;
    c.id = m2.fresh_id("label");
    c.type_name = "label";
    c.set_property("text", "Added");
    c.set_property("left", "10");
    c.set_property("top", "60");
    m2.children.push_back(c);
    m2.find("go")->set_handler("click", "on_go_click2");
    check("save with a new sub", save_model(m2, {"on_go_click2"}, quoted, err));
    const std::string saved2 = slurp(fixture);
    check("stub subroutine appended", saved2.find("sub on_go_click2") != std::string::npos);
    check("original body still intact", saved2.find(body) != std::string::npos);
    check("new component present", saved2.find("label label1") != std::string::npos);

    // Two saves in one session with no reload between them. The first grows
    // the form, so every line below it moves; a save that did not recompute
    // the span would splice the second save over somebody's subroutine.
    Model m3;
    check("reload before the two-save test", load_model(kiln, fixture, m3, err));
    Component extra;
    extra.id = m3.fresh_id("label");
    extra.type_name = "label";
    extra.set_property("text", "Second");
    extra.set_property("left", "10");
    extra.set_property("top", "90");
    m3.children.push_back(extra);
    check("first save of two", save_model(m3, {}, quoted, err));
    m3.find("go")->set_property("top", "77");
    check("second save without reloading", save_model(m3, {}, quoted, err));
    const std::string twice = slurp(fixture);
    check("two saves left the body intact", twice.find(body) != std::string::npos);
    check("two saves left one form", twice.find("form win") == twice.rfind("form win"));
    check("second edit applied", twice.find("top = 77") != std::string::npos);
    Model m4;
    check("file after two saves still parses", load_model(kiln, fixture, m4, err));

    // Undo across a save. The snapshot remembers where the form was BEFORE the
    // save moved it; restoring those line numbers and saving again splices over
    // whatever now lives there, which is somebody's subroutine.
    Model m5;
    check("reload before the undo test", load_model(kiln, fixture, m5, err));
    const Model snapshot = m5;              // what push_undo() keeps
    Component third;
    third.id = m5.fresh_id("label");
    third.type_name = "label";
    third.set_property("text", "Third");
    m5.children.push_back(third);
    check("save that moves the lines below the form", save_model(m5, {}, quoted, err));
    Model restored = snapshot;              // what undo() puts back
    restored.adopt_spans(m5);
    check("save after undo", save_model(restored, {}, quoted, err));
    const std::string after_undo = slurp(fixture);
    check("undo-then-save left the body intact", after_undo.find(body) != std::string::npos);
    check("undo-then-save left one form",
          after_undo.find("form win") == after_undo.rfind("form win"));
    Model m6;
    check("file after undo-then-save still parses", load_model(kiln, fixture, m6, err));

    test_multiline(kiln);
    test_module_components(kiln, quoted);
    test_round_trip(kiln, quoted);
    test_rename(kiln, quoted);
    test_json();


    std::printf("\n%d failure(s)\n", failures);
    return failures == 0 ? 0 : 1;
}

/* --- multi-line property values ----------------------------------------- *
 *
 * `inspect` keeps a value on its one line by escaping it, and the reader
 * reverses that. Before the escape a memo's text arrived as several lines
 * only the first of which was labelled, and the save that followed truncated
 * it to that line — a user losing a paragraph without being told.
 */

static void test_multiline(const std::string& kiln) {
    const std::string fixture = "/tmp/kiln_designer_mlfixture.kiln";
    const char* source =
        "module mlfixture\n"
        "use ui\n"
        "\n"
        "form win\n"
        "  title = \"ML\"\n"
        "\n"
        "  memo notes\n"
        "    text = \"one\\ntwo\\nthree\"\n"
        "    left = 10\n"
        "  end\n"
        "end\n";
    { std::ofstream f(fixture, std::ios::trunc); f << source; }

    Model m;
    std::string err;
    check("multi-line fixture loads", load_model(kiln, fixture, m, err));
    check("all three lines read back",
          m.find("notes") && *m.find("notes")->property("text") == "one\ntwo\nthree");
    check("the property after it still lands",
          m.find("notes") && m.find("notes")->property("left") &&
              *m.find("notes")->property("left") == "10");

    auto quoted = [](const std::string&, const std::string& p) {
        return p == "text" || p == "title";
    };
    check("save a multi-line value", save_model(m, {}, quoted, err));
    const std::string saved = slurp(fixture);
    check("written as an escape, not a raw newline",
          saved.find("text = \"one\\ntwo\\nthree\"") != std::string::npos);
    Model back;
    check("multi-line file still parses", load_model(kiln, fixture, back, err));
    check("round-trips unchanged",
          back.find("notes") && *back.find("notes")->property("text") == "one\ntwo\nthree");
}

/* --- module-level components -------------------------------------------- *
 *
 * A timer has no rectangle, so it is declared beside the form rather than
 * inside it. Writing one INTO the form is source the compiler rejects, which
 * makes this the test that stops the tray from corrupting a project.
 */

static void test_module_components(const std::string& kiln, const NeedsQuotes& quoted) {
    const std::string fixture = "/tmp/kiln_designer_modfixture.kiln";
    const char* source =
        "module modfixture\n"
        "use ui\n"
        "\n"
        "form win\n"
        "  title = \"Mod\"\n"
        "  width = 300\n"
        "  height = 200\n"
        "end\n"
        "\n"
        "sub on_tick\n"
        "  # A comment the designer must not touch.\n"
        "  call print_text(\"tick\")\n"
        "end\n";
    { std::ofstream f(fixture, std::ios::trunc); f << source; }

    Model m;
    std::string err;
    check("module fixture loads", load_model(kiln, fixture, m, err));

    Component t;
    t.id = m.fresh_id("timer");
    t.type_name = "timer";
    t.set_property("interval", "500");
    t.set_handler("tick", "on_tick");
    m.module_components.push_back(t);
    check("save with a module-level component", save_model(m, {}, quoted, err));

    const std::string saved = slurp(fixture);
    check("timer written at module level",
          saved.find("\ntimer timer1\n") != std::string::npos);
    // The one that matters: NOT inside the form, where the compiler refuses it.
    check("timer written after the form's end",
          saved.find("timer timer1") > saved.find("form win"));
    check("handler body untouched",
          saved.find("  # A comment the designer must not touch.") != std::string::npos);
    check("span recorded by the save",
          m.module_components[0].first_line > 0 && m.module_components[0].last_line >
              m.module_components[0].first_line);

    // Editing it and saving again must replace the block, not append a second.
    m.module_components[0].set_property("interval", "250");
    check("second save of the module component", save_model(m, {}, quoted, err));
    const std::string twice = slurp(fixture);
    check("still exactly one timer", twice.find("timer timer1") == twice.rfind("timer timer1"));
    check("module component edit applied", twice.find("interval = 250") != std::string::npos);

    Model back;
    check("file with a module component re-inspects", load_model(kiln, fixture, back, err));
    check("form still found", back.form_first_line == 4 && back.form_last_line == 8);

    // A stale span must be refused rather than spliced over live code.
    Model bad = m;
    bad.module_components[0].first_line = 1;
    bad.module_components[0].last_line = 9999;
    check("out-of-range span is refused", !save_model(bad, {}, quoted, err));

    // A kit the model uses that the file does not yet name: a `tcpserver`
    // dropped into a module that only says `use ui`. The save writes the
    // `use` line after the header, once, and everything below moves down.
    Model n = back;
    n.uses.push_back("net");
    Component srv;
    srv.id = n.fresh_id("tcpserver");
    srv.type_name = "tcpserver";
    srv.set_property("port", "8080");
    n.module_components.push_back(srv);
    check("save with a new kit in use", save_model(n, {}, quoted, err));
    const std::string used = slurp(fixture);
    check("use line written after the existing one",
          used.find("module modfixture\nuse ui\nuse net\n\nform win\n") != std::string::npos);
    check("use line written once", used.find("use net") == used.rfind("use net"));
    check("form span moved down by the inserted line",
          n.form_first_line == 5 && n.form_last_line == 9);
    check("handler body still untouched",
          used.find("  # A comment the designer must not touch.") != std::string::npos);
    check("second save does not add a second use", save_model(n, {}, quoted, err));
    const std::string again = slurp(fixture);
    check("still one use net", again.find("use net") == again.rfind("use net"));
    check("still one tcpserver", again.find("tcpserver tcpserver1") == again.rfind("tcpserver tcpserver1"));
    Model reread;
    check("file with a new use re-inspects", load_model(kiln, fixture, reread, err));
    check("inspect agrees on the spans",
          reread.form_first_line == n.form_first_line && reread.form_last_line == n.form_last_line &&
              reread.module_components.size() == 2 &&
              reread.module_components[1].first_line == n.module_components[1].first_line &&
              reread.module_components[1].last_line == n.module_components[1].last_line);
    check("inspect reports both uses", reread.uses.size() == 2 && reread.uses[1] == "net");
}

/* --- the round trip ----------------------------------------------------- *
 *
 * Everything above is mechanism for this: a file with a form, a memo holding
 * two lines, and a timer declared at module level BELOW a hand-written sub,
 * opened and saved with no edit, comes back byte for byte. The timer sits
 * after the sub so that a span off by one line shows up as a corrupted sub
 * rather than a pass.
 */

static void test_round_trip(const std::string& kiln, const NeedsQuotes& quoted) {
    const std::string fixture = "/tmp/kiln_designer_rtfixture.kiln";
    const char* source =
        "module rtfixture\n"
        "use ui\n"
        "\n"
        "form win\n"
        "  title = \"Round trip\"\n"
        "  width = 300\n"
        "\n"
        "  memo notes\n"
        "    text = \"first line\\nsecond line\"\n"
        "    left = 10\n"
        "  end\n"
        "end\n"
        "\n"
        "sub on_tick\n"
        "  # A comment the designer must not touch.\n"
        "  call print_text(\"tick\")\n"
        "end\n"
        "\n"
        "timer ticker\n"
        "  interval = 500\n"
        "  on tick: on_tick\n"
        "end\n";
    { std::ofstream f(fixture, std::ios::trunc); f << source; }

    Model m;
    std::string err;
    check("round-trip fixture loads", load_model(kiln, fixture, m, err));
    if (!err.empty()) std::printf("    error: %s\n", err.c_str());
    check("timer arrived as a module component, not a child",
          m.module_components.size() == 1 && m.children.size() == 1 &&
              m.module_components[0].id == "ticker");
    check("timer span reported",
          m.module_components.size() == 1 && m.module_components[0].first_line == 19 &&
              m.module_components[0].last_line == 22);
    check("timer handler read back",
          m.find("ticker") && m.find("ticker")->handler("tick") &&
              *m.find("ticker")->handler("tick") == "on_tick");
    check("memo holds both lines",
          m.find("notes") && *m.find("notes")->property("text") == "first line\nsecond line");

    check("save with no edits", save_model(m, {}, quoted, err));
    check("file is byte-identical after the round trip", slurp(fixture) == source);

    // And once more through the same model, since the save just rewrote the
    // spans it will splice at next time.
    check("save again", save_model(m, {}, quoted, err));
    check("still byte-identical after a second save", slurp(fixture) == source);
}

/* --- JSON: the LSP wire format ------------------------------------------ */

/// A rename travels with the model and is written by the save: the block the
/// designer emits carries the new name, the hand-written lines that refer to
/// the component are rewritten as they are copied, and a literal, a comment
/// and a longer word that merely ends in the old name are left alone.
static void test_rename(const std::string& kiln, const NeedsQuotes& quoted) {
    std::printf("rename\n");
    check("a plain word is an identifier", is_identifier("go_button") && is_identifier("_x1"));
    check("a digit first, a dash or a keyword is not",
          !is_identifier("9abc") && !is_identifier("go-button") && !is_identifier("end") &&
              !is_identifier("form") && !is_identifier(""));
    check("references are rewritten on word boundaries only",
          rename_references("  go.text = my_go.text + go.text", "go", "run") ==
              "  run.text = my_go.text + run.text");
    check("a literal and a comment are left alone",
          rename_references("  x.text = \"go.text\"   # go.text", "go", "run") ==
              "  x.text = \"go.text\"   # go.text");
    check("an escaped quote does not end the literal",
          rename_references("  x.text = \"a\\\"go.text\" + go.text", "go", "run") ==
              "  x.text = \"a\\\"go.text\" + run.text");
    check("a handler line names a sub, not an id",
          rename_references("    on click: go", "go", "run") == "    on click: go");

    const std::string fixture = "/tmp/kiln_designer_renamefixture.kiln";
    const char* source =
        "module renamefixture\n"
        "use ui\n"
        "\n"
        "form win\n"
        "  title = \"Rename\"\n"
        "  width = 300\n"
        "\n"
        "  button go\n"
        "    text = \"Go\"\n"
        "    left = 10\n"
        "    on click: on_go\n"
        "  end\n"
        "end\n"
        "\n"
        "timer tick\n"
        "  interval = 100\n"
        "end\n"
        "\n"
        "# go.text is what the button says\n"
        "sub on_go\n"
        "  go.text = \"go.text\"   # go.text\n"
        "  call print_text(go.text)\n"
        "  call timer_stop(tick.name)\n"
        "end\n";
    { std::ofstream f(fixture, std::ios::trunc); f << source; }
    Model m;
    std::string err;
    check("load", load_model(kiln, fixture, m, err));
    check("a taken id is refused", !rename_id(m, "go", "tick", err) && m.find("go"));
    check("a sub's name is refused", !rename_id(m, "go", "on_go", err) && m.find("go"));
    check("a keyword is refused", !rename_id(m, "go", "sub", err) && m.find("go"));
    check("an unknown id is refused", !rename_id(m, "nope", "x", err));
    check("nothing pending after refusals", m.renames.empty());
    check("rename the button", rename_id(m, "go", "run", err) && m.find("run") && !m.find("go"));
    check("rename it again: one rename of the file",
          rename_id(m, "run", "start", err) && m.renames.size() == 1 &&
              m.renames[0].first == "go" && m.renames[0].second == "start");
    check("rename the form", rename_id(m, "win", "main", err) && m.form_name == "main" &&
                                 m.find("main") == &m.form);
    check("rename a tray component", rename_id(m, "tick", "clock", err) && m.find("clock"));
    check("save", save_model(m, {}, quoted, err));
    const std::string saved = slurp(fixture);
    check("the block carries the new name", saved.find("  button start\n") != std::string::npos);
    check("the form carries its new name", saved.find("form main\n") != std::string::npos);
    check("the tray block carries its new name", saved.find("timer clock\n") != std::string::npos);
    check("references in the handler are rewritten",
          saved.find("  call print_text(start.text)\n") != std::string::npos &&
              saved.find("  start.text = \"go.text\"   # go.text\n") != std::string::npos &&
              saved.find("  call timer_stop(clock.name)\n") != std::string::npos);
    check("the comment above the sub is untouched",
          saved.find("# go.text is what the button says\n") != std::string::npos);
    check("the handler line still names the sub", saved.find("    on click: on_go\n") != std::string::npos);
    check("nothing pending after the save", m.renames.empty());
    check("the file still inspects", load_model(kiln, fixture, m, err) && m.find("start") &&
                                         m.form_name == "main");
}

static void test_json() {
    using namespace kiln;

    // Escaping is where an LSP client breaks silently: one unescaped quote or
    // backslash corrupts the frame, the server stops answering, and the editor
    // just looks dead.
    check("escapes quotes", json::escape("say \"hi\"") == "say \\\"hi\\\"");
    check("escapes backslashes", json::escape("a\\b") == "a\\\\b");
    check("escapes newlines", json::escape("a\nb") == "a\\nb");
    check("escapes control characters", json::escape("a\x01") == "a\\u0001");
    check("passes UTF-8 through", json::escape("héllo") == "héllo");

    // A document containing all three at once — the realistic worst case for a
    // source file being sent on every keystroke.
    const std::string nasty = "call print_text(\"a\\nb\")\n";
    const std::string round = json::parse("\"" + json::escape(nasty) + "\"").str();
    check("escape/parse round-trips", round == nasty);

    const json::Value v = json::parse(
        "{\"method\":\"textDocument/publishDiagnostics\",\"params\":{\"uri\":\"file:///x.kiln\","
        "\"diagnostics\":[{\"range\":{\"start\":{\"line\":7,\"character\":0}},"
        "\"severity\":1,\"message\":\"unknown command\"}]}}");
    check("reads the method", v["method"].str() == "textDocument/publishDiagnostics");
    check("reads a nested line number",
          v["params"]["diagnostics"].at(0)["range"]["start"]["line"].num(-1) == 7);
    check("reads the message",
          v["params"]["diagnostics"].at(0)["message"].str() == "unknown command");
    check("counts diagnostics", v["params"]["diagnostics"].size() == 1);

    // Missing fields must be null rather than a crash: a malformed message
    // should degrade, not take the IDE down.
    check("missing members are null", v["nope"]["deeper"].is_null());
    check("out-of-range index is null", v["params"]["diagnostics"].at(9).is_null());
    check("invalid JSON parses to null", json::parse("{oops").is_null());
}
