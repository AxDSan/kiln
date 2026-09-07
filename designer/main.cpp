/* Kiln Studio — the visual designer.
 *
 * Chrome follows the Kiln Studio design specification: title bar, menu bar,
 * action toolbar, toolbox / designer / inspector docks, a split code+output
 * panel, and a status bar. Tokens live in theme.h so no colour is hard-coded
 * here.
 *
 * Dogfoods RmlUi: the IDE and the apps it builds run on the same
 * substrate, and the canvas builds components through the SHARED mapping
 * (libs/ui/ui_mapping.h) so what you draw is what you get (D9).
 *
 * It never parses .kiln — `kiln inspect` is the only reader — and saving
 * splices the regenerated form over the original lines so hand-written code
 * survives.
 */
#include <RmlUi/Core.h>
#include <RmlUi/Core/Elements/ElementFormControl.h>
#include <RmlUi/Core/Elements/ElementFormControlInput.h>
#include <RmlUi/Core/Elements/ElementFormControlTextArea.h>
#include <cstdio>
#include <array>
#include <map>
#include <cstdlib>
#include <cstring>
#include <algorithm>
#include <dirent.h>
#include <sys/stat.h>
#include <functional>
#include <fstream>
#include <fcntl.h>
#include <time.h>
#include <signal.h>
#include <set>
#include <sstream>
#ifndef _WIN32
#include <sys/wait.h>
#endif
#include <unistd.h>
#include <string>
#include <vector>

#include <SDL.h>
#include <SDL_image.h>

#include "RmlUi_Backend.h"
#include "RmlUi_Include_GL3.h"
#include "RmlUi_Platform_SDL.h"
#include "RmlUi_Renderer_GL3.h"
#include "catalog.h"
#include "descriptors.h"
#include "dotgrid.h"
#include "highlight.h"
#include "model.h"
#include "portable.h"
#include "settings.h"
#include "settings_page.h"
#include "help_page.h"
#include "theme.h"
#include "lspclient.h"
#include "dbgclient.h"
#include "welcome.h"
#include "ui_mapping.h"

using namespace kiln::designer;

namespace {

/// Initial window size. The IDE follows the OS window after that — see
/// relayout(), which is re-run whenever the context's dimensions change.
constexpr int INIT_W = 1440, INIT_H = 900;

/// Components named in the design spec that the UI library does not provide
/// yet. Shown greyed so the toolbox reads as designed while staying honest
/// about what actually exists — clicking one says so rather than failing oddly.
/// A menu entry: label, the action it fires, and its shortcut hint.
struct MenuItem { const char* label; const char* action; const char* keys; };
struct Menu { const char* title; std::vector<MenuItem> items; };

/// The menu bar. Every entry here does something — an entry that only prints
/// "not implemented" is worse than no entry at all.
inline const std::vector<Menu>& menus() {
    static const std::vector<Menu> m = {
        {"File", {{"Save", "save", "Ctrl+S"}, {"Build Binary", "build", ""},
                  {"Run", "run", ""}, {"Exit", "exit", ""}}},
        {"Edit", {{"Undo", "undo", "Ctrl+Z"}, {"Redo", "redo", "Ctrl+Shift+Z"},
                  {"Copy", "copy", "Ctrl+C"}, {"Paste", "paste", "Ctrl+V"},
                  {"Delete", "delete", "Del"}}},
        {"View", {{"Designer", "view-designer", ""}, {"Code", "view-code", ""}}},
        {"Build", {{"Build Binary", "build", ""}, {"Run", "run", ""}, {"Stop", "stop", ""}}},
        // A key nobody has been told about does not exist, which is what the
        // menu is for as much as the clicking.
        {"Debug", {{"Debug", "debug", "F5"},
                   {"Toggle Breakpoint", "togglebp", "F9"},
                   {"Step Over", "dbgstepover", "F10"},
                   {"Step In", "dbgstepin", "F11"},
                   {"Step Out", "dbgstepout", "Shift+F11"},
                   {"Continue", "dbgcontinue", "F5"},
                   {"Stop Debugging", "dbgstop", "Shift+F5"}}},
        {"Tools", {{"Settings…", "settings", "Ctrl+,"}}},
        {"Help", {{"Documentation", "help", "F1"},
                  {"Command Reference", "helpcommands", ""},
                  {"Search the Handbook", "helpsearch", "Shift+F1"},
                  {"About Kiln", "about", ""}}},
    };
    return m;
}

struct PlannedTool { const char* section; const char* name; };
const PlannedTool PLANNED[] = {
    // Controls that do not exist yet. Listed greyed rather than omitted, so
    // the toolbox shows the intended shape without pretending they work.
    // Anything that has since been implemented is filtered out below by name,
    // so a kit shipping one of these does not make it appear twice.
    {"Containers", "TabControl"},    {"Containers", "Splitter"},
    {"System", "FileDialog"},        {"System", "TrayIcon"},
};

struct Designer {
    Rml::Context* context = nullptr;
    Rml::ElementDocument* doc = nullptr;
    Model model;
    /// The compiler. Resolved at startup from our own location so an installed
    /// bundle works wherever it is unpacked; the in-repo dev path is the
    /// fallback.
    std::string kiln_bin = "./target/debug/kiln";
    std::string selected;
    std::string inspector_tab = "props";
    /// The inspector needs rebuilding, but a field in it has focus. It is
    /// rebuilt once focus leaves; see refresh_all().
    bool inspector_stale = false;
    /// Every component the toolchain reports, with the section it files under.
    /// Built once at startup from `kiln kits` and `kiln commands`, so a
    /// kit installed after Studio was compiled still appears in the toolbox.
    Catalog catalog;
    /// The property an open editor popup is editing, and which component's.
    std::string editing_id, editing_prop;
    std::string search;
    std::vector<std::string> pending_subs;
    /// The console's history. Text and class apart, not markup: the text goes
    /// into a textarea, which is what makes it selectable, and the class
    /// into the colour layer under it — the two consumers want different
    /// shapes of the same line.
    struct LogLine { std::string text, cls; };
    std::vector<LogLine> log_lines;
    bool dirty = false;

    /// The code editor's text, and whether it holds edits the model has not
    /// seen. `code_dirty` is what stops a refresh from overwriting what the
    /// user is in the middle of typing.
    std::string model_text;
    bool code_dirty = false;
    /// Editor scroll, in pixels from the top. Ours rather than RmlUi's: a text
    /// control keeps its own overflow hidden, and both layers are absolutely
    /// positioned, so neither the control nor its container will scroll them.
    /// What the chrome's stylesheet was built from. Kept so a theme change can
    /// build it again without threading three arguments through every caller.
    std::string mono, dot_tile;
    /// The Settings dialog, and the category it is showing.
    Rml::ElementDocument* settings_doc = nullptr;
    std::string settings_cat = "Appearance";
    /// The canvas grid's decorator value, as `build_styles` composed it. Kept
    /// so `designer.show_grid` can put it back: `decorator:none` is easy to
    /// set and impossible to undo without this string.
    std::string grid_decorator;
    int code_scroll = 0;
    int code_content_h = 0;
    /// The same, sideways. The textarea is `wrap='nowrap'`, so it scrolls a long
    /// line horizontally on its own; the colour layer under it has to be moved
    /// by the same amount or the two draw the same line at different x.
    int code_scroll_x = 0;
    /// Set when a line is appended to the console, so the frame loop can scroll
    /// it to the bottom. Doing it inside log() would force a full layout per
    /// line, and a build emits many lines per frame.
    bool log_follow = false;
    /// Read end of the running app's stdout+stderr, so its output lands in the
    /// IDE console instead of the terminal the IDE was launched from.
    int app_output = -1;

    /// The language server. Studio is a client of the same `kiln lsp` that
    /// other editors use, so it never grows a private second analysis path.
    kiln::lsp::Client lsp;
    /// The debugger, spoken to exactly as the language server is: a subprocess
    /// on a pipe. Studio never traces the program itself — `kiln dap` owns
    /// it — because a blocking wait belongs anywhere but the frame loop.
    kiln::dbg::Client dbg;
    /// Lines the user has put a breakpoint on, in the open module. Kept here
    /// rather than in the `.kiln`: a breakpoint is a fact about debugging this
    /// program now, not about the program, and writing them into the source
    /// would put them in the user's next commit.
    std::set<int> breakpoints;
    /// The line the program is stopped on, or 0 when it is not stopped.
    int stopped_line = 0;
    std::vector<kiln::lsp::Diagnostic> diagnostics;

    /// The build, when one is in flight. The build MUST be asynchronous: run
    /// synchronously it blocks the frame loop, so no progress animation can
    /// play and the compiler's output arrives in one lump at the end instead
    /// of streaming as it happens.
    pid_t build_pid = 0;
    int build_output = -1;
#ifdef _WIN32
    /// The same two children on Windows, as CreateProcess hands them out;
    /// `build_pid` and `running_app` still say "> 0 = alive" for everyone
    /// who only asks that.
    kiln::sys::Child build_child, app_child;
#endif
    bool build_then_run = false;
    std::string build_target;
    double build_started = 0.0;
    bool dragging = false;
    int drag_dx = 0, drag_dy = 0;

    /// Form-window resize, as in Visual Studio / RAD Studio: grab an edge or
    /// the corner of the preview and drag.
    bool resizing_form = false;
    /// Every child's rectangle when the form's resize began, so anchored
    /// controls follow the form from where they were, not from where the
    /// last mouse move left them (integer deltas would drift otherwise).
    std::map<std::string, std::array<int, 4>> anchor_base;
    std::string resize_edge;          // "e", "s" or "se"
    int resize_x0 = 0, resize_y0 = 0, resize_w0 = 0, resize_h0 = 0;

    /// Component resize via the selection anchors.
    bool resizing_comp = false;
    std::string comp_edge;
    int comp_x0 = 0, comp_y0 = 0, comp_w0 = 0, comp_h0 = 0;

    /// Dock sizes, adjustable by dragging the splitters between panels.
    int toolbox_w = theme::TOOLBOX_W;
    int inspect_w = theme::INSPECT_W;
    int bottom_h = theme::BOTTOM_H;
    /// The code preview's share of the bottom dock, in pixels; 0 until the
    /// first layout, which gives it half.
    int code_w = 0;
    std::string splitting;            // "left", "right", "bottom" or empty
    int split_x0 = 0, split_y0 = 0, split_v0 = 0;

    /// "designer" or "code" — the centre pane's view.
    std::string view = "designer";

    /// The app launched by Run, so Stop can actually stop it.
    pid_t running_app = 0;

    /// Edit history. Whole-model snapshots: the model is a few dozen strings,
    /// so copying it is cheaper than describing every edit as a reversible
    /// command, and it cannot drift from what it claims to undo.
    std::vector<Model> undo_stack;
    std::vector<Model> redo_stack;

    /// Components selected in the designer. `selected` is the primary one.
    std::vector<std::string> selection;

    /// Clipboard for copy/paste.
    std::vector<Component> clipboard;

    /// Alignment guides to draw this frame: x positions and y positions.
    std::vector<int> guide_x, guide_y;

    /// Current window size. Layout is recomputed whenever this changes, so the
    /// IDE fills the OS window instead of leaving unpainted margins.
    int win_w = INIT_W, win_h = INIT_H;

    /// The cursor the platform was last asked for. Only a scripted session
    /// reads it: a frame dump cannot show the mouse pointer, so this is how
    /// the resize cursor gets verified at all.
    std::string cursor_name;
    /// The UI face, for documents built after the chrome — the About dialog.
    std::string family;
    /// The About dialog while it is up. A second document rather than a div
    /// in the chrome: shown modal, it is the one thing that can take focus,
    /// which is what makes Escape and "click outside" unambiguous.
    Rml::ElementDocument* about = nullptr;

    /// The handbook while it is up, and where it is. Modal for the same reason
    /// About is: it owns focus, so Escape and click-outside are unambiguous.
    /// The page is reloaded rather than mutated on every navigation — a
    /// document this size is built in well under a frame, and one build path
    /// beats a build path plus a patch path.
    Rml::ElementDocument* help = nullptr;
    std::string help_page = "introduction";
    std::string help_query;
    /// The code blocks on the page now shown, in the order they appear. A copy
    /// button carries its index into this, never the program itself.
    std::vector<std::string> help_code;

    /// The previous press on the canvas, for double-click detection. RmlUi's
    /// own `dblclick` compares element identity, and every press here rebuilds
    /// the canvas (select → refresh_all), so the second press always lands on
    /// a freshly created element and the substrate never sees a pair.
    std::string last_press_id;
    double last_press_time = 0.0;
    int last_press_x = 0, last_press_y = 0;

    /// The mouse over the editor, for hover-on-rest. A hover request is sent
    /// once the pointer has stayed put for a moment, and once only per spot.
    int hover_x = -1, hover_y = -1;
    double hover_moved_at = 0.0;
    bool hover_asked = false;
    int hover_request = 0;      // the in-flight hover's id, 0 when none
    int hover_shown_for = 0;    // the request whose answer is still wanted
    int hover_line = -1, hover_col = -1;
    /// Definition and references answers land here through the frame loop.
    int def_request = 0;
    int refs_request = 0;

    /// The completion popup. `complete_all` is what the server offered for
    /// the position asked about; `complete_shown` the indices into it that
    /// still match the word as typed, so keeping typing narrows the list
    /// without a round trip. The word begins at `complete_start` on
    /// `complete_line`; the caret leaving that word is what dismisses it.
    int complete_request = 0;
    std::vector<kiln::json::Value> complete_all;
    std::vector<size_t> complete_shown;
    int complete_line = 0, complete_start = 0;
    int complete_index = 0;
    bool complete_open = false;
    /// The component whose context menu is open, or empty. The menu's rows
    /// carry the id themselves; this only says whether Escape has a menu to
    /// close.
    std::string ctx_id;
    /// Text the next textinput must not insert. The platform follows Return
    /// with a '\n' textinput and may follow Ctrl+Space with a ' ': when the
    /// key went to the popup, the character it carries must not go to the
    /// editor.
    std::string swallow_text;
    /// Where a hit in the references list jumps to, by list index.
    std::vector<kiln::lsp::Location> refs;

    /// The module-level names the server's index knows, for the Code tab's
    /// label. Asked for again after every change to the text, one request
    /// in flight at a time; until the first answer lands the tab names the
    /// file, which is never wrong.
    std::vector<kiln::lsp::Symbol> symbols;
    int symbol_request = 0;
    bool symbols_stale = true;
    /// What the two document tabs currently say, so a frame that changes
    /// nothing rewrites nothing.
    std::string tab_designer_label, tab_code_label;
    /// The icon the form preview's title bar shows, so the image is only
    /// reloaded when the property changes — the canvas is rebuilt per drag.
    std::string form_icon_src;
};
Designer g;

std::string esc(const std::string& s) { return escape_rml(s); }

/// Filename without directories, for the title bar.
std::string basename_of(const std::string& p) {
    const size_t slash = p.find_last_of('/');
    return slash == std::string::npos ? p : p.substr(slash + 1);
}

Rml::Element* by_id(const char* id) { return g.doc ? g.doc->GetElementById(id) : nullptr; }

/// The platform layer, with the cursors the designer's stylesheet asks for.
///
/// RmlUi's SDL backend knows one `resize` cursor, the diagonal one. Every
/// handle here names its direction — `resize-ns` on the top and bottom
/// anchors, `resize-ew` on the sides — and a name the backend does not know
/// leaves the pointer as it was, so the directional cursors are created here
/// and the rest is handed back to the backend.
struct StudioSystem : SystemInterface_SDL {
    explicit StudioSystem(SDL_Window* window) : SystemInterface_SDL(window) {
        ns = SDL_CreateSystemCursor(SDL_SYSTEM_CURSOR_SIZENS);
        ew = SDL_CreateSystemCursor(SDL_SYSTEM_CURSOR_SIZEWE);
        nesw = SDL_CreateSystemCursor(SDL_SYSTEM_CURSOR_SIZENESW);
        nwse = SDL_CreateSystemCursor(SDL_SYSTEM_CURSOR_SIZENWSE);
    }
    ~StudioSystem() override {
        for (SDL_Cursor* c : {ns, ew, nesw, nwse}) {
            if (c) SDL_FreeCursor(c);
        }
    }
    void SetMouseCursor(const Rml::String& name) override {
        g.cursor_name = name;
        SDL_Cursor* c = name == "resize-ns"     ? ns
                        : name == "resize-ew"   ? ew
                        : name == "resize-nesw" ? nesw
                        : name == "resize-nwse" ? nwse
                                                : nullptr;
        if (c) SDL_SetCursor(c);
        else SystemInterface_SDL::SetMouseCursor(name);
    }
    SDL_Cursor *ns = nullptr, *ew = nullptr, *nesw = nullptr, *nwse = nullptr;
};

/* --- the window's own frame --------------------------------------------- */

/// What the three dots do. Close is a request to the event loop, not an exit:
/// it goes in as the same SDL_QUIT the window manager's close sends, so the
/// unsaved-changes handling at the end of the loop runs for both.
void window_control(const std::string& which) {
    SDL_Window* win = SDL_GL_GetCurrentWindow();
    if (!win) return;
    if (which == "min") {
        SDL_MinimizeWindow(win);
    } else if (which == "max") {
        if (SDL_GetWindowFlags(win) & SDL_WINDOW_MAXIMIZED) SDL_RestoreWindow(win);
        else SDL_MaximizeWindow(win);
    } else if (which == "close") {
        SDL_Event ev{};
        ev.type = SDL_QUIT;
        SDL_PushEvent(&ev);
    }
}

/// Which part of the frame the pointer is on. The window manager moves and
/// resizes the window from this — the one mechanism that works under every
/// compositor, since a borderless window cannot be moved by the program on
/// Wayland at all. The dots are left out of the drag strip so their clicks
/// still reach the document.
///
/// A press in the strip never reaches SDL's event queue: the platform hands
/// it straight to the window manager. So the double-click that toggles
/// maximise has to be recognised here, from the calls themselves. X11 calls
/// on every motion as well, for the cursor, and there the server's button
/// state tells a press from a pass-over. Wayland calls on press and on
/// release alike and offers nothing to tell them apart, so a click there
/// would read as a double-click: on Wayland the strip only drags.
SDL_HitTestResult window_hit_test(SDL_Window* win, const SDL_Point* p, void*) {
    int w = 0, h = 0;
    SDL_GetWindowSize(win, &w, &h);
    const bool maximised = SDL_GetWindowFlags(win) & SDL_WINDOW_MAXIMIZED;
    constexpr int EDGE = 6;
    if (!maximised) {
        const bool l = p->x < EDGE, r = p->x >= w - EDGE, t = p->y < EDGE, b = p->y >= h - EDGE;
        if (t && l) return SDL_HITTEST_RESIZE_TOPLEFT;
        if (t && r) return SDL_HITTEST_RESIZE_TOPRIGHT;
        if (b && l) return SDL_HITTEST_RESIZE_BOTTOMLEFT;
        if (b && r) return SDL_HITTEST_RESIZE_BOTTOMRIGHT;
        if (t) return SDL_HITTEST_RESIZE_TOP;
        if (b) return SDL_HITTEST_RESIZE_BOTTOM;
        if (l) return SDL_HITTEST_RESIZE_LEFT;
        if (r) return SDL_HITTEST_RESIZE_RIGHT;
    }
    if (p->y >= theme::TITLEBAR_H || p->x >= w - 90) return SDL_HITTEST_NORMAL;

    static const bool x11 = std::strcmp(SDL_GetCurrentVideoDriver(), "x11") == 0;
    static bool was_down = false;
    static Uint32 last_press = 0;
    static SDL_Point last_at{-100, -100};
    if (!x11) return SDL_HITTEST_DRAGGABLE;
    const bool down = SDL_GetGlobalMouseState(nullptr, nullptr) & SDL_BUTTON_LMASK;
    const bool press = down && !was_down;
    was_down = down;
    if (press) {
        // A dump cannot show a drag; this is how a press in the strip is
        // known to have reached here at all.
        if (std::getenv("KILN_DESIGNER_DEBUG")) {
            std::fprintf(stderr, "titlebar: press at %d,%d\n", p->x, p->y);
        }
        const Uint32 now = SDL_GetTicks();
        const bool twice = now - last_press < 400 && std::abs(p->x - last_at.x) < 6 &&
                           std::abs(p->y - last_at.y) < 6;
        last_press = twice ? 0 : now;
        last_at = *p;
        if (twice) {
            window_control("max");
            return SDL_HITTEST_NORMAL;   // a toggle, not the start of a move
        }
    }
    return SDL_HITTEST_DRAGGABLE;
}

void size_output_pane();
void refresh_highlight();
bool bound_breakpoint(int line);
void render_locals();
void jump_to(int line, int col);
void set_view(const std::string& view);
bool caret_position(int& line, int& col);
std::string identifier_at(int line, int col);
void toggle_breakpoint(int line);
void start_debug();
void stop_debug();
void poll_debug();
int code_char_width();
size_t byte_offset(const std::string& text, int line, int character);

/// Apply the current dock sizes to the layout. Geometry lives here rather than
/// in the stylesheet so the panels can be resized at run time.
void relayout() {
    using namespace theme;
    if (!g.doc) return;
    const int W = g.win_w, H = g.win_h;
    const int content_y = TITLEBAR_H + MENUBAR_H + TOOLBAR_H;
    const int content_h = H - content_y - STATUS_H;
    const int centre_w = W - g.toolbox_w - g.inspect_w;
    const int canvas_h = content_h - TABBAR_H - g.bottom_h;

    auto place = [&](const char* id, int x, int y, int w, int h) {
        if (Rml::Element* e = by_id(id)) {
            e->SetProperty("left", Rml::String(std::to_string(x) + "px"));
            e->SetProperty("top", Rml::String(std::to_string(y) + "px"));
            e->SetProperty("width", Rml::String(std::to_string(w) + "px"));
            e->SetProperty("height", Rml::String(std::to_string(h) + "px"));
        }
    };
    // Everything that spans the window has to be re-sized, or the area beyond
    // the original size stays unpainted (a black margin when the user enlarges
    // the window).
    // The body must be sized explicitly: with no size it collapses to 0x0 and
    // its background paints nothing, leaving the window black wherever no child
    // covers it. Setting it here (rather than in the stylesheet) is what lets
    // the IDE follow the OS window.
    g.doc->SetProperty("width", Rml::String(std::to_string(W) + "px"));
    g.doc->SetProperty("height", Rml::String(std::to_string(H) + "px"));
    if (std::getenv("KILN_DESIGNER_DEBUG")) {
        g.context->Update();
        const auto b = g.doc->GetBox().GetSize();
        std::fprintf(stderr, "designer: relayout W=%d H=%d body=%.0fx%.0f\n", W, H, b.x, b.y);
    }
    place("titlebar", 0, 0, W, TITLEBAR_H);
    place("menubar", 0, TITLEBAR_H, W, MENUBAR_H);
    place("toolbar", 0, TITLEBAR_H + MENUBAR_H, W, TOOLBAR_H);
    place("status", 0, H - STATUS_H, W, STATUS_H);
    place("toolbox", 0, content_y, g.toolbox_w, content_h);
    // Below the panel head and the search box; the rest of the dock is list.
    place("toollist", 0, 0, g.toolbox_w, content_h - 70);
    place("centre", g.toolbox_w, content_y, centre_w, content_h);
    place("canvasarea", 0, TABBAR_H, centre_w, canvas_h);
    place("tray", 0, canvas_h - TRAY_H, centre_w, TRAY_H);
    // The code view fills the centre column below the tab bar, and the editor
    // fills the view. Both are sized here, inline, on purpose: RmlUi's text
    // widget writes its own layout properties onto the element, and an inline
    // property beats the stylesheet — so a `width:100%` rule in the CSS is
    // silently ignored and the editor collapses to its default column width.
    // The same height as the canvas area, not the whole centre column: the
    // bottom dock sits below both. Sized without subtracting it, the code view
    // ran on underneath the preview pane and its last lines could not be seen
    // or scrolled to — they were behind it.
    place("codeview", 0, TABBAR_H, centre_w, canvas_h);
    // The editor fills what the gutter leaves of the view: it scrolls its own
    // text internally, and the highlight layer under it is redrawn to match.
    // Both are placed here rather than by the stylesheet, so the gutter's width
    // has to be subtracted here too — a `left` in the sheet is overwritten by
    // this and the numbers would be drawn over the code.
    place("dbggutter", 0, 0, CODE_GUTTER_W, canvas_h - 2 * CODE_PAD_Y);
    place("fullcode", CODE_GUTTER_W, 0, centre_w - 2 * CODE_PAD_X - CODE_GUTTER_W,
          canvas_h - 2 * CODE_PAD_Y);
    place("codehl", CODE_GUTTER_W, 0, centre_w - 2 * CODE_PAD_X - CODE_GUTTER_W,
          canvas_h - 2 * CODE_PAD_Y);
    place("inspectdock", g.toolbox_w + centre_w, content_y, g.inspect_w, content_h);
    // Panel head, tab strip and context label above the list; the wiring box
    // below it, pinned to the dock's foot.
    {
        const int above = 29 + 29 + 32;
        if (Rml::Element* e = by_id("grid")) {
            e->SetProperty("height", Rml::String(std::to_string(std::max(40, content_h - above - WIRE_H)) + "px"));
        }
        if (Rml::Element* e = by_id("wirebox")) {
            e->SetProperty("top", Rml::String(std::to_string(content_h - WIRE_H) + "px"));
            e->SetProperty("width", Rml::String(std::to_string(g.inspect_w - 20) + "px"));
        }
    }
    place("bottom", g.toolbox_w, content_y + TABBAR_H + canvas_h, centre_w, g.bottom_h);
    place("splitleft", g.toolbox_w - 3, content_y, 6, content_h);
    place("splitright", g.toolbox_w + centre_w - 3, content_y, 6, content_h);
    place("splitbottom", g.toolbox_w, content_y + TABBAR_H + canvas_h - 3, centre_w, 6);

    if (g.code_w <= 0) g.code_w = centre_w / 2;
    if (g.code_w > centre_w - 160) g.code_w = centre_w - 160;
    if (g.code_w < 160) g.code_w = 160;
    const int half = g.code_w;
    place("splitmid", g.toolbox_w + half - 3, content_y + TABBAR_H + canvas_h, 6, g.bottom_h);
    if (Rml::Element* e = by_id("codepane")) {
        e->SetProperty("left", "0px");
        e->SetProperty("width", Rml::String(std::to_string(half) + "px"));
        e->SetProperty("height", Rml::String(std::to_string(g.bottom_h) + "px"));
    }
    size_output_pane();
    if (Rml::Element* e = by_id("logpane")) {
        e->SetProperty("left", Rml::String(std::to_string(half) + "px"));
        e->SetProperty("width", Rml::String(std::to_string(centre_w - half) + "px"));
        e->SetProperty("height", Rml::String(std::to_string(g.bottom_h) + "px"));
    }
    if (Rml::Element* e = by_id("code")) {
        e->SetProperty("height", Rml::String(std::to_string(g.bottom_h - 32) + "px"));
    }
    // The highlight layer draws only the rows that fit, so a new geometry is a
    // new row count: repaint it here, after the layout it depends on, rather
    // than leaving it to the next scroll. Every path that resizes the editor
    // — the tab, the splitters, the OS window — comes through this function.
    if (g.view == "code") {
        g.context->Update();
        refresh_highlight();
    }
}

void log(const std::string& line, const char* cls);

void set_status(const std::string& text) {
    if (Rml::Element* e = by_id("statustext")) e->SetInnerRML(esc(text));
    std::printf("designer: %s\n", text.c_str());
    std::fflush(stdout);
}

/// The console is two layers, like the code editor: a textarea on top with
/// transparent glyphs, for the selection and the clipboard, and `#loghl`
/// under it painting the same lines in colour. A textarea renders through one
/// ElementText and can have one colour — and a div's text cannot be selected
/// — so neither element alone gives a console whose errors are red AND whose
/// lines can be copied into a bug report.
void log(const std::string& line, const char* cls) {
    g.log_lines.push_back({line, cls ? cls : ""});
    auto* e = dynamic_cast<Rml::ElementFormControl*>(by_id("log"));
    if (!e) return;
    // The whole history, every time: the textarea scrolls it, so nothing is
    // ever out of reach the way the old tail-only console left it. No
    // trailing newline — one would add an empty row that the layer has no
    // line for.
    std::string text;
    for (size_t i = 0; i < g.log_lines.size(); i++) {
        text += (i ? "\n" : "") + g.log_lines[i].text;
    }
    e->SetValue(text);
    g.log_follow = true;
}

/// Repaint the colour layer for the rows the textarea is showing. Every
/// frame, from the control's own scroll offset read back — never from a
/// value predicted for it — so the wheel, the keyboard, a drag-selection that
/// scrolls, and follow_log() all leave the two layers aligned by
/// construction. The offset is snapped to whole lines first: a part-line
/// offset would put the top row half above the box, which is the one thing
/// this layout cannot draw.
void sync_log_scroll() {
    Rml::Element* ta = by_id("log");
    Rml::Element* layer = by_id("loghl");
    if (!ta || !layer) return;
    const int snapped = (int)ta->GetScrollTop() - ((int)ta->GetScrollTop() % theme::LOG_LINE_H);
    if ((int)ta->GetScrollTop() != snapped) ta->SetScrollTop((float)snapped);
    const size_t first = (size_t)(snapped / theme::LOG_LINE_H);
    // Exactly the rows that fit, not one more: the offset is a whole number
    // of rows, so there is never a partial row to draw — and the box does not
    // clip an absolutely positioned layer, so an extra row would paint over
    // whatever sits below the console.
    const size_t rows = (size_t)((int)ta->GetBox().GetSize().y / theme::LOG_LINE_H);

    static size_t painted_first = (size_t)-1, painted_rows = 0, painted_count = 0;
    if (first == painted_first && rows == painted_rows && g.log_lines.size() == painted_count) {
        return;
    }
    painted_first = first;
    painted_rows = rows;
    painted_count = g.log_lines.size();
    std::string html;
    for (size_t i = first; i < g.log_lines.size() && i < first + rows; i++) {
        const auto& l = g.log_lines[i];
        // An empty div collapses, which would shift every following row up.
        html += "<div" + (l.cls.empty() ? std::string() : " class='" + l.cls + "'") + ">" +
                (l.text.empty() ? std::string("&nbsp;") : esc(l.text)) + "</div>";
    }
    layer->SetInnerRML(html);
}

/// Keep the colour layer level with the control, sideways.
///
/// The caret scrolls the textarea by itself — Right past the edge, Home, End,
/// a click beyond the last visible column — and raises no change event, so
/// nothing else notices. Compared once a frame instead. `refresh_highlight`
/// caches on the offset, so an editor that has not moved does no work.
void follow_code_scroll() {
    if (g.view != "code") return;
    Rml::Element* ed = by_id("fullcode");
    if (ed && (int)ed->GetScrollLeft() != g.code_scroll_x) refresh_highlight();
}

/// Scroll the console to the newest line. Must run after a layout pass, since
/// the scroll height is not known until the new lines have been laid out.
void follow_log() {
    if (!g.log_follow) return;
    g.log_follow = false;
    // Overshoot deliberately: RmlUi clamps to the real maximum, and the scroll
    // height can lag a layout pass behind the lines just appended. Asking for
    // more than exists is what reliably lands on the newest line.
    Rml::Element* e = by_id("log");
    if (!e) return;
    // Twice, with a layout between: RmlUi clamps the offset against the box it
    // last laid out, so the first call can stop short whenever the pane was
    // just resized. The second lands on the newest line.
    e->SetScrollTop((float)e->GetScrollHeight() + 4096.f);
    g.context->Update();
    e->SetScrollTop((float)e->GetScrollHeight() + 4096.f);
    sync_log_scroll();
}

void mark_dirty() { g.dirty = true; }

void run_app(const std::string& path);
void stop_app();
void stop_all();
void poll_build();
void set_activity(const char* what);
void refresh_highlight();
void sync_highlight_scroll();
std::string icon_img(const std::string& name, int px, const char* cls);
void follow_log();
void follow_code_scroll();
void size_output_pane();
void close_completion();
void render_diagnostics();
Rml::ElementFormControl* code_editor();
void drain_app_output();
void log(const std::string& line, const char* cls = nullptr);
Rml::ElementFormControl* code_editor();
bool apply_code();
void refresh_all();
void rebuild_tray();
bool is_selected(const std::string& id);
void update_tabs();
void open_handler(const std::string& id, const std::string& event = "");
void open_context_menu(const std::string& id, int x, int y);
void close_context_menu();

/// Record the model before a change, so it can be undone. Call BEFORE mutating.
void push_undo() {
    g.undo_stack.push_back(g.model);
    if (g.undo_stack.size() > 100) g.undo_stack.erase(g.undo_stack.begin());
    g.redo_stack.clear();
}

void undo() {
    if (g.undo_stack.empty()) { set_status("nothing to undo"); return; }
    // A snapshot remembers where the form was BEFORE a save moved it. Splicing
    // at those lines would overwrite whatever now lives there, so the spans
    // come from the model being replaced, which describes the file on disk.
    const Model live = g.model;
    g.redo_stack.push_back(g.model);
    g.model = g.undo_stack.back();
    g.undo_stack.pop_back();
    g.model.adopt_spans(live);
    if (!g.model.find(g.selected)) g.selected.clear();
    mark_dirty();
    refresh_all();
    set_status("undo");
}

void redo() {
    if (g.redo_stack.empty()) { set_status("nothing to redo"); return; }
    const Model live = g.model;
    g.undo_stack.push_back(g.model);
    g.model = g.redo_stack.back();
    g.redo_stack.pop_back();
    g.model.adopt_spans(live);
    if (!g.model.find(g.selected)) g.selected.clear();
    mark_dirty();
    refresh_all();
    set_status("redo");
}

/* --- toolbox -------------------------------------------------------------- */

bool matches_search(const std::string& name) {
    if (g.search.empty()) return true;
    std::string a = name, b = g.search;
    for (auto& c : a) c = (char)tolower((unsigned char)c);
    for (auto& c : b) c = (char)tolower((unsigned char)c);
    return a.find(b) != std::string::npos;
}

std::string lowered(const std::string& v) {
    std::string out = v;
    for (auto& c : out) c = (char)tolower((unsigned char)c);
    return out;
}

/// The toolbox, assembled from the catalogue rather than a list in this file.
///
/// Sections come from each kit's declared section, in the order `kiln kits`
/// resolved them. That is what makes a kit dropped into `kits/` show up here
/// with no change to the IDE — the promise the kit system exists to keep.
std::string build_toolbox() {
    std::vector<std::string> sections = g.catalog.sections;
    auto has_section = [&](const std::string& name) {
        for (const auto& s2 : sections) {
            if (s2 == name) return true;
        }
        return false;
    };
    // Sections that only unimplemented controls would fill still need a
    // heading, or the toolbox stops reading as the shape it is heading for.
    for (const auto& p : PLANNED) {
        if (!has_section(p.section)) {
            sections.insert(sections.empty() ? sections.end() : sections.end() - 1, p.section);
        }
    }

    std::string html;
    for (const auto& section : sections) {
        std::string body;
        for (const auto& c : g.catalog.components) {
            if (c.section != section) continue;
            // A form is what the canvas IS; offering one to drop into itself
            // is the one entry a toolbox must not have.
            if (c.type_name == "form") continue;
            if (!matches_search(c.type_name)) continue;
            std::string ico = icon_img(c.type_name, 16, "ico");
            if (ico.empty()) {
                ico = std::string("<span class='ico'>") +
                      (c.visual ? "\xe2\x96\xa0" : "\xe2\x97\x87") + "</span>";
            }
            body += "<div class='tool' oe-add='" + c.type_name + "'>" + ico + " " + c.type_name +
                    "</div>";
        }
        for (const auto& p : PLANNED) {
            if (std::strcmp(p.section, section.c_str()) != 0) continue;
            if (g.catalog.find(lowered(p.name))) continue;   // it exists now
            if (!matches_search(p.name)) continue;
            body += "<div class='tool soon' oe-soon='" + std::string(p.name) + "'>"
                    "<span class='ico'>\xe2\x96\xa1</span> " + p.name + "</div>";
        }
        if (body.empty()) continue;
        html += "<div class='sect'>" + esc(section) + "</div>" + body;
    }
    if (html.empty()) html = "<div class='hint'>No matches.</div>";
    return html;
}


/// Build the whole IDE chrome. Structure follows the Kiln Studio design
/// specification: title bar, menu bar, action toolbar, toolbox / designer /
/// inspector docks, a split code+output panel, and a status bar.
/// Locate a bundled asset (logo, icon) wherever Kiln was unpacked.
///
/// Tried in the order a real installation nests them: beside the binary's
/// directory in a release bundle (bin/ -> ../assets), inside the source tree
/// when running from a build, then the working directory. Returns "" when the
/// asset is missing, and every caller must cope — a missing logo should cost a
/// logo, not the IDE.
std::string asset_path(const char* name) {
    std::vector<std::string> roots;
    const std::string dir = kiln::sys::exe_dir();
    if (!dir.empty()) {
        roots.push_back(dir + "/../assets/");   // bundle: bin/ -> ../assets
        roots.push_back(dir + "/assets/");
        roots.push_back(dir + "/../../assets/"); // repo: designer/ -> ../assets
    }
    roots.push_back("assets/");
    for (const auto& r : roots) {
        const std::string candidate = r + name;
        if (kiln::sys::readable(candidate)) {
            // RmlUi resolves a decorator path as a URL and eats the leading
            // slash of an absolute one, so hand it back doubled.
            const std::string real = kiln::sys::real_path(candidate);
            if (!real.empty()) return "/" + real;
            return candidate;
        }
    }
    return "";
}

/// An `<img>` for a named toolbar or toolbox icon, or "" when the file is not
/// there. Callers fall back to their text glyph, so a missing icon costs a
/// little polish rather than a broken layout — the icon set is an asset, not a
/// dependency.
std::string icon_img(const std::string& name, int px, const char* cls) {
    const std::string path = asset_path(("icons/" + name + "_" + std::to_string(px) + ".png").c_str());
    if (path.empty()) return "";
    return "<img class='" + std::string(cls) + "' src='" + path + "'/>";
}

/// The stylesheet alone — everything between `<style>` and `</style>`.
///
/// Split out of `build_chrome` so the theme setting can regenerate it against
/// an already-running IDE: every colour in it was copied out of `theme::` as a
/// string when it was built, so a palette swap reaches the screen only by
/// building the sheet again and handing it to the document.
std::string build_styles(const std::string& family, const std::string& mono,
                         const std::string& dot_tile) {
    using namespace theme;
    const int WIN_W = g.win_w, WIN_H = g.win_h;
    const int content_y = TITLEBAR_H + MENUBAR_H + TOOLBAR_H;
    const int content_h = WIN_H - content_y - STATUS_H;
    const int centre_w  = WIN_W - TOOLBOX_W - INSPECT_W;
    const int canvas_h  = content_h - TABBAR_H - BOTTOM_H;

    std::ostringstream s;

    // ---- base -----------------------------------------------------------
    s << "body{font-family:'" << family << "';font-size:12px;color:" << TEXT
      << ";background-color:" << CHROME << "}";
    s << "div{display:block}";
    s << "#titlebar,#menubar,#toolbar,#toolbox,#centre,#inspectdock,#bottom,#status,"
         "#canvasarea,#formwin,#overlay,.pane,.wc,.dot,.selbox,.handle,.badge"
         "{position:absolute}";
    s << "span{display:inline}";

    // ---- title bar ------------------------------------------------------
    s << "#titlebar{left:0;top:0;width:" << WIN_W << "px;height:" << TITLEBAR_H
      << "px;background-color:" << CHROME << ";border-bottom:1px " << BORDER_SOFT << "}";
    s << "#titlebar .appicon{position:absolute;left:10px;top:6px;width:20px;height:20px;"
         "border-radius:5px;color:#fff;text-align:center;font-size:11px}";
    // Only the drawn fallback needs a background; the icon brings its own.
    s << "#titlebar div.appicon{background-color:" << ACCENT << ";padding-top:3px;"
         "width:18px;height:18px;top:7px}";
    s << "#titlebar .title{position:absolute;left:38px;top:8px;width:700px;height:18px;"
         "overflow:hidden;white-space:nowrap;font-size:13px;font-weight:bold;color:" << TEXT << "}";
    s << kiln::welcome::window_controls_styles();

    // ---- menu bar -------------------------------------------------------
    s << "#menubar{left:0;top:" << TITLEBAR_H << "px;width:" << WIN_W << "px;height:" << MENUBAR_H
      << "px;background-color:" << CHROME_ALT << ";border-bottom:1px " << BORDER_SOFT << "}";
    s << "#menubar .m{display:inline-block;padding:6px 10px 6px 10px;"
         "font-size:12px;color:" << TEXT << ";border-radius:4px}";
    s << "#menubar .m:hover{background-color:" << HOVER_STRONG << "}";
    s << "#menupop{position:absolute;background-color:" << PANEL << ";border:1px "
         << BORDER << ";border-radius:6px;padding:4px;"
         "box-shadow:#00000024 0 6px 18px 0px;z-index:10}";
    s << "#menupop .mi{display:block;padding:6px 26px 6px 10px;border-radius:4px;"
         "white-space:nowrap;color:" << TEXT << "}";
    s << "#menupop .mi:hover{background-color:" << ACCENT << ";color:#fff}";
    s << "#menupop .keys{color:" << TEXT_MUTED << ";padding-left:18px}";

    // ---- action toolbar -------------------------------------------------
    s << "#toolbar{left:0;top:" << (TITLEBAR_H + MENUBAR_H) << "px;width:" << WIN_W << "px;height:"
      << TOOLBAR_H << "px;background-color:" << CHROME_ALT << ";border-bottom:1px " << BORDER << "}";
    // An icon-only button is square: the label's padding would leave it
    // looking like a button someone forgot to write the text on.
    s << ".tb.ico{padding:5px 6px 0 6px}";
    s << ".tb{display:inline-block;height:26px;margin:7px 2px 0 2px;padding:5px 10px 0 10px;"
         "border-radius:5px;font-size:12px;color:" << TEXT << "}";
    s << ".tb:hover{background-color:" << HOVER_STRONG << "}";
    s << ".tb img.tbi{width:16px;height:16px;vertical-align:-3px;margin-right:6px}";
    s << ".tb.primary{background-color:" << ACCENT << ";color:" << ACCENT_TEXT << "}";
    s << ".tb.ghost{color:" << TEXT_MUTED << "}";
    s << ".tb.run{color:" << SUCCESS << ";font-weight:bold}";
    s << ".tb.stop{color:" << DANGER << "}";
    // Blocked rather than hidden: a Run button that vanishes while an app is
    // alive moves everything beside it, and a toolbar that reflows under the
    // pointer is worse than one that greys.
    s << ".tb.blocked{color:" << TEXT_MUTED << ";opacity:0.5}";
    s << ".tb.blocked:hover{background-color:transparent}";
    s << ".sep{display:inline-block;width:1px;height:20px;margin:10px 8px 0 8px;"
         "background-color:" << BORDER << "}";

    // ---- toolbox --------------------------------------------------------
    s << "#toolbox{left:0;top:" << content_y << "px;width:" << TOOLBOX_W << "px;height:" << content_h
      << "px;background-color:" << PANEL << ";border-right:1px " << BORDER << "}";
    s << ".panelhead{height:28px;padding:8px 10px 0 10px;font-size:11px;"
         "font-weight:bold;color:" << TEXT_MUTED << ";background-color:" << CHROME_ALT
      << ";border-bottom:1px " << BORDER_SOFT << "}";
    // The list scrolls, the header and the search box do not. Driven by
    // metadata the toolbox is as long as the installed kits make it, and a
    // panel that simply clips is a component the user cannot reach.
    s << "#toollist{overflow-y:auto}";
    s << "#search{margin:8px;width:" << (TOOLBOX_W - 16)
      << "px;height:26px;border:1px " << BORDER << ";border-radius:5px;background-color:" << PANEL
      << ";color:" << TEXT << ";padding-left:8px;font-size:12px}";
    s << ".sect{margin:6px 8px 2px 8px;font-size:11px;font-weight:bold;"
         "text-transform:uppercase;color:" << TEXT_MUTED << "}";
    s << ".tool{display:block;height:28px;margin:0 6px 2px 6px;padding:6px 8px 0 8px;"
         "border-radius:4px;font-size:12px;color:" << TEXT << "}";
    s << ".tool:hover{background-color:" << HOVER << "}";
    s << ".tool.sel{background-color:" << ACCENT << ";color:" << ACCENT_TEXT << "}";
    s << ".tool.soon{color:" << DISABLED << "}";
    s << ".tool .ico{display:inline-block;width:14px;color:" << ACCENT << "}";
    s << ".tool img.ico{width:16px;height:16px;vertical-align:-3px}";
    s << ".tool.soon .ico{color:" << DISABLED_ICO << "}";

    // ---- centre: tabs + canvas -----------------------------------------
    s << "#centre{left:" << TOOLBOX_W << "px;top:" << content_y << "px;width:" << centre_w
      << "px;height:" << content_h << "px;background-color:" << CANVAS << "}";
    // Document tabs. The bar's bottom rule is what the pane sits under; the
    // active tab is one pixel taller than the others so it covers that rule
    // and, painted in the pane's own colour, reads as joined to it. The
    // inactive ones stop on the rule and keep the chrome colour: recessed.
    s << "#tabs{height:" << (TABBAR_H - 1) << "px;background-color:" << CHROME_ALT
      << ";border-bottom:1px " << BORDER << ";padding-left:8px;white-space:nowrap}";
    s << ".tab{display:inline-block;height:18px;margin:7px 2px 0 0;padding:5px 14px 0 14px;"
         "font-size:12px;color:" << TEXT_MUTED << ";background-color:" << CHROME
      << ";border-top:1px " << BORDER << ";border-left:1px " << BORDER << ";border-right:1px "
      << BORDER << ";border-top-left-radius:6px;border-top-right-radius:6px}";
    s << ".tab:hover{color:" << TEXT << "}";
    s << ".tab.active{height:20px;margin-top:5px;padding-top:6px;color:" << TEXT
      << ";background-color:" << CANVAS << "}";
    s << ".tab.active.code{background-color:" << PANEL << "}";
    s << ".tab .tabfile{color:" << TEXT_MUTED << ";margin-left:5px}";
    s << ".tab.active .tabfile{color:" << ACCENT << "}";
    s << "#codeview{position:absolute;left:0;top:" << TABBAR_H << "px;width:100%;"
         "background-color:" << PANEL << ";overflow:auto}";
    // The editor fills the pane. A textarea has no default appearance in RmlUi,
    // so colours are explicit — the default text colour is white, which on this
    // panel would be invisible.
    // Indeterminate progress: the bar sweeps because we cannot know how far
    // through a build we are, and a fake percentage would be a lie.
    s << "@keyframes sweep{0%{margin-left:0px}100%{margin-left:96px}}";
    s << "@keyframes pulse{0%{opacity:1}100%{opacity:0.25}}";
    s << "#activity{position:absolute;right:16px;top:8px;width:220px}";
    s << "#activitylabel{display:inline-block;font-size:11px;color:" << TEXT_MUTED
      << ";margin-right:8px;vertical-align:middle}";
    s << "#activitytrack{display:inline-block;width:128px;height:4px;border-radius:2px;"
         "background-color:" << HAIRLINE << ";vertical-align:middle}";
    s << "#activitybar{width:32px;height:4px;border-radius:2px;background-color:" << ACCENT
      << ";animation:1.1s cubic-in-out infinite alternate sweep}";
    s << "#runlamp{position:absolute;right:250px;top:6px;color:#28c840;font-size:14px;"
         "animation:0.7s sine-in-out infinite alternate pulse}";
    // RmlUi ships no default scrollbar appearance or size. Left unstyled, a
    // textarea's slider covers the whole control and eats every click — which
    // is what made the code editor impossible to focus, and so impossible to
    // type in. Sizing them is not cosmetic.
    s << "scrollbarvertical{width:10px;background-color:" << CANVAS << "}";
    s << "scrollbarhorizontal{height:10px;background-color:" << CANVAS << "}";
    s << "scrollbarvertical slidertrack,scrollbarhorizontal slidertrack{"
         "background-color:" << CANVAS << "}";
    s << "scrollbarvertical sliderbar{width:10px;min-height:24px;border-radius:5px;"
         "background-color:" << SCROLL_THUMB << "}";
    s << "scrollbarhorizontal sliderbar{height:10px;min-width:24px;border-radius:5px;"
         "background-color:" << SCROLL_THUMB << "}";
    s << "scrollbarvertical sliderbar:hover,scrollbarhorizontal sliderbar:hover{"
         "background-color:" << TEXT_MUTED << "}";
    // No stepper arrows: undersized ones are the other way a scrollbar becomes
    // a click trap.
    s << "sliderarrowdec,sliderarrowinc{width:0px;height:0px}";
    // Both layers share typography exactly. Any difference in family, size,
    // line-height or padding shows up as text drifting away from its colour.
    // The breakpoint gutter, left of both layers. `overflow:hidden` and never
    // an RmlUi scrollbar: an unstyled one covers its owner and eats every
    // click, which has broken Studio twice. It is scrolled by rendering the
    // rows the code view is showing, not by scrolling the element.
    s << "#dbggutter{position:absolute;left:0;top:0;width:" << CODE_GUTTER_W
      << "px;bottom:0;overflow:hidden;background-color:" << PANEL << "}";
    s << "#dbggutter div{height:" << CODE_LINE_H << "px;line-height:" << CODE_LINE_H
      << "px;font-size:" << (CODE_FONT_PX - 1) << "px;color:" << GUTTER << "}";
    s << "#dbggutter div:hover{background-color:" << HOVER << "}";
    s << "#dbggutter .mk{display:inline-block;width:" << CODE_MARK_W
      << "px;text-align:center}";
    s << "#dbggutter .cue{display:inline-block;width:" << CODE_CUE_W
      << "px;text-align:center;color:" << ACCENT << "}";
    // Right-aligned against the code, so the digits line up whatever their
    // count and the eye reads down one edge.
    s << "#dbggutter .num{display:inline-block;width:"
      << (CODE_GUTTER_W - CODE_MARK_W - CODE_CUE_W - 6)
      << "px;text-align:right}";
    // Solid for a breakpoint the engine bound, hollow for one it could not.
    // The distinction earns its pixels: a breakpoint that silently never fires
    // is the worst thing a debugger can do to someone.
    s << "#dbggutter div.bp .mk{color:" << BREAKPOINT << "}";
    s << "#dbggutter div.pending .mk{color:" << GUTTER << "}";
    // The stopped row's number is lit too, so the cue survives being scrolled
    // to the edge of the view where the tint is easy to miss.
    s << "#dbggutter div.cur .num{color:" << ACCENT << "}";
    s << "#codehl{position:absolute;left:" << CODE_GUTTER_W << "px;top:0;font-family:'" << mono
      << "';font-size:" << CODE_FONT_PX << "px;line-height:" << CODE_LINE_H << "px;padding:" << CODE_PAD_Y << "px "
      << CODE_PAD_X << "px;padding-top:0px;white-space:pre;color:" << TEXT << "}";
    // Relative, so a diagnostic's underline can be placed within its row.
    //
    // `nowrap` and `overflow:hidden`, not `pre` alone: the textarea above is
    // `wrap='nowrap'` and scrolls a long line sideways, so a layer that wrapped
    // one instead put its tail on a second visual row and shoved every line
    // below it down a row. The two layers then drew different text at the same
    // y — the code looked garbled, and in a narrow pane it slid out of the
    // clipped box altogether and the editor went white. Indentation survives
    // `nowrap` because `escape_code` has already turned each space into U+00A0.
    s << "#codehl div{white-space:nowrap;overflow:hidden;height:" << CODE_LINE_H
      << "px;position:relative}";
    // Absolutely-positioned children ignore an ancestor's overflow in RmlUi
    // unless told to respect it. Without this the editor keeps painting once
    // scrolled — straight over the menu bar and toolbar.
    s << "#codehl,#fullcode{clip:always}";
    // The diagnostic's range is underlined, and the line it sits on is tinted
    // faintly so the mark can be found when it is off to the right. The bar
    // is a positioned child of the line's div: the font is monospace, so a
    // column is a multiple of one glyph's width and the bar lands under
    // exactly the name the server meant.
    s << "#codehl div.badline{background-color:" << BADLINE << "}";
    // The line the program is stopped on. Brighter than a diagnostic's tint,
    // because it is where the user is looking rather than something to notice.
    s << "#codehl div.stopline{background-color:" << STOPLINE << "}";
    // The Variables pane. Rows are plain text at the code's own size, because
    // a value is read rather than skimmed.
    s << "#vars{position:absolute;left:0;right:0;top:26px;bottom:0;overflow:auto;padding:4px 10px;"
         "font-family:'" << mono << "';font-size:" << (CODE_FONT_PX - 1) << "px}";
    s << "#vars .varrow{padding:1px 0;color:" << TEXT << "}";
    s << "#vars .varname{color:" << ACCENT << "}";
    s << "#vars .frame{color:" << TEXT << "}";
    s << "#vars .dim{color:" << TEXT_MUTED << "}";
    // The row's content, shifted left when the line is scrolled sideways. The
    // row clips it, so the layer stays inside the view whatever the offset.
    // Positioned, so a diagnostic's bar measures its column from the same
    // origin as the glyphs it underlines.
    s << "#codehl .sh{position:relative;display:inline-block;white-space:nowrap}";
    s << "#codehl .dline{position:absolute;height:2px;background-color:" << DANGER << "}";
    s << "#codehl .dline.warn{background-color:#bf8700}";
    s << "#problems{height:22px;overflow-y:auto;padding:6px 12px}";
    s << ".problem{font-size:12px;color:" << TEXT << ";padding:2px 0}";
    s << ".problem .pline{color:" << DANGER << ";margin-right:8px}";
    s << ".problem.ref .pline{color:" << ACCENT << "}";
    s << ".problem.ref:hover{background-color:" << HOVER << "}";
    s << ".noproblems{font-size:12px;color:" << TEXT_MUTED << ";font-style:italic}";
    // The completion popup, under the caret. Positioned at body level, not
    // inside the code view: the view clips its children, and a popup for the
    // last visible line has to hang below it.
    s << "#complete{position:absolute;background-color:" << PANEL << ";border:1px " << BORDER
      << ";border-radius:5px;padding:4px 0;z-index:80;min-width:280px;max-width:560px;"
         "font-size:12px;overflow:hidden;box-shadow:#00000024 0 4px 14px 0px}";
    s << ".citem{height:16px;padding:3px 10px;white-space:nowrap;overflow:hidden;cursor:pointer}";
    s << ".citem.sel{background-color:" << TINT << "}";
    s << ".citem .clabel{font-family:'" << mono << "';color:" << TEXT << "}";
    s << ".citem .cdetail{color:" << TEXT_MUTED << ";margin-left:10px}";
    // A component's context menu: its events, the way Delphi's offers them.
    // Above the overlay and the selection anchors, which it opens over.
    // Rows are 28px: 14px of text and 7px above and below.
    s << "#ctxmenu{position:absolute;background-color:" << PANEL << ";border:1px " << BORDER
      << ";border-radius:8px;padding:4px;z-index:90;min-width:200px;font-size:12px;"
         "box-shadow:#0000001f 0 8px 16px 0px,#0000000a 0 2px 4px 0px}";
    s << ".cxrow{display:block;height:14px;padding:7px 12px;border-radius:4px;white-space:nowrap;"
         "color:" << TEXT << ";cursor:pointer}";
    s << ".cxrow:hover{background-color:" << ACCENT << ";color:#fff}";
    s << ".cxrow:hover .cxhandler{color:#fff}";
    s << ".cxhandler{color:" << TEXT_MUTED << "}";
    s << ".cxhead{display:block;height:14px;padding:7px 12px;white-space:nowrap;color:" << TEXT
      << ";font-weight:bold}";
    s << ".cxhead .cxtype{color:" << TEXT_MUTED << ";font-weight:normal;margin-left:6px}";
    s << ".cxlabel{display:block;height:14px;padding:7px 12px;color:" << TEXT_MUTED
      << ";font-size:11px}";
    s << ".cxsep{height:1px;margin:4px 0;background-color:" << BORDER_SOFT << "}";
    // The hover tooltip: what the language server says about the name under
    // the pointer. Monospace, because the first line is a signature.
    s << "#tip{position:absolute;background-color:" << PANEL << ";border:1px " << BORDER
      << ";border-radius:5px;padding:6px 10px;z-index:70;max-width:520px;font-family:'" << mono
      << "';font-size:12px;color:" << TEXT << ";white-space:pre;"
         "box-shadow:#00000024 0 4px 14px 0px}";
    s << "#tip .tipkind{font-family:'" << family << "';color:" << TEXT_MUTED
      << ";font-size:11px;margin:5px 0 5px 0;line-height:1.4}";
    s << "#tip .tipcode{background-color:" << CHROME_ALT
      << ";border-radius:4px;padding:5px 7px 5px 7px;margin-bottom:2px;line-height:1.4}";
    // The CONTAINER scrolls, not the textarea. RmlUi keeps a text control's
    // own overflow hidden, so the wheel did nothing there — and mirroring one
    // layer's scroll onto the other is a sync bug waiting to happen. Sizing
    // both layers to the content and scrolling their parent means they cannot
    // drift apart by construction.
    // hidden, not auto: Studio scrolls the editor itself by moving both layers,
    // so RmlUi's own scrollbars are not wanted — but the clipping region very
    // much is. Without it the layers keep drawing once their offset goes
    // negative, painting the code straight over the menus and toolbar.
    s << "#codeview{background-color:" << PANEL << ";overflow:hidden}";
    // Positioned, so it paints ABOVE the highlight layer. Left unpositioned it
    // sits below an absolutely-positioned sibling — taking the caret and the
    // selection with it, which is an editor you cannot see yourself typing in.
    s << "#fullcode{position:absolute;left:" << CODE_GUTTER_W << "px;top:0;font-family:'" << mono << "';line-height:" << CODE_LINE_H << "px;font-size:" << CODE_FONT_PX << "px;padding:"
      // Top padding is zero on BOTH layers: the vertical offset is applied to
      // each layer's `top` instead. Leaving it on one and not the other put the
      // two half a line apart — every glyph doubled.
      << "0px " << CODE_PAD_X << "px;"
         "width:100%;height:100%;background-color:transparent;"
         // Transparent glyphs: the layer underneath supplies the colour. The
         // caret and the selection must stay opaque or the editor looks dead.
         "color:#00000000;border:0;caret-color:" << TEXT << ";cursor:text}";
    // RmlUi styles a text control's selection through a child `selection`
    // element, not through CSS properties on the control itself.
    s << "#fullcode selection{background-color:" << SELECTION << ";color:" << TEXT << "}";
    // The dot grid, kept as a string so the Show-the-grid setting is a real
    // two-way switch. Turning it off is `decorator:none`; turning it back on
    // needs this exact value again, and it is composed here from a tile path
    // nothing else has.
    g.grid_decorator = dot_tile.empty() ? "" : "image(\"" + dot_tile + "\" repeat)";
    s << "#canvasarea{left:0;top:" << TABBAR_H << "px;width:" << centre_w << "px;height:" << canvas_h
      << "px;background-color:" << CANVAS_GRID << ";decorator:"
      << (kiln::settings::boolean("designer.show_grid") && !g.grid_decorator.empty()
              ? g.grid_decorator
              : std::string("none"))
      << "}";

    // The form preview, drawn as the window it will become: a Windows 11
    // frame — 8px corners, a hairline border, a soft drop shadow. The title
    // bar is the form's own colour, seamless with the client area and parted
    // from it by one rule; it is decoration only, and the client area under
    // it is where the file's coordinates start.
    s << "#formwin{left:60px;top:40px;border-radius:8px;border:1px " << BORDER
      << ";background-color:#f3f3f3;"
         "box-shadow:#0000001a 0 10px 25px -5px, #0000000d 0 8px 10px -6px}";
    s << "#formtitle{position:relative;height:" << (FORM_TITLE_H - 1)
      << "px;border-bottom:1px " << HAIRLINE << ";border-top-left-radius:7px;border-top-right-radius:7px;"
         "font-size:13px;font-weight:bold;white-space:nowrap;overflow:hidden}";
    s << "#formicon{position:absolute;left:14px;top:" << (FORM_TITLE_H / 2 - 8) << "px;width:16px;height:16px}";
    s << "#formtitletext{position:absolute;left:38px;top:" << (FORM_TITLE_H / 2 - 8) << "px}";
    // The caption buttons are glyphs alone — no plate behind them, on hover
    // or otherwise — 10px across with 12px between, 14px in from the edge.
    // Minimize and maximize are drawn, so they are exactly the shapes and
    // sizes a caption shows; the close cross is the multiplication sign the
    // loaded face has, in the same colour.
    s << ".wbtn{position:absolute;top:0;width:22px;height:" << FORM_TITLE_H
      << "px;text-align:center;padding-top:" << (FORM_TITLE_H / 2 - 8) << "px;font-size:13px;font-weight:normal}";
    s << ".wbtn div{position:absolute;left:6px;box-sizing:border-box}";
    // RCSS has no `currentcolor`: the drawn glyphs take the text colour by
    // name here, and rebuild_canvas recolours them with the title's text.
    s << ".wbtn .mini{top:" << (FORM_TITLE_H / 2) << "px;width:10px;height:1px;background-color:" << TEXT << "}";
    s << ".wbtn .maxi{top:" << (FORM_TITLE_H / 2 - 5) << "px;width:10px;height:10px;border:1px " << TEXT << "}";
    // Components on the canvas get the SAME default styling as in the built
    // app, from the shared mapping — otherwise the preview lies.
    // SCOPED to the canvas: these rules include `div{position:absolute}`, which
    // would otherwise collapse every panel in the IDE onto one point.
    s << kiln::ui::control_styles("#canvas");
    s << "#canvas{position:relative;overflow:hidden;border-bottom-left-radius:8px;"
         "border-bottom-right-radius:8px}";
    // A faint centred wordmark on the design surface — a maker's mark, not
    // chrome. Behind the form window and the overlay (z-index 0; formwin sits
    // above), click-through, and low enough opacity to never fight the work.
    s << "#canvaswm{position:absolute;left:50%;top:50%;transform:translate(-50%,-50%);"
         "width:42%;max-width:520px;opacity:0.05;pointer-events:none;z-index:0}";
    // selection chrome
    // Above the components, or the handles are under them and only their
    // outer three pixels can be grabbed — and the pointer never rests on one
    // long enough to show its cursor. The overlay itself lets every hit
    // through, so components still take their own clicks; only the chrome
    // drawn on it is solid.
    s << "#overlay{position:absolute;left:0;top:0;width:100%;height:100%;z-index:5;"
         "pointer-events:none}";
    // Only the selection's badge takes the pointer. A badge floats over
    // whatever is above its component, and one that took every click would
    // make the component under it impossible to select.
    s << ".handle,.fgrip,.badge.live{pointer-events:auto}";
    // The selection's box and anchors are sized as border boxes, so the
    // numbers placing them are the numbers on screen.
    s << ".selbox,.handle{box-sizing:border-box}";
    s << ".selbox{border:1.5px " << SELECT << "}";
    // The anchors editor: a box a side, before the field, lit when set.
    s << ".anchbox{display:inline-block;width:88px;height:24px;vertical-align:top;margin-right:6px}";
    s << ".anch{display:inline-block;width:18px;height:18px;margin:2px 2px 0 0;text-align:center;"
         "padding-top:2px;font-size:10px;border:1px " << BORDER << ";border-radius:3px;background-color:"
      << PANEL << ";color:" << TEXT_MUTED << "}";
    s << ".anch:hover{border:1px " << ACCENT << "}";
    s << ".anch.on{background-color:" << ACCENT << ";border:1px " << ACCENT << ";color:#ffffff}";
    s << ".prow input.withanch{display:inline-block;width:" << (INSPECT_W - 40 - 94) << "px}";
    s << ".selbox.alt{border:1.5px #8fc0f5}";
    s << ".guide{background-color:#ff4d9a}";
    s << ".fgrip{background-color:#00000000}";
    s << ".fgrip:hover{background-color:" << SELECT << "}";
    s << ".fgrip.corner{border-radius:2px}";
    s << ".handle{width:6px;height:6px;background-color:" << SELECT << ";border:1px #ffffff}";
    // Cursor per anchor, so the resize direction is obvious before clicking.
    s << ".handle.nw,.handle.se{cursor:resize-nwse}";
    s << ".handle.ne,.handle.sw{cursor:resize-nesw}";
    s << ".handle.n,.handle.s{cursor:resize-ns}";
    s << ".handle.e,.handle.w{cursor:resize-ew}";
    s << ".fgrip.e{cursor:resize-ew}.fgrip.s{cursor:resize-ns}";
    s << ".fgrip.corner{cursor:resize-nwse}";
    // A faint pill, the selection's blue on white, centred over the
    // component it wires: `left` is the component's middle and the badge
    // is shifted back by half its own width, whatever that turns out to be.
    s << ".badge{box-sizing:border-box;height:" << BADGE_H << "px;background-color:#ffffffe6;color:"
      << SELECT << ";font-size:11px;padding:2px 8px 0 7px;border:1px " << SELECT
      << ";border-radius:10px;white-space:nowrap;cursor:pointer;transform:translateX(-50%)}";
    s << ".badge:hover{background-color:" << TINT << "}";
    // The link glyph is drawn, not typed: no face the IDE loads has one, and
    // a character the font lacks renders as nothing at all.
    // Selected by id as well as class: the canvas's own rule makes every div
    // in it absolute, and the glyph has to hold its place in the line.
    s << "#overlay .chain{display:inline-block;position:relative;width:15px;height:10px;"
         "margin-right:5px;vertical-align:-1px}";
    s << "#overlay .chain div{position:absolute;width:6px;height:4px;border:1px " << SELECT << ";"
         "border-radius:3px}";
    s << "#overlay .chain .a{left:0;top:0}#overlay .chain .b{left:6px;top:3px}";
    s << ".badge .ev{margin-right:4px}.badge .arrow{margin:0 5px 0 1px}";

    // ---- non-visual component tray --------------------------------------
    s << "#tray{position:absolute;left:0;top:" << (canvas_h - TRAY_H) << "px;width:" << centre_w
      << "px;height:" << TRAY_H << "px;background-color:" << CHROME_ALT << ";border-top:1px "
      << BORDER << "}";
    s << "#tray .trayhead{height:20px;padding:4px 10px 0 10px;font-size:10px;font-weight:bold;"
         "color:" << TEXT_MUTED << ";text-transform:uppercase}";
    s << "#traylist{padding:4px 8px 0 8px;white-space:nowrap}";
    s << ".trayitem{display:inline-block;width:88px;height:62px;margin-right:8px;padding-top:6px;"
         "text-align:center;border:1px " << BORDER_SOFT << ";border-radius:6px;background-color:"
      << PANEL << "}";
    s << ".trayitem:hover{border:1px " << ACCENT << "}";
    s << ".trayitem.sel{border:2px " << ACCENT << ";background-color:" << TINT << "}";
    s << ".trayico{font-size:20px;color:" << ACCENT << ";height:24px}";
    s << ".traylabel{font-size:11px;color:" << TEXT << ";white-space:nowrap;overflow:hidden}";
    s << ".traytype{font-size:10px;color:" << TEXT_MUTED << "}";
    s << ".trayhint{font-size:11px;color:" << TEXT_MUTED << ";padding:6px 4px 0 4px}";

    // ---- inspector ------------------------------------------------------
    s << "#inspectdock{left:" << (TOOLBOX_W + centre_w) << "px;top:" << content_y << "px;width:"
      << INSPECT_W << "px;height:" << content_h << "px;background-color:" << PANEL
      << ";border-left:1px " << BORDER << "}";
    // Properties / Events as tabs, in the same idiom as the document tabs:
    // the chosen one is underlined in the accent and the other is not.
    s << ".segbar{height:28px;padding:0 10px;white-space:nowrap;border-bottom:1px " << BORDER_SOFT
      << "}";
    s << ".seg{display:inline-block;height:20px;margin-right:14px;padding:8px 2px 0 2px;"
         "font-size:12px;color:" << TEXT_MUTED << "}";
    s << ".seg:hover{color:" << TEXT << "}";
    s << ".seg.active{color:" << ACCENT << ";font-weight:bold;border-bottom:2px " << ACCENT << "}";
    s << ".prow label .evgo{color:" << ACCENT << ";margin-left:8px;font-size:11px}";
    s << ".prow label .evgo:hover{text-decoration:underline}";
    s << ".prow label .evname{color:" << TEXT << ";font-weight:bold}";
    s << "#ctxlabel{margin:8px 10px 6px 10px;height:18px;font-size:13px;font-weight:bold;"
         "color:" << TEXT << "}";
    // The list scrolls above the wiring box, which does not move: a button
    // declares more properties than the dock is tall.
    s << "#grid{padding:0 10px 0 10px;overflow-y:auto}";
    s << ".prow{margin-bottom:8px}";
    s << ".prow label{display:block;font-size:11px;color:" << TEXT_MUTED << ";margin-bottom:2px}";
    s << ".prow input{display:block;width:" << (INSPECT_W - 40)
      << "px;height:24px;border:1px " << BORDER << ";border-radius:4px;background-color:" << PANEL
      << ";color:" << TEXT << ";padding-left:6px;font-size:12px}";
    s << ".prow input:focus{border:1px " << ACCENT << "}";
    s << ".note{font-size:11px;font-style:italic;color:" << TEXT_MUTED << ";margin-top:2px}";
    // Property editors. Which one a property gets is the descriptor's `editor`
    // hint, not a guess from its name or its value.
    s << ".swatch{display:inline-block;width:24px;height:24px;border:1px " << BORDER
      << ";border-radius:4px;vertical-align:top;margin-right:6px}";
    s << ".swatch:hover{border:1px " << ACCENT << "}";
    s << ".prow input.withswatch{display:inline-block;width:" << (INSPECT_W - 40 - 40) << "px}";
    s << ".prow input.withbtn{display:inline-block;width:" << (INSPECT_W - 40 - 84) << "px}";
    s << ".browse{display:inline-block;width:66px;height:24px;margin-left:6px;padding-top:4px;"
         "text-align:center;font-size:11px;border:1px " << BORDER << ";border-radius:4px;"
         "background-color:" << CHROME << ";color:" << TEXT << ";vertical-align:top}";
    s << ".browse:hover{border:1px " << ACCENT << ";color:" << ACCENT << "}";
    s << ".prow textarea{display:block;width:" << (INSPECT_W - 40)
      << "px;height:72px;border:1px " << BORDER << ";border-radius:4px;background-color:" << PANEL
      << ";color:" << TEXT << ";padding:4px 0 0 6px;font-size:12px}";
    s << ".prow textarea:focus{border:1px " << ACCENT << "}";
    // The popups the swatch and the browse button open.
    s << "#editpop{position:absolute;background-color:" << PANEL << ";border:1px " << BORDER
      << ";border-radius:6px;padding:8px;z-index:60;width:236px}";
    s << "#editpop .poptitle{font-size:11px;font-weight:bold;color:" << TEXT_MUTED
      << ";margin-bottom:6px}";
    s << ".chip{display:inline-block;width:22px;height:22px;margin:0 3px 3px 0;border:1px "
      << BORDER_SOFT << ";border-radius:3px}";
    s << ".chip:hover{border:2px " << ACCENT << "}";
    s << ".fileitem{display:block;height:22px;padding:3px 6px 0 6px;font-size:11px;color:" << TEXT
      << ";border-radius:3px;white-space:nowrap;overflow:hidden}";
    s << ".fileitem:hover{background-color:" << HOVER << ";color:" << ACCENT << "}";
    s << "#editpop .hint{padding:4px}";
    s << "#wirebox{position:absolute;left:0;width:" << (INSPECT_W - 20) << "px;height:" << WIRE_H
      << "px;padding:8px 10px 0 10px;background-color:" << CHROME_ALT << ";border-top:1px "
      << BORDER_SOFT << ";overflow:hidden}";
    s << "#wirebox .h{font-size:11px;font-weight:bold;color:" << TEXT_MUTED << ";margin-bottom:6px}";
    s << "#wirebox .row{font-size:12px;color:" << TEXT << ";white-space:nowrap;margin-bottom:3px}";
    s << "#wirebox .link{color:" << ACCENT << ";cursor:pointer}";
    s << "#wirebox .link:hover{text-decoration:underline}";
    s << "#wirebox .unlinked{color:" << TEXT_MUTED << ";font-size:12px}";
    s << ".hint{color:" << TEXT_MUTED << ";font-size:12px;padding:10px}";

    // ---- bottom split ---------------------------------------------------
    s << "#bottom{left:" << TOOLBOX_W << "px;top:" << (content_y + TABBAR_H + canvas_h)
      << "px;width:" << centre_w << "px;height:" << BOTTOM_H << "px;background-color:" << PANEL
      << ";border-top:1px " << BORDER << "}";
    s << ".pane{position:absolute;top:0;height:" << BOTTOM_H << "px;overflow:hidden}";
    s << ".panehead{height:26px;padding:6px 10px 0 10px;font-size:11px;"
         "font-weight:bold;color:" << TEXT_MUTED << ";background-color:" << CHROME_ALT
      << ";border-bottom:1px " << BORDER_SOFT << "}";
    s << "#code{font-family:'" << family
      << "';font-size:12px;padding:6px 0 0 0;height:" << (BOTTOM_H - 32) << "px;overflow:auto}";
    s << ".ln{color:" << GUTTER << "}";
    s << ".cl{display:block;white-space:nowrap}";
    s << ".ln{display:inline-block;width:34px;text-align:right;padding-right:10px}";
    
    s << ".k{color:" << SYN_KEYWORD << "}.m{color:" << SYN_METHOD << "}.s{color:" << SYN_STRING
      << "}.i{color:" << SYN_IDENT << "}.c{color:" << SYN_COMMENT << ";font-style:italic}.n{color:"
      << SYN_NUMBER << "}";
    // The console's two layers share one box, sized by size_output_pane() to
    // a whole number of rows so the textarea's furthest scroll is a whole
    // number of rows too — the layer under it can then never be half a line
    // out. The same recipe as #codehl / #fullcode: one line box on both, top
    // padding zero on both, glyphs transparent on top.
    s << "#logbox{position:relative;overflow:hidden;height:"
      << (BOTTOM_H - 2 * PANEHEAD_H - PROBLEMS_H) << "px}";
    s << "#loghl{position:absolute;left:0;top:0;font-family:'" << family
      << "';font-size:12px;line-height:" << LOG_LINE_H << "px;padding:0 10px;color:" << TEXT
      << ";white-space:nowrap}";
    // `pre`, as the editor's layer: `nowrap` collapses the leading spaces
    // the textarea keeps, and every indented line lands a column early.
    s << "#loghl div{white-space:pre;overflow:hidden;height:" << LOG_LINE_H << "px}";
    s << "#loghl .ok{color:" << SUCCESS << "}#loghl .muted{color:" << TEXT_MUTED
      << "}#loghl .err{color:" << DANGER << "}";
    s << "#log{position:absolute;left:0;top:0;font-family:'" << family
      << "';font-size:12px;line-height:" << LOG_LINE_H
      << "px;padding:0 10px;background-color:transparent;color:#00000000;border:0;"
         "caret-color:" << TEXT_MUTED << ";cursor:text}";
    s << "#log selection{background-color:" << SELECTION << ";color:" << TEXT << "}";

    // ---- status bar -----------------------------------------------------
    s << ".split{position:absolute;background-color:#00000000}";
    s << ".split.v{cursor:resize-ew}.split.h{cursor:resize-ns}";
    s << ".split:hover{background-color:" << ACCENT << "}";
    s << "#status{left:0;top:" << (WIN_H - STATUS_H) << "px;width:" << WIN_W << "px;height:"
      << STATUS_H << "px;background-color:" << CHROME << ";border-top:1px " << BORDER
      << ";font-size:11px;color:" << TEXT_MUTED << ";padding:5px 10px 0 10px}";
    s << "#status .right{position:absolute;right:26px;top:5px;width:200px;text-align:right;white-space:nowrap}";
    s << "#status .dot{color:" << SUCCESS << "}";

    return s.str();
}

std::string build_chrome(const std::string& family, const std::string& mono,
                         const std::string& dot_tile) {
    using namespace theme;
    const int WIN_W = g.win_w, WIN_H = g.win_h;
    const int content_y = TITLEBAR_H + MENUBAR_H + TOOLBAR_H;
    const int content_h = WIN_H - content_y - STATUS_H;
    const int centre_w  = WIN_W - TOOLBOX_W - INSPECT_W;
    const int canvas_h  = content_h - TABBAR_H - BOTTOM_H;
    (void)content_y; (void)canvas_h;
    std::ostringstream s;
    s << "<rml><head><style>" << build_styles(family, mono, dot_tile);
    s << "</style></head><body>";

    // ---- markup ---------------------------------------------------------
    const std::string icon = asset_path("kiln-icon-64.png");
    s << "<div id='titlebar'>"
      << (icon.empty() ? std::string("<div class='appicon'>E</div>")
                       : "<img class='appicon' src='" + icon + "'/>")
      << "<div class='title'>Kiln Studio — " << esc(basename_of(g.model.path)) << " — ["
      << esc(g.model.form_name) << "]</div>" << kiln::welcome::window_controls_markup()
      << "</div>";

    s << "<div id='menubar'>";
    for (size_t i = 0; i < menus().size(); i++) {
        s << "<div class='m' oe-menu='" << i << "'>" << menus()[i].title << "</div>";
    }
    s << "</div><div id='menupop' style='display:none'/>"
         "<div id='editpop' style='display:none'/>"
         "<div id='tip' style='display:none'/>"
         "<div id='complete' style='display:none'/>"
         "<div id='ctxmenu' style='display:none'/>";

    s << "<div id='toolbar'>"
         "<div class='tb' oe-action='save'>" + icon_img("save", 16, "tbi") + "Save</div>"
         "<div class='tb' oe-action='undo'>Undo</div>"
         "<div class='tb' oe-action='redo'>Redo</div>"
         "<div class='sep'/>"
         // Activity indicator: an indeterminate bar while the toolchain works,
         // and a pulsing lamp for as long as an app is alive.
         "<div id='activity' style='display:none'><div id='activitylabel'/>"
         "<div id='activitytrack'><div id='activitybar'/></div></div>"
         "<span id='runlamp' style='display:none'>●</span>"
         "<div class='sep'/>"
         "<div class='tb run' id='btn_run' oe-action='run'>" + icon_img("run", 16, "tbi") + "Run</div>"
         "<div class='tb' oe-action='build'>" + icon_img("build", 16, "tbi") + "Build Binary</div>"
         "<div class='tb stop' oe-action='stop'>" + icon_img("stop", 16, "tbi") + "Stop</div>"
         // The debugging controls. Hidden until a session starts, so the
         // toolbar of someone who never debugs is the toolbar they had —
         // and they replace nothing, because Run and Debug are different
         // things and a user who wants one does not want the other.
         "<div class='sep'/>"
         "<div class='tb' id='btn_debug' oe-action='debug'>" + icon_img("debug", 16, "tbi") + "Debug</div>"
         // The transport is icons alone. Seven labelled buttons is more
         // toolbar than the window has, and these are the glyphs every
         // debugger uses — the Debug menu beside them carries the words and
         // the keys for anyone who wants them spelled out.
         "<div class='tb ico dbgctl' id='btn_dbgcontinue' oe-action='dbgcontinue' style='display:none'>" +
             icon_img("continue", 16, "tbi") + "</div>"
         "<div class='tb ico dbgctl' id='btn_dbgpause' oe-action='dbgpause' style='display:none'>" +
             icon_img("pause", 16, "tbi") + "</div>"
         "<div class='tb ico dbgctl' id='btn_dbgstepover' oe-action='dbgstepover' style='display:none'>" +
             icon_img("stepover", 16, "tbi") + "</div>"
         "<div class='tb ico dbgctl' id='btn_dbgstepin' oe-action='dbgstepin' style='display:none'>" +
             icon_img("stepin", 16, "tbi") + "</div>"
         "<div class='tb ico dbgctl' id='btn_dbgstepout' oe-action='dbgstepout' style='display:none'>" +
             icon_img("stepout", 16, "tbi") + "</div>"
         "<div class='tb ico dbgctl stop' id='btn_dbgstop' oe-action='dbgstop' style='display:none'>" +
             icon_img("stop", 16, "tbi") + "</div>"
         "</div>";

    s << "<div id='toolbox'><div class='panelhead'>TOOLBOX</div>"
         "<input type='text' id='search' placeholder='Search toolbox...'/>"
         "<div id='toollist'>"
      << build_toolbox() << "</div></div>";

    s << "<div id='centre'>"
         "<div id='tabs'>"
         // Labels are written by update_tabs(): the Code tab's changes with
         // the caret, and one writer is fewer than two.
         "<div class='tab active' id='tabdesigner' oe-view='designer'/>"
         "<div class='tab' id='tabcode' oe-view='code'/>"
         "</div>"
         // The editor is two layers. Underneath, `#codehl` paints the
         // syntax-highlighted text; on top, a real RmlUi <textarea> supplies
         // the caret, selection, keyboard handling and clipboard, with its own
         // text made transparent. RmlUi draws a textarea through a single
         // ElementText, so one colour is all it can have — colour has to come
         // from a layer it does not own.
         //
         // `wrap='nowrap'` is structural, not cosmetic: a wrapped line in one
         // layer and not the other would desynchronise every line below it.
         "<div id='codeview' style='display:none'>"
         // Three siblings, not two layers plus a decoration. A dot drawn
         // inside `#codehl` would receive no clicks: the textarea is stacked
         // over it and the press lands there. The gutter sits beside both, and
         // both are shifted right by its width.
         "<div id='dbggutter'/>"
         "<div id='codehl'/><textarea id='fullcode' wrap='nowrap'/></div>"
         "<div id='canvasarea'>"
         << (asset_path("kiln-wordmark.png").empty() ? std::string() :
             "<img id='canvaswm' src='" + asset_path("kiln-wordmark.png") + "'/>")
         << ""
         "<div id='formwin'>"
         // Minimize and maximize are drawn; close is the multiplication
         // cross the loaded face has. The Windows caption glyphs proper are
         // in a private font nothing here can load.
         "<div id='formtitle'><img id='formicon'/><span id='formtitletext'>Form</span>"
         "<div class='wbtn' style='right:52px'><div class='mini'/></div>"
         "<div class='wbtn' style='right:30px'><div class='maxi'/></div>"
         "<div class='wbtn close' style='right:8px'>\xe2\x9c\x95</div></div>"
         "<div id='canvas'><div id='overlay'/></div></div>"
         // The tray. A timer, an action or a server has properties to edit and
         // no rectangle to click, so it needs a place that is not the canvas —
         // dropping one onto the form would write source the validator refuses.
         "<div id='tray'><div class='trayhead'>Non-visual components</div>"
         "<div id='traylist'/></div>"
         "</div></div>";

    s << "<div id='inspectdock'><div class='panelhead'>INSPECTOR</div>"
         "<div class='segbar'><div class='seg active' id='segprops' oe-tab='props'>Properties</div>"
         "<div class='seg' id='segevents' oe-tab='events'>Events</div></div>"
         "<div id='ctxlabel'>—</div><div id='grid'/><div id='wirebox'/></div>";

    const int half = centre_w / 2;
    s << "<div id='bottom'>"
         "<div class='pane' id='codepane' style='left:0;width:" << half
      << "px;border-right:1px " << BORDER << "'>"
         "<div class='panehead' id='codehead'>CODE PREVIEW</div>"
         "<div id='code' oe-view='code'/>"
         // The stopped program's stack and its variables, in the pane the
         // code preview otherwise has. They take it over rather than being
         // squeezed in beside it: while the program is stopped, what it holds
         // is what the user is reading, and a preview of source they are
         // already looking at is not.
         "<div class='panehead' id='varhead' style='display:none'>VARIABLES</div>"
         "<div id='vars' style='display:none'/></div>"
         "<div class='pane' id='logpane' style='left:" << half << "px;width:"
      << (centre_w - half) << "px'>"
         "<div class='panehead' id='problemcount'>PROBLEMS</div>"
         "<div id='problems'/>"
         // `wrap='nowrap'` for the same reason as the editor: a line wrapped
         // in one layer and not the other desynchronises every row below it.
         "<div class='panehead' id='loghead'>OUTPUT / BUILD LOG</div>"
         "<div id='logbox'><div id='loghl'/><textarea id='log' wrap='nowrap'/></div></div>"
         "</div>";

    // Splitters: thin draggable bars between the docks.
    s << "<div id='splitleft' class='split v'/>"
         "<div id='splitright' class='split v'/>"
         "<div id='splitbottom' class='split h'/>"
         "<div id='splitmid' class='split v'/>";

    s << "<div id='status'>Design it, wire it, run it  |  Native binaries, nothing to unpack"
         "<span class='right'><span id='statustext'>Ready</span>   "
         "<span class='dot'>●</span></span></div>";

    s << "</body></rml>";
    return s.str();
}

/* --- canvas --------------------------------------------------------------- */

/// Snap to the canvas dot grid, so dragged components line up the way the grid
/// implies they will. Holding nothing snaps; this is the behaviour users expect
/// from a designer that draws a grid.
/// The designer grid, and the distance Shift+arrow nudges by. A setting, and
/// clamped away from 0 by `settings::number` — this divides.
int grid_size() { return kiln::settings::number("designer.grid_size"); }
int snap(int v) {
    if (!kiln::settings::boolean("designer.snap_to_grid")) return v;
    const int g = grid_size();
    return ((v + g / 2) / g) * g;
}

int prop_int(const Component& c, const char* name, int fallback) {
    if (const std::string* v = c.property(name)) {
        return std::atoi(v->c_str());
    }
    return fallback;
}

/// Whether the designer may write `prop` on `c`. A type the catalogue does
/// not know declares nothing: writing blind is how a label got a `height`
/// the build then refused. A property the file already sets is left alone
/// either way — this guards what the designer adds, not what the user wrote.
bool can_write(const Component& c, const char* prop) {
    const CatalogComponent* cc = g.catalog.find(c.type_name);
    return cc && cc->declares(prop);
}

/// set_property behind the guard. False, and nothing written, when the
/// component does not declare the property.
bool write_prop(Component& c, const char* prop, const std::string& value) {
    if (!can_write(c, prop)) return false;
    c.set_property(prop, value);
    return true;
}
bool write_int(Component& c, const char* prop, int value) {
    return write_prop(c, prop, std::to_string(value));
}

/// The form grew by dw,dh: move and stretch every child the way its `anchors`
/// say, from the rectangles in `base` (or from what the model holds now). The
/// same rule the built program applies on a window resize — so what the
/// canvas shows after a resize is what the app will show after one.
void follow_form_resize(int dw, int dh, const std::map<std::string, std::array<int, 4>>* base) {
    if (!dw && !dh) return;
    for (auto& c : g.model.children) {
        const std::string* a = c.property("anchors");
        unsigned mask = 0;
        if (!a || !kiln::ui::parse_anchors(a->c_str(), &mask)) continue;
        if (!(mask & (kiln::ui::ANCHOR_RIGHT | kiln::ui::ANCHOR_BOTTOM))) continue;
        int l, t, w, h;
        if (base && base->count(c.id)) {
            const auto& r = base->at(c.id);
            l = r[0]; t = r[1]; w = r[2]; h = r[3];
        } else {
            l = prop_int(c, "left", 0); t = prop_int(c, "top", 0);
            w = prop_int(c, "width", 120); h = prop_int(c, "height", 32);
        }
        kiln::ui::anchored_rect(mask, dw, dh, &l, &t, &w, &h);
        if (c.property("left"))   write_int(c, "left", l);
        if (c.property("top"))    write_int(c, "top", t);
        if (c.property("width"))  write_int(c, "width", w);
        if (c.property("height")) write_int(c, "height", h);
    }
}

/// The rendered border-box rect of a component, in canvas coordinates.
/// Model width/height size the CONTENT box, so anything with padding or a
/// border draws larger; outlines must trace what is actually painted.
bool measure_component(Rml::Element* canvas, const std::string& id, int& x, int& y, int& w,
                       int& h) {
    if (!canvas) return false;
    Rml::Element* target = nullptr;
    for (int i = 0; i < canvas->GetNumChildren(); i++) {
        Rml::Element* child = canvas->GetChild(i);
        if (child->GetAttribute<Rml::String>("oe-id", "") == id) {
            target = child;
            break;
        }
    }
    if (!target) return false;
    g.context->Update();   // layout must be current for the measurement to mean anything
    const auto canvas_at = canvas->GetAbsoluteOffset(Rml::BoxArea::Border);
    const auto at = target->GetAbsoluteOffset(Rml::BoxArea::Border);
    const auto size = target->GetBox().GetSize(Rml::BoxArea::Border);
    x = (int)(at.x - canvas_at.x);
    y = (int)(at.y - canvas_at.y);
    w = (int)size.x;
    h = (int)size.y;
    return true;
}

bool is_hex_colour(const std::string& v);

/// Is this a colour light text belongs on? Perceived luminance of a hex
/// colour, since the title bar takes the form's own background: black text
/// on form.kiln's navy would be a title nobody could read.
bool dark_colour(const std::string& hex) {
    if (!is_hex_colour(hex)) return false;
    auto nib = [](char c) { return c <= '9' ? c - '0' : (c | 0x20) - 'a' + 10; };
    int r, gr, b;
    if (hex.size() == 4) {
        r = nib(hex[1]) * 17; gr = nib(hex[2]) * 17; b = nib(hex[3]) * 17;
    } else {
        r = nib(hex[1]) * 16 + nib(hex[2]);
        gr = nib(hex[3]) * 16 + nib(hex[4]);
        b = nib(hex[5]) * 16 + nib(hex[6]);
    }
    return (r * 299 + gr * 587 + b * 114) / 1000 < 128;
}

std::string asset_path(const char* name);

/// The image the preview's title bar shows: the form's `icon`, a path beside
/// the .kiln, or the app's own icon when the property is unset or names
/// nothing readable. Handed to RmlUi the way asset_path does, with the
/// leading slash doubled so its URL parser leaves the path absolute.
std::string form_icon_src() {
    if (const std::string* icon = g.model.form.property("icon")) {
        if (!icon->empty()) {
            std::string path = *icon;
            if (!kiln::sys::is_absolute(path)) {
                const size_t slash = g.model.path.find_last_of('/');
                path = (slash == std::string::npos ? std::string() : g.model.path.substr(0, slash + 1)) + path;
            }
            if (kiln::sys::readable(path)) {
                const std::string real = kiln::sys::real_path(path);
                if (!real.empty()) return "/" + real;
            }
        }
    }
    return asset_path("kiln-icon-64.png");
}

/* The offscreen driver applies SDL_SetWindowSize and tells nobody: no
 * SIZE_CHANGED event reaches the backend, so its GL viewport keeps the old
 * size and everything past it is painted black. A real window manager sends
 * the event; a scripted resize sends it itself. */
static void announce_window_size(SDL_Window* win, int w, int h) {
    SDL_Event ev;
    SDL_zero(ev);
    ev.type = SDL_WINDOWEVENT;
    ev.window.event = SDL_WINDOWEVENT_SIZE_CHANGED;
    ev.window.windowID = SDL_GetWindowID(win);
    ev.window.data1 = w;
    ev.window.data2 = h;
    SDL_PushEvent(&ev);
}

void rebuild_canvas() {
    using theme::SEL_GAP; using theme::HANDLE_PX; using theme::BADGE_H; using theme::BADGE_GAP;
    Rml::Element* formwin = by_id("formwin");
    Rml::Element* canvas = by_id("canvas");
    Rml::Element* overlay = by_id("overlay");
    if (!canvas || !formwin || !overlay) return;

    const int fw = prop_int(g.model.form, "width", 420);
    const int fh = prop_int(g.model.form, "height", 260);
    formwin->SetProperty("width", Rml::String(std::to_string(fw) + "px"));
    canvas->SetProperty("width", Rml::String(std::to_string(fw) + "px"));
    canvas->SetProperty("height", Rml::String(std::to_string(fh) + "px"));
    // The title bar is the form's colour too — reading only, so a form that
    // sets no colour is drawn white and stays a form that sets no colour.
    const std::string* bg = g.model.form.property("background_color");
    // The ground a built form actually paints when it declares no colour
    // (the ui library's form default). White here would preview a window
    // the program never renders.
    const std::string form_bg = bg && is_hex_colour(*bg) ? *bg : "#f3f3f3";
    canvas->SetProperty("background-color", form_bg);
    if (Rml::Element* t = by_id("formtitle")) {
        const bool dark = dark_colour(form_bg);
        const std::string ink = dark ? std::string("#ffffff") : theme::TEXT;
        t->SetProperty("background-color", form_bg);
        t->SetProperty("color", ink);
        // The rule under the title bar parts it from the client area; on a
        // dark form it is a lighter line, on a light one a darker.
        t->SetProperty("border-bottom-color", dark ? "#ffffff2e" : "#e5e7eb");
        // The drawn caption glyphs carry no text to inherit the colour.
        Rml::ElementList glyphs;
        t->GetElementsByClassName(glyphs, "mini");
        for (Rml::Element* e : glyphs) e->SetProperty("background-color", ink);
        glyphs.clear();
        t->GetElementsByClassName(glyphs, "maxi");
        for (Rml::Element* e : glyphs) e->SetProperty("border-color", ink);
    }
    if (Rml::Element* t = by_id("formtitletext")) {
        const std::string* title = g.model.form.property("title");
        t->SetInnerRML(esc(title ? *title : g.model.form_name));
    }
    if (Rml::Element* i = by_id("formicon")) {
        const std::string src = form_icon_src();
        if (src != g.form_icon_src) {
            g.form_icon_src = src;
            i->SetAttribute("src", Rml::String(src));
        }
    }

    // Clear components but keep the overlay element itself.
    for (int i = canvas->GetNumChildren() - 1; i >= 0; i--) {
        Rml::Element* child = canvas->GetChild(i);
        if (child != overlay) canvas->RemoveChild(child);
    }
    while (overlay->GetNumChildren() > 0) overlay->RemoveChild(overlay->GetChild(0));

    // An editbox with `multiline = true` previews as a text area, exactly as
    // the built app renders it; off, it is a single-line input.
    auto canvas_tag = [](const Component& c) -> std::string {
        if (c.type_name == "editbox") {
            const std::string* m = c.property("multiline");
            if (m && (*m == "true" || *m == "1")) return "textarea";
        }
        return kiln::ui::tag_for(c.type_name.c_str());
    };
    for (const auto& comp : g.model.children) {
        Rml::ElementPtr child = g.doc->CreateElement(canvas_tag(comp).c_str());
        const char* attr_value = nullptr;
        if (const char* attr =
                kiln::ui::creation_attribute(comp.type_name.c_str(), &attr_value)) {
            child->SetAttribute(attr, Rml::String(attr_value));
        }
        Rml::Element* e = canvas->AppendChild(std::move(child));
        e->SetProperty("position", "absolute");
        // The class comes from the shared mapping, not from a list here: a
        // control styled in the running app and not on the canvas is exactly
        // the WYSIWYG drift ui_mapping.h exists to prevent.
        if (const char* cls = kiln::ui::class_for(comp.type_name.c_str()))
            e->SetAttribute("class", Rml::String(cls));
        if (const char* markup = kiln::ui::inner_markup(comp.type_name.c_str())) {
            e->SetInnerRML(markup);
        }
        if (comp.type_name == "progressbar") e->SetAttribute("max", Rml::String("100"));
        e->SetAttribute("oe-id", comp.id);
        for (const auto& p : comp.properties) {
            const char* attr =
                kiln::ui::attribute_for(comp.type_name.c_str(), p.first.c_str());
            if (attr) {
                if (p.first == "checked") {
                    Rml::Element* box = e;
                    for (int i = 0; i < e->GetNumChildren(); i++) {
                        if (e->GetChild(i)->GetTagName() == "input") { box = e->GetChild(i); break; }
                    }
                    if (p.second == "true" || p.second == "1") {
                        box->SetAttribute(attr, Rml::String("checked"));
                    }
                } else {
                    e->SetAttribute(attr, Rml::String(p.second));
                }
            } else if (kiln::ui::is_text_property(p.first.c_str()) &&
                       kiln::ui::text_is_content(comp.type_name.c_str())) {
                Rml::Element* text_target = e;
                if (kiln::ui::is_composite(comp.type_name.c_str())) {
                    for (int i = 0; i < e->GetNumChildren(); i++) {
                        if (e->GetChild(i)->GetTagName() == "span") {
                            text_target = e->GetChild(i);
                            break;
                        }
                    }
                }
                text_target->SetInnerRML(esc(p.second));
            } else if (p.first == "enabled") {
                // Not RCSS: the running app dims a disabled control, and the
                // canvas shows the same so a form reads as it will run.
                const bool on = p.second == "true" || p.second == "1";
                e->SetProperty("opacity", on ? "1.0" : "0.4");
            } else if (!kiln::ui::is_control_value(comp.type_name.c_str(),
                                                      p.first.c_str())) {
                e->SetProperty(kiln::ui::rcss_name(p.first.c_str()),
                               kiln::ui::rcss_value(p.first.c_str(), p.second.c_str()));
            }
        }
        if (comp.type_name == "grid") {
            // Real rows at design time, through the same markup the running
            // app uses, so the grid on the canvas is the grid in the window.
            // A grid bound to a datasource shows the source's rows, as it will
            // when it runs; a source the file does not declare — filled from
            // code, or from a kit — leaves a header naming it, which says
            // what will fill the box rather than showing a broken control.
            const std::string* cols = comp.property("columns");
            const std::string* rows = comp.property("rows");
            const std::string* bind = comp.property("bind");
            std::string head = cols ? *cols : std::string();
            std::string body = rows ? *rows : std::string();
            if (bind && !bind->empty()) {
                bool found = false;
                for (const auto& m : g.model.module_components) {
                    const std::string* mname = m.property("name");
                    if (m.removed || m.type_name != "datasource") continue;
                    if (m.id != *bind && !(mname && *mname == *bind)) continue;
                    if (const std::string* c = m.property("columns")) head = *c;
                    if (const std::string* r = m.property("rows")) body = *r;
                    found = true;
                }
                if (!found && head.empty() && body.empty()) head = "bound to " + *bind;
            }
            e->SetInnerRML(kiln::ui::grid_markup(head, body, prop_int(comp, "selected", 0)));
        }
    }

    // Form resize grips, on the edges of the preview window itself.
    {
        const int fwid = fw, fhei = fh;
        auto grip = [&](const char* edge, int px, int py, int pw, int ph, const char* cls) {
            Rml::Element* d = overlay->AppendChild(g.doc->CreateElement("div"));
            d->SetProperty("position", "absolute");
            d->SetAttribute("class", Rml::String(cls));
            d->SetAttribute("oe-formgrip", Rml::String(edge));
            d->SetProperty("left", Rml::String(std::to_string(px) + "px"));
            d->SetProperty("top", Rml::String(std::to_string(py) + "px"));
            d->SetProperty("width", Rml::String(std::to_string(pw) + "px"));
            d->SetProperty("height", Rml::String(std::to_string(ph) + "px"));
        };
        grip("e",  fwid - 4, 0, 8, fhei - 4, "fgrip e");
        grip("s",  0, fhei - 4, fwid - 4, 8, "fgrip s");
        grip("se", fwid - 7, fhei - 7, 12, 12, "fgrip corner");
    }

    // Alignment guides, drawn only while dragging.
    for (int gx : g.guide_x) {
        Rml::Element* d = overlay->AppendChild(g.doc->CreateElement("div"));
        d->SetProperty("position", "absolute");
        d->SetAttribute("class", Rml::String("guide v"));
        d->SetProperty("left", Rml::String(std::to_string(gx) + "px"));
        d->SetProperty("top", "0px");
        d->SetProperty("width", "1px");
        d->SetProperty("height", "100%");
    }
    for (int gy : g.guide_y) {
        Rml::Element* d = overlay->AppendChild(g.doc->CreateElement("div"));
        d->SetProperty("position", "absolute");
        d->SetAttribute("class", Rml::String("guide h"));
        d->SetProperty("left", "0px");
        d->SetProperty("top", Rml::String(std::to_string(gy) + "px"));
        d->SetProperty("width", "100%");
        d->SetProperty("height", "1px");
    }

    // Secondary selections get a plain outline; the primary one gets handles.
    for (const auto& id : g.selection) {
        if (id == g.selected) continue;
        int sx = 0, sy = 0, sw = 0, sh = 0;
        if (!measure_component(canvas, id, sx, sy, sw, sh)) continue;
        Rml::Element* d = overlay->AppendChild(g.doc->CreateElement("div"));
        d->SetProperty("position", "absolute");
        d->SetAttribute("class", Rml::String("selbox alt"));
        d->SetProperty("left", Rml::String(std::to_string(sx - SEL_GAP) + "px"));
        d->SetProperty("top", Rml::String(std::to_string(sy - SEL_GAP) + "px"));
        d->SetProperty("width", Rml::String(std::to_string(sw + 2 * SEL_GAP) + "px"));
        d->SetProperty("height", Rml::String(std::to_string(sh + 2 * SEL_GAP) + "px"));
    }

    // The form itself, selected: its frame takes the accent, and a box just
    // inside the client area — the canvas clips what it holds, so one drawn
    // around it would not be seen. No anchors: the form is resized by its own
    // grips, and it has no position on the canvas to drag.
    const bool form_selected = !g.selected.empty() && g.selected == g.model.form_name;
    formwin->SetProperty("border-color", form_selected ? theme::SELECT : theme::BORDER);
    if (form_selected) {
        Rml::Element* d = overlay->AppendChild(g.doc->CreateElement("div"));
        d->SetId("formsel");
        d->SetProperty("position", "absolute");
        d->SetAttribute("class", Rml::String("selbox"));
        d->SetProperty("left", "0px");
        d->SetProperty("top", "0px");
        d->SetProperty("width", Rml::String(std::to_string(fw) + "px"));
        d->SetProperty("height", Rml::String(std::to_string(fh) + "px"));
    }

    // Selection chrome lives in an overlay so it never perturbs the components
    // themselves — what you see on the canvas is exactly what the app renders.
    if (!form_selected && g.model.find(g.selected)) {
        // Measure the RENDERED element rather than deriving the rect from the
        // model's width/height: those size the CONTENT box, so a component with
        // padding or a border (a groupbox has both) draws larger than its
        // declared size, and a model-derived outline sits inside the real frame.
        int x = 0, y = 0, w = 0, h = 0;
        if (!measure_component(canvas, g.selected, x, y, w, h)) return;
        if (std::getenv("KILN_DESIGNER_DEBUG")) {
            const Component* c = g.model.find(g.selected);
            std::fprintf(stderr,
                         "designer: selection rect=%d,%d %dx%d   model=%d,%d %dx%d\n", x, y, w, h,
                         c ? prop_int(*c, "left", 0) : -1, c ? prop_int(*c, "top", 0) : -1,
                         c ? prop_int(*c, "width", 0) : -1, c ? prop_int(*c, "height", 0) : -1);
        }
        const int ox = 0, oy = 0;
        auto place = [&](const char* cls, int px, int py, int pw, int ph) {
            Rml::Element* d = overlay->AppendChild(g.doc->CreateElement("div"));
            d->SetProperty("position", "absolute");
            d->SetAttribute("class", Rml::String(cls));
            d->SetProperty("left", Rml::String(std::to_string(px) + "px"));
            d->SetProperty("top", Rml::String(std::to_string(py) + "px"));
            if (pw) d->SetProperty("width", Rml::String(std::to_string(pw) + "px"));
            if (ph) d->SetProperty("height", Rml::String(std::to_string(ph) + "px"));
            return d;
        };
        // The box sits SEL_GAP outside the component; each anchor is centred
        // on the box's edge.
        place("selbox", ox + x - SEL_GAP, oy + y - SEL_GAP, w + 2 * SEL_GAP, h + 2 * SEL_GAP);
        const int hs = HANDLE_PX / 2;
        const int hx[3] = {ox + x - SEL_GAP - hs, ox + x + w / 2 - hs, ox + x + w + SEL_GAP - hs};
        const int hy[3] = {oy + y - SEL_GAP - hs, oy + y + h / 2 - hs, oy + y + h + SEL_GAP - hs};
        static const char* EDGE[3][3] = {{"nw", "w", "sw"}, {"n", "", "s"}, {"ne", "e", "se"}};
        // Only the anchors whose drag can be written. A handle that resizes
        // vertically on a component with no `height` offers a gesture whose
        // result the compiler refuses; the column i / row j of the table say
        // which properties the anchor moves.
        const Component* sel_comp = g.model.find(g.selected);
        const bool can_w = sel_comp && can_write(*sel_comp, "width");
        // A single-line editbox is clamped to one row: its height is fixed, so
        // the vertical anchors are withheld until `multiline` is turned on.
        bool editbox_single = false;
        if (sel_comp && sel_comp->type_name == "editbox") {
            const std::string* m = sel_comp->property("multiline");
            editbox_single = !(m && (*m == "true" || *m == "1"));
        }
        const bool can_h = sel_comp && can_write(*sel_comp, "height") && !editbox_single;
        const bool can_l = sel_comp && can_write(*sel_comp, "left");
        const bool can_t = sel_comp && can_write(*sel_comp, "top");
        for (int i = 0; i < 3; i++) {
            for (int j = 0; j < 3; j++) {
                if (i == 1 && j == 1) continue;   // 8 anchors, not 9
                if (i != 1 && (!can_w || (i == 0 && !can_l))) continue;
                if (j != 1 && (!can_h || (j == 0 && !can_t))) continue;
                Rml::Element* d = place("handle", hx[i], hy[j], 0, 0);
                d->SetAttribute("class", Rml::String(std::string("handle ") + EDGE[i][j]));
                d->SetAttribute("oe-grip", Rml::String(EDGE[i][j]));
            }
        }
    }

    // The wiring, on the canvas: every component with a handler carries a
    // badge naming the event and the subroutine, and the badge is a way into
    // that subroutine. Centred above the component, or below it when the
    // component sits too close to the top — the client area clips what it
    // holds, and a badge clipped to nothing is wiring the user cannot see.
    for (const auto& comp : g.model.children) {
        const std::pair<std::string, std::string>* hnd = nullptr;
        for (const auto& h : comp.handlers) {
            if (!h.second.empty()) { hnd = &h; break; }
        }
        if (!hnd) continue;
        int x = 0, y = 0, w = 0, h = 0;
        if (!measure_component(canvas, comp.id, x, y, w, h)) continue;
        Rml::Element* b = overlay->AppendChild(g.doc->CreateElement("div"));
        b->SetProperty("position", "absolute");
        b->SetAttribute("class", Rml::String(comp.id == g.selected ? "badge live" : "badge"));
        b->SetAttribute("oe-jump", comp.id);
        b->SetAttribute("oe-event", hnd->first);
        const int above = y - SEL_GAP - BADGE_GAP - BADGE_H;
        b->SetProperty("left", Rml::String(std::to_string(x + w / 2) + "px"));
        b->SetProperty("top", Rml::String(std::to_string(above >= 0 ? above : y + h + SEL_GAP + BADGE_GAP) + "px"));
        b->SetInnerRML("<div class='chain'><div class='a'/><div class='b'/></div><span class='ev'>" +
                       esc(hnd->first) + "</span><span class='arrow'>\xe2\x86\x92</span>" +
                       esc(hnd->second));
    }
}

/* --- the tray ------------------------------------------------------------- */

/// Draw the module-level components. Selecting one is what puts it in the
/// inspector, so this is the whole of their user interface.
void rebuild_tray() {
    Rml::Element* list = by_id("traylist");
    if (!list) return;
    std::string html;
    for (const auto& c : g.model.module_components) {
        if (c.removed) continue;
        html += "<div class='trayitem" + std::string(c.id == g.selected ? " sel" : "") +
                "' oe-id='" + esc(c.id) + "'>"
                "<div class='trayico'>\xe2\x97\x87</div>"
                "<div class='traylabel'>" + esc(c.id) + "</div>"
                "<div class='traytype'>" + esc(c.type_name) + "</div></div>";
    }
    if (html.empty()) {
        html = "<div class='trayhint'>Empty. A timer, an action or a server has no rectangle, "
               "so it is declared beside the form rather than inside it — drop one from the "
               "toolbox's System section, or double-click one here to write its handler.</div>";
    }
    list->SetInnerRML(html);
}

/* --- inspector ------------------------------------------------------------ */

/// Is this a colour RmlUi will actually paint? Only `#rgb`, `#rrggbb` and
/// `#rrggbbaa` are: handing it anything else paints nothing, and a swatch that
/// silently shows the panel behind it reads as "no colour set" when the
/// property may well hold a typo.
bool is_hex_colour(const std::string& v) {
    if (v.empty() || v[0] != '#') return false;
    if (v.size() != 4 && v.size() != 7 && v.size() != 9) return false;
    for (size_t i = 1; i < v.size(); i++) {
        if (!isxdigit((unsigned char)v[i])) return false;
    }
    return true;
}

std::string swatch_style(const std::string& value) {
    if (is_hex_colour(value)) return "background-color:" + value;
    return "background-color:#00000000;border:1px dashed " + std::string(theme::BORDER);
}

/// The colour picker's palette: greys along the top, then a hue ramp in three
/// tints. Fixed rather than derived — a palette is a design decision, and one
/// computed from a colour wheel gives forty colours nobody would choose.
inline const std::vector<const char*>& palette() {
    static const std::vector<const char*> p = {
        "#000000", "#1f2328", "#3d444d", "#656d76", "#8c959f", "#b1bac4", "#d0d7de", "#ffffff",
        "#8b0000", "#cf222e", "#fa4549", "#ff8182", "#7a2e00", "#bc4c00", "#fb8f44", "#ffb77c",
        "#7a4f01", "#bf8700", "#eac54f", "#f8e3a1", "#0a5a2b", "#1a7f37", "#2da44e", "#6fdd8b",
        "#023a5c", "#0969da", "#1e60d5", "#54aeff", "#3c1e70", "#6f42c1", "#a475f9", "#c297ff",
        "#5e103e", "#bf3989", "#ec6cb9", "#ffadda", "#1e2233", "#24292f", "#f6f8fa", "#fafafa",
    };
    return p;
}

/// Files under the project's own directory, one level deep, as relative paths.
///
/// Relative because that is what the .kiln must contain: an absolute path from
/// this machine is a project that only builds here.
std::vector<std::string> project_files() {
    std::vector<std::string> out;
    const size_t slash = g.model.path.find_last_of('/');
    const std::string root = slash == std::string::npos ? "." : g.model.path.substr(0, slash);

    // One level down, not a whole tree walk: assets sit in `assets/` beside the
    // project, and a recursive scan of somebody's home directory is a hang.
    std::vector<std::pair<std::string, std::string>> dirs = {{root, ""}};
    for (size_t d = 0; d < dirs.size(); d++) {
        const std::string dir = dirs[d].first, prefix = dirs[d].second;
        DIR* handle = opendir(dir.c_str());
        if (!handle) continue;
        while (struct dirent* e = readdir(handle)) {
            const std::string name = e->d_name;
            if (name.empty() || name[0] == '.') continue;
            struct stat st;
            if (::stat((dir + "/" + name).c_str(), &st) != 0) continue;
            if (S_ISDIR(st.st_mode)) {
                if (prefix.empty()) dirs.push_back({dir + "/" + name, name + "/"});
                continue;
            }
            if (kiln::sys::has_ext(name, ".kiln")) continue;
            out.push_back(prefix + name);
        }
        closedir(handle);
    }
    std::sort(out.begin(), out.end());
    return out;
}

/// Show an editor popup beside `anchor`, to the LEFT of it: the inspector is
/// against the right edge of the window, so a popup opened to the right would
/// hang off the screen.
void open_editpop(Rml::Element* anchor, const std::string& html) {
    Rml::Element* pop = by_id("editpop");
    if (!pop || !anchor) return;
    pop->SetInnerRML(html);
    const auto at = anchor->GetAbsoluteOffset(Rml::BoxArea::Border);
    int x = (int)at.x - 260;
    if (x < 8) x = 8;
    int y = (int)at.y;
    if (y > g.win_h - 240) y = g.win_h - 240;
    if (y < 8) y = 8;
    pop->SetProperty("left", Rml::String(std::to_string(x) + "px"));
    pop->SetProperty("top", Rml::String(std::to_string(y) + "px"));
    pop->SetProperty("display", "block");
}

void close_editpop() {
    if (Rml::Element* pop = by_id("editpop")) pop->SetProperty("display", "none");
    g.editing_id.clear();
    g.editing_prop.clear();
}

/// Apply a value the popup chose, exactly as typing it into the field would.
void set_edited_property(const std::string& value) {
    Component* c = g.model.find(g.editing_id);
    if (!c || g.editing_prop.empty()) { close_editpop(); return; }
    push_undo();
    c->set_property(g.editing_prop, value);
    mark_dirty();
    close_editpop();
    refresh_all();
}

/// The sides an `anchors` value names: `left,top,right`, in any order, with
/// or without spaces.
std::vector<std::string> anchor_sides(const std::string& value) {
    std::vector<std::string> out;
    std::string cur;
    for (char c : value + ",") {
        if (c == ',') {
            if (!cur.empty()) out.push_back(cur);
            cur.clear();
        } else if (c != ' ') {
            cur += c;
        }
    }
    return out;
}
bool has_anchor(const std::string& value, const std::string& side) {
    for (const auto& s : anchor_sides(value)) {
        if (s == side) return true;
    }
    return false;
}
/// `value` with `side` added or removed, written in the order the sides are
/// read — left, top, right, bottom — so two ways of clicking the same set
/// give the same text.
std::string toggle_anchor(const std::string& value, const std::string& side) {
    std::vector<std::string> sides = anchor_sides(value);
    if (has_anchor(value, side)) sides.erase(std::remove(sides.begin(), sides.end(), side), sides.end());
    else sides.push_back(side);
    std::string out;
    for (const char* s : {"left", "top", "right", "bottom"}) {
        for (const auto& have : sides) {
            if (have == s) out += std::string(out.empty() ? "" : ",") + s;
        }
    }
    return out;
}

void rebuild_inspector() {
    Rml::Element* ctx = by_id("ctxlabel");
    Rml::Element* grid = by_id("grid");
    Rml::Element* wire = by_id("wirebox");
    if (!ctx || !grid || !wire) return;

    Component* comp = g.model.find(g.selected);
    if (!comp) {
        ctx->SetInnerRML("—");
        grid->SetInnerRML("<div class='hint'>Select a component on the canvas.</div>");
        wire->SetInnerRML("<div class='h'>HANDLER WIRING</div>"
                          "<div class='unlinked'>Nothing selected.</div>");
        return;
    }
    ctx->SetInnerRML(esc(comp->id) + " <span style='color:#656d76;font-weight:normal'>(" +
                     esc(comp->type_name) + ")</span>");

    // The catalogue, so a component from a kit this build never linked still
    // gets an inspector rather than "unknown type".
    const CatalogComponent* cc = g.catalog.find(comp->type_name);
    if (!cc) { grid->SetInnerRML("<div class='hint'>unknown type</div>"); return; }
    const std::vector<CatalogProp>& props = cc->props;
    const std::vector<CatalogEvent>& events = cc->events;

    std::string html;
    if (g.inspector_tab == "props") {
        // The instance id, flagged as compile-time-only (it never ships — G8).
        html += "<div class='prow'><label>Name</label>"
                "<input type='text' class='cid' value='" + esc(comp->id) + "'/>"
                "<div class='note'>internal only — does not ship to the binary</div></div>";
        for (const auto& p : props) {
            const std::string* v = comp->property(p.name);
            // Show what the file actually sets, not the descriptor default:
            // an unset property is not applied at run time, and the canvas
            // renders it unset, so displaying the default here would make the
            // inspector disagree with both.
            const std::string val = v ? *v : std::string();
            html += "<div class='prow'><label>" + esc(p.name) +
                    (v ? "" : " <span style='color:#9aa3b0'>(unset" +
                                  (p.has_default ? ", default " + esc(p.default_value) : "") +
                                  ")</span>") +
                    "</label>";
            if (p.editor == "color") {
                // A hex field cannot be read at a glance, which is the whole
                // complaint the `editor` hint answers.
                html += "<div class='swatch' oe-swatch='" + esc(p.name) + "' style='" +
                        swatch_style(val) + "'/>"
                        "<input type='text' class='pv withswatch' name='" + esc(p.name) +
                        "' value='" + esc(val) + "'/>";
            } else if (p.editor == "file") {
                html += "<input type='text' class='pv withbtn' name='" + esc(p.name) +
                        "' value='" + esc(val) + "'/>"
                        "<div class='browse' oe-file='" + esc(p.name) + "'>Browse…</div>";
            } else if (p.editor == "multiline") {
                // The value is set through the control below, not written into
                // the markup: a paragraph with a newline in it is not an
                // attribute value.
                html += "<textarea class='pvm' name='" + esc(p.name) + "'/>";
            } else if (p.editor == "anchors") {
                // Four boxes, one a side, lit when the value names that side;
                // the field stays beside them so the value can still be typed.
                static const char* SIDES[4] = {"left", "top", "right", "bottom"};
                static const char* LETTERS[4] = {"L", "T", "R", "B"};
                std::string boxes;
                for (int i = 0; i < 4; i++) {
                    boxes += std::string("<div class='anch") +
                             (has_anchor(val, SIDES[i]) ? " on" : "") + "' id='anch-" + SIDES[i] +
                             "' oe-anchor='" + SIDES[i] + "' oe-prop='" + esc(p.name) + "'>" +
                             LETTERS[i] + "</div>";
                }
                html += "<div class='anchbox'>" + boxes + "</div>"
                        "<input type='text' class='pv withanch' name='" + esc(p.name) +
                        "' value='" + esc(val) + "'/>";
            } else {
                html += "<input type='text' class='pv' name='" + esc(p.name) + "' value='" +
                        esc(val) + "'/>";
            }
            html += "</div>";
        }
    } else {
        if (events.empty()) html += "<div class='hint'>This component has no events.</div>";
        for (const auto& ev : events) {
            const std::string* h = comp->handler(ev.name);
            const bool wired = h && !h->empty();
            // The field takes a name typed in; the link beside it is the
            // double-click, for this event: it goes to the handler, or
            // writes one first.
            html += "<div class='prow'><label><span class='evname'>on " + esc(ev.name) +
                    "</span><span class='evgo' id='ev-" + esc(ev.name) + "' oe-jump='" +
                    esc(comp->id) + "' oe-event='" + esc(ev.name) + "'>" +
                    (wired ? "open \xe2\x86\x97" : "+ create handler") + "</span></label>"
                    "<input type='text' class='ev' name='" + esc(ev.name) + "' value='" +
                    esc(h ? *h : "") + "'/></div>";
        }
    }
    grid->SetInnerRML(html);

    // A textarea holds its text as a value, not as markup, so it can only be
    // filled once the control exists.
    for (int i = 0; i < grid->GetNumChildren(); i++) {
        Rml::Element* row = grid->GetChild(i);
        for (int j = 0; j < row->GetNumChildren(); j++) {
            auto* box = dynamic_cast<Rml::ElementFormControl*>(row->GetChild(j));
            if (!box) continue;
            if (row->GetChild(j)->GetAttribute<Rml::String>("class", "").find("pvm") ==
                Rml::String::npos) {
                continue;
            }
            const std::string* v =
                comp->property(row->GetChild(j)->GetAttribute<Rml::String>("name", ""));
            box->SetValue(v ? *v : "");
        }
    }

    std::string wired;
    for (const auto& h : comp->handlers) {
        if (h.second.empty()) continue;
        // The first link keeps a fixed id, so a script can click it; the
        // event carried on it is what makes the jump land in the right sub
        // when a component has several.
        wired += "<div class='row'>Linked to: <span class='link'" +
                 std::string(wired.empty() ? " id='wirelink'" : "") + " oe-jump='" +
                 esc(comp->id) + "' oe-event='" + esc(h.first) + "'>" + esc(h.second) +
                 "()</span><span style='color:" + theme::TEXT_MUTED + ";margin-left:6px'>on " + esc(h.first) +
                 "</span></div>";
    }
    wire->SetInnerRML("<div class='h'>HANDLER WIRING</div>" +
                      (wired.empty()
                           ? std::string("<div class='unlinked' id='wirelink'>Not linked \xe2\x80\x94 "
                                         "double-click to create</div>")
                           : wired));
}

/* --- code pane ------------------------------------------------------------ */

/// Show the source of the subroutine wired to the selection, or the whole file
/// when nothing is selected.
void rebuild_code() {
    Rml::Element* code = by_id("code");
    if (!code) return;
    std::ifstream f(g.model.path);
    std::vector<std::string> lines;
    std::string l;
    while (std::getline(f, l)) lines.push_back(l);
    if (!g.code_dirty) {
        g.model_text.clear();
        for (const auto& line : lines) g.model_text += line + "\n";
    }

    std::string want;
    if (const Component* c = g.model.find(g.selected)) {
        if (!c->handlers.empty()) want = c->handlers.front().second;
    }

    size_t from = 0, to = lines.size();
    if (!want.empty()) {
        for (size_t i = 0; i < lines.size(); i++) {
            if (lines[i].rfind("sub " + want, 0) == 0) {
                from = i;
                for (size_t j = i; j < lines.size(); j++) {
                    if (lines[j] == "end") { to = j + 1; break; }
                }
                break;
            }
        }
    }

    std::string html;
    for (size_t i = from; i < to && i < lines.size(); i++) {
        char num[16];
        std::snprintf(num, sizeof num, "%4zu", i + 1);
        html += "<div class='cl'><span class='ln'>" + std::string(num) + "</span>" +
                highlight_line(lines[i]) + "</div>";
    }
    if (Rml::Element* head = code->GetParentNode()->GetChild(0)) {
        // Named a preview because it is one: it is syntax-highlighted and
        // read-only, and clicking it opens the editable Code tab. Calling it
        // an editor while it refuses to take a keystroke is the mismatch.
        head->SetInnerRML((want.empty() ? "CODE PREVIEW · whole module"
                                        : "CODE PREVIEW · " + esc(want)) +
                          std::string("  ·  CLICK TO EDIT"));
    }
    code->SetInnerRML(html);

    // The Code view is an editable textarea holding the whole module.
    //
    // Never overwrite it while the user has unsaved edits: a refresh triggered
    // by anything else (a selection change, a canvas drag) would silently throw
    // away what they were typing.
    if (!g.code_dirty) {
        if (auto* full = code_editor()) full->SetValue(g.model_text);
        // The server hears about typing from the editor's change event, and
        // about nothing else. A designer save rewrites the file underneath it,
        // and a hover answered against the text before the save names the
        // wrong line.
        g.lsp.did_change(g.model_text);
        g.symbols_stale = true;
    }
    refresh_highlight();
    update_tabs();
}

/// The code editor, or null before the chrome exists.
Rml::ElementFormControl* code_editor() {
    return dynamic_cast<Rml::ElementFormControl*>(by_id("fullcode"));
}

/// Repaint the syntax-highlight layer from the editor's LIVE text.
///
/// From the control, never from disk: the file lags behind by every unsaved
/// keystroke, and colour that is one save out of date is worse than none.
void refresh_highlight() {
    Rml::Element* layer = by_id("codehl");
    auto* ed = code_editor();
    if (!layer || !ed) return;

    const std::string text = ed->GetValue();

    // Split once; the slice we draw changes far more often than the text does.
    std::vector<std::string> lines;
    size_t start = 0;
    while (start <= text.size()) {
        const size_t nl = text.find('\n', start);
        lines.push_back(text.substr(start, nl == std::string::npos ? std::string::npos
                                                                   : nl - start));
        if (nl == std::string::npos) break;
        start = nl + 1;
    }
    g.code_content_h = (int)lines.size() * theme::CODE_LINE_H;

    // Draw ONLY the lines in view, starting at the top of the viewport.
    //
    // The obvious approach — lay out the whole document and slide it up by a
    // negative offset — does not work here: RmlUi does not clip an absolutely
    // positioned child against an ancestor's overflow, so a scrolled editor
    // painted its code straight over the menu bar and toolbar. Nothing is ever
    // positioned outside the view now, so there is nothing to clip.
    Rml::Element* view = by_id("codeview");
    const int viewport = view ? (int)view->GetBox().GetSize().y : 400;
    const size_t rows = (size_t)(viewport / theme::CODE_LINE_H) + 1;
    const size_t first_line = (size_t)(g.code_scroll / theme::CODE_LINE_H);

    // The row count belongs in the cache key. The first refresh can run before
    // the view has been laid out, when its height is zero and only one row
    // "fits"; keyed on text and scroll alone, that one-line render is what the
    // cache then preserves forever. So do the diagnostics: they are drawn
    // into these rows, and a new set with the same text must repaint.
    std::string marks;
    for (const auto& d : g.diagnostics) {
        marks += std::to_string(d.range.line) + ":" + std::to_string(d.range.character) + "-" +
                 std::to_string(d.range.end_line) + ":" + std::to_string(d.range.end_character) +
                 (d.severity == 1 ? "e;" : "w;");
    }
    // And the debugger's marks. Both change with the text untouched — a
    // breakpoint is set by clicking, and the stopped line moves on every step
    // — and the early return below is keyed on this string. Left out, clicking
    // the gutter would do nothing visible.
    marks += "|bp";
    for (int line : g.breakpoints) marks += std::to_string(line) + ",";
    marks += "|st" + std::to_string(g.stopped_line);
    // Sideways offset, read from the control every time. The control scrolls
    // itself when the caret walks past the right edge and tells nobody, so this
    // is the only moment it can be noticed — and it belongs in the cache key
    // below, or a line scrolled sideways with no text change never repaints.
    //
    // The layer itself is never moved to follow it. Moving it is what the
    // vertical case deliberately avoids: RmlUi does not clip a positioned
    // element against an ancestor's overflow, so a layer shifted out of the
    // view goes on painting over the toolbox and the menu bar. Each row clips
    // its own content instead — see the shift span below.
    g.code_scroll_x = (int)ed->GetScrollLeft();

    static std::string painted;
    static size_t painted_first = (size_t)-1;
    static size_t painted_rows = 0;
    static std::string painted_marks;
    static int painted_x = -1;
    if (text == painted && first_line == painted_first && rows == painted_rows &&
        marks == painted_marks && g.code_scroll_x == painted_x) {
        return;
    }
    painted = text;
    painted_first = first_line;
    painted_rows = rows;
    painted_marks = marks;
    painted_x = g.code_scroll_x;

    const int cw = code_char_width();
    std::string html;
    for (size_t i = first_line; i < lines.size() && i < first_line + rows; i++) {
        std::string bars;
        bool bad = false;
        for (const auto& d : g.diagnostics) {
            if ((int)i < d.range.line || (int)i > d.range.end_line) continue;
            bad = true;
            // The part of the range that falls on this row. A range with no
            // width still gets a mark two columns wide: the position is the
            // information, and nothing is not a mark.
            const int from = (int)i == d.range.line ? d.range.character : 0;
            int to = (int)i == d.range.end_line ? d.range.end_character : (int)lines[i].size();
            if (to <= from) to = from + 2;
            bars += "<div class='dline" + std::string(d.severity == 1 ? "" : " warn") +
                    "' style='left:" + std::to_string(from * cw) + "px;top:" +
                    std::to_string(theme::CODE_LINE_H - 2) + "px;width:" +
                    std::to_string((to - from) * cw) + "px'/>";
        }
        // An empty div collapses, which would shift every following line up.
        //
        // The row's content sits in a span the row clips, so a line scrolled
        // sideways is windowed without the layer leaving its box. Highlighting
        // still runs over the whole line — slicing the text first would cut a
        // string or a word in half and paint the remainder as the wrong thing.
        const std::string shift =
            g.code_scroll_x > 0
                ? "<span class='sh' style='left:" + std::to_string(-g.code_scroll_x) + "px'>"
                : "<span class='sh'>";
        // Three states, in the order that matters: the line the program is
        // stopped on wins over an error on the same line, because the user is
        // looking at where it stopped.
        const char* row_class = (int)i + 1 == g.stopped_line ? "<div class='stopline'>"
                                : bad                        ? "<div class='badline'>"
                                                             : "<div>";
        html += std::string(row_class) + shift +
                (lines[i].empty() ? std::string("&nbsp;") : highlight_line(lines[i])) + bars +
                "</span></div>";
    }
    layer->SetInnerRML(html);

    // The gutter renders the same rows, so the two cannot drift apart: they
    // are built from one loop bound and share `CODE_LINE_H`. It is scrolled by
    // rendering what is on screen rather than by scrolling the element, which
    // is what keeps an RmlUi scrollbar out of it.
    if (Rml::Element* gutter = by_id("dbggutter")) {
        std::string marks_html;
        for (size_t i = first_line; i < lines.size() && i < first_line + rows; i++) {
            const int line = (int)i + 1;
            const bool has = g.breakpoints.count(line) > 0;
            const bool here = line == g.stopped_line;
            // Three columns: the breakpoint, the execution cue, the number.
            // The first two are separate because they are separate facts and
            // are usually true of the same line at once — a program stops
            // where you put a breakpoint, and hiding one behind the other is
            // hiding the answer to "did it stop where I asked".
            std::string cls = "row";
            std::string mark;
            if (has) {
                // Hollow until the engine says where it went. A breakpoint the
                // engine could not place must not look like one that fired.
                cls += bound_breakpoint(line) ? " bp" : " pending";
                mark = bound_breakpoint(line) ? "\xe2\x97\x8f" : "\xe2\x97\x8b";
            }
            std::string cue;
            if (here) {
                cls += " cur";
                cue = "\xe2\x9e\x9c";
            }
            marks_html += "<div class='" + cls + "' oe-bp='" + std::to_string(line) + "'>"
                          "<span class='mk'>" + mark + "</span>"
                          "<span class='cue'>" + cue + "</span>"
                          "<span class='num'>" + std::to_string(line) + "</span></div>";
        }
        gutter->SetInnerRML(marks_html);
    }
}

/// Draw what the stopped program holds, and where it stopped.
///
/// Both panes come and go with the session rather than sitting empty: a
/// Variables pane showing nothing is indistinguishable from one that is broken.
void render_locals() {
    Rml::Element* head = by_id("varhead");
    Rml::Element* body = by_id("vars");
    if (!head || !body) return;
    const bool stopped = g.stopped_line > 0 && g.dbg.running();
    head->SetProperty("display", stopped ? "block" : "none");
    body->SetProperty("display", stopped ? "block" : "none");
    // The preview steps aside rather than sharing the pane: two scrolling
    // lists in one half-height box gives each too little to be read.
    if (Rml::Element* e = by_id("codehead")) {
        e->SetProperty("display", stopped ? "none" : "block");
    }
    if (Rml::Element* e = by_id("code")) {
        e->SetProperty("display", stopped ? "none" : "block");
    }
    if (!stopped) {
        body->SetInnerRML("");
        return;
    }
    std::string html;
    // Where it stopped, first: the call stack answers "how did I get here",
    // and it is the question a breakpoint in an event handler raises.
    for (const auto& f : g.dbg.frames()) {
        html += "<div class='varrow frame'>" + esc(f.name) + "  <span class='dim'>" +
                esc(g.model.path) + ":" + std::to_string(f.line) + "</span></div>";
    }
    for (const auto& v : g.dbg.locals()) {
        html += "<div class='varrow'><span class='varname'>" + esc(v.name) +
                "</span> = " + esc(v.value) + "</div>";
    }
    if (g.dbg.locals().empty()) {
        html += "<div class='varrow dim'>no variables here</div>";
    }
    body->SetInnerRML(html);
}

/// Split the output pane between PROBLEMS and the build log.
///
/// Both need an explicit height or they grow to fit their content and the pane
/// clips them, which looks exactly like a console that will not scroll. The
/// problems strip is sized to what it holds, so a clean file gives the log
/// almost the whole pane instead of reserving space for nothing.
void size_output_pane() {
    using namespace theme;
    // The strip holds diagnostics or, until the next diagnostics arrive, a
    // references list; either way it is sized to what it shows.
    const size_t shown = std::max(g.diagnostics.size(), g.refs.size());
    const int rows = (int)std::min<size_t>(shown, 4);
    const int problems_h = shown == 0 ? 22 : rows * 18 + 12;
    if (Rml::Element* e = by_id("problems")) {
        e->SetProperty("height", Rml::String(std::to_string(problems_h) + "px"));
    }
    Rml::Element* box = by_id("logbox");
    Rml::Element* pane = by_id("logpane");
    if (!box || !pane) return;

    // Measure rather than assume. Deriving the height from constants means
    // guessing the exact height of two pane headers; being 45px out put the
    // newest lines below the window, where scrolling could never reach them.
    // The laid-out geometry is the one thing that cannot be wrong.
    const float top = box->GetAbsoluteTop();
    const float pane_bottom = pane->GetAbsoluteTop() + pane->GetBox().GetSize().y;
    int h = (int)(pane_bottom - top);
    if (h <= 0) h = std::max(40, g.bottom_h - 2 * PANEHEAD_H - problems_h);   // before first layout
    h = std::max(2 * LOG_LINE_H, h - h % LOG_LINE_H);   // whole rows; see the stylesheet
    const int w = (int)pane->GetBox().GetSize().x;
    const std::string px = std::to_string(h) + "px";
    if (box->GetProperty(Rml::PropertyId::Height)->ToString() != px) {
        box->SetProperty("height", px);
    }
    // Both layers sized inline, as the editor's are: the text widget writes
    // its own layout properties onto the element, and an inline property
    // beats the stylesheet, so a `width:100%` rule there is silently ignored
    // and the textarea collapses to its default column width.
    // Guarded like the box's height: this runs every frame, and a property
    // written every frame is a layout every frame.
    const std::string wpx = std::to_string(w - 20) + "px";
    for (const char* id : {"log", "loghl"}) {
        Rml::Element* e = by_id(id);
        if (!e) continue;
        if (e->GetProperty(Rml::PropertyId::Width)->ToString() != wpx) e->SetProperty("width", wpx);
        if (e->GetProperty(Rml::PropertyId::Height)->ToString() != px) e->SetProperty("height", px);
    }
    // No scrollbars: Studio scrolls the console itself, and the widget's
    // own (an inline `overflow:auto`, written when it was built) took ten
    // pixels off the box and left the newest line half hidden under them.
    // Once: the widget writes its overflow when it is built, not after.
    static bool overflow_set = false;
    if (Rml::Element* e = by_id("log"); e && !overflow_set) {
        e->SetProperty("overflow-x", "hidden");
        e->SetProperty("overflow-y", "hidden");
        overflow_set = true;
    }
}

/// Show what the language server reported: a mark against each bad line, and
/// the messages in the console.
///
/// Diagnostics never touch the editor's text. Writing into the control would
/// fight the unsaved-changes guard and could destroy what the user typed.
void render_diagnostics() {
    // The marks are drawn by the highlight layer itself, which knows which
    // rows are on screen; marking children by source line here put the
    // underline on the wrong row as soon as the editor had scrolled.
    refresh_highlight();
    // The server republishes on every change, the same set included. A
    // references list is only displaced by news; the same diagnostics again
    // are not news, and typing clears the list itself.
    std::string marks;
    for (const auto& d : g.diagnostics) {
        marks += std::to_string(d.range.line) + ":" + std::to_string(d.range.character) + " " +
                 d.message + "\n";
    }
    static std::string shown;
    const bool same = marks == shown;
    shown = marks;
    if (same && !g.refs.empty()) return;
    g.refs.clear();
    if (Rml::Element* box = by_id("problems")) {
        std::string html;
        for (const auto& d : g.diagnostics) {
            html += "<div class='problem'><span class='pline'>line " +
                    std::to_string(d.line + 1) + ":" + std::to_string(d.range.character + 1) +
                    "</span>" + esc(d.message) + "</div>";
        }
        if (g.diagnostics.empty()) html = "<div class='noproblems'>No problems.</div>";
        box->SetInnerRML(html);
    }
    size_output_pane();
    if (Rml::Element* tab = by_id("problemcount")) {
        tab->SetInnerRML(g.diagnostics.empty()
                             ? "PROBLEMS"
                             : "PROBLEMS (" + std::to_string(g.diagnostics.size()) + ")");
    }
}

/// Apply the editor's scroll offset to both layers at once, so they cannot
/// drift apart, and clamp it to the text that actually exists.
void sync_highlight_scroll() {
    Rml::Element* ed = by_id("fullcode");
    Rml::Element* view = by_id("codeview");
    if (!ed || !view) return;

    // Whole lines only. A part-line offset would put the top row partly above
    // the viewport, which is the one thing this design cannot draw.
    const int viewport = (int)view->GetBox().GetSize().y;
    const int max_scroll =
        std::max(0, g.code_content_h - viewport + 2 * theme::CODE_LINE_H);
    g.code_scroll = std::max(0, std::min(g.code_scroll, max_scroll));
    g.code_scroll -= g.code_scroll % theme::CODE_LINE_H;

    // The text control scrolls its own text and clips it properly, so it does
    // the work for the editable layer; the highlight layer is redrawn instead.
    //
    // Ask, then read back what it actually did, then snap to a whole line and
    // ask again. RmlUi clamps to its own maximum, and a scroll that is not a
    // multiple of the line height leaves the two layers half a line apart —
    // which is invisible until you notice every glyph has a ghost.
    ed->SetScrollTop((float)g.code_scroll);
    const int actual = (int)ed->GetScrollTop();
    g.code_scroll = actual - (actual % theme::CODE_LINE_H);
    ed->SetScrollTop((float)g.code_scroll);

    // Sideways the control leads and the layer follows: the caret is the
    // control's, so it decides when a long line has to slide, and the colour
    // layer is moved to match. Reading it back rather than setting it is the
    // whole difference — the control scrolls itself as the caret moves, and
    // nothing here is told when that happens.
    g.code_scroll_x = (int)ed->GetScrollLeft();
    refresh_highlight();
}

/// Push the editor's text to disk and reload the designer model from it.
///
/// The Rust parser stays the only reader of `.kiln`, so the model is
/// rebuilt by re-inspecting the saved file rather than by parsing text here —
/// two grammars would drift.
bool apply_code() {
    auto* ed = code_editor();
    if (!ed) return false;
    const std::string text = ed->GetValue();

    std::FILE* f = std::fopen(g.model.path.c_str(), "w");
    if (!f) { log("cannot write " + g.model.path, "err"); return false; }
    std::fwrite(text.data(), 1, text.size(), f);
    std::fclose(f);

    Model fresh;
    std::string err;
    if (!load_model(g.kiln_bin, g.model.path, fresh, err)) {
        // The text is saved either way — losing the user's typing because it
        // does not compile yet would be far worse than an out-of-date canvas.
        log("saved, but the designer could not read it back:", "err");
        log("  " + err, "err");
        set_status("saved with errors — see the console");
        g.code_dirty = false;
        g.dirty = false;
        g.model_text = text;
        return false;
    }
    fresh.path = g.model.path;
    g.model = fresh;
    g.model_text = text;
    g.code_dirty = false;
    g.dirty = false;
    g.selection.clear();
    g.selected.clear();
    set_status("saved " + g.model.path);
    return true;
}

/// Switch the centre pane between the designer canvas and the code view.
void set_view(const std::string& view) {
    // Leaving the code view with unsaved text: commit it first, so the canvas
    // reflects what the file now says rather than a stale model.
    if (g.view == "code" && view != "code" && g.code_dirty) apply_code();
    close_completion();
    g.view = view;
    const bool code = (view == "code");
    if (Rml::Element* e = by_id("canvasarea")) e->SetProperty("display", code ? "none" : "block");
    if (Rml::Element* e = by_id("codeview")) e->SetProperty("display", code ? "block" : "none");
    // Sizing lives in relayout(), which is the single place that knows the
    // current dock geometry; duplicating it here is how the two drift.
    if (code) {
        relayout();
        // Lay out before deciding how many lines fit. Asking first gets the
        // height the view had a moment ago — zero, on the way in — and the
        // editor paints a single line until something else nudges it.
        g.context->Update();
        refresh_highlight();
    }
    // The document tabs show which view is up; there is no second switcher.
    for (const char* id : {"tabdesigner", "tabcode"}) {
        if (Rml::Element* e = by_id(id)) {
            const bool is_code = std::string(id).find("code") != std::string::npos;
            const bool active = (is_code == code);
            const std::string base = std::string(id).rfind("tab", 0) == 0 ? "tab" : "tb";
            e->SetAttribute("class", Rml::String(active ? base + (base == "tab" ? (is_code ? " active code" : " active")
                                                                               : " primary")
                                                        : base + (base == "tab" ? "" : " ghost")));
        }
    }
    rebuild_code();
    update_tabs();
    set_status(code ? "code view" : "designer view");
}

/// Is the keyboard focus inside the inspector's property grid?
bool focus_in_grid() {
    if (!g.context) return false;
    for (Rml::Element* e = g.context->GetFocusElement(); e; e = e->GetParentNode()) {
        if (e->GetId() == "grid") return true;
    }
    return false;
}

/// Rebuild the inspector that refresh_all() put off, and give the caret back
/// to the field it was in, where it was — so typing `left,right` into the
/// anchors field lights the boxes as it goes without the caret jumping.
void flush_inspector() {
    if (!g.inspector_stale) return;
    g.inspector_stale = false;
    std::string name;
    int s0 = 0, s1 = 0;
    if (focus_in_grid()) {
        Rml::Element* f = g.context->GetFocusElement();
        name = f->GetAttribute<Rml::String>("name", "");
        if (auto* in = dynamic_cast<Rml::ElementFormControlInput*>(f)) {
            Rml::String sel;
            in->GetSelection(&s0, &s1, &sel);
        }
    }
    rebuild_inspector();
    if (name.empty()) return;
    Rml::Element* grid = by_id("grid");
    Rml::ElementList inputs;
    if (grid) grid->GetElementsByTagName(inputs, "input");
    for (Rml::Element* e : inputs) {
        if (e->GetAttribute<Rml::String>("name", "") != name) continue;
        e->Focus();
        if (auto* in = dynamic_cast<Rml::ElementFormControlInput*>(e)) in->SetSelectionRange(s0, s1);
        break;
    }
}

void refresh_all() {
    rebuild_canvas();
    rebuild_tray();
    // Not the inspector while the user is typing in it: rebuilding the grid
    // destroys the input RmlUi is still delivering the keystroke to — the
    // caret lost after one character at best, a use-after-free inside the
    // text widget at worst (a SIGSEGV in WidgetTextInput::MoveToCursor).
    // The frame loop rebuilds it once focus has left the grid.
    if (focus_in_grid()) g.inspector_stale = true;
    else rebuild_inspector();
    rebuild_code();
}

/// Select `id`. With `add`, extend the selection (ctrl-click) instead of
/// replacing it.
void select(const std::string& id, bool add = false) {
    if (!add) g.selection.clear();
    if (!id.empty() && !is_selected(id)) g.selection.push_back(id);
    g.selected = id;
    refresh_all();
}

/* --- actions -------------------------------------------------------------- */

void add_component(const std::string& type_name) {
    const CatalogComponent* desc = g.catalog.find(type_name);
    if (!desc) { set_status("unknown component type " + type_name); return; }
    push_undo();
    Component c;
    c.id = g.model.fresh_id(type_name);
    c.type_name = type_name;
    for (const auto& p : desc->props) {
        if (p.has_default) c.set_property(p.name, p.default_value);
    }
    if (desc->visual) {
        write_int(c, "left", 20 + 12 * (int)g.model.children.size());
        write_int(c, "top", 20 + 34 * (int)g.model.children.size());
        g.model.children.push_back(c);
    } else {
        // No rectangle, so no place on the canvas: this becomes a declaration
        // beside the form, and the tray is where it can be selected.
        g.model.module_components.push_back(c);
    }
    // The kit has to be in scope for the declaration to compile. Dropping the
    // component is asking for it, so the module gains the `use` line: the
    // model records it here (after push_undo, so an undo takes it back out
    // with the component) and the next save writes it after the header.
    if (!desc->kit.empty()) {
        bool used = false;
        for (const auto& u : g.model.uses) {
            if (u == desc->kit) used = true;
        }
        if (!used) {
            g.model.uses.push_back(desc->kit);
            log("added `use " + desc->kit + "` for " + type_name + "; it is written on save.");
        }
    }
    mark_dirty();
    set_status("added " + c.id + (desc->visual ? "" : " (tray)"));
    select(c.id);
}

/// Only the descriptor's declared type can say whether a value needs quotes:
/// `text = "true"` and `checked = true` look identical as strings.
bool property_needs_quotes(const std::string& type_name, const std::string& property) {
    // The catalogue, not the linked table: a component from a kit the designer
    // was not compiled against still has declared types, and quoting its
    // `interval = 500` would write source that does not typecheck.
    if (const CatalogComponent* c = g.catalog.find(type_name)) {
        for (const auto& p : c->props) {
            if (p.name == property) return p.type == "text";
        }
    }
    const Kiln_ComponentDesc* desc = describe(type_name.c_str());
    if (!desc) return true;
    for (int i = 0; i < desc->property_count; i++) {
        if (property == desc->properties[i].name) {
            return desc->properties[i].tag == KN_SDT_TEXT;
        }
    }
    return true;
}

/// Begin dragging `id`, grabbing at screen point (mx,my).
///
/// The grab offset comes from the MODEL, not from the element: select()
/// rebuilds the canvas, so the element is destroyed before we could ask where
/// it was, and its stale offset made every drag jump to the top-left corner.
void begin_drag(const std::string& id, int mx, int my) {
    Rml::Element* canvas = by_id("canvas");
    const Component* comp = g.model.find(id);
    if (!canvas || !comp) return;
    // The form's left/top say where its window opens on the screen, not
    // where the preview sits on the canvas: a drag must not move it here.
    if (id == g.model.form_name) { select(id); return; }
    // A tray component has no left/top to move, and dragging one would invent
    // both — which the compiler then rejects. So would a canvas component
    // whose descriptor declares neither.
    if (g.model.is_module_level(id)) { select(id); return; }
    if (!can_write(*comp, "left") && !can_write(*comp, "top")) {
        select(id);
        set_status(comp->type_name + " has no position to move");
        return;
    }
    push_undo();   // one snapshot per drag, not per mouse move
    const auto origin = canvas->GetAbsoluteOffset();
    g.drag_dx = mx - ((int)origin.x + prop_int(*comp, "left", 0));
    g.drag_dy = my - ((int)origin.y + prop_int(*comp, "top", 0));
    g.dragging = true;
    select(id);
}

/// Continue a drag to screen point (mx,my).
void drag_to(int mx, int my) {
    if (!g.dragging || g.selected.empty()) return;
    Component* c = g.model.find(g.selected);
    Rml::Element* canvas = by_id("canvas");
    if (!c || !canvas) return;
    const auto origin = canvas->GetAbsoluteOffset();
    int x = mx - g.drag_dx - (int)origin.x;
    int y = my - g.drag_dy - (int)origin.y;
    const int fw = prop_int(g.model.form, "width", 420);
    const int fh = prop_int(g.model.form, "height", 260);
    const int w = prop_int(*c, "width", 120), h = prop_int(*c, "height", 32);
    if (x < 0) x = 0;
    if (y < 0) y = 0;
    if (x > fw - w) x = fw - w;
    if (y > fh - h) y = fh - h;

    // Alignment guides: if an edge or centre lines up with another component's,
    // snap exactly to it and remember the line so it can be drawn. This is what
    // makes a designer feel precise rather than approximate.
    constexpr int PULL = 6;
    g.guide_x.clear();
    g.guide_y.clear();
    for (const auto& other : g.model.children) {
        if (other.id == c->id) continue;
        const int ox = prop_int(other, "left", 0), oy = prop_int(other, "top", 0);
        const int ow = prop_int(other, "width", 120), oh = prop_int(other, "height", 32);
        const int xs[3] = {ox, ox + ow / 2 - w / 2, ox + ow - w};
        const int xg[3] = {ox, ox + ow / 2, ox + ow};
        for (int i = 0; i < 3; i++) {
            if (std::abs(x - xs[i]) <= PULL) { x = xs[i]; g.guide_x.push_back(xg[i]); }
        }
        const int ys[3] = {oy, oy + oh / 2 - h / 2, oy + oh - h};
        const int yg[3] = {oy, oy + oh / 2, oy + oh};
        for (int i = 0; i < 3; i++) {
            if (std::abs(y - ys[i]) <= PULL) { y = ys[i]; g.guide_y.push_back(yg[i]); }
        }
    }
    // Snap to the grid only when no neighbour claimed the position.
    write_int(*c, "left", g.guide_x.empty() ? snap(x) : x);
    write_int(*c, "top", g.guide_y.empty() ? snap(y) : y);
    mark_dirty();
    rebuild_canvas();
}

/// Is `id` part of the current selection?
bool is_selected(const std::string& id) {
    for (const auto& s : g.selection) {
        if (s == id) return true;
    }
    return false;
}

/// Delete every selected component.
void delete_selection() {
    if (g.selection.empty()) { set_status("nothing selected"); return; }
    if (is_selected(g.model.form_name)) { set_status("the form cannot be deleted"); return; }
    push_undo();
    int gone = 0;
    std::vector<Component> kept;
    for (const auto& c : g.model.children) {
        if (is_selected(c.id)) gone++;
        else kept.push_back(c);
    }
    g.model.children = kept;
    // A tray component already written to the file is FLAGGED rather than
    // dropped: only its span knows which lines the next save must splice away,
    // and a component simply removed from the model would stay in the file.
    std::vector<Component> kept_mod;
    for (auto& c : g.model.module_components) {
        if (!is_selected(c.id)) { kept_mod.push_back(c); continue; }
        gone++;
        if (c.last_line > 0) { c.removed = true; kept_mod.push_back(c); }
    }
    g.model.module_components = kept_mod;
    g.selection.clear();
    g.selected.clear();
    mark_dirty();
    refresh_all();
    set_status("deleted " + std::to_string(gone) + " component(s)");
}

void copy_selection() {
    g.clipboard.clear();
    for (const auto& c : g.model.children) {
        if (is_selected(c.id)) g.clipboard.push_back(c);
    }
    set_status("copied " + std::to_string(g.clipboard.size()) + " component(s)");
}

/// Paste the clipboard, offset slightly so the copies are visible and
/// re-identified so ids stay unique.
void paste_clipboard() {
    if (g.clipboard.empty()) { set_status("clipboard is empty"); return; }
    push_undo();
    g.selection.clear();
    for (Component c : g.clipboard) {
        c.id = g.model.fresh_id(c.type_name);
        write_int(c, "left", prop_int(c, "left", 0) + 10);
        write_int(c, "top", prop_int(c, "top", 0) + 10);
        g.model.children.push_back(c);
        g.selection.push_back(c.id);
        g.selected = c.id;
    }
    mark_dirty();
    refresh_all();
    set_status("pasted " + std::to_string(g.clipboard.size()) + " component(s)");
}

void save() {
    // In the code view the user's text is the truth, not the component model:
    // splicing the model over it would discard everything they typed.
    if (g.code_dirty) {
        apply_code();
        rebuild_canvas();
        rebuild_inspector();
        return;
    }
    std::string err;
    if (!save_model(g.model, g.pending_subs, property_needs_quotes, err)) {
        set_status("save failed: " + err);
        return;
    }
    g.pending_subs.clear();
    g.dirty = false;
    set_status("saved " + g.model.path);
    rebuild_code();
}

/// Give the selection a new id, from the Name field or the `rename:` verb.
/// Nothing is written until the next save, which is when the file's other
/// lines are renamed too; the grid is left alone so the field keeps its caret.
void rename_selected(const std::string& new_id) {
    Component* comp = g.model.find(g.selected);
    if (!comp) { set_status("nothing selected"); return; }
    if (g.code_dirty) { set_status("cannot rename while the code view has unsaved text"); return; }
    const std::string old_id = comp->id;
    const std::string type_name = comp->type_name;   // comp dies with the model below
    if (old_id == new_id) return;
    Model trial = g.model;
    std::string err;
    if (!rename_id(trial, old_id, new_id, err)) {
        set_status("cannot rename " + old_id + ": " + err);
        return;
    }
    push_undo();
    g.model = trial;
    for (auto& id : g.selection) {
        if (id == old_id) id = new_id;
    }
    g.selected = new_id;
    mark_dirty();
    if (Rml::Element* ctx = by_id("ctxlabel")) {
        ctx->SetInnerRML(esc(new_id) + " <span style='color:#656d76;font-weight:normal'>(" +
                         esc(type_name) + ")</span>");
    }
    rebuild_canvas();
    rebuild_tray();
    rebuild_code();
    set_status("renamed " + old_id + " to " + new_id);
}

/// Seconds since an arbitrary origin, for step timings.
double now_seconds() {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (double)ts.tv_sec + (double)ts.tv_nsec / 1e9;
}

/// Show or hide the activity indicator, and say what it is doing.
void set_activity(const char* what) {
    const bool busy = (what != nullptr);
    if (Rml::Element* e = by_id("activity"))
        e->SetProperty("display", busy ? "block" : "none");
    if (Rml::Element* e = by_id("activitylabel"))
        e->SetInnerRML(busy ? esc(what) : "");
    // The running indicator pulses for as long as the app is alive.
    if (Rml::Element* e = by_id("runlamp"))
        e->SetProperty("display", g.running_app > 0 ? "inline" : "none");
}

/// The directory holding the open project. A loose `.kiln` opened on its own
/// has no project around it, so its own directory is the project.
std::string project_dir() {
    const size_t slash = g.model.path.find_last_of('/');
    return slash == std::string::npos ? std::string(".") : g.model.path.substr(0, slash);
}

/// Where Run's throwaway binary lives. Kept in a dotted subdirectory so a
/// project listing does not fill with build output, and so one line in
/// `.gitignore` covers everything Studio ever writes into a project.
std::string run_dir() { return project_dir() + "/.kiln/run"; }

/// Remove what Run left behind. Called when a real build succeeds: from then
/// on the binary in the output directory is the answer to "where is my
/// program", and a stale throwaway beside it is a second, wrong answer.
void clear_run_artifacts() {
    const std::string stem =
        g.model.module_name.empty() ? std::string("app") : g.model.module_name;
    const std::string path = run_dir() + "/" + kiln::sys::program_name(stem);
    if (kiln::sys::remove_file(path)) log("  removed the run build " + path, "muted");
    // And the directories, when nothing else is in them. `rmdir` on a
    // non-empty directory fails, which is exactly the guard wanted: a user who
    // put something of their own in there keeps it.
    kiln::sys::remove_dir(run_dir());
    kiln::sys::remove_dir(project_dir() + "/.kiln");
}

/// Whether the engine placed a breakpoint on a line, so the gutter can draw
/// one it could not differently from one that will fire.
///
/// Unknown until the engine answers, and unknown draws as not-yet-bound: a
/// mark that promises to stop and does not is worse than one that looks
/// tentative for a moment.
bool bound_breakpoint(int line) {
    for (const auto& b : g.dbg.breakpoints()) {
        if (b.line == line) return b.verified;
    }
    return false;
}

/// Is the app started by Run still alive?
///
/// `g.running_app` is the pid Run recorded, and a pid outlives the process:
/// between the app exiting and `poll_app` noticing, the field still holds it.
/// Every guard below asks this rather than the field, so a Run refused
/// because "an app is already running" is never refused because of one that
/// finished a frame ago.
bool app_alive() {
#ifdef _WIN32
    if (g.running_app <= 0) return false;
    int code = 0;
    return !kiln::sys::try_wait(g.app_child, code);
#else
    return g.running_app > 0 && ::kill(g.running_app, 0) == 0;
#endif
}

/// Show or hide the debugging controls, and say what the session is doing.
void update_debug_controls() {
    const bool live = g.dbg.running();
    // By id rather than by class: these carry three classes each, and a
    // lookup that has to match one of several is a lookup that can quietly
    // match none.
    for (const char* id : {"btn_dbgcontinue", "btn_dbgpause", "btn_dbgstepover",
                           "btn_dbgstepin", "btn_dbgstepout", "btn_dbgstop"}) {
        if (Rml::Element* e = by_id(id)) {
            e->SetProperty("display", live ? "inline-block" : "none");
        }
    }
    // Run and Debug are mutually exclusive with everything already going: one
    // app at a time, and a debugging session is an app. Marked here rather
    // than only refused on the click, so the toolbar says so before you try.
    const bool busy = live || app_alive();
    for (const char* id : {"btn_run", "btn_debug"}) {
        if (Rml::Element* e = by_id(id)) e->SetClass("blocked", busy);
    }
}

/// Start a debugging session on the open module.
void start_debug() {
    if (g.dbg.running()) { set_status("already debugging"); return; }
    // One program at a time. Debugging a second copy while the first is still
    // running gives two processes writing to one console and two sets of
    // output attributed to neither.
    if (app_alive()) {
        set_status("an app is running \xE2\x80\x94 stop it before debugging");
        log("> an app is already running; press Stop first", "err");
        return;
    }
    // Saved first: a breakpoint on line 12 of text that has been edited and
    // not written is a breakpoint on a line the built program does not have.
    save();
    g.log_lines.clear();
    log("> debugging " + g.model.path, "muted");
    std::vector<int> lines(g.breakpoints.begin(), g.breakpoints.end());
    if (!g.dbg.start(g.kiln_bin, g.model.path)) {
        log("could not start the debugger", "err");
        return;
    }
    g.dbg.set_breakpoints(lines);
    update_debug_controls();
    set_status("debugging");
    set_activity("Debugging");
}

/// End the session, and put the editor back the way it was.
void stop_debug() {
    if (!g.dbg.running()) {
        // A session whose process has gone but whose *view* has not: the
        // stopped line is still highlighted and the locals still listed.
        // Clearing it is the whole job here, and skipping it left the editor
        // stopped on a line no debugger was on.
        if (g.stopped_line > 0) {
            g.stopped_line = 0;
            update_debug_controls();
            refresh_highlight();
            render_locals();
            set_activity(nullptr);
        }
        set_status("not debugging");
        return;
    }
    g.dbg.stop();
    g.stopped_line = 0;
    update_debug_controls();
    refresh_highlight();
    render_locals();
    set_activity(nullptr);
    set_status("debugging ended");
}

/// Take whatever the debugger has said since the last frame.
void poll_debug() {
    if (!g.dbg.running() && g.stopped_line == 0) return;
    g.dbg.poll();
    if (!g.dbg.has_update()) return;
    g.dbg.clear_update();

    for (const auto& line : g.dbg.output()) {
        log(line.text, line.category == "stderr" ? "err" : nullptr);
    }
    g.dbg.clear_output();

    // A breakpoint on a blank line or a comment runs nothing, and the engine
    // moves it to the next line that does. The mark moves with it: a dot on a
    // line that will never be reached lies about where the program will stop.
    for (const auto& b : g.dbg.breakpoints()) {
        if (b.verified && !g.breakpoints.count(b.line)) {
            g.breakpoints.insert(b.line);
        }
    }

    const int was = g.stopped_line;
    g.stopped_line = g.dbg.stopped_line();
    if (g.stopped_line != was) {
        // Follow the program: a stop on a line that is scrolled away is a stop
        // the user cannot see.
        if (g.stopped_line > 0) {
            if (g.view != "code") set_view("code");
            jump_to(g.stopped_line - 1, 0);
        }
        refresh_highlight();
    }
    render_locals();
    if (!g.dbg.running()) {
        update_debug_controls();
        set_activity(nullptr);
    }
}

/// Toggle a breakpoint on a line, and tell the engine if one is running.
void toggle_breakpoint(int line) {
    if (line <= 0) return;
    if (g.breakpoints.count(line)) {
        g.breakpoints.erase(line);
    } else {
        g.breakpoints.insert(line);
    }
    if (g.dbg.running()) {
        std::vector<int> lines(g.breakpoints.begin(), g.breakpoints.end());
        g.dbg.set_breakpoints(lines);
    }
    refresh_highlight();
}

/// Start a build. Returns immediately; poll_build() reports progress.
void build_binary(bool then_run) {
    if (g.build_pid > 0) { set_status("a build is already running"); return; }
    // Refused here rather than after the compile: Run rebuilds first, and a
    // rebuild that ends in "an app is already running" spent the time to say
    // nothing. Build Binary is unaffected — building while a program runs is
    // fine, it is *starting a second one* that is not.
    if (then_run && (app_alive() || g.dbg.running())) {
        const char* why = g.dbg.running() ? "a debugging session is open"
                                          : "an app is already running";
        set_status(std::string(why) + " \xE2\x80\x94 press Stop first");
        log(std::string("> ") + why + "; press Stop first", "err");
        return;
    }
    save();
    g.log_lines.clear();

    // Where the binary lands, and it is two different questions.
    //
    // Run wants a throwaway, and it belongs *inside the project* rather than
    // in the system's temporary directory: a shared /tmp name collides between
    // two Studios and between two projects, and a binary that lives beside the
    // source that produced it is one a user can find, inspect and delete.
    // Build wants a keepsake, and it is what clears the throwaway away.
    const std::string stem =
        g.model.module_name.empty() ? std::string("app") : g.model.module_name;
    if (then_run) {
        const std::string dir = run_dir();
        kiln::sys::make_dirs(dir);
        g.build_target = dir + "/" + kiln::sys::program_name(stem);
    } else {
        std::string out_dir = kiln::settings::text("build.output_dir");
        if (out_dir.empty()) out_dir = project_dir() + "/build";
        kiln::sys::make_dirs(out_dir);
        g.build_target = out_dir + "/" + kiln::sys::program_name(stem);
    }
    const std::string release =
        kiln::settings::boolean("build.release") ? " --release" : "";
    const std::string cmd = g.kiln_bin + " build " + g.model.path + release + " -o " +
                            g.build_target + " 2>&1";

    // Verbose by design: the console should let you see what the toolchain
    // actually did, not just whether it succeeded.
    log("> " + g.kiln_bin + " build " + g.model.path + " -o " + g.build_target);
    log("  stage 1/4  parse + validate .kiln", "muted");
    log("  stage 2/4  lower to LLVM IR", "muted");
    log("  stage 3/4  clang: assemble + link the runtime", "muted");
    log("  stage 4/4  dead-strip unused commands (--gc-sections)", "muted");

#ifdef _WIN32
    // No shell: the compiler is started directly, its stderr merged into the
    // same pipe the `2>&1` above merges it into on POSIX.
    (void)cmd;
    using namespace kiln::sys;
    if (!spawn(g.build_child,
               quote_arg(g.kiln_bin) + " build " + quote_arg(g.model.path) + " -o " +
                   quote_arg(g.build_target),
               true, false)) {
        log("could not start the build", "err");
        return;
    }
    g.build_pid = (pid_t)g.build_child.pid;
#else
    int fds[2] = {-1, -1};
    if (::pipe(fds) != 0) { log("could not create the build pipe", "err"); return; }
    const pid_t pid = fork();
    if (pid == 0) {
        ::close(fds[0]);
        ::dup2(fds[1], STDOUT_FILENO);
        ::dup2(fds[1], STDERR_FILENO);
        ::close(fds[1]);
        execl("/bin/sh", "sh", "-c", cmd.c_str(), (char*)nullptr);
        _exit(127);
    }
    ::close(fds[1]);
    if (pid < 0) { ::close(fds[0]); log("could not start the build", "err"); return; }
    ::fcntl(fds[0], F_SETFL, O_NONBLOCK);

    g.build_pid = pid;
    g.build_output = fds[0];
#endif
    g.build_then_run = then_run;
    g.build_started = now_seconds();
    set_status("building...");
    set_activity("Building");
}

/// Drain the build's output and, when it finishes, report and maybe run.
void poll_build() {
    if (g.build_pid <= 0) return;

#ifdef _WIN32
    if (g.build_child.out) {
        char buf[1024];
        int n;
        static std::string partial;
        while ((n = kiln::sys::read_nonblocking(g.build_child.out, buf, sizeof buf)) > 0) {
            partial.append(buf, (size_t)n);
            size_t nl;
            while ((nl = partial.find('\n')) != std::string::npos) {
                log("  " + partial.substr(0, nl), "muted");
                partial.erase(0, nl + 1);
            }
        }
        if (n == 0) {
            if (!partial.empty()) { log("  " + partial, "muted"); partial.clear(); }
            kiln::sys::close_output(g.build_child);
        }
    }

    int rc = -1;
    if (!kiln::sys::try_wait(g.build_child, rc)) return;
    const double secs = now_seconds() - g.build_started;
    g.build_pid = 0;
    kiln::sys::release(g.build_child);
#else
    if (g.build_output >= 0) {
        char buf[1024];
        ssize_t n;
        static std::string partial;
        while ((n = ::read(g.build_output, buf, sizeof buf)) > 0) {
            partial.append(buf, (size_t)n);
            size_t nl;
            while ((nl = partial.find('\n')) != std::string::npos) {
                log("  " + partial.substr(0, nl), "muted");
                partial.erase(0, nl + 1);
            }
        }
        if (n == 0) {
            if (!partial.empty()) { log("  " + partial, "muted"); partial.clear(); }
            ::close(g.build_output);
            g.build_output = -1;
        }
    }

    int status = 0;
    if (::waitpid(g.build_pid, &status, WNOHANG) != g.build_pid) return;
    const int rc = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
    const double secs = now_seconds() - g.build_started;
    g.build_pid = 0;
    if (g.build_output >= 0) { ::close(g.build_output); g.build_output = -1; }
#endif

    char line[320];
    if (rc != 0) {
        std::snprintf(line, sizeof line, "FAILED  exit %d after %.2fs", rc, secs);
        log(line, "err");
        set_status("build failed");
        set_activity(nullptr);
        return;
    }

    long bytes = 0;
    if (FILE* f = std::fopen(g.build_target.c_str(), "rb")) {
        std::fseek(f, 0, SEEK_END);
        bytes = std::ftell(f);
        std::fclose(f);
    }
    std::snprintf(line, sizeof line, "OK  %s — %ld KB in %.2fs (clean native, no runtime unpack)",
                  g.build_target.c_str(), bytes / 1024, secs);
    log(line, "ok");
    log("  IR stripped — no decompilation possible", "muted");
    set_status("build succeeded");

    if (g.build_then_run) {
        g.build_then_run = false;
        run_app(g.build_target);
    } else {
        clear_run_artifacts();
        set_activity(nullptr);
    }
}

/// Launch the built app, keeping its pid so Stop can end it. `system()` would
/// give us no handle on the child, which is why Stop could only ever say
/// "nothing running".
void run_app(const std::string& path) {
    // Refused, not restarted. Run used to stop whatever was running and start
    // again, which silently killed the program you were looking at — and did
    // it after a full rebuild, so the first sign was the output disappearing.
    if (app_alive()) {
        set_status("an app is already running \xE2\x80\x94 press Stop first");
        log("> an app is already running; press Stop first", "err");
        return;
    }
    if (g.dbg.running()) {
        set_status("a debugging session is open \xE2\x80\x94 press Stop first");
        log("> a debugging session is open; press Stop first", "err");
        return;
    }
    // Pipe the app's stdout and stderr back to us: its output belongs in the
    // IDE console, not in whatever terminal the IDE happened to start from.
#ifdef _WIN32
    if (!kiln::sys::spawn(g.app_child, kiln::sys::quote_arg(path), true, false)) {
        log("could not start the app");
        return;
    }
    const pid_t pid = (pid_t)g.app_child.pid;
#else
    int fds[2] = {-1, -1};
    if (::pipe(fds) != 0) { log("could not create the output pipe", "err"); return; }
    const pid_t pid = fork();
    if (pid == 0) {
        ::close(fds[0]);
        ::dup2(fds[1], STDOUT_FILENO);
        ::dup2(fds[1], STDERR_FILENO);
        ::close(fds[1]);
        execl(path.c_str(), path.c_str(), (char*)nullptr);
        _exit(127);
    }
    ::close(fds[1]);
    if (pid < 0) { ::close(fds[0]); log("could not start the app"); return; }
    // Non-blocking: poll_app drains it once per frame and must never stall the
    // UI waiting for an app that is simply quiet.
    ::fcntl(fds[0], F_SETFL, O_NONBLOCK);
    g.app_output = fds[0];
#endif
    g.running_app = pid;
    log("> running: " + path + "  (pid " + std::to_string(pid) + ")", "muted");
    log("  output below is the program's own stdout/stderr", "muted");
    set_status("running");
    set_activity("Running");
    update_debug_controls();
}

/// Stop the app started by Run, if it is still alive.
void stop_app() {
    if (g.running_app <= 0) { set_status("nothing running"); return; }

#ifdef _WIN32
    int code = 0;
    if (kiln::sys::try_wait(g.app_child, code)) {   // already exited
        kiln::sys::release(g.app_child);
        g.running_app = 0;
        set_status("nothing running");
        return;
    }
    kiln::sys::terminate(g.app_child);
    WaitForSingleObject(g.app_child.process, INFINITE);
    drain_app_output();
    log("> stopped (pid " + std::to_string(g.running_app) + ")", "muted");
    kiln::sys::release(g.app_child);
#else
    if (::kill(g.running_app, 0) != 0) {   // already exited
        g.running_app = 0;
        set_status("nothing running");
        return;
    }
    ::kill(g.running_app, SIGTERM);
    int status = 0;
    ::waitpid(g.running_app, &status, 0);
    drain_app_output();
    log("> stopped (pid " + std::to_string(g.running_app) + ")", "muted");
    if (g.app_output >= 0) { ::close(g.app_output); g.app_output = -1; }
#endif
    g.running_app = 0;
    set_status("stopped");
    set_activity(nullptr);
    update_debug_controls();
}

/// What the Stop button does: stop whatever is going, whichever it is.
///
/// Stop is one button and the user means one thing by it. Wired to `stop_app`
/// alone it left a debugging session open — the process died and the session
/// that was driving it did not, so the transport stayed on the toolbar, the
/// stopped line stayed highlighted, and Run went on refusing because Studio
/// still believed it was debugging. A debugging session *is* the running app;
/// ending it ends both.
void stop_all() {
    const bool dbg = g.dbg.running() || g.stopped_line > 0;
    const bool app = app_alive();
    if (dbg) stop_debug();
    if (app || g.running_app > 0) stop_app();
    if (!dbg && !app && g.running_app <= 0) set_status("nothing running");
}

/// Reap the app if it exited on its own, so Stop reports honestly.
void drain_app_output() {
#ifdef _WIN32
    if (!g.app_child.out) return;
    char buf[1024];
    int n;
    static std::string partial;
    while ((n = kiln::sys::read_nonblocking(g.app_child.out, buf, sizeof buf)) > 0) {
#else
    if (g.app_output < 0) return;
    char buf[1024];
    ssize_t n;
    static std::string partial;
    while ((n = ::read(g.app_output, buf, sizeof buf)) > 0) {
#endif
        partial.append(buf, (size_t)n);
        size_t nl;
        // Log whole lines only: a write split mid-line would otherwise appear
        // as two console entries.
        while ((nl = partial.find('\n')) != std::string::npos) {
            log(partial.substr(0, nl));
            partial.erase(0, nl + 1);
        }
    }
    if (n == 0) {   // the app closed its end
        if (!partial.empty()) { log(partial); partial.clear(); }
#ifdef _WIN32
        kiln::sys::close_output(g.app_child);
#else
        ::close(g.app_output);
        g.app_output = -1;
#endif
    }
}

void poll_app() {
    drain_app_output();
    if (g.running_app <= 0) return;
#ifdef _WIN32
    int code = 0;
    if (kiln::sys::try_wait(g.app_child, code)) {
        drain_app_output();      // whatever it printed on the way out
        kiln::sys::release(g.app_child);
#else
    int status = 0;
    if (::waitpid(g.running_app, &status, WNOHANG) == g.running_app) {
        drain_app_output();      // whatever it printed on the way out
        const int code = WEXITSTATUS(status);
#endif
        log("> app exited with code " + std::to_string(code), "muted");
        g.running_app = 0;
        // A console program finishes in milliseconds. Saying "nothing running"
        // afterwards reads as though it never started; say how it ended.
        set_status(code == 0 ? "finished (exit 0)" : "exited with code " + std::to_string(code));
        set_activity(nullptr);
        // Run is blocked while an app is alive, so the frame that notices it
        // died is the frame that has to unblock it.
        update_debug_controls();
    }
}


/// Global keyboard shortcuts.
///
/// Deliberately minimal: every key not claimed here falls through to RmlUi, and
/// therefore to the code editor's caret. Claiming plain letters for tool
/// shortcuts — the obvious next feature — would make the editor untypeable, so
/// anything added here must require a modifier.
bool on_key_down(Rml::Context* context, Rml::Input::KeyIdentifier key, int modifier, float, bool priority) {
    if (priority) return true;   // let RmlUi's own bindings go first
    const bool ctrl = (modifier & Rml::Input::KM_CTRL) != 0;
    if (ctrl && key == Rml::Input::KI_S) {
        save();
        return false;            // handled; don't type an 's' into the editor
    }
    (void)context;
    return true;
}

/* --- the editor's geometry ------------------------------------------------ */

/// One glyph's width in the editor, measured from its font rather than
/// assumed: the face is whichever monospace font the machine had, and a
/// column landed one glyph off for every guess made here.
int code_char_width() {
    static int width = 0;
    if (width > 0) return width;
    Rml::Element* ed = by_id("fullcode");
    if (!ed) return 8;
    const Rml::FontFaceHandle face = ed->GetFontFaceHandle();
    if (!face) return 8;
    const Rml::String language;
    const Rml::TextShapingContext shaping{language};
    width = Rml::GetFontEngineInterface()->GetStringWidth(face, "M", shaping);
    return width > 0 ? width : 8;
}

/// The source line and column under screen point (mx,my), 0-based, or false
/// when the point is not over the editor's text.
bool editor_position_at(int mx, int my, int& line, int& col) {
    Rml::Element* ed = by_id("fullcode");
    if (!ed || g.view != "code") return false;
    const auto at = ed->GetAbsoluteOffset(Rml::BoxArea::Border);
    const auto size = ed->GetBox().GetSize(Rml::BoxArea::Border);
    if (mx < at.x || my < at.y || mx >= at.x + size.x || my >= at.y + size.y) return false;
    line = ((int)(my - at.y) + g.code_scroll) / theme::CODE_LINE_H;
    col = ((int)(mx - at.x) - theme::CODE_PAD_X + g.code_scroll_x) / code_char_width();
    if (col < 0) col = 0;
    return true;
}

/// The caret's line and column, 0-based, from the editor's selection.
bool caret_position(int& line, int& col) {
    auto* ta = dynamic_cast<Rml::ElementFormControlTextArea*>(by_id("fullcode"));
    if (!ta) return false;
    int start = 0, end = 0;
    Rml::String selected;
    ta->GetSelection(&start, &end, &selected);
    const Rml::String text = ta->GetValue();
    // The selection is reported in code points and the text is bytes. A
    // comment with an em dash in it put every caret after it two columns
    // off — and a completion accepted there replaced the wrong span.
    const int bytes = Rml::StringUtilities::ConvertCharacterOffsetToByteOffset(text, start);
    line = 0;
    col = 0;
    for (int i = 0; i < bytes && i < (int)text.size(); i++) {
        const unsigned char c = (unsigned char)text[(size_t)i];
        if (c == '\n') { line++; col = 0; }
        else if ((c & 0xC0) != 0x80) col++;
    }
    return true;
}

/// Put the caret at a line and column and bring it into view. The offset is
/// a character index into the editor's text, which is what the control's
/// selection API takes.
void jump_to(int line, int col) {
    auto* ta = dynamic_cast<Rml::ElementFormControlTextArea*>(by_id("fullcode"));
    if (!ta) return;
    const Rml::String text = ta->GetValue();
    // The control's selection counts code points, not bytes: see
    // caret_position.
    const int offset = Rml::StringUtilities::ConvertByteOffsetToCharacterOffset(
        text, (int)byte_offset(text, line, col));
    // Focus first: the control ignores a selection set while it is not
    // focused, silently.
    ta->Focus();
    ta->SetSelectionRange(offset, offset);
    // A third of the way down rather than at the top edge: the lines above a
    // definition are the context it was written in.
    Rml::Element* view = by_id("codeview");
    const int viewport = view ? (int)view->GetBox().GetSize().y : 400;
    g.code_scroll = std::max(0, line * theme::CODE_LINE_H - viewport / 3);
    sync_highlight_scroll();
}

/// Where `sub <name>` is declared, as a 0-based line, or -1.
int line_of_sub(const std::string& text, const std::string& name) {
    int line = 0;
    size_t start = 0;
    while (start <= text.size()) {
        const size_t nl = text.find('\n', start);
        const std::string l = text.substr(start, nl == std::string::npos ? std::string::npos : nl - start);
        if (l.rfind("sub " + name, 0) == 0) {
            const char after = l.size() > 4 + name.size() ? l[4 + name.size()] : '\0';
            if (after == '\0' || after == '(' || after == ' ' || after == ':') return line;
        }
        if (nl == std::string::npos) break;
        start = nl + 1;
        line++;
    }
    return -1;
}

/// The subroutine the caret is inside, or "" when it is in none.
///
/// The names and their lines come from the server's index, which is the one
/// place that knows what a `sub` is; the index does not say where a block
/// ends, so the closing `end` is found in the text, the way the preview pane
/// finds it. Before the first answer arrives — or with no server at all —
/// the text is scanned for the names too, so the label never waits.
std::string enclosing_sub() {
    if (g.view != "code") return "";
    int line = 0, col = 0;
    if (!caret_position(line, col)) return "";
    auto* ed = code_editor();
    if (!ed) return "";
    const std::string text = ed->GetValue();
    std::string name;
    int start = -1;
    if (!g.symbols.empty()) {
        for (const auto& sym : g.symbols) {
            if (sym.is_sub && sym.line <= line && sym.line > start) {
                start = sym.line;
                name = sym.name;
            }
        }
    } else {
        int n = 0;
        for (size_t at = 0; at <= text.size() && n <= line; n++) {
            const size_t nl = text.find('\n', at);
            const std::string l = text.substr(at, nl == std::string::npos ? std::string::npos : nl - at);
            if (l.rfind("sub ", 0) == 0) {
                start = n;
                name = l.substr(4, l.find_first_of(" (:\t", 4) - 4);
            }
            if (nl == std::string::npos) break;
            at = nl + 1;
        }
    }
    if (start < 0) return "";
    // Inside, only up to the block's own `end`: the caret in the blank line
    // between two subroutines is in neither of them.
    int n = 0;
    for (size_t at = 0; at <= text.size(); n++) {
        const size_t nl = text.find('\n', at);
        const std::string l = text.substr(at, nl == std::string::npos ? std::string::npos : nl - at);
        if (n > start && l == "end") return line <= n ? name : "";
        if (nl == std::string::npos) break;
        at = nl + 1;
    }
    return name;
}

/// Label the document tabs with what they hold: the file, and — for the Code
/// tab while the caret is in a subroutine — that subroutine. Written only
/// when the words change; this runs every frame.
void update_tabs() {
    const std::string file = basename_of(g.model.path);
    const std::string sub = enclosing_sub();
    const std::string designer = "Designer <span class='tabfile'>[" + esc(file) + "]</span>";
    const std::string code = "Code <span class='tabfile'>[" + esc(sub.empty() ? file : sub) + "]</span>";
    if (designer != g.tab_designer_label) {
        g.tab_designer_label = designer;
        if (Rml::Element* e = by_id("tabdesigner")) e->SetInnerRML(designer);
    }
    if (code != g.tab_code_label) {
        g.tab_code_label = code;
        if (Rml::Element* e = by_id("tabcode")) e->SetInnerRML(code);
    }
}

/* --- hover, definition, references ----------------------------------------- */

void hide_tip() {
    if (Rml::Element* tip = by_id("tip")) tip->SetProperty("display", "none");
}

/// Show what the server said about the name under the pointer. The answer is
/// markdown with a fenced signature first; the fences are dropped and the
/// rest — "command", "subroutine", "declared on line 4" — is set in the
/// chrome's face under it.
/* --- Help > About ---------------------------------------------------------- */

/// A link, in the user's browser. Not from a scripted session: a test that
/// opened a browser on the machine running it would be a test nobody runs
/// twice.
void open_url(const std::string& url) {
    if (std::getenv("KILN_DESIGNER_SCRIPT")) {
        std::printf("about: open %s\n", url.c_str());
        std::fflush(stdout);
        return;
    }
    if (!kiln::sys::open_url(url)) set_status("could not open " + url);
}

std::string about_markup() {
    const int W = g.win_w, H = g.win_h;
    const std::string icon = asset_path("kiln-icon-64.png");
    const std::string mark = asset_path("kiln-wordmark.png");
    std::string version = kiln::welcome::version_string(g.kiln_bin);
    if (version.empty()) version = "kiln";
    std::ostringstream s;
    s << "<rml><head><style>";
    s << "div{display:block}span{display:inline}";
    // The body is the click-outside target, so it has to be the window's
    // size: an unsized body is 0x0 and no click ever lands on it.
    s << "body{position:absolute;left:0;top:0;width:" << W << "px;height:" << H
      << "px;font-family:'" << g.family << "';background-color:#0000001c}";
    s << "#about{position:absolute;left:" << (W - 480) / 2 << "px;top:" << (H - 400) / 2
      << "px;width:440px;padding:16px 20px 20px 20px;background-color:#ffffff;"
         "border-radius:12px;box-shadow:#00000038 0 14px 40px 0px}";
    s << "#ttl{height:20px;font-size:12px;color:#1F2328;margin-bottom:16px}";
    s << "#ttl img{width:16px;height:16px;vertical-align:-4px;margin-right:6px}";
    s << "#x{position:absolute;right:18px;top:12px;width:22px;height:22px;text-align:center;"
         "padding-top:2px;font-size:13px;color:#57606A;border-radius:4px;cursor:pointer}";
    s << "#x:hover{background-color:#F3F4F6;color:#1F2328}";
    s << "#hero{position:relative;height:64px}";
    s << "#hero img.big{position:absolute;left:0;top:0;width:64px;height:64px}";
    // The wordmark is 1100x224; 265px wide keeps its proportions at a height
    // that sits beside the icon.
    s << "#hero img.mark{position:absolute;left:80px;top:5px;width:265px;height:54px}";
    // The weights are the design's. RmlUi has no synthesis, but it does pick
    // the nearest face it was given — the regular and bold Studio loads —
    // so a machine whose family ships a medium or semibold face gets them.
    s << "#ver{margin-top:8px;font-size:14px;font-weight:600;color:#1F2328}";
    s << "#tag{margin-top:10px;font-size:12.5px;font-weight:500;line-height:1.4;color:#1F2328}";
    s << "#her{margin-top:10px;font-size:11.5px;line-height:1.4;color:#57606A}";
    s << "#pills{margin-top:14px}";
    s << ".pill{display:inline-block;height:24px;padding:3px 10px 0 10px;margin-right:6px;"
         "margin-bottom:6px;border-radius:12px;background-color:#F3F4F6;border:1px #E5E7EB;"
         "font-size:11px;font-weight:500;color:#374151}";
    // Kiln heat, not the indigo this predates the brand with. 4.88:1 on its own
    // tint, so the pill that says what Kiln is can actually be read.
    s << ".pill.accent{background-color:#fff7ed;border:1px #fed7aa;font-weight:600;color:"
      << theme::ACCENT << "}";
    s << "#foot{margin-top:10px;font-size:11px;color:#6E7781}";
    s << "#links{margin-top:6px;margin-bottom:16px;font-size:11.5px;font-weight:600;color:#0969DA}";
    s << "#links span.l{cursor:pointer}#links span.l:hover{text-decoration:underline}";
    s << "#links span.pipe{color:#D0D7DE;padding:0 8px 0 8px}";
    s << "#rule{height:1px;background-color:#E5E7EB;margin-bottom:12px}";
    s << "#ok{position:absolute;right:20px;bottom:20px;width:75px;height:30px;padding-top:7px;"
         "text-align:center;font-size:12px;color:#1F2328;background-color:#ffffff;border:1px #D0D7DE;"
         "border-radius:5px;cursor:pointer}";
    s << "#ok:hover{background-color:#F3F4F6}";
    s << "#okrow{height:30px}";
    s << "</style></head><body id='about-bg'><div id='about'>";
    s << "<div id='ttl'>" << (icon.empty() ? "" : "<img src='" + icon + "'/>")
      << "About Kiln Studio</div><div id='x' oe-about='close'>\u2715</div>";
    s << "<div id='hero'>" << (icon.empty() ? "" : "<img class='big' src='" + icon + "'/>")
      << (mark.empty() ? "" : "<img class='mark' src='" + mark + "'/>") << "</div>";
    s << "<div id='ver'>" << esc(version) << "</div>";
    s << "<div id='tag'>Visual builder for real desktop apps. Draw a window, wire a handler, "
         "hit Run to a small native binary.</div>";
    s << "<div id='her'>Open, cross-platform heir to the VB6 / Delphi RAD tradition. "
         "Built from EPL and BlackMoon concepts.</div>";
    s << "<div id='pills'>";
    for (const char* t : {"Clean native binary", "IR to LLVM to linker", "IR never ships",
                          "Radical ease"}) {
        s << "<div class='pill'>" << t << "</div>";
    }
    s << "<div class='pill accent'>RAD is the identity</div></div>";
    // Literal UTF-8: RmlUi prints an entity it does not know verbatim.
    // The holder and both licence arms, exactly as LICENSE states them. This
    // is the one place a user reads the licence rather than the repository, so
    // naming a different holder — or dropping the BSD arm — understates the
    // rights they actually have.
    s << "<div id='foot'>\u00a9 2026 The Kiln Contributors. MIT OR BSD-3-Clause.</div>";
    s << "<div id='links'><span class='l' id='GitHub-link' oe-url='https://github.com/AxDSan/kiln'>GitHub</span>"
         "<span class='pipe'>|</span>"
         "<span class='l' oe-url='https://axdsan.github.io/kiln/'>Docs</span></div>";
    s << "<div id='rule'/><div id='okrow'/><div id='ok' oe-about='close'>OK</div>";
    s << "</div></body></rml>";
    return s.str();
}

void close_about() {
    if (!g.about) return;
    // Unloading is deferred to the context's next Update, so closing from
    // inside the dialog's own listener is safe.
    g.about->Close();
    g.about = nullptr;
    if (g.doc) g.doc->Focus();
}

/// OK, the cross, Escape and a click on the backdrop all dismiss; a link
/// opens and leaves the dialog up.
struct AboutListener : Rml::EventListener {
    void ProcessEvent(Rml::Event& ev) override {
        if (ev.GetType() == "keydown") {
            if (ev.GetParameter<int>("key_identifier", 0) == Rml::Input::KI_ESCAPE) close_about();
            return;
        }
        for (Rml::Element* e = ev.GetTargetElement(); e; e = e->GetParentNode()) {
            if (e->HasAttribute("oe-url")) {
                open_url(e->GetAttribute<Rml::String>("oe-url", ""));
                return;
            }
            if (e->HasAttribute("oe-about")) { close_about(); return; }
            if (e->GetId() == "about") return;      // inside the dialog: nothing
        }
        close_about();
    }
} g_about_listener;

void show_about() {
    if (g.about) return;
    g.about = g.context->LoadDocumentFromMemory(about_markup());
    if (!g.about) return;
    g.about->Show(Rml::ModalFlag::Modal, Rml::FocusFlag::Document);
    g.about->AddEventListener("click", &g_about_listener);
    g.about->AddEventListener("keydown", &g_about_listener);
}

/* --- Help > Documentation -------------------------------------------------- */

/// The handbook, in Studio.
///
/// PureBasic's help is the model: offline, one keystroke away, and F1 on a
/// keyword lands on that keyword rather than on a search box. The pages are
/// the Markdown the bundle ships in `docs/src/`, which `markdown.h` renders —
/// there is no second copy of the prose here, and the command reference is
/// generated from the toolchain, so what Studio shows is what the compiler has.

void close_help() {
    if (!g.help) return;
    // Unloading is deferred to the context's next Update, so closing from
    // inside the dialog's own listener is safe.
    g.help->Close();
    g.help = nullptr;
    if (g.doc) g.doc->Focus();
}

void open_help(const std::string& page, const std::string& anchor, const std::string& query);

/// A click navigates, follows a link, searches, or dismisses. Typing in the
/// search box re-renders — the results are the body, so there is one document
/// and one close path rather than two of each.
struct HelpListener : Rml::EventListener {
    void ProcessEvent(Rml::Event& ev) override {
        if (ev.GetType() == "keydown") {
            if (ev.GetParameter<int>("key_identifier", 0) == Rml::Input::KI_ESCAPE) close_help();
            return;
        }
        if (ev.GetType() == "change") {
            Rml::Element* src = ev.GetTargetElement();
            if (src && src->GetId() == "find")
                open_help(g.help_page, "", src->GetAttribute<Rml::String>("value", ""));
            return;
        }
        for (Rml::Element* e = ev.GetTargetElement(); e; e = e->GetParentNode()) {
            if (e->HasAttribute("oe-url")) {
                open_url(e->GetAttribute<Rml::String>("oe-url", ""));
                return;
            }
            if (e->HasAttribute("oe-help-copy")) {
                const int n = e->GetAttribute<int>("oe-help-copy", -1);
                if (n >= 0 && n < (int)g.help_code.size()) {
                    Rml::GetSystemInterface()->SetClipboardText(g.help_code[(size_t)n]);
                    set_status("example copied");
                    if (std::getenv("KILN_DESIGNER_SCRIPT")) {
                        std::printf("helpcopy: %d bytes\n", (int)g.help_code[(size_t)n].size());
                        std::fflush(stdout);
                    }
                }
                return;
            }
            if (e->HasAttribute("oe-help-select")) {
                const int n = e->GetAttribute<int>("oe-help-select", -1);
                if (n >= 0 && n < (int)g.help_code.size()) {
                    if (Rml::Element* blk = g.help->GetElementById("mdcode-" + std::to_string(n))) {
                        // Swapped in place, so the page does not move. The
                        // code goes in as an attribute value rather than as
                        // markup: a sample is full of `<` and `&`, and one
                        // that is escaped as RML and then read back as text
                        // is a sample that no longer compiles.
                        Rml::ElementPtr ta = g.help->CreateElement("textarea");
                        ta->SetClass("mdsel", true);
                        ta->SetId("mdsel-" + std::to_string(n));
                        ta->SetAttribute("value", g.help_code[(size_t)n]);
                        ta->SetAttribute("rows", 1 + (int)std::count(g.help_code[(size_t)n].begin(),
                                                                     g.help_code[(size_t)n].end(),
                                                                     '\n'));
                        ta->SetAttribute("wrap", "nowrap");
                        blk->SetInnerRML("");
                        Rml::Element* live = blk->AppendChild(std::move(ta));
                        if (live) live->Focus();
                        set_status("select the code, then Ctrl+C");
                        if (std::getenv("KILN_DESIGNER_SCRIPT")) {
                            std::printf("helpselect: %d\n", n);
                            std::fflush(stdout);
                        }
                    }
                }
                return;
            }
            if (e->HasAttribute("oe-help-page")) {
                open_help(e->GetAttribute<Rml::String>("oe-help-page", ""),
                          e->GetAttribute<Rml::String>("oe-help-anchor", ""), "");
                return;
            }
            if (e->HasAttribute("oe-help")) {
                const Rml::String what = e->GetAttribute<Rml::String>("oe-help", "");
                if (what == "browser") {
                    const std::string index = kiln::designer::help_page::book_index();
                    if (!index.empty()) open_url("file://" + index);
                    return;
                }
                close_help();
                return;
            }
            if (e->GetId() == "dlg") return;        // inside the dialog: nothing
            if (e->GetId() == "find") return;
        }
        close_help();
    }
} g_help_listener;

/// A code block swapped to its selectable form is a real textarea, and a real
/// textarea can be typed into. The handbook is not editable, so the keys that
/// would change it are stopped in the capture phase — ahead of the widget's
/// own listener, since a bubble-phase one runs after the edit. Navigation,
/// select-all and copy go through, which is the whole point of the swap.
/// Escape still closes the handbook, which it would not if the widget ate it.
struct HelpKeyGate : Rml::EventListener {
    void ProcessEvent(Rml::Event& ev) override {
        Rml::Element* el = ev.GetTargetElement();
        if (!el || el->GetId().rfind("mdsel-", 0) != 0) return;
        if (ev.GetType() == "textinput") { ev.StopImmediatePropagation(); return; }
        const int key = ev.GetParameter<int>("key_identifier", 0);
        const bool ctrl = ev.GetParameter<bool>("ctrl_key", false);
        switch (key) {
        case Rml::Input::KI_LEFT: case Rml::Input::KI_RIGHT: case Rml::Input::KI_UP:
        case Rml::Input::KI_DOWN: case Rml::Input::KI_HOME: case Rml::Input::KI_END:
        case Rml::Input::KI_PRIOR: case Rml::Input::KI_NEXT:
            return;
        case Rml::Input::KI_ESCAPE:
            close_help();
            ev.StopImmediatePropagation();
            return;
        case Rml::Input::KI_A: case Rml::Input::KI_C:
            if (ctrl) return;
            break;
        default: break;
        }
        ev.StopImmediatePropagation();
    }
} g_help_gate;

/// Show `page`, scrolled to `anchor`, or the results for `query`.
void open_help(const std::string& page, const std::string& anchor, const std::string& query) {
    const std::string dir = kiln::designer::help_page::docs_dir();
    if (dir.empty()) {
        set_status("the handbook is not installed beside this build");
        return;
    }
    if (!page.empty()) g.help_page = page;
    g.help_query = query;

    // Rebuilt rather than patched. Closing first keeps exactly one help
    // document alive, so the modal stack cannot deepen on every link click.
    if (g.help) { g.help->Close(); g.help = nullptr; }
    g.help = g.context->LoadDocumentFromMemory(kiln::designer::help_page::markup(
        g.family, g.mono, g.win_w, g.win_h, dir, g.help_page, g.help_query, &g.help_code));
    if (!g.help) return;
    g.help->Show(Rml::ModalFlag::Modal, Rml::FocusFlag::Document);
    g.help->AddEventListener("click", &g_help_listener);
    g.help->AddEventListener("keydown", &g_help_listener);
    g.help->AddEventListener("change", &g_help_listener);
    for (const char* e : {"keydown", "textinput"}) g.help->AddEventListener(e, &g_help_gate, true);

    if (!anchor.empty()) {
        // Lay the document out first. `Show` only queues that for the
        // context's next Update, and an element with no box yet scrolls
        // nowhere — which looks exactly like an anchor that does not exist.
        g.help->UpdateDocument();
        if (Rml::Element* e = g.help->GetElementById(anchor)) e->ScrollIntoView();
    }
    if (std::getenv("KILN_DESIGNER_SCRIPT")) {
        std::printf("help: %s%s%s\n", g.help_query.empty() ? g.help_page.c_str() : "search",
                    anchor.empty() ? "" : "#", anchor.c_str());
        std::fflush(stdout);
    }
}

/// F1. The name under the caret decides the page: a command or a component
/// opens the reference at its own row, and anything else opens the handbook
/// where it was left. A caret sitting just past a word still counts — that is
/// where it lands after typing one.
void context_help() {
    const std::string dir = kiln::designer::help_page::docs_dir();
    if (dir.empty()) { set_status("the handbook is not installed beside this build"); return; }

    int line = 0, col = 0;
    std::string word;
    if (caret_position(line, col)) {
        word = identifier_at(line, col);
        if (word.empty() && col > 0) word = identifier_at(line, col - 1);
    }
    std::string page, anchor;
    if (!word.empty() && kiln::designer::help_page::lookup(dir, word, &page, &anchor)) {
        open_help(page, anchor, "");
        return;
    }
    // A name we do not document is still a question. Searching it beats
    // opening the index and making the user retype what they were looking at.
    if (!word.empty()) { open_help("", "", word); return; }
    open_help(g.help_page, "", "");
}

/* --- Settings -------------------------------------------------------------- */

/// Rebuild the chrome's stylesheet against the palette in force, and hand it to
/// the document.
///
/// `SetStyleSheetContainer` dirties the document's definitions and recurses, so
/// every element re-resolves against the new sheet — this is what makes the
/// theme switch live rather than restart-only. The dot tile is regenerated
/// first: it is a file, and its colour is baked into the pixels.
///
/// The canvas is repainted afterwards because `rebuild_canvas` writes colours
/// inline from the model, and those are not part of the sheet.
void rebuild_styles() {
    if (!g.doc) return;
    // The cache path, spelled out rather than through cache_file(): that is
    // defined further down, beside the startup code that first writes the tile.
    const std::string cache = kiln::sys::cache_dir();
    kiln::sys::make_dirs(cache);
    const std::string tile_path = cache + "/kiln_dotgrid.tga";
    g.dot_tile = write_dot_tile(tile_path, 10, theme::BORDER).empty() ? "" : "/" + tile_path;
    g.doc->SetStyleSheetContainer(
        Rml::Factory::InstanceStyleSheetString(build_styles(g.family, g.mono, g.dot_tile)));
    g.context->Update();
    relayout();
    rebuild_canvas();
    refresh_all();
    if (g.view == "code") refresh_highlight();
}

void close_settings() {
    if (!g.settings_doc) return;
    g.settings_doc->Close();
    g.settings_doc = nullptr;
    if (g.doc) g.doc->Focus();
}

void open_settings(const std::string& category);

/// Put a setting into effect now, for the settings that can be.
///
/// A row marked `restart` deliberately does nothing here: it said so on its
/// own row, and doing half of it — a new font size with the old line height —
/// would look like a bug rather than like a setting waiting for a relaunch.
void apply_setting(const std::string& key) {
    if (key == "appearance.theme") {
        theme::set_palette(kiln::settings::text("appearance.theme") == "dark");
        rebuild_styles();
        // The dialog is its own document with its own baked stylesheet, so the
        // IDE behind it going dark leaves it light. It has to be built again.
        if (g.settings_doc) open_settings(g.settings_cat);
    } else if (key == "designer.show_grid") {
        if (Rml::Element* e = by_id("canvasarea")) {
            e->SetProperty("decorator",
                           kiln::settings::boolean("designer.show_grid") && !g.grid_decorator.empty()
                               ? g.grid_decorator
                               : std::string("none"));
        }
    }
    kiln::settings::save();
}

/// Commit a text field. Called on blur and on Enter — never on `change`, which
/// RmlUi fires per keystroke: a grid size would pass through 0 on the way from
/// 10 to 20, and 0 reaches an integer division in `snap`.
///
/// A value the schema refuses is put back to what it was rather than kept, so
/// the field never shows something that is not in force.
void commit_field(Rml::Element* e) {
    if (!e || !e->HasAttribute("oe-set-field")) return;
    const std::string key = e->GetAttribute<Rml::String>("oe-set-field", "");
    auto* in = dynamic_cast<Rml::ElementFormControl*>(e);
    if (!in) return;
    const std::string typed = in->GetValue();
    if (typed == kiln::settings::text(key)) return;
    if (kiln::settings::set(key, typed)) {
        apply_setting(key);
        set_status(key + " = " + typed);
        open_settings(g.settings_cat);   // the modified marker and Reset appear
    } else {
        in->SetValue(kiln::settings::text(key));
        const kiln::settings::Row* r = kiln::settings::find(key);
        set_status(r && r->kind == kiln::settings::Kind::Int
                       ? key + " must be between " + std::to_string(r->min) + " and " +
                             std::to_string(r->max)
                       : std::string("that is not a value ") + key + " accepts");
    }
}

struct SettingsListener : Rml::EventListener {
    void ProcessEvent(Rml::Event& ev) override {
        const std::string type = ev.GetType();
        if (type == "blur") { commit_field(ev.GetTargetElement()); return; }
        if (type == "keydown") {
            const int key = ev.GetParameter<int>("key_identifier", 0);
            if (key == Rml::Input::KI_RETURN || key == Rml::Input::KI_NUMPADENTER) {
                commit_field(g.context->GetFocusElement());
                return;
            }
            // Escape commits nothing: a half-typed field is abandoned, which
            // is what Escape means everywhere else.
            if (key == Rml::Input::KI_ESCAPE) close_settings();
            return;
        }
        for (Rml::Element* e = ev.GetTargetElement(); e; e = e->GetParentNode()) {
            if (e->HasAttribute("oe-set-close")) { close_settings(); return; }
            if (e->HasAttribute("oe-set-cat")) {
                open_settings(e->GetAttribute<Rml::String>("oe-set-cat", ""));
                return;
            }
            if (e->HasAttribute("oe-set-reset")) {
                const std::string key = e->GetAttribute<Rml::String>("oe-set-reset", "");
                kiln::settings::reset(key);
                apply_setting(key);
                set_status(key + " reset");
                open_settings(g.settings_cat);
                return;
            }
            if (e->HasAttribute("oe-set-key")) {
                const std::string key = e->GetAttribute<Rml::String>("oe-set-key", "");
                const std::string val = e->GetAttribute<Rml::String>("oe-set-val", "");
                if (kiln::settings::set(key, val)) {
                    apply_setting(key);
                    set_status(key + " = " + val);
                    open_settings(g.settings_cat);
                }
                return;
            }
            if (e->GetId() == "openfile") {
                std::printf("settings: file %s\n", kiln::settings::path().c_str());
                std::fflush(stdout);
                return;
            }
            if (e->GetId() == "dlg") return;   // a click inside keeps it open
        }
        close_settings();                      // ...one outside does not
    }
} g_settings_listener;

/// Open the dialog, or re-render it in place.
///
/// Re-rendering the whole document is what makes a chip light up and a Reset
/// link appear the moment a value changes; it is safe here in a way it is not
/// in the inspector, because every commit path runs after the field has
/// already lost focus or taken Enter — never with the caret inside a control
/// that is about to be destroyed.
void open_settings(const std::string& category) {
    g.settings_cat = category.empty() ? std::string("Appearance") : category;
    const bool reopening = g.settings_doc != nullptr;
    if (reopening) { g.settings_doc->Close(); g.settings_doc = nullptr; g.context->Update(); }
    g.settings_doc = g.context->LoadDocumentFromMemory(
        kiln::designer::settings_page::markup(g.family, g.win_w, g.win_h, g.settings_cat));
    if (!g.settings_doc) return;
    g.settings_doc->Show(Rml::ModalFlag::Modal, Rml::FocusFlag::Document);
    g.settings_doc->AddEventListener("click", &g_settings_listener);
    g.settings_doc->AddEventListener("keydown", &g_settings_listener);
    // Blur bubbles only when asked for: it is not a bubbling event in RmlUi,
    // so the listener is registered in the capture phase to see every field's.
    g.settings_doc->AddEventListener("blur", &g_settings_listener, true);
    if (!reopening) set_status("settings");
}

void show_tip(const std::string& markdown, int x, int y) {
    Rml::Element* tip = by_id("tip");
    if (!tip) return;
    // The blocks in the order the server sent them. A command's hover is now
    // three of them — signature, sentence, example — and collapsing every
    // fence into one string would print the example above the sentence that
    // explains it.
    struct Block { bool code; bool kiln; std::string text; };
    std::vector<Block> blocks;
    bool in_code = false, is_kiln = false;
    size_t start = 0;
    while (start <= markdown.size()) {
        const size_t nl = markdown.find('\n', start);
        const std::string l =
            markdown.substr(start, nl == std::string::npos ? std::string::npos : nl - start);
        if (l.rfind("```", 0) == 0) {
            in_code = !in_code;
            if (in_code) {
                is_kiln = l.substr(3) == "kiln";
                blocks.push_back({true, is_kiln, ""});
            }
        } else if (!l.empty() || in_code) {
            if (!in_code && (blocks.empty() || blocks.back().code))
                blocks.push_back({false, false, ""});
            if (in_code && blocks.empty()) blocks.push_back({true, is_kiln, ""});
            std::string& into = blocks.back().text;
            into += (into.empty() ? "" : "\n") + l;
        }
        if (nl == std::string::npos) break;
        start = nl + 1;
    }

    std::string html;
    for (const Block& b : blocks) {
        if (b.text.empty()) continue;
        if (!b.code) { html += "<div class='tipkind'>" + esc(b.text) + "</div>"; continue; }
        // An example is real Kiln, so it is coloured the way the editor
        // colours it. The stylesheet already carries those classes.
        html += "<div class='tipcode'>";
        size_t at = 0;
        while (at <= b.text.size()) {
            const size_t nl = b.text.find('\n', at);
            const std::string line =
                b.text.substr(at, nl == std::string::npos ? std::string::npos : nl - at);
            html += "<div>" + (b.kiln ? highlight_line(line) : escape_code(line)) + "</div>";
            if (nl == std::string::npos) break;
            at = nl + 1;
        }
        html += "</div>";
    }
    tip->SetInnerRML(html);
    int tx = x + 14, ty = y + 18;
    if (tx > g.win_w - 540) tx = std::max(8, g.win_w - 540);
    if (ty > g.win_h - 120) ty = y - 60;
    tip->SetProperty("left", Rml::String(std::to_string(tx) + "px"));
    tip->SetProperty("top", Rml::String(std::to_string(ty) + "px"));
    tip->SetProperty("display", "block");
}

/// The name at a place in the source, or "" when there is not one there.
///
/// Its own scan rather than the highlighter's: the highlighter answers what a
/// run of characters *is*, and the question here is where one name starts and
/// stops. `$` is deliberately not a name character — the compiler's own
/// invented names contain one, and they are not the user's to hover.
std::string identifier_at(int line, int col) {
    auto* ed = code_editor();
    if (!ed || line < 0) return "";
    const std::string all = ed->GetValue();
    size_t start = 0;
    for (int i = 0; i < line; i++) {
        const size_t nl = all.find('\n', start);
        if (nl == std::string::npos) return "";
        start = nl + 1;
    }
    const size_t end = all.find('\n', start);
    const std::string text = all.substr(start, end == std::string::npos ? std::string::npos
                                                                        : end - start);
    if (col < 0 || (size_t)col >= text.size()) return "";
    auto part = [](char c) { return std::isalnum((unsigned char)c) || c == '_'; };
    if (!part(text[col])) return "";
    size_t from = (size_t)col;
    while (from > 0 && part(text[from - 1])) from--;
    size_t to = (size_t)col;
    while (to + 1 < text.size() && part(text[to + 1])) to++;
    return text.substr(from, to - from + 1);
}

/// Ask the server about the spot the pointer has rested on. Called every
/// frame; sends at most one request per resting place.
void hover_tick() {
    if (g.view != "code" || g.hover_x < 0 || g.hover_asked) return;
    if (now_seconds() - g.hover_moved_at < 0.45) return;
    // Nothing to ask is a settled state too: the frame loop stays awake while
    // a hover is pending, and a pointer resting on the toolbox must not keep
    // it spinning.
    g.hover_asked = true;
    int line = 0, col = 0;
    if (!editor_position_at(g.hover_x, g.hover_y, line, col)) return;
    // While the program is stopped, hovering a name shows what it holds. That
    // is the thing people ask a debugger for more than any other, and it costs
    // nothing here: the values were fetched when it stopped, so there is no
    // round trip to wait for and no chance of the answer arriving after the
    // pointer has moved on.
    if (g.stopped_line > 0) {
        const std::string word = identifier_at(line, col);
        if (!word.empty()) {
            for (const auto& v : g.dbg.locals()) {
                if (v.name == word) {
                    show_tip(word + " = " + v.value, g.hover_x, g.hover_y);
                    return;
                }
            }
        }
    }
    if (!g.lsp.running()) return;
    g.hover_line = line;
    g.hover_col = col;
    g.hover_request = g.lsp.hover(line, col);
    g.hover_shown_for = g.hover_request;
}

/// The list of places a name is used, in the Problems strip. Each row jumps.
void render_references() {
    Rml::Element* box = by_id("problems");
    if (!box) return;
    std::string html;
    const std::string text = code_editor() ? std::string(code_editor()->GetValue()) : "";
    for (size_t i = 0; i < g.refs.size(); i++) {
        const auto& r = g.refs[i].range;
        std::string line_text;
        size_t start = 0;
        for (int n = 0; n < r.line && start != std::string::npos; n++) {
            start = text.find('\n', start);
            if (start != std::string::npos) start++;
        }
        if (start != std::string::npos) {
            const size_t nl = text.find('\n', start);
            line_text = text.substr(start, nl == std::string::npos ? std::string::npos : nl - start);
        }
        while (!line_text.empty() && line_text[0] == ' ') line_text.erase(0, 1);
        html += "<div class='problem ref' oe-ref='" + std::to_string(i) + "'><span class='pline'>line " +
                std::to_string(r.line + 1) + ":" + std::to_string(r.character + 1) + "</span>" +
                esc(line_text) + "</div>";
    }
    if (g.refs.empty()) html = "<div class='noproblems'>No references.</div>";
    box->SetInnerRML(html);
    if (Rml::Element* tab = by_id("problemcount")) {
        tab->SetInnerRML("REFERENCES (" + std::to_string(g.refs.size()) + ")");
    }
    size_output_pane();
}

/// Definition at the caret, and jump to it.
void goto_definition() {
    int line = 0, col = 0;
    if (g.view != "code" || !caret_position(line, col)) return;
    g.def_request = g.lsp.definition(line, col);
    if (!g.def_request) set_status("no language server");
}

/// Every use of the name at the caret, listed in the Problems strip.
void find_references() {
    int line = 0, col = 0;
    if (g.view != "code" || !caret_position(line, col)) return;
    g.refs_request = g.lsp.references(line, col);
    if (!g.refs_request) set_status("no language server");
}

/// Deliver whatever the server has answered. Once per frame; a reply that has
/// not come yet is simply not there, and the UI never waits for it.
void filter_completion();
void poll_answers() {
    kiln::json::Value v;
    if (!g.lsp.running()) {
        // A server that has gone leaves no answers to wait for — and no
        // symbols to ask for: a flag left waiting for it keeps the frame
        // loop awake, since idle() counts it as an answer on its way.
        g.hover_request = g.def_request = g.refs_request = g.symbol_request = 0;
        g.symbols_stale = false;
        return;
    }
    if (g.hover_request && g.lsp.take_response(g.hover_request, v)) {
        // The pointer may have moved on since; the answer is for where it
        // rested, and only shown if it is still there.
        const bool current = g.hover_request == g.hover_shown_for;
        g.hover_request = 0;
        const std::string text = kiln::lsp::hover_text(v);
        if (!text.empty() && current) show_tip(text, g.hover_x, g.hover_y);
    }
    if (g.def_request && g.lsp.take_response(g.def_request, v)) {
        g.def_request = 0;
        const auto locs = kiln::lsp::read_locations(v);
        if (locs.empty()) {
            // A command is declared in C, not in this file: there is nowhere
            // to go, and hover already shows what a jump would have shown.
            set_status("no definition in this file — hover shows the signature");
        } else {
            jump_to(locs[0].range.line, locs[0].range.character);
            set_status("line " + std::to_string(locs[0].range.line + 1));
        }
    }
    if (g.refs_request && g.lsp.take_response(g.refs_request, v)) {
        g.refs_request = 0;
        g.refs = kiln::lsp::read_locations(v);
        render_references();
        set_status(std::to_string(g.refs.size()) + " reference(s)");
    }
    if (g.symbol_request && g.lsp.take_response(g.symbol_request, v)) {
        g.symbol_request = 0;
        g.symbols = kiln::lsp::read_symbols(v);
        update_tabs();
    }
    if (g.symbols_stale && !g.symbol_request) {
        g.symbols_stale = false;
        g.symbol_request = g.lsp.document_symbols();
    }
    if (g.complete_request && g.lsp.take_response(g.complete_request, v)) {
        g.complete_request = 0;
        // A list or a CompletionList; the server sends the former, the
        // protocol allows either.
        const kiln::json::Value& items =
            v.kind == kiln::json::Value::Kind::Array ? v : v["items"];
        g.complete_all.clear();
        for (size_t i = 0; i < items.size(); i++) g.complete_all.push_back(items.at(i));
        filter_completion();
    }
}

/* --- completion ------------------------------------------------------------- */

bool ident_char(char c) {
    return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') || c == '_';
}

/// Line `line` of `text`, 0-based, without its newline.
std::string line_text(const std::string& text, int line) {
    size_t start = 0;
    for (int n = 0; n < line; n++) {
        start = text.find('\n', start);
        if (start == std::string::npos) return "";
        start++;
    }
    const size_t nl = text.find('\n', start);
    return text.substr(start, nl == std::string::npos ? std::string::npos : nl - start);
}

/// Byte offset of (line, character) in `text`. The protocol counts UTF-16
/// units and the editor's selection counts code points; both agree with
/// code points for everything but an astral-plane glyph, so that is what is
/// counted here.
size_t byte_offset(const std::string& text, int line, int character) {
    size_t at = 0;
    for (int n = 0; n < line && at < text.size(); n++) {
        at = text.find('\n', at);
        if (at == std::string::npos) return text.size();
        at++;
    }
    for (int c = 0; c < character && at < text.size() && text[at] != '\n'; c++) {
        at++;
        while (at < text.size() && ((unsigned char)text[at] & 0xC0) == 0x80) at++;
    }
    return at;
}

void close_completion() {
    g.complete_open = false;
    g.complete_request = 0;
    g.complete_all.clear();
    g.complete_shown.clear();
    if (Rml::Element* pop = by_id("complete")) pop->SetProperty("display", "none");
}

/// Draw the list under the word being completed. The editor's glyph metrics
/// place it: a popup that floats somewhere near the caret rather than at it
/// reads as a dialog, not as a completion.
void render_completion() {
    Rml::Element* pop = by_id("complete");
    Rml::Element* ed = by_id("fullcode");
    if (!pop || !ed || g.complete_shown.empty()) return;
    // A window of rows around the selection: the server offers every
    // command there is, and a list taller than the editor is not a list.
    constexpr size_t WINDOW = 8;
    constexpr int ROW_H = 22;
    const size_t n = g.complete_shown.size();
    const size_t idx = (size_t)g.complete_index;
    size_t first = 0;
    if (n > WINDOW) {
        first = idx > WINDOW / 2 ? idx - WINDOW / 2 : 0;
        if (first + WINDOW > n) first = n - WINDOW;
    }
    std::string html;
    for (size_t i = first; i < n && i < first + WINDOW; i++) {
        const kiln::json::Value& it = g.complete_all[g.complete_shown[i]];
        html += "<div class='citem" + std::string(i == idx ? " sel" : "") + "' oe-citem='" +
                std::to_string(i) + "'><span class='clabel'>" + esc(it["label"].str()) +
                "</span><span class='cdetail'>" + esc(it["detail"].str()) + "</span></div>";
    }
    pop->SetInnerRML(html);
    const auto at = ed->GetAbsoluteOffset(Rml::BoxArea::Border);
    const int rows = (int)std::min(n, WINDOW);
    const int h = rows * ROW_H + 10;
    int x = (int)at.x + theme::CODE_PAD_X + g.complete_start * code_char_width() - g.code_scroll_x;
    int y = (int)at.y + (g.complete_line + 1) * theme::CODE_LINE_H - g.code_scroll;
    if (x > g.win_w - 300) x = std::max(8, g.win_w - 300);
    // Above the line when there is no room below it; never off the window.
    if (y + h > g.win_h - theme::STATUS_H) {
        y = (int)at.y + g.complete_line * theme::CODE_LINE_H - g.code_scroll - h;
    }
    pop->SetProperty("left", Rml::String(std::to_string(x) + "px"));
    pop->SetProperty("top", Rml::String(std::to_string(std::max(0, y)) + "px"));
    pop->SetProperty("display", "block");
}

/// Narrow what the server offered to the word as it now stands, and show it
/// — or dismiss the popup when nothing matches or the caret has left the
/// word. Called on the reply and after every keystroke while it is up.
void filter_completion() {
    int line = 0, col = 0;
    auto* ed = code_editor();
    if (!ed || !caret_position(line, col)) { close_completion(); return; }
    if (line != g.complete_line || col < g.complete_start) { close_completion(); return; }
    const std::string lt = line_text(ed->GetValue(), line);
    const std::string prefix = lt.substr((size_t)g.complete_start, (size_t)(col - g.complete_start));
    for (char c : prefix) {
        if (!ident_char(c)) { close_completion(); return; }
    }
    auto lower = [](std::string v) {
        for (char& c : v) c = (char)std::tolower((unsigned char)c);
        return v;
    };
    const std::string want = lower(prefix);
    g.complete_shown.clear();
    for (size_t i = 0; i < g.complete_all.size(); i++) {
        const std::string label = lower(g.complete_all[i]["label"].str());
        if (label.compare(0, want.size(), want) == 0) g.complete_shown.push_back(i);
    }
    if (g.complete_shown.empty()) { close_completion(); return; }
    g.complete_index = 0;
    g.complete_open = true;
    render_completion();
}

/// Ask the server what could go at the caret. The answer arrives through
/// poll_answers, like every other answer; nothing here waits for it.
void request_completion(bool manual) {
    int line = 0, col = 0;
    if (g.view != "code" || !caret_position(line, col)) return;
    if (!g.lsp.running()) {
        if (manual) set_status("no language server");
        return;
    }
    auto* ed = code_editor();
    if (!ed) return;
    const std::string lt = line_text(ed->GetValue(), line);
    int start = col;
    while (start > 0 && start <= (int)lt.size() && ident_char(lt[(size_t)start - 1])) start--;
    g.complete_line = line;
    g.complete_start = start;
    g.complete_request = g.lsp.completion(line, col);
}

/// After a keystroke in the editor: keep an open popup honest, or decide
/// whether this keystroke opens one. An identifier character, a `.`, `on `
/// and the space after `on <event>:` all do — the last is the handler
/// position, where accepting the offer writes the subroutine.
void completion_on_change() {
    int line = 0, col = 0;
    auto* ed = code_editor();
    if (!ed || !caret_position(line, col)) return;
    const std::string lt = line_text(ed->GetValue(), line);
    if (col > (int)lt.size()) return;
    if (g.complete_open || g.complete_request) {
        // Still inside the word the server was asked about: its answer
        // stands, narrowed. A request still in flight is filtered when it
        // lands, against the word as it is then. Outside it — a space, a
        // dot, another line — the list is for a word that is over, and this
        // keystroke is judged afresh as a trigger below.
        bool inside = line == g.complete_line && col >= g.complete_start;
        for (int i = g.complete_start; inside && i < col; i++) inside = ident_char(lt[(size_t)i]);
        if (inside) {
            if (g.complete_open) filter_completion();
            return;
        }
        close_completion();
    }
    if (col == 0) return;
    const char prev = lt[(size_t)col - 1];
    bool trigger = ident_char(prev) || prev == '.';
    if (prev == ' ') {
        std::string sofar = lt.substr(0, (size_t)col);
        sofar.erase(0, sofar.find_first_not_of(' '));
        trigger = sofar == "on " ||
                  (sofar.rfind("on ", 0) == 0 && sofar.size() > 3 && sofar[sofar.size() - 2] == ':');
    }
    if (trigger) request_completion(false);
}

void move_completion(int delta) {
    if (!g.complete_open || g.complete_shown.empty()) return;
    const int n = (int)g.complete_shown.size();
    g.complete_index = ((g.complete_index + delta) % n + n) % n;
    render_completion();
}

/// Put the chosen item into the editor: its insertText over the word being
/// completed, plus every additionalTextEdit it carries — the create-a-sub
/// offer is one of those, appended at the end of the file. Edits are
/// applied last-first so no edit moves the text an earlier one addresses.
void accept_completion() {
    if (!g.complete_open || g.complete_shown.empty()) return;
    auto* ta = dynamic_cast<Rml::ElementFormControlTextArea*>(by_id("fullcode"));
    int line = 0, col = 0;
    if (!ta || !caret_position(line, col)) return;
    const kiln::json::Value it = g.complete_all[g.complete_shown[(size_t)g.complete_index]];
    const std::string label = it["label"].str();
    const std::string insert = it["insertText"].str(label);

    struct Edit { kiln::lsp::Range range; std::string text; };
    std::vector<Edit> edits;
    edits.push_back({{line, g.complete_start, line, col}, insert});
    const kiln::json::Value& extra = it["additionalTextEdits"];
    for (size_t i = 0; i < extra.size(); i++) {
        Edit e;
        if (kiln::lsp::read_range(extra.at(i)["range"], e.range)) {
            e.text = extra.at(i)["newText"].str();
            edits.push_back(e);
        }
    }
    std::sort(edits.begin(), edits.end(), [](const Edit& a, const Edit& b) {
        return a.range.line != b.range.line ? a.range.line > b.range.line
                                            : a.range.character > b.range.character;
    });
    std::string text = ta->GetValue();
    for (const Edit& e : edits) {
        const size_t from = byte_offset(text, e.range.line, e.range.character);
        const size_t to = byte_offset(text, e.range.end_line, e.range.end_character);
        text.replace(from, to > from ? to - from : 0, e.text);
    }
    // The caret goes after what was inserted. An edit placed before it
    // would shift it; the server only ever appends after the last line.
    const size_t caret_byte = byte_offset(text, line, g.complete_start) + insert.size();
    const int caret = Rml::StringUtilities::ConvertByteOffsetToCharacterOffset(text, (int)caret_byte);
    ta->SetValue(text);
    ta->Focus();
    ta->SetSelectionRange(caret, caret);
    // SetValue raises no change event, so what a keystroke's change handler
    // does has to be done here: mark the text as the truth, repaint the
    // colour layer, and tell the server.
    g.code_dirty = true;
    g.dirty = true;
    refresh_highlight();
    g.lsp.did_change(text);
    set_status(edits.size() > 1 ? "completed " + label + " and wrote its subroutine"
                                : "completed " + label);
    close_completion();
}

/* --- indentation ------------------------------------------------------------ */

/// One level. Two spaces: it is what every example, template and generated
/// sample in the tree is written with, and an editor that disagreed with the
/// files it opens would re-indent them a line at a time.
/// What Tab inserts and what Enter copies after a block opener. A setting;
/// `settings::number` keeps it at 1 or more, because `col % width` divides.
int indent_width() { return kiln::settings::number("editor.indent_size"); }

/// Put `text` in the editor with the caret at character offset `caret`.
///
/// `SetValue` raises no change event, so everything a keystroke's change
/// handler would have done has to be done by hand — the same tail
/// `accept_completion` runs, and the only supported way to rewrite the buffer
/// without the colour layer, the dirty flag and the server falling behind it.
void set_editor_text(const std::string& text, size_t caret_byte) {
    auto* ta = dynamic_cast<Rml::ElementFormControlTextArea*>(by_id("fullcode"));
    if (!ta) return;
    const int caret =
        Rml::StringUtilities::ConvertByteOffsetToCharacterOffset(text, (int)caret_byte);
    ta->SetValue(text);
    ta->Focus();
    ta->SetSelectionRange(caret, caret);
    g.code_dirty = true;
    g.dirty = true;
    refresh_highlight();
    g.lsp.did_change(text);
    g.symbols_stale = true;
}

/// The leading spaces of `line`, as a string.
std::string leading_space(const std::string& line) {
    size_t i = 0;
    while (i < line.size() && (line[i] == ' ' || line[i] == '\t')) i++;
    return line.substr(0, i);
}

/// `line` without leading or trailing blanks.
std::string trimmed(const std::string& line) {
    size_t a = 0, b = line.size();
    while (a < b && std::isspace((unsigned char)line[a])) a++;
    while (b > a && std::isspace((unsigned char)line[b - 1])) b--;
    return line.substr(a, b - a);
}

/// The first word of `line`.
std::string first_word(const std::string& line) {
    const std::string t = trimmed(line);
    const size_t sp = t.find_first_of(" \t(");
    return sp == std::string::npos ? t : t.substr(0, sp);
}

/// Whether a new line after `line` should be indented one level further.
///
/// A few words and a shape, not a parser: the editor has no business deciding
/// what the language means, and a guess that is occasionally one level out is
/// corrected with Tab. What it must never do is indent after an ordinary
/// statement, which is why the one-line forms are excluded by hand.
bool opens_block(const std::string& line) {
    const std::string t = trimmed(line);
    if (t.empty() || t[0] == '#') return false;
    const std::string w = first_word(t);

    // A trailing `end` closes whatever the line opened, all on one line.
    if (t.size() >= 3 && t.compare(t.size() - 3, 3, "end") == 0) return false;

    // `if C then A else B` is the ternary and `STMT if COND` the one-line
    // guard; neither opens anything. A block `if` has no `then` on its line.
    if (w == "if") return t.find(" then ") == std::string::npos;
    if (w == "for" || w == "while" || w == "repeat" || w == "match" || w == "when" ||
        w == "else" || w == "otherwise" || w == "sub" || w == "form" || w == "record" ||
        w == "enum" || w == "try") {
        return true;
    }
    // A one-line guard puts the keyword in the middle: `call f() if ready`.
    if (t.find(" if ") != std::string::npos) return false;

    // A component block inside a form is `<type> <name>` and nothing else —
    // no `=`, no call. `use ui` and `module m` share that shape and open
    // nothing, so they are named rather than guessed at.
    if (w == "use" || w == "module" || w == "const" || w == "var" || w == "let" ||
        w == "call" || w == "return" || w == "defer" || w == "assert" || w == "check" ||
        w == "dll" || w == "on" || w == "end") {
        return false;
    }
    if (t.find('=') != std::string::npos || t.find('(') != std::string::npos) return false;
    // Exactly two words.
    const size_t sp = t.find(' ');
    if (sp == std::string::npos) return false;
    return t.find(' ', sp + 1) == std::string::npos && !t.substr(sp + 1).empty();
}

/// Enter, with the indentation the new line should start on.
///
/// This inserts the newline itself rather than letting the control do it: the
/// platform sends a Return keydown *and* a '\n' textinput, and doing half the
/// work in each is how a line ends up with two newlines or none.
void newline_with_indent() {
    auto* ta = dynamic_cast<Rml::ElementFormControlTextArea*>(by_id("fullcode"));
    int line = 0, col = 0;
    if (!ta || !caret_position(line, col)) return;
    std::string text = ta->GetValue();

    // A selection is replaced, as it is in any editor and as the control this
    // stands in for would have done. Taken out first, so the line the indent
    // is copied from is the line the new one actually follows.
    int sel_a = 0, sel_b = 0;
    Rml::String selected;
    ta->GetSelection(&sel_a, &sel_b, &selected);
    if (sel_b > sel_a) {
        const size_t from =
            (size_t)Rml::StringUtilities::ConvertCharacterOffsetToByteOffset(text, sel_a);
        const size_t to =
            (size_t)Rml::StringUtilities::ConvertCharacterOffsetToByteOffset(text, sel_b);
        if (to > from && to <= text.size()) {
            text.erase(from, to - from);
            line = 0;
            col = 0;
            for (size_t i = 0; i < from && i < text.size(); i++) {
                if (text[i] == '\n') { line++; col = 0; }
                else if (((unsigned char)text[i] & 0xC0) != 0x80) col++;
            }
        }
    }

    const std::string cur = line_text(text, line);
    std::string indent = leading_space(cur);
    if (opens_block(cur)) indent += std::string((size_t)indent_width(), ' ');

    // Only what is to the left of the caret decides the indent; splitting a
    // line in the middle keeps the tail, it does not re-indent it.
    const size_t at = byte_offset(text, line, col);
    const std::string insert = "\n" + indent;
    text.insert(at, insert);
    set_editor_text(text, at + insert.size());
}

/// Tab and Shift+Tab. With a selection spanning lines, every line in it moves;
/// otherwise Tab pads to the next stop and Shift+Tab takes one level off the
/// front of the line, wherever the caret happens to be sitting on it.
void indent_selection(bool out) {
    auto* ta = dynamic_cast<Rml::ElementFormControlTextArea*>(by_id("fullcode"));
    if (!ta) return;
    std::string text = ta->GetValue();
    int start = 0, end = 0;
    Rml::String selected;
    ta->GetSelection(&start, &end, &selected);
    const size_t from_b =
        (size_t)Rml::StringUtilities::ConvertCharacterOffsetToByteOffset(text, start);
    const size_t to_b = (size_t)Rml::StringUtilities::ConvertCharacterOffsetToByteOffset(text, end);

    int line = 0, col = 0;
    if (!caret_position(line, col)) return;

    // The single-caret case: pad to the next stop, or pull one level off.
    if (from_b == to_b && !out) {
        const size_t at = byte_offset(text, line, col);
        const int width = indent_width();
        const int pad = width - (col % width);
        text.insert(at, std::string((size_t)(pad ? pad : width), ' '));
        set_editor_text(text, at + (size_t)(pad ? pad : width));
        return;
    }

    // Which lines the selection touches. A selection ending exactly at a line
    // start does not include that line — the user selected up to it, not it.
    int first = 0, last = 0, c = 0;
    {
        std::string t2 = text;
        int l = 0;
        size_t i = 0;
        for (; i < t2.size() && i < from_b; i++) if (t2[i] == '\n') l++;
        first = l;
        for (; i < t2.size() && i < to_b; i++) if (t2[i] == '\n') l++;
        last = l;
        if (to_b > from_b && to_b <= t2.size() && to_b > 0 && t2[to_b - 1] == '\n' && last > first) {
            last--;
        }
        (void)c;
    }
    if (from_b == to_b) first = last = line;

    size_t moved_first = 0;
    for (int l = first; l <= last; l++) {
        const size_t ls = byte_offset(text, l, 0);
        if (ls > text.size()) break;
        if (out) {
            size_t n = 0;
            while (n < (size_t)indent_width() && ls + n < text.size() && text[ls + n] == ' ') n++;
            text.erase(ls, n);
        } else {
            text.insert(ls, std::string((size_t)indent_width(), ' '));
        }
        if (l == first) moved_first = ls;
    }
    (void)moved_first;
    // The caret stays on its line, shifted by what that line gained or lost.
    const size_t ls = byte_offset(text, line, 0);
    const std::string now = line_text(text, line);
    const int width = indent_width();
    size_t want = (size_t)col + (size_t)(out ? -width : width);
    if ((int)want < 0 || out) want = (size_t)std::max(0, col - width);
    if (want > now.size()) want = now.size();
    set_editor_text(text, ls + want);
}

/* --- the handler gesture ---------------------------------------------------- */

/// Parameter names from types, the way the language server and the checker
/// spell them (`param_list` in validate.rs), so a handler written from the
/// catalogue reads exactly like one the server wrote.
std::string param_list(const std::vector<std::string>& types) {
    std::string out;
    std::vector<std::pair<std::string, int>> seen;
    for (const auto& t : types) {
        const char* stem = (t == "int" || t == "int64") ? "n"
                           : t == "double"              ? "x"
                           : t == "text"                ? "s"
                           : t == "bool"                ? "flag"
                           : t == "bytes"               ? "data"
                                                        : "value";
        int n = 0;
        for (auto& s : seen) {
            if (s.first == stem) n = ++s.second;
        }
        if (n == 0) { seen.push_back({stem, 1}); n = 1; }
        const std::string name = n == 1 ? std::string(stem) : stem + std::to_string(n);
        out += (out.empty() ? "" : ", ") + name + ": " + t;
    }
    return out;
}

/// The stub for a new handler, with what the event hands it.
///
/// The catalogue knows the parameters of every component this build was
/// linked against. For one it was not — a timer, whose descriptor lives in
/// the runtime — the language server is asked: the handler completion it
/// offers other editors carries the full `sub` text, and the same text is
/// used here. When neither answers the stub takes no parameters, which the
/// checker accepts for every event.
std::string handler_stub(const CatalogComponent& cc, const CatalogEvent& ev,
                         const std::string& name, const std::string& file_text) {
    if (ev.known) {
        const std::string params = param_list(ev.params);
        return "\nsub " + name + (params.empty() ? "" : "(" + params + ")") + "\n  \nend\n";
    }
    (void)cc;
    // Where the wiring line now sits: the completion is asked for at the end
    // of it, which is the position the server treats as "the handler's name".
    int line = 0;
    size_t start = 0;
    while (start <= file_text.size()) {
        const size_t nl = file_text.find('\n', start);
        const std::string l =
            file_text.substr(start, nl == std::string::npos ? std::string::npos : nl - start);
        const size_t on = l.find("on " + ev.name + ":");
        if (on != std::string::npos && l.find(name, on) != std::string::npos) {
            g.lsp.did_change(file_text);
            kiln::json::Value v;
            const int id = g.lsp.completion(line, (int)l.size());
            if (id && g.lsp.wait(id, v, 2000)) {
                const kiln::json::Value& items =
                    v.kind == kiln::json::Value::Kind::Array ? v : v["items"];
                for (size_t i = 0; i < items.size(); i++) {
                    const kiln::json::Value& it = items.at(i);
                    if (it["label"].str() != name) continue;
                    const std::string text = it["additionalTextEdits"].at(0)["newText"].str();
                    if (!text.empty()) return text;
                }
            }
            break;
        }
        if (nl == std::string::npos) break;
        start = nl + 1;
        line++;
    }
    return "\nsub " + name + "\n  \nend\n";
}

/// Double-click on a component: its default event gets a handler, and the
/// editor opens inside it. Delphi's single most-used gesture.
///
/// The default event is the first one the component declares — the
/// descriptor's order is the author's order, and every author puts the one
/// that matters first: click for a button, change for an editbox, tick for a
/// timer, select for a grid. No table by name here, so a kit's component gets
/// the gesture with no change to the IDE.
///
/// With `event` named — from the Events tab, the wiring box or a badge — that
/// event is the one handled, through exactly this path.
void open_handler(const std::string& id, const std::string& event) {
    Component* c = g.model.find(id);
    if (!c) return;
    const CatalogComponent* cc = g.catalog.find(c->type_name);
    if (!cc || cc->events.empty()) {
        set_status(c->type_name + " has no events to handle");
        return;
    }
    CatalogEvent ev = cc->events.front();
    if (!event.empty()) {
        bool known = false;
        for (const auto& e : cc->events) {
            if (e.name == event) { ev = e; known = true; }
        }
        if (!known) {
            set_status(c->type_name + " has no event " + event);
            return;
        }
    }
    std::string name;
    if (const std::string* h = c->handler(ev.name)) name = *h;
    if (name.empty() || !g.model.has_sub(name)) {
        if (name.empty()) {
            name = id + "_" + ev.name;
            push_undo();
            c->set_handler(ev.name, name);
            mark_dirty();
        }
        // The wiring goes through the same save as any designer edit; the
        // stub is appended after it, because save_model's own stubs carry no
        // parameters and this event may hand some over. A stub the inspector
        // already promised for this name would be written twice otherwise.
        for (size_t i = 0; i < g.pending_subs.size(); i++) {
            if (g.pending_subs[i] == name) g.pending_subs.erase(g.pending_subs.begin() + (long)i--);
        }
        std::string err;
        if (!save_model(g.model, g.pending_subs, property_needs_quotes, err)) {
            set_status("save failed: " + err);
            return;
        }
        g.pending_subs.clear();
        std::string text;
        if (FILE* f = std::fopen(g.model.path.c_str(), "rb")) {
            // Re-read rather than reuse: save_model has just moved lines.
            char buf[4096];
            size_t n;
            while ((n = std::fread(buf, 1, sizeof buf, f)) > 0) text.append(buf, n);
            std::fclose(f);
        }
        const std::string stub = handler_stub(*cc, ev, name, text);
        if (FILE* f = std::fopen(g.model.path.c_str(), "ab")) {
            std::fwrite(stub.data(), 1, stub.size(), f);
            std::fclose(f);
        }
        g.model.subs.push_back(name);
        g.dirty = false;
        set_status("wired " + id + "." + ev.name + " to " + name);
    }
    set_view("code");
    const int line = line_of_sub(g.model_text, name);
    if (line >= 0) jump_to(line + 1, 2);
}

/* --- the context menu ----------------------------------------------------- */

/// Open the component's menu at the pointer: its name, one row per event the
/// descriptor declares — with the handler it goes to, when it has one — and
/// Delete. The rows carry what they do as attributes; the click handler reads
/// them, so a row is activated through the same path a mouse takes. Every row
/// also carries its label verbatim, which is what the test verbs read: a test
/// that matched rendered text would silently pass against a wrong menu.
void open_context_menu(const std::string& id, int x, int y) {
    Rml::Element* pop = by_id("ctxmenu");
    const Component* c = g.model.find(id);
    if (!pop || !c) return;
    const CatalogComponent* cc = g.catalog.find(c->type_name);
    const bool is_form = id == g.model.form_name;
    std::string html = "<div class='cxhead' oe-cxlabel='" + esc(id + " (" + c->type_name + ")") +
                       "'>" + esc(id) + "<span class='cxtype'>(" + esc(c->type_name) +
                       ")</span></div><div class='cxsep'/>"
                       "<div class='cxlabel' oe-cxlabel='Events'>Events</div>";
    if (!cc || cc->events.empty()) {
        html += "<div class='cxlabel' oe-cxlabel='no events'>no events</div>";
    }
    for (const auto& ev : cc ? cc->events : std::vector<CatalogEvent>{}) {
        std::string label = ev.name;
        std::string inner = esc(ev.name);
        if (const std::string* h = c->handler(ev.name)) {
            // U+2192, the arrow, written out the way the tray writes its glyph.
            label += " \xe2\x86\x92 " + *h;
            inner += "<span class='cxhandler'> \xe2\x86\x92 " + esc(*h) + "</span>";
        }
        html += "<div class='cxrow' oe-cxlabel='" + esc(label) + "' oe-cxid='" + esc(id) +
                "' oe-cxevent='" + esc(ev.name) + "'>" + inner + "</div>";
    }
    if (!is_form) {
        html += "<div class='cxsep'/><div class='cxrow' oe-cxlabel='Delete' oe-cxdelete='1'>"
                "Delete</div>";
    }
    pop->SetInnerRML(html);
    pop->SetProperty("display", "block");
    // Laid out once to learn its size, then placed: a menu near the right or
    // bottom edge opens to the left of or above the pointer instead of off
    // the window.
    g.context->Update();
    const auto size = pop->GetBox().GetSize(Rml::BoxArea::Border);
    const auto dim = g.context->GetDimensions();
    if (x + (int)size.x > dim.x) x -= (int)size.x;
    if (y + (int)size.y > dim.y) y -= (int)size.y;
    x = std::max(0, x);
    y = std::max(0, y);
    if (std::getenv("KILN_DESIGNER_DEBUG")) {
        std::fprintf(stderr, "ctxmenu: %s at %d,%d size %dx%d window %dx%d\n", id.c_str(), x, y,
                     (int)size.x, (int)size.y, dim.x, dim.y);
    }
    pop->SetProperty("left", Rml::String(std::to_string(x) + "px"));
    pop->SetProperty("top", Rml::String(std::to_string(y) + "px"));
    g.context->Update();
    g.ctx_id = id;
}

void close_context_menu() {
    g.ctx_id.clear();
    if (Rml::Element* pop = by_id("ctxmenu")) pop->SetProperty("display", "none");
}

/* --- the menu bar --------------------------------------------------------- */

/// Which menu bar title is open, or -1. The popup's own `display` is not the
/// answer: a click on the open title has to *close* it, and that needs to know
/// which title the visible popup belongs to.
int g_open_menu = -1;

void close_menu() {
    g_open_menu = -1;
    if (Rml::Element* pop = by_id("menupop")) pop->SetProperty("display", "none");
}

/// Show `idx`'s items under its title.
///
/// Placed against the title's own left edge rather than a stored column, so a
/// menu bar that reflows — a longer title, a different font — keeps its popups
/// under the words they belong to.
void open_menu(int idx) {
    if (idx < 0 || (size_t)idx >= menus().size()) return;
    Rml::Element* pop = by_id("menupop");
    Rml::Element* title = nullptr;
    if (Rml::Element* bar = by_id("menubar")) {
        for (int i = 0; i < bar->GetNumChildren(); i++) {
            Rml::Element* c = bar->GetChild(i);
            if (c->HasAttribute("oe-menu") && c->GetAttribute<int>("oe-menu", -1) == idx) {
                title = c;
                break;
            }
        }
    }
    if (!pop || !title) return;
    std::string html;
    for (const auto& item : menus()[(size_t)idx].items) {
        html += "<div class='mi' oe-action='" + std::string(item.action) + "'>" + item.label +
                (item.keys[0] ? "<span class='keys'>" + std::string(item.keys) + "</span>" : "") +
                "</div>";
    }
    pop->SetInnerRML(html);
    const auto at = title->GetAbsoluteOffset(Rml::BoxArea::Border);
    pop->SetProperty("left", Rml::String(std::to_string((int)at.x) + "px"));
    pop->SetProperty(
        "top", Rml::String(std::to_string(theme::TITLEBAR_H + theme::MENUBAR_H) + "px"));
    pop->SetProperty("display", "block");
    g_open_menu = idx;
}

/* --- events --------------------------------------------------------------- */

struct Listener : Rml::EventListener {
    void ProcessEvent(Rml::Event& ev) override {
        Rml::Element* el = ev.GetTargetElement();
        const Rml::String type = ev.GetType();

        if (type == "mousescroll") {
            if (std::getenv("KILN_DESIGNER_DEBUG")) {
                std::fprintf(stderr, "wheel: target=<%s> id=%s dy=%.1f\n",
                             el->GetTagName().c_str(), el->GetId().c_str(),
                             ev.GetParameter<float>("wheel_delta_y", 0.f));
            }
            // Scroll the code editor with the wheel. Three lines a tick, the
            // conventional amount.
            for (Rml::Element* e = el; e; e = e->GetParentNode()) {
                if (e->GetId() == "codeview" || e->GetId() == "fullcode" ||
                    e->GetId() == "codehl") {
                    g.code_scroll +=
                        (int)ev.GetParameter<float>("wheel_delta_y", 0.f) * theme::CODE_LINE_H *
                        kiln::settings::number("editor.scroll_lines");
                    sync_highlight_scroll();
                    ev.StopPropagation();
                    return;
                }
                // The console likewise: its textarea keeps its own overflow,
                // so the wheel over the layer or the box would otherwise do
                // nothing. A follow still pending is cancelled with it; the
                // next line appended follows again, because a console that
                // stops showing the newest output was the bug before this.
                if (e->GetId() == "logbox" || e->GetId() == "log" || e->GetId() == "loghl") {
                    if (Rml::Element* ta = by_id("log")) {
                        ta->SetScrollTop(ta->GetScrollTop() +
                                         ev.GetParameter<float>("wheel_delta_y", 0.f) *
                                             theme::LOG_LINE_H *
                                             kiln::settings::number("editor.scroll_lines"));
                        g.log_follow = false;
                        sync_log_scroll();
                    }
                    ev.StopPropagation();
                    return;
                }
            }
            return;
        }

        if (type == "change") {
            Rml::Element* src = el;
            const Rml::String cls = src->GetAttribute<Rml::String>("class", "");
            const Rml::String value = src->GetAttribute<Rml::String>("value", "");
            if (src->GetId() == "fullcode") {
                // Typing in the code editor. Mark it dirty so no refresh
                // overwrites the text, and so Save knows the editor — not the
                // component model — is what to write out.
                if (!g.code_dirty) {
                    g.code_dirty = true;
                    g.dirty = true;
                    set_status("code edited — Ctrl+S to save");
                }
                refresh_highlight();
                // A references list describes the text before this keystroke.
                if (!g.refs.empty()) {
                    g.refs.clear();
                    render_diagnostics();
                }
                // Diagnostics as you type: the server re-checks on every
                // change, exactly as it does for any other editor.
                if (auto* ed2 = code_editor()) g.lsp.did_change(ed2->GetValue());
                g.symbols_stale = true;
                completion_on_change();
                return;
            }
            if (src->GetId() == "search") {
                g.search = value;
                if (Rml::Element* list = by_id("toollist")) list->SetInnerRML(build_toolbox());
                return;
            }
            Component* comp = g.model.find(g.selected);
            if (!comp) return;
            const Rml::String name = src->GetAttribute<Rml::String>("name", "");
            if (cls.find("cid") != Rml::String::npos) {
                rename_selected(value);
                return;
            }
            if (name.empty()) return;
            if (cls.find("ev") != Rml::String::npos) {
                push_undo();
                comp->set_handler(name, value);
                mark_dirty();
                if (!value.empty() && !g.model.has_sub(value)) {
                    g.pending_subs.push_back(value);
                    set_status("will create sub " + value);
                }
                refresh_all();
            } else if (cls.find("pvm") != Rml::String::npos) {
                // A textarea keeps its text as a value, not as an attribute:
                // read the attribute and every edit after the first is lost.
                auto* box = dynamic_cast<Rml::ElementFormControl*>(src);
                if (!box) return;
                push_undo();
                comp->set_property(name, box->GetValue());
                mark_dirty();
                // Not refresh_all(): rebuilding the inspector under a control
                // the user is typing in takes the caret with it.
                rebuild_canvas();
                rebuild_code();
            } else if (cls.find("pv") != Rml::String::npos) {
                push_undo();
                // A new form size typed into the inspector is a resize: the
                // anchored children follow it exactly as they follow the grip.
                if (comp == &g.model.form && (name == "width" || name == "height")) {
                    const int before = prop_int(g.model.form, name.c_str(), 0);
                    const int after = std::atoi(value.c_str());
                    comp->set_property(name, value);
                    if (before > 0 && after > 0)
                        follow_form_resize(name == "width" ? after - before : 0,
                                           name == "height" ? after - before : 0, nullptr);
                } else {
                    comp->set_property(name, value);
                }
                mark_dirty();
                // The swatch beside a colour field follows the text at once,
                // since the grid itself is not rebuilt under the caret.
                if (Rml::Element* row = src->GetParentNode()) {
                    for (int i = 0; i < row->GetNumChildren(); i++) {
                        Rml::Element* c = row->GetChild(i);
                        if (c->GetAttribute<Rml::String>("oe-swatch", "") == name)
                            c->SetAttribute("style", Rml::String(swatch_style(value)));
                    }
                }
                refresh_all();
            }
            return;
        }

        if (type == "keydown") {
            // Escape closes the context menu from anywhere — an inspector
            // field can still hold the focus when a right-click opens it.
            if (!g.ctx_id.empty() &&
                ev.GetParameter<int>("key_identifier", 0) == Rml::Input::KI_ESCAPE) {
                close_context_menu();
                return;
            }
            // And an open menu, for the same reason: the mouse opened it, but
            // the keyboard has to be able to get back out.
            if (g_open_menu >= 0 &&
                ev.GetParameter<int>("key_identifier", 0) == Rml::Input::KI_ESCAPE) {
                close_menu();
                return;
            }
            // A key pressed inside a text control belongs to that control.
            // The editor's textarea stops every keydown before it bubbles
            // here today, but that is RmlUi's choice, not a contract: a
            // Ctrl+V while typing must never paste a component.
            for (Rml::Element* e = el; e; e = e->GetParentNode()) {
                const Rml::String tag = e->GetTagName();
                if (tag == "textarea" || tag == "input") return;
            }
            const int key = ev.GetParameter<int>("key_identifier", 0);
            const bool ctrl = ev.GetParameter<bool>("ctrl_key", false);
            const bool shift = ev.GetParameter<bool>("shift_key", false);
            // Ctrl+, — the shortcut every IDE surveyed uses for preferences.
            if (ctrl && key == Rml::Input::KI_OEM_COMMA) { open_settings(g.settings_cat); return; }
            if (ctrl && key == Rml::Input::KI_Z) { shift ? redo() : undo(); return; }
            if (ctrl && key == Rml::Input::KI_Y) { redo(); return; }
            if (ctrl && key == Rml::Input::KI_C) { copy_selection(); return; }
            if (ctrl && key == Rml::Input::KI_V) { paste_clipboard(); return; }
            // Not Ctrl+S: the backend's key callback saves once the context
            // has declined the key, and a save here too would be two.
            if (g.view == "code") return;   // the rest are designer gestures
            if (key == Rml::Input::KI_DELETE) { delete_selection(); return; }
            // Nudge the selection with the arrow keys: 1px normally, a grid
            // step with shift.
            int dx = 0, dy = 0;
            if (key == Rml::Input::KI_LEFT) dx = -1;
            else if (key == Rml::Input::KI_RIGHT) dx = 1;
            else if (key == Rml::Input::KI_UP) dy = -1;
            else if (key == Rml::Input::KI_DOWN) dy = 1;
            if (dx || dy) {
                if (shift) { dx *= grid_size(); dy *= grid_size(); }
                push_undo();
                for (const auto& id : g.selection) {
                    if (id == g.model.form_name) continue;   // not on the canvas to nudge
                    if (Component* c = g.model.find(id)) {
                        write_int(*c, "left", std::max(0, prop_int(*c, "left", 0) + dx));
                        write_int(*c, "top", std::max(0, prop_int(*c, "top", 0) + dy));
                    }
                }
                mark_dirty();
                refresh_all();
            }
            return;
        }

        if (type == "click") {
            // A row of the context menu does what it says; a click anywhere
            // else closes the menu, and goes on to whatever it was for. The
            // row's id and event are copied out before the menu closes and
            // the handler opens, since either may rebuild the chrome.
            if (!g.ctx_id.empty()) {
                bool in_menu = false;
                for (Rml::Element* e = el; e; e = e->GetParentNode()) {
                    if (e->GetId() == "ctxmenu") in_menu = true;
                    if (e->HasAttribute("oe-cxevent")) {
                        const std::string id = e->GetAttribute<Rml::String>("oe-cxid", "");
                        const std::string event = e->GetAttribute<Rml::String>("oe-cxevent", "");
                        close_context_menu();
                        open_handler(id, event);
                        return;
                    }
                    if (e->HasAttribute("oe-cxdelete")) {
                        close_context_menu();
                        delete_selection();
                        return;
                    }
                }
                if (in_menu) return;   // the heading or a separator
                close_context_menu();
            }
            // A click on a menu bar title opens it — or closes it, when it is
            // the one already open. A title that only ever opens leaves no way
            // back out with the mouse, which is the one thing every other menu
            // bar does. A click anywhere else dismisses whatever is open.
            bool hit_menu = false;
            for (Rml::Element* e = el; e; e = e->GetParentNode()) {
                if (!e->HasAttribute("oe-menu")) continue;
                const int idx = e->GetAttribute<int>("oe-menu", 0);
                if (idx == g_open_menu) close_menu();
                else open_menu(idx);
                hit_menu = true;
                break;
            }
            if (hit_menu) return;
            close_menu();

            // A choice made inside an editor popup, before anything dismisses
            // it — the click that picks a colour is also a click outside every
            // other popup.
            for (Rml::Element* e = el; e; e = e->GetParentNode()) {
                if (e->HasAttribute("oe-color")) {
                    set_edited_property(e->GetAttribute<Rml::String>("oe-color", ""));
                    return;
                }
                if (e->HasAttribute("oe-pick")) {
                    set_edited_property(e->GetAttribute<Rml::String>("oe-pick", ""));
                    return;
                }
            }
            {
                bool in_pop = false;
                for (Rml::Element* e = el; e; e = e->GetParentNode()) {
                    if (e->GetId() == "editpop") in_pop = true;
                }
                if (!in_pop && !el->HasAttribute("oe-swatch") && !el->HasAttribute("oe-file")) {
                    close_editpop();
                }
            }

            for (Rml::Element* e = el; e; e = e->GetParentNode()) {
                if (e->HasAttribute("oe-anchor")) {
                    if (Component* c = g.model.find(g.selected)) {
                        const std::string prop = e->GetAttribute<Rml::String>("oe-prop", "anchors");
                        const std::string* cur = c->property(prop);
                        push_undo();
                        c->set_property(prop, toggle_anchor(cur ? *cur : "",
                                                            e->GetAttribute<Rml::String>("oe-anchor", "")));
                        mark_dirty();
                        refresh_all();
                    }
                    return;
                }
                if (e->HasAttribute("oe-swatch")) {
                    g.editing_id = g.selected;
                    g.editing_prop = e->GetAttribute<Rml::String>("oe-swatch", "");
                    std::string html = "<div class='poptitle'>" + esc(g.editing_prop) + "</div>";
                    for (const char* c : palette()) {
                        html += "<div class='chip' oe-color='" + std::string(c) +
                                "' style='background-color:" + c + "'/>";
                    }
                    open_editpop(e, html);
                    return;
                }
                if (e->HasAttribute("oe-file")) {
                    g.editing_id = g.selected;
                    g.editing_prop = e->GetAttribute<Rml::String>("oe-file", "");
                    std::string html = "<div class='poptitle'>" + esc(g.editing_prop) +
                                       " — files beside the project</div>";
                    const auto files = project_files();
                    for (const auto& f : files) {
                        html += "<div class='fileitem' oe-pick='" + esc(f) + "'>" + esc(f) +
                                "</div>";
                    }
                    if (files.empty()) {
                        html += "<div class='hint'>Nothing to choose: this project's directory "
                                "holds no files other than the project itself.</div>";
                    }
                    open_editpop(e, html);
                    return;
                }
                if (e->HasAttribute("oe-add")) {
                    add_component(e->GetAttribute<Rml::String>("oe-add", ""));
                    return;
                }
                if (e->HasAttribute("oe-soon")) {
                    set_status(e->GetAttribute<Rml::String>("oe-soon", "") +
                               " is in the design spec but not implemented yet");
                    return;
                }
                if (e->HasAttribute("oe-view")) {
                    set_view(e->GetAttribute<Rml::String>("oe-view", "designer"));
                    return;
                }
                if (e->HasAttribute("oe-tab")) {
                    g.inspector_tab = e->GetAttribute<Rml::String>("oe-tab", "props");
                    if (Rml::Element* bar = e->GetParentNode()) {
                        for (int i = 0; i < bar->GetNumChildren(); i++) {
                            Rml::Element* seg = bar->GetChild(i);
                            const bool active =
                                seg->GetAttribute<Rml::String>("oe-tab", "") == g.inspector_tab;
                            seg->SetAttribute("class",
                                              Rml::String(active ? "seg active" : "seg"));
                        }
                    }
                    rebuild_inspector();
                    return;
                }
                if (e->HasAttribute("oe-bp")) {
                    toggle_breakpoint(e->GetAttribute<int>("oe-bp", 0));
                    return;
                }
                if (e->HasAttribute("oe-win")) {
                    window_control(e->GetAttribute<Rml::String>("oe-win", ""));
                    return;
                }
                if (e->HasAttribute("oe-action")) {
                    const Rml::String a = e->GetAttribute<Rml::String>("oe-action", "");
                    if (a == "save") save();
                    else if (a == "run") build_binary(true);
                    else if (a == "build") build_binary(false);
                    else if (a == "stop") stop_all();
                    else if (a == "debug") start_debug();
                    else if (a == "togglebp") {
                        int line = 0, col = 0;
                        if (caret_position(line, col)) toggle_breakpoint(line + 1);
                    }
                    else if (a == "dbgcontinue") g.dbg.continue_();
                    else if (a == "dbgstepover") g.dbg.step_over();
                    else if (a == "dbgstepin") g.dbg.step_in();
                    else if (a == "dbgstepout") g.dbg.step_out();
                    else if (a == "dbgpause") g.dbg.pause();
                    else if (a == "dbgstop") stop_debug();
                    else if (a == "undo") undo();
                    else if (a == "redo") redo();
                    else if (a == "copy") copy_selection();
                    else if (a == "paste") paste_clipboard();
                    else if (a == "delete") delete_selection();
                    else if (a == "view-designer") set_view("designer");
                    else if (a == "view-code") set_view("code");
                    else if (a == "about") show_about();
                    else if (a == "help") context_help();
                    else if (a == "helpcommands") open_help("reference-commands", "", "");
                    else if (a == "helpsearch") {
                        // The menu says "search", so the caret belongs in the
                        // search box — otherwise the entry just reopens the
                        // last page and the user has to go find the field.
                        open_help(g.help_page, "", "");
                        if (g.help)
                            if (Rml::Element* f = g.help->GetElementById("find")) f->Focus();
                    }
                    else if (a == "settings") open_settings(g.settings_cat);
                    else if (a == "exit") {
                        Backend::RequestExit();
                    } else {
                        set_status(a + " is not implemented yet");
                    }
                    return;
                }
                if (e->HasAttribute("oe-jump")) {
                    open_handler(e->GetAttribute<Rml::String>("oe-jump", ""),
                                 e->GetAttribute<Rml::String>("oe-event", ""));
                    return;
                }
                if (e->HasAttribute("oe-ref")) {
                    const size_t i = (size_t)e->GetAttribute<int>("oe-ref", 0);
                    if (i < g.refs.size()) jump_to(g.refs[i].range.line, g.refs[i].range.character);
                    return;
                }
                if (e->HasAttribute("oe-id")) {
                    select(e->GetAttribute<Rml::String>("oe-id", ""),
                           ev.GetParameter<bool>("ctrl_key", false));
                    return;
                }
            }
            return;
        }

        if (type == "mousedown") {
            const int mx = ev.GetParameter<int>("mouse_x", 0);
            const int my = ev.GetParameter<int>("mouse_y", 0);

            // A press on a completion row takes it; a press anywhere else
            // moves the caret or the focus, and the popup was for neither.
            if (g.complete_open) {
                for (Rml::Element* e = el; e; e = e->GetParentNode()) {
                    if (!e->HasAttribute("oe-citem")) continue;
                    g.complete_index = std::atoi(e->GetAttribute<Rml::String>("oe-citem", "0").c_str());
                    accept_completion();
                    ev.StopPropagation();
                    return;
                }
                close_completion();
            }

            // The right button (1, in RmlUi's numbering) opens a component's
            // context menu and nothing else: no drag, no double-click
            // bookkeeping, so a right press followed by a quick left press is
            // not read as the handler gesture. The component is found the
            // way a left press finds it — a composite's child, or the form
            // through its window — and selected first, as Delphi selects what
            // is right-clicked. A left press outside an open menu closes it;
            // one on a row leaves it for the click that follows.
            const int button = ev.GetParameter<int>("button", 0);
            if (button == 1 || !g.ctx_id.empty()) {
                std::string id;
                bool in_menu = false;
                for (Rml::Element* e = el; e; e = e->GetParentNode()) {
                    if (e->GetId() == "ctxmenu") in_menu = true;
                    if (e->HasAttribute("oe-jump")) break;
                    if (e->HasAttribute("oe-id")) id = e->GetAttribute<Rml::String>("oe-id", "");
                    else if (e->GetId() == "formwin") id = g.model.form_name;
                    if (!id.empty()) break;
                }
                if (in_menu) return;
                close_context_menu();   // a left press goes on to what it was for
                if (button == 1) {
                    if (!id.empty()) {
                        if (!is_selected(id)) select(id);
                        open_context_menu(id, mx, my);
                    }
                    return;
                }
            }

            // Dock splitters.
            {
                const Rml::String id = el->GetId();
                if (id == "splitleft" || id == "splitright" || id == "splitbottom" ||
                    id == "splitmid") {
                    g.splitting = id == "splitleft"    ? "left"
                                  : id == "splitright" ? "right"
                                  : id == "splitmid"   ? "mid"
                                                       : "bottom";
                    g.split_x0 = mx;
                    g.split_y0 = my;
                    g.split_v0 = g.splitting == "left"    ? g.toolbox_w
                                 : g.splitting == "right" ? g.inspect_w
                                 : g.splitting == "mid"   ? g.code_w
                                                          : g.bottom_h;
                    return;
                }
            }

            // Resizing the form preview, as in Visual Studio / RAD Studio.
            if (el->HasAttribute("oe-formgrip")) {
                push_undo();
                g.resizing_form = true;
                g.resize_edge = el->GetAttribute<Rml::String>("oe-formgrip", "se");
                g.resize_x0 = mx;
                g.resize_y0 = my;
                g.resize_w0 = prop_int(g.model.form, "width", 420);
                g.resize_h0 = prop_int(g.model.form, "height", 260);
                g.anchor_base.clear();
                for (const auto& c : g.model.children) {
                    g.anchor_base[c.id] = {prop_int(c, "left", 0), prop_int(c, "top", 0),
                                           prop_int(c, "width", 120), prop_int(c, "height", 32)};
                }
                return;
            }
            // Resizing the selected component by its anchor.
            if (el->HasAttribute("oe-grip")) {
                if (Component* c = g.model.find(g.selected)) {
                    push_undo();
                    g.resizing_comp = true;
                    g.comp_edge = el->GetAttribute<Rml::String>("oe-grip", "se");
                    g.resize_x0 = mx;
                    g.resize_y0 = my;
                    g.comp_x0 = prop_int(*c, "left", 0);
                    g.comp_y0 = prop_int(*c, "top", 0);
                    g.comp_w0 = prop_int(*c, "width", 120);
                    g.comp_h0 = prop_int(*c, "height", 32);
                }
                return;
            }
            // Composite components (a checkbox is a container holding a box and
            // a caption) deliver mousedown on the CHILD, which carries no id —
            // so walk up to the component. Without this, whether a component
            // could be dragged depended on which part you grabbed.
            //
            // Past the components is the form: its title bar, its buttons and
            // the bare client area all select it, the way any RAD tool's do.
            // Not through a badge, though — a badge is a way into a handler,
            // and selecting the form under it would rebuild the canvas and
            // take the badge away before its click arrived.
            for (Rml::Element* e = el; e; e = e->GetParentNode()) {
                if (e->HasAttribute("oe-jump")) break;
                std::string id;
                if (e->HasAttribute("oe-id")) id = e->GetAttribute<Rml::String>("oe-id", "");
                else if (e->GetId() == "formwin") id = g.model.form_name;
                if (id.empty()) continue;
                // A second press on the same component, soon and close: the
                // handler gesture, and not the start of a drag.
                const double t = now_seconds();
                const bool twice = id == g.last_press_id && t - g.last_press_time < 0.5 &&
                                   std::abs(mx - g.last_press_x) <= 4 &&
                                   std::abs(my - g.last_press_y) <= 4;
                g.last_press_id = twice ? std::string() : id;
                g.last_press_time = t;
                g.last_press_x = mx;
                g.last_press_y = my;
                if (twice) {
                    open_handler(id);
                    return;
                }
                if (ev.GetParameter<bool>("ctrl_key", false)) {
                    select(id, true);   // extend the selection instead of dragging
                } else {
                    if (!is_selected(id)) select(id);
                    begin_drag(id, mx, my);
                }
                break;
            }
            return;
        }

        if (type == "mousemove") {
            const int mx = ev.GetParameter<int>("mouse_x", 0);
            const int my = ev.GetParameter<int>("mouse_y", 0);

            // With a menu open, moving across the bar switches to the title
            // under the pointer without a second click — the behaviour every
            // menu bar has, and the reason a menu bar is faster than a row of
            // buttons. Only while one is open: hovering the bar with nothing
            // open must not start opening menus at the pointer.
            if (g_open_menu >= 0) {
                for (Rml::Element* e = el; e; e = e->GetParentNode()) {
                    if (!e->HasAttribute("oe-menu")) continue;
                    const int idx = e->GetAttribute<int>("oe-menu", -1);
                    if (idx >= 0 && idx != g_open_menu) open_menu(idx);
                    break;
                }
            }

            // Hover-on-rest: the clock restarts on every move, and the tip
            // for the last spot goes away with it.
            if (mx != g.hover_x || my != g.hover_y) {
                g.hover_x = mx;
                g.hover_y = my;
                g.hover_moved_at = now_seconds();
                g.hover_asked = false;
                // A request already out stays out, so its answer is taken
                // (and dropped) rather than left queued; it just no longer
                // matches the spot the tip would be shown for.
                g.hover_shown_for = 0;
                hide_tip();
            }

            if (!g.splitting.empty()) {
                int v = g.split_v0;
                if (g.splitting == "left") v = g.split_v0 + (mx - g.split_x0);
                else if (g.splitting == "right") v = g.split_v0 - (mx - g.split_x0);
                else if (g.splitting == "mid") v = g.split_v0 + (mx - g.split_x0);
                else v = g.split_v0 - (my - g.split_y0);
                if (g.splitting != "mid") {   // the middle one is clamped by relayout
                    if (v < 120) v = 120;
                    if (v > 640) v = 640;
                }
                if (g.splitting == "left") g.toolbox_w = v;
                else if (g.splitting == "right") g.inspect_w = v;
                else if (g.splitting == "mid") g.code_w = v;
                else g.bottom_h = v;
                relayout();
                return;
            }

            if (g.resizing_form) {
                int w = g.resize_w0, h = g.resize_h0;
                if (g.resize_edge != "s") w = g.resize_w0 + (mx - g.resize_x0);
                if (g.resize_edge != "e") h = g.resize_h0 + (my - g.resize_y0);
                const int nw = snap(w < 120 ? 120 : w), nh = snap(h < 80 ? 80 : h);
                write_int(g.model.form, "width", nw);
                write_int(g.model.form, "height", nh);
                follow_form_resize(nw - g.resize_w0, nh - g.resize_h0, &g.anchor_base);
                mark_dirty();
                rebuild_canvas();
                return;
            }

            if (g.resizing_comp) {
                Component* c = g.model.find(g.selected);
                if (!c) return;
                const int dx = mx - g.resize_x0, dy = my - g.resize_y0;
                int x = g.comp_x0, y = g.comp_y0, w = g.comp_w0, h = g.comp_h0;
                const bool west = g.comp_edge.find('w') != std::string::npos;
                const bool east = g.comp_edge.find('e') != std::string::npos;
                const bool north = g.comp_edge.find('n') != std::string::npos;
                const bool south = g.comp_edge.find('s') != std::string::npos;
                if (east) w = g.comp_w0 + dx;
                if (west) { x = g.comp_x0 + dx; w = g.comp_w0 - dx; }
                if (south) h = g.comp_h0 + dy;
                if (north) { y = g.comp_y0 + dy; h = g.comp_h0 - dy; }
                if (w < 20) w = 20;
                if (h < 16) h = 16;
                // The anchors offered are already only those whose properties
                // exist (rebuild_canvas), so a refusal here is the guard
                // holding against a stale overlay, not the normal path.
                write_int(*c, "left", snap(x < 0 ? 0 : x));
                write_int(*c, "top", snap(y < 0 ? 0 : y));
                write_int(*c, "width", snap(w));
                write_int(*c, "height", snap(h));
                mark_dirty();
                rebuild_canvas();
                return;
            }

            if (g.dragging) drag_to(mx, my);
            return;
        }

        if (type == "mouseup") {
            if (g.dragging || g.resizing_comp || g.resizing_form) rebuild_inspector();
            g.splitting.clear();
            if (g.dragging) {
                g.guide_x.clear();
                g.guide_y.clear();
                rebuild_canvas();
            }
            g.dragging = false;
            g.resizing_comp = false;
            g.resizing_form = false;
            return;
        }
    }
};
Listener g_listener;

/// The console is read-only, and RmlUi has no read-only textarea: `disabled`
/// switches the whole widget off, selection included. So the keys that would
/// edit it are stopped before they reach it. Registered in the CAPTURE phase
/// on the document, which runs ahead of the widget's own capture listener
/// on the element — a bubble-phase listener would run after the edit had
/// already happened. Navigation and the clipboard's read half go through.
///
/// The completion popup's keys live here for the same reason: the widget
/// moves the caret on Up/Down and stops the event, so a listener behind it
/// would never see the key the popup wanted.
struct KeyGate : Rml::EventListener {
    void ProcessEvent(Rml::Event& ev) override {
        Rml::Element* el = ev.GetTargetElement();
        if (!el) return;
        if (el->GetId() == "fullcode") { editor(ev); return; }
        if (el->GetId() != "log") return;
        if (ev.GetType() == "textinput") { ev.StopImmediatePropagation(); return; }
        const int key = ev.GetParameter<int>("key_identifier", 0);
        const bool ctrl = ev.GetParameter<bool>("ctrl_key", false);
        switch (key) {
        case Rml::Input::KI_LEFT: case Rml::Input::KI_RIGHT: case Rml::Input::KI_UP:
        case Rml::Input::KI_DOWN: case Rml::Input::KI_HOME: case Rml::Input::KI_END:
        case Rml::Input::KI_PRIOR: case Rml::Input::KI_NEXT:
            return;
        case Rml::Input::KI_A: case Rml::Input::KI_C:
            if (ctrl) return;
            break;
        default: break;
        }
        ev.StopImmediatePropagation();
    }

    void editor(Rml::Event& ev) {
        if (ev.GetType() == "textinput") {
            // The character a key the popup consumed still carries.
            if (!g.swallow_text.empty()) {
                const bool swallow = ev.GetParameter<Rml::String>("text", "") == g.swallow_text;
                g.swallow_text.clear();
                if (swallow) ev.StopImmediatePropagation();
            }
            return;
        }
        const int key = ev.GetParameter<int>("key_identifier", 0);
        const bool ctrl = ev.GetParameter<bool>("ctrl_key", false);
        const bool shift = ev.GetParameter<bool>("shift_key", false);
        g.swallow_text.clear();
        // F12 and Shift+F12, as in every editor that has them. Here, in the
        // capture phase, because the textarea stops every keydown it sees
        // whether or not it had a use for it: a listener on the document
        // would never hear an F12 pressed in the editor.
        if (key == Rml::Input::KI_F12) {
            shift ? find_references() : goto_definition();
            ev.StopImmediatePropagation();
            return;
        }
        // F1 on a name opens its reference entry; Shift+F1 searches the
        // handbook for it. Capture phase for the same reason as F12 — the
        // textarea swallows every keydown, and F1 is pressed in the editor.
        if (key == Rml::Input::KI_F1) {
            if (shift) {
                int line = 0, col = 0;
                std::string word;
                if (caret_position(line, col)) {
                    word = identifier_at(line, col);
                    if (word.empty() && col > 0) word = identifier_at(line, col - 1);
                }
                open_help(g.help_page, "", word);
            } else {
                context_help();
            }
            ev.StopImmediatePropagation();
            return;
        }
        // The debugging keys, in the capture phase for the same reason: the
        // textarea swallows every keydown it sees, and these are pressed in
        // the editor more than anywhere else.
        if (key == Rml::Input::KI_F5) {
            shift ? stop_debug() : (g.dbg.running() ? g.dbg.continue_() : start_debug());
            ev.StopImmediatePropagation();
            return;
        }
        if (key == Rml::Input::KI_F9) {
            int line = 0, col = 0;
            if (caret_position(line, col)) toggle_breakpoint(line + 1);
            ev.StopImmediatePropagation();
            return;
        }
        if (key == Rml::Input::KI_F10) {
            g.dbg.step_over();
            ev.StopImmediatePropagation();
            return;
        }
        if (key == Rml::Input::KI_F11) {
            shift ? g.dbg.step_out() : g.dbg.step_in();
            ev.StopImmediatePropagation();
            return;
        }
        if (ctrl && key == Rml::Input::KI_SPACE) {
            request_completion(true);
            g.swallow_text = " ";
            ev.StopImmediatePropagation();
            return;
        }
        // Tab and Return with no popup open: the editor's own, not the
        // control's. RmlUi's textarea does not indent — Tab moves the focus
        // out of the editor entirely — and its Return starts the new line at
        // column one whatever the line above was indented to.
        //
        // The keydown is stopped and the '\n' the platform sends after a
        // Return is swallowed, because the text was already inserted here;
        // letting the control add its own would give the line two newlines.
        if (!g.complete_open) {
            if (key == Rml::Input::KI_TAB && !ctrl) {
                indent_selection(shift);
                ev.StopImmediatePropagation();
                return;
            }
            if ((key == Rml::Input::KI_RETURN || key == Rml::Input::KI_NUMPADENTER) && !ctrl) {
                newline_with_indent();
                g.swallow_text = "\n";
                ev.StopImmediatePropagation();
                return;
            }
            return;
        }
        switch (key) {
        case Rml::Input::KI_UP:   move_completion(-1); break;
        case Rml::Input::KI_DOWN: move_completion(1); break;
        case Rml::Input::KI_RETURN:
        case Rml::Input::KI_NUMPADENTER:
            accept_completion();
            g.swallow_text = "\n";
            break;
        case Rml::Input::KI_TAB: accept_completion(); break;
        case Rml::Input::KI_ESCAPE: close_completion(); break;
        // The caret leaving the word by key: the word is no longer what the
        // list was for. The key itself still goes to the editor.
        case Rml::Input::KI_LEFT: case Rml::Input::KI_RIGHT: case Rml::Input::KI_HOME:
        case Rml::Input::KI_END: case Rml::Input::KI_PRIOR: case Rml::Input::KI_NEXT:
            close_completion();
            return;
        default: return;
        }
        ev.StopImmediatePropagation();
    }
};
KeyGate g_key_gate;

/// Render a few frames and write the framebuffer, so the chrome can be
/// inspected without a human at the window.
/// Write the current framebuffer to a PPM. Split out so the pre-IDE screens
/// (splash, welcome) can be verified the same way the IDE is — by looking at
/// what was actually drawn.
void dump_to(const char* path) {
    if (!path) return;
    if (std::getenv("KILN_DESIGNER_DEBUG")) {
        int dw = 0, dh = 0, ww = 0, wh = 0;
        if (SDL_Window* win = SDL_GL_GetCurrentWindow()) {
            SDL_GL_GetDrawableSize(win, &dw, &dh);
            SDL_GetWindowSize(win, &ww, &wh);
        }
        GLint vp[4] = {0, 0, 0, 0};
        glGetIntegerv(GL_VIEWPORT, vp);
        std::fprintf(stderr,
                     "dump: drawable %dx%d  window %dx%d  viewport %d,%d %dx%d  g.win %dx%d\n",
                     dw, dh, ww, wh, vp[0], vp[1], vp[2], vp[3], g.win_w, g.win_h);
    }
    auto* gl3 = static_cast<RenderInterface_GL3*>(Backend::GetRenderInterface());
    for (int i = 0; i < 3; i++) {
        g.context->Update();
        // The frame loop's per-frame work, or the dump shows a console whose
        // colour layer was never painted — an empty pane the user never sees.
        size_output_pane();
        follow_log();
        follow_code_scroll();
        sync_log_scroll();
        Backend::BeginFrame();
        g.context->Render();
        gl3->EndFrame();
    }
    // Read the ACTUAL drawable, not the size we asked for. A window manager may
    // hand back a smaller window than requested — and reading more rows than
    // exist fills the top of the image with undefined memory, which looks
    // exactly like a rendering bug that isn't there.
    int w = g.win_w, h = g.win_h;
    if (SDL_Window* win = SDL_GL_GetCurrentWindow()) {
        int dw = 0, dh = 0;
        SDL_GL_GetDrawableSize(win, &dw, &dh);
        if (dw > 0 && dh > 0) {
            w = dw;
            h = dh;
        }
    }
    std::vector<unsigned char> px((size_t)w * h * 3);
    glReadPixels(0, 0, w, h, GL_RGB, GL_UNSIGNED_BYTE, px.data());
    if (FILE* f = std::fopen(path, "wb")) {
        std::fprintf(f, "P6\n%d %d\n255\n", w, h);
        for (int y = h - 1; y >= 0; y--)
            std::fwrite(&px[(size_t)y * w * 3], 1, (size_t)w * 3, f);
        std::fclose(f);
    }
    std::printf("designer: wrote %s\n", path);
}

void dump_frame() { dump_to(std::getenv("KILN_DESIGNER_DUMP")); }

/// The element a script verb names: by element id, in the About box, or —
/// for a component on the canvas or in the tray — by the `oe-id` attribute
/// it carries instead. `form` is the preview's title bar, which selects the
/// form as a press on it does.
Rml::Element* script_target(const std::string& arg) {
    g.context->Update();
    Rml::Element* e = by_id(arg == "form" ? "formtitle" : arg.c_str());
    if (!e && g.about) e = g.about->GetElementById(arg);
    // The Settings dialog, the same way. A modal document no verb can address
    // is a page no test can check, which is where every UI bug has hidden.
    if (!e && g.settings_doc) {
        e = g.settings_doc->GetElementById(arg);
        if (!e) {
            // Its rows carry no ids — a control is named by the setting it
            // edits, which is what a test wants to say anyway.
            Rml::ElementList all;
            g.settings_doc->GetElementsByTagName(all, "span");
            Rml::ElementList inputs;
            g.settings_doc->GetElementsByTagName(inputs, "input");
            all.insert(all.end(), inputs.begin(), inputs.end());
            Rml::ElementList divs;
            g.settings_doc->GetElementsByTagName(divs, "div");
            all.insert(all.end(), divs.begin(), divs.end());
            for (Rml::Element* c : all) {
                const std::string field = c->GetAttribute<Rml::String>("oe-set-field", "");
                const std::string cat = c->GetAttribute<Rml::String>("oe-set-cat", "");
                const std::string key = c->GetAttribute<Rml::String>("oe-set-key", "");
                const std::string val = c->GetAttribute<Rml::String>("oe-set-val", "");
                // `key=value` addresses one chip; a bare key addresses a field
                // or a rail entry.
                if (arg == field || arg == cat || (!key.empty() && arg == key + "=" + val)) {
                    e = c;
                    break;
                }
            }
        }
    }
    if (e) return e;
    for (const char* holder : {"canvas", "traylist"}) {
        Rml::Element* box = by_id(holder);
        for (int i = 0; box && i < box->GetNumChildren(); i++) {
            if (box->GetChild(i)->GetAttribute<Rml::String>("oe-id", "") == arg) {
                e = box->GetChild(i);
            }
        }
    }
    return e;
}

/// Press `button` at the element's centre, through the context: the same
/// path a mouse takes, so whatever the listener does for the user's press it
/// does here. A row far down the inspector is clipped by the grid's scroll,
/// and a press at its centre would land on whatever is drawn there instead —
/// so it is scrolled on screen first, as the user's scrolling would.
void press_element(Rml::Element* e, int button, int times = 1) {
    e->ScrollIntoView(false);
    g.context->Update();
    const auto at = e->GetAbsoluteOffset(Rml::BoxArea::Border);
    const auto size = e->GetBox().GetSize(Rml::BoxArea::Border);
    const int cx = (int)(at.x + size.x / 2), cy = (int)(at.y + size.y / 2);
    for (int n = 0; n < times; n++) {
        g.context->ProcessMouseMove(cx, cy, 0);
        g.context->ProcessMouseButtonDown(button, 0);
        g.context->ProcessMouseButtonUp(button, 0);
        g.context->Update();
    }
}

void run_script(const char* script) {
    std::string s(script);
    size_t i = 0;
    while (i <= s.size()) {
        const size_t semi = s.find(';', i);
        const std::string cmd = s.substr(i, semi == std::string::npos ? std::string::npos : semi - i);
        if (!cmd.empty()) {
            const size_t colon = cmd.find(':');
            const std::string verb = cmd.substr(0, colon);
            const std::string arg = colon == std::string::npos ? "" : cmd.substr(colon + 1);
            flush_inspector();   // what the frame loop would have done by now
            if (verb == "add") add_component(arg);
            else if (verb == "select") select(arg);
            else if (verb == "rename") {
                // No caret to keep here, so the grid can show the new name.
                rename_selected(arg);
                rebuild_inspector();
            }
            else if (verb == "clickform") {
                // A press on the preview's title bar, through the context, as
                // the mouse does it; then what the press selected.
                g.context->Update();
                if (Rml::Element* t = by_id("formtitle")) {
                    const auto at = t->GetAbsoluteOffset(Rml::BoxArea::Border);
                    const auto size = t->GetBox().GetSize(Rml::BoxArea::Border);
                    g.context->ProcessMouseMove((int)(at.x + size.x / 2), (int)(at.y + size.y / 2), 0);
                    g.context->ProcessMouseButtonDown(0, 0);
                    g.context->ProcessMouseButtonUp(0, 0);
                    g.context->Update();
                }
                std::printf("clickform: selected %s\n", g.selected.c_str());
                std::fflush(stdout);
            }
            else if (verb == "props") {
                // The inspector as the user reads it: the header, then each
                // property row's field and value — and, for an anchors row,
                // which of its boxes are lit.
                g.context->Update();
                auto plain = [](std::string h) {
                    for (size_t a; (a = h.find('<')) != std::string::npos;) {
                        const size_t b = h.find('>', a);
                        h.erase(a, b == std::string::npos ? std::string::npos : b - a + 1);
                    }
                    return h;
                };
                std::string out;
                if (Rml::Element* ctx = by_id("ctxlabel")) out += plain(ctx->GetInnerRML());
                if (Rml::Element* grid = by_id("grid")) {
                    for (int i = 0; i < grid->GetNumChildren(); i++) {
                        Rml::Element* row = grid->GetChild(i);
                        for (int j = 0; j < row->GetNumChildren(); j++) {
                            Rml::Element* f = row->GetChild(j);
                            const Rml::String cls = f->GetAttribute<Rml::String>("class", "");
                            if (cls == "cid") {
                                out += " name=" + f->GetAttribute<Rml::String>("value", "");
                            } else if (cls.rfind("pv", 0) == 0) {
                                out += " " + f->GetAttribute<Rml::String>("name", "") + "=" +
                                       f->GetAttribute<Rml::String>("value", "");
                            } else if (cls == "anchbox") {
                                out += " [";
                                for (int k = 0; k < f->GetNumChildren(); k++) {
                                    if (f->GetChild(k)->GetAttribute<Rml::String>("class", "") ==
                                        "anch on") {
                                        out += f->GetChild(k)->GetAttribute<Rml::String>("oe-anchor", "") + " ";
                                    }
                                }
                                out += "]";
                            }
                        }
                    }
                }
                if (by_id("formsel")) out += " formsel=yes";
                std::printf("props: %s\n", out.c_str());
                std::fflush(stdout);
            }
            else if (verb == "set" || verb == "wire") {
                const size_t eq = arg.find('=');
                if (eq != std::string::npos && !g.selected.empty()) {
                    if (Component* c = g.model.find(g.selected)) {
                        const std::string k = arg.substr(0, eq), v = arg.substr(eq + 1);
                        if (verb == "set") {
                            // The same gate as the inspector: a scripted
                            // session must not be able to write what a
                            // user could not.
                            if (!write_prop(*c, k.c_str(), v)) {
                                std::printf("set: %s does not declare %s\n",
                                            c->type_name.c_str(), k.c_str());
                                std::fflush(stdout);
                            }
                        } else {
                            c->set_handler(k, v);
                            if (!g.model.has_sub(v)) g.pending_subs.push_back(v);
                        }
                        mark_dirty();
                        refresh_all();
                    }
                }
            } else if (verb == "drag") {
                // drag:<id>@<x0>,<y0>-><x1>,<y1> in CANVAS coordinates; exercises
                // exactly the path the mouse takes.
                const size_t at = arg.find('@'), arrow = arg.find("->");
                if (at != std::string::npos && arrow != std::string::npos) {
                    const std::string id = arg.substr(0, at);
                    int x0 = 0, y0 = 0, x1 = 0, y1 = 0;
                    std::sscanf(arg.c_str() + at + 1, "%d,%d", &x0, &y0);
                    std::sscanf(arg.c_str() + arrow + 2, "%d,%d", &x1, &y1);
                    if (Rml::Element* canvas = by_id("canvas")) {
                        const auto o = canvas->GetAbsoluteOffset();
                        begin_drag(id, (int)o.x + x0, (int)o.y + y0);
                        drag_to((int)o.x + x1, (int)o.y + y1);
                        g.dragging = false;
                    }
                }
            } else if (verb == "swatch" || verb == "browse") {
                // Open a property editor the way clicking its control does, so
                // a dumped frame can show what the user would see. Studio bugs
                // pass tests and fail on screen.
                if (Rml::Element* grid = by_id("grid")) {
                    const char* want = verb == "swatch" ? "oe-swatch" : "oe-file";
                    for (int i = 0; i < grid->GetNumChildren(); i++) {
                        Rml::Element* row = grid->GetChild(i);
                        for (int j = 0; j < row->GetNumChildren(); j++) {
                            Rml::Element* e = row->GetChild(j);
                            if (e->GetAttribute<Rml::String>(want, "") != arg) continue;
                            g.context->Update();   // offsets are stale until layout runs
                            g.editing_id = g.selected;
                            g.editing_prop = arg;
                            std::string html = "<div class='poptitle'>" + esc(arg) + "</div>";
                            if (verb == "swatch") {
                                for (const char* c : palette()) {
                                    html += "<div class='chip' oe-color='" + std::string(c) +
                                            "' style='background-color:" + c + "'/>";
                                }
                            } else {
                                for (const auto& f : project_files()) {
                                    html += "<div class='fileitem' oe-pick='" + esc(f) + "'>" +
                                            esc(f) + "</div>";
                                }
                            }
                            open_editpop(e, html);
                        }
                    }
                }
            } else if (verb == "pick") {
                set_edited_property(arg);
            } else if (verb == "view") {
                set_view(arg);
            } else if (verb == "winsize") {
                // Simulate an OS window resize, so the layout's response to one
                // can be tested without a window manager.
                int nw = 0, nh = 0;
                if (std::sscanf(arg.c_str(), "%dx%d", &nw, &nh) == 2 && nw > 400 && nh > 300) {
                    // Resize the real window too, or the GL framebuffer stays
                    // its original size and a screenshot shows phantom black
                    // margins that the running IDE would not have.
                    if (SDL_Window* win = SDL_GL_GetCurrentWindow()) {
                        SDL_SetWindowSize(win, nw, nh);
                        announce_window_size(win, nw, nh);
                        // Let the backend see the resize, so the renderer's
                        // viewport follows; otherwise only the layout changes.
                        for (int i = 0; i < 30; i++) Backend::ProcessEvents(g.context);
                        // And the viewport by hand: the offscreen driver's
                        // event does not always arrive, and a viewport left
                        // at the old size paints the new margin black.
                        static_cast<RenderInterface_GL3*>(Backend::GetRenderInterface())
                            ->SetViewport(nw, nh);
                    }
                    g.win_w = nw;
                    g.win_h = nh;
                    g.context->SetDimensions(Rml::Vector2i(nw, nh));
                    relayout();
                    rebuild_canvas();
                }
            } else if (verb == "settings") {
                open_settings(arg.empty() ? g.settings_cat : arg);
                g.context->Update();
                std::printf("settings: open %s\n", g.settings_cat.c_str());
            } else if (verb == "settingsclose") {
                close_settings();
                g.context->Update();
                std::printf("settings: closed\n");
            } else if (verb == "getsetting") {
                // The value in force, read back through the same accessor the
                // IDE uses — so the test checks what the IDE will do, not what
                // the file happens to say.
                std::printf("setting: %s = %s\n", arg.c_str(),
                            kiln::settings::text(arg).c_str());
            } else if (verb == "setsetting") {
                const size_t eq = arg.find('=');
                if (eq == std::string::npos) {
                    std::printf("setting: %s needs key=value\n", arg.c_str());
                } else {
                    const std::string key = arg.substr(0, eq), val = arg.substr(eq + 1);
                    const bool ok = kiln::settings::set(key, val);
                    if (ok) apply_setting(key);
                    std::printf("setting: %s = %s %s\n", key.c_str(),
                                kiln::settings::text(key).c_str(), ok ? "ok" : "refused");
                }
            } else if (verb == "settingsdump") {
                // Every row and its value, so a test can assert the page shows
                // what the store holds.
                for (const auto& r : kiln::settings::schema()) {
                    if (r.hidden) continue;
                    std::printf("row: %s|%s|%s|%s\n", r.category, r.key,
                                kiln::settings::text(r.key).c_str(),
                                kiln::settings::modified(r.key) ? "modified" : "default");
                }
            } else if (verb == "about") {
                show_about();
                g.context->Update();
            } else if (verb == "aboutstate") {
                g.context->Update();
                std::printf("about: %s\n", g.about ? "open" : "closed");
                std::fflush(stdout);
            } else if (verb == "clickat") {
                // A press at window coordinates, for what has no id — the
                // backdrop behind a dialog.
                int x = 0, y = 0;
                std::sscanf(arg.c_str(), "%d,%d", &x, &y);
                g.context->Update();
                g.context->ProcessMouseMove(x, y, 0);
                g.context->ProcessMouseButtonDown(0, 0);
                g.context->ProcessMouseButtonUp(0, 0);
                g.context->Update();
            } else if (verb == "pump") {
                // Let the window manager answer — a maximise asked for by a
                // button lands as a resize some events later — and follow
                // the window the way the interactive loop does each frame.
                for (int i = 0; i < 30; i++) {
                    Backend::ProcessEvents(g.context, nullptr, false);
                    SDL_Delay(10);
                }
                const auto dim = g.context->GetDimensions();
                if (dim.x != g.win_w || dim.y != g.win_h) {
                    g.win_w = dim.x;
                    g.win_h = dim.y;
                    relayout();
                    rebuild_canvas();
                }
                g.context->Update();
            } else if (verb == "quitcheck") {
                // Whether the event loop would now stop — how a close
                // request from the red dot is told from one that went
                // nowhere.
                const bool running = Backend::ProcessEvents(g.context, nullptr, false);
                std::printf("quitcheck: %s\n", running ? "running" : "quit");
                std::fflush(stdout);
            } else if (verb == "winflags") {
                // What the window manager did with the request; a dump cannot
                // show a minimised window.
                Uint32 flags = 0;
                int w = 0, h = 0;
                if (SDL_Window* win = SDL_GL_GetCurrentWindow()) {
                    flags = SDL_GetWindowFlags(win);
                    SDL_GetWindowSize(win, &w, &h);
                }
                std::printf("winflags: %dx%d%s%s%s\n", w, h,
                            flags & SDL_WINDOW_MAXIMIZED ? " maximized" : "",
                            flags & SDL_WINDOW_MINIMIZED ? " minimized" : "",
                            flags & SDL_WINDOW_BORDERLESS ? " borderless" : "");
                std::fflush(stdout);
            } else if (verb == "undo") undo();
            else if (verb == "redo") redo();
            else if (verb == "copy") copy_selection();
            else if (verb == "paste") paste_clipboard();
            else if (verb == "delete") delete_selection();
            else if (verb == "addsel") select(arg, true);
            else if (verb == "type") {
                // Simulate typing in the code editor: set its value and fire
                // the same path a keystroke takes, so the test exercises the
                // real dirty-tracking rather than a shortcut around it.
                if (auto* ed = code_editor()) {
                    ed->SetValue(arg);
                    g.code_dirty = true;
                    g.dirty = true;
                }
            }
            else if (verb == "codescroll") {
                // What the colour layer is offset by, so a test can see the
                // two layers agree without looking at pixels.
                g.context->Update();
                follow_code_scroll();
                std::printf("codescroll: x=%d\n", g.code_scroll_x);
                std::fflush(stdout);
            }
            else if (verb == "codetext") {
                if (auto* ed = code_editor()) {
                    g.context->Update();   // offsets are stale until layout runs
                    std::printf("codetext: %zu chars\n", ed->GetValue().size());
                    Rml::Element* e = by_id("fullcode");
                    std::printf("box: %.0fx%.0f at %.0f,%.0f  children=%d\n",
                                e->GetOffsetWidth(), e->GetOffsetHeight(),
                                e->GetAbsoluteLeft(), e->GetAbsoluteTop(),
                                e->GetNumChildren());
                    if (const auto* c = e->GetProperty(Rml::PropertyId::Color))
                        std::printf("color: %s\n", c->ToString().c_str());
                    if (const auto* fs = e->GetProperty(Rml::PropertyId::FontSize))
                        std::printf("font-size: %s\n", fs->ToString().c_str());
                    for (int ci = 0; ci < e->GetNumChildren(); ci++) {
                        Rml::Element* ch = e->GetChild(ci);
                        std::printf("  child %d <%s> %.0fx%.0f\n", ci, ch->GetTagName().c_str(),
                                    ch->GetOffsetWidth(), ch->GetOffsetHeight());
                    }
                    Rml::Element* cv = by_id("codeview");
                    std::printf("codeview: %.0fx%.0f display=%s\n", cv->GetOffsetWidth(),
                                cv->GetOffsetHeight(),
                                cv->GetProperty(Rml::PropertyId::Display)->ToString().c_str());
                    std::fflush(stdout);
                }
            }
            else if (verb == "save") save();
            else if (verb == "click" || verb == "dblclick" || verb == "rclick") {
                // A real press through the context, on the element with that
                // id — the same path a mouse takes, so a click on the Code tab
                // here runs set_view from inside the event listener exactly
                // as the user's click does. Twice for a double-click, which is
                // detected by the listener rather than the substrate; with the
                // right button for rclick, which opens the context menu.
                if (Rml::Element* e = script_target(arg)) {
                    press_element(e, verb == "rclick" ? 1 : 0, verb == "dblclick" ? 2 : 1);
                    std::printf("%s: %s\n", verb.c_str(), arg.c_str());
                } else {
                    std::printf("%s: %s NOT FOUND\n", verb.c_str(), arg.c_str());
                }
                std::fflush(stdout);
            }
            else if (verb == "menu" || verb == "menupick") {
                // The context menu's rows, by the labels they carry; menupick
                // presses the row with that label, as the mouse would.
                g.context->Update();
                Rml::Element* pop = by_id("ctxmenu");
                std::vector<Rml::Element*> rows;
                for (int i = 0; pop && !g.ctx_id.empty() && i < pop->GetNumChildren(); i++) {
                    if (pop->GetChild(i)->HasAttribute("oe-cxlabel")) rows.push_back(pop->GetChild(i));
                }
                if (verb == "menu") {
                    if (g.ctx_id.empty()) {
                        std::printf("menu: closed\n");
                    } else {
                        std::printf("menu: open rows=%zu\n", rows.size());
                        for (Rml::Element* r : rows) {
                            std::printf("  %s\n", r->GetAttribute<Rml::String>("oe-cxlabel", "").c_str());
                        }
                    }
                } else {
                    Rml::Element* hit = nullptr;
                    for (Rml::Element* r : rows) {
                        if (r->GetAttribute<Rml::String>("oe-cxlabel", "") == arg) hit = r;
                    }
                    if (hit) {
                        press_element(hit, 0);
                        std::printf("menupick: %s\n", arg.c_str());
                    } else {
                        std::printf("menupick: %s NOT FOUND\n", arg.c_str());
                    }
                }
                std::fflush(stdout);
            }
            else if (verb == "menubar" || verb == "menubarhover") {
                // The menu bar, by title. `menubar` clicks it as the mouse
                // would — which is what makes the second click on an open
                // title testable — and `menubarhover` only moves over it, so
                // hover-to-switch is exercised through the same event path a
                // real pointer takes rather than by calling `open_menu`.
                Rml::Element* title = nullptr;
                if (Rml::Element* bar = by_id("menubar")) {
                    for (int i = 0; i < bar->GetNumChildren(); i++) {
                        Rml::Element* c = bar->GetChild(i);
                        const int idx = c->GetAttribute<int>("oe-menu", -1);
                        if (idx >= 0 && (size_t)idx < menus().size() &&
                            menus()[(size_t)idx].title == arg) {
                            title = c;
                            break;
                        }
                    }
                }
                if (!title) {
                    std::printf("%s: %s NOT FOUND\n", verb.c_str(), arg.c_str());
                } else if (verb == "menubar") {
                    press_element(title, 0);
                    std::printf("menubar: %s\n", arg.c_str());
                } else {
                    g.context->Update();
                    const auto at = title->GetAbsoluteOffset(Rml::BoxArea::Border);
                    const auto size = title->GetBox().GetSize(Rml::BoxArea::Border);
                    g.context->ProcessMouseMove((int)(at.x + size.x / 2),
                                                (int)(at.y + size.y / 2), 0);
                    g.context->Update();
                    std::printf("menubarhover: %s\n", arg.c_str());
                }
                std::fflush(stdout);
            }
            else if (verb == "menubarstate") {
                if (g_open_menu < 0) {
                    std::printf("menubarstate: closed\n");
                } else {
                    std::printf("menubarstate: open %s\n",
                                menus()[(size_t)g_open_menu].title);
                    if (Rml::Element* pop = by_id("menupop")) {
                        for (int i = 0; i < pop->GetNumChildren(); i++) {
                            std::printf("  %s\n",
                                        pop->GetChild(i)
                                            ->GetAttribute<Rml::String>("oe-action", "")
                                            .c_str());
                        }
                    }
                }
                std::fflush(stdout);
            }
            else if (verb == "frame") {
                // One pass of the interactive loop's body, then a present, so
                // what a dump shows is what the first frame after a gesture
                // shows — not the third.
                sync_highlight_scroll();
                g.lsp.poll();
                if (g.lsp.has_update()) {
                    g.lsp.clear_update();
                    g.diagnostics = g.lsp.diagnostics();
                    render_diagnostics();
                }
                hover_tick();
                poll_answers();
                update_tabs();
                g.context->Update();
                size_output_pane();
                follow_log();
                sync_log_scroll();
                Backend::BeginFrame();
                g.context->Render();
                Backend::PresentFrame();
            }
            else if (verb == "hovergrip") {
                // Rest the pointer on a selection handle and report the cursor
                // the platform was asked for. A dump cannot show the pointer.
                g.context->Update();
                Rml::Element* found = nullptr;
                if (Rml::Element* overlay = by_id("overlay")) {
                    for (int i = 0; i < overlay->GetNumChildren(); i++) {
                        Rml::Element* h = overlay->GetChild(i);
                        if (h->GetAttribute<Rml::String>("oe-grip", "") == arg ||
                            h->GetAttribute<Rml::String>("oe-formgrip", "") == arg) {
                            found = h;
                        }
                    }
                }
                if (found) {
                    const auto at = found->GetAbsoluteOffset(Rml::BoxArea::Border);
                    const auto size = found->GetBox().GetSize(Rml::BoxArea::Border);
                    g.context->ProcessMouseMove((int)(at.x + size.x / 2), (int)(at.y + size.y / 2), 0);
                    g.context->Update();
                }
                Rml::Element* over = g.context->GetHoverElement();
                std::printf("cursor: %s %s  (over <%s class='%s' cursor=%s>)\n", arg.c_str(),
                            found ? g.cursor_name.c_str() : "(no such grip)",
                            over ? over->GetTagName().c_str() : "none",
                            over ? over->GetAttribute<Rml::String>("class", "").c_str() : "",
                            over ? over->GetProperty(Rml::PropertyId::Cursor)->ToString().c_str()
                                 : "");
                std::fflush(stdout);
            }
            else if (verb == "grip") {
                // grip:<edge>@<dx>,<dy> — press a selection anchor and drag it,
                // through the context, so the listener's resize path runs the
                // way a mouse runs it. Reports the rect the model now holds:
                // whether a property was written is the point of the test.
                const size_t at = arg.find('@');
                const std::string edge = arg.substr(0, at);
                int dx = 0, dy = 0;
                if (at != std::string::npos) std::sscanf(arg.c_str() + at + 1, "%d,%d", &dx, &dy);
                g.context->Update();
                Rml::Element* found = nullptr;
                if (Rml::Element* overlay = by_id("overlay")) {
                    for (int i = 0; i < overlay->GetNumChildren(); i++) {
                        Rml::Element* h = overlay->GetChild(i);
                        if (h->GetAttribute<Rml::String>("oe-grip", "") == edge) found = h;
                    }
                }
                if (found) {
                    const auto o = found->GetAbsoluteOffset(Rml::BoxArea::Border);
                    const auto sz = found->GetBox().GetSize(Rml::BoxArea::Border);
                    const int cx = (int)(o.x + sz.x / 2), cy = (int)(o.y + sz.y / 2);
                    g.context->ProcessMouseMove(cx, cy, 0);
                    g.context->ProcessMouseButtonDown(0, 0);
                    g.context->ProcessMouseMove(cx + dx, cy + dy, 0);
                    g.context->ProcessMouseButtonUp(0, 0);
                    g.context->Update();
                }
                const Component* c = g.model.find(g.selected);
                auto shown = [&](const char* p) {
                    const std::string* v = c ? c->property(p) : nullptr;
                    return v ? *v : std::string("-");
                };
                std::printf("grip: %s %s left=%s top=%s width=%s height=%s\n", edge.c_str(),
                            found ? "dragged" : "(no such grip)", shown("left").c_str(),
                            shown("top").c_str(), shown("width").c_str(),
                            shown("height").c_str());
                std::fflush(stdout);
            }
            else if (verb == "formgrip") {
                // formgrip:<edge>@<dx>,<dy> — drag the form's own resize grip
                // and report where every child ended up, anchored or not.
                const size_t at = arg.find('@');
                const std::string edge = arg.substr(0, at);
                int dx = 0, dy = 0;
                if (at != std::string::npos) std::sscanf(arg.c_str() + at + 1, "%d,%d", &dx, &dy);
                g.context->Update();
                Rml::Element* found = nullptr;
                if (Rml::Element* overlay = by_id("overlay")) {
                    for (int i = 0; i < overlay->GetNumChildren(); i++) {
                        Rml::Element* h = overlay->GetChild(i);
                        if (h->GetAttribute<Rml::String>("oe-formgrip", "") == edge) found = h;
                    }
                }
                if (found) {
                    const auto o = found->GetAbsoluteOffset(Rml::BoxArea::Border);
                    const auto sz = found->GetBox().GetSize(Rml::BoxArea::Border);
                    const int cx = (int)(o.x + sz.x / 2), cy = (int)(o.y + sz.y / 2);
                    g.context->ProcessMouseMove(cx, cy, 0);
                    g.context->ProcessMouseButtonDown(0, 0);
                    g.context->ProcessMouseMove(cx + dx, cy + dy, 0);
                    g.context->ProcessMouseButtonUp(0, 0);
                    g.context->Update();
                }
                std::printf("formgrip: %s %s form=%dx%d\n", edge.c_str(),
                            found ? "dragged" : "(no such grip)",
                            prop_int(g.model.form, "width", 0), prop_int(g.model.form, "height", 0));
                for (const auto& c : g.model.children) {
                    const std::string* a = c.property("anchors");
                    std::printf("  %s %d,%d %dx%d anchors=%s\n", c.id.c_str(), prop_int(c, "left", 0),
                                prop_int(c, "top", 0), prop_int(c, "width", 0), prop_int(c, "height", 0),
                                a ? a->c_str() : "-");
                }
                std::fflush(stdout);
            }
            else if (verb == "hoverat" || verb == "gotodef" || verb == "refs") {
                // The language-server gestures, at a 1-based line:column in
                // the editor. `hoverat` rests the pointer on that spot and
                // reports the tip; the other two move the caret there and
                // press F12 / Shift+F12 through the same functions the key
                // reaches, then report where the caret went or what was found.
                int line = 1, col = 1;
                std::sscanf(arg.c_str(), "%d,%d", &line, &col);
                line--;
                col--;
                if (g.view != "code") set_view("code");
                g.context->Update();
                if (verb == "hoverat") {
                    // Into view first: a spot below the fold is not a spot the
                    // pointer can rest on.
                    jump_to(line, col);
                    g.context->Update();
                    Rml::Element* ed = by_id("fullcode");
                    const auto at = ed->GetAbsoluteOffset(Rml::BoxArea::Border);
                    const int mx = (int)at.x + theme::CODE_PAD_X + col * code_char_width() + 2;
                    const int my = (int)at.y + line * theme::CODE_LINE_H - g.code_scroll + 4;
                    g.context->ProcessMouseMove(mx, my, 0);
                    g.context->Update();
                    g.hover_moved_at -= 1.0;      // the rest has already happened
                    hover_tick();
                    kiln::json::Value v;
                    std::string text;
                    if (g.hover_request && g.lsp.wait(g.hover_request, v, 3000)) {
                        text = kiln::lsp::hover_text(v);
                        g.hover_request = 0;
                        if (!text.empty()) show_tip(text, mx, my);
                    }
                    for (char& ch : text) {
                        if (ch == '\n') ch = ' ';
                    }
                    Rml::Element* tip = by_id("tip");
                    std::printf("hover: %d,%d asked=%d,%d tip=%s | %s\n", line + 1, col + 1,
                                g.hover_line + 1, g.hover_col + 1,
                                tip ? tip->GetProperty(Rml::PropertyId::Display)->ToString().c_str()
                                    : "?",
                                text.c_str());
                } else {
                    jump_to(line, col);
                    g.context->Update();
                    if (verb == "gotodef") goto_definition();
                    else find_references();
                    const int id = verb == "gotodef" ? g.def_request : g.refs_request;
                    if (id) g.lsp.pump_until(id, 3000);
                    // The reply is delivered the way the frame loop delivers it.
                    poll_answers();
                    g.context->Update();
                    int cl = 0, cc = 0;
                    caret_position(cl, cc);
                    if (verb == "gotodef") {
                        std::printf("definition: caret %d,%d scroll=%d\n", cl + 1, cc + 1,
                                    g.code_scroll);
                    } else {
                        std::printf("references: %zu\n", g.refs.size());
                        for (const auto& r : g.refs) {
                            std::printf("  line %d:%d\n", r.range.line + 1, r.range.character + 1);
                        }
                    }
                }
                std::fflush(stdout);
            }
            else if (verb == "goto") {
                // goto:<line>,<col> — the caret to a 1-based spot, in view.
                int line = 1, col = 1;
                std::sscanf(arg.c_str(), "%d,%d", &line, &col);
                if (g.view != "code") set_view("code");
                g.context->Update();
                jump_to(line - 1, col - 1);
                g.context->Update();
            }
            else if (verb == "typein") {
                // Type through the context, one character at a time, so each
                // keystroke runs the editor's change handler — and with it
                // the completion trigger. `type` sets the value wholesale and
                // fires nothing, which is a different thing to test.
                for (char c : arg) {
                    if (c == '\n') {
                        g.context->ProcessKeyDown(Rml::Input::KI_RETURN, 0);
                        g.context->ProcessTextInput('\n');
                    } else {
                        g.context->ProcessTextInput(Rml::String(1, c));
                    }
                    g.context->Update();
                }
            }
            else if (verb == "key") {
                // A named key through the context: up, down, enter, tab,
                // escape, back, ctrl-space. Enter and Ctrl+Space also send the
                // character the platform sends after them, so the swallow
                // path is exercised too.
                const bool ctrl = arg.rfind("ctrl-", 0) == 0;
                const bool shift = arg.rfind("shift-", 0) == 0;
                const std::string name = ctrl ? arg.substr(5) : shift ? arg.substr(6) : arg;
                const int mod = ctrl ? Rml::Input::KM_CTRL : shift ? Rml::Input::KM_SHIFT : 0;
                Rml::Input::KeyIdentifier key = Rml::Input::KI_UNKNOWN;
                if (name == "up") key = Rml::Input::KI_UP;
                else if (name == "left") key = Rml::Input::KI_LEFT;
                else if (name == "right") key = Rml::Input::KI_RIGHT;
                else if (name == "delete") key = Rml::Input::KI_DELETE;
                else if (name == "f12") key = Rml::Input::KI_F12;
                else if (name == "z") key = Rml::Input::KI_Z;
                else if (name == "y") key = Rml::Input::KI_Y;
                else if (name == "down") key = Rml::Input::KI_DOWN;
                else if (name == "enter") key = Rml::Input::KI_RETURN;
                else if (name == "tab") key = Rml::Input::KI_TAB;
                else if (name == "escape") key = Rml::Input::KI_ESCAPE;
                else if (name == "back") key = Rml::Input::KI_BACK;
                else if (name == "space") key = Rml::Input::KI_SPACE;
                else if (name == "home") key = Rml::Input::KI_HOME;
                else if (name == "end") key = Rml::Input::KI_END;
                g.context->ProcessKeyDown(key, mod);
                if (key == Rml::Input::KI_RETURN) g.context->ProcessTextInput('\n');
                if (key == Rml::Input::KI_SPACE) g.context->ProcessTextInput(' ');
                g.context->Update();
                // Where the key went is the whole question for a shortcut: a
                // context with no focus hands keys to its root, and nothing
                // registered on a document hears those.
                const Rml::Element* focus = g.context->GetFocusElement();
                std::printf("key: %s focus=%s\n", arg.c_str(),
                            focus ? (focus->GetId().empty() ? focus->GetTagName().c_str()
                                                            : focus->GetId().c_str())
                                  : "none");
                std::fflush(stdout);
            }
            else if (verb == "waitdef") {
                // The answer to a definition request the way the frame loop
                // delivers it; the key path that asked for it cannot wait.
                if (g.def_request) g.lsp.pump_until(g.def_request, 3000);
                poll_answers();
                g.context->Update();
                int cl = 0, cc = 0;
                caret_position(cl, cc);
                std::printf("definition: caret %d,%d scroll=%d\n", cl + 1, cc + 1, g.code_scroll);
                std::fflush(stdout);
            }
            else if (verb == "waitcomplete") {
                // Wait for the completion in flight the way the frame loop
                // would deliver it, then report the popup: open or not, what
                // it holds, which row is selected, and where it was drawn.
                if (g.complete_request) g.lsp.pump_until(g.complete_request, 3000);
                poll_answers();
                g.context->Update();
                Rml::Element* pop = by_id("complete");
                std::string labels;
                for (size_t i = 0; i < g.complete_shown.size() && i < 6; i++) {
                    labels += (i ? "," : "") + g.complete_all[g.complete_shown[i]]["label"].str();
                }
                std::printf("complete: open=%d offered=%zu shown=%zu index=%d selected=%s at=%s,%s "
                            "display=%s | %s\n",
                            g.complete_open ? 1 : 0, g.complete_all.size(),
                            g.complete_shown.size(), g.complete_index,
                            g.complete_open
                                ? g.complete_all[g.complete_shown[(size_t)g.complete_index]]["label"]
                                      .str().c_str()
                                : "-",
                            pop ? pop->GetProperty(Rml::PropertyId::Left)->ToString().c_str() : "?",
                            pop ? pop->GetProperty(Rml::PropertyId::Top)->ToString().c_str() : "?",
                            pop ? pop->GetProperty(Rml::PropertyId::Display)->ToString().c_str() : "?",
                            labels.c_str());
                std::fflush(stdout);
            }
            else if (verb == "bufline" || verb == "buftail") {
                // What the editor holds: one 1-based line, or the last n.
                if (auto* ed = code_editor()) {
                    const std::string text = ed->GetValue();
                    int count = 1;
                    for (char c : text) count += c == '\n';
                    const int n = std::atoi(arg.c_str());
                    const int from = verb == "bufline" ? n : std::max(1, count - n + 1);
                    const int to = verb == "bufline" ? n : count;
                    for (int l = from; l <= to; l++) {
                        std::printf("buf %d: %s\n", l, line_text(text, l - 1).c_str());
                    }
                    std::fflush(stdout);
                }
            }
            else if (verb == "tabs") {
                // The labels as written to the tabs, after the server has
                // had a moment to answer for the symbols they depend on.
                if (g.symbol_request) g.lsp.pump_until(g.symbol_request, 3000);
                poll_answers();
                update_tabs();
                auto plain = [](std::string h) {
                    for (size_t a; (a = h.find('<')) != std::string::npos;) {
                        const size_t b = h.find('>', a);
                        h.erase(a, b == std::string::npos ? std::string::npos : b - a + 1);
                    }
                    return h;
                };
                std::printf("tabs: %s | %s\n", plain(g.tab_designer_label).c_str(),
                            plain(g.tab_code_label).c_str());
                std::fflush(stdout);
            }
            else if (verb == "geometry") {
                // geometry:<id> — the title bar's height, where the client
                // area starts under it, and where the component was drawn
                // relative to that client area. The last is what must equal
                // the file's `top`, whatever is drawn above.
                g.context->Update();
                Rml::Element* win = by_id("formwin");
                Rml::Element* title = by_id("formtitle");
                Rml::Element* canvas = by_id("canvas");
                if (win && title && canvas) {
                    const auto wo = win->GetAbsoluteOffset(Rml::BoxArea::Border);
                    const auto co = canvas->GetAbsoluteOffset(Rml::BoxArea::Border);
                    int x = 0, y = 0, w = 0, h = 0;
                    const bool found = measure_component(canvas, arg, x, y, w, h);
                    std::printf("geometry: title=%d client_top=%d %s=%s%d,%d form=%dx%d icon=%s\n",
                                (int)title->GetBox().GetSize(Rml::BoxArea::Border).y,
                                (int)(co.y - wo.y), arg.c_str(), found ? "" : "missing ", x, y,
                                (int)canvas->GetBox().GetSize().x,
                                (int)canvas->GetBox().GetSize().y,
                                basename_of(g.form_icon_src).c_str());
                    std::fflush(stdout);
                }
            }
            else if (verb == "wiring" || verb == "badges") {
                // What the wiring box, or the canvas badges, say — with the
                // markup stripped, so the line reads as the user reads it.
                g.context->Update();
                auto plain = [](std::string h) {
                    for (size_t a; (a = h.find('<')) != std::string::npos;) {
                        const size_t b = h.find('>', a);
                        h.erase(a, b == std::string::npos ? std::string::npos : b - a + 1);
                    }
                    return h;
                };
                if (verb == "wiring") {
                    if (Rml::Element* w = by_id("wirebox")) {
                        std::printf("wiring: %s\n", plain(w->GetInnerRML()).c_str());
                    }
                } else if (Rml::Element* overlay = by_id("overlay")) {
                    for (int i = 0; i < overlay->GetNumChildren(); i++) {
                        Rml::Element* b = overlay->GetChild(i);
                        if (!b->HasAttribute("oe-jump")) continue;
                        std::printf("badge: %s %s at %s,%s\n",
                                    b->GetAttribute<Rml::String>("oe-jump", "").c_str(),
                                    plain(b->GetInnerRML()).c_str(),
                                    b->GetProperty<Rml::String>("left").c_str(),
                                    b->GetProperty<Rml::String>("top").c_str());
                    }
                }
                std::fflush(stdout);
            }
            else if (verb == "events") {
                // The Events tab's rows: each event and the handler its field
                // holds, as the inspector shows them.
                g.context->Update();
                std::string out;
                if (Rml::Element* grid = by_id("grid")) {
                    for (int i = 0; i < grid->GetNumChildren(); i++) {
                        Rml::Element* row = grid->GetChild(i);
                        for (int j = 0; j < row->GetNumChildren(); j++) {
                            Rml::Element* f = row->GetChild(j);
                            if (f->GetAttribute<Rml::String>("class", "") != "ev") continue;
                            out += " " + f->GetAttribute<Rml::String>("name", "") + "=" +
                                   f->GetAttribute<Rml::String>("value", "");
                        }
                    }
                }
                std::printf("events:%s\n", out.c_str());
                std::fflush(stdout);
            }
            else if (verb == "caret") {
                g.context->Update();
                int cl = 0, cc = 0;
                caret_position(cl, cc);
                std::printf("caret: %d,%d scroll=%d\n", cl + 1, cc + 1, g.code_scroll);
                std::fflush(stdout);
            }
            else if (verb == "buildstart") {
                // Start a build and leave it running, so a dumped frame shows
                // the IDE mid-build with its activity indicator up.
                build_binary(false);
                auto bar_pos = [&] {
                    Rml::Element* b = by_id("activitybar");
                    return b ? b->GetAbsoluteLeft() : -1.f;
                };
                for (int i = 0; i < 10; i++) { poll_build(); g.context->Update(); usleep(20000); }
                const float p1 = bar_pos();
                for (int i = 0; i < 20; i++) { poll_build(); g.context->Update(); usleep(20000); }
                const float p2 = bar_pos();
                std::printf("bar: %.1f -> %.1f  (%s)\n", p1, p2,
                            p1 != p2 ? "ANIMATING" : "STATIC");
                std::fflush(stdout);
            }
            else if (verb == "runstart") {
                build_binary(true);
                auto lamp = [&] {
                    Rml::Element* e = by_id("runlamp");
                    if (!e) return -1.f;
                    const auto* p = e->GetProperty(Rml::PropertyId::Opacity);
                    return p ? p->Get<float>() : -1.f;
                };
                for (int i = 0; i < 2400 && g.running_app <= 0; i++) {
                    poll_build(); g.context->Update(); usleep(20000);
                }
                for (int i = 0; i < 5; i++) { poll_app(); g.context->Update(); usleep(20000); }
                const float o1 = lamp();
                for (int i = 0; i < 15; i++) { poll_app(); g.context->Update(); usleep(20000); }
                const float o2 = lamp();
                Rml::Element* e = by_id("runlamp");
                std::printf("lamp display=%s opacity %.2f -> %.2f (%s)\n",
                            e ? e->GetProperty(Rml::PropertyId::Display)->ToString().c_str() : "?",
                            o1, o2, o1 != o2 ? "PULSING" : "STATIC");
                std::fflush(stdout);
            }
            // The debugging verbs. `waitstop` blocks on the adapter's own
            // `stopped` event through the same pump the LSP verbs use for an
            // asynchronous reply, so there is no new machinery here.
            else if (verb == "bp") {
                toggle_breakpoint(std::atoi(arg.c_str()));
                std::printf("bp: %s -> %s\n", arg.c_str(),
                            g.breakpoints.count(std::atoi(arg.c_str())) ? "set" : "cleared");
                std::fflush(stdout);
            }
            else if (verb == "dbgrun") {
                start_debug();
                for (int i = 0; i < 2000 && g.dbg.running() && g.stopped_line == 0; i++) {
                    poll_debug();
                    g.context->Update();
                    usleep(20000);
                }
            }
            else if (verb == "waitstop") {
                const int was = g.stopped_line;
                for (int i = 0; i < 1000 && g.dbg.running() &&
                                (g.stopped_line == was || g.stopped_line == 0);
                     i++) {
                    poll_debug();
                    g.context->Update();
                    usleep(20000);
                }
                std::printf("waitstop: line %d\n", g.stopped_line);
                std::fflush(stdout);
            }
            else if (verb == "dbgnext" || verb == "dbgstep" || verb == "dbgout" ||
                     verb == "dbgcont") {
                if (verb == "dbgnext") g.dbg.step_over();
                else if (verb == "dbgstep") g.dbg.step_in();
                else if (verb == "dbgout") g.dbg.step_out();
                else g.dbg.continue_();
                const int was = g.stopped_line;
                for (int i = 0; i < 1000 && g.dbg.running() &&
                                (g.stopped_line == was || g.stopped_line == 0);
                     i++) {
                    poll_debug();
                    g.context->Update();
                    usleep(20000);
                }
                std::printf("%s: line %d\n", verb.c_str(), g.stopped_line);
                std::fflush(stdout);
            }
            else if (verb == "locals") {
                for (const auto& v : g.dbg.locals()) {
                    std::printf("local: %s = %s\n", v.name.c_str(), v.value.c_str());
                }
                std::printf("locals: %d\n", (int)g.dbg.locals().size());
                std::fflush(stdout);
            }
            else if (verb == "frames") {
                for (const auto& f : g.dbg.frames()) {
                    std::printf("frame: %s:%d\n", f.name.c_str(), f.line);
                }
                std::fflush(stdout);
            }
            else if (verb == "gutter") {
                // What the gutter is actually showing, which is the thing a
                // rendered frame is checked against.
                if (Rml::Element* e = by_id(arg.empty() ? "dbggutter" : arg.c_str())) {
                    std::printf("gutter: %s\n", e->GetInnerRML().c_str());
                } else {
                    std::printf("gutter: (absent)\n");
                }
                std::fflush(stdout);
            }
            else if (verb == "hoverval") {
                // hoverval:<line>,<col> — what hovering a name would show
                // while the program is stopped, without a pointer to move.
                int line = 1, col = 1;
                std::sscanf(arg.c_str(), "%d,%d", &line, &col);
                const std::string word = identifier_at(line - 1, col - 1);
                std::string value = "(none)";
                for (const auto& v : g.dbg.locals()) {
                    if (v.name == word) value = v.value;
                }
                std::printf("hoverval: %s = %s\n", word.c_str(), value.c_str());
                std::fflush(stdout);
            }
            else if (verb == "help") {
                // help:<page> or help:<page>#<anchor>. Bare `help` opens the
                // page the viewer was last on, which is what F1 does.
                std::string page = arg, anchor;
                const size_t h = page.find('#');
                if (h != std::string::npos) { anchor = page.substr(h + 1); page = page.substr(0, h); }
                open_help(page.empty() ? g.help_page : page, anchor, "");
            }
            else if (verb == "helpselect") {
                // Press a code block's `select` button, in the help document —
                // `click:` looks in the main one and never sees it.
                Rml::Element* hit = nullptr;
                std::function<void(Rml::Element*)> walk = [&](Rml::Element* e) {
                    if (!e || hit) return;
                    if (e->GetAttribute<Rml::String>("oe-help-select", "") == arg) { hit = e; return; }
                    for (int i = 0; i < e->GetNumChildren(); i++) walk(e->GetChild(i));
                };
                if (g.help) walk(g.help);
                if (hit) press_element(hit, 0);
                else std::printf("helpselect: %s NOT FOUND\n", arg.c_str());
                std::fflush(stdout);
            }
            else if (verb == "helpsearch") {
                open_help(g.help_page, "", arg);
            }
            else if (verb == "f1") {
                // F1 with the caret wherever `goto:` last put it — the whole
                // point of context help, and the one thing a click cannot test.
                context_help();
            }
            else if (verb == "helptext") {
                // What the viewer is actually showing, as text. A rendered
                // page that silently came out empty looks identical to a
                // missing one until something prints it.
                std::string out = "(closed)";
                if (g.help) {
                    if (Rml::Element* b = g.help->GetElementById("body")) {
                        // Tags out, text in. The document is ours, so there is
                        // no attribute value that can contain a stray '>'.
                        const std::string rml = b->GetInnerRML();
                        out.clear();
                        bool in_tag = false;
                        for (char c : rml) {
                            if (c == '<') { in_tag = true; continue; }
                            if (c == '>') { in_tag = false; out += ' '; continue; }
                            if (!in_tag) out += c;
                        }
                        if (out.size() > 400) out = out.substr(0, 400);
                    }
                }
                std::printf("helptext: %s\n", out.c_str());
                std::fflush(stdout);
            }
            else if (verb == "dbgstop") {
                stop_debug();
                std::printf("dbgstop: done\n");
                std::fflush(stdout);
            }
            else if (verb == "runstate") {
                std::printf("runstate: app=%s debug=%s stopped_line=%d run_blocked=%s\n",
                            app_alive() ? "yes" : "no", g.dbg.running() ? "yes" : "no",
                            g.stopped_line,
                            (by_id("btn_run") && by_id("btn_run")->IsClassSet("blocked"))
                                ? "yes" : "no");
                std::fflush(stdout);
            }
            else if (verb == "stop") {
                stop_all();
                std::printf("stop: done\n");
                std::fflush(stdout);
            }
            else if (verb == "runbg") {
                // Run, but stop pumping the moment the app is alive rather
                // than when it exits — the only way a script can be in the
                // state "an app is running" and try something from there.
                build_binary(true);
                for (int i = 0; i < 2400 && g.build_pid > 0 && g.running_app <= 0; i++) {
                    poll_build();
                    poll_app();
                    g.context->Update();
                    usleep(20000);
                }
                std::printf("runbg: pid=%d\n", (int)g.running_app);
                std::fflush(stdout);
            }
            else if (verb == "build" || verb == "run") {
                build_binary(verb == "run");
                // Pump the same polls the frame loop does, so a scripted
                // session exercises the real asynchronous path.
                for (int i = 0; i < 2400 && (g.build_pid > 0 || g.running_app > 0); i++) {
                    poll_build();
                    poll_app();
                    g.context->Update();
                    size_output_pane();
                    follow_log();
                    sync_log_scroll();
                    usleep(20000);
                }
                poll_build();
                poll_app();
            }
            else if (verb == "typetest") {
                // Prove the real editing path: focus the control the way a
                // click does, then push a character through the context the
                // way a keystroke does.
                Rml::Element* e = by_id("fullcode");
                // First: what a real click does. Move the mouse over the
                // editor and press, exactly as the backend would.
                g.context->Update();
                const int cx = (int)(e->GetAbsoluteLeft() + 40);
                const int cy = (int)(e->GetAbsoluteTop() + 40);
                g.context->ProcessMouseMove(cx, cy, 0);
                g.context->ProcessMouseButtonDown(0, 0);
                g.context->ProcessMouseButtonUp(0, 0);
                g.context->Update();
                std::printf("after-click focus: %s\n",
                            g.doc->GetFocusLeafNode() ? g.doc->GetFocusLeafNode()->GetId().c_str()
                                                      : "(none)");
                Rml::Element* hit = g.context->GetHoverElement();
                std::printf("hover element: <%s> id=%s\n",
                            hit ? hit->GetTagName().c_str() : "(none)",
                            hit ? hit->GetId().c_str() : "");
                const bool focused = e && e->Focus();
                std::printf("focus: %s\n", focused ? "yes" : "NO");
                std::printf("focus-element: %s\n",
                            g.doc->GetFocusLeafNode() ? g.doc->GetFocusLeafNode()->GetId().c_str()
                                                      : "(none)");
                auto* ed = code_editor();
                const size_t before = ed ? ed->GetValue().size() : 0;
                g.context->ProcessTextInput(Rml::String("Z"));
                g.context->Update();
                const size_t after = ed ? ed->GetValue().size() : 0;
                std::printf("value: %zu -> %zu  (%s)\n", before, after,
                            after > before ? "TYPING WORKS" : "TYPING DOES NOT REACH THE EDITOR");
                // Undo the probe. A diagnostic that leaves the document dirty
                // gets written out by the save-on-exit path — which is how this
                // verb corrupted an example file the first time it was run.
                if (ed) ed->SetValue(g.model_text);
                g.code_dirty = false;
                g.dirty = false;
                std::fflush(stdout);
            }
            else if (verb == "scroll") {
                // Scroll the way a user does — a wheel event over the editor —
                // rather than by setting the offset directly, which RmlUi
                // clamps. This is what proves the highlight layer follows.
                if (Rml::Element* e = by_id("fullcode")) {
                    g.context->Update();
                    g.context->ProcessMouseMove((int)(e->GetAbsoluteLeft() + 40),
                                                (int)(e->GetAbsoluteTop() + 40), 0);
                    Rml::Element* view = by_id("codeview");
                    const int ticks = arg.empty() ? 5 : std::atoi(arg.c_str());
                    for (int i = 0; i < ticks; i++) g.context->ProcessMouseWheel(1.f, 0);
                    g.context->Update();
                    g.context->Update();
                    std::printf("scroll: offset=%d content=%d view_h=%.0f ed_h=%.0f "
                                "ed_scroll=%.0f ed_max=%.0f\n",
                                g.code_scroll, g.code_content_h,
                                view ? view->GetBox().GetSize().y : -1.f,
                                e->GetBox().GetSize().y, e->GetScrollTop(),
                                (float)e->GetScrollHeight() - e->GetBox().GetSize().y);
                    std::fflush(stdout);
                }
            }
            else if (verb == "waitdiag") {
                // Pump the language server the way the frame loop does, so a
                // scripted session can wait for diagnostics to arrive.
                const int limit = arg.empty() ? 300 : std::atoi(arg.c_str());
                for (int i = 0; i < limit; i++) {
                    g.lsp.poll();
                    if (g.lsp.has_update()) break;
                    usleep(20000);
                }
                g.lsp.clear_update();
                g.diagnostics = g.lsp.diagnostics();
                render_diagnostics();
                g.context->Update();
                std::printf("diagnostics: %zu\n", g.diagnostics.size());
                for (const auto& d : g.diagnostics) {
                    std::printf("  line %d: %s\n", d.line + 1, d.message.c_str());
                }
                std::fflush(stdout);
            }
            else if (verb == "hldebug") {
                // Make the editor's own glyphs visible in red. Alignment cannot
                // be judged from a screenshot otherwise: the text you can see
                // is the highlight layer, and the text you actually edit is
                // transparent. Misalignment shows up here as doubled glyphs.
                if (Rml::Element* e = by_id("fullcode")) {
                    e->SetProperty("color", "#ff0000");
                    g.context->Update();
                }
            }
            else if (verb == "focus") {
                if (Rml::Element* e = by_id("fullcode")) e->Focus();
                g.context->Update();
            }
            else if (verb == "focusprop") {
                // Put the caret in an inspector field, as a click there would.
                Rml::Element* grid = by_id("grid");
                Rml::ElementList inputs;
                if (grid) grid->GetElementsByTagName(inputs, "input");
                bool found = false;
                for (Rml::Element* e : inputs) {
                    if (e->GetAttribute<Rml::String>("name", "") == arg) {
                        e->Focus();
                        if (auto* in = dynamic_cast<Rml::ElementFormControlInput*>(e)) {
                            const int n = (int)in->GetValue().size();
                            in->SetSelectionRange(n, n);   // a click past the text
                        }
                        found = true;
                        break;
                    }
                }
                g.context->Update();
                std::printf("focusprop: %s %s\n", arg.c_str(), found ? "focused" : "missing");
                std::fflush(stdout);
            }
            else if (verb == "focused") {
                // Which field holds the caret now, and what it reads — after
                // typing, both must still be the field the user was in.
                g.context->Update();
                Rml::Element* f = g.context->GetFocusElement();
                auto* ctl = dynamic_cast<Rml::ElementFormControl*>(f);
                std::printf("focused: %s value=%s\n",
                            f ? f->GetAttribute<Rml::String>("name", f->GetId()).c_str() : "none",
                            ctl ? ctl->GetValue().c_str() : "");
                std::fflush(stdout);
            }
            else if (verb == "logscroll") {
                g.context->Update();
                size_output_pane();
                g.context->Update();
                follow_log();
                g.context->Update();
                sync_log_scroll();
                if (Rml::Element* e = by_id("log")) {
                    // Whether the newest line is on screen is the whole
                    // question: its row, against the rows the box holds.
                    const int rows = (int)e->GetBox().GetSize().y / theme::LOG_LINE_H;
                    const int first = (int)e->GetScrollTop() / theme::LOG_LINE_H;
                    const bool newest = (int)g.log_lines.size() <= first + rows;
                    std::printf("log: bottom=%.0f win=%d | top=%.0f height=%.0f box=%.0f lines=%zu "
                                "rows=%d first=%d newest=%s\n",
                                e->GetAbsoluteTop() + e->GetBox().GetSize().y, g.win_h,
                                e->GetScrollTop(), (float)e->GetScrollHeight(),
                                e->GetBox().GetSize().y, g.log_lines.size(), rows, first,
                                newest ? "VISIBLE" : "HIDDEN");
                    std::fflush(stdout);
                }
            }
            else if (verb == "logdump") {
                std::printf("logdump-begin\n");
                for (const auto& l : g.log_lines) {
                    std::printf("LOG %s%s\n", l.cls.empty() ? "" : ("[" + l.cls + "] ").c_str(),
                                l.text.c_str());
                }
                std::printf("logdump-end\n");
                std::fflush(stdout);
            }
            else if (verb == "logselect") {
                // logselect:<from>,<to> — press on history row <from> and drag
                // to row <to> (1-based), through the context, then report what
                // the console says is selected. The one proof that the
                // console's text can be selected at all.
                int from = 1, to = 1;
                std::sscanf(arg.c_str(), "%d,%d", &from, &to);
                g.context->Update();
                size_output_pane();
                g.context->Update();
                follow_log();
                sync_log_scroll();
                g.context->Update();
                Rml::Element* ta = by_id("log");
                if (ta) {
                    const auto at = ta->GetAbsoluteOffset(Rml::BoxArea::Border);
                    const int first = (int)ta->GetScrollTop() / theme::LOG_LINE_H;
                    auto row_y = [&](int row) {
                        return (int)at.y + (row - 1 - first) * theme::LOG_LINE_H +
                               theme::LOG_LINE_H / 2;
                    };
                    const int x0 = (int)at.x + 12;
                    g.context->ProcessMouseMove(x0, row_y(from), 0);
                    g.context->ProcessMouseButtonDown(0, 0);
                    g.context->Update();
                    // A drag needs more than one move: RmlUi starts one only
                    // once the pointer has travelled, then delivers `drag`.
                    g.context->ProcessMouseMove(x0 + 8, row_y(from), 0);
                    g.context->Update();
                    g.context->ProcessMouseMove(x0 + 400, row_y(to), 0);
                    g.context->Update();
                    g.context->ProcessMouseButtonUp(0, 0);
                    g.context->Update();
                    // What a keystroke would do now: the gate must stop it.
                    const std::string before = dynamic_cast<Rml::ElementFormControl*>(ta)->GetValue();
                    g.context->ProcessTextInput(Rml::String("Z"));
                    g.context->ProcessKeyDown(Rml::Input::KI_BACK, 0);
                    g.context->Update();
                    const std::string after = dynamic_cast<Rml::ElementFormControl*>(ta)->GetValue();
                    int s = 0, e = 0;
                    Rml::String selected;
                    dynamic_cast<Rml::ElementFormControlTextArea*>(ta)->GetSelection(&s, &e, &selected);
                    for (char& ch : selected) {
                        if (ch == '\n') ch = '|';
                    }
                    std::printf("logselect: %d..%d chars=%zu edited=%s focus=%s | %s\n", s, e,
                                selected.size(), before == after ? "no" : "YES",
                                g.doc->GetFocusLeafNode() ? g.doc->GetFocusLeafNode()->GetId().c_str()
                                                          : "(none)",
                                selected.c_str());
                    std::fflush(stdout);
                }
            }
        }
        if (semi == std::string::npos) break;
        i = semi + 1;
    }
}

} // namespace

/// Create a project from a template next to the working directory, and return
/// the file to open.
///
/// There is no file dialog yet, so the location is derived rather than asked
/// for — and the name is made unique, because silently writing into an existing
/// project would be the worst possible first impression.
std::string create_project(const std::string& template_id,
                           const std::vector<kiln::welcome::TemplateInfo>& templates) {
    std::string dir = template_id;
    for (int n = 2; ::access(dir.c_str(), F_OK) == 0 && n < 100; n++) {
        dir = template_id + "-" + std::to_string(n);
    }

    std::string text;
    if (kiln::sys::capture_output(g.kiln_bin + " new " + template_id + " " + dir, true, text) == -1 &&
        text.empty())
        return "";
    std::string open_path, line;
    std::istringstream lines(text);
    while (std::getline(lines, line)) {
        while (!line.empty() && (line.back() == '\n' || line.back() == '\r')) line.pop_back();
        // `new` reports which file to open, so we do not have to guess it from
        // the template's layout.
        if (line.rfind("open: ", 0) == 0) open_path = line.substr(6);
        std::fprintf(stderr, "designer: %s\n", line.c_str());
    }
    (void)templates;
    return open_path;
}

/// The splash, sized to the window as it is now. The markup bakes its size
/// in, and the window manager's maximise arrives in the first events after
/// the window opens — a splash built for the size we asked for is clipped by
/// the size we got.
struct Splash {
    Rml::ElementDocument* doc = nullptr;
    Rml::Vector2i built_for;
    Uint32 shown_at = 0;
};

void paint_splash(Splash& sp, const std::string& family) {
    const auto dim = g.context->GetDimensions();
    if (sp.doc && dim == sp.built_for) return;
    if (sp.doc) {
        sp.doc->Close();
        g.context->Update();
    }
    sp.built_for = dim;
    sp.doc = g.context->LoadDocumentFromMemory(
        kiln::welcome::splash_markup(family, dim.x, dim.y, asset_path("kiln-wordmark.png")));
    if (!sp.doc) return;
    sp.doc->Show();
    // Two frames: one to lay out, one to present. Without this the splash is
    // constructed and never actually drawn.
    for (int i = 0; i < 2; i++) {
        g.context->Update();
        Backend::BeginFrame();
        g.context->Render();
        Backend::PresentFrame();
    }
}

/// Put the splash on screen and paint it, so it is visible *during* the work
/// that follows rather than after it.
Splash show_splash(const std::string& family) {
    Splash sp;
    sp.shown_at = SDL_GetTicks();
    paint_splash(sp, family);
    // The backend applies the real window size in these events. Read it now
    // and rebuild, or the splash sits at 1440x900 in a maximised window.
    for (int i = 0; i < 3; i++) Backend::ProcessEvents(g.context, nullptr, false);
    paint_splash(sp, family);
    dump_to(std::getenv("KILN_DESIGNER_SPLASH_DUMP"));
    return sp;
}

/// Take the splash down — but not before it has been seen. The work it
/// covers finishes in a fraction of a second on a fast machine, and a splash
/// torn down that quickly is a flicker, not a splash. The work itself is not
/// delayed: it has already run by the time this is called, and the hold only
/// eats what remains of the minimum. Returns false when the user closed the
/// window during the hold.
///
/// No hold in a scripted session: nobody is watching, and the Studio tests
/// run dozens of sessions.
bool close_splash(Splash& sp, const std::string& family, Uint32 min_ms) {
    if (!sp.doc) return true;
    const bool scripted = std::getenv("KILN_DESIGNER_SCRIPT") != nullptr;
    while (!scripted && SDL_GetTicks() - sp.shown_at < min_ms) {
        if (!Backend::ProcessEvents(g.context, nullptr, false)) return false;
        paint_splash(sp, family);
        g.context->Update();
        Backend::BeginFrame();
        g.context->Render();
        Backend::PresentFrame();
        SDL_Delay(16);
    }
    if (std::getenv("KILN_DESIGNER_DEBUG")) {
        std::fprintf(stderr, "splash: shown for %u ms (minimum %u)\n",
                     SDL_GetTicks() - sp.shown_at, min_ms);
    }
    sp.doc->Close();
    sp.doc = nullptr;
    g.context->Update();
    return true;
}

/// The welcome screen. Returns the project file to open, or "" if the user
/// closed the window.
std::string run_welcome(const std::string& family) {
    auto dim = g.context->GetDimensions();
    const auto templates = kiln::welcome::load_templates(g.kiln_bin);
    const auto recent =
        kiln::welcome::load_recent((size_t)kiln::settings::number("startup.recent_limit"));

    Rml::ElementDocument* doc = g.context->LoadDocumentFromMemory(
        kiln::welcome::welcome_markup(family, dim.x, dim.y, templates, recent,
                                         asset_path("kiln-wordmark.png"), g.kiln_bin));
    if (!doc) return "";
    doc->Show();

    // Headless hooks, mirroring the IDE's scripted sessions: render the screen,
    // optionally dump it, and optionally choose without a click so the whole
    // create-and-open path can be tested.
    //
    // dump_to renders its own frames and reads the buffer without presenting;
    // presenting first would swap away the buffer it is about to read.
    // Pump the backend first: it is what applies the real window size to the
    // GL viewport. Rendering before it has run leaves part of the frame never
    // written, which reads back as black.
    if (const char* sz = std::getenv("KILN_DESIGNER_WELCOME_SIZE")) {
        int nw = 0, nh = 0;
        if (std::sscanf(sz, "%dx%d", &nw, &nh) == 2 && nw > 0 && nh > 0) {
            if (SDL_Window* win = SDL_GL_GetCurrentWindow()) SDL_SetWindowSize(win, nw, nh);
            for (int i = 0; i < 30; i++) Backend::ProcessEvents(g.context, nullptr, false);
            g.context->SetDimensions(Rml::Vector2i(nw, nh));
        }
    }
    for (int i = 0; i < 3; i++) {
        Backend::ProcessEvents(g.context, nullptr, false);
        g.context->Update();
        Backend::BeginFrame();
        g.context->Render();
        Backend::PresentFrame();
    }
    // The window manager's maximize arrives in those first events, after the
    // document was laid out for the size we asked for. The markup bakes its
    // size in, so rebuild for the size we actually have — otherwise everything
    // past 1440x900 stays black until something else forces a relayout.
    if (const auto now = g.context->GetDimensions(); now != dim) {
        dim = now;
        doc->Close();
        g.context->Update();
        doc = g.context->LoadDocumentFromMemory(kiln::welcome::welcome_markup(
            family, dim.x, dim.y, templates, recent, asset_path("kiln-wordmark.png"),
            g.kiln_bin));
        if (!doc) return "";
        doc->Show();
        g.context->Update();
        Backend::BeginFrame();
        g.context->Render();
        Backend::PresentFrame();
    }
    dump_to(std::getenv("KILN_DESIGNER_WELCOME_DUMP"));
    if (std::getenv("KILN_DESIGNER_DEBUG")) {
        const auto bb = doc->GetBox().GetSize();
        std::fprintf(stderr, "welcome: body %.0fx%.0f at %.0f,%.0f\n", bb.x, bb.y,
                     doc->GetAbsoluteLeft(), doc->GetAbsoluteTop());
        std::fprintf(stderr, "welcome: context %dx%d  g.win %dx%d\n",
                     g.context->GetDimensions().x, g.context->GetDimensions().y,
                     g.win_w, g.win_h);
        for (const char* id : {"bg", "head", "mark", "cols"}) {
            if (Rml::Element* e = doc->GetElementById(id)) {
                const auto b = e->GetBox().GetSize();
                std::fprintf(stderr, "welcome: #%s %.0fx%.0f at %.0f,%.0f\n", id, b.x, b.y,
                             e->GetAbsoluteLeft(), e->GetAbsoluteTop());
            } else {
                std::fprintf(stderr, "welcome: #%s MISSING\n", id);
            }
        }
    }
    std::string chosen;
    struct Pick : Rml::EventListener {
        std::string* out;
        const std::vector<kiln::welcome::TemplateInfo>* templates;
        void ProcessEvent(Rml::Event& ev) override {
            for (Rml::Element* e = ev.GetTargetElement(); e; e = e->GetParentNode()) {
                if (e->HasAttribute("oe-win")) {
                    window_control(e->GetAttribute<Rml::String>("oe-win", ""));
                    return;
                }
                if (e->HasAttribute("oe-open")) {
                    *out = e->GetAttribute<Rml::String>("oe-open", "");
                    return;
                }
                if (e->HasAttribute("oe-new")) {
                    *out = "new:" + e->GetAttribute<Rml::String>("oe-new", "");
                    return;
                }
                if (e->HasAttribute("oe-browse")) {
                    *out = "browse:" + e->GetAttribute<Rml::String>("oe-browse", "");
                    return;
                }
                if (e->HasAttribute("oe-browse-dir")) {
                    *out = "browsedir:" + e->GetAttribute<Rml::String>("oe-browse-dir", "");
                    return;
                }
                if (e->HasAttribute("oe-browse-cancel")) {
                    *out = "cancel";
                    return;
                }
            }
        }
    } pick;
    pick.out = &chosen;
    pick.templates = &templates;
    doc->AddEventListener("click", &pick);

    // The path browser is a second document that replaces the welcome screen
    // and hands back to it. Documents are swapped here, between frames, never
    // inside the listener that is still walking the clicked element's parents.
    // A directory entry carries only its path, so the mode is remembered from
    // the tile that opened the browser.
    std::string browse_mode;
    std::string browse_dir;
    auto swap_document = [&](const std::string& markup) {
        doc->Close();
        g.context->Update();
        doc = g.context->LoadDocumentFromMemory(markup);
        if (!doc) return false;
        doc->Show();
        doc->AddEventListener("click", &pick);
        chosen.clear();
        return true;
    };
    auto start_dir = []() {
        char buf[4096];
        if (::getcwd(buf, sizeof buf)) return kiln::sys::slashes(buf);
        const std::string home = kiln::sys::home_dir();
        return home.empty() ? kiln::sys::root_dir() : home;
    };

    // The welcome screen, as a picture. It waits for a click, so it is the one
    // document a scripted session could never reach — which is why its
    // screenshot went stale at the rename while every other one could be
    // regenerated. Same shape as the browser's dump below.
    if (const char* shot = std::getenv("KILN_DESIGNER_WELCOME_DUMP")) {
        for (int i = 0; i < 3; i++) {
            Backend::ProcessEvents(g.context, nullptr, false);
            g.context->Update();
            Backend::BeginFrame();
            g.context->Render();
            Backend::PresentFrame();
        }
        dump_to(shot);
        std::printf("welcomedump: %s\n", shot);
        std::fflush(stdout);
        // Closed before returning, like every other exit from here. `pick` is
        // a local of this function: leaving the document alive leaves it
        // holding a listener whose object is gone the moment we return, and
        // `Rml::Shutdown()` walks that list — the crash was a segfault in
        // DetachAllEvents at the very end of a run that had otherwise done
        // everything right, which is the kind that gets blamed on the dump.
        doc->Close();
        g.context->Update();
        return "";
    }

    if (const char* pick = std::getenv("KILN_DESIGNER_WELCOME_PICK")) {
        const std::string want(pick);
        // `browse:<mode>` swaps in the path browser and dumps it, so the same
        // document swap a click performs can be looked at without a click.
        if (want.rfind("browse:", 0) == 0) {
            if (swap_document(kiln::welcome::browse_markup(
                    family, dim.x, dim.y, start_dir(), want.substr(7), g.kiln_bin))) {
                for (int i = 0; i < 3; i++) {
                    Backend::ProcessEvents(g.context, nullptr, false);
                    g.context->Update();
                    Backend::BeginFrame();
                    g.context->Render();
                    Backend::PresentFrame();
                }
                dump_to(std::getenv("KILN_DESIGNER_BROWSE_DUMP"));
                // Click a row, the way a hand does: find it by the attribute
                // that carries its meaning, press in the middle of it, and
                // report what the listener made of it. The browser's rows are
                // the one part of the welcome screen no test had ever pressed.
                if (const char* row = std::getenv("KILN_DESIGNER_BROWSE_CLICK")) {
                    const std::string wanted(row);
                    Rml::Element* hit = nullptr;
                    std::function<void(Rml::Element*)> walk = [&](Rml::Element* e) {
                        if (hit || !e) return;
                        for (const char* attr : {"oe-open", "oe-browse-dir"}) {
                            if (e->HasAttribute(attr) &&
                                e->GetAttribute<Rml::String>(attr, "").find(wanted) !=
                                    Rml::String::npos) {
                                hit = e;
                                return;
                            }
                        }
                        for (int i = 0; i < e->GetNumChildren(); i++) walk(e->GetChild(i));
                    };
                    walk(doc);
                    if (!hit) {
                        std::printf("browseclick: no row matching %s\n", wanted.c_str());
                    } else {
                        const auto at = hit->GetAbsoluteOffset(Rml::BoxArea::Border);
                        const auto sz = hit->GetBox().GetSize(Rml::BoxArea::Border);
                        const int mx = (int)(at.x + sz.x / 2), my = (int)(at.y + sz.y / 2);
                        g.context->ProcessMouseMove(mx, my, 0);
                        g.context->Update();
                        const Rml::Element* hov = g.context->GetHoverElement();
                        g.context->ProcessMouseButtonDown(0, 0);
                        g.context->ProcessMouseButtonUp(0, 0);
                        g.context->Update();
                        std::printf("browseclick: row '%s' box %.0fx%.0f at %.0f,%.0f "
                                    "press %d,%d hover=<%s> chose '%s'\n",
                                    hit->GetAttribute<Rml::String>("oe-open", "-").c_str(), sz.x,
                                    sz.y, at.x, at.y, mx, my,
                                    hov ? hov->GetTagName().c_str() : "none", chosen.c_str());
                        // The same click sent straight to the row, which tells
                        // the two failures apart: a listener that never sees a
                        // click, or a click that never reaches the row.
                        chosen.clear();
                        hit->DispatchEvent(Rml::EventId::Click, Rml::Dictionary());
                        g.context->Update();
                        std::printf("browseclick: dispatched direct, chose '%s'\n", chosen.c_str());
                    }
                    std::fflush(stdout);
                }
                doc->Close();
            }
            g.context->Update();
            return "";
        }
        doc->Close();
        g.context->Update();
        if (want.rfind("open:", 0) == 0) return want.substr(5);
        return create_project(want, templates);
    }

    while (true) {
        if (!Backend::ProcessEvents(g.context, nullptr, true)) {   // window closed
            chosen.clear();
            break;
        }
        // The welcome markup bakes its width and height in, and a window
        // manager that maximises us does so after the document was laid out
        // for the size we asked for. Rebuild for the size we actually have,
        // or everything past 1440x900 stays black.
        if (const auto now = g.context->GetDimensions(); now != dim) {
            dim = now;
            const bool ok = browse_mode.empty()
                ? swap_document(kiln::welcome::welcome_markup(
                      family, dim.x, dim.y, templates, recent,
                      asset_path("kiln-wordmark.png"), g.kiln_bin))
                : swap_document(kiln::welcome::browse_markup(
                      family, dim.x, dim.y, browse_dir, browse_mode, g.kiln_bin));
            if (!ok) break;
        }
        g.context->Update();
        Backend::BeginFrame();
        g.context->Render();
        Backend::PresentFrame();
        if (chosen.empty()) continue;

        std::string dir;
        // Ask the platform first. Its dialog is the one the user already knows
        // — it types a path, remembers where they were and reaches the places
        // a list of one directory cannot.
        //
        // Not in a scripted session: there is nobody to answer it, and a modal
        // dialog in a test is a hang rather than a failure. Not when there is
        // no dialog to show either — Studio's own browser is the answer on a
        // machine with neither portal nor zenity, and it stays the fallback
        // rather than becoming dead code.
        if (chosen.rfind("browse:", 0) == 0 && !std::getenv("KILN_DESIGNER_SCRIPT") &&
            !std::getenv("KILN_DESIGNER_WELCOME_PICK") &&
            !std::getenv("KILN_NO_NATIVE_DIALOG") && kiln::sys::has_native_file_dialog()) {
            const std::string mode = chosen.substr(7);
            const bool project = mode == "project";
            const std::string picked = kiln::sys::pick_open_file(
                project ? "Open Project" : "Open File", start_dir(),
                project ? "Kiln project" : "Kiln source",
                project ? "*.kproj *.kiln" : "*.kiln");
            if (!picked.empty()) {
                // A project is named by its `project.kproj`; the CLI says which
                // `.kiln` that means, exactly as the in-app browser asks it.
                const std::string open =
                    project ? kiln::welcome::resolve_open(g.kiln_bin, picked) : picked;
                if (!open.empty()) {
                    chosen = open;
                    break;
                }
                log("that project has no module to open: " + picked, "err");
            }
            // Cancelled, or nothing usable: fall through to Studio's browser
            // rather than dropping the user back on the welcome screen having
            // apparently done nothing.
        }
        if (chosen.rfind("browse:", 0) == 0) {
            browse_mode = chosen.substr(7);
            dir = start_dir();
        } else if (chosen.rfind("browsedir:", 0) == 0) {
            dir = chosen.substr(10);
        } else if (chosen == "cancel") {
            browse_mode.clear();
            if (!swap_document(kiln::welcome::welcome_markup(
                    family, dim.x, dim.y, templates, recent,
                    asset_path("kiln-wordmark.png"), g.kiln_bin))) break;
            continue;
        } else {
            break;
        }
        browse_dir = dir;
        if (!swap_document(kiln::welcome::browse_markup(family, dim.x, dim.y, dir,
                                                           browse_mode, g.kiln_bin))) break;
    }
    if (doc) doc->Close();
    g.context->Update();

    if (chosen.rfind("new:", 0) == 0) {
        chosen = create_project(chosen.substr(4), templates);
    }
    return chosen;
}

/// A writable per-user cache path for `name`.
std::string cache_file(const char* name) {
    const std::string dir = kiln::sys::cache_dir();
    kiln::sys::make_dirs(dir);
    return dir + "/" + name;
}

/// The `kiln` binary that ships beside us.
///
/// A bundle is unpacked wherever the user likes, so the compiler cannot be
/// found by a fixed relative path. Ours is next to this executable; the repo's
/// debug build is the fallback for development.
std::string sibling_kiln() {
    const std::string dir = kiln::sys::exe_dir();
    if (!dir.empty()) {
        const std::string cand = dir + "/" + kiln::sys::kiln_exe_name();
        if (kiln::sys::executable(cand)) return cand;
    }
    return "./target/debug/" + std::string(kiln::sys::kiln_exe_name());
}

#ifdef _WIN32
/// The faces to try on Windows, where the ui library's list — Linux
/// distribution paths — finds nothing, and RmlUi text with no loaded face
/// renders invisibly. The bundle ships DejaVu beside Studio (assets/fonts,
/// see tools/package-windows.sh), so a Windows machine with no font Studio
/// knows still gets the one Linux uses; the system's Segoe UI and Consolas
/// come next, and Tahoma — the one face wine always has — last.
const kiln::ui::FontCandidate* windows_font_candidates(int* count) {
    static std::vector<std::string> paths;
    static std::vector<kiln::ui::FontCandidate> fonts;
    if (fonts.empty()) {
        const std::string exe = kiln::sys::exe_dir();
        const char* win = std::getenv("WINDIR");
        const std::string sys = kiln::sys::slashes(win && *win ? win : "C:/Windows") + "/Fonts/";
        std::vector<std::string> roots;
        if (!exe.empty()) {
            roots.push_back(exe + "/../assets/fonts/");     // bundle: bin/ -> ../assets
            roots.push_back(exe + "/assets/fonts/");
            roots.push_back(exe + "/../../assets/fonts/");  // repo: designer/ -> ../assets
        }
        roots.push_back("assets/fonts/");
        struct Face { const char* regular; const char* family; const char* bold; const char* italic;
                      const char* bold_italic; bool mono; };
        const Face dejavu[] = {
            {"DejaVuSans.ttf", "DejaVu Sans", "DejaVuSans-Bold.ttf", "DejaVuSans-Oblique.ttf",
             "DejaVuSans-BoldOblique.ttf", false},
            {"DejaVuSansMono.ttf", "DejaVu Sans Mono", "DejaVuSansMono-Bold.ttf",
             "DejaVuSansMono-Oblique.ttf", "DejaVuSansMono-BoldOblique.ttf", true},
        };
        const Face system[] = {
            {"segoeui.ttf", "Segoe UI", "segoeuib.ttf", "segoeuii.ttf", "segoeuiz.ttf", false},
            {"consola.ttf", "Consolas", "consolab.ttf", "consolai.ttf", "consolaz.ttf", true},
            {"tahoma.ttf", "Tahoma", "tahomabd.ttf", nullptr, nullptr, false},
            {"cour.ttf", "Courier New", "courbd.ttf", "couri.ttf", "courbi.ttf", true},
        };
        // The vector is sized once, up front: the FontCandidate pointers
        // into `paths` must not move after they are taken.
        paths.reserve((roots.size() * 2 + 4) * 4);
        auto add = [&](const std::string& root, const Face& f) {
            auto keep = [&](const char* name) -> const char* {
                if (!name) return nullptr;
                paths.push_back(root + name);
                return paths.back().c_str();
            };
            const char* regular = keep(f.regular);
            fonts.push_back({regular, f.family, keep(f.bold), keep(f.italic), keep(f.bold_italic), f.mono});
        };
        for (const auto& r : roots) {
            for (const auto& f : dejavu) add(r, f);
        }
        for (const auto& f : system) add(sys, f);
    }
    *count = (int)fonts.size();
    return fonts.data();
}
#endif

/// Record the window geometry, if the user asked us to.
///
/// Guarded against scripted sessions for the same reason `remember_recent` is:
/// a headless run resizes the window to whatever a test wanted, and that must
/// not become the size a person's IDE opens at tomorrow.
void remember_window_size() {
    if (std::getenv("KILN_DESIGNER_SCRIPT")) return;
    if (!kiln::settings::boolean("appearance.remember_window")) return;
    kiln::settings::set("window.width", std::to_string(g.win_w));
    kiln::settings::set("window.height", std::to_string(g.win_h));
    kiln::settings::save();
}

/// Apply the on-exit policy to unsaved work.
///
/// `discard` is offered because Studio saves on exit unconditionally, and the
/// project's own rule — never open a tracked example in Studio, because the
/// save lands in your next commit — exists only because of that. It says what
/// it did either way: an afternoon's work vanishing silently would be a far
/// worse failure than the one this setting fixes.
void save_or_discard_on_exit() {
    if (kiln::settings::text("startup.on_exit") == "discard") {
        std::printf("designer: unsaved changes discarded (Settings ▸ Files and startup)\n");
        return;
    }
    std::printf("designer: unsaved changes — saving before exit\n");
    save();
}

int main(int argc, char** argv) {
    // Settings before anything reads one. The palette in particular has to be
    // in force before the first stylesheet is built: every colour in the chrome,
    // the splash and the welcome screen is copied out of `theme::` as a string,
    // so a swap after that point paints nothing already drawn.
    kiln::settings::load();
    theme::set_palette(kiln::settings::text("appearance.theme") == "dark");
    theme::set_code_font_size(kiln::settings::number("editor.font_size"));

    g.kiln_bin = sibling_kiln();
    // A toolchain named in the settings beats the sibling, and the command
    // line beats both — the argument is the more deliberate act of the two.
    if (const std::string chosen = kiln::settings::text("toolchain.kiln"); !chosen.empty()) {
        g.kiln_bin = chosen;
    }
    // Either argument may be omitted: with no project we show the welcome
    // screen, and the compiler path is optional. They are told apart by
    // extension rather than by position, so `kiln-designer <compiler>` works
    // without inventing a flag.
    std::string path;
    for (int i = 1; i < argc; i++) {
        const std::string arg = argv[i];
        if (arg == "-h" || arg == "--help") {
            std::fprintf(stderr,
                         "usage: kiln-designer [project.kiln|project.kproj|dir] [path/to/kiln]\n\n"
                         "With no project, Studio opens its welcome screen.\n\n"
                         "Environment:\n"
                         "  KILN_DESIGNER_SCRIPT   run a scripted session headlessly\n"
                         "  KILN_DESIGNER_DEBUG    report chrome/toolbox diagnostics\n");
            return 2;
        }
        const bool is_project = kiln::sys::has_ext(arg, ".kiln") ||
                                kiln::welcome::is_project_path(arg);
        if (is_project) {
            path = arg;
        } else {
            g.kiln_bin = arg;
        }
    }

    /* A headless run must not open a window: a test or an agent that
     * renders a frame to a file steals focus from whoever is working on
     * the machine otherwise. SDL's offscreen driver renders through EGL
     * with no window at all, and a caller who set SDL_VIDEODRIVER
     * knows better than this default, as does KILN_UI_WINDOW=1 — the
     * one test that reads the manager's own flags back needs a real window. */
    // Not on Windows: SDL's offscreen driver draws through EGL, which the
    // Windows build of SDL has none of (libs/ui/ui_rmlui.cpp says the same).
#ifndef _WIN32
    if (!std::getenv("SDL_VIDEODRIVER") && !std::getenv("KILN_UI_WINDOW") &&
        (std::getenv("KILN_DESIGNER_SCRIPT") || std::getenv("KILN_DESIGNER_DUMP") ||
         std::getenv("KILN_DESIGNER_WELCOME_DUMP")))
        setenv("SDL_VIDEODRIVER", "offscreen", 1);
#endif
    // The size we were left at, when asked for. `Designer g` is constructed
    // during static initialisation — before any settings file has been read —
    // so g.win_w is seeded here rather than at its declaration.
    if (kiln::settings::boolean("appearance.remember_window")) {
        const int w = kiln::settings::number("window.width");
        const int h = kiln::settings::number("window.height");
        if (w > 0 && h > 0) { g.win_w = w; g.win_h = h; }
    }
    if (!Backend::Initialize("Kiln Studio", g.win_w, g.win_h, true)) return 1;

    // The window icon: what a task switcher and a dock show. SDL owns the
    // surface only until it copies it, so freeing straight after is correct.
    if (SDL_Window* win = SDL_GL_GetCurrentWindow()) {
        const std::string icon = asset_path("kiln-icon.png");
        if (!icon.empty()) {
            // asset_path returns the URL form (doubled leading slash) for
            // RmlUi; SDL wants the plain filesystem path.
            const std::string file = kiln::sys::path_of_url(icon);
            if (SDL_Surface* s = IMG_Load(file.c_str())) {
                SDL_SetWindowIcon(win, s);
                SDL_FreeSurface(s);
            }
        }
        // Studio draws its own title bar; the window manager's would sit on
        // top of it, and two title bars is one too many. Without a frame the
        // hit test is what makes the window movable and resizable at all,
        // so a platform that refuses one gets its frame back.
        SDL_SetWindowBordered(win, SDL_FALSE);
        if (SDL_SetWindowHitTest(win, window_hit_test, nullptr) != 0) {
            std::fprintf(stderr, "designer: no window hit test (%s); keeping the frame\n",
                         SDL_GetError());
            SDL_SetWindowBordered(win, SDL_TRUE);
        }
        if (std::getenv("KILN_DESIGNER_DEBUG")) {
            std::fprintf(stderr, "designer: video driver %s\n", SDL_GetCurrentVideoDriver());
        }
    }
    // Ours rather than the backend's, for the directional resize cursors.
    // Static so it outlives Rml::Shutdown, which still holds the pointer.
    static StudioSystem* system = new StudioSystem(SDL_GL_GetCurrentWindow());
    Rml::SetSystemInterface(system);
    Rml::SetRenderInterface(Backend::GetRenderInterface());
    Rml::Initialise();

    int font_count = 0;
#ifdef _WIN32
    const auto* fonts = windows_font_candidates(&font_count);
#else
    const auto* fonts = kiln::ui::font_candidates(&font_count);
#endif
    std::string family = "sans-serif";
    for (int i = 0; i < font_count; i++) {
        if (!Rml::LoadFontFace(fonts[i].path)) continue;
        family = fonts[i].family;
        // Load the companion styles too. RmlUi does not synthesise bold or
        // italic: text asking for a face that was never loaded renders with no
        // font at all, i.e. invisibly. Missing companions are not fatal —
        // that text just falls back to regular.
        for (const char* extra : {fonts[i].bold, fonts[i].italic, fonts[i].bold_italic}) {
            if (extra) Rml::LoadFontFace(extra);
        }
        break;
    }

    // Into a cache directory, not the working directory: Studio is launched
    // from wherever the user's project lives, and dropping a .tga into it is
    // littering someone else's folder.
    // The editor needs a MONOSPACE face: the highlight overlay sits behind the
    // textarea and only lines up if both use the same fixed-width metrics.
    // Companions come along too — the comment style is italic, and text in a
    // face that was never loaded renders invisibly.
    std::string mono = family;
    for (int i = 0; i < font_count; i++) {
        if (!fonts[i].is_mono || !Rml::LoadFontFace(fonts[i].path)) continue;
        mono = fonts[i].family;
        for (const char* extra : {fonts[i].bold, fonts[i].italic, fonts[i].bold_italic}) {
            if (extra) Rml::LoadFontFace(extra);
        }
        break;
    }

    // RmlUi resolves a decorator path as a URL and drops the leading slash of
    // an absolute one, so the tile has to be handed over in a form its URL
    // parser leaves alone. Doubling the slash survives that normalisation.
    g.family = family;
    const std::string tile_path = cache_file("kiln_dotgrid.tga");
    const std::string dot_tile =
        write_dot_tile(tile_path, 10, theme::BORDER).empty() ? "" : "/" + tile_path;
    g.dot_tile = dot_tile;
    g.context = Rml::CreateContext("studio", Rml::Vector2i(g.win_w, g.win_h));
    // Off by default in RmlUi: without this every `cursor:` rule in the
    // stylesheet is inert, and a resize handle looks like anything else.
    g.context->EnableMouseCursor(true);

    // Splash first, and painted before the slow part starts. Loading the
    // component registry shells out to `kiln commands` once per kit; done
    // before the window exists it is a second of nothing at all.
    Splash splash = show_splash(family);

    // Ask the toolchain what exists before drawing a toolbox that claims to
    // know — and if the toolchain cannot be run at all the catalogue comes
    // back empty, which shows an empty toolbox rather than a wrong one.
    g.catalog = build_catalog(g.kiln_bin);

    // With no file to open, ask what to build. The welcome screen has no
    // project yet, so the IDE chrome cannot meaningfully exist behind it.
    // The second splash is a transition between two screens, not a launch;
    // it gets no minimum.
    Uint32 splash_min = (Uint32)kiln::settings::number("appearance.splash_ms");
    auto quit = [] {
        Rml::Shutdown();
        Backend::Shutdown();
        return 0;
    };
    // Reopening the last project skips the welcome screen entirely — asked for
    // once, then never asked again. Guarded by an existence check, because a
    // project that has been moved or deleted must land on the welcome screen
    // rather than on an error.
    if (path.empty() && kiln::settings::boolean("startup.reopen_last") &&
        !std::getenv("KILN_DESIGNER_SCRIPT")) {
        const auto recent =
            kiln::welcome::load_recent((size_t)kiln::settings::number("startup.recent_limit"));
        if (!recent.empty()) path = kiln::welcome::resolve_open(g.kiln_bin, recent.front());
    }
    if (path.empty()) {
        if (!close_splash(splash, family, splash_min)) return quit();
        path = run_welcome(family);
        if (path.empty()) return quit();   // the user closed the window
        splash = show_splash(family);
        splash_min = 0;
    }

    std::string err;
    if (!load_model(g.kiln_bin, path, g.model, err)) {
        std::fprintf(stderr, "designer: cannot load %s\n%s\n", path.c_str(), err.c_str());
        return 1;
    }
    // A scripted session is a test, not a person: it must not appear on the
    // welcome screen the person sees next.
    if (!std::getenv("KILN_DESIGNER_SCRIPT") && !std::getenv("KILN_DESIGNER_DUMP"))
        kiln::welcome::remember_recent(
            path, (size_t)kiln::settings::number("startup.recent_limit"));

    // The language server, started on the project's directory so it finds the
    // runtime and the component library the same way the compiler does.
    {
        std::string abs = kiln::sys::real_path(path);
        if (abs.empty()) abs = path;
        const size_t slash = abs.find_last_of('/');
        g.lsp.start(g.kiln_bin, slash == std::string::npos ? "." : abs.substr(0, slash));
        std::string text;
        if (FILE* f = std::fopen(abs.c_str(), "rb")) {
            char buf[4096];
            size_t n;
            while ((n = std::fread(buf, 1, sizeof buf, f)) > 0) text.append(buf, n);
            std::fclose(f);
        }
        g.lsp.did_open(abs, text);
    }
    if (!close_splash(splash, family, splash_min)) return quit();

    g.mono = mono;
    const std::string chrome = build_chrome(family, mono, dot_tile);
    if (std::getenv("KILN_DESIGNER_DEBUG")) {
        std::fprintf(stderr, "designer: chrome %zu bytes\n", chrome.size());
    }
    g.doc = g.context->LoadDocumentFromMemory(chrome);
    if (!g.doc) { std::fprintf(stderr, "designer: chrome failed to load\n"); return 1; }
    g.doc->Show();

    for (const char* e : {"click", "change", "mousedown", "mousemove", "mouseup", "mousescroll",
                          "keydown"}) {
        g.doc->AddEventListener(e, &g_listener);
    }
    for (const char* e : {"keydown", "textinput"}) g.doc->AddEventListener(e, &g_key_gate, true);

    relayout();
    refresh_all();
    log("Kiln Studio ready.", "muted");
    log("> " + g.model.path, "muted");
    set_status("Ready");

    if (std::getenv("KILN_DESIGNER_DEBUG")) {
        for (const char* id : {"bottom", "codepane", "code", "codeview"}) {
            if (Rml::Element* e = by_id(id)) {
                const auto b = e->GetBox().GetSize();
                const auto o = e->GetAbsoluteOffset();
                std::fprintf(stderr, "designer: #%s box=%.0fx%.0f at %.0f,%.0f\n", id, b.x, b.y,
                             o.x, o.y);
            }
        }
        if (Rml::Element* tb = by_id("toollist")) {
            std::fprintf(stderr, "designer: toolbox has %d children\n", tb->GetNumChildren());
            for (int i = 0; i < tb->GetNumChildren(); i++) {
                Rml::Element* c = tb->GetChild(i);
                std::fprintf(stderr, "designer:   <%s> oe-add=%s\n", c->GetTagName().c_str(),
                             c->GetAttribute<Rml::String>("oe-add", "(none)").c_str());
            }
        }
    }

    if (const char* script = std::getenv("KILN_DESIGNER_SCRIPT")) {
        // Adopt the window the compositor actually gave us before laying out.
        // The interactive loop does this every frame; without it a scripted
        // session lays out for the size we asked for and renders into the size
        // we got, so dumps show a layout the user would never see.
        Backend::ProcessEvents(g.context, nullptr, false);
        const auto dim = g.context->GetDimensions();
        if (dim.x != g.win_w || dim.y != g.win_h) {
            g.win_w = dim.x;
            g.win_h = dim.y;
            relayout();
            rebuild_canvas();
        }
        g.context->Update();
        run_script(script);
        if (g.dirty) save_or_discard_on_exit();
        std::printf("designer: script complete\n");
        if (std::getenv("KILN_DESIGNER_DUMP")) dump_frame();
        Rml::Shutdown();
        Backend::Shutdown();
        return 0;
    }

    if (std::getenv("KILN_DESIGNER_DUMP")) {
        dump_frame();
        Rml::Shutdown();
        Backend::Shutdown();
        return 0;
    }

    // Power-save blocks until Studio itself gets an input event. That is right
    // when idle, but while an app is running the user is clicking in *its*
    // window, not ours — and a blocked loop never drains the app's output pipe,
    // so its prints would only appear when you happened to jiggle Studio.
    // Nor while an answer from the language server is awaited, or the pointer
    // is resting on the editor: the hover fires on a timer, and a loop that
    // sleeps until the next input event would show the tip on the next
    // keystroke instead.
    auto idle = [] {
        const bool awaiting = g.hover_request || g.def_request || g.refs_request ||
                              g.complete_request || g.symbol_request || g.symbols_stale ||
                              (g.view == "code" && g.hover_x >= 0 && !g.hover_asked);
        return g.running_app <= 0 && g.build_pid <= 0 && !awaiting;
    };
    while (Backend::ProcessEvents(g.context, &on_key_down, idle())) {
        poll_build();
        poll_app();
        poll_debug();
        if (g.view == "code") sync_highlight_scroll();
        g.lsp.poll();
        if (g.lsp.has_update()) {
            g.lsp.clear_update();
            g.diagnostics = g.lsp.diagnostics();
            render_diagnostics();
        }
        hover_tick();
        poll_answers();
        update_tabs();
        flush_inspector();
        // Follow the OS window. The backend resizes the context; the layout has
        // to follow or everything past the old size is left unpainted.
        const auto dim = g.context->GetDimensions();
        if (dim.x != g.win_w || dim.y != g.win_h) {
            g.win_w = dim.x;
            g.win_h = dim.y;
            relayout();
            rebuild_canvas();
        }
        g.context->Update();
        size_output_pane();
        follow_log();
        follow_code_scroll();
        sync_log_scroll();
        Backend::BeginFrame();
        g.context->Render();
        Backend::PresentFrame();
    }
    stop_all();
    g.lsp.stop();
    remember_window_size();
    if (g.dirty) {
        save_or_discard_on_exit();
    }
    Rml::Shutdown();
    Backend::Shutdown();
    return 0;
}
