#include "pcm.h"
#include <cstddef>

namespace cued {
namespace pcm {

void shortsToMonoFloat(const int16_t* src, int channels, float* dst, int frames) {
    const float scale = 1.0f / 32768.0f;
    for (int f = 0; f < frames; ++f) {
        int acc = 0;
        const int base = f * channels;
        for (int c = 0; c < channels; ++c) acc += src[base + c];
        dst[f] = static_cast<float>(acc) * scale / static_cast<float>(channels);
    }
}

std::vector<float> decimate(const std::vector<float>& src, int factor) {
    if (factor <= 1) return src;
    std::vector<float> out(src.size() / static_cast<size_t>(factor));
    for (size_t i = 0; i < out.size(); ++i) {
        float acc = 0.0f;
        const size_t base = i * static_cast<size_t>(factor);
        for (int k = 0; k < factor; ++k) acc += src[base + k];
        out[i] = acc / static_cast<float>(factor);
    }
    return out;
}

} // namespace pcm
} // namespace cued
