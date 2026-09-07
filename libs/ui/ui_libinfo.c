/* "ui" support library metadata — visual component descriptors (D9/D11).
 *
 * DESIGN-TIME ONLY: compiled into the introspection .so, never into a shipped
 * program (same split as core_libinfo.c. The compiler reads this
 * to learn which component types exist, what properties and events they have,
 * and their accessibility roles (D16).
 */
#include "kiln_abi.h"

/* --- form ------------------------------------------------------------- */
static const Kiln_PropertyDesc FORM_PROPS[] = {
    { "title",            KN_SDT_TEXT, "Kiln Application", NULL },
    { "width",            KN_SDT_INT,  "800",                 NULL },
    { "height",           KN_SDT_INT,  "600",                 NULL },
    /* A window's ground, not a terminal's. Every desktop the target audience
     * has used draws a form in light grey; a dark default reads as a theme
     * someone has to switch off before their first app looks normal. */
    { "background_color", KN_SDT_TEXT, "#f3f3f3",             "color" },
    /* The window's icon: a PNG beside the source, embedded at build time like
     * an image's source, so the shipped binary carries it. */
    { "icon",             KN_SDT_TEXT, "",                    "file" },
    /* Where the window opens. `default` leaves it to the window manager (the
     * substrate centres it today); `center` asks for the middle of the
     * screen; `manual` puts its top-left corner at `left`,`top`. A later
     * assignment of `left`/`top` from a subroutine moves the window when the
     * form is `manual`; assigning `position` after the window exists is
     * ignored, because a window that jumps between modes mid-run is a bug a
     * program cannot mean. */
    { "position",         KN_SDT_TEXT, "default",             NULL },
    { "left",             KN_SDT_INT,  "0",                   NULL },
    { "top",              KN_SDT_INT,  "0",                   NULL },
};
static const Kiln_EventDesc FORM_EVENTS[] = { { "load", 0, NULL } };

/* Delphi's `Anchors`, on every control that has a rectangle: which edges of
 * the window it keeps its distance from when the window is resized. The far
 * edge alone moves the control with the window, both edges stretch it, and
 * the default keeps it where the form put it — so a form written without a
 * thought for resizing behaves exactly as it always has. The layout rule is
 * `anchored_rect` in ui_mapping.h, shared with the designer. */
#define ANCHORS { "anchors", KN_SDT_TEXT, "left,top", "anchors" }

/* --- button ----------------------------------------------------------- */
static const Kiln_PropertyDesc BUTTON_PROPS[] = {
    { "text",             KN_SDT_TEXT, "Button",  NULL },
    { "left",             KN_SDT_INT,  "0",       NULL },
    { "top",              KN_SDT_INT,  "0",       NULL },
    { "width",            KN_SDT_INT,  "120",     NULL },
    /* 32px is the specification's control height, and the whole palette is
     * sized to that grid: an editbox, a combobox and a button drawn on one
     * row line up without anyone reaching for the inspector. */
    { "height",           KN_SDT_INT,  "32",      NULL },
    ANCHORS,
    /* NO default colour, deliberately. The stylesheet's neutral button —
     * white ground, hairline outline, and the hover and pressed shades that
     * go with them — is what a button gets when it declares none, and a
     * default written here would be written into every form the designer
     * saves and would replace it. A form that DOES name a colour still gets
     * exactly that colour, and the backend's own hover shades with it: the
     * language has no `primary`/`accent` property yet, and inventing one
     * silently in a stylesheet would be worse than the omission. */
    { "background_color", KN_SDT_TEXT, NULL,      "color" },
    { "color",            KN_SDT_TEXT, NULL,      "color" },
    { "border_radius",    KN_SDT_INT,  "4",       NULL },
    { "enabled",          KN_SDT_BOOL, "true",    NULL },
    { "action",           KN_SDT_TEXT, "",        NULL },
};
static const Kiln_EventDesc BUTTON_EVENTS[] = { { "click", 0, NULL } };

/* --- label ------------------------------------------------------------ */
static const Kiln_PropertyDesc LABEL_PROPS[] = {
    { "text",  KN_SDT_TEXT, "Label",   NULL },
    { "left",  KN_SDT_INT,  "0",       NULL },
    { "top",   KN_SDT_INT,  "0",       NULL },
    { "width", KN_SDT_INT,  "200",     NULL },
    /* A label the designer can size only sideways is a label the designer
     * writes a `height` into anyway, and the build then rejects. */
    { "height", KN_SDT_INT, "24",      NULL },
    ANCHORS,
    { "color", KN_SDT_TEXT, "#1a1a1a", "color" },
};

/* --- editbox ---------------------------------------------------------- */
static const Kiln_PropertyDesc EDIT_PROPS[] = {
    { "text",      KN_SDT_TEXT, "",        NULL },
    { "left",      KN_SDT_INT,  "0",       NULL },
    { "top",       KN_SDT_INT,  "0",       NULL },
    { "width",     KN_SDT_INT,  "160",     NULL },
    { "height",    KN_SDT_INT,  "32",      NULL },
    ANCHORS,
    { "color",     KN_SDT_TEXT, "#1a1a1a", "color" },
    /* Off by default: a single-line field. On, the box becomes a multi-line
     * text area whose height is honoured — and the designer lets it be resized
     * vertically only then, clamping a single-line field to one row. */
    { "multiline", KN_SDT_BOOL, "false",   NULL },
};
static const Kiln_EventDesc EDIT_EVENTS[] = { { "change", 0, NULL } };

/* --- checkbox --------------------------------------------------------- */
static const Kiln_PropertyDesc CHECK_PROPS[] = {
    { "text",    KN_SDT_TEXT, "Check me", NULL },
    { "checked", KN_SDT_BOOL, "false",    NULL },
    { "left",    KN_SDT_INT,  "0",        NULL },
    { "top",     KN_SDT_INT,  "0",        NULL },
    { "width",   KN_SDT_INT,  "140",      NULL },
    { "height",  KN_SDT_INT,  "24",       NULL },
    ANCHORS,
    { "color",   KN_SDT_TEXT, "#1a1a1a",  "color" },
};
static const Kiln_EventDesc CHECK_EVENTS[] = { { "change", 0, NULL } };

/* --- groupbox --------------------------------------------------------- */
static const Kiln_PropertyDesc GROUP_PROPS[] = {
    { "text",         KN_SDT_TEXT, "Group",   NULL },
    { "left",         KN_SDT_INT,  "0",       NULL },
    { "top",          KN_SDT_INT,  "0",       NULL },
    { "width",        KN_SDT_INT,  "200",     NULL },
    { "height",       KN_SDT_INT,  "120",     NULL },
    ANCHORS,
    { "border_color", KN_SDT_TEXT, "#e5e5e5", "color" },
};

/* --- image ------------------------------------------------------------ */
static const Kiln_PropertyDesc IMAGE_PROPS[] = {
    { "source", KN_SDT_TEXT, "",    "file" },
    { "left",   KN_SDT_INT,  "0",   NULL },
    { "top",    KN_SDT_INT,  "0",   NULL },
    { "width",  KN_SDT_INT,  "120", NULL },
    { "height", KN_SDT_INT,  "120", NULL },
    ANCHORS,
};

/* --- progressbar ------------------------------------------------------ */
static const Kiln_PropertyDesc PROG_PROPS[] = {
    { "value",  KN_SDT_INT, "50",  NULL },
    { "left",   KN_SDT_INT, "0",   NULL },
    { "top",    KN_SDT_INT, "0",   NULL },
    { "width",  KN_SDT_INT, "200", NULL },
    { "height", KN_SDT_INT, "16",  NULL },
    ANCHORS,
};


/* --- combobox / listbox ----------------------------------------------- *
 *
 * `items` is ONE text with a newline between entries, not a `text[]`.
 *
 * That is forced, not preferred. A property value is a literal at the D10
 * boundary (backend/src/lib.rs `property_text`), so an aggregate cannot be
 * written in a form; and there is no expression form for a bare component id,
 * so the `thing_count`/`thing_at` command pair libs/README.md reaches for —
 * `combobox_add(list, "Red")` — has nothing to name the list with. A delimited
 * text is the only shape that both a designer inspector and a running
 * subroutine can write today: `list.items = concat(list.items, "\nPurple")`.
 *
 * `selected` counts from 1 and answers 0 for nothing selected, like every
 * other position in the language. `count` is read-only.
 */
static const Kiln_PropertyDesc COMBO_PROPS[] = {
    { "items",    KN_SDT_TEXT, "",    "multiline" },
    { "selected", KN_SDT_INT,  "0",   NULL },
    { "count",    KN_SDT_INT,  "0",   NULL },
    { "left",     KN_SDT_INT,  "0",   NULL },
    { "top",      KN_SDT_INT,  "0",   NULL },
    { "width",    KN_SDT_INT,  "160", NULL },
    { "height",   KN_SDT_INT,  "32",  NULL },
    ANCHORS,
    { "enabled",  KN_SDT_BOOL, "true", NULL },
};
/* `change`, not `changed`: the palette already spells this event `change` on
 * editbox and checkbox, and one vocabulary for one concept is worth more than
 * matching the word a request happened to use. */
static const Kiln_EventDesc COMBO_EVENTS[] = { { "change", 0, NULL } };

static const Kiln_PropertyDesc LIST_PROPS[] = {
    { "items",    KN_SDT_TEXT, "",    "multiline" },
    { "selected", KN_SDT_INT,  "0",   NULL },
    { "count",    KN_SDT_INT,  "0",   NULL },
    { "left",     KN_SDT_INT,  "0",   NULL },
    { "top",      KN_SDT_INT,  "0",   NULL },
    { "width",    KN_SDT_INT,  "160", NULL },
    { "height",   KN_SDT_INT,  "120", NULL },
    ANCHORS,
    { "enabled",  KN_SDT_BOOL, "true", NULL },
};
static const Kiln_EventDesc LIST_EVENTS[] = { { "change", 0, NULL } };

/* --- radiobutton ------------------------------------------------------ *
 *
 * Exclusion is by `group` NAME rather than by containment, because the
 * component tree is flat: a form holds children, and a groupbox holds nothing
 * (kiln_ir::Form). Naming the group is the same answer `action` gives to
 * the same problem, and it survives the designer moving a button out of the
 * rectangle it happened to be drawn over.
 */
static const Kiln_PropertyDesc RADIO_PROPS[] = {
    { "text",    KN_SDT_TEXT, "Option",  NULL },
    { "group",   KN_SDT_TEXT, "default", NULL },
    { "checked", KN_SDT_BOOL, "false",   NULL },
    { "left",    KN_SDT_INT,  "0",       NULL },
    { "top",     KN_SDT_INT,  "0",       NULL },
    { "width",   KN_SDT_INT,  "140",     NULL },
    { "height",  KN_SDT_INT,  "24",      NULL },
    ANCHORS,
    { "color",   KN_SDT_TEXT, "#1a1a1a", "color" },
};
static const Kiln_EventDesc RADIO_EVENTS[] = { { "change", 0, NULL } };

/* --- memo ------------------------------------------------------------- *
 *
 * The `multiline` editor hint has been in ABI v2 with nothing consuming it;
 * this is the component it was written for. An inspector offering a one-line
 * field for a paragraph is the whole reason the hint exists.
 */
static const Kiln_PropertyDesc MEMO_PROPS[] = {
    { "text",   KN_SDT_TEXT, "",       "multiline" },
    { "left",   KN_SDT_INT,  "0",      NULL },
    { "top",    KN_SDT_INT,  "0",      NULL },
    { "width",  KN_SDT_INT,  "240",    NULL },
    { "height", KN_SDT_INT,  "100",    NULL },
    ANCHORS,
    { "color",  KN_SDT_TEXT, "#1a1a1a", "color" },
    { "enabled", KN_SDT_BOOL, "true",  NULL },
};
static const Kiln_EventDesc MEMO_EVENTS[] = { { "change", 0, NULL } };

/* --- slider ----------------------------------------------------------- *
 *
 * `min`/`max` are the range and `value` is where the handle sits. Unlike a
 * progressbar this reports back, so it carries `change`.
 */
static const Kiln_PropertyDesc SLIDER_PROPS[] = {
    { "value",   KN_SDT_INT,  "50",   NULL },
    { "min",     KN_SDT_INT,  "0",    NULL },
    { "max",     KN_SDT_INT,  "100",  NULL },
    { "left",    KN_SDT_INT,  "0",    NULL },
    { "top",     KN_SDT_INT,  "0",    NULL },
    { "width",   KN_SDT_INT,  "200",  NULL },
    { "height",  KN_SDT_INT,  "20",   NULL },
    ANCHORS,
    { "enabled", KN_SDT_BOOL, "true", NULL },
};
static const Kiln_EventDesc SLIDER_EVENTS[] = { { "change", 0, NULL } };

/* --- spinner ---------------------------------------------------------- *
 *
 * A number with the two buttons that step it. `step` is what one press moves,
 * and the value is clamped to `min`..`max` however it was reached — typed,
 * stepped, or assigned from a subroutine — because a spinner whose bounds hold
 * only for the arrows is not bounded.
 */
static const Kiln_PropertyDesc SPIN_PROPS[] = {
    { "value",   KN_SDT_INT,  "0",    NULL },
    { "min",     KN_SDT_INT,  "0",    NULL },
    { "max",     KN_SDT_INT,  "100",  NULL },
    { "step",    KN_SDT_INT,  "1",    NULL },
    { "left",    KN_SDT_INT,  "0",    NULL },
    { "top",     KN_SDT_INT,  "0",    NULL },
    { "width",   KN_SDT_INT,  "110",  NULL },
    { "height",  KN_SDT_INT,  "32",   NULL },
    ANCHORS,
    { "enabled", KN_SDT_BOOL, "true", NULL },
};
static const Kiln_EventDesc SPIN_EVENTS[] = { { "change", 0, NULL } };

/* --- action ----------------------------------------------------------- *
 *
 * The one thing in Delphi this language had no answer for: the text, the
 * enabled state and the code behind a command live in ONE place, and every
 * control that offers that command follows it.  A button points at an action
 * through its `action` property; disabling the action greys the button.
 *
 * The reference is by `name` rather than by the component's identifier
 * because a property value is a literal (backend/src/lib.rs) and component
 * identifiers deliberately never reach the binary.
 */
static const Kiln_PropertyDesc ACTION_PROPS[] = {
    { "name",     KN_SDT_TEXT, "",        NULL },
    { "text",     KN_SDT_TEXT, "",        NULL },
    { "shortcut", KN_SDT_TEXT, "",        NULL },
    { "enabled",  KN_SDT_BOOL, "true",    NULL },
};
static const Kiln_EventDesc ACTION_EVENTS[] = { { "execute", 0, NULL } };

/* --- grid / datasource ------------------------------------------------ *
 *
 * A grid's data is the property that most wants to be a `text[][]`, and it
 * cannot be one for the two reasons `items` above cannot: a property value is
 * a literal, and a bare component id is not an expression.  So `rows` is ONE
 * text — a newline between rows, a tab between cells — and `columns` is the
 * header, tab-separated.  A cell can hold neither character, and there is no
 * escape for them: an escape the inspector shows and a program must spell is
 * worse than a stated limit.
 *
 * The commands are what make that representation bearable.  `grid_add_row`
 * and `grid_set_cell` put real data in from a subroutine with no string
 * building, and `grid_cell` reads one back.  They take the grid's `name` —
 * the same answer `action` gives to the same problem — because nothing else
 * a program can write names a component.
 *
 * `bind` names a datasource.  While one by that name exists the grid shows
 * ITS rows, and every grid bound to it shows the same rows; the grid's own
 * `rows` are what it falls back to.  A grid's commands reach whichever table
 * it is showing, so a program written against an unbound grid keeps working
 * when a datasource is wired in.  This is the shape that lets a `database`
 * kit hand a query result to a datasource and have it on screen with no code
 * between.
 *
 * `selected` counts from 1 and is 0 for no row; `select` hands the handler
 * that position, and `activate` — a double-click, or Enter on the selected
 * row — hands it the same.  `count` is read-only.
 */
static const Kiln_PropertyDesc GRID_PROPS[] = {
    { "name",     KN_SDT_TEXT, "",     NULL },
    { "bind",     KN_SDT_TEXT, "",     NULL },
    { "columns",  KN_SDT_TEXT, "",     NULL },
    { "rows",     KN_SDT_TEXT, "",     "multiline" },
    { "selected", KN_SDT_INT,  "0",    NULL },
    { "count",    KN_SDT_INT,  "0",    NULL },
    { "left",     KN_SDT_INT,  "0",    NULL },
    { "top",      KN_SDT_INT,  "0",    NULL },
    { "width",    KN_SDT_INT,  "320",  NULL },
    { "height",   KN_SDT_INT,  "160",  NULL },
    ANCHORS,
    { "enabled",  KN_SDT_BOOL, "true", NULL },
};
static const int32_t ROW_PARAM[] = { KN_SDT_INT };
static const Kiln_EventDesc GRID_EVENTS[] = {
    { "select",   1, ROW_PARAM },
    { "activate", 1, ROW_PARAM },
};

/* A datasource is rows with no rectangle: filled once, shown by every grid
 * that binds it.  It has no events — the grids watch it, not the program. */
static const Kiln_PropertyDesc DATASOURCE_PROPS[] = {
    { "name",    KN_SDT_TEXT, "", NULL },
    { "columns", KN_SDT_TEXT, "", NULL },
    { "rows",    KN_SDT_TEXT, "", "multiline" },
    { "count",   KN_SDT_INT,  "0", NULL },
};

#define N(a) (int32_t)(sizeof(a) / sizeof((a)[0]))

/* One signature table per shape; the grid and datasource families share
 * them, and differ only in which component the name is looked up among. */
static const int32_t A_NAME[]          = { KN_SDT_TEXT };
static const int32_t A_NAME_ROW[]      = { KN_SDT_TEXT, KN_SDT_TEXT };
static const int32_t A_NAME_CELL[]     = { KN_SDT_TEXT, KN_SDT_INT, KN_SDT_INT };
static const int32_t A_NAME_CELL_VAL[] = { KN_SDT_TEXT, KN_SDT_INT, KN_SDT_INT, KN_SDT_TEXT };

static const Kiln_CommandDesc UI_COMMANDS[] = {
    { "grid_clear",           "ui_grid_clear",           KN_SDT_BOOL, 1, A_NAME },
    { "grid_add_row",         "ui_grid_add_row",         KN_SDT_INT,  2, A_NAME_ROW },
    { "grid_cell",            "ui_grid_cell",            KN_SDT_TEXT, 3, A_NAME_CELL },
    { "grid_set_cell",        "ui_grid_set_cell",        KN_SDT_BOOL, 4, A_NAME_CELL_VAL },
    { "grid_row_count",       "ui_grid_row_count",       KN_SDT_INT,  1, A_NAME },
    { "datasource_clear",     "ui_datasource_clear",     KN_SDT_BOOL, 1, A_NAME },
    { "datasource_add_row",   "ui_datasource_add_row",   KN_SDT_INT,  2, A_NAME_ROW },
    { "datasource_cell",      "ui_datasource_cell",      KN_SDT_TEXT, 3, A_NAME_CELL },
    { "datasource_set_cell",  "ui_datasource_set_cell",  KN_SDT_BOOL, 4, A_NAME_CELL_VAL },
    { "datasource_row_count", "ui_datasource_row_count", KN_SDT_INT,  1, A_NAME },
};
#define VISUAL KN_COMPONENT_VISUAL
#define NONVISUAL KN_COMPONENT_NONVISUAL

static const Kiln_ComponentDesc UI_COMPONENTS[] = {
    { "form",   KN_ROLE_WINDOW, N(FORM_PROPS),   FORM_PROPS,   N(FORM_EVENTS),   FORM_EVENTS,   VISUAL },
    { "button", KN_ROLE_BUTTON, N(BUTTON_PROPS), BUTTON_PROPS, N(BUTTON_EVENTS), BUTTON_EVENTS, VISUAL },
    { "label",  KN_ROLE_LABEL,  N(LABEL_PROPS),  LABEL_PROPS,  0,                0,             VISUAL },
    { "editbox", KN_ROLE_TEXTBOX, N(EDIT_PROPS),  EDIT_PROPS,   N(EDIT_EVENTS),   EDIT_EVENTS,  VISUAL },
    { "checkbox", KN_ROLE_CHECKBOX, N(CHECK_PROPS), CHECK_PROPS, N(CHECK_EVENTS), CHECK_EVENTS, VISUAL },
    { "groupbox", KN_ROLE_GROUP,  N(GROUP_PROPS),  GROUP_PROPS,  0,               0,            VISUAL },
    { "image",   KN_ROLE_UNKNOWN, N(IMAGE_PROPS),  IMAGE_PROPS,  0,               0,            VISUAL },
    { "progressbar", KN_ROLE_UNKNOWN, N(PROG_PROPS), PROG_PROPS, 0,               0,            VISUAL },
    { "combobox", KN_ROLE_LIST, N(COMBO_PROPS), COMBO_PROPS, N(COMBO_EVENTS), COMBO_EVENTS, VISUAL },
    { "listbox", KN_ROLE_LIST, N(LIST_PROPS), LIST_PROPS, N(LIST_EVENTS), LIST_EVENTS, VISUAL },
    /* No KN_ROLE_RADIO exists in abi/kiln_abi.h, and that header is not
     * this library's to extend; checkbox is the nearest true role — a
     * two-state control that announces its state. */
    { "radiobutton", KN_ROLE_CHECKBOX, N(RADIO_PROPS), RADIO_PROPS, N(RADIO_EVENTS), RADIO_EVENTS, VISUAL },
    { "memo", KN_ROLE_TEXTBOX, N(MEMO_PROPS), MEMO_PROPS, N(MEMO_EVENTS), MEMO_EVENTS, VISUAL },
    { "slider", KN_ROLE_UNKNOWN, N(SLIDER_PROPS), SLIDER_PROPS, N(SLIDER_EVENTS), SLIDER_EVENTS, VISUAL },
    { "spinner", KN_ROLE_TEXTBOX, N(SPIN_PROPS), SPIN_PROPS, N(SPIN_EVENTS), SPIN_EVENTS, VISUAL },
    { "action", KN_ROLE_UNKNOWN, N(ACTION_PROPS), ACTION_PROPS, N(ACTION_EVENTS), ACTION_EVENTS, NONVISUAL },
    /* No table role exists in abi/kiln_abi.h; a list of rows is the
     * nearest true one, and a reader stepping through rows is served by it. */
    { "grid", KN_ROLE_LIST, N(GRID_PROPS), GRID_PROPS, N(GRID_EVENTS), GRID_EVENTS, VISUAL },
    { "datasource", KN_ROLE_UNKNOWN, N(DATASOURCE_PROPS), DATASOURCE_PROPS, 0, 0, NONVISUAL },
};

static const Kiln_LibInfo UI_INFO = {
    KILN_ABI_VERSION,
    "ui",
    "kiln-ui-0000-0000-0000-000000000003",
    0, 1, 0,
    N(UI_COMMANDS), UI_COMMANDS,
    N(UI_COMPONENTS), UI_COMPONENTS,
};

const Kiln_LibInfo *kiln_get_lib_info(void) { return &UI_INFO; }
