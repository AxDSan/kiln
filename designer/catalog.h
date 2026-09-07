/* What the toolbox holds, assembled from metadata rather than a list here.
 *
 * Two sources, because neither is complete on its own:
 *
 *   `kiln kits` / `kiln commands --use <kit>` name every kit installed,
 *   the section it files under, and the components it declares — including
 *   kits the IDE has never heard of, which is the whole promise of the kit
 *   system: a kit dropped into `kits/` appears in Studio with no IDE change.
 *   The listing says whether each component is visual and which editor a
 *   property wants, so a kit's control files under its own heading whether or
 *   not this build was linked against it.
 *
 *   The linked `ui_libinfo.c` adds what those lines do not carry: each
 *   property's default, and what every event hands its handler. The listing
 *   names events but not their parameters, so for a component the linked
 *   table does not know the parameters stay unknown here — and the language
 *   server, which does know, is asked instead.
 *
 * A component the CLI reports without a `kind:` line and the linked table
 * does not know is treated as NON-VISUAL. The two mistakes are not symmetric:
 * a visual component in the tray is a nuisance, and a non-visual one dropped
 * into a form is source the validator rejects.
 */
#ifndef KILN_DESIGNER_CATALOG_H
#define KILN_DESIGNER_CATALOG_H

#include <cstdio>
#include <cstring>
#include <sstream>
#include <string>
#include <vector>

#include "descriptors.h"
#include "portable.h"

namespace kiln::designer {

struct CatalogProp {
    std::string name;
    std::string type;            // as `kiln commands` spells it
    std::string editor;          // "", "color", "file", "font", "multiline"
    std::string default_value;
    bool has_default = false;
};

struct CatalogEvent {
    std::string name;
    /// What the event hands its handler, as type names. `known` is false when
    /// no source could say: the listing does not carry parameters, and only
    /// the linked table fills them in.
    bool known = false;
    std::vector<std::string> params;
};

struct CatalogComponent {
    std::string type_name;
    std::string kit;             // "" for the ones the runtime itself provides
    std::string section;         // the toolbox heading it files under
    bool visual = false;
    /// Whether `visual` came from the listing. When it did, the linked table
    /// must not overrule it: the CLI reads the same descriptor, and reads the
    /// current one.
    bool kind_known = false;
    std::vector<CatalogProp> props;
    std::vector<CatalogEvent> events;

    const CatalogEvent* event(const std::string& name) const {
        for (const auto& e : events) {
            if (e.name == name) return &e;
        }
        return nullptr;
    }

    /// Whether the descriptor declares `name`. Everything the designer writes
    /// on its own initiative — a drag, a resize, a nudge, a paste — has to
    /// pass this: the compiler rejects a property the component does not
    /// declare, and a build that fails on a line the user never typed is the
    /// designer's fault, not theirs.
    bool declares(const std::string& name) const {
        for (const auto& p : props) {
            if (p.name == name) return true;
        }
        return false;
    }
};

struct Catalog {
    std::vector<CatalogComponent> components;   // already in toolbox order
    /// Section headings in the order the kits reported them.
    std::vector<std::string> sections;

    const CatalogComponent* find(const std::string& type_name) const {
        for (const auto& c : components) {
            if (c.type_name == type_name) return &c;
        }
        return nullptr;
    }
};

/// The Kiln spelling of a slot tag, for what the linked table declares.
inline const char* tag_name(int tag) {
    return tag == KN_SDT_INT      ? "int"
           : tag == KN_SDT_INT64  ? "int64"
           : tag == KN_SDT_DOUBLE ? "double"
           : tag == KN_SDT_BOOL   ? "bool"
                                  : "text";
}

namespace catalog_detail {

inline std::vector<std::string> run(const std::string& cmd) {
    std::vector<std::string> out;
    std::string text;
    if (kiln::sys::capture_output(cmd, false, text) == -1 && text.empty()) return out;
    std::istringstream lines(text);
    std::string line;
    while (std::getline(lines, line)) {
        while (!line.empty() && (line.back() == '\n' || line.back() == '\r')) line.pop_back();
        out.push_back(line);
    }
    return out;
}

/// `<prefix><rest>` -> rest, or no value when the line is a different kind.
inline bool after(const std::string& line, const char* prefix, std::string& rest) {
    const size_t n = std::strlen(prefix);
    if (line.rfind(prefix, 0) != 0) return false;
    rest = line.substr(n);
    return true;
}

inline std::vector<std::string> words(const std::string& s) {
    std::vector<std::string> out;
    std::istringstream in(s);
    std::string w;
    while (in >> w) out.push_back(w);
    return out;
}

/// One kit, as `kiln kits` describes it. Emission order IS resolution
/// order, so the vector's order is the kit ordering and nothing sorts it.
struct KitLine {
    std::string name, display, section;
};

inline std::vector<KitLine> read_kits(const std::string& bin) {
    std::vector<KitLine> kits;
    auto by_name = [&](const std::string& n) -> KitLine* {
        for (auto& k : kits) {
            if (k.name == n) return &k;
        }
        return nullptr;
    };
    for (const auto& line : run(bin + " kits")) {
        std::string rest;
        if (after(line, "kit: ", rest)) {
            const auto w = words(rest);
            if (!w.empty() && !by_name(w[0])) kits.push_back({w[0], w[0], ""});
        } else if (after(line, "name: ", rest)) {
            const auto w = words(rest);
            if (w.size() >= 2) {
                if (KitLine* k = by_name(w[0])) k->display = rest.substr(w[0].size() + 1);
            }
        } else if (after(line, "section: ", rest)) {
            const auto w = words(rest);
            if (w.size() >= 2) {
                if (KitLine* k = by_name(w[0])) k->section = rest.substr(w[0].size() + 1);
            }
        }
    }
    return kits;
}

/// The components in one `kiln commands` listing, with their declared
/// properties, editors, kind and events. Takes the lines rather than running
/// the command so the reading can be tested against a listing no installed
/// kit produces.
inline std::vector<CatalogComponent> parse_components(const std::vector<std::string>& lines,
                                                      const std::string& use) {
    std::vector<CatalogComponent> out;
    auto named = [&](const std::string& type_name) -> CatalogComponent* {
        for (auto& c : out) {
            if (c.type_name == type_name) return &c;
        }
        return nullptr;
    };
    for (const auto& line : lines) {
        std::string rest;
        if (after(line, "component: ", rest)) {
            const auto w = words(rest);
            if (!w.empty()) {
                CatalogComponent c;
                c.type_name = w[0];
                c.kit = use;
                out.push_back(c);
            }
        } else if (after(line, "kind: ", rest)) {
            const auto w = words(rest);
            if (w.size() >= 2) {
                if (CatalogComponent* c = named(w[0])) {
                    c->visual = (w[1] == "visual");
                    c->kind_known = true;
                }
            }
        } else if (after(line, "property: ", rest)) {
            const auto w = words(rest);
            if (w.size() >= 3) {
                if (CatalogComponent* c = named(w[0])) c->props.push_back({w[1], w[2], "", "", false});
            }
        } else if (after(line, "editor: ", rest)) {
            const auto w = words(rest);
            if (w.size() >= 3) {
                if (CatalogComponent* c = named(w[0])) {
                    for (auto& p : c->props) {
                        if (p.name == w[1]) p.editor = w[2];
                    }
                }
            }
        } else if (after(line, "event: ", rest)) {
            const auto w = words(rest);
            if (w.size() >= 2) {
                if (CatalogComponent* c = named(w[0])) c->events.push_back({w[1], false, {}});
            }
        }
    }
    return out;
}

inline std::vector<CatalogComponent> read_components(const std::string& bin,
                                                     const std::string& use) {
    const std::string cmd = use.empty() ? bin + " commands" : bin + " commands --use " + use;
    return parse_components(run(cmd), use);
}

/// Fill in what only the linked metadata table knows: each property's
/// default, what each event hands its handler, and — for a listing old enough
/// to have no `kind:` line — whether the component is visual.
inline void enrich_from_libinfo(CatalogComponent& c) {
    const Kiln_ComponentDesc* desc = describe(c.type_name.c_str());
    if (!desc) return;
    if (!c.kind_known) c.visual = (desc->kind == KN_COMPONENT_VISUAL);
    for (auto& p : c.props) {
        for (int i = 0; i < desc->property_count; i++) {
            if (p.name != desc->properties[i].name) continue;
            if (desc->properties[i].editor && p.editor.empty()) p.editor = desc->properties[i].editor;
            if (desc->properties[i].default_value) {
                p.default_value = desc->properties[i].default_value;
                p.has_default = true;
            }
        }
    }
    for (auto& e : c.events) {
        for (int i = 0; i < desc->event_count; i++) {
            if (e.name != desc->events[i].name) continue;
            e.known = true;
            e.params.clear();
            for (int j = 0; j < desc->events[i].param_count; j++) {
                e.params.push_back(tag_name(desc->events[i].param_tags[j]));
            }
        }
    }
}

}  // namespace catalog_detail

/// Assemble the catalogue. One subprocess per kit, once, at startup.
inline Catalog build_catalog(const std::string& kiln_bin) {
    using namespace catalog_detail;
    Catalog cat;

    // What exists with no `use` at all: the runtime's own components, which
    // belong to no kit. `--use net` reports them too, so this is also the
    // baseline that keeps them from being attributed to whichever kit was
    // asked first.
    std::vector<std::string> baseline_names;
    for (auto& c : read_components(kiln_bin, "")) {
        baseline_names.push_back(c.type_name);
        enrich_from_libinfo(c);
        c.section = "System";
        cat.components.push_back(c);
    }

    for (const auto& kit : read_kits(kiln_bin)) {
        for (auto& c : read_components(kiln_bin, kit.name)) {
            bool baseline = false;
            for (const auto& b : baseline_names) {
                if (b == c.type_name) baseline = true;
            }
            if (baseline || cat.find(c.type_name)) continue;
            enrich_from_libinfo(c);
            // A kit with nothing to say about its section gets `kit.rs`'s
            // generic default, which reads as a heading no better than the
            // kit's own name. The name at least distinguishes one kit's
            // controls from another's.
            const std::string kit_section =
                (kit.section.empty() || kit.section == "Libraries") ? kit.display : kit.section;
            // Everything without pixels files under one heading, whichever kit
            // it came from: the tray is one place, so its source is too.
            c.section = c.visual ? kit_section : "System";
            cat.components.push_back(c);
        }
    }

    // The linked table is the floor, not the ceiling: if the toolchain cannot
    // be run — a wrong compiler path, a tree mid-rebuild — a toolbox built
    // only from its answers would be empty, and an IDE with no components is
    // no more use than one with the wrong ones. Everything Studio was compiled
    // against is added here if the CLI did not already report it.
    {
        std::string ui_section = "Common Controls";
        for (const auto& k : read_kits(kiln_bin)) {
            if (k.name != "ui") continue;
            ui_section = (k.section.empty() || k.section == "Libraries") ? k.display : k.section;
        }
        const Kiln_LibInfo* lib = ui_library();
        for (int i = 0; i < lib->component_count; i++) {
            const Kiln_ComponentDesc& d = lib->components[i];
            if (cat.find(d.name)) continue;
            CatalogComponent c;
            c.type_name = d.name;
            c.kit = "ui";
            c.visual = (d.kind == KN_COMPONENT_VISUAL);
            c.kind_known = true;
            c.section = c.visual ? ui_section : "System";
            for (int j = 0; j < d.property_count; j++) {
                CatalogProp p;
                p.name = d.properties[j].name;
                p.type = tag_name(d.properties[j].tag);
                if (d.properties[j].editor) p.editor = d.properties[j].editor;
                if (d.properties[j].default_value) {
                    p.default_value = d.properties[j].default_value;
                    p.has_default = true;
                }
                c.props.push_back(p);
            }
            for (int j = 0; j < d.event_count; j++) c.events.push_back({d.events[j].name, false, {}});
            enrich_from_libinfo(c);
            cat.components.push_back(c);
        }
    }

    for (const auto& c : cat.components) {
        bool seen = false;
        for (const auto& s : cat.sections) {
            if (s == c.section) seen = true;
        }
        if (!seen) cat.sections.push_back(c.section);
    }
    // System last: it is the tray's shelf, not the palette people reach for.
    for (size_t i = 0; i < cat.sections.size(); i++) {
        if (cat.sections[i] == "System") {
            cat.sections.erase(cat.sections.begin() + (long)i);
            cat.sections.push_back("System");
            break;
        }
    }
    return cat;
}

}  // namespace kiln::designer
#endif
