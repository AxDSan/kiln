/* The Settings dialog's markup and styles.
 *
 * A separate `LoadDocumentFromMemory` document, the way About is, because the
 * IDE's own chrome is a fixed five-region layout that `set_view` toggles two
 * ways — a third view would mean teaching the tab strip, `relayout` and the
 * frame loop about a screen that has no project in it.
 *
 * Three things here are not arbitrary:
 *
 * **It is built from `settings::schema()`.** Categories, rows, controls, hints
 * and defaults all come from that one table, so a new setting is a row there
 * and the code that reads it — never a change in two places that can disagree.
 *
 * **The scrolling pane carries a full scrollbar block.** RmlUi ships no default
 * size for a scrollbar, and an unsized one is laid out over the whole of its
 * owner and swallows every click meant for the content underneath. That has
 * broken Studio twice — the code editor could not be focused, and the welcome
 * screen's file list opened nothing — so the rules are here from the start.
 *
 * **Nothing inside the scrolling pane is absolutely positioned.** RmlUi does
 * not clip a positioned element against an ancestor's overflow, so a row laid
 * out that way would paint straight over the rail and the title as it scrolled
 * — the same fault the code editor's highlight layer works around by redrawing
 * rather than shifting.
 *
 * There is no OK / Apply / Cancel. Every control applies when you use it and
 * the file is written straight away, which is what VS Code and Qt Creator both
 * settled on; Visual Studio's whole-dialog commit is documented as a trap
 * ("Cancel on any page cancels all change requests, including any just made on
 * other pages"). A row that cannot take effect until relaunch says so on the
 * row instead of pretending.
 */
#ifndef KILN_DESIGNER_SETTINGS_PAGE_H
#define KILN_DESIGNER_SETTINGS_PAGE_H

#include <sstream>
#include <string>

#include "settings.h"
#include "theme.h"

namespace kiln::designer::settings_page {

inline std::string esc_attr(const std::string& v) {
    std::string out;
    for (char c : v) {
        if (c == '&') out += "&amp;";
        else if (c == '<') out += "&lt;";
        else if (c == '>') out += "&gt;";
        else if (c == '\'') out += "&apos;";
        else if (c == '"') out += "&quot;";
        else out += c;
    }
    return out;
}

constexpr int DLG_W = 640;
constexpr int DLG_H = 470;
constexpr int RAIL_W = 150;

/// The whole document, for a window `w` x `h`, showing `category`.
inline std::string markup(const std::string& family, int w, int h, const std::string& category) {
    using namespace kiln::designer::theme;
    namespace st = kiln::settings;

    const int left = (w - DLG_W) / 2 > 0 ? (w - DLG_W) / 2 : 0;
    const int top = (h - DLG_H) / 2 > 0 ? (h - DLG_H) / 2 : 0;
    const int rows_w = DLG_W - RAIL_W;

    std::ostringstream s;
    s << "<rml><head><style>";
    s << "div{display:block}span{display:inline}";
    // The body is the click-outside target, so it must be the window's size:
    // an unsized body is 0x0 and no click ever lands on it.
    s << "body{position:absolute;left:0;top:0;width:" << w << "px;height:" << h << "px;font-family:'"
      << family << "';font-size:12px;color:" << TEXT << ";background-color:#0000001c}";
    s << "#dlg{position:absolute;left:" << left << "px;top:" << top << "px;width:" << DLG_W
      << "px;height:" << DLG_H << "px;background-color:" << PANEL << ";border:1px " << BORDER
      << ";border-radius:12px;box-shadow:#00000038 0 14px 40px 0px}";

    s << "#head{position:absolute;left:0;top:0;width:" << DLG_W
      << "px;height:42px;border-bottom:1px " << BORDER_SOFT << "}";
    s << "#title{position:absolute;left:18px;top:13px;font-size:13px;font-weight:bold;color:"
      << TEXT << "}";
    s << "#x{position:absolute;right:12px;top:9px;width:24px;height:24px;text-align:center;"
         "padding-top:3px;font-size:13px;color:" << TEXT_MUTED << ";border-radius:4px;cursor:pointer}";
    s << "#x:hover{background-color:" << CHROME << ";color:" << TEXT << "}";

    // The category rail. A flat list, not a tree: seven categories do not earn
    // one, and Qt Creator's flat rail is the shape a page this size wants.
    s << "#rail{position:absolute;left:0;top:42px;width:" << RAIL_W << "px;height:" << (DLG_H - 42 - 44)
      << "px;background-color:" << CHROME_ALT << ";border-right:1px " << BORDER_SOFT
      << ";padding:8px 0 0 0;overflow-y:auto}";
    s << "#rail .cat{padding:7px 10px 7px 16px;font-size:12px;color:" << TEXT
      << ";cursor:pointer}";
    s << "#rail .cat:hover{background-color:" << CHROME << "}";
    s << "#rail .cat.on{background-color:" << ACCENT << ";color:" << ACCENT_TEXT
      << ";font-weight:bold}";

    s << "#rows{position:absolute;left:" << RAIL_W << "px;top:42px;width:" << rows_w
      << "px;height:" << (DLG_H - 42 - 44) << "px;padding:12px 16px 12px 16px;overflow-y:auto}";

    // Rows are in normal flow. Nothing inside #rows may be absolutely
    // positioned: RmlUi would not clip it against #rows' overflow, and it
    // would paint over the rail and the header as the pane scrolled.
    s << ".row{margin-bottom:14px}";
    s << ".row .lbl{font-size:12px;font-weight:bold;color:" << TEXT << ";margin-bottom:4px}";
    s << ".row .lbl .dot{color:" << ACCENT << ";margin-left:6px;font-size:14px}";
    s << ".row .lbl .rst{color:" << ACCENT
      << ";margin-left:8px;font-size:11px;font-weight:normal;cursor:pointer}";
    s << ".row .lbl .rst:hover{text-decoration:underline}";
    s << ".row .hint{font-size:11px;color:" << TEXT_MUTED << ";margin-top:4px;line-height:1.35}";
    s << ".row .warn{font-size:11px;color:#bf8700;margin-top:4px}";

    // A choice, and a bool, are both chips. Deliberately not `<select>` or a
    // checkbox: neither appears anywhere else in Studio, and RmlUi's drop-down
    // brings a third unstyled scrollbar with it.
    s << ".chip{display:inline-block;height:24px;padding:4px 12px 0 12px;margin-right:6px;"
         "border:1px " << BORDER << ";border-radius:12px;background-color:" << PANEL
      << ";color:" << TEXT << ";font-size:11px;cursor:pointer}";
    s << ".chip:hover{border:1px " << ACCENT << ";color:" << ACCENT << "}";
    s << ".chip.on{background-color:" << ACCENT << ";border:1px " << ACCENT << ";color:"
      << ACCENT_TEXT << ";font-weight:bold}";

    s << "input{display:block;width:" << (rows_w - 32)
      << "px;height:26px;border:1px " << BORDER << ";border-radius:4px;background-color:" << PANEL
      << ";color:" << TEXT << ";padding-left:7px;font-size:12px}";
    s << "input:focus{border:1px " << ACCENT << "}";
    s << "input.narrow{width:110px}";

    s << "#foot{position:absolute;left:0;top:" << (DLG_H - 44) << "px;width:" << DLG_W
      << "px;height:44px;border-top:1px " << BORDER_SOFT << "}";
    s << "#file{position:absolute;left:18px;top:15px;font-size:11px;color:" << TEXT_MUTED << "}";
    s << "#openfile{color:" << ACCENT << ";cursor:pointer}";
    s << "#openfile:hover{text-decoration:underline}";
    s << "#done{position:absolute;right:16px;top:9px;width:78px;height:27px;padding-top:6px;"
         "text-align:center;font-size:12px;font-weight:bold;background-color:" << ACCENT
      << ";color:" << ACCENT_TEXT << ";border-radius:5px;cursor:pointer}";

    // Sized, because RmlUi gives a scrollbar no default size and an unsized
    // one covers its owner and eats every click. This has broken Studio twice.
    s << "scrollbarvertical{width:10px}";
    s << "scrollbarhorizontal{height:10px}";
    s << "slidertrack{background-color:" << CHROME_ALT << "}";
    s << "sliderbar{width:10px;min-height:24px;border-radius:5px;background-color:" << BORDER << "}";
    s << "sliderbar:hover{background-color:" << TEXT_MUTED << "}";
    s << "sliderarrowdec,sliderarrowinc{width:0px;height:0px}";

    s << "</style></head><body>";
    s << "<div id='dlg'>";
    s << "<div id='head'><div id='title'>Settings</div><div id='x' oe-set-close='1'>&#10005;</div></div>";

    s << "<div id='rail'>";
    for (const auto& c : st::categories()) {
        s << "<div class='cat" << (c == category ? " on" : "") << "' oe-set-cat='" << esc_attr(c)
          << "'>" << c << "</div>";
    }
    s << "</div>";

    s << "<div id='rows'>";
    for (const auto& r : st::schema()) {
        if (r.hidden || r.category != category) continue;
        const std::string value = st::text(r.key);
        s << "<div class='row'>";
        s << "<div class='lbl'>" << r.label;
        if (st::modified(r.key)) {
            s << "<span class='dot'>&#8226;</span><span class='rst' oe-set-reset='"
              << esc_attr(r.key) << "'>Reset</span>";
        }
        s << "</div>";

        switch (r.kind) {
        case st::Kind::Bool:
            for (const char* v : {"true", "false"}) {
                s << "<span class='chip" << (value == v ? " on" : "") << "' oe-set-key='"
                  << esc_attr(r.key) << "' oe-set-val='" << v << "'>"
                  << (std::string(v) == "true" ? "On" : "Off") << "</span>";
            }
            break;
        case st::Kind::Choice:
            for (const char* v : r.choices) {
                s << "<span class='chip" << (value == v ? " on" : "") << "' oe-set-key='"
                  << esc_attr(r.key) << "' oe-set-val='" << esc_attr(v) << "'>" << v << "</span>";
            }
            break;
        case st::Kind::Int:
            // Committed when the field loses focus or takes Enter, never on
            // `change`: that fires per character, and a half-typed grid size
            // of "" reaches an integer division.
            s << "<input type='text' class='narrow' oe-set-field='" << esc_attr(r.key)
              << "' value='" << esc_attr(value) << "'/>";
            break;
        case st::Kind::Text:
        case st::Kind::Path:
            s << "<input type='text' oe-set-field='" << esc_attr(r.key) << "' value='"
              << esc_attr(value) << "'/>";
            break;
        }

        if (r.kind == st::Kind::Int) {
            s << "<div class='hint'>" << r.min << " to " << r.max
              << (r.hint[0] ? std::string(". ") + r.hint : std::string("")) << "</div>";
        } else if (r.hint[0]) {
            s << "<div class='hint'>" << r.hint << "</div>";
        }
        if (r.restart) s << "<div class='warn'>Takes effect when Studio restarts.</div>";
        s << "</div>";
    }
    s << "</div>";

    s << "<div id='foot'><div id='file'>Saved to <span id='openfile'>"
      << esc_attr(st::path()) << "</span></div>"
         "<div id='done' oe-set-close='1'>Done</div></div>";
    s << "</div></body></rml>";
    return s.str();
}

} // namespace kiln::designer::settings_page

#endif
