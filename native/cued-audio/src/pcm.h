// Port of dev.cued.core.dsp.Pcm: interleaved 16-bit PCM to mono float, and
// box-filter decimation used to keep whole-track analysis cheap.
#pragma once

#include <cstdint>
#include <cstddef>
#include <vector>

namespace cued {
namespace pcm {

// dst[f] = mean over channels of src[f*channels + c] / 32768.
void shortsToMonoFloat(const int16_t* src, int channels, float* dst, int frames);

// Decimates by an integer factor with a simple box filter (drops the tail
// that does not fill a whole block, like the phone).
std::vector<float> decimate(const std::vector<float>& src, int factor);

} // namespace pcm
} // namespace cued
