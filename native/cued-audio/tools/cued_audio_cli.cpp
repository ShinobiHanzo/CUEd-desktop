// cued-audio: command line front end for the engine.
//
//   cued-audio analyse <file>            JSON: bpm, confidence, firstBeatSec, durationSec, envelope[64]
//   cued-audio play <file> [file2] [--null] [--crossfade S] [--curve 0|1|2]
//                                        plays file, then crossfades into file2 after a few seconds
//   cued-audio version
#include <chrono>
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>
#include <thread>
#include <vector>

#include "../include/cued_audio.h"

namespace {

void usage() {
    std::fprintf(stderr,
        "usage:\n"
        "  cued-audio analyse <file> [--columns N]\n"
        "  cued-audio play <file> [file2] [--null] [--crossfade SECONDS] [--curve 0|1|2] [--after SECONDS]\n"
        "  cued-audio version\n");
}

std::string jsonString(const std::string& s) {
    std::string out = "\"";
    for (char c : s) {
        switch (c) {
            case '"': out += "\\\""; break;
            case '\\': out += "\\\\"; break;
            case '\n': out += "\\n"; break;
            default: out += c;
        }
    }
    return out + "\"";
}

int analyse(const std::string& path, int columns) {
    cued_analysis a;
    if (cued_analyse_file(path.c_str(), &a) != 0) {
        std::fprintf(stderr, "cued-audio: %s\n", cued_last_error());
        return 1;
    }
    std::vector<float> env(static_cast<size_t>(columns));
    if (cued_envelope_file(path.c_str(), env.data(), columns) != 0) {
        std::fprintf(stderr, "cued-audio: %s\n", cued_last_error());
        return 1;
    }
    std::printf("{\"path\":%s,\"bpm\":%.3f,\"confidence\":%.4f,\"firstBeatSec\":%.4f,\"durationSec\":%.3f,"
                "\"sampleRate\":%d,\"channels\":%d,\"envelope\":[",
                jsonString(path).c_str(), a.bpm, a.confidence, a.first_beat_sec, a.duration_sec, a.sample_rate, a.channels);
    for (int i = 0; i < columns; ++i) std::printf("%s%.4f", i ? "," : "", env[i]);
    std::printf("]}\n");
    return 0;
}

void printStatus(cued_engine* e) {
    cued_status st;
    cued_engine_status(e, &st);
    float spec[64];
    const int n = cued_engine_spectrum(e, spec, 64);
    std::string bar;
    for (int i = 0; i < n; i += 2) {
        const float v = std::max(spec[i], i + 1 < n ? spec[i + 1] : 0.0f);
        bar += v > 0.75f ? '#' : v > 0.5f ? '=' : v > 0.25f ? '-' : v > 0.05f ? '.' : ' ';
    }
    const char* name = std::strrchr(st.path, '/');
    name = name ? name + 1 : st.path;
    std::printf("\r%s %6.1fs / %6.1fs %s%-4.0f%% |%s| %-40.40s",
                st.playing ? ">" : "|", st.position_sec, st.duration_sec,
                st.crossfading ? "xfade " : "", st.crossfading ? st.crossfade_progress * 100.0f : 0.0f,
                bar.c_str(), name);
    std::fflush(stdout);
}

int play(const std::string& first, const std::string& second, bool nullDevice, double crossfade, int curve, double after) {
    cued_engine* e = cued_engine_create(0, nullDevice ? 0 : 1);
    if (!e) {
        std::fprintf(stderr, "cued-audio: %s\n", cued_last_error());
        return 1;
    }
    const bool ticking = !cued_engine_has_device(e);
    if (ticking && !nullDevice) std::fprintf(stderr, "cued-audio: %s\n", cued_last_error());
    cued_engine_set_crossfade(e, crossfade, curve);
    if (cued_engine_load(e, first.c_str(), 1) != 0) {
        std::fprintf(stderr, "cued-audio: %s\n", cued_last_error());
        cued_engine_destroy(e);
        return 1;
    }
    const int sr = 48000;
    const int step = sr / 20; // 50 ms
    bool secondLoaded = second.empty();
    cued_status st;
    for (;;) {
        if (ticking) cued_engine_tick(e, step);
        std::this_thread::sleep_for(std::chrono::milliseconds(50));
        cued_engine_status(e, &st);
        if (!secondLoaded && st.position_sec >= after) {
            if (cued_engine_load(e, second.c_str(), 1) != 0) {
                std::fprintf(stderr, "\ncued-audio: %s\n", cued_last_error());
                cued_engine_destroy(e);
                return 1;
            }
            secondLoaded = true;
        }
        printStatus(e);
        if (!st.playing) break;
    }
    std::printf("\n");
    cued_engine_destroy(e);
    return 0;
}

} // namespace

int main(int argc, char** argv) {
    if (argc < 2) { usage(); return 2; }
    const std::string cmd = argv[1];
    if (cmd == "version") {
        std::printf("cued-audio %s\n", cued_audio_version());
        return 0;
    }
    std::vector<std::string> positional;
    bool nullDevice = false;
    double crossfade = 6.0, after = 5.0;
    int curve = CUED_CURVE_EQUAL_POWER, columns = 64;
    for (int i = 2; i < argc; ++i) {
        const std::string a = argv[i];
        auto value = [&](double& out) { if (i + 1 < argc) out = std::atof(argv[++i]); };
        if (a == "--null") nullDevice = true;
        else if (a == "--crossfade") value(crossfade);
        else if (a == "--after") value(after);
        else if (a == "--curve" && i + 1 < argc) curve = std::atoi(argv[++i]);
        else if (a == "--columns" && i + 1 < argc) columns = std::max(1, std::atoi(argv[++i]));
        else if (!a.empty() && a[0] == '-') { usage(); return 2; }
        else positional.push_back(a);
    }
    if (cmd == "analyse" || cmd == "analyze") {
        if (positional.size() != 1) { usage(); return 2; }
        return analyse(positional[0], columns);
    }
    if (cmd == "play") {
        if (positional.empty() || positional.size() > 2) { usage(); return 2; }
        return play(positional[0], positional.size() > 1 ? positional[1] : "", nullDevice, crossfade, curve, after);
    }
    usage();
    return 2;
}
