// Port of BpmDetectorTest and BpmOctaveTest, plus the 120 BPM case the desktop
// engine test suite asks for (octave aware, +-2 BPM).
#include <cmath>
#include <vector>

#include "bpm.h"
#include "pcm.h"
#include "test_util.h"

namespace {
bool octaveNear(float got, float want, float tol) {
    return std::fabs(got - want) < tol || std::fabs(got - 2 * want) < 2 * tol || std::fabs(got * 2 - want) < tol;
}
}

int main() {
    using namespace cued;
    BpmDetector det;
    {
        BpmResult r = det.analyse(test::click(128.0f, 20.0f));
        CHECK_MSG(std::fabs(r.bpm - 128.0f) < 1.5f, "got %g", r.bpm);
        CHECK_MSG(r.confidence > 0.2f, "confidence %g", r.confidence);
    }
    {
        BpmResult r = det.analyse(test::click(90.0f, 20.0f, 44100, 0.25f));
        CHECK_MSG(std::fabs(r.bpm - 90.0f) < 1.5f, "got %g", r.bpm);
        CHECK_MSG(std::fabs(r.firstBeatSec - 0.25f) < 0.05f, "first beat %g", r.firstBeatSec);
    }
    {
        std::vector<float> silence(44100 * 5, 0.0f);
        BpmResult r = det.analyse(silence);
        CHECK(r.bpm == 0.0f);
        CHECK(r.confidence == 0.0f);
    }
    {
        // 170 bpm is not halved and 70 bpm is not doubled.
        BpmResult fast = det.analyse(test::click(170.0f, 20.0f, 44100, 0.0f, 1500, 250.0, 0.4));
        CHECK_MSG(std::fabs(fast.bpm - 170.0f) < 2.0f, "got %g", fast.bpm);
        BpmResult slow = det.analyse(test::click(70.0f, 20.0f, 44100, 0.0f, 1500, 250.0, 0.4));
        CHECK_MSG(std::fabs(slow.bpm - 70.0f) < 2.0f, "got %g", slow.bpm);
    }
    {
        // 120 BPM click, analysed the way cued_analyse_file does it: decimated
        // to 11025 Hz with hop = rate / 86.
        std::vector<float> pcm = test::click(120.0f, 20.0f);
        std::vector<float> low = pcm::decimate(pcm, 4);
        CHECK(low.size() == pcm.size() / 4);
        BpmDetector d2(11025, 60.0f, 200.0f, std::max(64, 11025 / 86));
        BpmResult r = d2.analyse(low);
        CHECK_MSG(octaveNear(r.bpm, 120.0f, 2.0f), "got %g", r.bpm);
        CHECK_MSG(std::fabs(r.bpm - 120.0f) < 2.0f, "got %g", r.bpm);
        CHECK(r.confidence > 0.2f);
        CHECK_MSG(std::fabs(r.firstBeatSec) < 0.05f, "first beat %g", r.firstBeatSec);
    }
    {
        // Pcm helpers.
        const int16_t stereo[] = {32767, -32768, 16384, 16384};
        float mono[2];
        pcm::shortsToMonoFloat(stereo, 2, mono, 2);
        CHECK_NEAR(mono[0], (32767.0f - 32768.0f) / 32768.0f / 2.0f, 1e-7f);
        CHECK_NEAR(mono[1], 0.5f, 1e-7f);
        std::vector<float> seq = {1, 3, 5, 7, 9};
        std::vector<float> dec = pcm::decimate(seq, 2);
        CHECK(dec.size() == 2);
        CHECK_NEAR(dec[0], 2.0f, 1e-7f);
        CHECK_NEAR(dec[1], 6.0f, 1e-7f);
        CHECK(pcm::decimate(seq, 1).size() == 5);
    }
    return test::finish("bpm");
}
