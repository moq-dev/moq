// Subscribe-only cross-language interop client for the interop test, linked
// against the workspace moq-c (the C bindings, built by `cargo build -p moq-c`).
//
// moq-c is a handle + callback API: connect, consume a broadcast, get a
// catalog snapshot via callback, start the video track and the audio decoder,
// and a callback fires as each frame arrives. Once a non-empty video frame has
// landed and --audio-ms of audio has been decoded to --audio-out (for tone.py to
// judge) we close and drain every registration, then return 0; a timeout exits 1.
// Publishing isn't wired up: the raw-stream importer that the other clients use
// to publish isn't part of this subscribe-only client.
//
//   c-interop subscribe --url http://127.0.0.1:4443 --broadcast b.hang --timeout 20 \
//       --audio-out /tmp/audio --audio-ms 600
//
// --version offers only that version, through moq_client_config.
#include <moq.h>

#include <pthread.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

typedef struct {
    pthread_mutex_t mu;
    pthread_cond_t cv;
    int got; // a non-empty frame arrived
    int32_t origin;
    int32_t session;
    int32_t broadcast_wait;
    int32_t broadcast; // handle delivered by moq_origin_announced_broadcast (0 until it arrives)
    int32_t catalog;
    int32_t video_track; // handle from moq_consume_video (0 until on_catalog starts it)

    // The audio decoder (0 until on_catalog starts it), where it writes, and how
    // much it has written. `audio_got` is set once `audio_samples` reaches `audio_want`.
    int32_t audio_track;
    FILE *audio_log, *audio_pcm;
    uint64_t audio_ms, audio_samples, audio_want;
    uint32_t audio_frame_bytes;
    int audio_got;

    // One flag per registration handed &ctx, set by that registration's terminal
    // (<= 0) callback. main returns only once all five are set: that terminal is
    // moq-c's documented last touch of user_data, so it is what makes main's
    // frame safe to destroy.
    int done_status, done_broadcast, done_catalog, done_frame, done_audio;
} ctx_t;

// Callbacks run on moq-c's runtime thread; main waits on the condvar. ctx lives
// on main's stack and moq-c keeps the pointer until each registration's
// terminal (<= 0) callback fires, so main must not return until every one of
// them has. Closing the session ends its status registration alone;
// moq_origin_announced_broadcast, moq_consume_catalog and moq_consume_video each
// keep the pointer until their own terminal. See drain() at the bottom.
static void done(ctx_t *c, int *flag) {
    pthread_mutex_lock(&c->mu);
    *flag = 1;
    pthread_cond_broadcast(&c->cv);
    pthread_mutex_unlock(&c->mu);
}

static void on_status(void *ud, int32_t code) {
    ctx_t *c = (ctx_t *)ud;
    fprintf(stderr, "session status: %d\n", code);
    if (code <= 0) done(c, &c->done_status);
}

static void on_frame(void *ud, int32_t frame) {
    ctx_t *c = (ctx_t *)ud;
    if (frame <= 0) { // 0 = ended, negative = error
        done(c, &c->done_frame);
        return;
    }
    moq_frame f;
    memset(&f, 0, sizeof(f));
    if (moq_consume_frame((uint32_t)frame, &f) == 0 && f.payload_size > 0) {
        pthread_mutex_lock(&c->mu);
        c->got = 1;
        pthread_cond_broadcast(&c->cv);
        pthread_mutex_unlock(&c->mu);
    }
    moq_consume_frame_free((uint32_t)frame);
}

// Writes each decoded audio frame to the files tone.py reads, until enough has arrived.
// Runs on moq-c's runtime thread, the only writer, so the files need no lock.
static void on_audio(void *ud, int32_t frame) {
    ctx_t *c = (ctx_t *)ud;
    if (frame <= 0) { // 0 = ended, negative = error
        if (frame < 0) fprintf(stderr, "audio decode error: %d\n", frame);
        done(c, &c->done_audio);
        return;
    }
    moq_audio_frame f;
    memset(&f, 0, sizeof(f));
    if (moq_decode_audio_frame((uint32_t)frame, &f) == 0 && c->audio_samples < c->audio_want) {
        uint64_t samples = f.data_size / c->audio_frame_bytes;
        fprintf(c->audio_log, "%llu %llu\n", (unsigned long long)f.timestamp_us, (unsigned long long)samples);
        fwrite(f.data, 1, f.data_size, c->audio_pcm);
        c->audio_samples += samples;
        if (c->audio_samples >= c->audio_want) {
            fflush(c->audio_log);
            fflush(c->audio_pcm);
            pthread_mutex_lock(&c->mu);
            c->audio_got = 1;
            pthread_cond_broadcast(&c->cv);
            pthread_mutex_unlock(&c->mu);
        }
    }
    moq_decode_audio_frame_free((uint32_t)frame);
}

// Delivers a broadcast handle for each publisher instance serving the path, then
// a terminal code (<= 0). Store the first positive handle and wake main; one frame
// is all this client reads, so a later instance's handle is freed unread.
static void on_broadcast(void *ud, int32_t broadcast) {
    ctx_t *c = (ctx_t *)ud;
    if (broadcast <= 0) { // 0 = ended, negative = error
        done(c, &c->done_broadcast);
        return;
    }
    pthread_mutex_lock(&c->mu);
    int first = c->broadcast <= 0;
    if (first) {
        c->broadcast = broadcast;
        pthread_cond_broadcast(&c->cv);
    }
    pthread_mutex_unlock(&c->mu);
    if (!first) moq_consume_close((uint32_t)broadcast);
}

static void on_catalog(void *ud, int32_t catalog) {
    ctx_t *c = (ctx_t *)ud;
    if (catalog <= 0) {
        done(c, &c->done_catalog);
        return;
    }

    pthread_mutex_lock(&c->mu);
    int start = c->video_track <= 0;
    int start_audio = c->audio_track <= 0;
    pthread_mutex_unlock(&c->mu);

    if (start_audio) {
        // Like video, a lazy publisher may add audio in a later update. Decode at
        // the codec's own rate and layout, so the catalog's are what tone.py checks.
        moq_audio_config acfg;
        memset(&acfg, 0, sizeof(acfg));
        if (moq_consume_audio_config((uint32_t)catalog, 0, &acfg) == 0 && acfg.channel_count > 0) {
            c->audio_frame_bytes = 4 * acfg.channel_count;
            c->audio_want = c->audio_ms * acfg.sample_rate / 1000;
            fprintf(c->audio_log, "%u %u\n", acfg.sample_rate, acfg.channel_count);
            moq_audio_decoder_output out;
            memset(&out, 0, sizeof(out));
            out.format = MOQ_AUDIO_SAMPLE_FORMAT_F32;
            out.max_delay_us = 1000000;
            int32_t track = moq_decode_audio((uint32_t)catalog, 0, &out, on_audio, ud);
            if (track > 0) {
                pthread_mutex_lock(&c->mu);
                c->audio_track = track;
                pthread_mutex_unlock(&c->mu);
            } else {
                fprintf(stderr, "moq_decode_audio failed: %d\n", track);
            }
        }
    }

    if (start) {
        // A lazy publisher may announce video in a later catalog update, so this
        // just no-ops (config returns < 0) until a video track exists at index 0.
        moq_video_config vcfg;
        memset(&vcfg, 0, sizeof(vcfg));
        if (moq_consume_video_config((uint32_t)catalog, 0, &vcfg) == 0) {
            int32_t track = moq_consume_video((uint32_t)catalog, 0, 1000000, on_frame, ud);
            if (track > 0) {
                pthread_mutex_lock(&c->mu);
                c->video_track = track;
                pthread_mutex_unlock(&c->mu);
            }
        }
    }
    moq_consume_catalog_free((uint32_t)catalog);
}

// The context whose session a failure closes, once connected.
static ctx_t *failing;

// Report a failure and exit 1, for the paths where moq-c still holds &ctx.
//
// _exit rather than a return: it leaves main's frame (and so ctx) intact for the
// callbacks still pointing at it, which is what the failure paths want. Only the
// session is closed first, so the relay sees a close rather than timing the
// connection out; a client that has already failed should not wait on the rest of
// a teardown that may be exactly what is broken.
static _Noreturn void fail(const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    vfprintf(stderr, fmt, ap);
    va_end(ap);
    fflush(stderr);

    ctx_t *c = failing;
    failing = NULL; // a failure while closing exits at once
    if (c && moq_session_close((uint32_t)c->session) == 0) {
        struct timespec deadline;
        clock_gettime(CLOCK_REALTIME, &deadline);
        deadline.tv_sec += 5;
        pthread_mutex_lock(&c->mu);
        while (!c->done_status) {
            if (pthread_cond_timedwait(&c->cv, &c->mu, &deadline) != 0) break; // timed out
        }
        pthread_mutex_unlock(&c->mu);
    }
    _exit(1);
}

// Close every registration holding &ctx and wait for each one's terminal
// callback. Only then may main return: main's frame is ctx's storage, and
// exit()'s own call frames reuse those addresses, so a callback that arrives
// after the return writes ctx->mu and ctx->cv over the live exit path. That is
// a use-after-return, and what it corrupts decides the symptom (a SIGSEGV or
// SIGBUS on a clobbered return address, or the glibc __pthread_tpp_change_priority
// assertion when the mutex's __kind is the field that got garbage).
//
// Each *_close returns immediately and does NOT deliver the terminal itself; the
// task delivers it, so every one of them has to be waited for.
static void drain(ctx_t *c) {
    pthread_mutex_lock(&c->mu);
    int32_t track = c->video_track;
    int32_t audio = c->audio_track;
    // A track that was never started registered nothing, so it owes no terminal.
    if (track <= 0) c->done_frame = 1;
    if (audio <= 0) c->done_audio = 1;
    pthread_mutex_unlock(&c->mu);

    if (track > 0) moq_consume_video_cancel((uint32_t)track);
    if (audio > 0) moq_decode_audio_cancel((uint32_t)audio);
    moq_consume_catalog_cancel((uint32_t)c->catalog);
    moq_origin_announced_broadcast_cancel((uint32_t)c->broadcast_wait);
    moq_session_close((uint32_t)c->session);

    struct timespec deadline;
    clock_gettime(CLOCK_REALTIME, &deadline);
    deadline.tv_sec += 10;

    pthread_mutex_lock(&c->mu);
    while (!(c->done_frame && c->done_audio && c->done_catalog && c->done_broadcast && c->done_status)) {
        if (pthread_cond_timedwait(&c->cv, &c->mu, &deadline) != 0) break; // timed out
    }
    int ok = c->done_frame && c->done_audio && c->done_catalog && c->done_broadcast && c->done_status;
    int f = c->done_frame, a = c->done_audio, ct = c->done_catalog, b = c->done_broadcast, s = c->done_status;
    pthread_mutex_unlock(&c->mu);

    if (!ok) {
        fail("error: teardown never quiesced (frame=%d audio=%d catalog=%d broadcast=%d status=%d)\n", f, a, ct, b,
             s);
    }
}

int main(int argc, char **argv) {
    const char *url = NULL, *broadcast = NULL, *version = NULL, *audio_out = NULL;
    double timeout_s = 20.0;
    long audio_ms = 0;
    for (int i = 1; i < argc; i++) {
        if (!strcmp(argv[i], "--url") && i + 1 < argc) url = argv[++i];
        else if (!strcmp(argv[i], "--broadcast") && i + 1 < argc) broadcast = argv[++i];
        else if (!strcmp(argv[i], "--timeout") && i + 1 < argc) timeout_s = atof(argv[++i]);
        else if (!strcmp(argv[i], "--version") && i + 1 < argc) version = argv[++i];
        else if (!strcmp(argv[i], "--audio-out") && i + 1 < argc) audio_out = argv[++i];
        else if (!strcmp(argv[i], "--audio-ms") && i + 1 < argc) audio_ms = atol(argv[++i]);
        // a leading "subscribe" positional (and anything else) is ignored.
    }
    if (!url || !broadcast || !audio_out || audio_ms <= 0) {
        fprintf(stderr, "usage: c-interop subscribe --url U --broadcast B --audio-out PREFIX --audio-ms MS [--timeout S] [--version V]\n");
        return 2;
    }

    ctx_t c;
    memset(&c, 0, sizeof(c));
    pthread_mutex_init(&c.mu, NULL);
    pthread_cond_init(&c.cv, NULL);

    char path[4096];
    snprintf(path, sizeof(path), "%s.txt", audio_out);
    c.audio_log = fopen(path, "w");
    snprintf(path, sizeof(path), "%s.pcm", audio_out);
    c.audio_pcm = fopen(path, "wb");
    if (!c.audio_log || !c.audio_pcm) {
        fprintf(stderr, "error: cannot write %s.{txt,pcm}\n", audio_out);
        return 1;
    }
    c.audio_ms = (uint64_t)audio_ms;

    c.origin = moq_origin_create();
    if (c.origin <= 0) {
        fprintf(stderr, "error: moq_origin_create failed: %d\n", c.origin);
        return 1;
    }

    // moq-c reads the config during the call alone.
    moq_client_config config;
    memset(&config, 0, sizeof(config));
    moq_string versions[1];
    if (version) {
        versions[0].data = version;
        versions[0].len = strlen(version);
        config.versions = versions;
        config.versions_len = 1;
    }

    // origin_publish = 0 disables publishing; consume via our origin.
    c.session = moq_session_connect(url, strlen(url), version ? &config : NULL, 0, (uint32_t)c.origin, on_status, &c);
    if (c.session <= 0) {
        // A registration that fails never invokes its callback, so &c isn't held
        // yet and returning is still safe here.
        fprintf(stderr, "error: moq_session_connect failed: %d\n", c.session);
        return 1;
    }
    failing = &c;

    struct timespec deadline;
    clock_gettime(CLOCK_REALTIME, &deadline);
    deadline.tv_sec += (time_t)timeout_s;

    // The broadcast arrives over the network after connect, so wait for it to be
    // announced. moq_origin_announced_broadcast resolves via on_broadcast once it's
    // available; we block on the condvar until then (or the deadline).
    c.broadcast_wait =
        moq_origin_announced_broadcast((uint32_t)c.origin, broadcast, strlen(broadcast), on_broadcast, &c);
    if (c.broadcast_wait <= 0) {
        fail("error: moq_origin_announced_broadcast failed: %d\n", c.broadcast_wait);
    }

    pthread_mutex_lock(&c.mu);
    while (c.broadcast <= 0) {
        if (pthread_cond_timedwait(&c.cv, &c.mu, &deadline) != 0) break; // timed out
    }
    int32_t bc = c.broadcast;
    pthread_mutex_unlock(&c.mu);
    if (bc <= 0) {
        fail("error: broadcast never announced\n");
    }

    c.catalog = moq_consume_catalog((uint32_t)bc, on_catalog, &c);
    if (c.catalog <= 0) {
        fail("error: moq_consume_catalog failed: %d\n", c.catalog);
    }

    pthread_mutex_lock(&c.mu);
    while (!c.got || !c.audio_got) {
        if (pthread_cond_timedwait(&c.cv, &c.mu, &deadline) != 0) break; // timed out
    }
    int got = c.got, audio_got = c.audio_got;
    pthread_mutex_unlock(&c.mu);

    if (!got || !audio_got) {
        fail("error: timed out waiting for data (video=%d audio=%d)\n", got, audio_got);
    }

    fprintf(stderr, "received a frame and %llu audio samples from %s\n", (unsigned long long)c.audio_samples,
            broadcast);

    // The data path succeeded, which is all this interop client verifies. Returning
    // (rather than _exit) is the other half of what it verifies: an embedder that
    // closes and drains gets a clean process exit, with moq-c's runtime thread
    // still live behind its LazyLock.
    drain(&c);
    fclose(c.audio_log);
    fclose(c.audio_pcm);
    moq_consume_close((uint32_t)bc);
    moq_origin_close((uint32_t)c.origin);
    fflush(stderr);
    return 0;
}
