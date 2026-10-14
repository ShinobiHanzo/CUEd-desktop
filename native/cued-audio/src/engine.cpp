#include "engine.h"

#include <algorithm>
#include <cmath>
#include <cstring>

#include "../include/cued_audio.h"
#include "../third_party/miniaudio/miniaudio.h"
#include "error.h"

#ifdef __linux__
#include <dlfcn.h>
#endif

namespace cued {

namespace {

#ifdef __linux__
// libasound prints a wall of "cannot find card '0'" to stderr on machines
// without a sound card. We never link ALSA (miniaudio dlopens it), so install
// a silent error handler the same way, only if the library is already around.
extern "C" void silentAlsaError(const char*, int, const char*, int, const char*, ...) {}

void quietAlsa() {
    void* lib = dlopen("libasound.so.2", RTLD_NOW | RTLD_NOLOAD);
    if (!lib) lib = dlopen("libasound.so.2", RTLD_NOW);
    if (!lib) return;
    using Handler = void (*)(const char*, int, const char*, int, const char*, ...);
    using SetHandler = int (*)(Handler);
    auto set = reinterpret_cast<SetHandler>(dlsym(lib, "snd_lib_error_set_handler"));
    if (set) set(&silentAlsaError);
    // Intentionally leaked: miniaudio keeps the library loaded anyway.
}
#else
void quietAlsa() {}
#endif

void deviceCallback(ma_device* dev, void* out, const void* /*in*/, ma_uint32 frames) {
    auto* engine = static_cast<Engine*>(dev->pUserData);
    engine->mix(static_cast<float*>(out), static_cast<int>(frames));
}

} // namespace

Engine::Engine(int sampleRate, bool useDevice)
    : sampleRate_(sampleRate > 0 ? sampleRate : 48000),
      // Same configuration as the phone's live tap (SpectrumTapProcessor): 2048/1024, 30 Hz..16 kHz.
      spectrogram_(2048, 1024, kBands, sampleRate_, 30.0f, 16000.0f),
      streaming_(spectrogram_, [this](const float* bands, int count) {
          std::copy(bands, bands + std::min(count, kBands), latestBands_.begin());
      }),
      latestBands_(static_cast<size_t>(kBands), 0.0f) {
    if (useDevice) {
        quietAlsa();
        auto* dev = new ma_device;
        ma_device_config cfg = ma_device_config_init(ma_device_type_playback);
        cfg.playback.format = ma_format_f32;
        cfg.playback.channels = kChannels;
        cfg.sampleRate = static_cast<ma_uint32>(sampleRate_);
        cfg.dataCallback = deviceCallback;
        cfg.pUserData = this;
        ma_result r = ma_device_init(nullptr, &cfg, dev);
        const bool initialised = r == MA_SUCCESS;
        if (initialised) r = ma_device_start(dev);
        if (r == MA_SUCCESS) {
            device_ = dev;
            // The backend may have picked a different rate; follow it.
            if (dev->sampleRate != 0) sampleRate_ = static_cast<int>(dev->sampleRate);
        } else {
            if (initialised) ma_device_uninit(dev);
            delete dev;
            setLastError(std::string("no audio device, using null output: ") + ma_result_description(r));
        }
    }
}

Engine::~Engine() {
    if (device_) {
        ma_device_uninit(device_);
        delete device_;
        device_ = nullptr;
    }
    std::lock_guard<std::mutex> lk(m_);
    decks_[0].reset();
    decks_[1].reset();
}

int Engine::load(const std::string& path, bool autoplay) {
    std::string err;
    auto dec = Decoder::open(path, sampleRate_, kChannels, Decoder::Format::F32, err);
    if (!dec) {
        setLastError("cannot decode " + path + ": " + err);
        return CUED_ERR_DECODE;
    }
    std::lock_guard<std::mutex> lk(m_);
    if (crossfading_) completeCrossfade();
    const int idle = 1 - active_;
    decks_[idle].reset();
    decks_[idle].decoder = std::move(dec);
    decks_[idle].path = path;
    if (playing_ && decks_[active_].loaded() && !decks_[active_].finished) {
        if (crossfadeSec_ > 0.0) {
            crossfading_ = true;
            fadePos_ = 0;
            fadeLen_ = std::max<uint64_t>(1, static_cast<uint64_t>(crossfadeSec_ * sampleRate_));
        } else {
            decks_[active_].reset();
            active_ = idle;
        }
    } else {
        decks_[active_].reset();
        active_ = idle;
        playing_ = autoplay;
    }
    return CUED_OK;
}

int Engine::play() {
    std::lock_guard<std::mutex> lk(m_);
    if (!decks_[active_].loaded()) {
        setLastError("nothing loaded");
        return CUED_ERR_STATE;
    }
    if (decks_[active_].finished) {
        decks_[active_].decoder->seek(0);
        decks_[active_].position = 0;
        decks_[active_].finished = false;
    }
    playing_ = true;
    return CUED_OK;
}

int Engine::pause() {
    std::lock_guard<std::mutex> lk(m_);
    playing_ = false;
    return CUED_OK;
}

int Engine::stop() {
    std::lock_guard<std::mutex> lk(m_);
    if (crossfading_) completeCrossfade();
    playing_ = false;
    Deck& d = decks_[active_];
    if (d.loaded()) {
        d.decoder->seek(0);
        d.position = 0;
        d.finished = false;
    }
    return CUED_OK;
}

int Engine::seek(double seconds) {
    if (seconds < 0.0 || std::isnan(seconds)) {
        setLastError("seek position must be >= 0");
        return CUED_ERR_INVALID;
    }
    std::lock_guard<std::mutex> lk(m_);
    Deck& d = decks_[active_];
    if (!d.loaded()) {
        setLastError("nothing loaded");
        return CUED_ERR_STATE;
    }
    uint64_t frame = static_cast<uint64_t>(seconds * sampleRate_);
    const uint64_t len = d.decoder->lengthFrames();
    if (len != 0 && frame > len) frame = len;
    if (!d.decoder->seek(frame)) {
        setLastError("seek failed");
        return CUED_ERR_GENERIC;
    }
    d.position = frame;
    d.finished = false;
    return CUED_OK;
}

void Engine::setCrossfade(double seconds, CrossfadeCurve curve) {
    std::lock_guard<std::mutex> lk(m_);
    crossfadeSec_ = std::max(0.0, std::isnan(seconds) ? 0.0 : seconds);
    curve_ = curve;
}

void Engine::setVolume(float v) {
    std::lock_guard<std::mutex> lk(m_);
    if (std::isnan(v)) v = 0.0f;
    volume_ = std::min(std::max(v, 0.0f), 1.0f);
}

void Engine::setKaraoke(bool on, float strength) {
    std::lock_guard<std::mutex> lk(m_);
    karaoke_.setSampleRate(sampleRate_);
    karaoke_.setStrength(strength);
    if (on && !karaokeOn_) karaoke_.reset();
    karaokeOn_ = on;
}

EngineStatus Engine::status() {
    std::lock_guard<std::mutex> lk(m_);
    EngineStatus s;
    const Deck& d = decks_[active_];
    s.playing = playing_;
    s.loaded = d.loaded();
    if (d.loaded()) {
        s.positionSec = static_cast<double>(d.position) / sampleRate_;
        s.durationSec = d.decoder->durationSec();
        s.path = d.path;
    }
    s.crossfading = crossfading_;
    s.crossfadeProgress = crossfading_ && fadeLen_ > 0
        ? static_cast<float>(static_cast<double>(fadePos_) / static_cast<double>(fadeLen_)) : 0.0f;
    return s;
}

int Engine::spectrum(float* out, int maxBands) {
    if (!out || maxBands <= 0) return 0;
    std::lock_guard<std::mutex> lk(m_);
    const int n = std::min(maxBands, kBands);
    std::copy(latestBands_.begin(), latestBands_.begin() + n, out);
    return n;
}

void Engine::tick(int frames) {
    if (device_ || frames <= 0) return;
    tickBuf_.resize(static_cast<size_t>(frames) * kChannels);
    mix(tickBuf_.data(), frames);
}

void Engine::readDeck(Deck& d, float* buf, int frames) {
    const uint64_t got = d.decoder->read(buf, static_cast<uint64_t>(frames));
    if (got < static_cast<uint64_t>(frames)) {
        std::fill(buf + got * kChannels, buf + static_cast<size_t>(frames) * kChannels, 0.0f);
        if (d.decoder->atEnd()) d.finished = true;
    }
    d.position += got;
}

void Engine::completeCrossfade() {
    decks_[active_].reset();
    active_ = 1 - active_;
    crossfading_ = false;
    fadePos_ = 0;
}

void Engine::feedSpectrum(const float* stereo, int frames) {
    mono_.resize(static_cast<size_t>(frames));
    for (int i = 0; i < frames; ++i) mono_[i] = 0.5f * (stereo[2 * i] + stereo[2 * i + 1]);
    streaming_.push(mono_.data(), frames);
}

void Engine::mix(float* out, int frames) {
    if (frames <= 0) return;
    const size_t samples = static_cast<size_t>(frames) * kChannels;
    std::lock_guard<std::mutex> lk(m_);
    Deck& a = decks_[active_];
    if (!playing_ || !a.loaded()) {
        std::fill(out, out + samples, 0.0f);
        feedSpectrum(out, frames);
        return;
    }
    bufA_.resize(samples);
    readDeck(a, bufA_.data(), frames);
    if (crossfading_) {
        Deck& b = decks_[1 - active_];
        bufB_.resize(samples);
        if (b.loaded()) readDeck(b, bufB_.data(), frames);
        else std::fill(bufB_.begin(), bufB_.end(), 0.0f);
        const double invLen = 1.0 / static_cast<double>(fadeLen_);
        for (int i = 0; i < frames; ++i) {
            const float progress = static_cast<float>(static_cast<double>(fadePos_ + i) * invLen);
            const Gains g = crossfadeGains(progress, curve_);
            out[2 * i] = bufA_[2 * i] * g.outgoing + bufB_[2 * i] * g.incoming;
            out[2 * i + 1] = bufA_[2 * i + 1] * g.outgoing + bufB_[2 * i + 1] * g.incoming;
        }
        fadePos_ += static_cast<uint64_t>(frames);
        if (fadePos_ >= fadeLen_) completeCrossfade();
    } else {
        std::memcpy(out, bufA_.data(), samples * sizeof(float));
        if (a.finished) playing_ = false; // end of track: stop here
    }
    if (karaokeOn_) karaoke_.process(out, frames);
    feedSpectrum(out, frames);
    if (volume_ != 1.0f) {
        for (size_t i = 0; i < samples; ++i) out[i] *= volume_;
    }
}

} // namespace cued
