// Fixture for the C decoder-output contract: `moq_video_decoder_output`
// selects the decoded CPU pixel format and target size, and accepted requests
// produce that layout or fail explicitly.
//
// Compiled and run by `just rs c-tests` against this build's generated `moq.h`
// and `libmoq.a`, linked from outside cargo the way an embedder does, so this
// file must only use the stable C ABI:
//
//   1. Layout: compile-time asserts pin the struct fields and the pixel-format
//      discriminants the test below depends on.
//   2. Refusal: unsupported formats and sizes fail `moq_decode_video`
//      synchronously, with no session needed (validation runs before anything
//      is resolved).
//   3. Behavior: an in-process origin publishes a gray software-H.264 stream
//      which is decoded three times: the I420 native default, RGBA at native
//      size, and I420 resized. Each first frame must arrive at the requested
//      dimensions with the requested byte size.
#include <moq.h>

#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>
#include <pthread.h>
#include <stdarg.h>

// ---- layout: the ABI this fixture pins ----

_Static_assert(MOQ_VIDEO_PIXEL_FORMAT_I420 == 0, "I420 discriminant moved");
_Static_assert(MOQ_VIDEO_PIXEL_FORMAT_RGBA == 1, "RGBA discriminant moved");
_Static_assert(offsetof(moq_video_decoder_output, max_age_us) == 0, "max_age_us moved");
_Static_assert(offsetof(moq_video_decoder_output, format) == 8, "format moved");
_Static_assert(offsetof(moq_video_decoder_output, width) == 12, "width moved");
_Static_assert(offsetof(moq_video_decoder_output, height) == 16, "height moved");
_Static_assert(sizeof(moq_video_decoder_output) == 24, "decoder output size changed");

// Numeric codes from rs/libmoq/src/error.rs. They are the contract a C caller
// matches on; if they drift, this fixture fails and the header is stale.
#define MOQ_ERR_INVALID_POINTER -6
#define MOQ_ERR_INVALID_CODE -15
#define MOQ_ERR_CATALOG_NOT_FOUND -25
#define MOQ_ERR_INVALID_CONFIG -40

static _Noreturn void fail(const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    vfprintf(stderr, fmt, ap);
    va_end(ap);
    fflush(stderr);
    _exit(1);
}

static void check_refusals(void) {
    // Unknown pixel format.
    moq_video_decoder_output bad_format = {0, 999, 0, 0};
    if (moq_decode_video(1, 0, &bad_format, NULL, NULL) != MOQ_ERR_INVALID_CODE)
        fail("error: unknown format was not refused with InvalidCode: %s\n", moq_error());

    // Half-set, odd, and unrepresentable sizes.
    const uint32_t bad_sizes[][2] = {{320, 0}, {0, 240}, {321, 240}, {320, 241}, {UINT32_MAX - 1, UINT32_MAX - 1}};
    for (size_t i = 0; i < sizeof(bad_sizes) / sizeof(bad_sizes[0]); i++) {
        moq_video_decoder_output bad_size = {0, MOQ_VIDEO_PIXEL_FORMAT_I420, bad_sizes[i][0], bad_sizes[i][1]};
        if (moq_decode_video(1, 0, &bad_size, NULL, NULL) != MOQ_ERR_INVALID_CONFIG)
            fail("error: size %ux%u was not refused with InvalidConfig: %s\n", bad_sizes[i][0],
                 bad_sizes[i][1], moq_error());
    }

    // Null output.
    if (moq_decode_video(1, 0, NULL, NULL, NULL) != MOQ_ERR_INVALID_POINTER)
        fail("error: null output was not refused with InvalidPointer: %s\n", moq_error());

    // A valid request gets past validation and fails on the bogus catalog.
    moq_video_decoder_output valid = {0, MOQ_VIDEO_PIXEL_FORMAT_RGBA, 160, 120};
    if (moq_decode_video(INT32_MAX, 0, &valid, NULL, NULL) != MOQ_ERR_CATALOG_NOT_FOUND)
        fail("error: valid request did not reach catalog lookup: %s\n", moq_error());

    fprintf(stderr, "decoder output refusals ok\n");
}

// ---- behavior: publish in-process, decode three ways ----

#define WIDTH 320
#define HEIGHT 240

typedef struct {
    pthread_mutex_t mu;
    pthread_cond_t cv;
    // Broadcast/catalog rendezvous.
    int32_t broadcast; // delivered by on_broadcast (> 0)
    int done_broadcast, done_catalog_sub;
    int32_t catalog_snapshot; // first snapshot held for the decodes (0 until it arrives)
    // Current decode: what was requested and what arrived.
    uint32_t want_format, want_width, want_height, want_size;
    int got_frame, done_frame;
} ctx_t;

static void on_broadcast(void *ud, int32_t broadcast) {
    ctx_t *c = (ctx_t *)ud;
    pthread_mutex_lock(&c->mu);
    if (broadcast <= 0) {
        c->done_broadcast = 1;
    } else if (c->broadcast <= 0) {
        c->broadcast = broadcast;
    }
    pthread_cond_broadcast(&c->cv);
    pthread_mutex_unlock(&c->mu);
}

static void on_catalog(void *ud, int32_t catalog) {
    ctx_t *c = (ctx_t *)ud;
    if (catalog <= 0) {
        pthread_mutex_lock(&c->mu);
        c->done_catalog_sub = 1;
        pthread_cond_broadcast(&c->cv);
        pthread_mutex_unlock(&c->mu);
        return;
    }
    pthread_mutex_lock(&c->mu);
    if (c->catalog_snapshot <= 0) {
        c->catalog_snapshot = catalog;
        catalog = 0; // held; main frees it after the decodes
    }
    pthread_cond_broadcast(&c->cv);
    pthread_mutex_unlock(&c->mu);
    if (catalog > 0) moq_consume_catalog_free((uint32_t)catalog);
}

static void on_frame(void *ud, int32_t frame_id) {
    ctx_t *c = (ctx_t *)ud;
    if (frame_id <= 0) {
        pthread_mutex_lock(&c->mu);
        c->done_frame = 1;
        pthread_cond_broadcast(&c->cv);
        pthread_mutex_unlock(&c->mu);
        return;
    }
    moq_video_frame frame;
    memset(&frame, 0, sizeof(frame));
    if (moq_decode_video_frame((uint32_t)frame_id, &frame) == 0) {
        pthread_mutex_lock(&c->mu);
        if (!c->got_frame) {
            if (frame.width != c->want_width || frame.height != c->want_height ||
                frame.data_size != c->want_size) {
                pthread_mutex_unlock(&c->mu);
                fail("error: decoded %ux%u size %zu, want %ux%u size %u\n", frame.width, frame.height,
                     frame.data_size, c->want_width, c->want_height, c->want_size);
            }
            c->got_frame = 1;
        }
        pthread_cond_broadcast(&c->cv);
        pthread_mutex_unlock(&c->mu);
    }
    moq_decode_video_frame_free((uint32_t)frame_id);
}

static void wait_for(pthread_mutex_t *mu, pthread_cond_t *cv, const int *flag, double timeout_s) {
    struct timespec deadline;
    clock_gettime(CLOCK_REALTIME, &deadline);
    deadline.tv_sec += (time_t)timeout_s;
    while (!*flag) {
        if (pthread_cond_timedwait(cv, mu, &deadline) != 0) break;
    }
    if (!*flag) fail("error: timed out waiting\n");
}

static uint8_t gray_rgba[WIDTH * HEIGHT * 4];
static uint64_t next_ts;

static void publish_frames(int32_t producer, int count) {
    for (int i = 0; i < count; i++) {
        moq_video_encoder_frame frame = {next_ts, gray_rgba, sizeof(gray_rgba)};
        next_ts += 33333;
        int32_t rc = moq_encode_video_frame((uint32_t)producer, &frame);
        if (rc < 0) fail("error: moq_encode_video_frame failed: %d (%s)\n", rc, moq_error());
    }
}

// Decode once with `output`, verifying the first frame arrives at
// `want_width` x `want_height` with `want_size` bytes.
static void decode_once(ctx_t *c, int32_t catalog, const moq_video_decoder_output *output, uint32_t want_width,
                        uint32_t want_height, uint32_t want_size, int32_t producer) {
    pthread_mutex_lock(&c->mu);
    c->want_format = output->format;
    c->want_width = want_width;
    c->want_height = want_height;
    c->want_size = want_size;
    c->got_frame = 0;
    c->done_frame = 0;
    pthread_mutex_unlock(&c->mu);

    int32_t consumer = moq_decode_video((uint32_t)catalog, 0, output, on_frame, c);
    if (consumer <= 0) fail("error: moq_decode_video failed: %d (%s)\n", consumer, moq_error());

    // Keep feeding the encoder so the subscriber has live frames whatever
    // group boundary it landed on.
    publish_frames(producer, 15);

    pthread_mutex_lock(&c->mu);
    wait_for(&c->mu, &c->cv, &c->got_frame, 15.0);
    pthread_mutex_unlock(&c->mu);

    if (moq_decode_video_close((uint32_t)consumer) < 0)
        fail("error: moq_decode_video_close failed (%s)\n", moq_error());
    pthread_mutex_lock(&c->mu);
    wait_for(&c->mu, &c->cv, &c->done_frame, 10.0);
    pthread_mutex_unlock(&c->mu);

    fprintf(stderr, "decode %ux%u format %u ok\n", want_width, want_height, output->format);
}

int main(void) {
    memset(gray_rgba, 0x80, sizeof(gray_rgba));

    check_refusals();

    ctx_t c;
    memset(&c, 0, sizeof(c));
    pthread_mutex_init(&c.mu, NULL);
    pthread_cond_init(&c.cv, NULL);

    int32_t origin = moq_origin_create();
    if (origin <= 0) fail("error: moq_origin_create failed: %d\n", origin);

    const char *path = "c-decoder-output-test";
    int32_t broadcast = moq_origin_create_broadcast((uint32_t)origin, path, strlen(path));
    if (broadcast <= 0) fail("error: moq_origin_create_broadcast failed: %d\n", broadcast);
    if (moq_publish_announce((uint32_t)broadcast, NULL) < 0)
        fail("error: moq_publish_announce failed (%s)\n", moq_error());

    moq_video_encoder_input input = {MOQ_VIDEO_PIXEL_FORMAT_RGBA, WIDTH, HEIGHT, 30};
    moq_video_encoder_output output = {MOQ_VIDEO_CODEC_H264, 0, 0, MOQ_VIDEO_ENCODER_KIND_SOFTWARE, NULL, 0};
    int32_t producer = moq_encode_video((uint32_t)broadcast, &input, &output, 0);
    if (producer <= 0) fail("error: moq_encode_video failed: %d (%s)\n", producer, moq_error());

    if (moq_encode_video_cut((uint32_t)producer) < 0)
        fail("error: moq_encode_video_cut failed (%s)\n", moq_error());
    publish_frames(producer, 5);

    int32_t request = moq_origin_request((uint32_t)origin, path, strlen(path), on_broadcast, &c);
    if (request <= 0) fail("error: moq_origin_request failed: %d\n", request);

    pthread_mutex_lock(&c.mu);
    while (c.broadcast <= 0) wait_for(&c.mu, &c.cv, &c.broadcast, 10.0);
    int32_t consume = c.broadcast;
    pthread_mutex_unlock(&c.mu);

    int32_t catalog_sub = moq_consume_catalog((uint32_t)consume, on_catalog, &c);
    if (catalog_sub <= 0) fail("error: moq_consume_catalog failed: %d\n", catalog_sub);

    pthread_mutex_lock(&c.mu);
    while (c.catalog_snapshot <= 0) wait_for(&c.mu, &c.cv, &c.catalog_snapshot, 10.0);
    int32_t catalog = c.catalog_snapshot;
    pthread_mutex_unlock(&c.mu);

    // The I420 native default, RGBA at native size, and a resized I420.
    moq_video_decoder_output native_i420 = {10000000, MOQ_VIDEO_PIXEL_FORMAT_I420, 0, 0};
    decode_once(&c, catalog, &native_i420, WIDTH, HEIGHT, WIDTH * HEIGHT * 3 / 2, producer);
    moq_video_decoder_output native_rgba = {10000000, MOQ_VIDEO_PIXEL_FORMAT_RGBA, 0, 0};
    decode_once(&c, catalog, &native_rgba, WIDTH, HEIGHT, WIDTH * HEIGHT * 4, producer);
    moq_video_decoder_output small_i420 = {10000000, MOQ_VIDEO_PIXEL_FORMAT_I420, 160, 120};
    decode_once(&c, catalog, &small_i420, 160, 120, 160 * 120 * 3 / 2, producer);

    if (moq_consume_catalog_free((uint32_t)catalog) < 0)
        fail("error: moq_consume_catalog_free failed (%s)\n", moq_error());
    c.catalog_snapshot = 0;
    if (moq_consume_catalog_close((uint32_t)catalog_sub) < 0)
        fail("error: moq_consume_catalog_close failed (%s)\n", moq_error());
    // The request already terminated once it delivered the broadcast, so its
    // task is gone; close only a still-pending wait.
    pthread_mutex_lock(&c.mu);
    int request_pending = !c.done_broadcast;
    pthread_mutex_unlock(&c.mu);
    if (request_pending && moq_origin_request_close((uint32_t)request) < 0)
        fail("error: moq_origin_request_close failed (%s)\n", moq_error());
    if (moq_consume_close((uint32_t)consume) < 0)
        fail("error: moq_consume_close failed (%s)\n", moq_error());
    if (moq_encode_video_finish((uint32_t)producer) < 0)
        fail("error: moq_encode_video_finish failed (%s)\n", moq_error());
    if (moq_publish_finish((uint32_t)broadcast) < 0)
        fail("error: moq_publish_finish failed (%s)\n", moq_error());
    if (moq_origin_close((uint32_t)origin) < 0)
        fail("error: moq_origin_close failed (%s)\n", moq_error());

    // Every registration holding &c has been closed; wait for each terminal so
    // returning (and destroying c) cannot race a late callback.
    pthread_mutex_lock(&c.mu);
    wait_for(&c.mu, &c.cv, &c.done_broadcast, 10.0);
    wait_for(&c.mu, &c.cv, &c.done_catalog_sub, 10.0);
    pthread_mutex_unlock(&c.mu);

    fprintf(stderr, "c decoder output fixture passed\n");
    return 0;
}
