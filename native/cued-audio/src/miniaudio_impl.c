/*
 * The single translation unit that carries the miniaudio implementation.
 * Everything else includes miniaudio.h as a plain header.
 *
 * Trimmed to what the engine needs: device output (ALSA / PulseAudio on
 * Linux, WASAPI on Windows, Core Audio on macOS; all dlopen'ed at runtime so
 * no system audio headers are needed at build time) and the WAV / FLAC / MP3
 * decoders. Encoding, generation, the resource manager, node graph and
 * high-level engine are compiled out. AAC / M4A and anything else miniaudio
 * does not understand is handled by the ffmpeg fallback in decoder.cpp.
 */
#define MA_NO_ENCODING
#define MA_NO_GENERATION
#define MA_NO_RESOURCE_MANAGER
#define MA_NO_NODE_GRAPH
#define MA_NO_ENGINE
#define MA_NO_JACK
#define MA_NO_SNDIO
#define MA_NO_AUDIO4
#define MA_NO_OSS
#define MA_NO_WEBAUDIO
#define MA_NO_AAUDIO
#define MA_NO_OPENSL
#define MA_NO_CUSTOM
#define MA_NO_WINMM
#define MA_NO_DSOUND
#define MA_NO_NULL
#define MINIAUDIO_IMPLEMENTATION
#include "../third_party/miniaudio/miniaudio.h"
