// Offline analysis: whole-file BPM detection and loudness envelope, mirroring
// the phone's AnalysisQueue / AudioDecoder pipeline.
#pragma once

#include <string>
#include <vector>

#include "bpm.h"

namespace cued {

struct FileAnalysis {
    BpmResult bpm;
    double durationSec = 0.0;
    int sampleRate = 0;  // source
    int channels = 0;    // source
    int analysisRate = 0;
};

// Decodes to mono, decimates by max(1, sr / 11025) with a box filter, caps at
// 8 minutes and runs BpmDetector(rate, hop = max(64, rate / 86)).
bool analyseFile(const std::string& path, FileAnalysis& out, std::string& error);

// Decodes to mono at the source rate, computes a 2048/1024 64-band
// spectrogram, max-pools it to `columns` and averages the bands.
bool envelopeFile(const std::string& path, int columns, std::vector<float>& out, std::string& error);

} // namespace cued
