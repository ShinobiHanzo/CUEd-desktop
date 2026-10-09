#include "decoder.h"

#include <algorithm>
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

#include <fcntl.h>
#include <signal.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#include "../third_party/miniaudio/miniaudio.h"

namespace cued {

namespace {

// ---------------------------------------------------------------------------
// miniaudio back end
// ---------------------------------------------------------------------------
class MiniaudioDecoder final : public Decoder {
public:
    ~MiniaudioDecoder() override {
        if (initialised_) ma_decoder_uninit(&dec_);
    }

    static std::unique_ptr<MiniaudioDecoder> open(const std::string& path, int outSampleRate, int outChannels,
                                                  Format format, std::string& error) {
        std::unique_ptr<MiniaudioDecoder> d(new MiniaudioDecoder());
        ma_decoder_config cfg = ma_decoder_config_init(
            format == Format::F32 ? ma_format_f32 : ma_format_s16,
            static_cast<ma_uint32>(outChannels), static_cast<ma_uint32>(outSampleRate));
        ma_result r = ma_decoder_init_file(path.c_str(), &cfg, &d->dec_);
        if (r != MA_SUCCESS) {
            error = std::string("miniaudio: ") + ma_result_description(r);
            return nullptr;
        }
        d->initialised_ = true;
        d->format_ = format;
        ma_format f;
        ma_uint32 ch = 0, sr = 0;
        if (ma_decoder_get_data_format(&d->dec_, &f, &ch, &sr, nullptr, 0) == MA_SUCCESS) {
            d->channels_ = static_cast<int>(ch);
            d->sampleRate_ = static_cast<int>(sr);
        }
        ma_uint32 sch = 0, ssr = 0;
        if (d->dec_.pBackend != nullptr &&
            ma_data_source_get_data_format(d->dec_.pBackend, nullptr, &sch, &ssr, nullptr, 0) == MA_SUCCESS) {
            d->sourceChannels_ = static_cast<int>(sch);
            d->sourceSampleRate_ = static_cast<int>(ssr);
        } else {
            d->sourceChannels_ = d->channels_;
            d->sourceSampleRate_ = d->sampleRate_;
        }
        ma_uint64 len = 0;
        if (ma_decoder_get_length_in_pcm_frames(&d->dec_, &len) == MA_SUCCESS) d->length_ = len;
        return d;
    }

    int sampleRate() const override { return sampleRate_; }
    int channels() const override { return channels_; }
    int sourceSampleRate() const override { return sourceSampleRate_; }
    int sourceChannels() const override { return sourceChannels_; }
    Format format() const override { return format_; }
    uint64_t lengthFrames() const override { return length_; }
    bool usedFfmpeg() const override { return false; }

    uint64_t read(void* out, uint64_t frames) override {
        ma_uint64 got = 0;
        ma_result r = ma_decoder_read_pcm_frames(&dec_, out, frames, &got);
        if (r == MA_AT_END || (r == MA_SUCCESS && got < frames)) atEnd_ = true;
        else if (r != MA_SUCCESS) atEnd_ = true;
        cursor_ += got;
        return got;
    }

    bool atEnd() const override { return atEnd_; }

    bool seek(uint64_t frame) override {
        if (ma_decoder_seek_to_pcm_frame(&dec_, frame) != MA_SUCCESS) return false;
        cursor_ = frame;
        atEnd_ = length_ != 0 && frame >= length_;
        return true;
    }

    uint64_t cursor() const override { return cursor_; }

private:
    ma_decoder dec_{};
    bool initialised_ = false;
    Format format_ = Format::F32;
    int sampleRate_ = 0, channels_ = 0, sourceSampleRate_ = 0, sourceChannels_ = 0;
    uint64_t length_ = 0;
    uint64_t cursor_ = 0;
    bool atEnd_ = false;
};

// ---------------------------------------------------------------------------
// ffmpeg CLI back end
// ---------------------------------------------------------------------------

// Spawns argv with stdout on a pipe. Returns the read end or -1.
int spawnPipe(const std::vector<std::string>& argv, pid_t& pid) {
    int fds[2];
    if (pipe(fds) != 0) return -1;
    std::vector<char*> cargv;
    for (const auto& a : argv) cargv.push_back(const_cast<char*>(a.c_str()));
    cargv.push_back(nullptr);
    pid = fork();
    if (pid < 0) {
        close(fds[0]);
        close(fds[1]);
        return -1;
    }
    if (pid == 0) {
        // Child: stdout -> pipe, stdin -> /dev/null, stderr inherited (ffmpeg -v error only).
        dup2(fds[1], STDOUT_FILENO);
        int devnull = ::open("/dev/null", O_RDONLY);
        if (devnull >= 0) { dup2(devnull, STDIN_FILENO); close(devnull); }
        close(fds[0]);
        close(fds[1]);
        execvp(cargv[0], cargv.data());
        _exit(127);
    }
    close(fds[1]);
    return fds[0];
}

// Runs a command and captures its stdout; returns the exit status (or -1).
int runCapture(const std::vector<std::string>& argv, std::string& out) {
    pid_t pid = -1;
    int fd = spawnPipe(argv, pid);
    if (fd < 0) return -1;
    char buf[4096];
    for (;;) {
        ssize_t n = ::read(fd, buf, sizeof buf);
        if (n > 0) out.append(buf, static_cast<size_t>(n));
        else if (n == 0) break;
        else if (errno != EINTR) break;
    }
    close(fd);
    int status = 0;
    if (waitpid(pid, &status, 0) < 0) return -1;
    return WIFEXITED(status) ? WEXITSTATUS(status) : -1;
}

struct ProbeInfo {
    int sampleRate = 0;
    int channels = 0;
    double duration = 0.0;
};

ProbeInfo probe(const std::string& path) {
    ProbeInfo info;
    std::string out;
    int rc = runCapture({"ffprobe", "-v", "error", "-select_streams", "a:0",
                         "-show_entries", "stream=sample_rate,channels:format=duration",
                         "-of", "default=noprint_wrappers=1", path}, out);
    if (rc != 0) return info;
    size_t pos = 0;
    while (pos < out.size()) {
        size_t nl = out.find('\n', pos);
        if (nl == std::string::npos) nl = out.size();
        std::string line = out.substr(pos, nl - pos);
        pos = nl + 1;
        size_t eq = line.find('=');
        if (eq == std::string::npos) continue;
        std::string key = line.substr(0, eq), val = line.substr(eq + 1);
        if (key == "sample_rate") info.sampleRate = std::atoi(val.c_str());
        else if (key == "channels") info.channels = std::atoi(val.c_str());
        else if (key == "duration") info.duration = std::atof(val.c_str());
    }
    return info;
}

class FfmpegDecoder final : public Decoder {
public:
    ~FfmpegDecoder() override {
        stop_.store(true);
        if (pid_ > 0 && !reaped_.load()) kill(pid_, SIGKILL);
        if (thread_.joinable()) thread_.join();
    }

    static std::unique_ptr<FfmpegDecoder> open(const std::string& path, int outSampleRate, int outChannels,
                                               Format format, std::string& error) {
        struct stat st;
        if (stat(path.c_str(), &st) != 0) {
            error = "file not found: " + path;
            return nullptr;
        }
        std::unique_ptr<FfmpegDecoder> d(new FfmpegDecoder());
        d->format_ = format;
        ProbeInfo pi = probe(path);
        d->sourceSampleRate_ = pi.sampleRate;
        d->sourceChannels_ = pi.channels;
        d->probedDuration_ = pi.duration;
        d->sampleRate_ = outSampleRate > 0 ? outSampleRate : (pi.sampleRate > 0 ? pi.sampleRate : 44100);
        d->channels_ = outChannels > 0 ? outChannels : (pi.channels > 0 ? pi.channels : 2);
        if (d->sourceSampleRate_ == 0) d->sourceSampleRate_ = d->sampleRate_;
        if (d->sourceChannels_ == 0) d->sourceChannels_ = d->channels_;

        std::vector<std::string> argv = {"ffmpeg", "-nostdin", "-v", "error", "-i", path, "-f", "s16le",
                                         "-ac", std::to_string(d->channels_), "-ar", std::to_string(d->sampleRate_), "-"};
        d->fd_ = spawnPipe(argv, d->pid_);
        if (d->fd_ < 0) {
            error = "could not spawn ffmpeg";
            return nullptr;
        }
        d->thread_ = std::thread([p = d.get()] { p->pump(); });

        // Wait for the first half second (or EOF) so the audio thread finds data ready.
        const uint64_t want = static_cast<uint64_t>(d->sampleRate_) / 2;
        {
            std::unique_lock<std::mutex> lk(d->m_);
            d->cv_.wait_for(lk, std::chrono::seconds(10), [&] { return d->finished_ || d->frames() >= want; });
            if (d->finished_ && d->frames() == 0) {
                lk.unlock();
                error = "ffmpeg produced no audio for " + path + (d->exitCode_ == 127 ? " (ffmpeg not found on PATH)" : "");
                return nullptr;
            }
        }
        return d;
    }

    int sampleRate() const override { return sampleRate_; }
    int channels() const override { return channels_; }
    int sourceSampleRate() const override { return sourceSampleRate_; }
    int sourceChannels() const override { return sourceChannels_; }
    Format format() const override { return format_; }
    bool usedFfmpeg() const override { return true; }

    uint64_t lengthFrames() const override {
        std::lock_guard<std::mutex> lk(m_);
        if (finished_) return frames();
        if (probedDuration_ > 0.0) return static_cast<uint64_t>(probedDuration_ * sampleRate_);
        return 0;
    }

    uint64_t read(void* out, uint64_t wanted) override {
        std::unique_lock<std::mutex> lk(m_);
        if (!finished_ && frames() < cursor_ + wanted) {
            cv_.wait_for(lk, std::chrono::milliseconds(20), [&] { return finished_ || frames() >= cursor_ + wanted; });
        }
        const uint64_t total = frames();
        const uint64_t avail = total > cursor_ ? total - cursor_ : 0;
        const uint64_t n = std::min(avail, wanted);
        const int16_t* src = pcm_.data() + cursor_ * static_cast<uint64_t>(channels_);
        const size_t samples = static_cast<size_t>(n) * static_cast<size_t>(channels_);
        if (format_ == Format::S16) {
            std::memcpy(out, src, samples * sizeof(int16_t));
        } else {
            float* dst = static_cast<float*>(out);
            for (size_t i = 0; i < samples; ++i) dst[i] = static_cast<float>(src[i]) * (1.0f / 32768.0f);
        }
        cursor_ += n;
        return n;
    }

    bool atEnd() const override {
        std::lock_guard<std::mutex> lk(m_);
        return finished_ && cursor_ >= frames();
    }

    bool seek(uint64_t frame) override {
        std::lock_guard<std::mutex> lk(m_);
        cursor_ = finished_ ? std::min(frame, frames()) : frame;
        return true;
    }

    uint64_t cursor() const override {
        std::lock_guard<std::mutex> lk(m_);
        return cursor_;
    }

private:
    // Frames buffered so far (caller holds m_).
    uint64_t frames() const { return pcm_.size() / static_cast<size_t>(channels_); }

    void pump() {
        std::vector<int16_t> chunk(static_cast<size_t>(16384) * static_cast<size_t>(channels_));
        size_t carry = 0; // bytes of a partial sample left from the previous read
        unsigned char* bytes = reinterpret_cast<unsigned char*>(chunk.data());
        const size_t cap = chunk.size() * sizeof(int16_t);
        while (!stop_.load()) {
            ssize_t n = ::read(fd_, bytes + carry, cap - carry);
            if (n < 0) {
                if (errno == EINTR) continue;
                break;
            }
            if (n == 0) break;
            const size_t have = carry + static_cast<size_t>(n);
            const size_t frameBytes = sizeof(int16_t) * static_cast<size_t>(channels_);
            const size_t whole = have - have % frameBytes;
            {
                std::lock_guard<std::mutex> lk(m_);
                pcm_.insert(pcm_.end(), chunk.data(), chunk.data() + whole / sizeof(int16_t));
            }
            cv_.notify_all();
            carry = have - whole;
            if (carry) std::memmove(bytes, bytes + whole, carry);
        }
        close(fd_);
        int status = 0;
        if (waitpid(pid_, &status, 0) >= 0) exitCode_ = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
        reaped_.store(true);
        {
            std::lock_guard<std::mutex> lk(m_);
            finished_ = true;
        }
        cv_.notify_all();
    }

    Format format_ = Format::S16;
    int sampleRate_ = 0, channels_ = 0, sourceSampleRate_ = 0, sourceChannels_ = 0;
    double probedDuration_ = 0.0;
    int fd_ = -1;
    pid_t pid_ = -1;
    int exitCode_ = 0;
    std::thread thread_;
    std::atomic<bool> stop_{false};
    std::atomic<bool> reaped_{false};
    mutable std::mutex m_;
    std::condition_variable cv_;
    std::vector<int16_t> pcm_;
    bool finished_ = false;
    uint64_t cursor_ = 0;
};

} // namespace

std::unique_ptr<Decoder> Decoder::open(const std::string& path, int outSampleRate, int outChannels,
                                       Format format, std::string& error) {
    std::string maError;
    if (auto d = MiniaudioDecoder::open(path, outSampleRate, outChannels, format, maError)) return d;
    std::string ffError;
    if (auto d = FfmpegDecoder::open(path, outSampleRate, outChannels, format, ffError)) return d;
    error = maError + "; ffmpeg fallback: " + ffError;
    return nullptr;
}

} // namespace cued
