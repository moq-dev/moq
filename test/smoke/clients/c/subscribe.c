// Subscribe-only cross-language interop client for the smoke test, linked
// against the workspace libmoq (the C bindings, built by `cargo build -p libmoq`).
//
// libmoq is a handle + callback API: connect, consume a broadcast, get a
// catalog snapshot via callback, start the video track, and a frame callback
// fires as frames arrive. We exit 0 the moment a non-empty frame lands, 1 on
// timeout. Publishing isn't wired up: the raw-stream importer that the other
// clients use to publish isn't part of this subscribe-only client.
//
//   c-smoke subscribe --url http://127.0.0.1:4443 --broadcast b.hang --timeout 20
#include <moq.h>

#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

typedef struct {
    pthread_mutex_t mu;
    pthread_cond_t cv;
    int got;           // a non-empty frame arrived
    int video_started; // guard: start the video track only once
    int32_t broadcast; // handle delivered by moq_origin_consume_announced (0 until it arrives)
} ctx_t;

// Callbacks run on libmoq's runtime thread; main waits on the condvar. ctx
// lives on main's stack and outlives the callbacks (main blocks until got/
// timeout, then the process exits), so there's no use-after-free.
static void on_status(void *ud, int32_t code) {
    (void)ud;
    fprintf(stderr, "session status: %d\n", code);
}

static void on_frame(void *ud, int32_t frame) {
    ctx_t *c = (ctx_t *)ud;
    if (frame <= 0) return; // 0 = ended, negative = error
    moq_frame f;
    memset(&f, 0, sizeof(f));
    if (moq_consume_frame((uint32_t)frame, &f) == 0 && f.payload_size > 0) {
        pthread_mutex_lock(&c->mu);
        c->got = 1;
        pthread_cond_signal(&c->cv);
        pthread_mutex_unlock(&c->mu);
    }
    moq_consume_frame_free((uint32_t)frame);
}

// Delivers the broadcast handle once it's announced, then once more with a
// terminal code (<= 0) we ignore. Store the first positive handle and wake main.
static void on_broadcast(void *ud, int32_t broadcast) {
    ctx_t *c = (ctx_t *)ud;
    if (broadcast <= 0) return; // 0 = ended, negative = error
    pthread_mutex_lock(&c->mu);
    if (c->broadcast <= 0) {
        c->broadcast = broadcast;
        pthread_cond_signal(&c->cv);
    }
    pthread_mutex_unlock(&c->mu);
}

static void on_catalog(void *ud, int32_t catalog) {
    ctx_t *c = (ctx_t *)ud;
    if (catalog <= 0) return;

    pthread_mutex_lock(&c->mu);
    int start = !c->video_started;
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
                c->video_started = 1;
                pthread_mutex_unlock(&c->mu);
            }
        }
    }
    moq_consume_catalog_free((uint32_t)catalog);
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
    pthread_mutex_init(&c.mu, NULL);
    pthread_cond_init(&c.cv, NULL);
    c.got = 0;
    c.video_started = 0;
    c.broadcast = 0;

    int32_t origin = moq_origin_create();
    if (origin <= 0) {
        fprintf(stderr, "error: moq_origin_create failed: %d\n", origin);
        return 1;
    }

    // origin_publish = 0 disables publishing; consume via our origin.
    int32_t session = moq_session_connect(url, strlen(url), 0, (uint32_t)origin, on_status, &c);
    if (session <= 0) {
        fprintf(stderr, "error: moq_session_connect failed: %d\n", session);
        return 1;
    }

    struct timespec deadline;
    clock_gettime(CLOCK_REALTIME, &deadline);
    deadline.tv_sec += (time_t)timeout_s;

    // The broadcast arrives over the network after connect, so wait for it to be
    // announced. moq_origin_consume_announced resolves via on_broadcast once it's
    // available; we block on the condvar until then (or the deadline).
    int32_t wait = moq_origin_consume_announced((uint32_t)origin, broadcast, strlen(broadcast), on_broadcast, &c);
    if (wait <= 0) {
        fprintf(stderr, "error: moq_origin_consume_announced failed: %d\n", wait);
        return 1;
    }

    pthread_mutex_lock(&c.mu);
    while (c.broadcast <= 0) {
        if (pthread_cond_timedwait(&c.cv, &c.mu, &deadline) != 0) break; // timed out
    }
    int32_t bc = c.broadcast;
    pthread_mutex_unlock(&c.mu);
    if (bc <= 0) {
        fprintf(stderr, "error: broadcast never announced\n");
        return 1;
    }

    int32_t cat = moq_consume_catalog((uint32_t)bc, on_catalog, &c);
    if (cat <= 0) {
        fprintf(stderr, "error: moq_consume_catalog failed: %d\n", cat);
        return 1;
    }

    pthread_mutex_lock(&c.mu);
    while (!c.got) {
        if (pthread_cond_timedwait(&c.cv, &c.mu, &deadline) != 0) break; // timed out
    }
    int got = c.got;
    pthread_mutex_unlock(&c.mu);

    if (got) {
        fprintf(stderr, "received a frame from %s\n", broadcast);
        // The data path succeeded, which is all this smoke client verifies.
        // libmoq statically bundles moq-video (openh264/vaapi/cuda), whose
        // worker threads use priority-protected mutexes; tearing them down at
        // normal exit can trip a glibc pthread priority assertion and abort.
        // Skip that atexit teardown with _exit now that we have our result.
        fflush(stderr);
        _exit(0);
    }
    fprintf(stderr, "error: timed out waiting for data\n");
    return 1;
}
