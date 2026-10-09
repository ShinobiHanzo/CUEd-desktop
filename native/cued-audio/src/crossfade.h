// Port of dev.cued.core.mix.Crossfade (gain curves only; tempo matching is
// not part of the desktop engine yet).
#pragma once

namespace cued {

enum class CrossfadeCurve {
    // Both tracks at -3 dB in the middle; constant perceived loudness. Default.
    EqualPower = 0,
    // Straight lines; slight dip in the middle, transparent for very short fades.
    Linear = 1,
    // Outgoing holds then drops late, incoming rises early.
    SmoothStep = 2,
};

struct Gains {
    float outgoing;
    float incoming;
};

// Gain pair for a progress value in 0..1 (clamped).
Gains crossfadeGains(float progress, CrossfadeCurve curve = CrossfadeCurve::EqualPower);

// Maps the C ABI integer (0 equal power, 1 linear, 2 smooth step) to a curve;
// unknown values map to EqualPower.
CrossfadeCurve crossfadeCurveFromInt(int v);

} // namespace cued
