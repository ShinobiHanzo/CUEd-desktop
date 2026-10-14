// Port of dev.cued.core.dsp.VocalCut: karaoke / vocal cut for stereo float PCM.
//
// Lead vocals sit in the centre of nearly every mix, so the difference of the
// two channels cancels them. That alone also removes kick and bass, which are
// centred too, so the centre's low band (below bassHz) is put back. Strength
// blends between the original (0) and the full cut (1).
#pragma once

#include <cstddef>

namespace cued {

class VocalCut {
public:
    explicit VocalCut(int sampleRate = 48000, float bassHz = 160.0f);

    void setSampleRate(int sampleRate);
    // 0..1, clamped. 0 leaves the audio untouched.
    void setStrength(float s);
    float strength() const { return strength_; }
    void reset() { lp1_ = lp2_ = 0.0f; }

    // In place on interleaved stereo f32.
    void process(float* stereo, int frames);

private:
    void recompute();

    int sampleRate_;
    float bassHz_;
    float strength_ = 1.0f;
    float alpha_ = 0.0f;
    // Two cascaded one-pole stages: 12 dB/octave, no resonance.
    float lp1_ = 0.0f;
    float lp2_ = 0.0f;
};

} // namespace cued
