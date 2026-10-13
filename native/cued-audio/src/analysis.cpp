#include "analysis.h"
#include <cstddef>

#include <algorithm>
#include <cstdint>
#include <vector>

#include "decoder.h"
#include "pcm.h"
#include "spectrogram.h"

namespace cued {

namespace {

constexpr int kAnalysisRate = 11025;      // AnalysisQueue.RATE
constexpr int kBpmMaxSeconds = 60 * 8;    // AnalysisQueue.BPM_MAX_SECONDS
constexpr int kChunkFrames = 8192;

// Streams the file as mono float at the source rate; calls onChunk(samples, n).
template <typename F>
bool decodeMono(const std::string& path, std::string& error, int& sampleRate, int& channels,
                uint64_t& framesDecoded, uint64_t maxFrames, F onChunk) {
    auto dec = Decoder::open(path, 0, 0, Decoder::Format::S16, error);
    if (!dec) return false;
    sampleRate = dec->sampleRate();
    channels = dec->channels();
    if (sampleRate <= 0 || channels <= 0) {
        error = "decoder reported no format";
        return false;
    }
    std::vector<int16_t> raw(static_cast<size_t>(kChunkFrames) * channels);
    std::vector<float> mono(kChunkFrames);
    framesDecoded = 0;
    for (;;) {
        uint64_t want = kChunkFrames;
        if (maxFrames != 0 && framesDecoded + want > maxFrames) want = maxFrames - framesDecoded;
        if (want == 0) break;
        const uint64_t got = dec->read(raw.data(), want);
        if (got > 0) {
            pcm::shortsToMonoFloat(raw.data(), channels, mono.data(), static_cast<int>(got));
            onChunk(mono.data(), static_cast<int>(got));
            framesDecoded += got;
        }
        if (dec->atEnd()) break;
        if (got == 0 && !dec->usedFfmpeg()) break;
    }
    return true;
}

} // namespace

bool analyseFile(const std::string& path, FileAnalysis& out, std::string& error) {
    int sr = 0, ch = 0;
    uint64_t frames = 0;
    std::vector<float> pcm;
    int factor = 1;
    float acc = 0.0f;
    int accCount = 0;
    // Like AnalysisQueue: keep at most RATE * BPM_MAX_SECONDS decimated samples.
    const size_t cap = static_cast<size_t>(kAnalysisRate) * kBpmMaxSeconds;
    bool first = true;
    auto ok = decodeMono(path, error, sr, ch, frames, 0, [&](const float* s, int n) {
        if (first) {
            factor = std::max(1, sr / kAnalysisRate);
            pcm.reserve(static_cast<size_t>(sr / factor) * 60);
            first = false;
        }
        for (int i = 0; i < n; ++i) {
            acc += s[i];
            if (++accCount == factor) {
                if (pcm.size() < cap) pcm.push_back(acc / static_cast<float>(factor));
                acc = 0.0f;
                accCount = 0;
            }
        }
    });
    if (!ok) return false;
    const int rate = sr / factor;
    BpmDetector det(rate, 60.0f, 200.0f, std::max(64, rate / 86));
    out.bpm = det.analyse(pcm);
    out.durationSec = static_cast<double>(frames) / sr;
    out.sampleRate = sr;
    out.channels = ch;
    out.analysisRate = rate;
    return true;
}

bool envelopeFile(const std::string& path, int columns, std::vector<float>& out, std::string& error) {
    if (columns <= 0) {
        error = "columns must be > 0";
        return false;
    }
    int sr = 0, ch = 0;
    uint64_t frames = 0;
    std::vector<float> image;
    int bands = 64;
    Spectrogram* spec = nullptr;
    StreamingSpectrogram* streaming = nullptr;
    std::vector<float> tail;
    auto ok = decodeMono(path, error, sr, ch, frames, 0, [&](const float* s, int n) {
        if (!spec) {
            spec = new Spectrogram(2048, 1024, bands, sr);
            streaming = new StreamingSpectrogram(*spec, [&](const float* b, int count) {
                image.insert(image.end(), b, b + count);
            });
        }
        streaming->push(s, n);
    });
    if (streaming) streaming->finish();
    delete streaming;
    delete spec;
    if (!ok) return false;
    SpectrogramImage img;
    img.bands = bands;
    img.frames = static_cast<int>(image.size() / static_cast<size_t>(bands));
    if (img.frames == 0) {
        out.assign(static_cast<size_t>(columns), 0.0f);
        return true;
    }
    img.data = std::move(image);
    std::vector<float> env = img.downsample(columns).envelope();
    if (static_cast<int>(env.size()) == columns) {
        out = std::move(env);
    } else {
        // Fewer frames than columns: stretch with nearest neighbour.
        out.resize(static_cast<size_t>(columns));
        for (int c = 0; c < columns; ++c) {
            out[c] = env[static_cast<size_t>(static_cast<long long>(c) * env.size() / columns)];
        }
    }
    return true;
}

} // namespace cued
