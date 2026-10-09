#include "spectrogram.h"

#include <algorithm>
#include <cmath>
#include <cstring>

namespace cued {

namespace {
constexpr float kFloorDb = -80.0f;
}

Spectrogram::Spectrogram(int fftSize, int hopSize, int bands, int sampleRate, float minHz, float maxHz)
    : fftSize_(fftSize), hopSize_(hopSize), bands_(bands), fft_(fftSize),
      window_(Fft::hann(fftSize)), re_(fftSize), im_(fftSize), mags_(fftSize / 2),
      bandEdges_(bands + 1) {
    const float nyquist = static_cast<float>(sampleRate) / 2.0f;
    const float hzPerBin = nyquist / static_cast<float>(fftSize / 2);
    const float lo = std::max(minHz, hzPerBin);
    const float hi = std::min(maxHz, nyquist);
    const double ratio = std::pow(static_cast<double>(hi / lo), 1.0 / bands);
    for (int i = 0; i <= bands; ++i) {
        const float hz = static_cast<float>(lo * std::pow(ratio, static_cast<double>(i)));
        int bin = static_cast<int>(hz / hzPerBin);
        bandEdges_[i] = std::min(std::max(bin, 1), fftSize / 2 - 1);
    }
}

float Spectrogram::powerToUnit(float power) {
    if (power <= 1e-12f) return 0.0f;
    const float db = 10.0f * std::log10(power) - 60.0f; // normalise for windowed FFT of unit signals
    const float v = (db - kFloorDb) / -kFloorDb;
    return std::min(std::max(v, 0.0f), 1.0f);
}

void Spectrogram::frame(const float* pcm, int count, int offset, float* out) {
    const int end = std::min(offset + fftSize_, count);
    for (int i = 0; i < fftSize_; ++i) {
        const int idx = offset + i;
        re_[i] = idx < end ? pcm[idx] * window_[i] : 0.0f;
        im_[i] = 0.0f;
    }
    fft_.transform(re_.data(), im_.data());
    for (size_t i = 0; i < mags_.size(); ++i) mags_[i] = re_[i] * re_[i] + im_[i] * im_[i];
    for (int b = 0; b < bands_; ++b) {
        const int from = bandEdges_[b];
        const int to = std::max(from + 1, bandEdges_[b + 1]);
        float acc = 0.0f;
        for (int k = from; k < to; ++k) acc += mags_[k];
        const float power = acc / static_cast<float>(to - from);
        out[b] = powerToUnit(power);
    }
}

SpectrogramImage Spectrogram::analyse(const float* pcm, int count) {
    const int frames = std::max(1, (count - fftSize_) / hopSize_ + 1);
    SpectrogramImage img;
    img.frames = frames;
    img.bands = bands_;
    img.data.resize(static_cast<size_t>(frames) * bands_);
    for (int f = 0; f < frames; ++f) {
        frame(pcm, count, f * hopSize_, img.data.data() + static_cast<size_t>(f) * bands_);
    }
    return img;
}

SpectrogramImage SpectrogramImage::downsample(int columns) const {
    if (columns >= frames) return *this;
    SpectrogramImage out;
    out.frames = columns;
    out.bands = bands;
    out.data.resize(static_cast<size_t>(columns) * bands);
    for (int c = 0; c < columns; ++c) {
        const int from = static_cast<int>(static_cast<long long>(c) * frames / columns);
        const int to = std::max(from + 1, static_cast<int>(static_cast<long long>(c + 1) * frames / columns));
        for (int b = 0; b < bands; ++b) {
            float m = 0.0f;
            for (int f = from; f < to; ++f) m = std::max(m, at(f, b));
            out.data[static_cast<size_t>(c) * bands + b] = m;
        }
    }
    return out;
}

std::vector<float> SpectrogramImage::envelope() const {
    std::vector<float> env(static_cast<size_t>(frames));
    for (int f = 0; f < frames; ++f) {
        float acc = 0.0f;
        for (int b = 0; b < bands; ++b) acc += at(f, b);
        env[f] = acc / static_cast<float>(bands);
    }
    return env;
}

StreamingSpectrogram::StreamingSpectrogram(Spectrogram& spectrogram, FrameCallback onFrame)
    : spec_(spectrogram), onFrame_(std::move(onFrame)), fftSize_(spectrogram.fftSize()),
      hop_(spectrogram.hopSize()), buffer_(static_cast<size_t>(spectrogram.fftSize()) * 4),
      out_(static_cast<size_t>(spectrogram.bands())) {}

void StreamingSpectrogram::push(const float* samples, int count) {
    int offset = 0;
    while (offset < count) {
        const int n = std::min(count - offset, static_cast<int>(buffer_.size()) - filled_);
        std::memcpy(buffer_.data() + filled_, samples + offset, sizeof(float) * static_cast<size_t>(n));
        filled_ += n;
        offset += n;
        drain();
    }
}

void StreamingSpectrogram::drain() {
    int pos = 0;
    while (pos + fftSize_ <= filled_) {
        spec_.frame(buffer_.data(), filled_, pos, out_.data());
        onFrame_(out_.data(), static_cast<int>(out_.size()));
        ++framesEmitted_;
        pos += hop_;
    }
    if (pos > 0) {
        std::memmove(buffer_.data(), buffer_.data() + pos, sizeof(float) * static_cast<size_t>(filled_ - pos));
        filled_ -= pos;
    }
}

void StreamingSpectrogram::finish() {
    if (filled_ > hop_) {
        spec_.frame(buffer_.data(), filled_, 0, out_.data());
        onFrame_(out_.data(), static_cast<int>(out_.size()));
        ++framesEmitted_;
    }
    filled_ = 0;
}

} // namespace cued
