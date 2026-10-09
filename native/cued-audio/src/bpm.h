// Port of dev.cued.core.dsp.BpmDetector: tempo and beat-phase estimation from
// mono PCM by onset-energy autocorrelation with a log-Gaussian tempo prior.
#pragma once

#include <vector>

namespace cued {

struct BpmResult {
    float bpm = 0.0f;
    // Confidence in 0..1, from the autocorrelation peak prominence.
    float confidence = 0.0f;
    // Position of the first detected beat in seconds.
    float firstBeatSec = 0.0f;
};

class BpmDetector {
public:
    BpmDetector(int sampleRate = 44100, float minBpm = 60.0f, float maxBpm = 200.0f, int hopSize = 512,
                float priorBpm = 120.0f, float priorWidthOctaves = 1.0f);

    BpmResult analyse(const float* pcm, int count) const;
    BpmResult analyse(const std::vector<float>& pcm) const { return analyse(pcm.data(), static_cast<int>(pcm.size())); }

private:
    int sampleRate_;
    float minBpm_, maxBpm_;
    int hopSize_;
    float priorBpm_, priorWidthOctaves_;
};

} // namespace cued
