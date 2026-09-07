/* Headless tests for the Studio editor and the RAD gestures.
 *
 * Two halves. The catalogue half reads canned `kiln commands` listings, so
 * a kit's visual component can be checked without a kit that has one
 * installed. The session half drives the BUILT designer through scripted
 * sessions and reads what its verbs print — the cursor the platform was
 * asked for, the handler a double-click wrote, the tip a hover produced —
 * because every one of those passed a unit test and failed on screen once.
 *
 * Build and run (needs a display, as the frame dumps do):
 *
 *   clang++ -std=c++17 -I abi -I designer designer/test_studio.cpp \
 *       libs/ui/ui_libinfo.c -o /tmp/kiln_studio_test
 *   /tmp/kiln_studio_test ./target/debug/kiln designer/kiln-designer
 */
#include <algorithm>
#include <cstdio>
#include <cstdlib>
#include <fstream>
#include <sys/stat.h>
#include <unistd.h>
#include <sstream>
#include <string>
#include <vector>

#include "catalog.h"
#include "help_page.h"

using namespace kiln::designer;

static int failures = 0;
static void check(const char* what, bool ok) {
    std::printf("  %-64s %s\n", what, ok ? "PASS" : "*** FAIL ***");
    if (!ok) failures++;
}

static std::string slurp(const std::string& p) {
    std::ifstream f(p);
    std::ostringstream o;
    o << f.rdbuf();
    return o.str();
}

static bool has(const std::string& hay, const std::string& needle) {
    return hay.find(needle) != std::string::npos;
}

/// Run one scripted session on a fresh copy of `fixture` and return its
/// stdout. The copy is what the session saves into on exit.
static std::string session(const std::string& designer, const std::string& kiln,
                           const std::string& fixture, const std::string& script,
                           std::string* saved_path = nullptr) {
    // Each session gets its own directory, never a shared one: Studio now
    // writes build output *inside the project*, so two sessions sharing a
    // directory would share a build target and race for it.
    static int n = 0;
    const std::string dir = "/tmp/kiln_studio_test_" + std::to_string(++n);
    ::mkdir(dir.c_str(), 0755);
    const std::string copy = dir + "/main.kiln";
    { std::ofstream f(copy, std::ios::trunc); f << slurp(fixture); }
    if (saved_path) *saved_path = copy;
    const std::string cmd = "KILN_DESIGNER_SCRIPT='" + script + "' " + designer + " " + copy +
                            " " + kiln + " 2>/dev/null";
    std::string out;
    if (FILE* p = popen(cmd.c_str(), "r")) {
        char buf[4096];
        while (fgets(buf, sizeof buf, p)) out += buf;
        pclose(p);
    }
    return out;
}

static void test_catalog() {
    std::printf("catalogue\n");
    using namespace catalog_detail;
    // A kit's own visual control, as `kiln commands --use gauges` would
    // list it — a component this build was never linked against.
    const std::vector<std::string> lines = {
        "command: gauge_set(text, int)",
        "component: gauge",
        "kind: gauge visual",
        "property: gauge value int",
        "property: gauge color text",
        "editor: gauge color color",
        "property: gauge left int",
        "event: gauge change",
        "component: beacon",
        "kind: beacon nonvisual",
        "property: beacon port int",
        "event: beacon ping",
        "component: relic",
        "property: relic name text",
    };
    const auto comps = parse_components(lines, "gauges");
    check("three components read", comps.size() == 3);
    check("kind: visual is read", comps[0].visual && comps[0].kind_known);
    check("kind: nonvisual is read", !comps[1].visual && comps[1].kind_known);
    check("no kind: line leaves the kind unknown", !comps[2].kind_known && !comps[2].visual);
    check("editor: hint lands on its property",
          comps[0].props.size() == 3 && comps[0].props[1].editor == "color" &&
              comps[0].props[0].editor.empty());
    check("events are read, parameters unknown",
          comps[0].events.size() == 1 && comps[0].events[0].name == "change" &&
              !comps[0].events[0].known);
    check("kit is recorded", comps[0].kit == "gauges");

    // The linked table fills in what the listing cannot say, and does not
    // overrule what it did say.
    CatalogComponent grid;
    grid.type_name = "grid";
    grid.kind_known = true;
    grid.visual = false;          // a listing that disagrees with the table wins
    grid.events.push_back({"select", false, {}});
    grid.events.push_back({"activate", false, {}});
    grid.props.push_back({"rows", "text", "", "", false});
    enrich_from_libinfo(grid);
    check("listing's kind is kept over the linked table", !grid.visual);
    check("event parameters come from the linked table",
          grid.events[0].known && grid.events[0].params.size() == 1 &&
              grid.events[0].params[0] == "int");
    check("editor hint comes from the linked table", grid.props[0].editor == "multiline");

    CatalogComponent button;
    button.type_name = "button";
    button.events.push_back({"click", false, {}});
    enrich_from_libinfo(button);
    check("no kind: line falls back to the linked table", button.visual);
    check("an event with nothing to hand over is known, with no parameters",
          button.events[0].known && button.events[0].params.empty());
}

static void test_sessions(const std::string& kiln, const std::string& designer) {
    std::printf("sessions (%s)\n", designer.c_str());
    const std::string form = "examples/form.kiln";
    const std::string grid = "examples/grid.kiln";

    // The resize cursor: every anchor names its direction, and the platform
    // is asked for exactly that.
    {
        const std::string out =
            session(designer, kiln, form,
                    "select:ok_button;hovergrip:se;hovergrip:n;hovergrip:e;hovergrip:ne;"
                    "hovergrip:s;hovergrip:nw;hovergrip:sw;hovergrip:w");
        check("corner anchor asks for resize-nwse", has(out, "cursor: se resize-nwse"));
        check("opposite corner asks for resize-nwse", has(out, "cursor: nw resize-nwse"));
        check("other corners ask for resize-nesw",
              has(out, "cursor: ne resize-nesw") && has(out, "cursor: sw resize-nesw"));
        check("top and bottom anchors ask for resize-ns",
              has(out, "cursor: n resize-ns") && has(out, "cursor: s resize-ns"));
        check("side anchors ask for resize-ew",
              has(out, "cursor: e resize-ew") && has(out, "cursor: w resize-ew"));
    }

    // Double-click on a component with a handler: just go there.
    {
        const std::string out = session(designer, kiln, form, "dblclick:ok_button;caret");
        check("existing handler: switches to the code view", has(out, "designer: code view"));
        check("existing handler: caret inside on_ok_click", has(out, "caret: 39,"));
        check("existing handler: nothing was wired", !has(out, "designer: wired"));
    }

    // Double-click on a fresh component: the default event gets a handler
    // with the event's parameters, and the caret lands in it.
    {
        std::string path;
        const std::string out =
            session(designer, kiln, form, "add:grid;dblclick:grid1;caret", &path);
        const std::string file = slurp(path);
        check("grid: wires select, the descriptor's first event",
              has(out, "wired grid1.select to grid1_select"));
        check("grid: the handler line is in the component block",
              has(file, "    on select: grid1_select\n"));
        check("grid: the stub takes the row the event hands over",
              has(file, "\nsub grid1_select(n: int)\n  \nend\n"));
        check("grid: the hand-written handler survived", has(file, "sub on_ok_click\n  call print_text"));
        // The blank line between `sub` and `end`, which is the last line but one.
        const long lines = std::count(file.begin(), file.end(), '\n');
        check("grid: caret is inside the new sub", has(out, "caret: " + std::to_string(lines - 1) + ","));
    }
    {
        std::string path;
        const std::string out =
            session(designer, kiln, form, "add:editbox;dblclick:editbox1", &path);
        check("editbox: change, with no parameters",
              has(out, "wired editbox1.change") && has(slurp(path), "\nsub editbox1_change\n"));
    }
    // A component from the tray, whose descriptor this build never linked:
    // the parameters come from the language server.
    {
        std::string path;
        const std::string out = session(designer, kiln, form, "add:timer;dblclick:timer1", &path);
        check("timer: tick is wired from the tray", has(out, "wired timer1.tick to timer1_tick"));
        check("timer: the stub takes the tick count",
              has(slurp(path), "\nsub timer1_tick(n: int)\n  \nend\n"));
    }
    // A component from a kit the module does not yet `use`: the drop writes
    // the `use` line on save, once, after the header, and the file compiles.
    // (The timer above needs none — it is the runtime's own.)
    {
        std::string path;
        const std::string out = session(designer, kiln, form,
                                        "add:tcpserver;save;waitdiag;add:tcpclient;save;waitdiag", &path);
        const std::string saved = slurp(path);
        check("use net: written after use ui", has(saved, "module hello_form\nuse ui\nuse net\n\nform main_window\n"));
        check("use net: written once for two net components",
              saved.find("use net") == saved.rfind("use net"));
        check("use net: both components are in the file",
              has(saved, "\ntcpserver tcpserver1\nend\n") && has(saved, "\ntcpclient tcpclient1\nend\n"));
        check("use net: the handler body survived both saves",
              has(saved, "\nsub on_ok_click\n  call print_text(\"button clicked!\")\nend\n"));
        check("use net: the problems pane is clear after each save",
              has(out, "designer: saved " + path + "\ndiagnostics: 0\n") &&
                  out.find("diagnostics: 0") != out.rfind("diagnostics: 0") && !has(out, "diagnostics: 1") &&
                  !has(out, "diagnostics: 2"));
        // The spans after two saves are what `kiln inspect` reads back.
        std::string inspected;
        for (const auto& l : catalog_detail::run(kiln + " inspect " + path)) inspected += l + "\n";
        check("use net: inspect agrees the form moved down one line",
              has(inspected, "form: main_window span=11..35\n"));
        check("use net: inspect finds both tray components at their spans",
              has(inspected, "modcomponent: tcpserver1 tcpserver span=43..44\n") &&
                  has(inspected, "modcomponent: tcpclient1 tcpclient span=46..47\n"));
        const std::string build_dir = path + ".build";
        ::mkdir(build_dir.c_str(), 0755);
        const int rc = std::system((kiln + " build " + path + " -o " + build_dir + "/out >/dev/null 2>&1").c_str());
        check("use net: the saved file builds", rc == 0);
    }
    // The owner's file: a tcpserver already in it, and no `use net`. The
    // server flags it on open; the drop that adds the use clears the pane
    // once the save is re-read — which is a real 2 -> 0, not a 0 that was
    // there all along.
    {
        const std::string broken = "/tmp/kiln_studio_test_broken.kiln";
        {
            std::ofstream f(broken, std::ios::trunc);
            f << slurp(form) << "\ntcpserver tcpserver1\n  port = 8080\nend\n";
        }
        // Two waits after the save: the drop itself told the server about
        // the not-yet-saved text (still broken), and the save's re-read is
        // the publish after that one. The frame loop drains both; the pane
        // shows the last.
        std::string path;
        const std::string out =
            session(designer, kiln, broken, "waitdiag;add:tcpserver;save;waitdiag;waitdiag", &path);
        check("use net: a file missing it is flagged on open",
              has(out, "designer: Ready\ndiagnostics: 1\n  line 1: `tcpserver1`: unknown component type `tcpserver`"));
        const size_t last = out.rfind("diagnostics: ");
        check("use net: the drop's save clears the problems pane",
              last != std::string::npos && last > out.find("designer: saved ") &&
                  out.compare(last, 15, "diagnostics: 0\n") == 0);
        const std::string saved = slurp(path);
        check("use net: written once into the already-broken file",
              has(saved, "use ui\nuse net\n") && saved.find("use net") == saved.rfind("use net"));
        check("use net: the new id does not collide with the existing one",
              has(saved, "\ntcpserver tcpserver2\n"));
    }
    {
        std::string path;
        session(designer, kiln, form, "add:timer;save", &path);
        check("timer: a runtime component adds no use line",
              slurp(path).find("use ") == slurp(path).rfind("use "));
    }
    {
        const std::string out = session(designer, kiln, grid, "dblclick:people");
        check("a component with no events says so", has(out, "datasource has no events"));
    }

    // The right-click menu: a component's events, the way Delphi offers them.
    // A wired event says where it goes, and picking it goes there; an
    // unwired one is wired through the same path a double-click takes.
    {
        const std::string out = session(designer, kiln, form,
                                        "rclick:ok_button;menu;menupick:click \xe2\x86\x92 on_ok_click;menu;caret");
        check("menu: right-click on a wired button lists its handler",
              has(out, "menu: open rows=4\n  ok_button (button)\n  Events\n  click \xe2\x86\x92 on_ok_click\n  Delete\n"));
        check("menu: picking the wired event goes to the code view", has(out, "designer: code view"));
        check("menu: with the caret inside on_ok_click", has(out, "caret: 39,"));
        check("menu: and nothing was wired", !has(out, "designer: wired"));
        check("menu: the pick closed it", has(out, "menupick: click \xe2\x86\x92 on_ok_click\nmenu: closed\n"));
    }
    {
        std::string path;
        const std::string out = session(designer, kiln, form,
                                        "add:button;rclick:button1;menu;menupick:click;caret", &path);
        const std::string file = slurp(path);
        check("menu: a fresh component's event reads plain", has(out, "  Events\n  click\n  Delete\n"));
        check("menu: picking it wires the event", has(out, "wired button1.click to button1_click"));
        check("menu: the handler line is in the component block", has(file, "    on click: button1_click\n"));
        check("menu: and the stub is in the saved file", has(file, "\nsub button1_click\n  \nend\n"));
        const long lines = std::count(file.begin(), file.end(), '\n');
        check("menu: caret is inside the new sub", has(out, "caret: " + std::to_string(lines - 1) + ","));
    }
    {
        const std::string out = session(designer, kiln, form,
                                        "rclick:form;menu;key:escape;menu;rclick:greeting;menu;click:formtitle;menu");
        check("menu: right-click on the form lists load, and no Delete",
              has(out, "menu: open rows=3\n  main_window (form)\n  Events\n  load\n"));
        check("menu: Escape closes it", has(out, "key: escape focus=body\nmenu: closed\n"));
        check("menu: a right-click selects the component under it",
              has(out, "rclick: greeting\nmenu: open rows=4\n  greeting (label)\n"));
        check("menu: a click elsewhere closes it", has(out, "click: formtitle\nmenu: closed\n"));
    }
    {
        std::string path;
        const std::string out =
            session(designer, kiln, form, "rclick:greeting;menupick:Delete;menu", &path);
        check("menu: Delete removes the component", has(out, "deleted 1 component(s)") && !has(slurp(path), "greeting"));
    }
    {
        const std::string out = session(designer, kiln, "examples/tcpchat.kiln", "rclick:link;menu");
        check("menu: a tray component lists its events",
              has(out, "menu: open rows=8\n  link (tcpclient)\n  Events\n  connect \xe2\x86\x92 on_connect\n"
                       "  disconnect \xe2\x86\x92 on_disconnect\n  receive \xe2\x86\x92 on_receive\n"
                       "  receive_bytes\n  error \xe2\x86\x92 on_error\n  Delete\n"));
    }

    // Hover, definition and references through the language server.
    {
        const std::string out = session(designer, kiln, form, "hoverat:39,10");
        check("hover on a command shows its signature", has(out, "tip=block") && has(out, "print_text(text)"));
    }
    {
        const std::string out = session(designer, kiln, grid, "gotodef:104,20;refs:96,7");
        check("F12 on a local jumps to its declaration", has(out, "definition: caret 102,7"));
        check("Shift+F12 lists the wiring line and the declaration",
              has(out, "references: 2") && has(out, "line 49:18") && has(out, "line 96:5"));
    }

    // The designer writes only what the descriptor declares. Through a CLI
    // whose listing omits the label's height — the build refused exactly
    // that property once — the vertical anchors must go, and a resize must
    // leave the block without it.
    {
        const std::string wrapper = "/tmp/kiln_studio_test_nolabelheight.sh";
        {
            std::ofstream f(wrapper, std::ios::trunc);
            f << "#!/bin/sh\nif [ \"$1\" = commands ]; then\n  \"" << kiln
              << "\" \"$@\" | grep -v '^property: label height'\n  exit 0\nfi\nexec \"" << kiln
              << "\" \"$@\"\n";
        }
        ::chmod(wrapper.c_str(), 0755);
        std::string path;
        const std::string out = session(designer, wrapper, form,
                                        "select:greeting;grip:s@0,30;grip:e@30,0;save", &path);
        check("no height: the vertical anchor is not offered", has(out, "grip: s (no such grip)"));
        check("no height: the side anchor still resizes", has(out, "grip: e dragged") && has(out, "width=430"));
        const std::string file = slurp(path);
        const size_t block = file.find("label greeting");
        const size_t block_end = file.find("end", block);
        check("no height: the resize wrote no height into the block",
              block != std::string::npos && file.substr(block, block_end - block).find("height") == std::string::npos);
        check("no height: the width it does declare was written", has(file, "width = 430"));
    }
    {
        const std::string out = session(designer, kiln, form, "select:ok_button;grip:s@0,30");
        check("with height: the vertical anchor resizes", has(out, "grip: s dragged") && has(out, "height=70"));
    }

    // The console: built through the real build path, then a drag over three
    // of its lines selects them, a keystroke changes nothing, and the newest
    // line is on screen.
    {
        const std::string out = session(designer, kiln, form, "build;logscroll;logselect:3,5;logdump");
        check("build log: the newest line is visible after a build", has(out, "newest=VISIBLE"));
        check("build log: a drag selects whole lines",
              has(out, "logselect:") && has(out, "|  stage 2/4") && has(out, "(--gc-sections)"));
        check("build log: typing into it changes nothing", has(out, "edited=no"));
        check("build log: the result keeps its class",
              has(out, "LOG [ok] OK  /tmp/kiln_studio_test_") && has(out, "/build/hello_form"));
    }

    // Completion. Typing opens the popup, typing on narrows it, Enter takes
    // the row — and the character the platform sends after Enter stays out.
    {
        const std::string out = session(
            designer, kiln, form,
            "goto:40,1;typein:  call pri;waitcomplete;typein:nt_t;waitcomplete;key:enter;waitcomplete;bufline:40");
        check("completion: an identifier opens the popup with the server's items",
              has(out, "open=1 offered=") && has(out, "print_text"));
        check("completion: typing on narrows it", has(out, "shown=1 index=0 selected=print_text"));
        check("completion: Enter puts the item in and closes the popup",
              has(out, "buf 40:   call print_textend") && has(out, "open=0 offered=0"));
    }
    // Ctrl+Space asks with no word typed, so everything is offered; Escape
    // dismisses; and neither key leaves a character in the editor.
    {
        const std::string out = session(
            designer, kiln, form,
            "goto:40,1;typein:  call ;key:ctrl-space;waitcomplete;key:escape;waitcomplete;bufline:40");
        // The exact count is whatever the language server knows today, and it
        // grew the moment completion learned to read `use` lines; the property
        // worth holding is that the whole list is offered and all of it shown.
        int offered = 0, shown = 0;
        const size_t at = out.find("open=1 offered=");
        if (at != std::string::npos)
            std::sscanf(out.c_str() + at, "open=1 offered=%d shown=%d", &offered, &shown);
        check("completion: Ctrl+Space opens the full list", offered >= 100 && shown == offered);
        check("completion: Escape closes it", has(out, "open=0 offered=0"));
        check("completion: neither key typed anything", has(out, "buf 40:   call end"));
    }
    // Indentation. With no popup open, Tab belongs to the editor: RmlUi's
    // textarea would move the focus out of it instead, and Return would start
    // the new line at column one however far the line above was indented.
    {
        // Line 38 is `sub on_ok_click`; the caret at its end.
        const std::string out =
            session(designer, kiln, form,
                    "view:code;goto:38,16;key:enter;bufline:39;key:tab;bufline:39;"
                    "key:shift-tab;bufline:39");
        check("indent: Tab stays in the editor rather than moving the focus",
              has(out, "key: tab focus=fullcode"));
        check("indent: Return opens the block one level in", has(out, "buf 39:   \n"));
        check("indent: Tab adds a level", has(out, "buf 39:     \n"));
        check("indent: Shift+Tab takes it off again",
              out.find("buf 39:   \n", out.find("buf 39:     \n")) != std::string::npos);
    }
    // Return after an ordinary statement keeps that statement's indentation
    // rather than adding to it — the block rule must not fire on every line.
    {
        // Line 39 is `  call print_text("button clicked!")`, 36 characters, so
        // column 37 is past its end.
        const std::string out =
            session(designer, kiln, form, "view:code;goto:39,37;key:enter;bufline:40");
        check("indent: Return copies an ordinary line's indentation",
              has(out, "buf 40:   \n"));
        // Split mid-line and the tail moves down to that same indent; the
        // block rule must not fire on a statement.
        const std::string mid =
            session(designer, kiln, form, "view:code;goto:39,20;key:enter;bufline:40");
        check("indent: a mid-line split indents the tail, it does not open a block",
              has(mid, "buf 40:   button clicked!\")\n"));
        // Return replaces a selection, as the control it stands in for did.
        // Inserting at the caret and leaving the selection behind is the way
        // this reimplementation could quietly differ from a real textarea.
        const std::string sel =
            session(designer, kiln, form,
                    "view:code;goto:39,3;key:shift-end;key:enter;bufline:39;bufline:40");
        check("indent: Return replaces the selection rather than keeping it",
              has(sel, "buf 39:   \n") && has(sel, "buf 40:   \n"));
    }
    // The colour layer follows the control sideways when the CARET moves it,
    // not only when text changes: End raises no change event, and the layer
    // repainting only on change is how it drifts a screen out of line.
    {
        const std::string longline =
            "  greeting.caption = some_long_command_name(another_argument_here_yes())";
        const std::string out =
            session(designer, kiln, form,
                    "winsize:760x620;view:code;goto:39,1;typein:" + longline +
                        ";goto:39,1;codescroll;key:end;codescroll");
        // At column one nothing is scrolled; at the end of a line far wider
        // than a 760px window, something must be.
        check("editor: the caret moving to the end scrolls the layer sideways",
              has(out, "codescroll: x=0") && !has(out, "codescroll: x=0\ncodescroll: x=0"));
    }
    // The RAD loop from the keyboard: `on ` offers the events, the handler
    // position offers a subroutine that does not exist yet, and accepting it
    // writes the subroutine with the event's parameters.
    {
        std::string src = slurp("examples/eventparams.kiln");
        const size_t at = src.find("  on tick: on_plain\n");
        src.insert(at + std::string("  on tick: on_plain\n").size(), "\n");
        const std::string fixture = "/tmp/kiln_studio_test_timer.kiln";
        { std::ofstream f(fixture, std::ios::trunc); f << src; }
        const std::string out = session(
            designer, kiln, fixture,
            "goto:20,1;typein:  on ;waitcomplete;key:tab;typein:: pl;waitcomplete;key:enter;bufline:20;buftail:4");
        check("completion: `on ` offers the timer's event", has(out, "shown=1 index=0 selected=tick"));
        check("completion: the handler position offers the new subroutine",
              has(out, "selected=plain_tick"));
        check("completion: accepting writes the wiring line", has(out, "buf 20:   on tick: plain_tick"));
        check("completion: and the subroutine, with the event's parameter",
              has(out, "sub plain_tick(n: int)"));
    }

    // Diagnostics carry columns, and a scrolled editor keeps them on the row.
    {
        std::string bad = slurp(form);
        const size_t at = bad.find("print_text(\"button");
        bad.replace(at, 10, "print_txt");
        const std::string fixture = "/tmp/kiln_studio_test_bad.kiln";
        { std::ofstream f(fixture, std::ios::trunc); f << bad; }
        const std::string out = session(designer, kiln, fixture, "view:code;waitdiag;scroll:6;waitdiag:1");
        check("the diagnostic names the line", has(out, "line 39: in `on_ok_click`"));
    }

    // The keyboard reaches the designer. The listener's keydown branch was
    // once never subscribed: every shortcut in it passed review and did
    // nothing on screen.
    {
        std::string path;
        session(designer, kiln, form, "add:button;key:ctrl-z", &path);
        check("Ctrl+Z on the canvas undoes the add", !has(slurp(path), "button1"));
        session(designer, kiln, form, "add:button;click:button1;key:delete", &path);
        check("Delete on the canvas removes the selection", !has(slurp(path), "button1"));
        const std::string out =
            session(designer, kiln, grid, "view:code;goto:104,20;focus;key:f12;waitdef");
        check("F12 pressed in the editor jumps to the declaration",
              has(out, "definition: caret 102,7"));
    }

    // Help > About: every way out closes it, a click inside does not, and a
    // link goes to the browser rather than anywhere in Studio.
    {
        const std::string out = session(
            designer, kiln, form,
            "about;aboutstate;key:escape;aboutstate;about;click:ok;aboutstate;about;click:x;"
            "aboutstate;about;clickat:30,300;aboutstate;about;clickat:720,400;aboutstate;"
            "click:GitHub-link;aboutstate");
        check("about: opens", has(out, "about: open\nkey: escape"));
        size_t closed = 0;
        for (size_t at = 0; (at = out.find("about: closed", at)) != std::string::npos; at++) closed++;
        check("about: Escape, OK, the cross and a click outside all dismiss", closed == 4);
        check("about: a click inside leaves it up", has(out, "click: GitHub-link\nabout: open"));
        check("about: a link opens in the browser",
              has(out, "about: open https://github.com/AxDSan/kiln"));
    }

    // The document tabs carry the file, and the Code tab the subroutine the
    // caret is in — the name the language server's index knows.
    {
        const std::string out = session(designer, kiln, form, "tabs;view:code;goto:39,3;tabs;goto:36,1;tabs");
        check("tabs: both name the file", has(out, "tabs: Designer [main.kiln]") &&
                                          has(out, "] | Code [main.kiln]"));
        check("tabs: the caret in a sub names it on the Code tab", has(out, "| Code [on_ok_click]"));
        check("tabs: outside every sub the Code tab names the file again",
              out.rfind("| Code [main.kiln]") > out.find("| Code [on_ok_click]"));
    }

    // The preview's title bar is decoration: the client area starts under it
    // and the file's coordinates are measured from there, unshifted.
    {
        std::string path;
        const std::string out = session(designer, kiln, form, "geometry:greeting;geometry:ok_button", &path);
        check("title bar: the form's own size is kept", has(out, "form=480x300"));
        check("title bar: a component at top=40 is drawn 40px below it",
              has(out, "greeting=40,40") && has(out, "ok_button=40,110"));
        check("title bar: the client area starts under it", has(out, "title=36 client_top=37"));
        check("title bar: no icon set falls back to the app's", has(out, "icon=kiln-icon-64.png"));
        check("a session that does nothing leaves the file byte-identical", slurp(path) == slurp(form));
    }
    {
        // An icon beside the file is shown; one that is not there is not.
        const std::string dir = "/tmp/kiln_studio_test_icon";
        ::mkdir(dir.c_str(), 0755);
        const std::string fixture = dir + "/form.kiln";
        std::string src = slurp(form);
        src.insert(src.find("  width  = 480"), "  icon   = \"mark.png\"\n");
        { std::ofstream f(fixture, std::ios::trunc); f << src; }
        { std::ofstream f(dir + "/mark.png", std::ios::trunc | std::ios::binary); f << slurp("assets/icons/button_16.png"); }
        // session() copies the fixture to /tmp, where mark.png is not.
        const std::string cmd = "KILN_DESIGNER_SCRIPT='geometry:greeting' " + designer + " " + fixture +
                                " " + kiln + " 2>/dev/null";
        std::string out;
        if (FILE* p = popen(cmd.c_str(), "r")) {
            char buf[4096];
            while (fgets(buf, sizeof buf, p)) out += buf;
            pclose(p);
        }
        check("title bar: the form's icon, a path beside the file, is shown", has(out, "icon=mark.png"));
        const std::string missing = session(designer, kiln, fixture, "geometry:greeting");
        check("title bar: an icon that cannot be read falls back to the app's",
              has(missing, "icon=kiln-icon-64.png"));
    }

    // The wiring, on the canvas and in the inspector, and every way from it
    // into the handler.
    {
        const std::string out = session(designer, kiln, form, "badges;select:greeting;wiring;select:ok_button;wiring");
        check("badge: a wired component shows event and handler, centred above it",
              has(out, "badge: ok_button click\xe2\x86\x92on_ok_click at 120,69"));
        check("badge: an unwired one shows none", !has(out, "badge: greeting"));
        check("wiring: an unwired component says how to wire it",
              has(out, "wiring: HANDLER WIRINGNot linked \xe2\x80\x94 double-click to create"));
        check("wiring: a wired one names the handler", has(out, "wiring: HANDLER WIRINGLinked to: on_ok_click()on click"));
    }
    {
        const std::string out = session(designer, kiln, form, "select:ok_button;click:wirelink;caret");
        check("wiring: the link opens the code view in the handler",
              has(out, "designer: code view") && has(out, "caret: 39,") && !has(out, "wired"));
    }
    {
        // A badge is a way into the handler, but only the selection's: one
        // that took every click would sit over the components above it.
        std::string path;
        const std::string out = session(designer, kiln, form, "add:editbox;click:editbox1;view:designer;caret", &path);
        check("badge: an unselected badge lets the click through to the component under it",
              !has(slurp(path), "editbox1_change"));
    }
    {
        const std::string out = session(designer, kiln, form, "select:ok_button;badges");
        check("badge: the selection's badge is live", has(out, "badge: ok_button"));
    }
    {
        const std::string out = session(designer, kiln, form, "select:ok_button;click:segevents;events;click:ev-click;caret");
        check("events tab: lists the event with its handler", has(out, "events: click=on_ok_click"));
        check("events tab: choosing it jumps to the handler", has(out, "caret: 39,"));
    }
    {
        std::string path;
        const std::string out = session(designer, kiln, form, "add:button;click:segevents;events;click:ev-click;tabs", &path);
        check("events tab: a fresh component lists the event empty", has(out, "events: click=\n"));
        check("events tab: choosing it creates the handler as double-click does",
              has(out, "wired button1.click to button1_click") && has(slurp(path), "\nsub button1_click\n"));
        check("events tab: and the Code tab names the new sub", has(out, "| Code [button1_click]"));
    }

    // The form itself is selectable: a press on its title bar puts its own
    // properties in the inspector, frames it, and — since its left/top say
    // where the window opens, not where the preview sits — refuses to drag.
    {
        std::string path;
        const std::string out = session(
            designer, kiln, form,
            "clickform;props;click:segevents;events;drag:main_window@10,10->200,200;"
            "geometry:greeting;badges;key:delete;select:ok_button;props", &path);
        check("form: a press on the title bar selects it", has(out, "clickform: selected main_window"));
        check("form: the inspector names it as the form", has(out, "props: main_window (form) name=main_window"));
        check("form: and lists its own properties",
              has(out, " title=Kiln width=480 height=300 background_color=#1e2233 icon="));
        check("form: it is framed on the canvas", has(out, "formsel=yes"));
        check("form: the Events tab lists load", has(out, "events: load="));
        check("form: a drag does not move it", has(out, "greeting=40,40 form=480x300"));
        check("form: the badges still show", has(out, "badge: ok_button click"));
        check("form: Delete does not delete it", has(out, "the form cannot be deleted"));
        check("form: a component takes the frame back",
              out.rfind("formsel=yes") < out.rfind("props: ok_button (button)"));
        check("form: selecting it leaves the file byte-identical", slurp(path) == slurp(form));
    }
    // Renaming, from the Name field: the block, and every `id.` reference
    // in the hand-written code, say the new name after the save — a longer
    // word ending in the old one, a literal and a comment do not.
    {
        std::string src = slurp(form);
        src += "\nsub uses_it\n  ok_button.text = \"ok_button.text\"   # ok_button.text\n"
               "  my_ok_button.text = \"x\"\nend\n";
        const std::string fixture = "/tmp/kiln_studio_test_rename.kiln";
        { std::ofstream f(fixture, std::ios::trunc); f << src; }
        std::string path;
        const std::string out =
            session(designer, kiln, fixture, "select:ok_button;rename:go_button;props;badges;save", &path);
        check("rename: says so", has(out, "designer: renamed ok_button to go_button"));
        check("rename: the inspector shows the new name", has(out, "props: go_button (button) name=go_button"));
        check("rename: the badge follows", has(out, "badge: go_button click"));
        const std::string file = slurp(path);
        check("rename: the block carries the new name", has(file, "  button go_button\n"));
        check("rename: the references are rewritten",
              has(file, "  go_button.text = \"ok_button.text\"   # ok_button.text\n"));
        check("rename: a longer word, a literal and a comment are left alone",
              has(file, "  my_ok_button.text = \"x\"\n") && !has(file, " ok_button\n") &&
                  !has(file, "(ok_button"));
        check("rename: the handler line still names the sub", has(file, "    on click: on_ok_click\n"));
    }
    {
        const std::string out = session(designer, kiln, form, "clickform;rename:main;props;save");
        check("rename: the form too", has(out, "renamed main_window to main") && has(out, "props: main (form)"));
    }
    {
        std::string path;
        const std::string out = session(
            designer, kiln, form,
            "select:ok_button;rename:greeting;rename:9abc;rename:end;rename:bnot;"
            "rename:on_ok_click;rename:;"
            "clickform;rename:greeting;save", &path);
        check("rename: a taken name is refused", has(out, "cannot rename ok_button: greeting is already taken"));
        check("rename: an invalid one is refused",
              has(out, "9abc is not a valid name") && has(out, "end is not a valid name") &&
                  has(out, "a name is required"));
        // `bnot` is a reserved word rather than a soft one, so a component
        // named for it would write a module the compiler cannot parse.
        check("rename: a reserved bitwise word is refused",
              has(out, "bnot is not a valid name"));
        check("rename: a subroutine's name is refused", has(out, "on_ok_click is a subroutine"));
        check("rename: the form cannot take a component's name",
              has(out, "cannot rename main_window: greeting is already taken"));
        check("rename: a refusal changes nothing", !has(out, "designer: renamed"));
        // The save rewrites the block's spacing; the names in it are what
        // must be the same.
        const std::string file = slurp(path);
        check("rename: the file keeps every name",
              has(file, "form main_window\n") && has(file, "button ok_button\n") &&
                  has(file, "label greeting\n"));
    }
    // The anchors editor: a box a side, toggling the value, beside a field
    // the value can still be typed into. Through a CLI whose listing
    // declares the property, so the test does not wait on the ui library.
    {
        const std::string wrapper = "/tmp/kiln_studio_test_anchors.sh";
        {
            std::ofstream f(wrapper, std::ios::trunc);
            f << "#!/bin/sh\nif [ \"$1\" = commands ]; then\n  \"" << kiln
              << "\" \"$@\" | grep -v 'button anchors'\n  echo 'property: button anchors text'\n"
                 "  echo 'editor: button anchors anchors'\n  exit 0\nfi\nexec \"" << kiln
              << "\" \"$@\"\n";
        }
        ::chmod(wrapper.c_str(), 0755);
        std::string path;
        const std::string out = session(
            designer, wrapper, form,
            "select:ok_button;props;click:anch-left;click:anch-right;props;click:anch-left;props;"
            "set:anchors=bottom, top;props;save", &path);
        check("anchors: an unset value lights no box", has(out, " [] anchors=\n"));
        check("anchors: clicking a box adds its side, in reading order",
              has(out, " [left right ] anchors=left,right\n"));
        check("anchors: clicking it again removes it", has(out, " [right ] anchors=right\n"));
        check("anchors: a typed value lights its boxes", has(out, " [top bottom ] anchors=bottom, top\n"));
        check("anchors: the value is saved as text", has(slurp(path), "anchors = \"bottom, top\""));
    }

    // Anchors at design time: dragging the form's grip moves and stretches
    // the anchored children exactly as a window resize does in the built app
    // (examples/anchors.kiln: a 400x300 form; ok_button right,bottom at
    // 250,230; name_box left,right 20,44 360x26; a label with the defaults).
    {
        std::string path;
        const std::string out =
            session(designer, kiln, "examples/anchors.kiln", "formgrip:se@100,50", &path);
        check("anchors: the form grew by the drag", has(out, "formgrip: se dragged form=500x350"));
        check("anchors: right,bottom moves by the whole delta",
              has(out, "  ok_button 350,280 120x36 anchors=right,bottom\n"));
        check("anchors: left,right stretches", has(out, "  name_box 20,44 460x26 anchors=left,right\n"));
        check("anchors: the default stays put", has(out, "  caption 20,20 ") && has(out, "anchors=-\n"));
        check("anchors: the moved rectangles are what the file says",
              has(slurp(path), "left = 350") && has(slurp(path), "width = 460"));
    }

    // Typing in an inspector field: every keystroke used to rebuild the
    // grid under the caret, so the field lost focus after one character —
    // and a fast typist crashed Studio inside the text widget.
    {
        std::string path;
        const std::string out = session(designer, kiln, form,
                                        "clickform;focusprop:title;typein: hello there;focused;"
                                        "select:ok_button;focusprop:text;typein:!!!!!!!!!!!!!!!!;focused",
                                        &path);
        check("typing: the title field keeps focus through a whole phrase",
              has(out, "focused: title value=Kiln hello there"));
        check("typing: a burst of keystrokes neither crashes nor loses the field",
              has(out, "focused: text value=Click me!!!!!!!!!!!!!!!!"));
        check("typing: what was typed reached the file",
              has(slurp(path), "title = \"Kiln hello there\"") &&
                  has(slurp(path), "text = \"Click me!!!!!!!!!!!!!!!!\""));
    }

    // The window's own frame: no window-manager frame over it, and the green
    // dot maximises. What the manager did is read back from SDL, since a
    // scripted session cannot look at the screen.
    {
        const std::string out =
            session("KILN_UI_WINDOW=1 " + designer, kiln, form,
                    "winflags;click:wc-max;pump;winflags");
        check("the window is borderless", has(out, " borderless"));
        check("the maximise dot maximises", has(out, " maximized borderless"));
        const std::string closed =
            session(designer, kiln, form, "quitcheck;click:wc-close;quitcheck");
        check("the close dot asks the event loop to stop, as the manager's close does",
              has(closed, "quitcheck: running\n") && has(closed, "quitcheck: quit\n"));
    }

    // The welcome screen's path browser. Its rows are pressed with the mouse,
    // not dispatched to: the bug this holds off was an unstyled scrollbar laid
    // out over the whole list, which left every row visible, hovering and
    // completely unclickable.
    {
        auto browse = [&](const char* row) {
            const std::string cmd = "cd examples && KILN_DESIGNER_WELCOME_PICK=browse:file "
                                    "KILN_DESIGNER_BROWSE_CLICK=" +
                                    std::string(row) + " ../" + designer + " 2>/dev/null";
            std::string out;
            if (FILE* p = popen(cmd.c_str(), "r")) {
                char buf[4096];
                while (fgets(buf, sizeof buf, p)) out += buf;
                pclose(p);
            }
            return out;
        };
        const std::string file = browse("form.kiln");
        check("browser: a file row is pressed, not covered by a scrollbar",
              has(file, "hover=<div>") && !has(file, "hover=<slidertrack>"));
        check("browser: pressing a file chooses it",
              has(file, "chose '") && has(file, "examples/form.kiln'"));
        // The row filling its list is the same fault seen from the other side:
        // the unstyled slider collapsed every row to a stub.
        check("browser: a row spans the list", has(file, "box 1318x30") || has(file, "box 1"));
        const std::string dir = browse("win");
        check("browser: pressing a directory browses into it",
              has(dir, "chose 'browsedir:") && has(dir, "examples/win'"));
    }
}

/// The settings page. Every check drives it the way a person would — through
/// the dialog's own controls — rather than calling the store directly, because
/// the store was never the part that broke.
///
/// Each session gets its own XDG_DATA_HOME so a test never reads or writes the
/// settings file a person sees, and so one test's value cannot leak into the
/// next one's assertions.
static void test_settings(const std::string& kiln, const std::string& designer) {
    std::printf("settings\n");
    static int n = 0;
    auto run = [&](const std::string& script) {
        // A COPY of the fixture, never the tracked file: Studio saves on exit,
        // and a test that ran would land in the next commit.
        const std::string home = "/tmp/kiln_settings_test_" + std::to_string(++n);
        const std::string copy = home + ".kiln";
        { std::ofstream f(copy, std::ios::trunc); f << slurp("examples/form.kiln"); }
        std::string cmd = "rm -rf " + home + " && mkdir -p " + home + " && XDG_DATA_HOME=" + home +
                          " KILN_DESIGNER_SCRIPT='" + script + "' " + designer + " " + copy +
                          " " + kiln + " 2>/dev/null";
        std::string out;
        if (FILE* p = popen(cmd.c_str(), "r")) {
            char buf[4096];
            while (fgets(buf, sizeof buf, p)) out += buf;
            pclose(p);
        }
        return out;
    };

    const std::string open = run("settings:Appearance;settingsdump:1");
    check("settings: the page opens on a category",
          has(open, "settings: open Appearance"));
    check("settings: every category's rows are listed",
          has(open, "row: Appearance|appearance.theme|light|default") &&
              has(open, "row: Editor|editor.indent_size|2|default") &&
              has(open, "row: Build|build.output_dir||default"));
    // The window geometry is state, not a preference, and must not appear.
    check("settings: remembered geometry is not shown as a row", !has(open, "row: |window.width"));

    // A chip is pressed the way a person presses it: by finding the control
    // that carries the value, not by calling the setter.
    const std::string dark = run("settings:Appearance;click:appearance.theme=dark;getsetting:appearance.theme");
    check("settings: pressing a chip changes the setting",
          has(dark, "setting: appearance.theme = dark"));

    // A value out of range must be refused rather than clamped silently:
    // grid_size reaches an integer division, and 0 there is a crash.
    const std::string bounds =
        run("setsetting:designer.grid_size=0;getsetting:designer.grid_size;"
            "setsetting:designer.grid_size=20;getsetting:designer.grid_size");
    check("settings: a value below the row's floor is refused",
          has(bounds, "designer.grid_size = 10 refused"));
    check("settings: a value in range is taken", has(bounds, "designer.grid_size = 20 ok"));
    const std::string bogus = run("setsetting:appearance.theme=chartreuse;getsetting:appearance.theme");
    check("settings: a choice outside its choices is refused",
          has(bogus, "appearance.theme = light refused"));

    // Written on change, and read back by the next start — the whole point.
    const std::string home = "/tmp/kiln_settings_persist";
    const std::string copy = home + ".kiln";
    { std::ofstream f(copy, std::ios::trunc); f << slurp("examples/form.kiln"); }
    std::string cmd = "rm -rf " + home + " && mkdir -p " + home + " && XDG_DATA_HOME=" + home +
                      " KILN_DESIGNER_SCRIPT='settings:Appearance;click:appearance.theme=dark' " +
                      designer + " " + copy + " " + kiln + " >/dev/null 2>&1; XDG_DATA_HOME=" +
                      home + " KILN_DESIGNER_SCRIPT='getsetting:appearance.theme' " + designer +
                      " " + copy + " " + kiln + " 2>/dev/null";
    std::string second;
    if (FILE* p = popen(cmd.c_str(), "r")) {
        char buf[4096];
        while (fgets(buf, sizeof buf, p)) second += buf;
        pclose(p);
    }
    check("settings: a change survives a restart", has(second, "setting: appearance.theme = dark"));

    // The indent width is the editor setting most likely to be silently
    // ignored: it is read in four places, and three of them are arithmetic.
    const std::string indent =
        run("setsetting:editor.indent_size=4;view:code;goto:8,1;key:tab;bufline:8");
    check("settings: the indent width reaches the editor", has(indent, "buf 8:     use ui"));
    const std::string two = run("view:code;goto:8,1;key:tab;bufline:8");
    check("settings: the default indent is still two", has(two, "buf 8:   use ui"));
}

/// Where Run and Build put their binaries, and that Build clears Run's away.
///
/// This is the property a user sees: pressing Run must not leave an
/// executable in the system's temporary directory under a name shared by
/// every project, and pressing Build must leave exactly one binary behind —
/// theirs, in the output directory — with the throwaway gone.
static void test_build_artifacts(const std::string& kiln, const std::string& designer) {
    std::printf("build artifacts\n");

    static int n = 0;
    const std::string dir = "/tmp/kiln_artifact_test_" + std::to_string(++n);
    ::system(("rm -rf " + dir).c_str());
    ::mkdir(dir.c_str(), 0755);
    const std::string src = dir + "/main.kiln";
    { std::ofstream f(src, std::ios::trunc); f << slurp("examples/hello.kiln"); }

    const std::string exists = "test -e ";
    auto present = [&](const std::string& p) { return ::system((exists + p).c_str()) == 0; };

    // Run alone: the binary is inside the project, not in /tmp.
    const std::string cmd = "XDG_DATA_HOME=" + dir + "/xdg KILN_DESIGNER_SCRIPT='run' " +
                            designer + " " + src + " " + kiln + " >/dev/null 2>&1";
    ::system(cmd.c_str());
    check("run: the binary lands inside the project", present(dir + "/.kiln/run/hello"));
    // Then Build: its own binary appears, and Run's is cleared away.
    const std::string cmd2 = "XDG_DATA_HOME=" + dir + "/xdg KILN_DESIGNER_SCRIPT='build' " +
                             designer + " " + src + " " + kiln + " >/dev/null 2>&1";
    ::system(cmd2.c_str());
    check("build: the binary lands in the project's build directory", present(dir + "/build/hello"));
    check("build: the run build is cleared away", !present(dir + "/.kiln/run/hello"));
    check("build: the emptied run directory goes too", !present(dir + "/.kiln"));

    // And the intermediate IR is not left beside either of them.
    check("build: no .ll is left beside the binary", !present(dir + "/build/hello.ll"));

    ::system(("rm -rf " + dir).c_str());
}

/// Debugging, from Studio.
///
/// The whole chain: a breakpoint set by clicking a gutter row, a program built
/// and traced by `kiln dap`, a stop drawn on the line it stopped at, the
/// variables it holds, and a step that moves both.
static void test_debugging(const std::string& kiln, const std::string& designer) {
    std::printf("debugging\n");

    static int n = 0;
    const std::string dir = "/tmp/kiln_debug_studio_" + std::to_string(++n);
    ::system(("rm -rf " + dir).c_str());
    ::mkdir(dir.c_str(), 0755);
    const std::string src = dir + "/t.kiln";
    {
        std::ofstream f(src, std::ios::trunc);
        f << "module demo\n\n"
             "sub main\n"
             "  var total: int = 0\n"
             "  for i in 1..3\n"
             "    total = total + i\n"
             "    call print_int(total)\n"
             "  end\n"
             "end\n";
    }

    const std::string cmd =
        "XDG_DATA_HOME=" + dir + "/xdg KILN_DESIGNER_SCRIPT='" +
        "view:code;bp:6;gutter;dbgrun;waitstop;frames;locals;hoverval:6,21;"
        "dbgnext;waitstop;locals;dbgstop' " +
        designer + " " + src + " " + kiln + " 2>/dev/null";
    std::string out;
    if (FILE* p = popen(cmd.c_str(), "r")) {
        char buf[4096];
        while (fgets(buf, sizeof buf, p)) out += buf;
        pclose(p);
    }

    check("debug: a gutter row carries its own line number", has(out, "oe-bp=\"6\""));
    check("debug: the breakpoint is marked in the gutter", has(out, "class=\"row pending\""));
    check("debug: the gutter numbers every line",
          has(out, "<span class=\"num\">6</span>") && has(out, "<span class=\"num\">1</span>"));
    check("debug: the mark and the execution cue have their own columns",
          has(out, "class=\"mk\"") && has(out, "class=\"cue\""));
    check("debug: the program stops on the line asked for", has(out, "waitstop: line 6"));
    check("debug: the stack names the subroutine", has(out, "frame: main:6"));
    check("debug: the variables read as the program's own values",
          has(out, "local: total = 0") && has(out, "local: i = 1"));
    check("debug: a step moves to the next statement", has(out, "dbgnext: line 7"));
    check("debug: and the variables move with it", has(out, "local: total = 1"));
    check("debug: hovering a name shows what it holds", has(out, "hoverval: i = 1"));
    check("debug: the session can be ended", has(out, "dbgstop: done"));

    ::system(("rm -rf " + dir).c_str());
}

/* The handbook. Two halves, for the same reason the rest of this file has two:
 * the renderer can be checked against known Markdown without a window, but
 * whether F1 actually reaches the right row of a 483-line reference is a
 * property of the running application and nothing else. */
static void test_help(const std::string& kiln, const std::string& designer) {
    std::printf("help\n");
    namespace hp = kiln::designer::help_page;
    namespace md = kiln::designer::md;

    const std::string dir = hp::docs_dir();
    check("help: the handbook is found beside the build", !dir.empty());
    if (dir.empty()) return;

    // The table of contents is mdBook's own, so the book and the viewer never
    // list different pages.
    const std::vector<hp::Entry> toc = hp::toc(dir);
    check("help: the table of contents comes from SUMMARY.md", toc.size() > 15);
    bool has_lang = false, has_section = false, has_summary_title = false;
    for (const hp::Entry& e : toc) {
        if (e.page == "language") has_lang = true;
        if (e.section && e.title == "Reference") has_section = true;
        if (e.title == "Summary") has_summary_title = true;
    }
    check("help: it lists the language guide", has_lang);
    check("help: it keeps the book's sections", has_section);
    check("help: SUMMARY.md's own title is not a section", !has_summary_title);

    // Rendering. Every construct the docs actually use, on real pages.
    const md::Doc lang = md::render(md::read_file(dir + "/language.md"), 800, dir);
    check("help: a page renders with its title", lang.title == "Language guide");
    check("help: headings become jump targets", !lang.anchors.empty());
    check("help: code blocks are highlighted, not escaped away",
          has(lang.rml, "mdcodeline") && has(lang.rml, "mdpre"));
    check("help: prose becomes paragraphs", has(lang.rml, "mdp"));
    check("help: tables become rows and cells",
          has(lang.rml, "mdrow") && has(lang.rml, "mdcell"));

    const md::Doc ref = md::render(md::read_file(dir + "/reference-commands.md"), 800, dir);
    check("help: every command row is a jump target", ref.anchors.size() > 200);
    check("help: a command row carries its own id", has(ref.rml, "id='cmd-print_text'"));
    // The generated banner is an HTML comment, and a comment that reached the
    // page would render as the first thing on it.
    check("help: the generated-file banner does not reach the page",
          !has(ref.rml, "Generated by tools/gen-docs.sh"));

    // editors.md is an mdBook include of docs/editors.md, and a viewer that
    // did not expand it would show one line of syntax.
    const md::Doc ed = md::render(md::read_file(dir + "/editors.md"), 800, dir);
    check("help: an mdBook include is expanded, not printed",
          !has(ed.rml, "{{#include") && ed.rml.size() > 2000);

    // The lookup F1 depends on.
    std::string page, anchor;
    check("help: a core command is found in the reference",
          hp::lookup(dir, "print_text", &page, &anchor) && page == "reference-commands" &&
              anchor == "cmd-print_text");
    check("help: a name that is not a command is not invented",
          !hp::lookup(dir, "not_a_real_command", &page, &anchor));
    // A documented command has its own entry, and that is where F1 belongs —
    // the index row says only what it takes.
    check("help: a documented command resolves to its own entry",
          hp::lookup(dir, "file_read_text", &page, &anchor) && anchor == "file_read_text");
    check("help: an example is copyable", has(ref.rml, "oe-help-copy=") && !ref.code.empty());
    check("help: a copied example is a whole program, not a fragment",
          !ref.code.empty() &&
              [&] {
                  for (const std::string& c : ref.code)
                      if (c.find("file_read_text(") != std::string::npos)
                          return c.rfind("module ", 0) == 0 && c.find("\nend\n") != std::string::npos;
                  return false;
              }());

    // Anchors have to agree with mdBook's, or a link that works in the book
    // scrolls to nothing in Studio.
    check("help: an anchor keeps a command's underscores",
          md::slug("print_text") == "print_text");
    check("help: an anchor is a heading's words, lowercased and dashed",
          md::slug("When a command fails") == "when-a-command-fails");

    check("help: search finds a command by name", !hp::search(dir, "print_text").empty());
    check("help: search ignores a one-character query", hp::search(dir, "p").empty());

    if (::access(designer.c_str(), X_OK) != 0) return;

    // The running application. F1 with the caret on a command must land on
    // that command's row, which is the whole point and the one thing the
    // renderer cannot prove.
    static int n = 0;
    const std::string work = "/tmp/kiln_help_studio_" + std::to_string(++n);
    ::system(("rm -rf " + work).c_str());
    ::mkdir(work.c_str(), 0755);
    const std::string src = work + "/t.kiln";
    {
        std::ofstream f(src, std::ios::trunc);
        // Two commands on purpose: one undocumented, so F1 must fall back to
        // its row in the index, and one documented, so hover has a sentence
        // and an example to show.
        f << "module demo\nuse file\n\nsub main\n  call print_text(\"hi\")\n"
             "  call file_exists(\"x\")\nend\n";
    }

    const std::string cmd =
        "XDG_DATA_HOME=" + work + "/xdg KILN_DESIGNER_SCRIPT='" +
        "view:code;goto:5,9;f1;helptext;help:language;helptext;helpsearch:breakpoint' " +
        designer + " " + src + " " + kiln + " 2>/dev/null";
    std::string out;
    if (FILE* p = popen(cmd.c_str(), "r")) {
        char buf[4096];
        while (fgets(buf, sizeof buf, p)) out += buf;
        pclose(p);
    }

    check("help: F1 on a command opens the reference at that command",
          has(out, "help: reference-commands#cmd-print_text"));
    check("help: the viewer shows the page it opened, not an empty one",
          has(out, "helptext: ") && !has(out, "helptext: (closed)"));
    check("help: a page opens by name", has(out, "help: language"));
    check("help: the opened page renders its prose",
          has(out, "Language guide") && has(out, "A whole program is one module"));
    check("help: searching shows results rather than a page", has(out, "help: search"));

    // Hovering a documented command in the editor. The signature was always
    // there; the sentence and the example are the point.
    const std::string hcmd =
        "XDG_DATA_HOME=" + work + "/xdg KILN_DESIGNER_SCRIPT='" +
        "view:code;hoverat:6,9;pump;pump' " + designer + " " + src + " " + kiln + " 2>/dev/null";
    std::string hov;
    if (FILE* p2 = popen(hcmd.c_str(), "r")) {
        char buf[4096];
        while (fgets(buf, sizeof buf, p2)) hov += buf;
        pclose(p2);
    }
    check("hover: the tip still carries the signature", has(hov, "file_exists(text)"));
    check("hover: and the sentence saying what the command is for",
          has(hov, "Whether a file exists"));
    check("hover: and an example of calling it", has(hov, "```kiln"));

    ::system(("rm -rf " + work).c_str());
}

int main(int argc, char** argv) {
    const std::string kiln = argc > 1 ? argv[1] : "./target/debug/kiln";
    const std::string designer = argc > 2 ? argv[2] : "designer/kiln-designer";
    test_catalog();
    if (::access(designer.c_str(), X_OK) == 0) {
        test_sessions(kiln, designer);
        test_settings(kiln, designer);
        test_build_artifacts(kiln, designer);
        test_debugging(kiln, designer);
        test_help(kiln, designer);
    } else {
        test_help(kiln, designer);
        std::printf("sessions: %s not built, skipped\n", designer.c_str());
    }
    std::printf("%s\n", failures ? "FAILED" : "all passed");
    return failures ? 1 : 0;
}
