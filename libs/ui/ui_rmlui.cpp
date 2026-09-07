/* Kiln UI backend — RmlUi implementation of the D10 interface.
 *
 * This is the ONLY file that knows RmlUi exists. Everything above it speaks
 * `abi/kiln_ui.h`, which contains no substrate types — that is what keeps the
 * substrate swappable (D10). Swapping backends means replacing this file.
 *
 */
/* abi/kiln_abi.h checks its slot layout with C11's `_Static_assert`, which
 * clang also takes in C++ and g++ does not. The Windows build of this file is
 * g++'s — it has to be, the vendored RmlUi archive is g++'s and the two
 * compilers disagree about C++ COMDAT sizes — so the C spelling is mapped to
 * the C++ one before the header is read. */
#if defined(__GNUC__) && !defined(__clang__) && !defined(_Static_assert)
    #define _Static_assert static_assert
#endif
#include <RmlUi/Core.h>
#include <RmlUi/Core/ElementInstancer.h>
#include <RmlUi/Core/Elements/ElementFormControl.h>
#include <RmlUi/Core/Elements/ElementFormControlInput.h>
#include <RmlUi/Core/Elements/ElementFormControlSelect.h>
#include <RmlUi/Core/Elements/ElementFormControlTextArea.h>
#include <RmlUi/Core/FileInterface.h>
#include <RmlUi/Core/Input.h>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>
#include <unordered_map>
#include <unordered_set>
#include <utility>
#include <vector>

#include <SDL.h>
#include <SDL_image.h>

#include "RmlUi_Backend.h"
#include "RmlUi_Renderer_GL3.h"
#include "RmlUi_Include_GL3.h"
#include "kiln_abi.h"   /* kn_malloc — runtime-owned allocation (D4) */
#include "a11y_bridge.h"
#include "ui_mapping.h"
#include "ui_data.h"
#include "a11y_model.h"
#include "kiln_ui.h"

namespace {

/* The one place this file knows the platform (libs/README.md, Portability):
 * setting a variable in our own environment, which POSIX spells setenv and
 * the Microsoft CRT spells _putenv_s. Everything below reads the same on both. */
void ui_setenv(const char* name, const char* value) {
#ifdef _WIN32
    _putenv_s(name, value);
#else
    setenv(name, value, 1);
#endif
}

struct UiState {
    Rml::Context* context = nullptr;
    std::unordered_map<uint64_t, bool> interactive;   /* handle -> wants hover states */
    std::unordered_map<uint64_t, uint64_t> parent_of;  /* handle -> parent handle */
    std::unordered_map<uint64_t, std::string> type_of;  /* handle -> component type */
    Rml::ElementDocument* document = nullptr;
    std::vector<Rml::Element*> widgets;   // index+1 == Kiln_Widget handle
    std::string get_scratch;
    bool initialised = false;
};
UiState g;

/* Frames drawn since kn_ui_run, for KILN_UI_EXIT_AFTER_FRAMES. */
int g_frames = 0;

Rml::Element* resolve(Kiln_Widget w) {
    if (w == 0 || w > g.widgets.size()) return nullptr;
    return g.widgets[(size_t)w - 1];
}

Kiln_Widget publish(Rml::Element* e) {
    g.widgets.push_back(e);
    return (Kiln_Widget)g.widgets.size();
}

/* --- actions ----------------------------------------------------------
 *
 * An action is one command: its caption, its shortcut, whether it can be
 * invoked at all, and the subroutine behind it, held in a single place. Every
 * control that offers that command follows it, so disabling the action greys
 * the button and renaming it renames the button.
 *
 * Controls name the action they point at rather than referring to it: a
 * property value is a literal (backend/src/lib.rs) and a component identifier
 * deliberately never reaches the binary, so a name is the only channel there
 * is.
 *
 * Action handles share the widget numbering because the compiler numbers every
 * component of a library from ONE counter, in creation order — a module-level
 * action declared after a form's children is handle N+2, and that is the number
 * `save.enabled` in a subroutine passes back. Reserving a widget slot for each
 * action is what keeps the two sequences the same sequence.
 */
struct Action {
    std::string     name;
    std::string     text;
    std::string     shortcut;
    bool            enabled = true;
    Kiln_EventFn on_execute = nullptr;
};
std::unordered_map<uint64_t, Action>   g_actions;
std::unordered_map<uint64_t, uint64_t> g_action_of;   /* widget -> action handle */

/* A form is built before the module-level components it points at, so a
 * control almost always names an action that does not exist yet. Unresolved
 * bindings wait here and are claimed when an action takes that name. */
std::vector<std::pair<uint64_t, std::string>> g_pending_bindings;

/* Disabled is a Kiln concept, not an RCSS property, and RmlUi's `disabled`
 * attribute means nothing on a plain <button> element: the events still fire
 * and the hover shading still lights up. So the state is held here and every
 * listener asks — a control that answers the mouse while disabled is disabled
 * only in the source. */
std::unordered_set<uint64_t> g_disabled;

bool widget_disabled(Kiln_Widget w) { return g_disabled.count(w) > 0; }

void set_widget_enabled(Kiln_Widget w, bool on) {
    if (on) g_disabled.erase(w);
    else    g_disabled.insert(w);
    Rml::Element* e = resolve(w);
    if (!e) return;
    e->SetProperty("opacity", on ? "1.0" : "0.4");
    /* A form control enforces its own disabled state — it stops taking the
     * keyboard, not just the click — and the listener gate above cannot do
     * that for it, because typing into a memo never reaches a listener of
     * ours. */
    if (auto* fc = rmlui_dynamic_cast<Rml::ElementFormControl*>(e)) fc->SetDisabled(!on);
}

/* Adapts a Kiln function-pointer handler to an RmlUi listener. Handlers are
 * bound by pointer, never by name, so no user identifier ships (G8). */
struct HandlerBridge : Rml::EventListener {
    Kiln_EventFn fn;
    Kiln_Widget  widget;
    HandlerBridge(Kiln_EventFn f, Kiln_Widget w) : fn(f), widget(w) {}
    void ProcessEvent(Rml::Event&) override {
        if (fn && !widget_disabled(widget)) fn();
    }
};
std::vector<HandlerBridge*> g_bridges;   // owned; freed at shutdown
/* The form's `load`. RmlUi raises no such event — the window simply comes
 * up — so it is held here and called by kn_ui_run once everything the
 * handler could touch exists and before the first frame is drawn. */
Kiln_EventFn g_on_load = nullptr;

/* Interactive visual states.
 *
 * Component properties are applied as INLINE properties, which outrank any
 * `:hover`/`:active` stylesheet rule — so a stylesheet cannot provide hover
 * feedback here. The backend therefore drives the states itself: it remembers
 * the base colour and swaps in derived shades on mouse over/down. Keeping this
 * in the backend also means every substrate gets to implement interaction the
 * way its own system prefers. */
struct StateStyler : Rml::EventListener {
    Rml::Element*  element;
    Kiln_Widget widget;
    Rml::String base;      /* colour as authored            */
    Rml::String hover;     /* lightened                     */
    Rml::String active;    /* darkened                      */

    StateStyler(Rml::Element* e, Kiln_Widget w, Rml::String b, Rml::String h, Rml::String a)
        : element(e), widget(w), base(std::move(b)), hover(std::move(h)), active(std::move(a)) {}

    void ProcessEvent(Rml::Event& ev) override {
        if (widget_disabled(widget)) return;
        const Rml::String& t = ev.GetType();
        if (t == "mouseover")      element->SetProperty("background-color", hover);
        else if (t == "mouseout")  element->SetProperty("background-color", base);
        else if (t == "mousedown") element->SetProperty("background-color", active);
        else if (t == "mouseup")   element->SetProperty("background-color", hover);
    }
};
std::vector<StateStyler*> g_stylers;   /* owned; freed at shutdown */

/* Scale an #rgb/#rrggbb colour by `factor`, clamped. */
Rml::String shade(const Rml::String& css, float factor) {
    if (css.size() < 4 || css[0] != '#') return css;
    Rml::String hex = css.substr(1);
    if (hex.size() == 3) hex = Rml::String{hex[0],hex[0],hex[1],hex[1],hex[2],hex[2]};
    if (hex.size() < 6) return css;
    char out[8];
    int v[3];
    for (int i = 0; i < 3; i++) {
        int c = (int)std::strtol(hex.substr((size_t)i * 2, 2).c_str(), nullptr, 16);
        c = (int)(c * factor);
        v[i] = c < 0 ? 0 : (c > 255 ? 255 : c);
    }
    std::snprintf(out, sizeof out, "#%02x%02x%02x", v[0], v[1], v[2]);
    return Rml::String(out);
}

/* Invokes an action on behalf of one of the controls that offer it. The action
 * decides, not the control: the same click does nothing while the action is
 * disabled, whichever button it arrived from. */
struct ActionBridge : Rml::EventListener {
    uint64_t action;
    explicit ActionBridge(uint64_t h) : action(h) {}
    void ProcessEvent(Rml::Event&) override {
        auto it = g_actions.find(action);
        if (it == g_actions.end() || !it->second.enabled || !it->second.on_execute) return;
        it->second.on_execute();
    }
};
std::vector<ActionBridge*> g_action_bridges;   /* owned; freed at shutdown */

void apply_action(uint64_t h);

uint64_t action_named(const std::string& name) {
    if (name.empty()) return 0;
    for (const auto& kv : g_actions) {
        if (kv.second.name == name) return kv.first;
    }
    return 0;
}

/* Point a control at an action: it takes the caption and the enabled state now,
 * and follows every later change to them. */
void bind_widget(Kiln_Widget w, uint64_t h) {
    g_action_of[w] = h;
    auto* b = new ActionBridge(h);
    g_action_bridges.push_back(b);
    if (Rml::Element* e = resolve(w)) e->AddEventListener("click", b);
    apply_action(h);
}

/* Push an action's state out to every control that offers it. An empty caption
 * leaves the control's own text alone, so a toolbar button showing an icon is
 * not blanked by an action that only carries a shortcut and a handler. */
void apply_action(uint64_t h) {
    auto it = g_actions.find(h);
    if (it == g_actions.end()) return;
    for (const auto& kv : g_action_of) {
        if (kv.second != h) continue;
        const Kiln_Widget w = (Kiln_Widget)kv.first;
        if (!it->second.text.empty()) kn_ui_set(w, "text", it->second.text.c_str());
        set_widget_enabled(w, it->second.enabled);
    }
}

/* An action has taken a name; adopt the controls that were waiting for it. */
void claim_pending(uint64_t h, const std::string& name) {
    if (name.empty()) return;
    for (size_t i = 0; i < g_pending_bindings.size();) {
        if (g_pending_bindings[i].second == name) {
            const Kiln_Widget w = (Kiln_Widget)g_pending_bindings[i].first;
            g_pending_bindings.erase(g_pending_bindings.begin() + (long)i);
            bind_widget(w, h);
        } else {
            i++;
        }
    }
}

/* --- shortcuts --------------------------------------------------------- */

/* `ctrl+shift+s`, `f5`, `escape` — spelled the way a menu prints it, lowercased
 * so the source can spell it either way. */
int key_for_name(const std::string& k) {
    using namespace Rml::Input;
    if (k.size() == 1) {
        const char c = k[0];
        if (c >= 'a' && c <= 'z') return KI_A + (c - 'a');
        if (c >= '0' && c <= '9') return KI_0 + (c - '0');
    }
    if (k.size() >= 2 && k[0] == 'f') {
        const int n = std::atoi(k.c_str() + 1);
        if (n >= 1 && n <= 12) return KI_F1 + (n - 1);
    }
    if (k == "escape" || k == "esc")   return KI_ESCAPE;
    if (k == "enter"  || k == "return") return KI_RETURN;
    if (k == "space")                  return KI_SPACE;
    if (k == "tab")                    return KI_TAB;
    if (k == "backspace")              return KI_BACK;
    if (k == "delete" || k == "del")   return KI_DELETE;
    if (k == "insert")                 return KI_INSERT;
    if (k == "home")                   return KI_HOME;
    if (k == "end")                    return KI_END;
    if (k == "up")                     return KI_UP;
    if (k == "down")                   return KI_DOWN;
    if (k == "left")                   return KI_LEFT;
    if (k == "right")                  return KI_RIGHT;
    return KI_UNKNOWN;
}

/* A shortcut matches only when the modifiers match exactly: ctrl+s must not
 * fire on ctrl+shift+s, which is routinely a different command. */
bool shortcut_matches(const std::string& spec, int key, bool ctrl, bool shift, bool alt) {
    std::string want_key;
    bool want_ctrl = false, want_shift = false, want_alt = false;
    std::string part;
    for (size_t i = 0; i <= spec.size(); i++) {
        if (i < spec.size() && spec[i] != '+') {
            part += (char)tolower((unsigned char)spec[i]);
            continue;
        }
        if (part == "ctrl" || part == "control")     want_ctrl = true;
        else if (part == "shift")                    want_shift = true;
        else if (part == "alt")                      want_alt = true;
        else if (!part.empty())                      want_key = part;
        part.clear();
    }
    if (want_ctrl != ctrl || want_shift != shift || want_alt != alt) return false;
    const int want = key_for_name(want_key);
    return want != Rml::Input::KI_UNKNOWN && want == key;
}

/* Shortcuts are matched on the DOCUMENT, not on the control that offers the
 * action: one that only fired while its button had focus would not be a
 * shortcut. */
struct ShortcutListener : Rml::EventListener {
    void ProcessEvent(Rml::Event& ev) override {
        const int  key   = ev.GetParameter<int>("key_identifier", 0);
        const bool ctrl  = ev.GetParameter<int>("ctrl_key", 0) != 0;
        const bool shift = ev.GetParameter<int>("shift_key", 0) != 0;
        const bool alt   = ev.GetParameter<int>("alt_key", 0) != 0;
        for (const auto& kv : g_actions) {
            const Action& a = kv.second;
            if (a.shortcut.empty() || !a.enabled || !a.on_execute) continue;
            if (!shortcut_matches(a.shortcut, key, ctrl, shift, alt)) continue;
            a.on_execute();
            ev.StopPropagation();
            return;
        }
    }
};
ShortcutListener g_shortcuts;

/* --- embedded resources ------------------------------------------------ */

/* Pictures a form names are compiled INTO the program (cli/src/main.rs), so a
 * built binary carries them instead of a promise about a path on the machine it
 * was built on. The table is weak: a program that names none defines neither
 * the symbol nor an empty stand-in, and a null table is the empty one. */
/* Outside the unnamed namespace, on purpose: g++ gives a variable declared
 * inside one internal linkage even under `extern "C"`, and then a mangled name
 * the linker has never heard of; clang does not. Outside, both spell it the
 * way the compiler emits it. */
} // namespace
extern "C" {
struct Kiln_Resource {
    const char*          name;
    const unsigned char* data;
    long long            size;
};
/* Terminated by a null `name` rather than carrying a count, because a weak
 * COUNT would have to be read through a null pointer to discover it is absent.
 * PE has no weak undefined symbol — a reference either resolves or the link
 * fails — so a Windows program always carries the table, empty when it names
 * nothing (cli/src/main.rs), and the declaration there is an ordinary extern. */
#ifdef _WIN32
extern const Kiln_Resource kn_embedded_resources[];
#else
__attribute__((weak)) extern const Kiln_Resource kn_embedded_resources[];
#endif
}
namespace {

std::string file_name_of(const std::string& path) {
    const size_t slash = path.find_last_of("/\\");
    return slash == std::string::npos ? path : path.substr(slash + 1);
}

const Kiln_Resource* find_resource(const std::string& path) {
    if (!kn_embedded_resources) return nullptr;
    for (const Kiln_Resource* r = kn_embedded_resources; r->name; r++) {
        if (path == r->name) return r;
    }
    /* The document is built from memory, so it has no URL for RmlUi to resolve
     * a source against and what arrives here may be joined, normalised, or
     * neither. The file name is what identifies a resource in every case. */
    const std::string base = file_name_of(path);
    for (const Kiln_Resource* r = kn_embedded_resources; r->name; r++) {
        if (base == file_name_of(r->name)) return r;
    }
    return nullptr;
}

/* Serves embedded resources, and everything else off the disk.
 *
 * The fallback is not optional: fonts are loaded through this same interface,
 * and an interface that only knew about resources would leave every Kiln
 * program with no text in it. */
class EmbeddedFiles : public Rml::FileInterface {
    struct Slot {
        const unsigned char* data = nullptr;
        size_t               size = 0;
        size_t               pos  = 0;
        FILE*                fp   = nullptr;
    };

public:
    Rml::FileHandle Open(const Rml::String& path) override {
        if (const Kiln_Resource* r = find_resource(path)) {
            auto* s = new Slot{r->data, (size_t)r->size, 0, nullptr};
            return (Rml::FileHandle)s;
        }
        FILE* fp = std::fopen(path.c_str(), "rb");
        if (!fp) return (Rml::FileHandle)0;
        auto* s = new Slot{nullptr, 0, 0, fp};
        return (Rml::FileHandle)s;
    }

    void Close(Rml::FileHandle file) override {
        auto* s = (Slot*)file;
        if (!s) return;
        if (s->fp) std::fclose(s->fp);
        delete s;
    }

    size_t Read(void* buffer, size_t size, Rml::FileHandle file) override {
        auto* s = (Slot*)file;
        if (!s) return 0;
        if (s->fp) return std::fread(buffer, 1, size, s->fp);
        const size_t left = s->pos < s->size ? s->size - s->pos : 0;
        const size_t n = size < left ? size : left;
        std::memcpy(buffer, s->data + s->pos, n);
        s->pos += n;
        return n;
    }

    bool Seek(Rml::FileHandle file, long offset, int origin) override {
        auto* s = (Slot*)file;
        if (!s) return false;
        if (s->fp) return std::fseek(s->fp, offset, origin) == 0;
        long base = 0;
        if (origin == SEEK_CUR)      base = (long)s->pos;
        else if (origin == SEEK_END) base = (long)s->size;
        const long want = base + offset;
        if (want < 0 || (size_t)want > s->size) return false;
        s->pos = (size_t)want;
        return true;
    }

    size_t Tell(Rml::FileHandle file) override {
        auto* s = (Slot*)file;
        if (!s) return 0;
        if (s->fp) return (size_t)std::ftell(s->fp);
        return s->pos;
    }
};
EmbeddedFiles g_files;


/* --- the controls that carry a value -----------------------------------
 *
 * A combobox's items, a memo's paragraph and a slider's position live behind
 * RmlUi's typed element interfaces, not on attributes: an attribute written
 * beside one of them goes stale the moment the user touches the control, so
 * reading it back reports what the form said rather than what the person did.
 * This is the one file allowed to know that, which is why the translation is
 * here instead of being approximated with attributes further up.
 *
 * Indices cross here too. RmlUi counts options from 0 and says -1 for none;
 * Kiln counts from 1 and says 0. The conversion happens at this boundary
 * and nowhere else.
 */

/* Items arrive as ONE text with a newline between entries — a property value
 * is a literal at the D10 boundary, so there is no `text[]` to carry and no
 * component to name in a `combobox_add` call (see ui_libinfo.c). An empty text
 * is no items; a trailing newline is the separator after the last entry rather
 * than an empty entry after it. */
std::vector<std::string> split_items(const char* text) {
    std::vector<std::string> out;
    if (!text || !*text) return out;
    std::string cur;
    for (const char* c = text; *c; c++) {
        if (*c == '\n') { out.push_back(cur); cur.clear(); }
        else             cur += *c;
    }
    if (!cur.empty()) out.push_back(cur);
    return out;
}

std::string join_items(const std::vector<std::string>& items) {
    std::string out;
    for (size_t i = 0; i < items.size(); i++) {
        if (i) out += '\n';
        out += items[i];
    }
    return out;
}

/* A form applies properties in the order its lines are written, so
 * `selected = 2` routinely arrives before the items it counts into. The wish
 * is remembered and re-applied when items land, so a form does not silently
 * depend on the order two of its lines happen to be in. */
std::unordered_map<uint64_t, int> g_wanted_selection;

Rml::Element* child_with_tag(Rml::Element* e, const char* tag) {
    if (!e) return nullptr;
    for (int i = 0; i < e->GetNumChildren(); i++) {
        if (e->GetChild(i)->GetTagName() == tag) return e->GetChild(i);
    }
    return nullptr;
}

/* --- combobox ---------------------------------------------------------- */

Rml::ElementFormControlSelect* as_select(Kiln_Widget w) {
    return rmlui_dynamic_cast<Rml::ElementFormControlSelect*>(resolve(w));
}

void combo_select(Kiln_Widget w, int n) {
    g_wanted_selection[w] = n;
    if (auto* sel = as_select(w)) sel->SetSelection(n >= 1 ? n - 1 : -1);
}

void combo_set_items(Kiln_Widget w, const char* text) {
    auto* sel = as_select(w);
    if (!sel) return;
    sel->RemoveAll();
    for (const std::string& item : split_items(text)) sel->Add(item, item);
    const auto it = g_wanted_selection.find(w);
    combo_select(w, it == g_wanted_selection.end() ? 0 : it->second);
}

std::string combo_items(Kiln_Widget w) {
    auto* sel = as_select(w);
    if (!sel) return std::string();
    std::vector<std::string> items;
    for (int i = 0; i < sel->GetNumOptions(); i++) {
        if (Rml::Element* o = sel->GetOption(i)) items.push_back(o->GetInnerRML());
    }
    return join_items(items);
}

/* --- listbox ----------------------------------------------------------- *
 *
 * RmlUi has no always-visible list, so this one is assembled from a row per
 * item. Selection is carried by a class on the chosen row rather than in a
 * table beside the widget: the element already has to say which row is
 * highlighted, and a second copy of that answer is a second copy to keep true.
 */
int list_selected(Kiln_Widget w) {
    Rml::Element* e = resolve(w);
    if (!e) return 0;
    for (int i = 0; i < e->GetNumChildren(); i++) {
        if (e->GetChild(i)->IsClassSet("oe-selected")) return i + 1;
    }
    return 0;
}

void list_select(Kiln_Widget w, int n) {
    g_wanted_selection[w] = n;
    Rml::Element* e = resolve(w);
    if (!e) return;
    for (int i = 0; i < e->GetNumChildren(); i++) e->GetChild(i)->SetClass("oe-selected", i + 1 == n);
}

struct ListItemClick : Rml::EventListener {
    Kiln_Widget list;
    int            index;   /* counts from 1, like every position */
    ListItemClick(Kiln_Widget l, int i) : list(l), index(i) {}
    void ProcessEvent(Rml::Event&) override {
        if (widget_disabled(list)) return;
        list_select(list, index);
        /* Raised on the LISTBOX, not on the row: the row is an implementation
         * detail and the handler was bound to the control the form declared. */
        if (Rml::Element* e = resolve(list)) e->DispatchEvent("change", Rml::Dictionary());
    }
};
std::vector<ListItemClick*> g_item_clicks;   /* owned; freed at shutdown */

void list_set_items(Kiln_Widget w, const char* text) {
    Rml::Element* e = resolve(w);
    if (!e || !g.document) return;
    while (e->GetNumChildren() > 0) e->RemoveChild(e->GetChild(0));
    const auto items = split_items(text);
    for (size_t i = 0; i < items.size(); i++) {
        Rml::ElementPtr row = g.document->CreateElement("div");
        if (!row) continue;
        row->SetClass("oe-item", true);
        row->SetInnerRML(items[i]);
        Rml::Element* raw = e->AppendChild(std::move(row));
        auto* click = new ListItemClick(w, (int)i + 1);
        g_item_clicks.push_back(click);
        raw->AddEventListener("click", click);
    }
    const auto it = g_wanted_selection.find(w);
    list_select(w, it == g_wanted_selection.end() ? 0 : it->second);
}

std::string list_items(Kiln_Widget w) {
    Rml::Element* e = resolve(w);
    if (!e) return std::string();
    std::vector<std::string> items;
    for (int i = 0; i < e->GetNumChildren(); i++) items.push_back(e->GetChild(i)->GetInnerRML());
    return join_items(items);
}

/* --- slider ------------------------------------------------------------ *
 *
 * The bounds are attributes and the position is a value, and RmlUi clamps the
 * position against whichever bounds it holds AT THE MOMENT the value is set —
 * so `value = 200` written above `max = 500` would stick at 100. The wanted
 * position is kept here and re-applied after every bound change, which is what
 * makes the three lines of a form order-independent.
 */
struct Range { int value = 50; };
std::unordered_map<uint64_t, Range> g_ranges;

void slider_apply(Kiln_Widget w) {
    auto* in = rmlui_dynamic_cast<Rml::ElementFormControlInput*>(resolve(w));
    if (!in) return;
    in->SetValue(std::to_string(g_ranges[w].value));
}

int slider_value(Kiln_Widget w) {
    auto* in = rmlui_dynamic_cast<Rml::ElementFormControlInput*>(resolve(w));
    if (!in) return g_ranges[w].value;
    return (int)std::strtol(in->GetValue().c_str(), nullptr, 10);
}

/* --- spinner ----------------------------------------------------------- *
 *
 * A number and the two buttons that step it. The bounds hold however the value
 * was reached — stepped, typed, or assigned from a subroutine — because a
 * spinner whose range only applies to its arrows is not a bounded number, it
 * is a text box with decoration.
 */
struct Spin {
    int value = 0, min = 0, max = 100, step = 1;
};
std::unordered_map<uint64_t, Spin> g_spins;

int clamp_int(int v, int lo, int hi) { return v < lo ? lo : (v > hi ? hi : v); }

void spin_apply(Kiln_Widget w) {
    Spin& s = g_spins[w];
    s.value = clamp_int(s.value, s.min, s.max);
    if (auto* in = rmlui_dynamic_cast<Rml::ElementFormControlInput*>(
            child_with_tag(resolve(w), "input")))
        in->SetValue(std::to_string(s.value));
}

/* Reads what the box actually holds, so a typed number is the answer rather
 * than the last one this library wrote. */
int spin_value(Kiln_Widget w) {
    Spin& s = g_spins[w];
    if (auto* in = rmlui_dynamic_cast<Rml::ElementFormControlInput*>(
            child_with_tag(resolve(w), "input"))) {
        const Rml::String text = in->GetValue();
        if (!text.empty()) s.value = clamp_int((int)std::strtol(text.c_str(), nullptr, 10), s.min, s.max);
    }
    return s.value;
}

struct SpinStep : Rml::EventListener {
    Kiln_Widget widget;
    int            direction;
    SpinStep(Kiln_Widget w, int d) : widget(w), direction(d) {}
    void ProcessEvent(Rml::Event&) override {
        if (widget_disabled(widget)) return;
        Spin& s = g_spins[widget];
        s.value = spin_value(widget) + direction * s.step;
        spin_apply(widget);
        if (Rml::Element* e = resolve(widget)) e->DispatchEvent("change", Rml::Dictionary());
    }
};
std::vector<SpinStep*> g_spin_steps;   /* owned; freed at shutdown */

/* --- grid --------------------------------------------------------------- *
 *
 * The rows live in a UiTable (ui_data.h), never in the element: a program
 * reads `count` and `grid_cell` from the table, which exists before the first
 * frame and is the same table a bound datasource hands to every grid on it.
 * The element is a picture of the table, redrawn when the table's version
 * moves — so a row added from `main`, from a handler, or into a datasource
 * three grids share reaches the screen through one path, with nothing having
 * to be told.
 *
 * `select` and `activate` hand over the row, so the handler pointer is called
 * through the signature the compiler emitted the thunk with, the same way
 * `tick` is in runtime/kn_component.c.
 */
typedef void (*RowFn)(int32_t);

struct Grid {
    UiEntry*        entry = nullptr;
    int             wanted = 0;          /* `selected` as written; 0 = none */
    const UiTable*  drawn_table = nullptr;
    int32_t         drawn_version = -1;
    int             drawn_selected = -1;
    Kiln_EventFn on_select = nullptr;
    Kiln_EventFn on_activate = nullptr;
    /* One listener per row position, kept across redraws: a grid refilled
     * every tick must not grow a listener per row per frame. */
    std::vector<struct GridRowEvent*> rows;
    int             scroll_to = 0;       /* a row to bring into view after layout */
};
std::unordered_map<uint64_t, Grid> g_grids;

/* A wish for a row past the end is kept, not clamped: rows routinely arrive
 * after `selected = 2` in the form, and after them the wish comes true. */
int grid_selected(Kiln_Widget w) {
    auto it = g_grids.find(w);
    if (it == g_grids.end()) return 0;
    const int rows = ui_table_row_count(ui_entry_table(it->second.entry));
    return it->second.wanted >= 1 && it->second.wanted <= rows ? it->second.wanted : 0;
}

void grid_fire(Kiln_EventFn fn, int row) {
    if (fn) ((RowFn)fn)(row);
}

void grid_select(Kiln_Widget w, int n, bool announce) {
    auto it = g_grids.find(w);
    if (it == g_grids.end()) return;
    it->second.wanted = n;
    if (announce) grid_fire(it->second.on_select, grid_selected(w));
}

struct GridRowEvent : Rml::EventListener {
    Kiln_Widget grid;
    int            index;   /* counts from 1 */
    GridRowEvent(Kiln_Widget g, int i) : grid(g), index(i) {}
    void ProcessEvent(Rml::Event& ev) override {
        if (widget_disabled(grid)) return;
        if (ev.GetType() == "click") {
            /* Keyboard handling is on the document and asks who has focus,
             * so a clicked grid must take it. */
            if (Rml::Element* e = resolve(grid)) e->Focus();
            grid_select(grid, index, true);
        } else if (ev.GetType() == "dblclick") {
            grid_select(grid, index, false);
            grid_fire(g_grids[grid].on_activate, index);
        }
    }
};
/* Redraw a grid whose table or selection moved since it was last drawn. */
void grid_sync(Kiln_Widget w, Grid& gr) {
    Rml::Element* e = resolve(w);
    if (!e) return;
    const UiTable* t = ui_entry_table(gr.entry);
    const int sel = grid_selected(w);
    if (t == gr.drawn_table && ui_table_version(t) == gr.drawn_version && sel == gr.drawn_selected)
        return;
    gr.drawn_table = t;
    gr.drawn_version = ui_table_version(t);
    gr.drawn_selected = sel;

    /* Released once drawn: these are runtime-owned copies, and a grid a timer
     * refills every tick would otherwise keep every text it ever drew. */
    char* columns = ui_table_columns(t);
    char* rows = ui_table_rows(t);
    e->SetInnerRML(kiln::ui::grid_markup(columns ? columns : "", rows ? rows : "", sel));
    kn_mfree(columns);
    kn_mfree(rows);

    Rml::Element* table = e->GetNumChildren() ? e->GetChild(0) : nullptr;
    if (!table) return;
    int index = 0;
    for (int i = 0; i < table->GetNumChildren(); i++) {
        Rml::Element* row = table->GetChild(i);
        if (!row->IsClassSet("oe-row")) continue;
        if ((int)gr.rows.size() < ++index) gr.rows.push_back(new GridRowEvent(w, index));
        row->AddEventListener("click", gr.rows[(size_t)index - 1]);
        row->AddEventListener("dblclick", gr.rows[(size_t)index - 1]);
    }
    /* Selection made from code or the keyboard may land outside the box, and
     * the row is what the person wants to see — but where it is will only be
     * known once the new rows are laid out, so the scroll waits for that. */
    gr.scroll_to = sel;
}

/* After layout: scroll each grid to the row its last redraw selected. Answers
 * whether anything moved, since a scroll changes what the next layout shows. */
bool scroll_grids() {
    bool moved = false;
    for (auto& kv : g_grids) {
        Grid& gr = kv.second;
        if (!gr.scroll_to) continue;
        Rml::Element* e = resolve((Kiln_Widget)kv.first);
        Rml::Element* table = e && e->GetNumChildren() ? e->GetChild(0) : nullptr;
        int index = 0;
        for (int i = 0; table && i < table->GetNumChildren(); i++) {
            Rml::Element* row = table->GetChild(i);
            if (!row->IsClassSet("oe-row") || ++index != gr.scroll_to) continue;
            row->ScrollIntoView(false);
            moved = true;
            break;
        }
        gr.scroll_to = 0;
    }
    return moved;
}

void sync_grids() {
    for (auto& kv : g_grids) grid_sync((Kiln_Widget)kv.first, kv.second);
}

/* The grid holding the focus, or 0. Focus may sit on the grid itself or on a
 * row inside it, so the search walks up. */
Kiln_Widget focused_grid() {
    Rml::Element* f = g.context ? g.context->GetFocusElement() : nullptr;
    for (; f; f = f->GetParentNode()) {
        for (const auto& kv : g_grids) {
            if (resolve((Kiln_Widget)kv.first) == f) return (Kiln_Widget)kv.first;
        }
    }
    return 0;
}

/* On the document, like shortcuts, because a keydown dispatched to the
 * document does not descend to the grid — and neither does the one the test
 * hook sends. */
struct GridKeys : Rml::EventListener {
    void ProcessEvent(Rml::Event& ev) override {
        const Kiln_Widget w = focused_grid();
        if (!w || widget_disabled(w)) return;
        const int key = ev.GetParameter<int>("key_identifier", 0);
        const int sel = grid_selected(w);
        const int rows = ui_table_row_count(ui_entry_table(g_grids[w].entry));
        if (key == Rml::Input::KI_RETURN) {
            if (sel) grid_fire(g_grids[w].on_activate, sel);
        } else if (key == Rml::Input::KI_DOWN) {
            if (sel < rows) grid_select(w, sel + 1, true);
        } else if (key == Rml::Input::KI_UP) {
            if (sel > 1) grid_select(w, sel - 1, true);
        } else {
            return;
        }
        ev.StopPropagation();
    }
};
GridKeys g_grid_keys;

/* --- datasource --------------------------------------------------------- *
 *
 * Rows with no rectangle. The entry lives in ui_data.c; all that is here is
 * the handle the compiler numbered, which shares the widget sequence for the
 * reason `action` states. */
std::unordered_map<uint64_t, UiEntry*> g_datasources;

UiEntry* entry_of(Kiln_Widget w) {
    auto g_it = g_grids.find(w);
    if (g_it != g_grids.end()) return g_it->second.entry;
    auto d_it = g_datasources.find(w);
    return d_it == g_datasources.end() ? nullptr : d_it->second;
}

/* The properties a grid and a datasource share: what a table holds. Returns
 * false for a property that is not one of them. */
bool table_set(Kiln_Widget w, const char* prop, const char* value) {
    UiEntry* e = entry_of(w);
    if (!e) return false;
    if (std::strcmp(prop, "name") == 0)         { ui_entry_set_name(e, value); return true; }
    if (std::strcmp(prop, "columns") == 0)      { ui_table_set_columns(ui_entry_table(e), value); return true; }
    if (std::strcmp(prop, "rows") == 0)         { ui_table_set_rows(ui_entry_table(e), value); return true; }
    if (std::strcmp(prop, "count") == 0)        return true;   /* read-only; swallowed as elsewhere */
    return false;
}

bool table_get(Kiln_Widget w, const char* prop, std::string* out) {
    UiEntry* e = entry_of(w);
    if (!e) return false;
    if (std::strcmp(prop, "name") == 0)    { *out = ui_entry_name(e); return true; }
    if (std::strcmp(prop, "columns") == 0) { const char* c = ui_table_columns(ui_entry_table(e)); *out = c ? c : ""; return true; }
    if (std::strcmp(prop, "rows") == 0)    { const char* r = ui_table_rows(ui_entry_table(e)); *out = r ? r : ""; return true; }
    if (std::strcmp(prop, "count") == 0)   { *out = std::to_string(ui_table_row_count(ui_entry_table(e))); return true; }
    return false;
}

/* Handle a property that belongs to one of the controls above. Returns false
 * when the property is nobody's special case and the generic attribute/RCSS
 * path below should have it. */
bool control_set(const std::string& type, Kiln_Widget w, const char* prop, const char* value) {
    const int n = (int)std::strtol(value, nullptr, 10);
    /* `count` is what the control holds, not something a form gets to declare.
     * Swallowed rather than refused: it is a readable property, and an error
     * here would make a designer that writes every property back fail. */
    if (std::strcmp(prop, "count") == 0) return true;

    if (type == "combobox") {
        if (std::strcmp(prop, "items") == 0)    { combo_set_items(w, value); return true; }
        if (std::strcmp(prop, "selected") == 0) { combo_select(w, n); return true; }
        return false;
    }
    if (type == "listbox") {
        if (std::strcmp(prop, "items") == 0)    { list_set_items(w, value); return true; }
        if (std::strcmp(prop, "selected") == 0) { list_select(w, n); return true; }
        return false;
    }
    if (type == "grid") {
        if (std::strcmp(prop, "selected") == 0) { grid_select(w, n, false); return true; }
        if (std::strcmp(prop, "bind") == 0)     { ui_entry_set_bind(entry_of(w), value); return true; }
        return table_set(w, prop, value);
    }
    if (type == "memo" && std::strcmp(prop, "text") == 0) {
        if (auto* ta = rmlui_dynamic_cast<Rml::ElementFormControlTextArea*>(resolve(w))) {
            ta->SetValue(value);
            kiln::a11y::set_label(w, value);
            return true;
        }
        return false;
    }
    if (type == "slider") {
        if (std::strcmp(prop, "value") == 0) {
            g_ranges[w].value = n;
            slider_apply(w);
            return true;
        }
        if (std::strcmp(prop, "min") == 0 || std::strcmp(prop, "max") == 0) {
            if (Rml::Element* e = resolve(w)) e->SetAttribute(prop, Rml::String(value));
            slider_apply(w);   /* the bound just moved; re-assert the position */
            return true;
        }
        return false;
    }
    if (type == "spinner") {
        Spin& s = g_spins[w];
        if (std::strcmp(prop, "value") == 0)      s.value = n;
        else if (std::strcmp(prop, "min") == 0)   s.min = n;
        else if (std::strcmp(prop, "max") == 0)   s.max = n;
        else if (std::strcmp(prop, "step") == 0)  { s.step = n > 0 ? n : 1; return true; }
        else return false;
        spin_apply(w);
        return true;
    }
    return false;
}

/* The read side of `control_set`. Returns false when the generic path should
 * answer instead. */
bool control_get(const std::string& type, Kiln_Widget w, const char* prop, std::string* out) {
    if (type == "combobox") {
        if (std::strcmp(prop, "items") == 0)    { *out = combo_items(w); return true; }
        if (std::strcmp(prop, "selected") == 0) {
            auto* sel = as_select(w);
            *out = std::to_string(sel ? sel->GetSelection() + 1 : 0);
            return true;
        }
        if (std::strcmp(prop, "count") == 0) {
            auto* sel = as_select(w);
            *out = std::to_string(sel ? sel->GetNumOptions() : 0);
            return true;
        }
        return false;
    }
    if (type == "grid") {
        if (std::strcmp(prop, "selected") == 0) { *out = std::to_string(grid_selected(w)); return true; }
        if (std::strcmp(prop, "bind") == 0)     { *out = ui_entry_bind(entry_of(w)); return true; }
        return table_get(w, prop, out);
    }
    if (type == "listbox") {
        if (std::strcmp(prop, "items") == 0)    { *out = list_items(w); return true; }
        if (std::strcmp(prop, "selected") == 0) { *out = std::to_string(list_selected(w)); return true; }
        if (std::strcmp(prop, "count") == 0) {
            Rml::Element* e = resolve(w);
            *out = std::to_string(e ? e->GetNumChildren() : 0);
            return true;
        }
        return false;
    }
    if (type == "memo" && std::strcmp(prop, "text") == 0) {
        auto* ta = rmlui_dynamic_cast<Rml::ElementFormControlTextArea*>(resolve(w));
        if (!ta) return false;
        *out = ta->GetValue();
        return true;
    }
    if (type == "slider" && std::strcmp(prop, "value") == 0) {
        *out = std::to_string(slider_value(w));
        return true;
    }
    if (type == "spinner") {
        const Spin& s = g_spins[w];
        if (std::strcmp(prop, "value") == 0)     { *out = std::to_string(spin_value(w)); return true; }
        if (std::strcmp(prop, "min") == 0)       { *out = std::to_string(s.min); return true; }
        if (std::strcmp(prop, "max") == 0)       { *out = std::to_string(s.max); return true; }
        if (std::strcmp(prop, "step") == 0)      { *out = std::to_string(s.step); return true; }
        return false;
    }
    return false;
}

/* Properties that are Kiln concepts rather than RCSS properties. */

int env_int(const char* name, int fallback) {
    const char* v = std::getenv(name);
    return v ? std::atoi(v) : fallback;
}

bool ui_debug() { return std::getenv("KILN_UI_DEBUG") != nullptr; }

/* --- anchors ------------------------------------------------------------ *
 *
 * Every control remembers the rectangle the form gave it, in the coordinates
 * of the form's DECLARED size, and the window's growth since then is what an
 * anchor answers to. The base is kept apart from the element's live style on
 * purpose: reading the style back after a resize would give the moved
 * rectangle, and anchoring that again on the next resize would move the
 * control twice. The form's size at creation is the origin the deltas are
 * measured from — a form that follows its window (ui_pump) is the same form
 * with a different `dw`,`dh`.
 */
struct Placement {
    int         left = 0, top = 0, width = 0, height = 0;
    /* Only an axis the form actually wrote can be anchored: a control that
     * never had a `width` has nothing to stretch. */
    bool        has_left = false, has_top = false, has_width = false, has_height = false;
    unsigned    mask = kiln::ui::ANCHOR_DEFAULT;
    std::string anchors = "left,top";   /* as written, for reading back */
};
std::unordered_map<uint64_t, Placement> g_placement;
int g_form_width = 0, g_form_height = 0;   /* the size the form declared */

/* How far the controls have been moved from the form's declared size — the
 * delta apply_anchors last applied, not the window's current size. Zero until
 * the first frame, so a property set while the form is being built records
 * exactly what the form wrote even when the window was created larger than
 * the form declared (KILN_UI_SIZE does that, and so may a manager). */
int g_applied_dw = 0, g_applied_dh = 0;
void resize_delta(int* dw, int* dh) {
    *dw = g_applied_dw;
    *dh = g_applied_dh;
}

/* The rectangle a control shows right now: its base, moved by its anchors. */
void visible_rect(const Placement& p, int* l, int* t, int* w, int* h) {
    int dw, dh;
    resize_delta(&dw, &dh);
    *l = p.left; *t = p.top; *w = p.width; *h = p.height;
    kiln::ui::anchored_rect(p.mask, dw, dh, l, t, w, h);
}

/* Re-derive the base from a rectangle the program wants on screen NOW.
 * `anchored_rect` is its own inverse under a negated delta, so a control set
 * to `left = 500` in a window already 200 wider is stored as 300 — and stays
 * at 500 through the next resize instead of jumping by the whole delta. */
void rebase(Placement& p, int l, int t, int w, int h) {
    int dw, dh;
    resize_delta(&dw, &dh);
    kiln::ui::anchored_rect(p.mask, -dw, -dh, &l, &t, &w, &h);
    p.left = l; p.top = t; p.width = w; p.height = h;
}

/* A numeric left/top/width/height has been set on a control: keep the base
 * in step so a later resize starts from where the program just put it. */
void record_geometry(Kiln_Widget w, const char* prop, int value) {
    Placement& p = g_placement[w];
    int l, t, wd, ht;
    visible_rect(p, &l, &t, &wd, &ht);
    if (std::strcmp(prop, "left") == 0)        { l = value;  p.has_left = true; }
    else if (std::strcmp(prop, "top") == 0)    { t = value;  p.has_top = true; }
    else if (std::strcmp(prop, "width") == 0)  { wd = value; p.has_width = true; }
    else if (std::strcmp(prop, "height") == 0) { ht = value; p.has_height = true; }
    rebase(p, l, t, wd, ht);
}

/* The window's size moved: put every anchored control where its anchors say.
 * A control with the default anchors is left alone — its rectangle is what
 * the form wrote and nothing here needs to touch it. */
void apply_anchors(int dw, int dh) {
    g_applied_dw = dw;
    g_applied_dh = dh;
    for (auto& kv : g_placement) {
        const Placement& p = kv.second;
        if (!(p.mask & (kiln::ui::ANCHOR_RIGHT | kiln::ui::ANCHOR_BOTTOM))) continue;
        Rml::Element* e = resolve((Kiln_Widget)kv.first);
        if (!e) continue;
        int l = p.left, t = p.top, w = p.width, h = p.height;
        kiln::ui::anchored_rect(p.mask, dw, dh, &l, &t, &w, &h);
        if (p.has_left)   e->SetProperty("left",   Rml::String(std::to_string(l) + "px"));
        if (p.has_top)    e->SetProperty("top",    Rml::String(std::to_string(t) + "px"));
        if (p.has_width)  e->SetProperty("width",  Rml::String(std::to_string(w) + "px"));
        if (p.has_height) e->SetProperty("height", Rml::String(std::to_string(h) + "px"));
        /* By handle, never by name: component identifiers do not reach the
         * binary (G8), and the handle is what a test can address. */
        if (ui_debug())
            std::fprintf(stderr, "ui: anchored %llu -> %d,%d %dx%d\n",
                         (unsigned long long)kv.first, l, t, w, h);
    }
}

/* --- the window's place on the screen ------------------------------------ *
 *
 * The three form properties are held together and applied from the set,
 * because a form writes them in whichever order its lines happen to be in
 * and `position = "manual"` must mean the same thing above `left` as below
 * it. After the window is up (kn_ui_run), `left`/`top` move a manual window
 * and `position` is ignored: a window that switched from centred to manual
 * mid-run would jump to a corner the form never asked to see.
 *
 * The substrate creates the window centred, and that file is not this
 * library's to change — so `default` means "where the substrate put it",
 * which is the centre of the screen today, and a window manager may still
 * override any of this (Wayland ignores a client's position entirely).
 */
struct WindowPlace {
    std::string mode = "default";
    int left = 0, top = 0;
};
WindowPlace g_window;
bool g_running = false;   /* kn_ui_run has begun; the window is on screen */

void apply_window_position() {
    SDL_Window* win = SDL_GL_GetCurrentWindow();
    if (!win) return;
    if (g_window.mode == "center")
        SDL_SetWindowPosition(win, SDL_WINDOWPOS_CENTERED, SDL_WINDOWPOS_CENTERED);
    else if (g_window.mode == "manual")
        SDL_SetWindowPosition(win, g_window.left, g_window.top);
    if (ui_debug())
        std::fprintf(stderr, "ui: window position %s %d,%d\n",
                     g_window.mode.c_str(), g_window.left, g_window.top);
}

/* -1 when the property is not one of the window's; else the set's status. */
int window_set(const char* prop, const char* value) {
    if (std::strcmp(prop, "position") == 0) {
        if (g_running) return 0;   /* ignored once the window exists; see above */
        if (std::strcmp(value, "default") != 0 && std::strcmp(value, "center") != 0 &&
            std::strcmp(value, "manual") != 0) {
            kn_error_set(KN_ERR_INVALID_ARG, "position: expected default, center or manual");
            return 1;
        }
        g_window.mode = value;
    } else if (std::strcmp(prop, "left") == 0) {
        g_window.left = (int)std::strtol(value, nullptr, 10);
    } else if (std::strcmp(prop, "top") == 0) {
        g_window.top = (int)std::strtol(value, nullptr, 10);
    } else {
        return -1;
    }
    if (!g_running || g_window.mode == "manual") apply_window_position();
    return 0;
}

bool window_get(const char* prop, std::string* out) {
    if (std::strcmp(prop, "position") == 0)  { *out = g_window.mode; return true; }
    if (std::strcmp(prop, "left") == 0)      { *out = std::to_string(g_window.left); return true; }
    if (std::strcmp(prop, "top") == 0)       { *out = std::to_string(g_window.top); return true; }
    return false;
}

} // namespace

extern "C" {

int kn_ui_init(const char* title, int width, int height) {
    if (g.initialised) return 0;
    /* A headless run must not open a window: a test or an agent that
     * renders a frame to a file steals focus from whoever is working on
     * the machine otherwise. SDL's offscreen driver renders through EGL
     * with no window at all, and a caller who set SDL_VIDEODRIVER
     * knows better than this default, as does KILN_UI_WINDOW=1 — the
     * one test that reads the manager's own flags back needs a real window.
     * A Linux fact, so a Linux default: the offscreen driver draws through
     * EGL, which Windows has no build of, and a Windows program asked for a
     * frame count keeps its ordinary window and stops after that many. */
#ifndef _WIN32
    if (!std::getenv("SDL_VIDEODRIVER") && !std::getenv("KILN_UI_WINDOW") && (std::getenv("KILN_UI_EXIT_AFTER_FRAMES") || std::getenv("KILN_UI_DUMP")))
        ui_setenv("SDL_VIDEODRIVER", "offscreen");
#endif
    /* KILN_UI_SIZE sizes the window at creation rather than after: an
     * offscreen surface cannot grow, so a resize there paints nothing new.
     * The form still declares its own size, which is what anchors measure
     * against, so the first frame sees the difference as a resize. */
    g_form_width = width;    /* the DECLARED size: what anchors measure against */
    g_form_height = height;
    {
        int nw = 0, nh = 0;
        const char* sz = std::getenv("KILN_UI_SIZE");
        if (sz && std::sscanf(sz, "%dx%d", &nw, &nh) == 2 && nw > 0 && nh > 0) {
            width = nw;
            height = nh;
        }
    }
    if (!Backend::Initialize(title ? title : "Kiln", width, height, true)) return 1;
    Rml::SetSystemInterface(Backend::GetSystemInterface());
    Rml::SetRenderInterface(Backend::GetRenderInterface());
    /* Before Initialise, which installs the stdio interface itself if nothing
     * else has claimed the slot — and after that a resource would be looked for
     * on disk, which is the whole thing embedding exists to stop. */
    Rml::SetFileInterface(&g_files);
    Rml::Initialise();

    /* Fonts: try a few common paths so the spike works on a bare system. */
    /* Font list and family names come from the shared mapping, so the designer
     * and the built app resolve the same font. */
    int font_count = 0;
    const auto* fonts = kiln::ui::font_candidates(&font_count);
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

    g.context = Rml::CreateContext("main", Rml::Vector2i(width, height));
    if (!g.context) return 1;

    /* D21: forms are ALWAYS instantiated into a stylesheet-seeded document.
     * A bare CreateDocument() silently drops decorators while SetProperty still
     * returns true — the spike's most expensive finding. */
    const std::string seed = kiln::ui::seed_document(width, height, family);
    g.document = g.context->LoadDocumentFromMemory(seed);
    if (!g.document) return 1;
    g.document->Show();

    /* The origin every anchor measures from, kept from the declared size and
     * not read back from the window: a window manager may have already made
     * the window something else by the first frame. */

    g.widgets.clear();
    publish(g.document);          // handle 1 == the form root
    g.document->AddEventListener("keydown", &g_shortcuts);
    g.document->AddEventListener("keydown", &g_grid_keys);
    g.initialised = true;
    return 0;
}

Kiln_Widget kn_ui_root(void) { return g.initialised ? 1 : 0; }

Kiln_Widget kn_ui_create(Kiln_Widget parent, const char* type_name) {
    if (!g.initialised || !type_name) return 0;
    Rml::Element* p = parent ? resolve(parent) : g.document;
    if (!p) return 0;
    Rml::ElementPtr child = g.document->CreateElement(kiln::ui::tag_for(type_name));
    if (!child) return 0;
    // Some substrate elements need an attribute at creation (an <input> needs
    // its type before it will behave as a text box or a checkbox).
    const char* attr_value = nullptr;
    if (const char* attr = kiln::ui::creation_attribute(type_name, &attr_value)) {
        child->SetAttribute(attr, Rml::String(attr_value));
    }
    Rml::Element* raw = p->AppendChild(std::move(child));
    if (!raw) return 0;
    Kiln_Widget h = publish(raw);
    /* Buttons get interaction feedback; a control that does not visibly respond
     * to the mouse reads as broken regardless of whether its event fires. */
    if (std::strcmp(type_name, "button") == 0) g.interactive[h] = true;
    g.parent_of[h] = parent ? parent : 1;
    g.type_of[h] = type_name;
    if (const char* cls = kiln::ui::class_for(type_name))
        raw->SetAttribute("class", Rml::String(cls));
    if (const char* markup = kiln::ui::inner_markup(type_name)) raw->SetInnerRML(markup);
    /* The arrows have to be wired at creation: a form that never assigns a
     * spinner property would otherwise get two buttons that do nothing. */
    if (std::strcmp(type_name, "spinner") == 0) {
        g_spins[h] = Spin{};
        for (int i = 0; i < raw->GetNumChildren(); i++) {
            Rml::Element* c = raw->GetChild(i);
            if (c->GetTagName() != "button") continue;
            auto* step = new SpinStep(h, c->IsClassSet("oe-up") ? +1 : -1);
            g_spin_steps.push_back(step);
            c->AddEventListener("click", step);
        }
        spin_apply(h);
    }
    /* Same reason: a radio button with no `group` written must still be
     * exclusive against the other radio buttons that wrote none. */
    if (std::strcmp(type_name, "radiobutton") == 0) {
        if (Rml::Element* box = child_with_tag(raw, "input"))
            box->SetAttribute("name", Rml::String("default"));
    }
    if (std::strcmp(type_name, "grid") == 0) {
        Grid gr;
        gr.entry = ui_entry_new(UI_ENTRY_GRID);
        g_grids[h] = gr;
        /* A focused grid is what the arrow keys and Enter act on. */
        raw->SetProperty("tab-index", "auto");
    }
    // RmlUi's <progress> defaults to max=1, which makes any percentage look
    // full; Kiln's `value` is a percentage.
    if (std::strcmp(type_name, "progressbar") == 0) raw->SetAttribute("max", Rml::String("100"));
    return h;
}

int kn_ui_set(Kiln_Widget w, const char* property, const char* value) {
    Rml::Element* e = resolve(w);
    if (!e || !property || !value) return 1;

    /* An editbox is an <input> (single line) or a <textarea> (multi-line); the
     * tag cannot change after creation, so turning `multiline` on or off
     * replaces the element in place — its value, class and geometry carried
     * over. Properties are set before events are bound (the backend's order),
     * so no handler is lost by swapping here. */
    if (std::strcmp(property, "multiline") == 0 &&
        g.type_of.count(w) && g.type_of[w] == "editbox") {
        const bool on = std::strcmp(value, "true") == 0 || std::strcmp(value, "1") == 0;
        const bool is_area = e->GetTagName() == "textarea";
        if (on == is_area) return 0;   /* already the right shape */
        Rml::Element* parent = e->GetParentNode();
        if (!parent) return 1;
        Rml::ElementPtr fresh = g.document->CreateElement(on ? "textarea" : "input");
        if (!fresh) return 1;
        if (!on) fresh->SetAttribute("type", Rml::String("text"));
        /* Carry the class (its styling) and the current text across. */
        fresh->SetAttribute("class", e->GetAttribute<Rml::String>("class", "oe-input"));
        Rml::String v;
        if (auto* oldc = dynamic_cast<Rml::ElementFormControl*>(e)) v = oldc->GetValue();
        /* Geometry (left/top/width/height) was set as inline properties before
         * `multiline`, so carry every inline property across or the swapped
         * control loses its place and size. */
        const auto inl = e->GetLocalStyleProperties();
        Rml::Element* r = fresh.get();                 /* ReplaceChild returns the OLD node */
        parent->ReplaceChild(std::move(fresh), e);
        for (const auto& kv : inl) r->SetProperty(kv.first, kv.second);
        if (auto* newc = dynamic_cast<Rml::ElementFormControl*>(r)) newc->SetValue(v);
        g.widgets[(size_t)w - 1] = r;                  /* the handle now names the new element */
        return 0;
    }

    /* Pointing a control at an action, by the action's name. The form is built
     * before the module-level components, so the action named here usually does
     * not exist yet and the binding waits for it. */
    if (std::strcmp(property, "action") == 0) {
        if (const uint64_t h = action_named(value)) bind_widget(w, h);
        else if (*value) g_pending_bindings.emplace_back((uint64_t)w, value);
        return 0;
    }
    if (std::strcmp(property, "enabled") == 0) {
        set_widget_enabled(w, std::strcmp(value, "true") == 0 || std::strcmp(value, "1") == 0);
        return 0;
    }
    /* The form's `left`/`top` place the WINDOW, not a box in the document —
     * a form has no parent to be offset in. Caught before the generic path
     * would write them into the stylesheet as pixel lengths. */
    if (w == 1) {
        if (const int status = window_set(property, value); status >= 0) return status;
    }

    const std::string type = g.type_of.count(w) ? g.type_of[w] : std::string("form");

    if (std::strcmp(property, "anchors") == 0) {
        unsigned mask = 0;
        if (w == 1 || !kiln::ui::parse_anchors(value, &mask)) {
            kn_error_set(KN_ERR_INVALID_ARG,
                         "anchors: expected a comma-separated subset of left, top, right, bottom");
            return 1;
        }
        /* The control must not move on the spot: what shows now stays, and the
         * new anchors decide only what the NEXT resize does with it. */
        Placement& p = g_placement[w];
        int l, t, wd, ht;
        visible_rect(p, &l, &t, &wd, &ht);
        p.mask = mask;
        p.anchors = value;
        rebase(p, l, t, wd, ht);
        return 0;
    }

    // Values that live behind a typed element interface rather than on an
    // attribute or in the stylesheet.
    if (control_set(type, w, property, value)) return 0;

    // Attribute-backed properties (an editbox's value, a checkbox's checked
    // state, an image's source) are not RCSS styling.
    if (const char* attr = kiln::ui::attribute_for(type.c_str(), property)) {
        // Composite components route to the child that actually carries it.
        // `checked` and the radio group both belong to the inner <input>: set
        // on the wrapper they would be inert, and exclusion would not work.
        Rml::Element* target = e;
        if (kiln::ui::is_composite(type.c_str()) &&
            (std::strcmp(property, "checked") == 0 || std::strcmp(property, "group") == 0)) {
            if (Rml::Element* box = child_with_tag(e, "input")) target = box;
        }
        e = target;
        if (std::strcmp(property, "checked") == 0) {
            const bool on = std::strcmp(value, "true") == 0 || std::strcmp(value, "1") == 0;
            if (on) e->SetAttribute(attr, Rml::String("checked"));
            else e->RemoveAttribute(attr);
        } else {
            e->SetAttribute(attr, Rml::String(value));
        }
        return 0;
    }

    if (kiln::ui::is_text_property(property) && kiln::ui::text_is_content(type.c_str())) {
        if (kiln::ui::is_composite(type.c_str())) {
            for (int i = 0; i < e->GetNumChildren(); i++) {
                if (e->GetChild(i)->GetTagName() == "span") {
                    e->GetChild(i)->SetInnerRML(value);
                    return 0;
                }
            }
        }
        e->SetInnerRML(value);
        /* Keep the accessible name in step: a screen reader must announce the
         * current text, not whatever it was at construction. */
        kiln::a11y::set_label(w, value);
        return 0;
    }

    const std::string v = kiln::ui::rcss_value(property, value);

    if (w == 1 && std::strcmp(property, "title") == 0) return 0;  /* window title: set at init */
    if (w == 1 && std::strcmp(property, "icon") == 0) {
        /* Embedded bytes first, the path second, the same order every other
         * resource resolves in — so the icon a program shipped with wins over
         * whatever happens to sit at that path on the running machine. */
        SDL_Surface* icon = nullptr;
        if (const Kiln_Resource* r = find_resource(value)) {
            icon = IMG_Load_RW(SDL_RWFromConstMem(r->data, (int)r->size), 1);
        } else if (value && *value) {
            icon = IMG_Load(value);
        }
        if (icon) {
            if (SDL_Window* win = SDL_GL_GetCurrentWindow()) SDL_SetWindowIcon(win, icon);
            if (std::getenv("KILN_UI_DEBUG"))
                std::fprintf(stderr, "ui: window icon %dx%d\n", icon->w, icon->h);
            SDL_FreeSurface(icon);
        } else if (value && *value) {
            kn_error_set(KN_ERR_INVALID_ARG, "form icon could not be loaded");
        }
        return 0;
    }

    /* Refused here rather than handed on: the substrate's answer to `#44444`
     * is a syntax error on stderr and a SetProperty that still says yes, so
     * nothing a program can read would ever know. */
    if (kiln::ui::is_colour_property(property) && !kiln::ui::is_hex_colour(value)) {
        char msg[160];
        std::snprintf(msg, sizeof msg, "%s: '%.32s' is not a colour (#rgb, #rrggbb or #rrggbbaa)",
                      property, value);
        kn_error_set(KN_ERR_INVALID_ARG, msg);
        return 1;
    }

    /* A control's geometry is also its anchor base (see Placement). Only a
     * bare number counts: `50%` is a length the stylesheet understands and
     * an anchor cannot add pixels to. */
    if (w != 1 && kiln::ui::is_length_property(property) &&
        std::strcmp(property, "border_radius") != 0 && *value &&
        std::strspn(value, "-0123456789") == std::strlen(value)) {
        record_geometry(w, property, (int)std::strtol(value, nullptr, 10));
    }

    bool ok = e->SetProperty(kiln::ui::rcss_name(property), v);

    if (ok && std::strcmp(property, "background_color") == 0 && g.interactive.count(w)) {
        /* A declared fill carries its own outline. The stylesheet gives an
         * undeclared button the neutral grey ring the specification asks for,
         * and that ring round a program's chosen blue reads as a mistake — a
         * filled button's border is its fill, everywhere this look comes from.
         * Only the border COLOUR is taken: the width and radius stay whatever
         * the sheet or the form set. */
        e->SetProperty("border-color", v);
        auto* st = new StateStyler(e, w, v, shade(v, 1.18f), shade(v, 0.82f));
        g_stylers.push_back(st);
        for (const char* ev : {"mouseover", "mouseout", "mousedown", "mouseup"})
            e->AddEventListener(ev, st);
    }
    return ok ? 0 : 1;
}

const char* kn_ui_get(Kiln_Widget w, const char* property) {
    Rml::Element* e = resolve(w);
    if (!e || !property) return nullptr;

    if (std::strcmp(property, "enabled") == 0) {
        const char* lit = widget_disabled(w) ? "false" : "true";
        char* out = (char*)kn_malloc((long)std::strlen(lit) + 1);
        if (out) std::strcpy(out, lit);
        return out;
    }

    const std::string type = g.type_of.count(w) ? g.type_of[w] : std::string("form");

    Rml::String value;
    std::string special;
    if (control_get(type, w, property, &special)) {
        value = special;
    } else if (w == 1 && window_get(property, &special)) {
        value = special;
    } else if (w != 1 && std::strcmp(property, "anchors") == 0) {
        /* As written, or the default a form that wrote nothing has. */
        const auto it = g_placement.find(w);
        value = it == g_placement.end() ? "left,top" : it->second.anchors;
    } else if (const char* attr = kiln::ui::attribute_for(type.c_str(), property)) {
        // Attribute-backed properties must be READ from the attribute too, or
        // an editbox reports its markup instead of what the user typed.
        Rml::Element* from = e;
        if (kiln::ui::is_composite(type.c_str()) &&
            (std::strcmp(property, "checked") == 0 || std::strcmp(property, "group") == 0)) {
            if (Rml::Element* box = child_with_tag(e, "input")) from = box;
        }
        if (std::strcmp(property, "checked") == 0) {
            /* RmlUi records a checkbox as the attribute's PRESENCE, and a user
             * click sets it to the empty string — so reading the value cannot
             * tell a box the user ticked from one that is clear. Presence can,
             * and a truth value must read back as the "true"/"false" the core
             * implementation of this ABI already answers. */
            value = from->HasAttribute(attr) ? "true" : "false";
        } else {
            value = from->GetAttribute<Rml::String>(attr, "");
        }
    } else if (kiln::ui::is_text_property(property) &&
               kiln::ui::text_is_content(type.c_str())) {
        value = e->GetInnerRML();
    } else {
        const Rml::Property* p = e->GetProperty(kiln::ui::rcss_name(property));
        if (!p) return nullptr;
        value = p->ToString();
    }

    /* Return a runtime-owned copy rather than a shared scratch buffer, so two
     * reads in one expression — concat(a.text, b.text) — cannot alias. Freed
     * with all other runtime data at shutdown (D4). */
    char* out = (char*)kn_malloc((long)value.size() + 1);
    if (!out) return nullptr;
    std::memcpy(out, value.c_str(), value.size() + 1);
    return out;
}

int32_t kn_ui_get_int(Kiln_Widget w, const char* property) {
    /* A truth value does not read as a number: `strtol("true")` is 0, which is
     * the wrong answer rather than a missing one. */
    if (std::strcmp(property, "enabled") == 0) return widget_disabled(w) ? 0 : 1;
    const char* text = kn_ui_get(w, property);
    return text ? (int32_t)std::strtol(text, nullptr, 10) : 0;
}

int kn_ui_on(Kiln_Widget w, const char* event, Kiln_EventFn handler) {
    Rml::Element* e = resolve(w);
    if (!e || !event || !handler) return 1;
    /* A grid's events carry the row, so they are raised by this file with the
     * argument rather than by the substrate through a void bridge. */
    if (auto it = g_grids.find(w); it != g_grids.end()) {
        if (std::strcmp(event, "select") == 0)   { it->second.on_select = handler; return 0; }
        if (std::strcmp(event, "activate") == 0) { it->second.on_activate = handler; return 0; }
    }
    /* The form's own `load`, which no element raises: see g_on_load. */
    if (w == 1 && std::strcmp(event, "load") == 0) {
        g_on_load = handler;
        return 0;
    }
    auto* bridge = new HandlerBridge(handler, w);
    g_bridges.push_back(bridge);
    e->AddEventListener(event, bridge);
    return 0;
}

/* --- the library's non-visual components (abi/kiln_abi.h) ------------
 *
 * `action` and `datasource`, addressed through these rather than through the
 * widget interface because neither has a rectangle. The five entry points
 * are the same five `timer` implements in runtime/kn_component.c.
 */
int64_t kn_ui_component_create(const char* type_name) {
    if (type_name && std::strcmp(type_name, "datasource") == 0) {
        const Kiln_Widget h = publish(nullptr);
        g_datasources[h] = ui_entry_new(UI_ENTRY_DATASOURCE);
        kn_error_clear();
        return (int64_t)h;
    }
    if (!type_name || std::strcmp(type_name, "action") != 0) {
        kn_error_set(KN_ERR_INVALID_ARG, "ui declares no such non-visual component");
        return 0;
    }
    /* A widget slot is reserved for the action so the two kinds of component
     * keep ONE numbering, which is what the compiler assumed when it handed a
     * subroutine the handle to write `save.enabled` through. */
    const Kiln_Widget h = publish(nullptr);
    g_actions[h] = Action{};
    kn_error_clear();
    return (int64_t)h;
}

int32_t kn_ui_component_set(int64_t h, const char* prop, const char* value) {
    if (prop && value && g_datasources.count((uint64_t)h))
        return table_set((Kiln_Widget)h, prop, value) ? 0 : 1;
    auto it = g_actions.find((uint64_t)h);
    if (it == g_actions.end() || !prop || !value) return 1;
    Action& a = it->second;
    if (std::strcmp(prop, "name") == 0) {
        a.name = value;
        claim_pending((uint64_t)h, a.name);
        return 0;
    }
    if (std::strcmp(prop, "text") == 0)          a.text = value;
    else if (std::strcmp(prop, "shortcut") == 0) a.shortcut = value;
    else if (std::strcmp(prop, "enabled") == 0)
        a.enabled = std::strcmp(value, "true") == 0 || std::strcmp(value, "1") == 0;
    else return 1;
    apply_action((uint64_t)h);
    return 0;
}

const char* kn_ui_component_get(int64_t h, const char* prop) {
    std::string v;
    if (prop && g_datasources.count((uint64_t)h)) {
        if (!table_get((Kiln_Widget)h, prop, &v)) return nullptr;
        char* out = (char*)kn_malloc((long)v.size() + 1);
        if (out) std::memcpy(out, v.c_str(), v.size() + 1);
        return out;
    }
    auto it = g_actions.find((uint64_t)h);
    if (it == g_actions.end() || !prop) return nullptr;
    const Action& a = it->second;
    if (std::strcmp(prop, "name") == 0)          v = a.name;
    else if (std::strcmp(prop, "text") == 0)     v = a.text;
    else if (std::strcmp(prop, "shortcut") == 0) v = a.shortcut;
    else if (std::strcmp(prop, "enabled") == 0)  v = a.enabled ? "true" : "false";
    else return nullptr;
    /* Runtime-owned, like every other text result, so a caller may hold it. */
    char* out = (char*)kn_malloc((long)v.size() + 1);
    if (out) std::memcpy(out, v.c_str(), v.size() + 1);
    return out;
}

int32_t kn_ui_component_get_int(int64_t h, const char* prop) {
    if (prop && g_datasources.count((uint64_t)h)) {
        std::string v;
        return table_get((Kiln_Widget)h, prop, &v) ? (int32_t)std::strtol(v.c_str(), nullptr, 10) : 0;
    }
    auto it = g_actions.find((uint64_t)h);
    if (it == g_actions.end() || !prop) return 0;
    if (std::strcmp(prop, "enabled") == 0) return it->second.enabled ? 1 : 0;
    return 0;
}

int32_t kn_ui_component_on(int64_t h, const char* event, Kiln_EventFn handler) {
    auto it = g_actions.find((uint64_t)h);
    if (it == g_actions.end() || !event || !handler) return 1;
    if (std::strcmp(event, "execute") != 0) return 1;
    it->second.on_execute = handler;
    return 0;
}

int kn_ui_set_a11y(Kiln_Widget w, int32_t role, const char* name) {
    Rml::Element* e = resolve(w);
    if (!e) return 1;
    /* Publish into the substrate-free model the AccessKit bridge serves. The
     * model is the thread boundary: adapter callbacks read it, never widgets. */
    kiln::a11y::Node n;
    n.id = w;
    n.parent = (w == 1) ? 0 : (g.parent_of.count(w) ? g.parent_of[w] : 1);
    n.role = role;
    n.label = name ? name : "";
    n.clickable = g.interactive.count(w) > 0;
    kiln::a11y::put_node(n);
    return 0;
}

/* One turn of the window: pump input, lay out, publish accessibility, draw.
 *
 * This is a runtime event SOURCE, not a loop of its own. The loop belongs to
 * the runtime (abi/kiln_abi.h), so a window and a timer can be alive in the
 * same program — which they cannot be when whichever library is linked owns the
 * only loop there is. Registered with a period of 0: a window wants every turn
 * it can get, and deliberately does not power-save, or an app whose window is
 * not focused stops updating and its timers and animation stop with it.
 */
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

static int32_t ui_pump(void *) {
    const int max_frames = env_int("KILN_UI_EXIT_AFTER_FRAMES", 0);
    const char* dump_path = std::getenv("KILN_UI_DUMP");

    if (max_frames == 0 && !Backend::ProcessEvents(g.context, nullptr, false)) {
        /* The window closed. That ends the PROGRAM, not just this source: a
         * windowless process still running its timers is not what closing a
         * window has ever meant. */
        kn_loop_quit(0);
        return 1;
    }
    /* Headless hook: a window manager's resize cannot be scripted, a size can.
     * Same shape as the designer's KILN_DESIGNER_WELCOME_SIZE. */
    static bool sized_once = false;
    if (!sized_once) {
        sized_once = true;
        int nw = 0, nh = 0;
        const char* sz = std::getenv("KILN_UI_SIZE");
        if (sz && std::sscanf(sz, "%dx%d", &nw, &nh) == 2 && nw > 0 && nh > 0) {
            if (SDL_Window* win = SDL_GL_GetCurrentWindow()) {
                SDL_SetWindowSize(win, nw, nh);
                announce_window_size(win, nw, nh);
            }
            for (int i = 0; i < 30; i++) Backend::ProcessEvents(g.context, nullptr, false);
            static_cast<RenderInterface_GL3*>(Backend::GetRenderInterface())->SetViewport(nw, nh);
            g.context->SetDimensions(Rml::Vector2i(nw, nh));
        }
    }

    /* The form IS the window. The seed document fixes the body to the size the
     * form declared, and a window manager that maximises us does so after that
     * layout — so everything past the declared size stayed black. Follow the
     * window: the backend has already applied its size to the context. */
    static Rml::Vector2i last_dims;
    if (const auto now = g.context->GetDimensions(); now != last_dims) {
        last_dims = now;
        g.document->SetProperty("width", Rml::String(std::to_string(now.x) + "px"));
        g.document->SetProperty("height", Rml::String(std::to_string(now.y) + "px"));
        /* Same moment, same delta: the controls follow the window the way
         * their anchors say, before this frame is laid out. */
        apply_anchors(now.x - g_form_width, now.y - g_form_height);
    }
    sync_grids();
    g.context->Update();
    if (scroll_grids()) g.context->Update();

    /* Refresh accessible bounds from the laid-out widgets, then publish.
     * Cheap: update_if_active does nothing until an AT connects. */
    for (size_t i = 0; i < g.widgets.size(); i++) {
        if (Rml::Element* e = g.widgets[i]) {
            auto off = e->GetAbsoluteOffset();
            auto size = e->GetBox().GetSize();
            kiln::a11y::set_bounds((uint64_t)i + 1, off.x, off.y, size.x, size.y);
        }
    }
    kiln::a11y::bridge_publish();

    /* An assistive technology may have asked to activate a control. The
     * request arrived on the adapter thread and was queued; dispatch it
     * here, on the main thread, where touching widgets is safe. */
    for (uint64_t id : kiln::a11y::take_actions()) {
        if (Rml::Element* target = resolve(id)) {
            if (std::getenv("KILN_A11Y_TRACE"))
                std::fprintf(stderr, "kiln-a11y: dispatching click to widget %llu\n",
                             (unsigned long long)id);
            kiln::a11y::set_focus(id);
            target->DispatchEvent("click", Rml::Dictionary());
        }
    }

    Backend::BeginFrame();
    g.context->Render();

    /* A frame-limited run counts frames and a timer counts time: with no
     * vsync (the offscreen driver has none) eight frames take a millisecond
     * and a 20 ms timer never fires. Pace headless frames as a display would;
     * KILN_UI_FRAME_MS=0 for a run that counts frames as a lifetime. */
    if (max_frames > 0) SDL_Delay(env_int("KILN_UI_FRAME_MS", 16));
    if (max_frames > 0 && ++g_frames >= max_frames) {
        /* Test hook: print the accessibility tree. Substrate-independent
         * and needs no accessibility bus, so it works in CI. */
        if (const char* d = std::getenv("KILN_UI_DUMP_A11Y")) {
            if (*d && d[0] != '0') {
                for (const auto& n : kiln::a11y::snapshot()) {
                    std::printf("a11y: id=%llu parent=%llu role=%d bounds=%.0f,%.0f,%.0fx%.0f name=\"%s\"%s\n",
                                (unsigned long long)n.id, (unsigned long long)n.parent,
                                n.role, n.x, n.y, n.w, n.h, n.label.c_str(),
                                n.clickable ? " clickable" : "");
                }
                std::printf("a11y: adapter_active=%d\n", (int)kiln::a11y::bridge_active());
            }
        }
        auto* gl3 = static_cast<RenderInterface_GL3*>(Backend::GetRenderInterface());
        gl3->EndFrame();
        if (dump_path) {
            int W = g.context->GetDimensions().x, H = g.context->GetDimensions().y;
            std::vector<unsigned char> px((size_t)W * H * 3);
            glReadPixels(0, 0, W, H, GL_RGB, GL_UNSIGNED_BYTE, px.data());
            if (FILE* f = std::fopen(dump_path, "wb")) {
                std::fprintf(f, "P6\n%d %d\n255\n", W, H);
                for (int y = H - 1; y >= 0; y--) std::fwrite(&px[(size_t)y * W * 3], 1, (size_t)W * 3, f);
                std::fclose(f);
            }
        }
        kn_loop_quit(0);
        return 1;
    }
    Backend::PresentFrame();
    return 0;
}

int kn_ui_run(void) {
    if (!g.initialised) return 1;
    g_running = true;   /* from here on, the form's `position` is settled */

    const char* synth_click = std::getenv("KILN_UI_SYNTH_CLICK");
    const char* mouse_at = std::getenv("KILN_UI_MOUSE");   /* "x,y" — drives hover */

    if (synth_click) {
        /* Targets a widget HANDLE, not an element id — component ids are
         * compile-time only and never reach the binary (G8), so the test hook
         * must not depend on them. */
        /* Grids draw their rows on the first frame; a click aimed at a row
         * before that would find nothing. */
        sync_grids();
        g.context->Update();
        char* after = nullptr;
        Kiln_Widget target = (Kiln_Widget)std::strtoull(synth_click, &after, 10);
        Rml::Element* e = resolve(target);
        /* `5.3` clicks the third part of widget 5 — a listbox row, a spinner
         * arrow — and `5.1.3` the third part of that part, which is how a grid
         * row is reached through the table holding it (with `columns` set the
         * header is part 1 and row N is `.1.N+1`; without, row N is `.1.N`).
         * A control assembled from several elements is
         * only exercised by hitting one of them, and its parts have no handles
         * of their own: a part is not a component, so it must not become
         * addressable from a program just to be testable from a test. */
        while (e && after && *after == '.') {
            const int nth = (int)std::strtol(after + 1, &after, 10);
            e = (nth >= 1 && nth <= e->GetNumChildren()) ? e->GetChild(nth - 1) : nullptr;
        }
        /* `KILN_UI_SYNTH_EVENT` names what is dispatched, for the events a
         * click cannot stand in for — a grid's double-click. */
        const char* synth_event = std::getenv("KILN_UI_SYNTH_EVENT");
        if (e)
            e->DispatchEvent(synth_event ? synth_event : "click", Rml::Dictionary());
        else
            std::fprintf(stderr, "kiln-ui: no widget handle %s to click\n", synth_click);
    }

    /* A shortcut cannot be verified by looking at a frame, so there is a hook
     * to press one — dispatched as a real keydown, through the same listener a
     * keyboard reaches. */
    if (const char* key = std::getenv("KILN_UI_SYNTH_KEY")) {
        std::string spec;
        for (const char* c = key; *c; c++) spec += (char)tolower((unsigned char)*c);
        Rml::Dictionary p;
        std::string name;
        for (size_t i = 0; i <= spec.size(); i++) {
            if (i < spec.size() && spec[i] != '+') { name += spec[i]; continue; }
            if (name == "ctrl" || name == "control") p["ctrl_key"] = Rml::Variant(1);
            else if (name == "shift")                p["shift_key"] = Rml::Variant(1);
            else if (name == "alt")                  p["alt_key"] = Rml::Variant(1);
            else if (!name.empty())                  p["key_identifier"] = Rml::Variant(key_for_name(name));
            name.clear();
        }
        g.document->DispatchEvent("keydown", p);
    }

    if (mouse_at) {
        int mx = 0, my = 0;
        if (std::sscanf(mouse_at, "%d,%d", &mx, &my) == 2)
            g.context->ProcessMouseMove(mx, my, 0);
    }

    kiln::a11y::bridge_init();
    {
        /* Report the window's screen position so ATs can map node bounds to
         * screen coordinates. Returns 0,0 under Wayland, where a client cannot
         * know its own position — accessible bounds stay window-relative there. */
        int wx = 0, wy = 0, ww = 0, wh = 0;
        /* The backend owns the SDL window privately; recover it from the
         * current GL context rather than reaching into backend internals. */
        if (SDL_Window* win = SDL_GL_GetCurrentWindow()) {
            SDL_GetWindowPosition(win, &wx, &wy);
            SDL_GetWindowSize(win, &ww, &wh);
        }
        kiln::a11y::bridge_set_window_bounds((float)wx, (float)wy, (float)ww, (float)wh);
    }

    /* `load` before the first frame, so a handler that fills a list or sets a
     * caption is never seen half-done — and after the window exists, so one
     * that reads the form's size gets the real one. */
    if (g_on_load) {
        Kiln_EventFn fn = g_on_load;
        g_on_load = nullptr;   /* once per run, even if the loop is re-entered */
        fn();
    }

    g_frames = 0;
    if (!kn_loop_add(ui_pump, nullptr, 0)) return 1;
    return kn_loop_run();
}

void kn_ui_shutdown(void) {
    if (!g.initialised) return;
    kiln::a11y::bridge_shutdown();
    kiln::a11y::clear();
    Rml::Shutdown();
    Backend::Shutdown();
    for (auto* b : g_bridges) delete b;
    g_bridges.clear();
    for (auto* s : g_stylers) delete s;
    g_stylers.clear();
    for (auto* a : g_action_bridges) delete a;
    g_action_bridges.clear();
    for (auto* c : g_item_clicks) delete c;
    g_item_clicks.clear();
    for (auto* st : g_spin_steps) delete st;
    g_spin_steps.clear();
    for (auto& kv : g_grids) {
        for (auto* r : kv.second.rows) delete r;
    }
    g_grids.clear();
    g_datasources.clear();
    g_wanted_selection.clear();
    g_ranges.clear();
    g_spins.clear();
    g_actions.clear();
    g_action_of.clear();
    g_pending_bindings.clear();
    g_disabled.clear();
    g_placement.clear();
    g_window = WindowPlace{};
    g_running = false;
    g_form_width = g_form_height = 0;
    g_on_load = nullptr;
    g_applied_dw = g_applied_dh = 0;
    g.interactive.clear();
    g.widgets.clear();
    g.type_of.clear();
    g.parent_of.clear();
    g.initialised = false;
}

} // extern "C"
