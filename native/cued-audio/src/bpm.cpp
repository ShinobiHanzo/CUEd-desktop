#include "bpm.h"
#include <cstddef>

#include <algorithm>
#include <cmath>
#include <limits>

namespace cued {

namespace {
// Kotlin's Float.roundToInt(): round half up (towards positive infinity).
int roundToInt(float v) { return static_cast<int>(std::floor(v + 0.5f)); }
}

BpmDetector::BpmDetector(int sampleRate, float minBpm, float maxBpm, int hopSize, float priorBpm, float priorWidthOctaves)
    : sampleRate_(sampleRate), minBpm_(minBpm), maxBpm_(maxBpm), hopSize_(hopSize),
      priorBpm_(priorBpm), priorWidthOctaves_(priorWidthOctaves) {}

BpmResult BpmDetector::analyse(const float* pcm, int count) const {
    const int frames = std::max(1, count / hopSize_);
    std::vector<float> energy(static_cast<size_t>(frames));
    for (int f = 0; f < frames; ++f) {
        float acc = 0.0f;
        const int start = f * hopSize_;
        const int end = std::min(start + hopSize_, count);
        for (int i = start; i < end; ++i) acc += pcm[i] * pcm[i];
        energy[f] = std::sqrt(acc / static_cast<float>(std::max(1, end - start)));
    }
    // Onset strength: positive change in energy, lightly smoothed.
    std::vector<float> onset(static_cast<size_t>(frames), 0.0f);
    float smooth = 0.0f;
    for (int f = 1; f < frames; ++f) {
        const float diff = energy[f] - energy[f - 1];
        smooth = 0.6f * smooth + 0.4f * std::max(0.0f, diff);
        onset[f] = smooth;
    }
    // Kotlin's FloatArray.average() accumulates in double.
    double sum = 0.0;
    for (float v : onset) sum += v;
    const float mean = static_cast<float>(sum / frames);
    for (float& v : onset) v -= mean;

    const float framesPerSec = static_cast<float>(sampleRate_) / static_cast<float>(hopSize_);
    const int minLag = std::max(1, roundToInt(framesPerSec * 60.0f / maxBpm_));
    const int maxLag = std::min(frames - 1, roundToInt(framesPerSec * 60.0f / minBpm_));
    if (maxLag <= minLag) return BpmResult{};

    std::vector<float> ac(static_cast<size_t>(maxLag) * 2 + 2, 0.0f);
    const int acMax = std::min(frames - 1, maxLag * 2);
    for (int lag = minLag; lag <= acMax; ++lag) {
        float acc = 0.0f;
        for (int f = lag; f < frames; ++f) acc += onset[f] * onset[f - lag];
        ac[lag] = acc / static_cast<float>(frames - lag);
    }
    // Parabolic-interpolated peak height at local maxima (integer lags
    // under-read peaks that fall between lags).
    std::vector<float> peak(ac.size(), 0.0f);
    for (int lag = 1; lag < acMax; ++lag) {
        const float y0 = ac[lag - 1], y1 = ac[lag], y2 = ac[lag + 1];
        if (y1 >= y0 && y1 >= y2) {
            const float denom = y0 - 2.0f * y1 + y2;
            peak[lag] = std::fabs(denom) > 1e-9f ? y1 - (y0 - y2) * (y0 - y2) / (8.0f * denom) : y1;
        } else {
            peak[lag] = y1;
        }
    }
    auto peakNear = [&](int lag) {
        float m = 0.0f;
        for (int l = lag - 1; l <= lag + 1; ++l) {
            if (l >= 1 && l < acMax) m = std::max(m, peak[l]);
        }
        return m;
    };
    // Score each candidate lag: raw autocorrelation reinforced by its double
    // and weighted by a broad log-Gaussian prior centred on priorBpm.
    std::vector<float> score(static_cast<size_t>(maxLag) + 1, 0.0f);
    int best = minLag;
    const float ln2 = std::log(2.0f);
    for (int lag = minLag; lag <= maxLag; ++lag) {
        const float bpmAtLag = 60.0f * framesPerSec / static_cast<float>(lag);
        const float octaves = std::log(bpmAtLag / priorBpm_) / ln2;
        const float prior = std::exp(-0.5f * (octaves / priorWidthOctaves_) * (octaves / priorWidthOctaves_));
        const float harmonic = (lag * 2 <= acMax) ? 0.5f * peakNear(lag * 2) : 0.0f;
        score[lag] = prior * (std::max(0.0f, peak[lag]) + harmonic);
        if (score[lag] > score[best]) best = lag;
    }
    if (score[best] <= 0.0f || ac[best] <= 0.0f) return BpmResult{};
    // Tie-break towards the fundamental: the shortest lag nearly as good as the best.
    const float threshold = score[best] * 0.85f;
    for (int lag = minLag; lag <= maxLag; ++lag) {
        if (score[lag] >= threshold) { best = lag; break; }
    }

    // Parabolic interpolation for sub-frame lag precision.
    float refined = static_cast<float>(best);
    if (best >= minLag + 1 && best < maxLag) {
        const float y0 = ac[best - 1], y1 = ac[best], y2 = ac[best + 1];
        const float denom = y0 - 2.0f * y1 + y2;
        if (std::fabs(denom) > 1e-9f) refined = static_cast<float>(best) + 0.5f * (y0 - y2) / denom;
    }

    const float bpm = 60.0f * framesPerSec / refined;
    // Confidence: prominence over the strongest rival that is neither adjacent
    // nor a simple multiple of the chosen lag.
    float second = 0.0f;
    for (int lag = minLag; lag <= maxLag; ++lag) {
        if (std::abs(lag - best) <= 2) continue;
        if (std::abs(lag - best * 2) <= 2 || std::abs(lag * 2 - best) <= 2) continue;
        if (score[lag] > second) second = score[lag];
    }
    const float confidence = std::min(std::max((score[best] - second) / score[best], 0.0f), 1.0f);

    // Beat phase: the comb offset that best matches the onsets.
    const float period = refined;
    int bestPhase = 0;
    float bestScore = -std::numeric_limits<float>::infinity();
    const int phaseSteps = std::max(1, roundToInt(period));
    for (int phase = 0; phase < phaseSteps; ++phase) {
        float s = 0.0f;
        float t = static_cast<float>(phase);
        while (t < static_cast<float>(frames)) {
            s += onset[std::min(roundToInt(t), frames - 1)];
            t += period;
        }
        if (s > bestScore) { bestScore = s; bestPhase = phase; }
    }
    return BpmResult{bpm, confidence, static_cast<float>(bestPhase) / framesPerSec};
}

} // namespace cued
