// Exact timestamps across the installed C ABI: a 90 kHz tick and a present zero
// read back unchanged from raw frames and datagrams, a JSON value carries its
// frame's timestamp, and a write refuses an out-of-range timestamp or an
// untimed frame on a timed track. moq-c only creates timed tracks, so the
// untimed read path is covered by the Rust tests.
#include <moq.h>

#include <pthread.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#define CHECK(expr) do { if (!(expr)) { \
    fprintf(stderr, "%s:%d: %s (%s)\n", __FILE__, __LINE__, #expr, moq_error()); \
    abort(); \
} } while (0)

// Wait up to 10 seconds for `cond`, evaluated under the context lock.
#define WAIT_FOR(c, cond) do { \
    struct timespec deadline; \
    CHECK(clock_gettime(CLOCK_REALTIME, &deadline) == 0); \
    deadline.tv_sec += 10; \
    pthread_mutex_lock(&(c)->mutex); \
    while (!(cond)) CHECK(pthread_cond_timedwait(&(c)->changed, &(c)->mutex, &deadline) == 0); \
    pthread_mutex_unlock(&(c)->mutex); \
} while (0)

typedef struct {
    pthread_mutex_t mutex;
    pthread_cond_t changed;
    int32_t broadcast, frames[2], datagram, value;
    size_t frame_count;
    int request_done, frames_done, datagrams_done, values_done;
} Context;

// Record a delivered id in `slot`, or the terminal status in `done`.
static void deliver(Context *c, int32_t id, int32_t *slot, int *done) {
    pthread_mutex_lock(&c->mutex);
    if (id > 0) *slot = id;
    else { CHECK(id == 0); *done = 1; }
    pthread_cond_broadcast(&c->changed);
    pthread_mutex_unlock(&c->mutex);
}

static void on_broadcast(void *data, int32_t id) {
    Context *c = data;
    deliver(c, id, &c->broadcast, &c->request_done);
}

static void on_frame(void *data, int32_t id) {
    Context *c = data;
    pthread_mutex_lock(&c->mutex);
    if (id > 0) { CHECK(c->frame_count < 2); c->frames[c->frame_count++] = id; }
    else { CHECK(id == 0); c->frames_done = 1; }
    pthread_cond_broadcast(&c->changed);
    pthread_mutex_unlock(&c->mutex);
}

static void on_datagram(void *data, int32_t id) {
    Context *c = data;
    deliver(c, id, &c->datagram, &c->datagrams_done);
}

static void on_value(void *data, int32_t id) {
    Context *c = data;
    deliver(c, id, &c->value, &c->values_done);
}

static bool same(const moq_timestamp *got, moq_timestamp want) {
    return got != NULL && got->value == want.value && got->scale == want.scale;
}

int main(void) {
    Context c = {.mutex = PTHREAD_MUTEX_INITIALIZER, .changed = PTHREAD_COND_INITIALIZER};
    int32_t origin = moq_origin_create();
    CHECK(origin > 0);
    const char *path = "timestamps";
    uint32_t broadcast = 0;
    int32_t publication = moq_origin_create_broadcast((uint32_t)origin, path, strlen(path), &broadcast);
    CHECK(publication > 0);
    CHECK(moq_publication_announce((uint32_t)publication, NULL) == 0);
    CHECK(moq_publication_free((uint32_t)publication) == 0);
    moq_track_info info = {0};
    info.timescale = 90000;
    info.timescale_present = true;
    int32_t track = moq_publish_track((uint32_t)broadcast, "ticks", 5, &info);
    CHECK(track > 0);
    int32_t json = moq_publish_json_stream((uint32_t)broadcast, "json", 4);
    CHECK(json > 0);

    const uint8_t payload[] = {1, 2, 3};
    const char *doc = "{\"n\":1}";
    const moq_timestamp invalid[] = {{0, 0}, {0, 1ull << 62}, {1ull << 61, 1000000}};
    for (size_t i = 0; i < sizeof(invalid) / sizeof(invalid[0]); i++) {
        CHECK(moq_publish_track_frame((uint32_t)track, payload, sizeof(payload), &invalid[i]) ==
              MOQ_ERROR_TIMESTAMP_OVERFLOW);
        CHECK(moq_publish_json_stream_append((uint32_t)json, doc, strlen(doc), &invalid[i]) ==
              MOQ_ERROR_TIMESTAMP_OVERFLOW);
    }
    CHECK(moq_publish_track_frame((uint32_t)track, payload, sizeof(payload), NULL) == MOQ_ERROR_MOQ);

    CHECK(moq_origin_request((uint32_t)origin, path, strlen(path), on_broadcast, &c) > 0);
    WAIT_FOR(&c, c.broadcast);
    int32_t frames = moq_consume_track((uint32_t)c.broadcast, "ticks", 5, NULL, on_frame, &c);
    CHECK(frames > 0);
    int32_t datagrams = moq_consume_datagrams((uint32_t)c.broadcast, "ticks", 5, on_datagram, &c);
    CHECK(datagrams > 0);
    int32_t values = moq_consume_json_stream((uint32_t)c.broadcast, "json", 4, on_value, &c);
    CHECK(values > 0);

    // One group at a time, so the default subscription never skips the older one, and in
    // time order, since a group may not start before the one preceding it.
    const moq_timestamp zero = {0, 90000}, tick = {1, 90000};
    CHECK(moq_publish_track_frame((uint32_t)track, payload, sizeof(payload), &zero) == 0);
    WAIT_FOR(&c, c.frame_count == 1);
    CHECK(moq_publish_track_frame((uint32_t)track, payload, sizeof(payload), &tick) == 0);
    WAIT_FOR(&c, c.frame_count == 2);
    CHECK(moq_publish_track_datagram((uint32_t)track, payload, sizeof(payload), &tick, NULL) == 0);
    WAIT_FOR(&c, c.datagram);
    // JSON tracks moq-c creates count milliseconds.
    const moq_timestamp stamped = {1500, 1000};
    CHECK(moq_publish_json_stream_append((uint32_t)json, doc, strlen(doc), &stamped) == 0);
    WAIT_FOR(&c, c.value);

    // Each timestamp points into its own read, so both stay intact until freed.
    moq_frame first, second;
    CHECK(moq_consume_track_frame((uint32_t)c.frames[0], &first) == 0);
    CHECK(moq_consume_track_frame((uint32_t)c.frames[1], &second) == 0);
    CHECK(same(first.timestamp, zero));
    CHECK(same(second.timestamp, tick));
    CHECK(moq_consume_track_frame_free((uint32_t)c.frames[0]) == 0);
    CHECK(moq_consume_track_frame_free((uint32_t)c.frames[1]) == 0);

    moq_datagram datagram;
    CHECK(moq_consume_datagram((uint32_t)c.datagram, &datagram) == 0);
    CHECK(same(datagram.timestamp, tick));
    CHECK(moq_consume_datagram_free((uint32_t)c.datagram) == 0);

    moq_json_value value;
    CHECK(moq_consume_json_value((uint32_t)c.value, &value) == 0);
    CHECK(same(value.timestamp, stamped));
    CHECK(moq_consume_json_value_free((uint32_t)c.value) == 0);

    CHECK(moq_consume_track_cancel((uint32_t)frames) == 0);
    CHECK(moq_consume_datagrams_cancel((uint32_t)datagrams) == 0);
    CHECK(moq_consume_json_cancel((uint32_t)values) == 0);
    WAIT_FOR(&c, c.frames_done && c.datagrams_done && c.values_done);
    CHECK(moq_consume_close((uint32_t)c.broadcast) == 0);
    CHECK(moq_publish_json_stream_finish((uint32_t)json) == 0);
    CHECK(moq_publish_track_finish((uint32_t)track) == 0);
    CHECK(moq_publish_close((uint32_t)broadcast) == 0);
    CHECK(moq_origin_close((uint32_t)origin) == 0);
    WAIT_FOR(&c, c.request_done);
    fprintf(stderr, "C exact timestamps passed\n");
    return 0;
}
