// Port of VocalCutTest: a centred voice is cancelled, centred bass survives,
// wide stereo survives, strength blends, and the engine switch applies it.
#include <cmath>
#include <cstdio>
#include <cstring>
#include <vector>

#include "cued_audio.h"
#include "engine.h"
#include "karaoke.h"
#include "test_util.h"

namespace {

const int kSr = 48000;
const double kPi = 3.14159265358979323846;

double rms(const std::vector<float>& stereo, int channel, int fromFrame = 0) {
    double acc = 0.0;
    int n = 0;
    for (size_t i = static_cast<size_t>(fromFrame) * 2 + channel; i < stereo.size(); i += 2) {
        acc += static_cast<double>(stereo[i]) * stereo[i];
        ++n;
    }
    return n ? std::sqrt(acc / n) : 0.0;
}

} // namespace

int main() {
    using namespace cued;
    {
        // Centred 1 kHz voice (gain 0.6) + centred 60 Hz bass (gain 0.3).
        std::vector<float> pcm(static_cast<size_t>(kSr) * 2);
        for (int i = 0; i < kSr; ++i) {
            const float v = 0.6f * static_cast<float>(std::sin(2 * kPi * 1000.0 * i / kSr)) + 0.3f * static_cast<float>(std::sin(2 * kPi * 60.0 * i / kSr));
            pcm[2 * i] = v;
            pcm[2 * i + 1] = v;
        }
        const double before = rms(pcm, 0);
        VocalCut cut(kSr);
        cut.process(pcm.data(), kSr);
        const double after = rms(pcm, 0, kSr / 10);
        const double bassOnly = 0.3 / std::sqrt(2.0);
        CHECK_MSG(after < before * 0.6, "after %g before %g", after, before);
        CHECK_MSG(after > bassOnly * 0.7 && after < bassOnly * 1.1, "after %g expected ~%g", after, bassOnly);
    }
    {
        // Hard-left source: left keeps most of it, right is no longer silent (inverted side).
        std::vector<float> pcm(static_cast<size_t>(kSr) / 2 * 2, 0.0f);
        for (int i = 0; i < kSr / 2; ++i) pcm[2 * i] = 0.5f * static_cast<float>(std::sin(2 * kPi * 440.0 * i / kSr));
        const double ref = rms(pcm, 0);
        VocalCut cut(kSr);
        cut.process(pcm.data(), kSr / 2);
        CHECK_MSG(rms(pcm, 0, kSr / 10) > ref * 0.4, "left %g", rms(pcm, 0, kSr / 10));
        CHECK_MSG(rms(pcm, 1, kSr / 10) > ref * 0.3, "right %g", rms(pcm, 1, kSr / 10));
    }
    {
        // Half strength leaves roughly half the voice; zero strength is a no-op.
        std::vector<float> pcm(static_cast<size_t>(kSr) / 2 * 2);
        for (int i = 0; i < kSr / 2; ++i) pcm[2 * i] = pcm[2 * i + 1] = 0.5f * static_cast<float>(std::sin(2 * kPi * 1000.0 * i / kSr));
        const double ref = rms(pcm, 0);
        VocalCut half(kSr);
        half.setStrength(0.5f);
        half.process(pcm.data(), kSr / 2);
        const double got = rms(pcm, 0, kSr / 10);
        CHECK_MSG(got > ref * 0.4 && got < ref * 0.6, "half %g of %g", got, ref);

        std::vector<float> same = {0.3f, -0.2f, 0.1f, 0.1f};
        VocalCut off(kSr);
        off.setStrength(0.0f);
        off.process(same.data(), 2);
        CHECK(same[0] == 0.3f && same[1] == -0.2f && same[2] == 0.1f && same[3] == 0.1f);
        off.setStrength(7.0f);
        CHECK(off.strength() == 1.0f);
    }
    {
        // Through the engine: a centred tone with karaoke on comes out much quieter in the spectrum peak.
        const std::string dir = test::tempDir();
        const std::string wav = dir + "/cued_karaoke_tone.wav";
        std::vector<float> tone(static_cast<size_t>(kSr) * 3);
        for (size_t i = 0; i < tone.size(); ++i) tone[i] = 0.5f * static_cast<float>(std::sin(2 * kPi * 1000.0 * i / kSr));
        CHECK(test::writeWav(wav, tone, kSr, 2));
        {
            Engine engine(kSr, false);
            CHECK_MSG(engine.load(wav, true) == 0, "%s", cued_last_error());
            std::vector<float> out(static_cast<size_t>(kSr) / 10 * 2);
            engine.mix(out.data(), kSr / 10);
            const double off = rms(out, 0);
            engine.setKaraoke(true, 1.0f);
            engine.mix(out.data(), kSr / 10); // the low-pass settles
            engine.mix(out.data(), kSr / 10);
            const double on = rms(out, 0);
            CHECK_MSG(on < off * 0.1, "karaoke on %g off %g", on, off);
            engine.setKaraoke(false, 1.0f);
            engine.mix(out.data(), kSr / 10);
            CHECK_MSG(rms(out, 0) > off * 0.9, "back off %g vs %g", rms(out, 0), off);
        }
        // The C ABI reaches the same switch.
        cued_engine* e = cued_engine_create(kSr, 0);
        CHECK(e != nullptr);
        if (e) {
            cued_engine_set_karaoke(e, 1, 0.5f);
            cued_engine_set_karaoke(e, 0, 0.0f);
            cued_engine_destroy(e);
        }
        std::remove(wav.c_str());
    }
    return test::finish("karaoke");
}
