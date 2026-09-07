/* Generates the designer canvas's dot-grid tile.
 *
 * RmlUi tiles an image decorator with `repeat`, so a 10x10 tile with a single
 * dot gives the spec's dot grid at any canvas size. Written as uncompressed TGA
 * because that needs no image library and SDL_image loads it.
 */
#ifndef KILN_DESIGNER_DOTGRID_H
#define KILN_DESIGNER_DOTGRID_H

#include <cstdio>
#include <cstdlib>
#include <sys/stat.h>
#include <string>

namespace kiln::designer {

/// Write a `spacing`x`spacing` BGRA tile with one dot; returns the path.
///
/// The dot's colour is passed in rather than fixed, because the palette is a
/// setting: a grid drawn in the light border colour is a row of pale specks on
/// a dark canvas, which is worse than no grid at all.
inline std::string write_dot_tile(const std::string& path, int spacing = 10,
                                  const std::string& rgb = "#d0d7de") {
    auto hex2 = [&](size_t at) -> unsigned char {
        if (rgb.size() < at + 2) return 0xd0;
        return (unsigned char)std::strtol(rgb.substr(at, 2).c_str(), nullptr, 16);
    };
    const unsigned char R = hex2(1), G = hex2(3), B = hex2(5);
    FILE* f = std::fopen(path.c_str(), "wb");
    if (!f) return "";
    const unsigned char header[18] = {
        0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        (unsigned char)(spacing & 0xff), (unsigned char)(spacing >> 8),
        (unsigned char)(spacing & 0xff), (unsigned char)(spacing >> 8),
        32, 0x20 /* top-left origin */
    };
    std::fwrite(header, 1, sizeof header, f);
    // One dot per tile, 1.5px across, in the palette's border colour,
    // centred on a pixel so it reads as a dot of that colour with a faint
    // rim rather than a pale square. Each pixel's alpha is the share of it
    // the disc covers — sampled, not guessed. Straight alpha: the renderer's
    // TGA loader premultiplies on the way in.
    const double r = 0.75, cx = 0.5, cy = 0.5;
    for (int y = 0; y < spacing; y++) {
        for (int x = 0; x < spacing; x++) {
            int inside = 0;
            const int n = 8;
            for (int sy = 0; sy < n; sy++) {
                for (int sx = 0; sx < n; sx++) {
                    const double px = x + (sx + 0.5) / n, py = y + (sy + 0.5) / n;
                    if ((px - cx) * (px - cx) + (py - cy) * (py - cy) <= r * r) inside++;
                }
            }
            const double a = (double)inside / (n * n);
            const unsigned char pix[4] = {B, G, R, (unsigned char)(0xff * a + 0.5)};
            std::fwrite(pix, 1, 4, f);
        }
    }
    std::fclose(f);
    return path;
}

} // namespace kiln::designer
#endif
