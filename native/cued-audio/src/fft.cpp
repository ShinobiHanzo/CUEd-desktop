#include "fft.h"

#include <cmath>
#include <stdexcept>
#include <utility>

namespace cued {

namespace {
constexpr double kPi = 3.14159265358979323846;

int trailingZeros(int v) {
    int n = 0;
    while ((v & 1) == 0) { v >>= 1; ++n; }
    return n;
}

// Integer.reverse(i) ushr (32 - levels): reverse the lowest `levels` bits.
int reverseBits(int v, int levels) {
    int r = 0;
    for (int i = 0; i < levels; ++i) {
        r = (r << 1) | (v & 1);
        v >>= 1;
    }
    return r;
}
} // namespace

Fft::Fft(int size) : size_(size) {
    if (size < 2 || (size & (size - 1)) != 0) {
        throw std::invalid_argument("FFT size must be a power of two");
    }
    levels_ = trailingZeros(size);
    cosTable_.resize(size / 2);
    sinTable_.resize(size / 2);
    for (int i = 0; i < size / 2; ++i) {
        cosTable_[i] = static_cast<float>(std::cos(2.0 * kPi * i / size));
        sinTable_[i] = static_cast<float>(std::sin(2.0 * kPi * i / size));
    }
    bitRev_.resize(size);
    for (int i = 0; i < size; ++i) bitRev_[i] = reverseBits(i, levels_);
}

void Fft::transform(float* re, float* im) const {
    const int n = size_;
    for (int i = 0; i < n; ++i) {
        const int j = bitRev_[i];
        if (j > i) {
            std::swap(re[i], re[j]);
            std::swap(im[i], im[j]);
        }
    }
    for (int half = 1; half < n; half *= 2) {
        const int step = n / (half * 2);
        for (int i = 0; i < n; i += half * 2) {
            int k = 0;
            for (int j = i; j < i + half; ++j) {
                const int l = j + half;
                const float tre = re[l] * cosTable_[k] + im[l] * sinTable_[k];
                const float tim = -re[l] * sinTable_[k] + im[l] * cosTable_[k];
                re[l] = re[j] - tre;
                im[l] = im[j] - tim;
                re[j] += tre;
                im[j] += tim;
                k += step;
            }
        }
    }
}

void Fft::magnitudes(const float* input, int inputCount, const float* window,
                     float* re, float* im, float* out) const {
    for (int i = 0; i < size_; ++i) {
        re[i] = i < inputCount ? input[i] * window[i] : 0.0f;
        im[i] = 0.0f;
    }
    transform(re, im);
    const int bins = size_ / 2;
    for (int i = 0; i < bins; ++i) {
        out[i] = std::sqrt(re[i] * re[i] + im[i] * im[i]);
    }
}

std::vector<float> Fft::hann(int size) {
    std::vector<float> w(static_cast<size_t>(size));
    for (int i = 0; i < size; ++i) {
        w[i] = static_cast<float>(0.5 * (1.0 - std::cos(2.0 * kPi * i / (size - 1))));
    }
    return w;
}

} // namespace cued
