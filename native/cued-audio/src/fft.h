// Port of dev.cued.core.dsp.Fft (CUEd phone app).
// In-place iterative radix-2 complex FFT, float precision, power-of-two sizes.
#pragma once

#include <cstddef>
#include <vector>

namespace cued {

class Fft {
public:
    // size must be a power of two >= 2 (throws std::invalid_argument otherwise).
    explicit Fft(int size);

    int size() const { return size_; }

    // Forward transform of re/im in place. Both arrays must hold size() values.
    void transform(float* re, float* im) const;

    // Magnitude spectrum of a real signal: writes size()/2 bins (DC .. Nyquist-1)
    // into out. `window` (size() values) is applied to the first inputCount
    // samples of input; the rest is zero padded. re/im are scratch (size() each).
    void magnitudes(const float* input, int inputCount, const float* window,
                    float* re, float* im, float* out) const;

    // Hann window, same formula as the phone: 0.5 * (1 - cos(2*pi*i/(size-1))).
    static std::vector<float> hann(int size);

private:
    int size_;
    int levels_;
    std::vector<float> cosTable_;
    std::vector<float> sinTable_;
    std::vector<int> bitRev_;
};

} // namespace cued
