// Minimal test harness shared by the ctest executables: CHECK macros, a
// synthetic click track (same shape as BpmDetectorTest.kt) and a WAV writer.
#pragma once

#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <fstream>
#include <string>
#include <vector>

namespace test {

inline int g_failures = 0;
inline int g_checks = 0;

#define CHECK(cond)                                                                        \
    do {                                                                                   \
        ++test::g_checks;                                                                  \
        if (!(cond)) {                                                                     \
            ++test::g_failures;                                                            \
            std::fprintf(stderr, "%s:%d: CHECK failed: %s\n", __FILE__, __LINE__, #cond);  \
        }                                                                                  \
    } while (0)

#define CHECK_MSG(cond, ...)                                                               \
    do {                                                                                   \
        ++test::g_checks;                                                                  \
        if (!(cond)) {                                                                     \
            ++test::g_failures;                                                            \
            std::fprintf(stderr, "%s:%d: CHECK failed: %s: ", __FILE__, __LINE__, #cond);  \
            std::fprintf(stderr, __VA_ARGS__);                                             \
            std::fprintf(stderr, "\n");                                                    \
        }                                                                                  \
    } while (0)

#define CHECK_NEAR(a, b, eps) CHECK_MSG(std::fabs((a) - (b)) <= (eps), "%g vs %g", (double)(a), (double)(b))

inline int finish(const char* name) {
    std::printf("%s: %d checks, %d failures\n", name, g_checks, g_failures);
    return g_failures == 0 ? 0 : 1;
}

// java.util.Random-compatible generator so noise tests use the same numbers as the phone tests.
class JavaRandom {
public:
    explicit JavaRandom(int64_t seed) : seed_((seed ^ 0x5DEECE66DLL) & ((1LL << 48) - 1)) {}
    int next(int bits) {
        seed_ = (seed_ * 0x5DEECE66DLL + 0xBLL) & ((1LL << 48) - 1);
        return static_cast<int>(static_cast<int64_t>(seed_) >> (48 - bits));
    }
    float nextFloat() { return next(24) / static_cast<float>(1 << 24); }
private:
    int64_t seed_;
};

// BpmDetectorTest.click(): decaying sine bursts every beat.
inline std::vector<float> click(float bpm, float seconds, int sr = 44100, float offsetSec = 0.0f,
                                int burst = 2000, double decay = 300.0, double tone = 0.3) {
    const int n = static_cast<int>(seconds * sr);
    std::vector<float> pcm(static_cast<size_t>(n), 0.0f);
    const float period = 60.0f / bpm * sr;
    float t = offsetSec * sr;
    while (t < n) {
        const int start = static_cast<int>(t);
        for (int i = 0; i < burst; ++i) {
            const int idx = start + i;
            if (idx >= n) break;
            pcm[idx] += static_cast<float>(std::exp(-i / decay) * std::sin(i * tone));
        }
        t += period;
    }
    return pcm;
}

// Writes a 16-bit PCM RIFF/WAVE file from mono float samples duplicated to `channels`.
inline bool writeWav(const std::string& path, const std::vector<float>& mono, int sr, int channels = 2) {
    std::ofstream f(path, std::ios::binary);
    if (!f) return false;
    auto u32 = [&](uint32_t v) { f.write(reinterpret_cast<const char*>(&v), 4); };
    auto u16 = [&](uint16_t v) { f.write(reinterpret_cast<const char*>(&v), 2); };
    const uint32_t dataBytes = static_cast<uint32_t>(mono.size() * channels * 2);
    f.write("RIFF", 4); u32(36 + dataBytes); f.write("WAVE", 4);
    f.write("fmt ", 4); u32(16); u16(1); u16(static_cast<uint16_t>(channels)); u32(static_cast<uint32_t>(sr));
    u32(static_cast<uint32_t>(sr * channels * 2)); u16(static_cast<uint16_t>(channels * 2)); u16(16);
    f.write("data", 4); u32(dataBytes);
    for (float s : mono) {
        float c = s < -1.0f ? -1.0f : (s > 1.0f ? 1.0f : s);
        int16_t v = static_cast<int16_t>(std::lround(c * 32767.0f));
        for (int ch = 0; ch < channels; ++ch) f.write(reinterpret_cast<const char*>(&v), 2);
    }
    return static_cast<bool>(f);
}

inline std::string tempDir() {
    const char* d = std::getenv("CUED_TEST_TMP");
    if (d && *d) return d;
    d = std::getenv("TMPDIR");
    return d && *d ? d : "/tmp";
}

} // namespace test
