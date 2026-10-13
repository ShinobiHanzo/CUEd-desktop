// Port of dev.cued.core.dsp.Spectrogram / SpectrogramImage / StreamingSpectrogram.
// A short-time Fourier transform reduced to a fixed number of log-spaced bands,
// each value dB-scaled to 0..1 with an -80 dB floor.
#pragma once

#include <functional>
#include <cstddef>
#include <vector>

#include "fft.h"

namespace cued {

// A static spectrogram: `frames` columns of `bands` values in 0..1, row-major.
struct SpectrogramImage {
    int frames = 0;
    int bands = 0;
    std::vector<float> data;

    float at(int frame, int band) const { return data[static_cast<size_t>(frame) * bands + band]; }

    // Resamples to `columns` by max-pooling frames (no-op when columns >= frames).
    SpectrogramImage downsample(int columns) const;

    // Per-column loudness in 0..1: the mean of the bands.
    std::vector<float> envelope() const;
};

class Spectrogram {
public:
    Spectrogram(int fftSize = 2048, int hopSize = 1024, int bands = 64, int sampleRate = 44100,
                float minHz = 40.0f, float maxHz = 16000.0f);

    int fftSize() const { return fftSize_; }
    int hopSize() const { return hopSize_; }
    int bands() const { return bands_; }
    const std::vector<int>& bandEdges() const { return bandEdges_; }

    // Computes bands() values for one frame starting at `offset` of pcm[0..count).
    // Not thread-safe (uses internal scratch buffers).
    void frame(const float* pcm, int count, int offset, float* out);

    // Converts a whole mono track to a frames x bands image.
    SpectrogramImage analyse(const float* pcm, int count);

    // Maps FFT power to 0..1 on a dB scale with an -80 dB floor.
    static float powerToUnit(float power);

private:
    int fftSize_, hopSize_, bands_;
    Fft fft_;
    std::vector<float> window_, re_, im_, mags_;
    std::vector<int> bandEdges_;
};

// Feed PCM in arbitrary chunk sizes and receive spectrogram columns as they
// become available.
class StreamingSpectrogram {
public:
    using FrameCallback = std::function<void(const float* bands, int count)>;

    StreamingSpectrogram(Spectrogram& spectrogram, FrameCallback onFrame);

    void push(const float* samples, int count);
    // Flushes the tail as a final zero-padded frame if more than a hop is left.
    void finish();
    int framesEmitted() const { return framesEmitted_; }

private:
    void drain();

    Spectrogram& spec_;
    FrameCallback onFrame_;
    int fftSize_, hop_;
    std::vector<float> buffer_;
    int filled_ = 0;
    std::vector<float> out_;
    int framesEmitted_ = 0;
};

} // namespace cued
