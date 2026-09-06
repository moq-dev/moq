// Subscribe-only cross-language interop client for the smoke test, linked
// against the workspace libmoq (the C bindings, built by `cargo build -p libmoq`).
//
// libmoq is a handle + callback API: connect, consume a broadcast, get a
// catalog snapshot via callback, start the video track, and a frame callback
// fires as frames arrive. Once a non-empty frame lands we close and drain every
// registration, then return 0; a timeout exits 1. Publishing isn't wired up: the
// raw-stream importer that the other clients use to publish isn't part of this
// subscribe-only client.
//
//   c-smoke subscribe --url http://127.0.0.1:4443 --broadcast b.hang --timeout 20
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
    int32_t broadcast; // handle delivered by moq_origin_consume_announced (0 until it arrives)
    int32_t catalog;
    int32_t video_track; // handle from moq_consume_video (0 until on_catalog starts it)

    // One flag per registration handed &ctx, set by that registration's terminal
    // (<= 0) callback. main returns only once all four are set: that terminal is
    // libmoq's documented last touch of user_data, so it is what makes main's
    // frame safe to destroy.
    int done_status, done_broadcast, done_catalog, done_frame;
} ctx_t;

// Callbacks run on libmoq's runtime thread; main waits on the condvar. ctx lives
// on main's stack and libmoq keeps the pointer until each registration's
// terminal (<= 0) callback fires, so main must not return until every one of
// them has. Closing the session ends its status registration alone;
// moq_origin_consume_announced, moq_consume_catalog and moq_consume_video each
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

// Delivers the broadcast handle once it's announced, then once more with a
// terminal code (<= 0). Store the first positive handle and wake main.
static void on_broadcast(void *ud, int32_t broadcast) {
    ctx_t *c = (ctx_t *)ud;
    if (broadcast <= 0) { // 0 = ended, negative = error
        done(c, &c->done_broadcast);
        return;
    }
    pthread_mutex_lock(&c->mu);
    if (c->broadcast <= 0) {
        c->broadcast = broadcast;
        pthread_cond_broadcast(&c->cv);
    }
    pthread_mutex_unlock(&c->mu);
}

static void on_catalog(void *ud, int32_t catalog) {
    ctx_t *c = (ctx_t *)ud;
    if (catalog <= 0) {
        done(c, &c->done_catalog);
        return;
    }

    pthread_mutex_lock(&c->mu);
    int start = c->video_track <= 0;
    pthread_mutex_unlock(&c->mu);

    if (start) {
        // A lazy publisher may announce video in a later catalog update, so this
        // just no-ops (config returns < 0) until a video track exists at index 0.
        moq_video_config vcfg;
        memset(&vcfg, 0, sizeof(vcfg));
        if (moq_consume_video_config((uint32_t)catalog, 0, &vcfg) == 0) {
            int32_t track = moq_consume_video((uint32_t)catalog, 0, 1000, on_frame, ud);
            if (track > 0) {
                pthread_mutex_lock(&c->mu);
                c->video_track = track;
                pthread_mutex_unlock(&c->mu);
            }
        }
    }
    moq_consume_catalog_free((uint32_t)catalog);
}

// Report a failure and exit 1, for the paths where libmoq still holds &ctx.
//
// _exit rather than a return: it leaves main's frame (and so ctx) intact for the
// callbacks still pointing at it, which is what the failure paths want. Draining
// first would work too, but a client that has already failed should print its
// reason and go, not block for up to ten seconds on a teardown that may be exactly
// what is broken.
static _Noreturn void fail(const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    vfprintf(stderr, fmt, ap);
    va_end(ap);
    fflush(stderr);
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
    // A video track that was never started registered nothing, so it owes no terminal.
    if (track <= 0) c->done_frame = 1;
    pthread_mutex_unlock(&c->mu);

    if (track > 0) moq_consume_video_close((uint32_t)track);
    moq_consume_catalog_close((uint32_t)c->catalog);
    moq_origin_consume_announced_close((uint32_t)c->broadcast_wait);
    moq_session_close((uint32_t)c->session);

    struct timespec deadline;
    clock_gettime(CLOCK_REALTIME, &deadline);
    deadline.tv_sec += 10;

    pthread_mutex_lock(&c->mu);
    while (!(c->done_frame && c->done_catalog && c->done_broadcast && c->done_status)) {
        if (pthread_cond_timedwait(&c->cv, &c->mu, &deadline) != 0) break; // timed out
    }
    int ok = c->done_frame && c->done_catalog && c->done_broadcast && c->done_status;
    int f = c->done_frame, ct = c->done_catalog, b = c->done_broadcast, s = c->done_status;
    pthread_mutex_unlock(&c->mu);

    if (!ok) {
        fail("error: teardown never quiesced (frame=%d catalog=%d broadcast=%d status=%d)\n", f, ct, b, s);
    }
}

int main(int argc, char **argv) {
    const char *url = NULL, *broadcast = NULL;
    double timeout_s = 20.0;
    for (int i = 1; i < argc; i++) {
        if (!strcmp(argv[i], "--url") && i + 1 < argc) url = argv[++i];
        else if (!strcmp(argv[i], "--broadcast") && i + 1 < argc) broadcast = argv[++i];
        else if (!strcmp(argv[i], "--timeout") && i + 1 < argc) timeout_s = atof(argv[++i]);
        // a leading "subscribe" positional (and anything else) is ignored.
    }
    if (!url || !broadcast) {
        fprintf(stderr, "usage: c-smoke subscribe --url U --broadcast B [--timeout S]\n");
        return 2;
    }

    ctx_t c;
    memset(&c, 0, sizeof(c));
    pthread_mutex_init(&c.mu, NULL);
    pthread_cond_init(&c.cv, NULL);

    c.origin = moq_origin_create();
    if (c.origin <= 0) {
        fprintf(stderr, "error: moq_origin_create failed: %d\n", c.origin);
        return 1;
    }

    // origin_publish = 0 disables publishing; consume via our origin.
    c.session = moq_session_connect(url, strlen(url), NULL, 0, (uint32_t)c.origin, on_status, &c);
    if (c.session <= 0) {
        // A registration that fails never invokes its callback, so &c isn't held
        // yet and returning is still safe here.
        fprintf(stderr, "error: moq_session_connect failed: %d\n", c.session);
        return 1;
    }

    struct timespec deadline;
    clock_gettime(CLOCK_REALTIME, &deadline);
    deadline.tv_sec += (time_t)timeout_s;

    // The broadcast arrives over the network after connect, so wait for it to be
    // announced. moq_origin_consume_announced resolves via on_broadcast once it's
    // available; we block on the condvar until then (or the deadline).
    c.broadcast_wait =
        moq_origin_consume_announced((uint32_t)c.origin, broadcast, strlen(broadcast), on_broadcast, &c);
    if (c.broadcast_wait <= 0) {
        fail("error: moq_origin_consume_announced failed: %d\n", c.broadcast_wait);
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
    while (!c.got) {
        if (pthread_cond_timedwait(&c.cv, &c.mu, &deadline) != 0) break; // timed out
    }
    int got = c.got;
    pthread_mutex_unlock(&c.mu);

    if (!got) {
        fail("error: timed out waiting for data\n");
    }

    fprintf(stderr, "received a frame from %s\n", broadcast);

    // The data path succeeded, which is all this smoke client verifies. Returning
    // (rather than _exit) is the other half of what it verifies: an embedder that
    // closes and drains gets a clean process exit, with libmoq's runtime thread
    // still live behind its LazyLock.
    drain(&c);
    moq_consume_close((uint32_t)bc);
    moq_origin_close((uint32_t)c.origin);
    fflush(stderr);
    return 0;
}
