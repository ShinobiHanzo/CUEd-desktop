// Two decks, a crossfading mixer and an output (miniaudio device or a
// tick-driven null output). See include/cued_audio.h for the semantics.
#pragma once

#include <cstdint>
#include <memory>
#include <mutex>
#include <string>
#include <vector>

#include "crossfade.h"
#include "decoder.h"
#include "karaoke.h"
#include "spectrogram.h"

struct ma_device; // from miniaudio.h, kept out of this header

namespace cued {

struct EngineStatus {
    bool playing = false;
    bool loaded = false;
    double positionSec = 0.0;
    double durationSec = 0.0;
    bool crossfading = false;
    float crossfadeProgress = 0.0f;
    std::string path;
};

class Engine {
public:
    static constexpr int kChannels = 2;
    static constexpr int kBands = 64;

    // sampleRate 0 = 48000. useDevice: try a miniaudio playback device, fall
    // back to the null output when none can be opened.
    Engine(int sampleRate, bool useDevice);
    ~Engine();

    Engine(const Engine&) = delete;
    Engine& operator=(const Engine&) = delete;

    int sampleRate() const { return sampleRate_; }
    bool hasDevice() const { return device_ != nullptr; }

    // Returns 0 or a CUED_ERR_* value; sets the thread-local last error.
    int load(const std::string& path, bool autoplay);
    int play();
    int pause();
    int stop();
    int seek(double seconds);
    void setCrossfade(double seconds, CrossfadeCurve curve);
    void setVolume(float v);
    // Vocal cut on the mixed output (karaoke). strength 0..1.
    void setKaraoke(bool on, float strength);
    EngineStatus status();
    // Copies up to maxBands of the latest spectrum; returns the count.
    int spectrum(float* out, int maxBands);
    // Null output only: renders `frames` frames through the mixer.
    void tick(int frames);

    // The mixer. Writes `frames` interleaved stereo f32 frames. Called by the
    // device callback or by tick().
    void mix(float* out, int frames);

private:
    struct Deck {
        std::unique_ptr<Decoder> decoder;
        std::string path;
        uint64_t position = 0;
        bool finished = false;
        bool loaded() const { return decoder != nullptr; }
        void reset() { decoder.reset(); path.clear(); position = 0; finished = false; }
    };

    // Caller holds m_.
    void readDeck(Deck& d, float* buf, int frames);
    void completeCrossfade();
    void feedSpectrum(const float* stereo, int frames);

    int sampleRate_;
    std::mutex m_;
    Deck decks_[2];
    int active_ = 0;
    bool playing_ = false;
    bool crossfading_ = false;
    uint64_t fadePos_ = 0;
    uint64_t fadeLen_ = 0;
    double crossfadeSec_ = 6.0;
    CrossfadeCurve curve_ = CrossfadeCurve::EqualPower;
    float volume_ = 1.0f;
    bool karaokeOn_ = false;
    VocalCut karaoke_;

    Spectrogram spectrogram_;
    StreamingSpectrogram streaming_;
    std::vector<float> latestBands_;
    std::vector<float> bufA_, bufB_, mono_, tickBuf_;

    ma_device* device_ = nullptr;
};

} // namespace cued
