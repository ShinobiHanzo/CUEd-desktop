// Audio file decoding for the engine and the analysers.
//
// Two back ends behind one interface:
//  * miniaudio (WAV, FLAC, MP3): pull-based, resamples / converts on the fly.
//  * ffmpeg CLI fallback (AAC/M4A, Ogg, anything else): used when miniaudio
//    cannot open the file. `ffmpeg -nostdin -v error -i <path> -f s16le -ac N
//    -ar R -` is spawned with its stdout on a pipe; a background thread drains
//    the pipe into an in-memory buffer of interleaved 16-bit samples (chosen
//    over f32 to halve the footprint: a 5 minute 48 kHz stereo track is ~57 MB
//    as s16 versus ~115 MB as f32). Reads are served from that buffer, so
//    seeking is free and the audio thread never blocks on the pipe for long.
//    Duration and source format come from `ffprobe` when it is available and
//    are otherwise known once the pipe reaches EOF.
#pragma once

#include <cstdint>
#include <memory>
#include <string>

namespace cued {

class Decoder {
public:
    enum class Format { F32, S16 };

    virtual ~Decoder() = default;

    // Opens `path`. outSampleRate / outChannels 0 = keep the source's.
    // Returns nullptr and fills `error` on failure.
    static std::unique_ptr<Decoder> open(const std::string& path, int outSampleRate, int outChannels,
                                         Format format, std::string& error);

    virtual int sampleRate() const = 0;         // output rate
    virtual int channels() const = 0;           // output channels
    virtual int sourceSampleRate() const = 0;
    virtual int sourceChannels() const = 0;
    virtual Format format() const = 0;
    // Total length in output frames; 0 when unknown (still streaming).
    virtual uint64_t lengthFrames() const = 0;
    double durationSec() const {
        const uint64_t n = lengthFrames();
        return n == 0 ? 0.0 : static_cast<double>(n) / sampleRate();
    }
    // Reads up to `frames` interleaved frames in format() into out. Returns the
    // number of frames written. A short read means either the end (atEnd())
    // or, for the ffmpeg path, an underrun while the pipe is still filling.
    virtual uint64_t read(void* out, uint64_t frames) = 0;
    virtual bool atEnd() const = 0;
    virtual bool seek(uint64_t frame) = 0;
    virtual uint64_t cursor() const = 0;
    virtual bool usedFfmpeg() const = 0;
};

// Bytes per interleaved frame for a decoder's output.
inline size_t frameBytes(Decoder::Format f, int channels) {
    return static_cast<size_t>(channels) * (f == Decoder::Format::F32 ? sizeof(float) : sizeof(int16_t));
}

} // namespace cued
