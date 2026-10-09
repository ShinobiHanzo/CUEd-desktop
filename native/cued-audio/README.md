# cued-audio (native)

The CUEd desktop audio engine, in C++17 with a C ABI (`include/cued_audio.h`).
It is a port of the phone app's DSP (`Fft`, `Spectrogram`, `BpmDetector`,
`Pcm`, `Crossfade`) with the same maths and constants, plus what a desktop
needs around it: file decoding, two decks, a crossfading mixer, device output
and a live spectrum. `crates/cued-audio` wraps it for Rust (and builds it with
`cc`, no CMake needed there).

## What is in it

| File | What |
|---|---|
| `src/fft.*`, `spectrogram.*`, `bpm.*`, `pcm.*`, `crossfade.*` | faithful ports of `core/.../dsp/*.kt` and `mix/Crossfade.kt` |
| `src/decoder.*` | one decoder interface over miniaudio and an `ffmpeg` pipe |
| `src/engine.*` | Deck A/B, crossfade (equal power / linear / smooth step), 64-band `StreamingSpectrogram` tap, miniaudio device or null output |
| `src/analysis.*` | whole-file BPM (mono, box-decimated to ~11 kHz, hop `rate/86`, 8 minute cap, exactly like `AnalysisQueue`) and loudness envelope |
| `src/cued_audio.cpp` | the C ABI |
| `src/miniaudio_impl.c` | the one translation unit that compiles miniaudio |
| `tools/cued_audio_cli.cpp` | `cued-audio analyse <file>` (JSON) and `cued-audio play <file> [file2] [--null]` |
| `tests/` | ctest suite, including ports of `FftTest`, `BpmDetectorTest`, `BpmOctaveTest` and the crossfade part of `MixTest` |

## Build

```sh
cmake -S native/cued-audio -B native/cued-audio/build
cmake --build native/cued-audio/build -j4
ctest --test-dir native/cued-audio/build --output-on-failure
./native/cued-audio/build/cued-audio analyse some.mp3
./native/cued-audio/build/cued-audio play a.flac b.m4a --crossfade 6 --null
```

Needs CMake >= 3.16 and a C++17 compiler; links only `pthread`, `dl` and `m`.
No audio development headers are required: miniaudio `dlopen`s ALSA /
PulseAudio (Linux), Core Audio (macOS) or WASAPI (Windows) at runtime.

## Decoding

* **miniaudio** (`third_party/miniaudio`, v0.11.22, MIT / public domain) decodes
  WAV, FLAC and MP3 and drives the playback device. Encoding, generation, the
  resource manager, node graph and high-level engine are compiled out.
* **ffmpeg fallback.** When miniaudio cannot open a file (AAC / M4A, Ogg, ...),
  `decoder.cpp` spawns `ffmpeg -nostdin -v error -i <path> -f s16le -ac N -ar R -`
  with stdout on a pipe and drains it on a background thread into memory as
  interleaved 16-bit PCM (s16 rather than f32 to halve the footprint: a 5 minute
  48 kHz stereo track is about 57 MB). Reads are served from that buffer, so
  seeks are instant and the audio callback never waits on the pipe for more
  than 20 ms. `ffprobe`, when present, provides the duration and source format
  up front; otherwise they are known at EOF. `ffmpeg` must be on `PATH`; if it
  is missing, loading such a file fails with a clear `cued_last_error()`.

## Output

`cued_engine_create(rate, use_device)`:

* `use_device = 1`: a miniaudio playback device (f32 stereo). If none can be
  opened (headless box, CI), the engine silently falls back to the null output
  and `cued_engine_has_device()` returns 0.
* `use_device = 0`: the null output. Nothing runs on its own; call
  `cued_engine_tick(engine, frames)` to render through the mixer, which is what
  the tests and the `--null` CLI mode do.

The spectrum (`cued_engine_spectrum`) is taken from the mono mix before the
master volume, with the phone's live-tap configuration (2048/1024, 64 bands,
30 Hz .. 16 kHz).

## Threading

One mutex guards deck state; the device callback, `tick`, and every API call
take it briefly. Decoding happens on the callback thread (miniaudio decoders are
pull-based and cheap; the ffmpeg path reads from memory). `cued_last_error()` is
thread-local.
