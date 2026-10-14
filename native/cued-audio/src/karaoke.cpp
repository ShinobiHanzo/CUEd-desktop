#include "karaoke.h"

#include <algorithm>
#include <cmath>

namespace cued {

namespace {
constexpr double kTwoPi = 6.283185307179586476925286766559;
}

VocalCut::VocalCut(int sampleRate, float bassHz) : sampleRate_(sampleRate > 0 ? sampleRate : 48000), bassHz_(bassHz) {
    recompute();
}

void VocalCut::setSampleRate(int sampleRate) {
    sampleRate_ = sampleRate > 0 ? sampleRate : 48000;
    recompute();
}

void VocalCut::setStrength(float s) {
    if (std::isnan(s)) s = 0.0f;
    strength_ = std::min(std::max(s, 0.0f), 1.0f);
}

void VocalCut::recompute() {
    // One-pole low-pass: y += a * (x - y), a = 1 - e^(-2π f / sr).
    const double a = 1.0 - std::exp(-kTwoPi * static_cast<double>(bassHz_) / static_cast<double>(sampleRate_));
    alpha_ = static_cast<float>(std::min(std::max(a, 0.0), 1.0));
}

void VocalCut::process(float* stereo, int frames) {
    if (strength_ <= 0.0f || frames <= 0) return;
    const float s = strength_;
    const float keep = 1.0f - s;
    float lp1 = lp1_, lp2 = lp2_;
    for (int i = 0; i < frames; ++i) {
        const float l = stereo[2 * i];
        const float r = stereo[2 * i + 1];
        const float mid = 0.5f * (l + r);
        const float side = 0.5f * (l - r);
        lp1 += alpha_ * (mid - lp1);
        lp2 += alpha_ * (lp1 - lp2);
        stereo[2 * i] = l * keep + (side + lp2) * s;
        stereo[2 * i + 1] = r * keep + (-side + lp2) * s;
    }
    lp1_ = lp1;
    lp2_ = lp2;
}

} // namespace cued
