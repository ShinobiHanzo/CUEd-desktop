// C ABI over the C++ engine and analysers.
#include "../include/cued_audio.h"

#include <algorithm>
#include <cstring>
#include <exception>
#include <string>

#include "../third_party/miniaudio/miniaudio.h"
#include "analysis.h"
#include "engine.h"
#include "error.h"

struct cued_engine {
    cued::Engine impl;
    cued_engine(int sr, bool dev) : impl(sr, dev) {}
};

extern "C" {

const char* cued_audio_version(void) {
    static const std::string v = std::string("0.1.0 (miniaudio ") + ma_version_string() + ")";
    return v.c_str();
}

cued_engine* cued_engine_create(int sample_rate, int use_device) {
    try {
        return new cued_engine(sample_rate, use_device != 0);
    } catch (const std::exception& e) {
        cued::setLastError(std::string("engine create failed: ") + e.what());
        return nullptr;
    }
}

void cued_engine_destroy(cued_engine* e) { delete e; }

int cued_engine_has_device(cued_engine* e) { return e && e->impl.hasDevice() ? 1 : 0; }

int cued_engine_load(cued_engine* e, const char* path, int autoplay) {
    if (!e || !path) { cued::setLastError("null argument"); return CUED_ERR_INVALID; }
    return e->impl.load(path, autoplay != 0);
}

int cued_engine_play(cued_engine* e) {
    if (!e) { cued::setLastError("null engine"); return CUED_ERR_INVALID; }
    return e->impl.play();
}

int cued_engine_pause(cued_engine* e) {
    if (!e) { cued::setLastError("null engine"); return CUED_ERR_INVALID; }
    return e->impl.pause();
}

int cued_engine_stop(cued_engine* e) {
    if (!e) { cued::setLastError("null engine"); return CUED_ERR_INVALID; }
    return e->impl.stop();
}

int cued_engine_seek(cued_engine* e, double seconds) {
    if (!e) { cued::setLastError("null engine"); return CUED_ERR_INVALID; }
    return e->impl.seek(seconds);
}

void cued_engine_set_crossfade(cued_engine* e, double seconds, int curve) {
    if (e) e->impl.setCrossfade(seconds, cued::crossfadeCurveFromInt(curve));
}

void cued_engine_set_volume(cued_engine* e, float v01) {
    if (e) e->impl.setVolume(v01);
}

void cued_engine_status(cued_engine* e, cued_status* out) {
    if (!out) return;
    std::memset(out, 0, sizeof *out);
    if (!e) return;
    cued::EngineStatus s = e->impl.status();
    out->playing = s.playing ? 1 : 0;
    out->loaded = s.loaded ? 1 : 0;
    out->position_sec = s.positionSec;
    out->duration_sec = s.durationSec;
    out->crossfading = s.crossfading ? 1 : 0;
    out->crossfade_progress = s.crossfadeProgress;
    const size_t n = std::min(s.path.size(), sizeof(out->path) - 1);
    std::memcpy(out->path, s.path.data(), n);
    out->path[n] = '\0';
}

int cued_engine_spectrum(cued_engine* e, float* out, int max_bands) {
    if (!e) return 0;
    return e->impl.spectrum(out, max_bands);
}

void cued_engine_tick(cued_engine* e, int frames) {
    if (e) e->impl.tick(frames);
}

int cued_analyse_file(const char* path, cued_analysis* out) {
    if (!path || !out) { cued::setLastError("null argument"); return CUED_ERR_INVALID; }
    std::memset(out, 0, sizeof *out);
    cued::FileAnalysis a;
    std::string err;
    if (!cued::analyseFile(path, a, err)) {
        cued::setLastError("analyse " + std::string(path) + ": " + err);
        return CUED_ERR_DECODE;
    }
    out->bpm = a.bpm.bpm;
    out->confidence = a.bpm.confidence;
    out->first_beat_sec = a.bpm.firstBeatSec;
    out->duration_sec = a.durationSec;
    out->sample_rate = a.sampleRate;
    out->channels = a.channels;
    return CUED_OK;
}

int cued_envelope_file(const char* path, float* out, int columns) {
    if (!path || !out || columns <= 0) { cued::setLastError("null argument or columns <= 0"); return CUED_ERR_INVALID; }
    std::vector<float> env;
    std::string err;
    if (!cued::envelopeFile(path, columns, env, err)) {
        cued::setLastError("envelope " + std::string(path) + ": " + err);
        return CUED_ERR_DECODE;
    }
    std::copy(env.begin(), env.end(), out);
    return CUED_OK;
}

const char* cued_last_error(void) { return cued::lastError().c_str(); }

} // extern "C"
