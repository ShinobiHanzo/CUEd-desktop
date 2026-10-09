// Engine with the null output: load two files, crossfade, status/position
// advance when ticked, spectrum, seek, pause/stop, volume.
#include <cmath>
#include <cstdio>
#include <cstring>
#include <string>
#include <vector>

#include "../include/cued_audio.h"
#include "test_util.h"

int main() {
    const std::string dir = test::tempDir();
    const int sr = 48000;
    const std::string a = dir + "/cued_test_engine_a.wav";
    const std::string b = dir + "/cued_test_engine_b.wav";
    std::vector<float> toneA(sr * 4), toneB(sr * 4);
    for (size_t i = 0; i < toneA.size(); ++i) {
        toneA[i] = static_cast<float>(0.5 * std::sin(2.0 * M_PI * 220.0 * i / sr));
        toneB[i] = static_cast<float>(0.5 * std::sin(2.0 * M_PI * 3000.0 * i / sr));
    }
    CHECK(test::writeWav(a, toneA, sr, 2));
    CHECK(test::writeWav(b, toneB, sr, 2));

    CHECK(std::strlen(cued_audio_version()) > 0);

    cued_engine* e = cued_engine_create(sr, 0);
    CHECK(e != nullptr);
    CHECK(cued_engine_has_device(e) == 0);
    cued_status st;
    cued_engine_status(e, &st);
    CHECK(st.loaded == 0 && st.playing == 0 && st.path[0] == '\0');
    CHECK(cued_engine_play(e) == CUED_ERR_STATE);

    // Load and autoplay.
    CHECK_MSG(cued_engine_load(e, a.c_str(), 1) == 0, "%s", cued_last_error());
    cued_engine_status(e, &st);
    CHECK(st.loaded == 1 && st.playing == 1 && !st.crossfading);
    CHECK(std::string(st.path) == a);
    CHECK_NEAR(st.duration_sec, 4.0, 1e-6);
    CHECK_NEAR(st.position_sec, 0.0, 1e-9);

    // Ticking advances the position by exactly the frames rendered.
    cued_engine_tick(e, sr / 2);
    cued_engine_status(e, &st);
    CHECK_NEAR(st.position_sec, 0.5, 1e-9);
    cued_engine_tick(e, sr / 2);
    cued_engine_status(e, &st);
    CHECK_NEAR(st.position_sec, 1.0, 1e-9);

    // The spectrum of a 220 Hz tone sits in the low bands.
    float spec[64];
    CHECK(cued_engine_spectrum(e, spec, 64) == 64);
    int peak = 0;
    for (int i = 1; i < 64; ++i) if (spec[i] > spec[peak]) peak = i;
    CHECK_MSG(peak < 32, "peak band %d", peak);
    CHECK(spec[peak] > 0.3f);
    CHECK(cued_engine_spectrum(e, spec, 8) == 8);

    // Pause freezes, play resumes.
    CHECK(cued_engine_pause(e) == 0);
    cued_engine_tick(e, sr);
    cued_engine_status(e, &st);
    CHECK(st.playing == 0);
    CHECK_NEAR(st.position_sec, 1.0, 1e-9);
    CHECK(cued_engine_play(e) == 0);

    // Seek.
    CHECK(cued_engine_seek(e, 2.0) == 0);
    cued_engine_status(e, &st);
    CHECK_NEAR(st.position_sec, 2.0, 1e-9);
    CHECK(cued_engine_seek(e, -1.0) == CUED_ERR_INVALID);

    // Load a second file while playing: a crossfade starts, the active path is
    // still the first file until the fade completes.
    cued_engine_set_crossfade(e, 1.0, CUED_CURVE_EQUAL_POWER);
    CHECK_MSG(cued_engine_load(e, b.c_str(), 1) == 0, "%s", cued_last_error());
    cued_engine_status(e, &st);
    CHECK(st.crossfading == 1);
    CHECK(std::string(st.path) == a);
    CHECK_NEAR(st.crossfade_progress, 0.0f, 1e-6f);
    cued_engine_tick(e, sr / 2);
    cued_engine_status(e, &st);
    CHECK(st.crossfading == 1);
    CHECK_NEAR(st.crossfade_progress, 0.5f, 1e-5f);
    CHECK(std::string(st.path) == a);
    CHECK_NEAR(st.position_sec, 2.5, 1e-9);
    cued_engine_tick(e, sr / 2);
    cued_engine_status(e, &st);
    CHECK(st.crossfading == 0);
    CHECK(std::string(st.path) == b);
    CHECK(st.playing == 1);
    // The incoming deck ran for the whole fade.
    CHECK_NEAR(st.position_sec, 1.0, 1e-9);
    CHECK_NEAR(st.crossfade_progress, 0.0f, 1e-6f);

    // Now the spectrum peaks high (3 kHz).
    cued_engine_tick(e, 4096);
    CHECK(cued_engine_spectrum(e, spec, 64) == 64);
    peak = 0;
    for (int i = 1; i < 64; ++i) if (spec[i] > spec[peak]) peak = i;
    CHECK_MSG(peak > 32, "peak band %d", peak);

    // Zero-length crossfade switches at once.
    cued_engine_set_crossfade(e, 0.0, CUED_CURVE_LINEAR);
    CHECK(cued_engine_load(e, a.c_str(), 1) == 0);
    cued_engine_status(e, &st);
    CHECK(st.crossfading == 0);
    CHECK(std::string(st.path) == a);
    CHECK_NEAR(st.position_sec, 0.0, 1e-9);

    // Playing to the end stops the engine with the position at the duration.
    cued_engine_tick(e, sr * 5);
    cued_engine_status(e, &st);
    CHECK(st.playing == 0);
    CHECK_NEAR(st.position_sec, 4.0, 1e-6);
    // play() from the end restarts.
    CHECK(cued_engine_play(e) == 0);
    cued_engine_status(e, &st);
    CHECK(st.playing == 1);
    CHECK_NEAR(st.position_sec, 0.0, 1e-9);

    // Stop rewinds and pauses.
    cued_engine_tick(e, sr);
    CHECK(cued_engine_stop(e) == 0);
    cued_engine_status(e, &st);
    CHECK(st.playing == 0 && st.loaded == 1);
    CHECK_NEAR(st.position_sec, 0.0, 1e-9);

    // Loading while paused replaces the track; autoplay=0 stays paused.
    CHECK(cued_engine_load(e, b.c_str(), 0) == 0);
    cued_engine_status(e, &st);
    CHECK(st.playing == 0 && std::string(st.path) == b && st.crossfading == 0);

    // Volume is clamped and does not break anything.
    cued_engine_set_volume(e, 2.0f);
    cued_engine_set_volume(e, -1.0f);
    cued_engine_set_volume(e, 0.5f);
    CHECK(cued_engine_play(e) == 0);
    cued_engine_tick(e, 1024);

    // Bad file.
    CHECK(cued_engine_load(e, (dir + "/nope.wav").c_str(), 1) == CUED_ERR_DECODE);
    CHECK(std::strlen(cued_last_error()) > 0);
    cued_engine_status(e, &st);
    CHECK(std::string(st.path) == b); // unchanged

    // A load mid-crossfade completes the pending fade first.
    cued_engine_set_crossfade(e, 2.0, CUED_CURVE_SMOOTH_STEP);
    CHECK(cued_engine_load(e, a.c_str(), 1) == 0);
    cued_engine_tick(e, 1000);
    cued_engine_status(e, &st);
    CHECK(st.crossfading == 1 && std::string(st.path) == b);
    CHECK(cued_engine_load(e, b.c_str(), 1) == 0);
    cued_engine_status(e, &st);
    CHECK(st.crossfading == 1 && std::string(st.path) == a);

    cued_engine_destroy(e);

    // use_device=1 falls back to the null output where no device exists, and
    // the engine still works either way.
    cued_engine* e2 = cued_engine_create(0, 1);
    CHECK(e2 != nullptr);
    if (e2) {
        CHECK(cued_engine_load(e2, a.c_str(), 1) == 0);
        cued_engine_status(e2, &st);
        CHECK(st.loaded == 1);
        if (!cued_engine_has_device(e2)) {
            cued_engine_tick(e2, 4800);
            cued_engine_status(e2, &st);
            CHECK_NEAR(st.position_sec, 0.1, 1e-9);
        }
        cued_engine_destroy(e2);
    }

    // NULL safety.
    cued_engine_destroy(nullptr);
    cued_engine_status(nullptr, &st);
    CHECK(st.loaded == 0);
    CHECK(cued_engine_load(nullptr, a.c_str(), 1) == CUED_ERR_INVALID);
    CHECK(cued_engine_spectrum(nullptr, spec, 64) == 0);

    std::remove(a.c_str());
    std::remove(b.c_str());
    return test::finish("engine");
}
