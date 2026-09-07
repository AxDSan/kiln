/* Component descriptors, read from the UI library's design-time metadata.
 *
 * The designer links `ui_libinfo.c` directly — the metadata translation unit
 * finally meeting its intended consumer. That is the `.fne` design-time /
 * `.fnr` runtime split (D12) working exactly as designed: the same table the
 * compiler introspects tells the designer what a toolbox holds, which
 * properties an inspector shows, and which events can be wired.
 */
#ifndef KILN_DESIGNER_DESCRIPTORS_H
#define KILN_DESIGNER_DESCRIPTORS_H

#include "kiln_abi.h"

namespace kiln::designer {

inline const Kiln_LibInfo* ui_library() { return kiln_get_lib_info(); }

inline const Kiln_ComponentDesc* describe(const char* type_name) {
    const Kiln_LibInfo* lib = ui_library();
    for (int i = 0; i < lib->component_count; i++) {
        if (std::strcmp(lib->components[i].name, type_name) == 0) return &lib->components[i];
    }
    return nullptr;
}

} // namespace kiln::designer
#endif
