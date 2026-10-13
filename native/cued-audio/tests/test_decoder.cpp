// Decoder round trip on a generated WAV: format, length, samples, seek, and
// the analysis helpers on top of it.
#include <cmath>
#include <cstdio>
#include <string>
#include <vector>

#include "../include/cued_audio.h"
#include "analysis.h"
#include "decoder.h"
#include "test_util.h"

int main() {
    using namespace cued;
    const std::string dir = test::tempDir();
    const std::string wav = dir + "/cued_test_decoder.wav";
    const int sr = 44100;
    std::vector<float> tone(sr * 2);
    for (size_t i = 0; i < tone.size(); ++i) tone[i] = static_cast<float>(0.5 * std::sin(2.0 * M_PI * 440.0 * i / sr));
    CHECK(test::writeWav(wav, tone, sr, 2));

    {
        std::string err;
        auto d = Decoder::open(wav, 0, 0, Decoder::Format::S16, err);
        CHECK_MSG(d != nullptr, "%s", err.c_str());
        if (d) {
            CHECK(d->sampleRate() == sr);
            CHECK(d->channels() == 2);
            CHECK(d->sourceSampleRate() == sr);
            CHECK(d->sourceChannels() == 2);
            CHECK(!d->usedFfmpeg());
            CHECK_MSG(d->lengthFrames() == tone.size(), "%llu", (unsigned long long)d->lengthFrames());
            CHECK_NEAR(d->durationSec(), 2.0, 1e-6);
            std::vector<int16_t> buf(1000 * 2);
            uint64_t got = d->read(buf.data(), 1000);
            CHECK(got == 1000);
            for (int i = 0; i < 1000; ++i) {
                const int16_t want = static_cast<int16_t>(std::lround(tone[i] * 32767.0f));
                CHECK(std::abs(buf[2 * i] - want) <= 1);
                CHECK(buf[2 * i] == buf[2 * i + 1]);
            }
            // Read to the end and count.
            uint64_t total = got;
            for (;;) {
                uint64_t n = d->read(buf.data(), 1000);
                total += n;
                if (n < 1000) break;
            }
            CHECK_MSG(total == tone.size(), "%llu", (unsigned long long)total);
            CHECK(d->atEnd());
            CHECK(d->seek(sr));
            CHECK(!d->atEnd());
            CHECK(d->cursor() == static_cast<uint64_t>(sr));
            got = d->read(buf.data(), 10);
            CHECK(got == 10);
            CHECK(std::abs(buf[0] - static_cast<int16_t>(std::lround(tone[sr] * 32767.0f))) <= 1);
        }
    }
    {
        // Resampled, f32 output.
        std::string err;
        auto d = Decoder::open(wav, 48000, 2, Decoder::Format::F32, err);
        CHECK_MSG(d != nullptr, "%s", err.c_str());
        if (d) {
            CHECK(d->sampleRate() == 48000);
            CHECK(d->sourceSampleRate() == sr);
            CHECK_NEAR(d->durationSec(), 2.0, 0.01);
            std::vector<float> buf(4800 * 2);
            uint64_t got = d->read(buf.data(), 4800);
            CHECK(got == 4800);
            float peak = 0.0f;
            for (float v : buf) peak = std::max(peak, std::fabs(v));
            CHECK_MSG(peak > 0.45f && peak < 0.55f, "peak %g", peak);
        }
    }
    {
        // Missing file: both back ends fail, error mentions both.
        std::string err;
        auto d = Decoder::open(dir + "/does_not_exist.wav", 0, 0, Decoder::Format::F32, err);
        CHECK(d == nullptr);
        CHECK(!err.empty());
        cued_analysis a;
        CHECK(cued_analyse_file((dir + "/does_not_exist.wav").c_str(), &a) == CUED_ERR_DECODE);
        CHECK(std::string(cued_last_error()).find("does_not_exist") != std::string::npos);
    }
    {
        // Full analysis through the C ABI on a 120 BPM click track.
        const std::string clickWav = dir + "/cued_test_click120.wav";
        CHECK(test::writeWav(clickWav, test::click(120.0f, 20.0f), sr, 2));
        cued_analysis a;
        int rc = cued_analyse_file(clickWav.c_str(), &a);
        CHECK_MSG(rc == 0, "%s", cued_last_error());
        CHECK_MSG(std::fabs(a.bpm - 120.0f) < 2.0f, "bpm %g", a.bpm);
        CHECK(a.confidence > 0.2f);
        CHECK_NEAR(a.duration_sec, 20.0, 0.01);
        CHECK(a.sample_rate == sr);
        CHECK(a.channels == 2);
        CHECK(std::fabs(a.first_beat_sec) < 0.05f);

        // Envelope: 64 columns in 0..1, louder where the clicks are.
        float env[64];
        CHECK(cued_envelope_file(clickWav.c_str(), env, 64) == 0);
        float mx = 0.0f, mn = 1.0f;
        for (float v : env) { mx = std::max(mx, v); mn = std::min(mn, v); CHECK(v >= 0.0f && v <= 1.0f); }
        CHECK(mx > 0.0f);
        // Envelope of silence is zero.
        const std::string silentWav = dir + "/cued_test_silence.wav";
        CHECK(test::writeWav(silentWav, std::vector<float>(sr, 0.0f), sr, 1));
        float env2[8];
        CHECK(cued_envelope_file(silentWav.c_str(), env2, 8) == 0);
        for (float v : env2) CHECK(v == 0.0f);
        // Very short file: fewer frames than columns is stretched, not an error.
        const std::string shortWav = dir + "/cued_test_short.wav";
        CHECK(test::writeWav(shortWav, std::vector<float>(1000, 0.1f), sr, 1));
        float env3[16];
        CHECK(cued_envelope_file(shortWav.c_str(), env3, 16) == 0);
        std::remove(clickWav.c_str());
        std::remove(silentWav.c_str());
        std::remove(shortWav.c_str());
    }
    {
        // ffmpeg fallback, exercised when ffmpeg can make an m4a for us.
        const std::string m4a = dir + "/cued_test_tone.m4a";
        std::string cmd = "ffmpeg -nostdin -v error -y -f lavfi -i \"sine=frequency=440:duration=3\" -c:a aac \"" + m4a + "\" 2>/dev/null";
        if (std::system(cmd.c_str()) == 0) {
            std::string err;
            auto d = Decoder::open(m4a, 48000, 2, Decoder::Format::F32, err);
            CHECK_MSG(d != nullptr, "%s", err.c_str());
            if (d) {
                CHECK(d->usedFfmpeg());
                CHECK(d->sampleRate() == 48000);
                CHECK(d->channels() == 2);
                std::vector<float> buf(48000 * 2);
                uint64_t total = 0;
                for (int guard = 0; guard < 100; ++guard) {
                    uint64_t n = d->read(buf.data(), 48000);
                    total += n;
                    if (d->atEnd()) break;
                }
                CHECK_MSG(total > 48000 * 2 && total < 48000 * 4, "total %llu", (unsigned long long)total);
                CHECK_NEAR(d->durationSec(), 3.0, 0.2);
            }
            cued_analysis a;
            CHECK_MSG(cued_analyse_file(m4a.c_str(), &a) == 0, "%s", cued_last_error());
            CHECK_NEAR(a.duration_sec, 3.0, 0.2);
            CHECK(a.sample_rate == 44100);
            std::remove(m4a.c_str());

            // Longer than the 30 s ring: reading all of it must not grow past the window, and a
            // seek beyond what is buffered restarts ffmpeg at the right place.
            const std::string longM4a = dir + "/cued_test_long.m4a";
            cmd = "ffmpeg -nostdin -v error -y -f lavfi -i \"sine=frequency=440:duration=70\" -c:a aac \"" + longM4a + "\" 2>/dev/null";
            if (std::system(cmd.c_str()) == 0) {
                auto d2 = Decoder::open(longM4a, 48000, 2, Decoder::Format::F32, err);
                CHECK_MSG(d2 != nullptr, "%s", err.c_str());
                if (d2) {
                    std::vector<float> buf(48000 * 2);
                    uint64_t got = d2->read(buf.data(), 48000);
                    CHECK(got > 0);
                    // Forward past the ring: 60 s in.
                    CHECK(d2->seek(48000ull * 60));
                    uint64_t tail = 0;
                    for (int guard = 0; guard < 40; ++guard) {
                        uint64_t n = d2->read(buf.data(), 48000);
                        tail += n;
                        if (d2->atEnd()) break;
                    }
                    CHECK_MSG(tail > 48000ull * 8 && tail < 48000ull * 12, "tail after seek %llu", (unsigned long long)tail);
                    CHECK_NEAR(static_cast<double>(d2->lengthFrames()) / 48000.0, 70.0, 1.0);
                    // Back to the start: a backwards seek is a restart too.
                    CHECK(d2->seek(0));
                    CHECK(!d2->atEnd());
                    // read() never blocks the audio thread for more than 20 ms, so right after a
                    // restart it hands back silence until ffmpeg has produced the first frames.
                    uint64_t afterRestart = 0;
                    for (int guard = 0; guard < 250 && afterRestart == 0; ++guard) afterRestart = d2->read(buf.data(), 48000);
                    CHECK(afterRestart > 0);
                    CHECK_MSG(d2->cursor() >= afterRestart && d2->cursor() < 48000ull * 2, "cursor %llu", (unsigned long long)d2->cursor());
                }
                std::remove(longM4a.c_str());
            }
        } else {
            std::printf("ffmpeg not available; skipping m4a fallback check\n");
        }
    }
    std::remove(wav.c_str());
    return test::finish("decoder");
}
