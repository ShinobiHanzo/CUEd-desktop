#include "crossfade.h"

#include <cmath>

namespace cued {

Gains crossfadeGains(float progress, CrossfadeCurve curve) {
    float t = progress;
    if (t < 0.0f) t = 0.0f;
    if (t > 1.0f) t = 1.0f;
    switch (curve) {
        case CrossfadeCurve::Linear:
            return Gains{1.0f - t, t};
        case CrossfadeCurve::SmoothStep: {
            const float s = t * t * (3.0f - 2.0f * t);
            return Gains{1.0f - s, s};
        }
        case CrossfadeCurve::EqualPower:
        default: {
            const float a = t * static_cast<float>(3.14159265358979323846 / 2.0);
            return Gains{std::cos(a), std::sin(a)};
        }
    }
}

CrossfadeCurve crossfadeCurveFromInt(int v) {
    switch (v) {
        case 1: return CrossfadeCurve::Linear;
        case 2: return CrossfadeCurve::SmoothStep;
        default: return CrossfadeCurve::EqualPower;
    }
}

} // namespace cued
