# Building

Requirements: Rust 1.75+ (stable), a C++17 compiler (g++ 9+, clang 10+, MSVC 2019+), Python 3.9+.
No system audio headers: miniaudio loads ALSA/PulseAudio/WASAPI/CoreAudio at run time.
`ffmpeg`/`ffprobe` on the PATH are optional but recommended (AAC decoding, tags on imported files).

```bash
cargo build --release                 # cuedd, cued-funnel; compiles the C++ engine through `cc`
cargo test                            # all Rust tests
cmake -S native/cued-audio -B build/audio && cmake --build build/audio -j && ctest --test-dir build/audio --output-on-failure
cd python && python3 -m pip install -e ".[dev,relay]" && python3 -m pytest -q
```

## Running

```bash
cuedd                                 # data in ~/.local/share/cued (or $XDG_DATA_HOME/cued, %APPDATA%\cued)
cuedd --data-dir ~/CUEd --port 8770 --name "Living room" --open
CUED_AUDIO_NULL=1 cuedd --no-companion  # headless: no sound device, no Python
RUST_LOG=debug cuedd                  # chatty
```

The daemon finds `python/cued_companion` next to the source tree or the binary
(`CUED_COMPANION_DIR` overrides). It needs `spotdl` importable by `python3` (`CUED_PYTHON`
overrides the interpreter).

## End to end without a phone

```bash
cuedd --data-dir /tmp/cued-test &
LINK=$(curl -s -X POST http://127.0.0.1:8770/api/pair/new | python3 -c 'import sys,json;print(json.load(sys.stdin)["link"])')
cued-phone-sim pair "$LINK" --name "Sim"
cued-phone-sim sync ~/Music
cued-phone-sim status
```

## CI

`.github/workflows/ci.yml` runs the three suites on Ubuntu, builds release binaries on Linux,
macOS and Windows, and attaches them to a release when a `v*` tag is pushed.
