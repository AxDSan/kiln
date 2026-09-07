/* The designer's document model: what `kiln inspect` reported, plus edits.
 *
 * Deliberately dumb — a list of components with string properties. The compiler
 * owns all meaning; the designer only moves text around.
 */
#ifndef KILN_DESIGNER_MODEL_H
#define KILN_DESIGNER_MODEL_H

#include <functional>
#include <string>
#include <utility>
#include <vector>

namespace kiln::designer {

struct Component {
    std::string id;
    std::string type_name;
    std::vector<std::pair<std::string, std::string>> properties;  // ordered
    std::vector<std::pair<std::string, std::string>> handlers;    // event -> sub

    /// The lines this component occupies in the file, for a module-level one.
    /// Zero until it has been written: a component the designer has just
    /// dropped exists only in memory, and the save that first writes it is
    /// what gives it a span.
    int first_line = 0;
    int last_line = 0;
    /// Deleted in the designer but still present in the file. It cannot simply
    /// be dropped from the model: the save that removes it has to splice
    /// nothing over the lines it still occupies, and only its span knows where
    /// those are.
    bool removed = false;

    const std::string* property(const std::string& name) const {
        for (const auto& p : properties) {
            if (p.first == name) return &p.second;
        }
        return nullptr;
    }
    void set_property(const std::string& name, const std::string& value) {
        for (auto& p : properties) {
            if (p.first == name) { p.second = value; return; }
        }
        properties.emplace_back(name, value);
    }
    const std::string* handler(const std::string& event) const {
        for (const auto& h : handlers) {
            if (h.first == event) return &h.second;
        }
        return nullptr;
    }
    void set_handler(const std::string& event, const std::string& sub) {
        for (auto& h : handlers) {
            if (h.first == event) { h.second = sub; return; }
        }
        handlers.emplace_back(event, sub);
    }
};

struct Model {
    std::string path;                 // the .kiln being edited
    /// The project.kproj this file belongs to, or "" for a loose file. Set
    /// when the model was opened THROUGH a project; the file is what is
    /// edited and saved, the project is what is remembered.
    std::string project;
    std::string module_name;
    std::vector<std::string> uses;
    std::vector<std::string> subs;    // existing subroutine names
    std::string form_name;
    int form_first_line = 0;          // 1-based, inclusive
    int form_last_line = 0;
    Component form;                   // the form's own properties/handlers
    std::vector<Component> children;
    /// Components declared at module level: `timer`, `action`, `httpserver` —
    /// the ones with no rectangle. A form cannot hold them (the validator
    /// rejects it), so they are a separate list rather than children with a
    /// flag, and they are spliced into the file at their own line spans.
    std::vector<Component> module_components;
    /// Ids renamed in the designer but not yet in the file, oldest first.
    /// A rename is not written when it is made: the hand-written lines that
    /// name the component are outside every span the designer owns, and only
    /// the save that copies them past knows where they are — so the rename
    /// travels with the model, is applied to those lines as they are copied,
    /// and an undo that restores the model before it simply never writes it.
    std::vector<std::pair<std::string, std::string>> renames;

    Component* find(const std::string& id) {
        if (!form_name.empty() && id == form_name) return &form;
        for (auto& c : children) {
            if (c.id == id) return &c;
        }
        for (auto& c : module_components) {
            if (c.id == id) return &c;
        }
        return nullptr;
    }
    bool is_module_level(const std::string& id) const {
        for (const auto& c : module_components) {
            if (c.id == id) return true;
        }
        return false;
    }
    /// Is `id` the form, a component, or a subroutine — anything a new id
    /// would collide with.
    bool has_id(const std::string& id) const {
        if (!form_name.empty() && id == form_name) return true;
        for (const auto& c : children) {
            if (c.id == id) return true;
        }
        for (const auto& c : module_components) {
            if (c.id == id) return true;
        }
        return false;
    }
    bool has_sub(const std::string& name) const {
        for (const auto& s : subs) {
            if (s == name) return true;
        }
        return false;
    }
    /// An id not already taken, based on a component type (`button1`, `button2`…).
    std::string fresh_id(const std::string& type_name) const {
        for (int n = 1;; n++) {
            std::string candidate = type_name + std::to_string(n);
            bool taken = false;
            for (const auto& c : children) {
                if (c.id == candidate) { taken = true; break; }
            }
            for (const auto& c : module_components) {
                if (c.id == candidate) { taken = true; break; }
            }
            if (!taken) return candidate;
        }
    }

    /// Take the on-disk line spans from `live`, discarding this model's own.
    ///
    /// A span describes the file, not the edit: an undo snapshot was taken
    /// before a save that moved every line after the form, so restoring one
    /// wholesale would point the next save's splice at the wrong lines and
    /// overwrite whatever now lives there.
    void adopt_spans(const Model& live) {
        form_first_line = live.form_first_line;
        form_last_line = live.form_last_line;
        for (auto& c : module_components) {
            c.first_line = c.last_line = 0;
            for (const auto& l : live.module_components) {
                if (l.id == c.id) { c.first_line = l.first_line; c.last_line = l.last_line; }
            }
        }
    }
};

/// Load by running `kiln inspect` — never by parsing.kiln here.
///
/// `path` may also be a `project.kproj` or a directory holding one; the
/// entry is then resolved through `kiln project`, and `out.path` is that
/// entry. A save splices into `out.path`, so it must never be the project
/// file.
bool load_model(const std::string& kiln_bin, const std::string& path, Model& out,
                std::string& error);

/// Decides whether a property value must be written as a quoted text literal.
/// The designer supplies this from the component descriptors, because only the
/// declared type can tell `text = "true"` from `checked = true` — guessing from
/// the value's shape writes uncompilable source.
using NeedsQuotes =
    std::function<bool(const std::string& type_name, const std::string& property)>;

/// Is `id` a name the compiler would take as an identifier: a letter or an
/// underscore, then letters, digits and underscores, and not a keyword.
bool is_identifier(const std::string& id);

/// Record that `old_id` is now `new_id`: the form's name or a component's,
/// plus the pending rename a save applies to the file's other lines. Refuses,
/// with the reason in `error` and nothing changed, when the new id is not an
/// identifier, is taken by the form, a component or a subroutine, or when
/// `old_id` names nothing.
bool rename_id(Model& m, const std::string& old_id, const std::string& new_id, std::string& error);

/// `line` with every reference `old_id.` rewritten to `new_id.` — whole words
/// only, and never inside a text literal or a comment. A component is
/// referred to as `id.property` or `id.method`; an `on event: sub` line names
/// a subroutine, which a rename must leave alone, and this leaves it alone
/// because the id there is not followed by a dot.
std::string rename_references(const std::string& line, const std::string& old_id,
                              const std::string& new_id);

/// Render the model's `form … end` block as .kiln source.
std::string emit_form(const Model& m, const NeedsQuotes& needs_quotes);

/// Render one module-level component as a top-level `<type> <id> … end` block.
std::string emit_module_component(const Component& c, const NeedsQuotes& needs_quotes);

/// Save by SPLICING the emitted form over the original file's form lines,
/// leaving everything else — every hand-written subroutine body — byte-identical.
/// Newly wired handlers are appended as stub subroutines.
///
/// Takes the model by reference because a save moves lines: the spans it
/// splices at are recomputed from where each block actually landed, and a
/// component written for the first time gets its span here. A second save in
/// the same session splices at stale spans otherwise, which overwrites code.
bool save_model(Model& m, const std::vector<std::string>& new_subs,
                const NeedsQuotes& needs_quotes, std::string& error);

} // namespace kiln::designer
#endif
