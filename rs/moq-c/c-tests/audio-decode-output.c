// The installed C ABI owns every decoded read until read_free, independently
// of its consumer. PCM avoids a lossy-codec tolerance in this boundary test;
// moq-audio's independent-reference suite covers the codec samples themselves.
#include <moq.h>

#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#define CHECK(expr) do { if (!(expr)) { \
    fprintf(stderr, "%s:%d: %s (%s)\n", __FILE__, __LINE__, #expr, moq_error()); \
    abort(); \
} } while (0)

typedef struct {
    pthread_mutex_t mutex;
    pthread_cond_t changed;
    int32_t broadcast, catalog;
    int request_done, catalog_done, decode_done;
    uint32_t format, channels;
    size_t frames;
} Context;

static void wait_until(Context *c, const int *done) {
    struct timespec deadline;
    CHECK(clock_gettime(CLOCK_REALTIME, &deadline) == 0);
    deadline.tv_sec += 10;
    while (!*done) CHECK(pthread_cond_timedwait(&c->changed, &c->mutex, &deadline) == 0);
}

static void on_broadcast(void *data, int32_t id) {
    Context *c = data;
    pthread_mutex_lock(&c->mutex);
    if (id > 0) c->broadcast = id;
    else { CHECK(id == 0); c->request_done = 1; }
    pthread_cond_broadcast(&c->changed);
    pthread_mutex_unlock(&c->mutex);
}

static void on_catalog(void *data, int32_t id) {
    Context *c = data;
    pthread_mutex_lock(&c->mutex);
    if (id > 0 && !c->catalog) { c->catalog = id; id = 0; }
    else if (id <= 0) { CHECK(id == 0); c->catalog_done = 1; }
    pthread_cond_broadcast(&c->changed);
    pthread_mutex_unlock(&c->mutex);
    if (id > 0) CHECK(moq_consume_catalog_free((uint32_t)id) == 0);
}

static void check_frame(Context *c, const moq_audio_frame *frame) {
    CHECK(frame->timestamp_us == 123000 + c->frames * 1000000 / 48000);
    size_t bytes = c->format == MOQ_AUDIO_SAMPLE_FORMAT_S16 ? 2 : 4;
    CHECK(frame->data_size % (bytes * c->channels) == 0);
    size_t count = frame->data_size / (bytes * c->channels);
    CHECK(count > 0);
    for (size_t sample = 0; sample < count * c->channels; sample++) {
        size_t channel = c->format == MOQ_AUDIO_SAMPLE_FORMAT_F32_PLANAR ? sample / count : sample % c->channels;
        // Stereo is FL=+1/4, FR=-1/2; the documented mono mix averages them.
        float expected = c->channels == 1 ? -0.125f : channel == 0 ? 0.25f : -0.5f;
        if (bytes == 2) {
            int16_t value;
            memcpy(&value, frame->data + sample * bytes, bytes);
            CHECK(value == (c->channels == 1 ? -4096 : channel == 0 ? 8192 : -16384));
        } else {
            float value;
            memcpy(&value, frame->data + sample * bytes, bytes);
            CHECK(value == expected);
        }
    }
    c->frames += count;
}

static void on_read(void *data, int32_t id) {
    Context *c = data;
    pthread_mutex_lock(&c->mutex);
    if (id <= 0) {
        CHECK(id == 0);
        c->decode_done = 1;
    } else {
        moq_audio_read read;
        CHECK(moq_decode_audio_read((uint32_t)id, NULL) < 0);
        CHECK(moq_decode_audio_read((uint32_t)id, &read) == 0);
        // Continuous PCM loses nothing, and a whole last frame needs no end marker.
        CHECK(read.kind == MOQ_DECODE_READ_FRAME);
        check_frame(c, &read.frame);
        CHECK(moq_decode_audio_read_free((uint32_t)id) == 0);
        CHECK(moq_decode_audio_read((uint32_t)id, &read) < 0);
        CHECK(moq_decode_audio_read_free((uint32_t)id) < 0);
    }
    pthread_cond_broadcast(&c->changed);
    pthread_mutex_unlock(&c->mutex);
}

int main(void) {
    Context c = {.mutex = PTHREAD_MUTEX_INITIALIZER, .changed = PTHREAD_COND_INITIALIZER};
    moq_audio_decoder_output invalid = {999, 0, 0, 0};
    CHECK(moq_decode_audio(INT32_MAX, 0, &invalid, on_read, &c) == -15);
    CHECK(moq_decode_audio(INT32_MAX, 0, NULL, on_read, &c) == -6);

    int32_t origin = moq_origin_create();
    CHECK(origin > 0);
    const char *path = "audio-decode-output";
    uint32_t broadcast = 0;
    int32_t publication = moq_origin_create_broadcast((uint32_t)origin, path, strlen(path), &broadcast);
    CHECK(publication > 0);
    CHECK(moq_publication_announce((uint32_t)publication, NULL) == 0);
    CHECK(moq_publication_free((uint32_t)publication) == 0);
    moq_audio_encoder_input input = {MOQ_AUDIO_SAMPLE_FORMAT_F32, 48000, 2};
    moq_audio_encoder_output output = {"pcm", 3, 0, 0, 0, 0};
    int32_t producer = moq_encode_audio((uint32_t)broadcast, "audio", 5, &input, &output, 0);
    CHECK(producer > 0);
    float pcm[960 * 2];
    for (size_t sample = 0; sample < 960; sample++) { pcm[sample * 2] = 0.25f; pcm[sample * 2 + 1] = -0.5f; }
    moq_audio_frame frame = {123000, (const uint8_t *)pcm, sizeof(pcm)};
    CHECK(moq_encode_audio_frame((uint32_t)producer, &frame) == 0);
    CHECK(moq_encode_audio_finish((uint32_t)producer) == 0);

    CHECK(moq_origin_request((uint32_t)origin, path, strlen(path), NULL, 0, on_broadcast, &c) > 0);
    pthread_mutex_lock(&c.mutex);
    wait_until(&c, &c.broadcast);
    pthread_mutex_unlock(&c.mutex);
    int32_t catalogs = moq_consume_catalog((uint32_t)c.broadcast, on_catalog, &c);
    CHECK(catalogs > 0);
    pthread_mutex_lock(&c.mutex);
    wait_until(&c, &c.catalog);
    pthread_mutex_unlock(&c.mutex);

    // Each decode reads the finished track from the start. Its budget is the track's whole
    // retention, since the budget ages groups by wall clock and a stalled host would skip them.
    const uint32_t formats[] = {MOQ_AUDIO_SAMPLE_FORMAT_F32, MOQ_AUDIO_SAMPLE_FORMAT_F32_PLANAR, MOQ_AUDIO_SAMPLE_FORMAT_S16};
    for (size_t format = 0; format < sizeof(formats) / sizeof(formats[0]); format++) {
        for (uint32_t channels = 1; channels <= 2; channels++) {
            c.format = formats[format]; c.channels = channels; c.frames = 0; c.decode_done = 0;
            moq_audio_decoder_output decoded = {c.format, 48000, channels, 30000000};
            int32_t decoder = moq_decode_audio((uint32_t)c.catalog, 0, &decoded, on_read, &c);
            CHECK(decoder > 0);
            pthread_mutex_lock(&c.mutex);
            wait_until(&c, &c.decode_done);
            CHECK(c.frames == 960);
            pthread_mutex_unlock(&c.mutex);
            CHECK(moq_decode_audio_cancel((uint32_t)decoder) < 0); // A completed task is already released.
        }
    }
    CHECK(moq_consume_catalog_free((uint32_t)c.catalog) == 0);
    CHECK(moq_consume_catalog_cancel((uint32_t)catalogs) == 0);
    CHECK(moq_consume_close((uint32_t)c.broadcast) == 0);
    CHECK(moq_publish_close((uint32_t)broadcast) == 0);
    CHECK(moq_origin_close((uint32_t)origin) == 0);
    pthread_mutex_lock(&c.mutex);
    wait_until(&c, &c.request_done);
    wait_until(&c, &c.catalog_done);
    pthread_mutex_unlock(&c.mutex);
    fprintf(stderr, "C audio decode layout, remix, timeline and read ownership passed\n");


    return 0;
}
