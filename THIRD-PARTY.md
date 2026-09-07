# Third-party notices

Kiln bundles and statically links the components below. Their licence texts
ship in `licenses/` inside the release bundle, and this file must stay accurate
— these notices are a condition of redistribution, not a courtesy.

Only permissive licences are accepted (MIT / BSD / Apache-2.0 / Zlib / ISC);
GPL and LGPL without a static-linking exception, non-OSI grants and proprietary
code are rejected.

## Statically linked into `kiln-studio` and into GUI programs it builds

| Component | Licence | Used for |
| --- | --- | --- |
| [RmlUi](https://github.com/mikke89/RmlUi) | MIT | The UI substrate: layout, styling and form controls |
| [AccessKit C](https://github.com/AccessKit/accesskit-c) | MIT OR Apache-2.0 | The accessibility bridge, exposing the UI tree to AT-SPI |

RmlUi in turn uses:

* **FreeType** — dynamically linked, [FTL or GPLv2](https://freetype.org/license.html);
  Kiln relies on the FreeType Licence (BSD-style with a credit clause).

## Dynamically linked (not redistributed)

These are resolved from the host system and are **not** included in the bundle,
so their licences do not attach to it. They must be installed to build or run
GUI programs:

| Component | Licence |
| --- | --- |
| SDL2, SDL2_image | Zlib |
| FreeType | FTL / GPLv2 |
| OpenGL / GLX (system driver) | vendor-specific |
| libstdc++ | GPLv3 with the GCC Runtime Library Exception |

## Build-time tools (not redistributed)

`clang` and `ar` are invoked by `kiln build`; they are ordinary external
tools and are not part of the bundle.

## Not vendored, deliberately

The BlackMoon `kernel.lib` (BSD-3-Clause) is used **only as a behaviour oracle**
and is reimplemented from public specifications — never copied or shipped. No
proprietary EPL or BlackMoon binary is included.
