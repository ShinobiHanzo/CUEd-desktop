/*
 * cued_audio: the CUEd desktop audio engine. C ABI.
 *
 * Two decks, a crossfading mixer, device output through miniaudio (or a
 * tick-driven null output for tests and headless use), a live 64-band
 * spectrum of the mix, and offline analysis (BPM, beat phase, loudness
 * envelope) that uses the same maths as the CUEd phone app.
 *
 * Thread safety: every function on a cued_engine may be called from any
 * thread; the engine serialises deck state with a mutex. The audio callback
 * takes the same mutex, so keep calls short (they are). cued_analyse_file and
 * cued_envelope_file are pure functions and may run concurrently.
 *
 * Errors: functions returning int return 0 on success and a negative value on
 * failure (see CUED_ERR_*). cued_last_error() describes the most recent
 * failure on the calling thread.
 */
#ifndef CUED_AUDIO_H
#define CUED_AUDIO_H

#ifdef __cplusplus
extern "C" {
#endif

/* Error codes returned by the int-returning functions. */
#define CUED_OK            0
#define CUED_ERR_GENERIC  -1  /* see cued_last_error() */
#define CUED_ERR_INVALID  -2  /* bad argument (NULL engine, negative seek, ...) */
#define CUED_ERR_DECODE   -3  /* file could not be opened or decoded */
#define CUED_ERR_STATE    -4  /* nothing loaded, etc. */

/* Crossfade curves for cued_engine_set_crossfade (same meaning as the phone). */
#define CUED_CURVE_EQUAL_POWER 0
#define CUED_CURVE_LINEAR      1
#define CUED_CURVE_SMOOTH_STEP 2

typedef struct cued_engine cued_engine;

typedef struct {
    int playing;              /* 1 while the mixer advances the active deck */
    int loaded;               /* 1 when the active deck has a track */
    double position_sec;      /* position of the active deck */
    double duration_sec;      /* 0 when still unknown (ffmpeg pipe without ffprobe) */
    int crossfading;          /* 1 while blending the active deck into the next */
    float crossfade_progress; /* 0..1 during a crossfade, else 0 */
    char path[1024];          /* path of the active deck, "" when nothing is loaded */
} cued_status;

typedef struct {
    float bpm;             /* 0 when no tempo was found */
    float confidence;      /* 0..1 */
    float first_beat_sec;  /* beat phase: time of the first beat */
    double duration_sec;   /* length of the decoded audio */
    int sample_rate;       /* of the source file */
    int channels;          /* of the source file */
} cued_analysis;

/* Library version string, e.g. "0.1.0 (miniaudio 0.11.22)". */
const char* cued_audio_version(void);

/*
 * Creates an engine. sample_rate 0 = 48000. use_device 0 = null output that
 * advances time only on cued_engine_tick (for tests/CI); 1 = a real device
 * via miniaudio, falling back to the null output if no device can be opened.
 * Returns NULL on failure (cued_last_error() says why).
 */
cued_engine* cued_engine_create(int sample_rate, int use_device);
void cued_engine_destroy(cued_engine*);

/* 1 when a real audio device is driving the mixer, 0 for the null output. */
int cued_engine_has_device(cued_engine*);

/*
 * Loads path into the idle deck. If something is playing, starts a crossfade
 * of the configured length into it (a crossfade of 0 s switches at once);
 * otherwise makes it the active deck and plays it at once when autoplay=1.
 * A load during a crossfade first completes that crossfade.
 * Returns 0 or a negative error.
 */
int  cued_engine_load(cued_engine*, const char* path, int autoplay);
int  cued_engine_play(cued_engine*);
int  cued_engine_pause(cued_engine*);
/* Pauses and rewinds the active deck to 0 (completing any crossfade first). */
int  cued_engine_stop(cued_engine*);
int  cued_engine_seek(cued_engine*, double seconds);
/* curve: 0 equal power, 1 linear, 2 smooth step. seconds is clamped to >= 0. */
void cued_engine_set_crossfade(cued_engine*, double seconds, int curve);
/* Master volume, 0..1 (clamped). Applied after the mix; the spectrum is pre-volume. */
void cued_engine_set_volume(cued_engine*, float v01);
/* Karaoke: cancel the centre (lead vocal) of the mixed output, keeping its bass. strength 0..1. */
void cued_engine_set_karaoke(cued_engine*, int on, float strength);
void cued_engine_status(cued_engine*, cued_status* out);
/* Latest 64-band spectrum (0..1) of what is being output; returns bands written. */
int  cued_engine_spectrum(cued_engine*, float* out, int max_bands);
/* Null-device only: render `frames` frames (advances position, runs the mixer). Real device ignores it. */
void cued_engine_tick(cued_engine*, int frames);

/*
 * Decodes the whole file (mono, box-decimated to ~11 kHz exactly like the
 * phone's AnalysisQueue, capped at 8 minutes) and runs the BPM detector.
 */
int  cued_analyse_file(const char* path, cued_analysis* out);
/*
 * Loudness envelope with n columns in 0..1 for a scrubber background:
 * a 2048/1024 64-band spectrogram, max-pooled to `columns` and averaged
 * across bands (Spectrogram.downsample + envelope on the phone).
 */
int  cued_envelope_file(const char* path, float* out, int columns);
/* Description of the last error on this thread ("" when none). Never NULL. */
const char* cued_last_error(void);

#ifdef __cplusplus
}
#endif

#endif /* CUED_AUDIO_H */
