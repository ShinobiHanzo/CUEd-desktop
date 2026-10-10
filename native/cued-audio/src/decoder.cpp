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

#include <cerrno>
#include <sys/stat.h>

#ifdef _WIN32
#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>
#else
#include <fcntl.h>
#include <signal.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>
#endif

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

// A child process whose stdout we read: fork/exec with a pipe on POSIX,
// CreateProcess with an inheritable pipe on Windows. stdin is /dev/null (NUL),
// stderr is inherited (ffmpeg runs with -v error, so it stays quiet).
class Child {
public:
    Child() = default;
    Child(const Child&) = delete;
    Child& operator=(const Child&) = delete;
    ~Child() {
        closeRead();
#ifdef _WIN32
        if (proc_ != INVALID_HANDLE_VALUE) CloseHandle(proc_);
#endif
    }

    // Starts argv[0] with argv. False when it could not be started; `notFound`
    // then says the executable itself was missing (POSIX only learns that from
    // the exit code 127, see wait()).
    bool start(const std::vector<std::string>& argv, bool& notFound);
    // Reads up to cap bytes: the count, 0 at end of stream, -1 on error.
    long read(void* buf, size_t cap);
    void closeRead();
    // Ends the process if it still runs.
    void kill();
    // Waits for the exit and returns the exit code (127 = not found on POSIX), or -1.
    int wait();

private:
#ifdef _WIN32
    HANDLE read_ = INVALID_HANDLE_VALUE;
    HANDLE proc_ = INVALID_HANDLE_VALUE;
#else
    int fd_ = -1;
    pid_t pid_ = -1;
#endif
};

#ifdef _WIN32

// One argument the way CommandLineToArgvW / the MSVC runtime parse it back.
std::string quoteArg(const std::string& a) {
    if (!a.empty() && a.find_first_of(" \t\"") == std::string::npos) return a;
    std::string out = "\"";
    size_t backslashes = 0;
    for (char c : a) {
        if (c == '\\') { ++backslashes; continue; }
        if (c == '"') { out.append(backslashes * 2 + 1, '\\'); out += '"'; backslashes = 0; continue; }
        out.append(backslashes, '\\');
        backslashes = 0;
        out += c;
    }
    out.append(backslashes * 2, '\\');
    out += '"';
    return out;
}

bool Child::start(const std::vector<std::string>& argv, bool& notFound) {
    notFound = false;
    SECURITY_ATTRIBUTES sa{};
    sa.nLength = sizeof sa;
    sa.bInheritHandle = TRUE;
    HANDLE readEnd = nullptr, writeEnd = nullptr;
    if (!CreatePipe(&readEnd, &writeEnd, &sa, 0)) return false;
    SetHandleInformation(readEnd, HANDLE_FLAG_INHERIT, 0);
    HANDLE nul = CreateFileA("NUL", GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE, &sa, OPEN_EXISTING, 0, nullptr);

    STARTUPINFOA si{};
    si.cb = sizeof si;
    si.dwFlags = STARTF_USESTDHANDLES;
    si.hStdOutput = writeEnd;
    si.hStdInput = nul != INVALID_HANDLE_VALUE ? nul : GetStdHandle(STD_INPUT_HANDLE);
    si.hStdError = GetStdHandle(STD_ERROR_HANDLE);

    std::string cmd;
    for (const auto& a : argv) {
        if (!cmd.empty()) cmd += ' ';
        cmd += quoteArg(a);
    }
    std::vector<char> line(cmd.begin(), cmd.end());
    line.push_back('\0');

    PROCESS_INFORMATION pi{};
    BOOL ok = CreateProcessA(nullptr, line.data(), nullptr, nullptr, TRUE, CREATE_NO_WINDOW, nullptr, nullptr, &si, &pi);
    DWORD err = ok ? 0 : GetLastError();
    CloseHandle(writeEnd);
    if (nul != INVALID_HANDLE_VALUE) CloseHandle(nul);
    if (!ok) {
        CloseHandle(readEnd);
        notFound = err == ERROR_FILE_NOT_FOUND || err == ERROR_PATH_NOT_FOUND;
        return false;
    }
    CloseHandle(pi.hThread);
    proc_ = pi.hProcess;
    read_ = readEnd;
    return true;
}

long Child::read(void* buf, size_t cap) {
    if (read_ == INVALID_HANDLE_VALUE) return 0;
    DWORD n = 0;
    if (!ReadFile(read_, buf, static_cast<DWORD>(cap), &n, nullptr)) {
        return GetLastError() == ERROR_BROKEN_PIPE ? 0 : -1;
    }
    return static_cast<long>(n);
}

void Child::closeRead() {
    if (read_ != INVALID_HANDLE_VALUE) { CloseHandle(read_); read_ = INVALID_HANDLE_VALUE; }
}

void Child::kill() {
    if (proc_ != INVALID_HANDLE_VALUE) TerminateProcess(proc_, 1);
}

int Child::wait() {
    if (proc_ == INVALID_HANDLE_VALUE) return -1;
    WaitForSingleObject(proc_, INFINITE);
    DWORD code = 0;
    int rc = GetExitCodeProcess(proc_, &code) ? static_cast<int>(code) : -1;
    CloseHandle(proc_);
    proc_ = INVALID_HANDLE_VALUE;
    return rc;
}

#else

bool Child::start(const std::vector<std::string>& argv, bool& notFound) {
    notFound = false;
    int fds[2];
    if (pipe(fds) != 0) return false;
    std::vector<char*> cargv;
    for (const auto& a : argv) cargv.push_back(const_cast<char*>(a.c_str()));
    cargv.push_back(nullptr);
    pid_ = fork();
    if (pid_ < 0) {
        close(fds[0]);
        close(fds[1]);
        pid_ = -1;
        return false;
    }
    if (pid_ == 0) {
        // Child: stdout -> pipe, stdin -> /dev/null, stderr inherited.
        dup2(fds[1], STDOUT_FILENO);
        int devnull = ::open("/dev/null", O_RDONLY);
        if (devnull >= 0) { dup2(devnull, STDIN_FILENO); close(devnull); }
        close(fds[0]);
        close(fds[1]);
        execvp(cargv[0], cargv.data());
        _exit(127);
    }
    close(fds[1]);
    fd_ = fds[0];
    return true;
}

long Child::read(void* buf, size_t cap) {
    if (fd_ < 0) return 0;
    for (;;) {
        ssize_t n = ::read(fd_, buf, cap);
        if (n < 0 && errno == EINTR) continue;
        return static_cast<long>(n);
    }
}

void Child::closeRead() {
    if (fd_ >= 0) { close(fd_); fd_ = -1; }
}

void Child::kill() {
    if (pid_ > 0) ::kill(pid_, SIGKILL);
}

int Child::wait() {
    if (pid_ <= 0) return -1;
    int status = 0;
    int rc = waitpid(pid_, &status, 0) < 0 ? -1 : (WIFEXITED(status) ? WEXITSTATUS(status) : -1);
    pid_ = -1;
    return rc;
}

#endif

// Runs a command and captures its stdout; returns the exit status (or -1).
int runCapture(const std::vector<std::string>& argv, std::string& out) {
    Child child;
    bool notFound = false;
    if (!child.start(argv, notFound)) return -1;
    char buf[4096];
    for (;;) {
        long n = child.read(buf, sizeof buf);
        if (n > 0) out.append(buf, static_cast<size_t>(n));
        else break;
    }
    child.closeRead();
    return child.wait();
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
        if (!reaped_.load()) child_.kill();
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
        bool notFound = false;
        if (!d->child_.start(argv, notFound)) {
            error = notFound ? "ffmpeg not found on PATH" : "could not spawn ffmpeg";
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
            long n = child_.read(bytes + carry, cap - carry);
            if (n <= 0) break;
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
        child_.closeRead();
        exitCode_ = child_.wait();
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
    Child child_;
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
