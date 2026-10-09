// Port of FftTest.`spectrogram of silence is zero and of noise is not` and
// StreamingSpectrogramTest, plus band-edge checks.
#include <algorithm>
#include <vector>

#include "spectrogram.h"
#include "test_util.h"

int main() {
    using namespace cued;
    {
        Spectrogram spec(512, 256, 16);
        std::vector<float> silence(4096, 0.0f);
        SpectrogramImage img = spec.analyse(silence.data(), static_cast<int>(silence.size()));
        CHECK(std::all_of(img.data.begin(), img.data.end(), [](float v) { return v == 0.0f; }));
        test::JavaRandom rnd(1);
        std::vector<float> noise(4096);
        for (auto& v : noise) v = (rnd.nextFloat() * 2.0f - 1.0f) * 0.5f;
        SpectrogramImage img2 = spec.analyse(noise.data(), static_cast<int>(noise.size()));
        CHECK(std::any_of(img2.data.begin(), img2.data.end(), [](float v) { return v > 0.2f; }));
        SpectrogramImage down = img2.downsample(4);
        CHECK(down.frames == 4);
        CHECK(down.bands == 16);
        CHECK(down.envelope().size() == 4);
    }
    {
        // Band edges: monotonic, clamped to 1..fftSize/2-1, log spaced between
        // max(minHz, hzPerBin) and min(maxHz, nyquist).
        Spectrogram spec(2048, 1024, 64, 44100, 40.0f, 16000.0f);
        const auto& e = spec.bandEdges();
        CHECK(e.size() == 65);
        CHECK(e.front() >= 1);
        CHECK(e.back() <= 1023);
        for (size_t i = 1; i < e.size(); ++i) CHECK(e[i] >= e[i - 1]);
        const float hzPerBin = 22050.0f / 1024.0f;
        CHECK(e[0] == static_cast<int>(40.0f / hzPerBin));     // 1
        CHECK(e[64] == static_cast<int>(16000.0f / hzPerBin)); // 743
        // Low rate: hi is clamped to nyquist.
        Spectrogram low(512, 256, 8, 8000, 40.0f, 16000.0f);
        CHECK(low.bandEdges().back() <= 255);
        CHECK(low.bandEdges().back() == 255 || low.bandEdges().back() == static_cast<int>(4000.0f / (4000.0f / 256.0f)) - 1);
    }
    {
        // powerToUnit: -80 dB floor, +60 dB normalisation, clamped to 0..1.
        CHECK(Spectrogram::powerToUnit(0.0f) == 0.0f);
        CHECK(Spectrogram::powerToUnit(1e-13f) == 0.0f);
        CHECK_NEAR(Spectrogram::powerToUnit(1e6f), 1.0f, 1e-6f);          // 60 dB -> 0 dB -> 1
        CHECK_NEAR(Spectrogram::powerToUnit(1e2f), 0.5f, 1e-5f);          // 20 dB -> -40 dB -> 0.5
        CHECK_NEAR(Spectrogram::powerToUnit(1e-2f), 0.0f, 1e-6f);         // -20 dB -> -80 dB -> 0
        CHECK(Spectrogram::powerToUnit(1e9f) == 1.0f);
    }
    {
        // Streaming matches batch analysis.
        Spectrogram spec(256, 128, 8);
        test::JavaRandom rnd(7);
        std::vector<float> pcm(10000);
        for (auto& v : pcm) v = (rnd.nextFloat() * 2.0f - 1.0f) * 0.3f;
        SpectrogramImage batch = spec.analyse(pcm.data(), static_cast<int>(pcm.size()));
        std::vector<std::vector<float>> frames;
        StreamingSpectrogram streaming(spec, [&](const float* b, int n) { frames.emplace_back(b, b + n); });
        size_t i = 0;
        while (i < pcm.size()) {
            const size_t n = std::min<size_t>(333, pcm.size() - i);
            streaming.push(pcm.data() + i, static_cast<int>(n));
            i += n;
        }
        CHECK_MSG(static_cast<int>(frames.size()) == batch.frames, "%zu vs %d", frames.size(), batch.frames);
        for (int f = 0; f < batch.frames && f < static_cast<int>(frames.size()); ++f)
            for (int b = 0; b < 8; ++b) CHECK_NEAR(batch.at(f, b), frames[f][b], 1e-5f);
        CHECK(streaming.framesEmitted() == batch.frames);
    }
    {
        // A pure tone lands in the expected band and nowhere else.
        const int sr = 44100;
        Spectrogram spec(2048, 1024, 64, sr);
        std::vector<float> tone(8192);
        const float freq = 1000.0f;
        for (size_t i = 0; i < tone.size(); ++i) tone[i] = static_cast<float>(0.5 * std::sin(2.0 * M_PI * freq * i / sr));
        std::vector<float> out(64);
        spec.frame(tone.data(), static_cast<int>(tone.size()), 0, out.data());
        const int bin = static_cast<int>(freq / (22050.0f / 1024.0f));
        const auto& e = spec.bandEdges();
        int expected = -1;
        for (int b = 0; b < 64; ++b) if (bin >= e[b] && bin < std::max(e[b] + 1, e[b + 1])) { expected = b; break; }
        const int peak = static_cast<int>(std::max_element(out.begin(), out.end()) - out.begin());
        CHECK_MSG(std::abs(peak - expected) <= 1, "peak band %d expected %d", peak, expected);
        CHECK(out[peak] > 0.5f);
        CHECK(out[0] < out[peak]);
    }
    return test::finish("spectrogram");
}
