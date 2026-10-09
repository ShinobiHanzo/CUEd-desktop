// Port of MixTest.`equal power crossfade keeps power constant`, plus linear and
// smooth-step shape checks.
#include <cmath>

#include "crossfade.h"
#include "test_util.h"

int main() {
    using namespace cued;
    for (int i = 0; i <= 10; ++i) {
        Gains g = crossfadeGains(i / 10.0f, CrossfadeCurve::EqualPower);
        CHECK_MSG(std::fabs(g.outgoing * g.outgoing + g.incoming * g.incoming - 1.0f) < 1e-4f,
                  "t=%g out=%g in=%g", i / 10.0, g.outgoing, g.incoming);
    }
    CHECK(crossfadeGains(0.0f).outgoing == 1.0f);
    CHECK(crossfadeGains(1.0f).incoming == 1.0f);
    CHECK_NEAR(crossfadeGains(0.5f).outgoing, std::sqrt(0.5f), 1e-5f); // -3 dB in the middle
    CHECK_NEAR(crossfadeGains(0.5f).incoming, std::sqrt(0.5f), 1e-5f);

    for (int i = 0; i <= 10; ++i) {
        const float t = i / 10.0f;
        Gains g = crossfadeGains(t, CrossfadeCurve::Linear);
        CHECK_NEAR(g.outgoing, 1.0f - t, 1e-6f);
        CHECK_NEAR(g.incoming, t, 1e-6f);
        CHECK_NEAR(g.outgoing + g.incoming, 1.0f, 1e-6f);
    }

    for (int i = 0; i <= 10; ++i) {
        const float t = i / 10.0f;
        const float s = t * t * (3.0f - 2.0f * t);
        Gains g = crossfadeGains(t, CrossfadeCurve::SmoothStep);
        CHECK_NEAR(g.outgoing, 1.0f - s, 1e-6f);
        CHECK_NEAR(g.incoming, s, 1e-6f);
    }
    // Smooth step is symmetric, flat at the ends, and equal to linear at the midpoint.
    CHECK_NEAR(crossfadeGains(0.5f, CrossfadeCurve::SmoothStep).incoming, 0.5f, 1e-6f);
    CHECK(crossfadeGains(0.1f, CrossfadeCurve::SmoothStep).incoming < 0.1f);
    CHECK(crossfadeGains(0.9f, CrossfadeCurve::SmoothStep).incoming > 0.9f);

    // Progress is clamped.
    CHECK(crossfadeGains(-1.0f, CrossfadeCurve::Linear).outgoing == 1.0f);
    CHECK(crossfadeGains(2.0f, CrossfadeCurve::Linear).incoming == 1.0f);

    CHECK(crossfadeCurveFromInt(0) == CrossfadeCurve::EqualPower);
    CHECK(crossfadeCurveFromInt(1) == CrossfadeCurve::Linear);
    CHECK(crossfadeCurveFromInt(2) == CrossfadeCurve::SmoothStep);
    CHECK(crossfadeCurveFromInt(99) == CrossfadeCurve::EqualPower);
    return test::finish("crossfade");
}
