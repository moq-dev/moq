// Subscribe-only cross-language interop client for the interop test, linked
// against the workspace moq-c (the C bindings, built by `cargo build -p moq-c`).
//
// moq-c is a handle + callback API: connect, consume a broadcast, get a
// catalog snapshot via callback, start the video track and a decoder per audio
// rendition, and a callback fires as each frame arrives. Once a non-empty video
// frame has landed and --audio-ms of every audio rendition has been decoded to
// --audio-out.<codec> (for tone.py to judge) we close and drain every
// registration, then return 0; a timeout exits 1.
// --finish instead verifies the complete finite tail, clean media/catalog EOF,
// withdrawal, and a live session.
// Publishing isn't wired up: the raw-stream importer that the other clients use
// to publish isn't part of this subscribe-only client.
//
//   c-interop subscribe --url http://127.0.0.1:4443 --broadcast b.hang --timeout 20 \
//       --audio-out /tmp/audio --audio-ms 600
//
// --version offers only that version, through moq_client_config.
#include <moq.h>

#include <errno.h>
#include <limits.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

typedef struct {
    int enabled, failed, announce_end, done_announce, done_health_wait, done_health;
    unsigned expected, frames, health_frames;
    int connected;
    int32_t frame_end, catalog_end, health_end;
    int32_t announcements, health_wait, health_broadcast;
    const char *path;
} finish_t;

typedef struct ctx ctx_t;

// One audio rendition's decoder, where it writes (PREFIX.<codec>.{txt,pcm}, the codec being the
// catalog's up to its first `.`), and how much it has written. `got` is set once `samples`
// reaches `want`, and `done` by the decoder's terminal callback.
typedef struct {
    ctx_t *ctx;
    int32_t track;
    FILE *log, *pcm;
    uint64_t samples, want;
    uint32_t frame_bytes;
    int got, done;
} audio_t;

// The most audio renditions this client decodes; a catalog offering more fails it.
#define MAX_AUDIO 4

// How far a read may fall behind live before it skips, in microseconds. A gap fails the run, which
// a reader skipping behind live would cause itself: the matrix starts every subscriber at once, and
// a runtime starved by the others can fall a second behind. That is this reader's speed, not the
// delivery under test. A track's max-age caps it, so interop.sh's Rust publisher keeps its media at
// least this long.
#define MAX_DELAY_US 5000000

struct ctx {
    pthread_mutex_t mu;
    pthread_cond_t cv;
    int got; // a non-empty frame arrived
    const char *error; // the first video gap or audio failure, which fails the run
    finish_t finish;
    int32_t origin;
    int32_t session;
    int32_t broadcast_wait;
    int32_t broadcast; // handle delivered by moq_origin_announced_broadcast (0 until it arrives)
    int32_t catalog;
    int32_t video_track; // handle from moq_consume_video (0 until on_catalog starts it)

    // The audio decoders, started together by the first catalog with audio.
    audio_t audio[MAX_AUDIO];
    uint32_t audio_count;
    const char *audio_out;
    uint64_t audio_ms;

    // One flag per registration handed &ctx, set by that registration's terminal
    // (<= 0) callback. main returns only once every registration is done: that terminal is
    // moq-c's documented last touch of user_data, so it is what makes main's
    // frame safe to destroy.
    int done_status, done_broadcast, done_catalog, done_frame;
};

// Callbacks run on moq-c's runtime thread; main waits on the condvar. ctx lives
// on main's stack and moq-c keeps the pointer until each registration's
// terminal (<= 0) callback fires, so main must not return until every one of
// them has. Closing the session ends its state registration alone;
// moq_origin_announced_broadcast, moq_consume_catalog and moq_consume_video each
// keep the pointer until their own terminal. See drain() at the bottom.
static void done(ctx_t *c, int *flag) {
    pthread_mutex_lock(&c->mu);
    *flag = 1;
    pthread_cond_broadcast(&c->cv);
    pthread_mutex_unlock(&c->mu);
}

static void on_state(void *ud, const moq_connection_state *state) {
    ctx_t *c = (ctx_t *)ud;
    fprintf(stderr, "session state: phase=%d connects=%llu failure=%d\n", (int)state->phase,
            (unsigned long long)state->connects, state->failure);
    if (c->finish.enabled && state->connects > 0) {
        pthread_mutex_lock(&c->mu);
        c->finish.connected = (int)state->connects;
        if (state->connects > 1) c->finish.failed = 1;
        pthread_cond_broadcast(&c->cv);
        pthread_mutex_unlock(&c->mu);
    }
    if (state->phase == MOQ_CONNECTION_PHASE_CLOSED) done(c, &c->done_status);
}

static void on_frame(void *ud, int32_t frame) {
    ctx_t *c = (ctx_t *)ud;
    if (frame <= 0) { // 0 = ended, negative = error
        pthread_mutex_lock(&c->mu);
        c->finish.frame_end = frame;
        if (c->finish.enabled && frame < 0) {
            fprintf(stderr, "finish media error: %d %s\n", frame, moq_error());
            c->finish.failed = 1;
        }
        pthread_mutex_unlock(&c->mu);
        done(c, &c->done_frame);
        return;
    }
    moq_media_read item = {0};
    int32_t read = moq_consume_read((uint32_t)frame, &item);
    pthread_mutex_lock(&c->mu);
    if (read < 0 && c->finish.enabled) c->finish.failed = 1;
    // Lost media fails the run; an encoder end only precedes fresh media.
    if (read == 0 && item.kind == MOQ_DECODE_READ_GAP) {
        if (c->finish.enabled) c->finish.failed = 1;
        if (!c->error) c->error = "media gap";
    }
    if (read == 0 && item.kind == MOQ_DECODE_READ_FRAME && item.frame.payload_size > 0) {
        c->got = 1;
        if (c->finish.enabled && ++c->finish.frames > c->finish.expected) c->finish.failed = 1;
    }
    pthread_cond_broadcast(&c->cv);
    pthread_mutex_unlock(&c->mu);
    moq_consume_read_free((uint32_t)frame);
}

// Writes each decoded audio frame to its rendition's files for tone.py, until enough has arrived.
// A gap or an encoder end before then fails the run, as the Rust client does.
// Runs on moq-c's runtime thread, the only writer, so the files need no lock.
static void on_audio(void *ud, int32_t id) {
    audio_t *a = (audio_t *)ud;
    ctx_t *c = a->ctx;
    if (id <= 0) { // 0 = ended, negative = error
        if (id < 0) fprintf(stderr, "audio decode error: %d\n", id);
        done(c, &a->done);
        return;
    }
    moq_audio_read read;
    memset(&read, 0, sizeof(read));
    int32_t status = moq_decode_audio_read((uint32_t)id, &read);
    if (status == 0 && read.kind != MOQ_DECODE_READ_FRAME && a->samples < a->want) {
        pthread_mutex_lock(&c->mu);
        if (!c->error) {
            c->error = read.kind == MOQ_DECODE_READ_GAP ? "audio gap" : "audio track ended early";
        }
        pthread_cond_broadcast(&c->cv);
        pthread_mutex_unlock(&c->mu);
    }
    moq_audio_frame f = read.frame;
    if (status == 0 && read.kind == MOQ_DECODE_READ_FRAME && a->samples < a->want) {
        uint64_t samples = f.data_size / a->frame_bytes;
        fprintf(a->log, "%llu %llu\n", (unsigned long long)f.timestamp_us, (unsigned long long)samples);
        fwrite(f.data, 1, f.data_size, a->pcm);
        a->samples += samples;
        if (a->samples >= a->want) {
            fflush(a->log);
            fflush(a->pcm);
            pthread_mutex_lock(&c->mu);
            a->got = 1;
            pthread_cond_broadcast(&c->cv);
            pthread_mutex_unlock(&c->mu);
        }
    }
    moq_decode_audio_read_free((uint32_t)id);
}

// Whether every audio rendition has decoded enough, once any has started. Called under the lock.
static int audio_got(ctx_t *c) {
    for (uint32_t i = 0; i < c->audio_count; i++) {
        if (!c->audio[i].got) return 0;
    }
    return c->audio_count > 0;
}

// Whether every started audio decoder has delivered its terminal. Called under the lock.
static int audio_done(ctx_t *c) {
    for (uint32_t i = 0; i < c->audio_count; i++) {
        if (!c->audio[i].done) return 0;
    }
    return 1;
}

// Opens one rendition's files exclusively, so two renditions of one codec fail rather than
// overwrite each other. Returns why it could not, or NULL.
static const char *open_audio(ctx_t *c, audio_t *a, const moq_audio_config *cfg) {
    if (cfg->channel_count == 0) return "an audio rendition without channels";
    size_t codec = 0;
    while (codec < cfg->codec_len && cfg->codec[codec] != '.') codec++;
    char path[4096];
    snprintf(path, sizeof(path), "%s.%.*s.txt", c->audio_out, (int)codec, cfg->codec);
    a->log = fopen(path, "wx");
    snprintf(path, sizeof(path), "%s.%.*s.pcm", c->audio_out, (int)codec, cfg->codec);
    a->pcm = fopen(path, "wbx");
    if (!a->log || !a->pcm) return "cannot create an audio rendition's files (a second rendition of one codec?)";
    a->ctx = c;
    a->frame_bytes = 4 * cfg->channel_count;
    a->want = c->audio_ms * cfg->sample_rate / 1000;
    fprintf(a->log, "%u %u\n", cfg->sample_rate, cfg->channel_count);
    return NULL;
}

// Starts a decoder for every audio rendition the catalog offers, at its codec's own rate and
// layout, so the catalog's are what tone.py checks.
static void start_audio(ctx_t *c, int32_t catalog) {
    moq_audio_config cfg;
    uint32_t count = 0;
    const char *error = NULL;
    for (;; count++) {
        memset(&cfg, 0, sizeof(cfg));
        if (moq_consume_audio_config((uint32_t)catalog, count, &cfg) != 0) break;
        if (count == MAX_AUDIO) {
            error = "the catalog offers more audio renditions than this client decodes";
            break;
        }
        error = open_audio(c, &c->audio[count], &cfg);
        if (error) break;
    }
    // Registered before any starts, so drain() owes a terminal for each one from here on.
    pthread_mutex_lock(&c->mu);
    c->audio_count = count;
    pthread_mutex_unlock(&c->mu);
    for (uint32_t i = 0; i < count && !error; i++) {
        moq_audio_decoder_output out;
        memset(&out, 0, sizeof(out));
        out.format = MOQ_AUDIO_SAMPLE_FORMAT_F32;
        out.max_delay_us = MAX_DELAY_US;
        int32_t track = moq_decode_audio((uint32_t)catalog, i, &out, on_audio, &c->audio[i]);
        pthread_mutex_lock(&c->mu);
        if (track > 0) c->audio[i].track = track;
        else {
            fprintf(stderr, "moq_decode_audio failed: %d\n", track);
            error = "moq_decode_audio failed";
        }
        pthread_mutex_unlock(&c->mu);
    }
    if (error) {
        pthread_mutex_lock(&c->mu);
        c->error = error;
        pthread_cond_broadcast(&c->cv);
        pthread_mutex_unlock(&c->mu);
    }
}

// Delivers a broadcast handle for each publisher instance serving the path, then
// a terminal code (<= 0). Store the first positive handle and wake main; one frame
// is all this client reads, so a later instance's handle is freed unread.
static void on_broadcast(void *ud, int32_t broadcast) {
    ctx_t *c = (ctx_t *)ud;
    if (broadcast <= 0) { // 0 = ended, negative = error
        if (c->finish.enabled) {
            if (broadcast < 0) fprintf(stderr, "finish broadcast error: %d %s\n", broadcast, moq_error());
            pthread_mutex_lock(&c->mu);
            c->finish.failed = 1;
            pthread_mutex_unlock(&c->mu);
        }
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
    if (!first) {
        if (c->finish.enabled) {
            pthread_mutex_lock(&c->mu);
            c->finish.failed = 1; // A replacement instance cannot finish this fixture.
            pthread_cond_broadcast(&c->cv);
            pthread_mutex_unlock(&c->mu);
        }
        moq_consume_close((uint32_t)broadcast);
    }
}

static void on_catalog(void *ud, int32_t catalog) {
    ctx_t *c = (ctx_t *)ud;
    if (catalog <= 0) {
        pthread_mutex_lock(&c->mu);
        c->finish.catalog_end = catalog;
        if (c->finish.enabled && (catalog < 0 || c->video_track <= 0)) {
            if (catalog < 0) fprintf(stderr, "finish catalog error: %d %s\n", catalog, moq_error());
            c->finish.failed = 1;
        }
        pthread_mutex_unlock(&c->mu);
        done(c, &c->done_catalog);
        return;
    }

    pthread_mutex_lock(&c->mu);
    int start = c->video_track <= 0;
    // A finish reader checks the video tail alone. Like video, a lazy publisher may add audio
    // in a later update, so the first catalog with audio starts every rendition it offers.
    int audio = !c->finish.enabled && c->audio_count == 0;
    pthread_mutex_unlock(&c->mu);
    if (audio) start_audio(c, catalog);

    if (start) {
        // A lazy publisher may announce video in a later catalog update, so this
        // just no-ops (config returns < 0) until a video track exists at index 0.
        moq_video_config vcfg;
        memset(&vcfg, 0, sizeof(vcfg));
        if (moq_consume_video_config((uint32_t)catalog, 0, &vcfg) == 0) {
            int32_t track = moq_consume_video((uint32_t)catalog, 0, MAX_DELAY_US, on_frame, ud);
            if (track > 0) {
                pthread_mutex_lock(&c->mu);
                c->video_track = track;
                pthread_mutex_unlock(&c->mu);
            } else if (c->finish.enabled) {
                fprintf(stderr, "finish video registration error: %d %s\n", track, moq_error());
                pthread_mutex_lock(&c->mu);
                c->finish.failed = 1;
                pthread_cond_broadcast(&c->cv);
                pthread_mutex_unlock(&c->mu);
            }
        }
    }
    moq_consume_catalog_free((uint32_t)catalog);
}

// An END is a route withdrawal, not the terminal of the announcement listener.
static void on_finish_announce(void *ud, int32_t event) {
    ctx_t *c = ud;
    if (event <= 0) {
        if (event < 0) {
            fprintf(stderr, "finish announcement error: %d %s\n", event, moq_error());
            pthread_mutex_lock(&c->mu);
            c->finish.failed = 1;
            pthread_mutex_unlock(&c->mu);
        }
        done(c, &c->finish.done_announce);
        return;
    }
    moq_announce_event info;
    int32_t read = moq_origin_announced_info((uint32_t)event, &info);
    pthread_mutex_lock(&c->mu);
    if (read < 0) c->finish.failed = 1;
    else if (info.prefix_len == strlen(c->finish.path) &&
             memcmp(info.prefix, c->finish.path, info.prefix_len) == 0 &&
             info.kind == MOQ_ANNOUNCE_KIND_END) c->finish.announce_end = 1;
    pthread_cond_broadcast(&c->cv);
    pthread_mutex_unlock(&c->mu);
    moq_origin_announced_free((uint32_t)event);
}

static void on_finish_health_frame(void *ud, int32_t frame) {
    ctx_t *c = ud;
    if (frame <= 0) {
        pthread_mutex_lock(&c->mu);
        c->finish.health_end = frame;
        if (frame < 0) {
            fprintf(stderr, "finish health error: %d %s\n", frame, moq_error());
            c->finish.failed = 1;
        }
        pthread_mutex_unlock(&c->mu);
        done(c, &c->finish.done_health);
        return;
    }
    moq_frame info;
    int32_t read = moq_consume_track_frame((uint32_t)frame, &info);
    const char payload[] = "session-alive";
    pthread_mutex_lock(&c->mu);
    if (read < 0 || info.payload_size != sizeof(payload) - 1 ||
        memcmp(info.payload, payload, sizeof(payload) - 1) != 0 ||
        info.timestamp_present || ++c->finish.health_frames != 1) c->finish.failed = 1;
    pthread_cond_broadcast(&c->cv);
    pthread_mutex_unlock(&c->mu);
    moq_consume_track_frame_free((uint32_t)frame);
}

static void on_finish_health(void *ud, int32_t broadcast) {
    ctx_t *c = ud;
    if (broadcast <= 0) {
        if (broadcast < 0) fprintf(stderr, "finish health broadcast error: %d %s\n", broadcast, moq_error());
        pthread_mutex_lock(&c->mu);
        c->finish.failed = 1;
        pthread_mutex_unlock(&c->mu);
        done(c, &c->finish.done_health_wait);
        return;
    }
    pthread_mutex_lock(&c->mu);
    int first = c->finish.health_broadcast == 0;
    if (first) c->finish.health_broadcast = broadcast;
    else c->finish.failed = 1;
    pthread_cond_broadcast(&c->cv);
    pthread_mutex_unlock(&c->mu);
    if (!first) {
        moq_consume_close((uint32_t)broadcast);
        return;
    }
    int32_t track = moq_consume_track((uint32_t)broadcast, "live", 4, NULL, on_finish_health_frame, ud);
    pthread_mutex_lock(&c->mu);
    if (track <= 0) c->finish.failed = 1;
    pthread_cond_broadcast(&c->cv);
    pthread_mutex_unlock(&c->mu);
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

// These assertions precede every local cancel, which would also produce terminal 0.
static void finish(ctx_t *c, const struct timespec *deadline) {
    pthread_mutex_lock(&c->mu);
    while (!c->finish.failed && !c->done_status && !c->finish.done_announce &&
           !(c->done_frame && c->done_catalog && c->finish.announce_end)) {
        if (pthread_cond_timedwait(&c->cv, &c->mu, deadline) != 0) break;
    }
    int ok = !c->finish.failed && !c->done_status && !c->finish.done_announce &&
             c->done_frame && c->done_catalog &&
             c->finish.frame_end == 0 && c->finish.catalog_end == 0 && c->finish.announce_end &&
             c->finish.frames == c->finish.expected;
    unsigned frames = c->finish.frames;
    pthread_mutex_unlock(&c->mu);
    if (!ok) fail("error: finish did not drain naturally (frames=%u expected=%u)\n", frames, c->finish.expected);

    size_t length = strlen(c->finish.path) + sizeof(".health");
    char *path = malloc(length);
    if (!path) fail("error: cannot allocate health path\n");
    snprintf(path, length, "%s.health", c->finish.path);
    c->finish.health_wait = moq_origin_announced_broadcast((uint32_t)c->origin, path, strlen(path), on_finish_health, c);
    free(path);
    if (c->finish.health_wait <= 0) fail("error: health registration failed\n");

    pthread_mutex_lock(&c->mu);
    while (!c->finish.failed && !c->done_status && !c->finish.done_health) {
        if (pthread_cond_timedwait(&c->cv, &c->mu, deadline) != 0) break;
    }
    ok = !c->finish.failed && !c->done_status && c->finish.done_health &&
         c->finish.health_end == 0 && c->finish.health_frames == 1;
    pthread_mutex_unlock(&c->mu);
    if (!ok) fail("error: session health failed after broadcast finish\n");
    fprintf(stderr, "finish clean frames=%u catalog=eof announce=end health=ok\n", frames);
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
    int32_t audio[MAX_AUDIO];
    uint32_t audio_count = c->audio_count;
    int cancel_frame = !c->finish.enabled || !c->done_frame;
    int cancel_catalog = !c->finish.enabled || !c->done_catalog;
    // A track that was never started registered nothing, so it owes no terminal.
    if (track <= 0) c->done_frame = 1;
    for (uint32_t i = 0; i < audio_count; i++) {
        audio[i] = c->audio[i].track;
        if (audio[i] <= 0) c->audio[i].done = 1;
    }
    pthread_mutex_unlock(&c->mu);

    if (track > 0 && cancel_frame) moq_consume_video_cancel((uint32_t)track);
    for (uint32_t i = 0; i < audio_count; i++) {
        if (audio[i] > 0) moq_decode_audio_cancel((uint32_t)audio[i]);
    }
    if (cancel_catalog) moq_consume_catalog_cancel((uint32_t)c->catalog);
    moq_origin_announced_broadcast_cancel((uint32_t)c->broadcast_wait);
    if (c->finish.enabled) {
        moq_origin_announced_cancel((uint32_t)c->finish.announcements);
        moq_origin_announced_broadcast_cancel((uint32_t)c->finish.health_wait);
    }
    moq_session_close((uint32_t)c->session);

    struct timespec deadline;
    clock_gettime(CLOCK_REALTIME, &deadline);
    deadline.tv_sec += 10;

    pthread_mutex_lock(&c->mu);
    while (!(c->done_frame && audio_done(c) && c->done_catalog && c->done_broadcast &&
             c->done_status &&
             (!c->finish.enabled || (c->finish.done_announce && c->finish.done_health_wait && c->finish.done_health)))) {
        if (pthread_cond_timedwait(&c->cv, &c->mu, &deadline) != 0) break; // timed out
    }
    int ok = c->done_frame && audio_done(c) && c->done_catalog && c->done_broadcast &&
             c->done_status &&
             (!c->finish.enabled || (c->finish.done_announce && c->finish.done_health_wait && c->finish.done_health));
    int f = c->done_frame, a = audio_done(c), ct = c->done_catalog, b = c->done_broadcast,
        s = c->done_status;
    pthread_mutex_unlock(&c->mu);

    if (!ok) {
        fail("error: teardown never quiesced (frame=%d audio=%d catalog=%d broadcast=%d status=%d)\n", f, a, ct, b,
             s);
    }
}

int main(int argc, char **argv) {
    const char *url = NULL, *broadcast = NULL, *version = NULL, *audio_out = NULL;
    double timeout_s = 20.0;
    int finish_mode = 0;
    unsigned frames = 0;
    long audio_ms = 0;
    for (int i = 1; i < argc; i++) {
        if (!strcmp(argv[i], "--url") && i + 1 < argc) url = argv[++i];
        else if (!strcmp(argv[i], "--broadcast") && i + 1 < argc) broadcast = argv[++i];
        else if (!strcmp(argv[i], "--timeout") && i + 1 < argc) timeout_s = atof(argv[++i]);
        else if (!strcmp(argv[i], "--version") && i + 1 < argc) version = argv[++i];
        else if (!strcmp(argv[i], "--audio-out") && i + 1 < argc) audio_out = argv[++i];
        else if (!strcmp(argv[i], "--audio-ms") && i + 1 < argc) audio_ms = atol(argv[++i]);
        else if (!strcmp(argv[i], "--finish")) finish_mode = 1;
        else if (!strcmp(argv[i], "--frames") && i + 1 < argc) {
            char *end;
            errno = 0;
            unsigned long value = strtoul(argv[++i], &end, 10);
            if (errno || !*argv[i] || argv[i][0] == '-' || *end || value == 0 || value > UINT_MAX) return 2;
            frames = (unsigned)value;
        }
        // a leading "subscribe" positional (and anything else) is ignored.
    }
    if (!url || !broadcast || (finish_mode ? frames == 0 || audio_out != NULL : frames != 0 || !audio_out || audio_ms <= 0)) {
        fprintf(stderr, "usage: c-interop subscribe --url U --broadcast B (--audio-out PREFIX --audio-ms MS | --finish --frames N) [--timeout S] [--version V]\n");
        return 2;
    }

    ctx_t c;
    memset(&c, 0, sizeof(c));
    c.finish.enabled = finish_mode;
    c.finish.expected = frames;
    c.finish.path = broadcast;
    pthread_mutex_init(&c.mu, NULL);
    pthread_cond_init(&c.cv, NULL);

    c.audio_out = audio_out;
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
    c.session = moq_session_connect(url, strlen(url), version ? &config : NULL, 0, (uint32_t)c.origin, on_state, &c);
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

    if (finish_mode) {
        pthread_mutex_lock(&c.mu);
        while (!c.finish.connected && !c.done_status && !c.finish.failed) {
            if (pthread_cond_timedwait(&c.cv, &c.mu, &deadline) != 0) break;
        }
        int ready = c.finish.connected == 1 && !c.done_status && !c.finish.failed;
        pthread_mutex_unlock(&c.mu);
        if (!ready) fail("error: finish reader did not establish its first session\n");
        puts("finish reader ready");
        fflush(stdout);
        char command[16];
        if (!fgets(command, sizeof(command), stdin) || strcmp(command, "read\n")) {
            fail("error: finish reader expected read command\n");
        }
        // Cold reader startup precedes publication; only the released read spends the linger.
        clock_gettime(CLOCK_REALTIME, &deadline);
        deadline.tv_sec += (time_t)timeout_s;

        moq_announce_config announce;
        memset(&announce, 0, sizeof(announce));
        announce.prefix = broadcast;
        announce.prefix_len = strlen(broadcast);
        announce.filter = ""; // Exact path only, excluding the health sibling.
        c.finish.announcements = moq_origin_announced((uint32_t)c.origin, &announce, on_finish_announce, &c);
        if (c.finish.announcements <= 0) fail("error: finish announcement registration failed\n");
    }

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

    if (finish_mode) finish(&c, &deadline);
    else {
        pthread_mutex_lock(&c.mu);
        while ((!c.got || !audio_got(&c)) && !c.error) {
            if (pthread_cond_timedwait(&c.cv, &c.mu, &deadline) != 0) break; // timed out
        }
        int got = c.got, audio = audio_got(&c);
        const char *error = c.error;
        pthread_mutex_unlock(&c.mu);

        if (error) fail("error: %s\n", error);
        if (!got || !audio) {
            fail("error: timed out waiting for data (video=%d audio=%d)\n", got, audio);
        }

        fprintf(stderr, "received a frame and %u audio renditions from %s\n", c.audio_count, broadcast);
    }

    // The data path succeeded, which is all this interop client verifies. Returning
    // (rather than _exit) is the other half of what it verifies: an embedder that
    // closes and drains gets a clean process exit, with moq-c's runtime thread
    // still live behind its LazyLock.
    drain(&c);
    if (finish_mode) moq_consume_close((uint32_t)c.finish.health_broadcast);
    for (uint32_t i = 0; i < c.audio_count; i++) {
        fclose(c.audio[i].log);
        fclose(c.audio[i].pcm);
    }
    moq_consume_close((uint32_t)bc);
    moq_origin_close((uint32_t)c.origin);
    fflush(stderr);
    return 0;
}
