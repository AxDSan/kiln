/* Kiln Studio design tokens.
 *
 * From the Kiln Studio design specification. Kept in one place so the chrome
 * never hard-codes a colour: changing the palette here changes the whole IDE —
 * which is what the theme setting does.
 *
 * The colours are mutable `std::string`s rather than `constexpr const char*`
 * because `set_palette()` overwrites them at run time. Every existing spelling
 * still compiles — `s << ACCENT`, `s += CANVAS`, `std::string(BORDER)`,
 * `SetProperty("border-color", SELECT)` — because `Rml::String` *is*
 * `std::string`; only code assigning one to a `const char*` has to change.
 *
 * The metrics stay `constexpr`. They are laid out into a stylesheet built once,
 * and a size that changed under an already-laid-out document would move every
 * box without telling the layout.
 */
#ifndef KILN_DESIGNER_THEME_H
#define KILN_DESIGNER_THEME_H

#include <string>

namespace kiln::designer::theme {

// Primary / accent
/// Padding inside the code editor. Named because relayout() must subtract it
/// from the editor's box: RmlUi sizes content, not border, boxes.
/// Line box for the code editor. Both editor layers must use it: the
/// highlight layer lines up with the text only if their line heights match.
///
/// Not `constexpr`, because the editor's font size is a setting. It is derived
/// from that size once at startup — before the stylesheet is built — and never
/// changes again, which is why the setting says it needs a restart. The scroll
/// arithmetic in `refresh_highlight` divides by it, so it must never be 0.
inline int CODE_LINE_H = 18;
inline int CODE_FONT_PX = 13;
/// The editor's text size, and the line box it implies. The five pixels of
/// leading are what the original 13/18 pair had; keeping the ratio means a
/// larger font stays as readable rather than getting cramped.
inline void set_code_font_size(int px) {
    CODE_FONT_PX = px < 8 ? 8 : (px > 32 ? 32 : px);
    CODE_LINE_H = CODE_FONT_PX + 5;
}
/// The output pane stacks a PROBLEMS strip over the build log; both need fixed
/// heights so the log can scroll instead of growing to fit its content.
constexpr int PANEHEAD_H = 26;
constexpr int PROBLEMS_H = 72;
/// Line box in the output console, so the tail calculation is exact.
constexpr int LOG_LINE_H = 17;
constexpr int CODE_PAD_X = 10;
/// The gutter's width: a column for the breakpoint mark, then the line number
/// right-aligned against the code. Four digits fit, which is more lines than
/// a module has any business having.
constexpr int CODE_GUTTER_W = 62;
/// The mark's own column, at the gutter's left edge — the same place every
/// editor puts it, and far enough from the text that a click meant for the
/// gutter is not a click meant for the code.
constexpr int CODE_MARK_W = 13;
/// The execution cue's column, beside the mark rather than sharing it: a
/// breakpoint and the line the program is stopped on are two different facts
/// and are usually true of the same line at once.
constexpr int CODE_CUE_W = 13;
constexpr int CODE_PAD_Y = 8;

inline std::string ACCENT      = "#c2410c";
inline std::string ACCENT_TEXT = "#ffffff";
/// The selection on the canvas: its box, its anchors, its wiring badge and
/// the frame of a selected form. GitHub's link blue, not the IDE's accent.
inline std::string SELECT      = "#0969da";

// Backgrounds
inline std::string PANEL       = "#ffffff";
inline std::string CHROME      = "#f3f3f3";
inline std::string CHROME_ALT  = "#f8f9fa";
inline std::string CANVAS      = "#fafafa";
/// The designer's workspace behind the form preview: the dot grid's ground.
inline std::string CANVAS_GRID = "#f8f9fa";

// Text
inline std::string TEXT        = "#1f2328";
inline std::string TEXT_MUTED  = "#656d76";
inline std::string BORDER      = "#d0d7de";
inline std::string BORDER_SOFT = "#e1e4e8";

// Semantic
inline std::string SUCCESS     = "#1a7f37";
inline std::string DANGER      = "#cf222e";

// Syntax highlighting
inline std::string HOVER        = "#eef2f8";
/// The menu bar and toolbar's hover, a shade stronger than a list row's.
inline std::string HOVER_STRONG = "#e8eaed";
/// A selected row's tint, and the editor's selection band.
inline std::string TINT         = "#e8f0fe";
inline std::string SELECTION    = "#cfe3ff";
/// A scrollbar's thumb, and a hairline weaker than BORDER.
inline std::string SCROLL_THUMB = "#c9d1d9";
inline std::string HAIRLINE     = "#e5e7eb";
/// Text and icons for a control that exists but does not work yet.
inline std::string DISABLED     = "#aeb6c2";
inline std::string DISABLED_ICO = "#c9d0da";
/// The code editor's line numbers, and an unset property's note.
inline std::string GUTTER       = "#9aa3b0";
/// The line the debugger is stopped on, and the dot marking a breakpoint.
inline std::string STOPLINE     = "#fff3c4";
inline std::string BREAKPOINT   = "#d1242f";
/// The line a diagnostic is on.
inline std::string BADLINE      = "#fff6f6";
inline std::string SYN_KEYWORD = "#6f42c1";
inline std::string SYN_METHOD  = "#0550ae";
inline std::string SYN_STRING  = "#9a6700";
inline std::string SYN_IDENT   = "#0e7490";
inline std::string SYN_COMMENT = "#656d76";
inline std::string SYN_NUMBER  = "#0550ae";

// Metrics
constexpr int TOOLBOX_W  = 220;
constexpr int INSPECT_W  = 280;
constexpr int TITLEBAR_H = 32;
constexpr int MENUBAR_H  = 28;
constexpr int TOOLBAR_H  = 40;
constexpr int TABBAR_H   = 32;
constexpr int STATUS_H   = 24;
constexpr int BOTTOM_H   = 220;
/// The non-visual component tray, below the form preview. Delphi put one here
/// for the same reason: a timer has properties to edit and no rectangle to
/// click, so it needs somewhere to be.
constexpr int TRAY_H     = 104;
/// The form preview's own title bar: decoration above the client area, which
/// is the only thing the file's coordinates describe.
constexpr int FORM_TITLE_H = 36;
/// The selection box's distance outside the component it frames, the size
/// of a resize anchor, and the wiring badge: its height and how far above
/// the selection box it floats.
constexpr int SEL_GAP    = 3;
constexpr int HANDLE_PX  = 6;
constexpr int BADGE_H    = 20;
constexpr int BADGE_GAP  = 18;
/// The wiring box pinned to the foot of the inspector, so it is on screen
/// however long the property list above it grows.
constexpr int WIRE_H     = 84;

/// Swap the whole palette. The chrome is built from these values once, so the
/// caller must regenerate the stylesheet afterwards — changing the strings
/// alone moves nothing already on screen.
///
/// The dark values are GitHub's dark default, for the same reason the light
/// ones are its light default: they are a matched pair, contrasted together
/// rather than a light palette with its lightness inverted.
inline void set_palette(bool dark) {
    // Kiln heat, at weights that carry text on their own ground: #c2410c is
    // 5.18:1 on white, #fb923c is 8.36:1 on the dark panel. The brand's own
    // amber (#D97706) is a display colour and fails on white at 3.19:1.
    ACCENT = dark ? "#fb923c" : "#c2410c";
    ACCENT_TEXT = dark ? "#0d1117" : "#ffffff";
    SELECT = dark ? "#58a6ff" : "#0969da";
    PANEL = dark ? "#0d1117" : "#ffffff";
    CHROME = dark ? "#161b22" : "#f3f3f3";
    CHROME_ALT = dark ? "#1c2128" : "#f8f9fa";
    CANVAS = dark ? "#010409" : "#fafafa";
    CANVAS_GRID = dark ? "#161b22" : "#f8f9fa";
    TEXT = dark ? "#e6edf3" : "#1f2328";
    TEXT_MUTED = dark ? "#8b949e" : "#656d76";
    BORDER = dark ? "#30363d" : "#d0d7de";
    BORDER_SOFT = dark ? "#21262d" : "#e1e4e8";
    SUCCESS = dark ? "#3fb950" : "#1a7f37";
    DANGER = dark ? "#f85149" : "#cf222e";
    HOVER = dark ? "#1c2128" : "#eef2f8";
    HOVER_STRONG = dark ? "#21262d" : "#e8eaed";
    TINT = dark ? "#193050" : "#e8f0fe";
    SELECTION = dark ? "#264f78" : "#cfe3ff";
    SCROLL_THUMB = dark ? "#30363d" : "#c9d1d9";
    HAIRLINE = dark ? "#21262d" : "#e5e7eb";
    DISABLED = dark ? "#484f58" : "#aeb6c2";
    DISABLED_ICO = dark ? "#3d444d" : "#c9d0da";
    GUTTER = dark ? "#6e7681" : "#9aa3b0";
    STOPLINE = dark ? "#3a3316" : "#fff3c4";
    BREAKPOINT = dark ? "#f85149" : "#d1242f";
    BADLINE = dark ? "#2d1618" : "#fff6f6";
    SYN_KEYWORD = dark ? "#d2a8ff" : "#6f42c1";
    SYN_METHOD = dark ? "#79c0ff" : "#0550ae";
    SYN_STRING = dark ? "#ffa657" : "#9a6700";
    SYN_IDENT = dark ? "#7ee787" : "#0e7490";
    SYN_COMMENT = dark ? "#8b949e" : "#656d76";
    SYN_NUMBER = dark ? "#79c0ff" : "#0550ae";
}

/// Is the dark palette in force? Derived from a colour rather than kept in a
/// flag beside it, so the two can never disagree.
inline bool dark_palette() { return PANEL != "#ffffff"; }

} // namespace kiln::designer::theme
#endif
