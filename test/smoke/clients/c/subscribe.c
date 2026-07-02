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
} ctx_t;

// Callbacks run on libmoq's runtime thread while main waits on the condvar.
// The video track-consume task keeps firing on_frame until it's closed AND has
// delivered a terminal callback; moq_session_close only signals shutdown, it
// doesn't synchronously stop that task. So a frame callback can still be in
// flight when main is ready to leave. We terminate the terminal paths with
// _exit rather than returning, so main's stack frame (which backs ctx, mu, and
// cv) is never unwound out from under an in-flight callback. Returning normally
// let the racing on_frame lock a mutex on a dead stack frame and corrupt it,
// tripping a glibc tpp.c assertion (abort) on Linux.
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
    moq_consume_frame_close((uint32_t)frame);
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
            int32_t track = moq_consume_video_ordered((uint32_t)catalog, 0, 1000, on_frame, ud);
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

    // moq_origin_consume is a synchronous lookup, but the broadcast arrives over
    // the network after connect. Retry until it's announced (or we run out of
    // time). We don't enable libmoq logging, so the misses stay quiet.
    int32_t bc = -1;
    while (1) {
        bc = moq_origin_consume((uint32_t)origin, broadcast, strlen(broadcast));
        if (bc > 0) break;
        struct timespec now;
        clock_gettime(CLOCK_REALTIME, &now);
        if (now.tv_sec >= deadline.tv_sec) break;
        usleep(150 * 1000);
    }
    if (bc <= 0) {
        fprintf(stderr, "error: broadcast never announced (moq_origin_consume: %d)\n", bc);
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

    // The track-consume task is still running on libmoq's runtime thread and may
    // be mid on_frame, touching mu/cv on this stack frame. _exit ends the process
    // without unwinding the stack or running teardown, so that memory stays valid
    // for any in-flight callback (see the note above on_status). We already have
    // our verdict, so a clean session close isn't worth the teardown race.
    if (got) {
        fprintf(stderr, "received a frame from %s\n", broadcast);
        _exit(0);
    }
    fprintf(stderr, "error: timed out waiting for data\n");
    _exit(1);
}
