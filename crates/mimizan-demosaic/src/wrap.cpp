// Calls the four RawTherapee demosaicers. The Rust side passes linear samples
// in 0..1. AMaZE stays on that scale. RCD and DCB are written for about
// 0..65536, LMMSE for a 0..65535 gamma table; that conversion stays here.
//
// DCB: 2 iterations and enhance, LMMSE: 2 iterations. Those are the
// RawTherapee defaults used for the Kodak comparison.

#include "librtprocess.h"

#include <algorithm>
#include <vector>

namespace {

bool keep_going(double) { return false; }

void scale_rows(const float *const *src, float **dst, int w, int h, float factor) {
    for (int y = 0; y < h; ++y) {
        for (int x = 0; x < w; ++x) {
            dst[y][x] = src[y][x] * factor;
        }
    }
}

} // namespace

extern "C" int mimizan_rt_demosaic(int method, int width, int height, const float *const *raw,
                                   float **red, float **green, float **blue,
                                   const unsigned (*cfa)[2]) {
    const auto cancel = [](double p) { return keep_going(p); };
    rpError rc = RP_NO_ERROR;
    switch (method) {
    case 0: // AMaZE. initGain 1, border 4, scales 1: values stay in 0..1.
        rc = amaze_demosaic(width, height, 0, 0, width, height, raw, red, green, blue, cfa, cancel,
                            1.0, 4, 1.f, 1.f, 2, false);
        break;
    case 1: { // RCD divides and multiplies by 65536 internally.
        constexpr float scale = 65536.f;
        std::vector<float> buf(static_cast<size_t>(width) * static_cast<size_t>(height));
        std::vector<float *> rows(static_cast<size_t>(height));
        for (int y = 0; y < height; ++y) {
            rows[static_cast<size_t>(y)] = buf.data() + static_cast<size_t>(y) * static_cast<size_t>(width);
        }
        scale_rows(raw, rows.data(), width, height, scale);
        rc = rcd_demosaic(width, height, const_cast<const float *const *>(rows.data()), red, green,
                          blue, cfa, cancel, 2, false, true);
        if (rc == RP_NO_ERROR) {
            scale_rows(const_cast<const float *const *>(red), red, width, height, 1.f / scale);
            scale_rows(const_cast<const float *const *>(green), green, width, height, 1.f / scale);
            scale_rows(const_cast<const float *const *>(blue), blue, width, height, 1.f / scale);
        }
        break;
    }
    case 2: { // DCB expects about 1..65536, not 0..1 (the +1 stabiliser).
        constexpr float scale = 65536.f;
        std::vector<float> buf(static_cast<size_t>(width) * static_cast<size_t>(height));
        std::vector<float *> rows(static_cast<size_t>(height));
        for (int y = 0; y < height; ++y) {
            rows[static_cast<size_t>(y)] = buf.data() + static_cast<size_t>(y) * static_cast<size_t>(width);
        }
        scale_rows(raw, rows.data(), width, height, scale);
        rc = dcb_demosaic(width, height, const_cast<const float *const *>(rows.data()), red, green, blue,
                          cfa, cancel, 2, true);
        if (rc == RP_NO_ERROR) {
            scale_rows(const_cast<const float *const *>(red), red, width, height, 1.f / scale);
            scale_rows(const_cast<const float *const *>(green), green, width, height, 1.f / scale);
            scale_rows(const_cast<const float *const *>(blue), blue, width, height, 1.f / scale);
        }
        break;
    }
    case 3: { // LMMSE indexes a 0..65535 gamma table.
        constexpr float scale = 65535.f;
        std::vector<float> buf(static_cast<size_t>(width) * static_cast<size_t>(height));
        std::vector<float *> rows(static_cast<size_t>(height));
        for (int y = 0; y < height; ++y) {
            rows[static_cast<size_t>(y)] = buf.data() + static_cast<size_t>(y) * static_cast<size_t>(width);
            for (int x = 0; x < width; ++x) {
                rows[static_cast<size_t>(y)][x] =
                    std::clamp(raw[y][x] * scale, 0.f, scale);
            }
        }
        rc = lmmse_demosaic(width, height, const_cast<const float *const *>(rows.data()), red, green,
                            blue, cfa, cancel, 2);
        if (rc == RP_NO_ERROR) {
            scale_rows(const_cast<const float *const *>(red), red, width, height, 1.f / scale);
            scale_rows(const_cast<const float *const *>(green), green, width, height, 1.f / scale);
            scale_rows(const_cast<const float *const *>(blue), blue, width, height, 1.f / scale);
        }
        break;
    }
    default:
        return 2;
    }
    return static_cast<int>(rc);
}
