// An on-demand publisher ends a track nobody wants by freeing its handle with `*_free`. Each
// handle that can serve one is checked both ways: freed idle, the track ends cleanly and the
// handle is released; freed while a reader that joined since still wants the track, the broadcast
// asks its request handler for the track again, and the reader reads what the next producer
// writes. The reader joins through an origin, as a remote one does.
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

#define QUEUE_SIZE 64

// The codes a callback delivered, in order.
typedef struct {
    pthread_mutex_t mutex;
    pthread_cond_t changed;
    int32_t codes[QUEUE_SIZE];
    size_t head, tail;
} Queue;

// Never freed: a callback may still deliver its terminal code after the check that made it ends.
static Queue *queue_new(void) {
    Queue *q = calloc(1, sizeof(*q));
    CHECK(q != NULL);
    CHECK(pthread_mutex_init(&q->mutex, NULL) == 0);
    CHECK(pthread_cond_init(&q->changed, NULL) == 0);
    return q;
}

static void on_code(void *data, int32_t code) {
    Queue *q = data;
    pthread_mutex_lock(&q->mutex);
    CHECK(q->tail - q->head < QUEUE_SIZE);
    q->codes[q->tail++ % QUEUE_SIZE] = code;
    pthread_cond_broadcast(&q->changed);
    pthread_mutex_unlock(&q->mutex);
}

// Wait up to 10 seconds for the next code, naming the caller's line if it never comes.
#define next(q) next_at(q, __LINE__)
static int32_t next_at(Queue *q, int line) {
    struct timespec deadline;
    CHECK(clock_gettime(CLOCK_REALTIME, &deadline) == 0);
    deadline.tv_sec += 10;
    pthread_mutex_lock(&q->mutex);
    while (q->head == q->tail) {
        if (pthread_cond_timedwait(&q->changed, &q->mutex, &deadline) != 0) {
            fprintf(stderr, "%s:%d: no callback within 10 seconds\n", __FILE__, line);
            abort();
        }
    }
    int32_t code = q->codes[q->head++ % QUEUE_SIZE];
    pthread_mutex_unlock(&q->mutex);
    return code;
}

// Wait for a demand watcher to report that someone wants the track.
static void wait_wanted(Queue *demand) {
    while (next(demand) != MOQ_DEMAND_WANTED) {}
}

typedef struct {
    int32_t (*free)(uint32_t handle);
    int32_t (*demand)(uint32_t handle, moq_status_callback on_demand, void *user_data);
} Handle;

// An idle handle ends: freeing it releases the handle and ends its watcher cleanly.
static void check_idle(const Handle *ops, uint32_t handle) {
    Queue *demand = queue_new();
    CHECK(ops->demand(handle, on_code, demand) > 0);
    CHECK(next(demand) == MOQ_DEMAND_UNWANTED);

    CHECK(ops->free(handle) == 0);
    CHECK(ops->free(handle) < 0);
    CHECK(next(demand) == 0);
}

// The handler that serves a track asked for again.
static uint32_t handler;
static Queue *requests;
static int64_t raw_at = 10000000;

// A handle whose reader joined is freed: the broadcast asks for the track again, and the reader
// reads what the raw track accepted from that request writes, past any group it already read.
static void check_wanted(const Handle *ops, uint32_t handle, const char *name, Queue *frames) {
    Queue *demand = queue_new();
    int32_t watcher = ops->demand(handle, on_code, demand);
    CHECK(watcher > 0);
    wait_wanted(demand);
    CHECK(ops->free(handle) == 0);
    CHECK(ops->free(handle) < 0);

    int32_t request = next(requests);
    CHECK(request > 0);
    moq_string requested = {NULL, 0};
    CHECK(moq_track_request_name((uint32_t)request, &requested) == 0);
    CHECK(requested.len == strlen(name) && memcmp(requested.data, name, requested.len) == 0);
    int32_t track = moq_track_request_accept((uint32_t)request, NULL);
    CHECK(track > 0);
    moq_timestamp timestamp = {(uint64_t)raw_at, 1000000, true};
    raw_at += 1000000;
    CHECK(moq_publish_track_frame((uint32_t)track, (const uint8_t *)"served again", 12, &timestamp) == 0);

    // What the freed handle already published may come first.
    for (;;) {
        int32_t frame = next(frames);
        CHECK(frame > 0);
        moq_frame read;
        memset(&read, 0, sizeof(read));
        CHECK(moq_consume_track_frame((uint32_t)frame, &read) == 0);
        bool served = read.payload_size == 12 && memcmp(read.payload, "served again", 12) == 0;
        CHECK(moq_consume_track_frame_free((uint32_t)frame) == 0);
        if (served) break;
    }

    // The watcher follows the track, not the freed handle, so it is stopped here.
    CHECK(moq_publish_demand_cancel((uint32_t)watcher) == 0);
    while (next(demand) > 0) {}
    // The origin keeps a joined track wanted past the reader's leaving, so it is finished instead.
    CHECK(moq_publish_track_finish((uint32_t)track) == 0);
}

// ---- raw tracks ----

static int32_t track_demand(uint32_t track, moq_status_callback on_demand, void *user_data) {
    return moq_publish_track_demand(track, on_demand, user_data);
}

static const Handle TRACK = {moq_publish_track_free, track_demand};

// ---- media tracks ----

static const uint8_t OPUS_HEAD[19] = {'O', 'p', 'u', 's', 'H', 'e', 'a', 'd', 1, 2, 0, 0, 0x80, 0xbb, 0, 0, 0, 0, 0};

static int32_t media_demand(uint32_t media, moq_status_callback on_demand, void *user_data) {
    return moq_publish_media_demand(media, on_demand, user_data);
}

static const Handle MEDIA = {moq_publish_media_free, media_demand};

// ---- encoded audio ----

static int32_t audio_demand(uint32_t producer, moq_status_callback on_demand, void *user_data) {
    return moq_encode_audio_demand(producer, on_demand, user_data);
}

static int32_t encode_audio(uint32_t broadcast, const char *name) {
    moq_audio_encoder_input input = {MOQ_AUDIO_SAMPLE_FORMAT_F32, 48000, 2};
    moq_audio_encoder_output output = {"pcm", 3, 0, 0, 0, 0};
    return moq_encode_audio(broadcast, name, strlen(name), &input, &output, 0);
}

static const Handle AUDIO = {moq_encode_audio_free, audio_demand};

// ---- encoded video ----

#define WIDTH 320
#define HEIGHT 240

static uint8_t gray_rgba[WIDTH * HEIGHT * 4];

static void write_video(uint32_t producer) {
    for (uint64_t i = 0; i < 5; i++) {
        moq_video_encoder_frame frame = {{i * 33333, 1000000, true}, gray_rgba, sizeof(gray_rgba)};
        CHECK(moq_encode_video_frame(producer, &frame) == 0);
    }
}

static int32_t video_demand(uint32_t producer, moq_status_callback on_demand, void *user_data) {
    return moq_encode_video_demand(producer, on_demand, user_data);
}

static int32_t encode_video(uint32_t broadcast) {
    moq_video_encoder_input input = {MOQ_VIDEO_PIXEL_FORMAT_RGBA, WIDTH, HEIGHT, {30000, 1001}};
    moq_video_encoder_output output = {MOQ_VIDEO_CODEC_H264, 0, 0, MOQ_VIDEO_ENCODER_KIND_SOFTWARE, NULL, 0};
    return moq_encode_video(broadcast, &input, &output, NULL);
}

static const Handle VIDEO = {moq_encode_video_free, video_demand};

// ---- the fixture ----

// Subscribe to `name` on `consume`, delivering its frames to `frames`.
static int32_t join(uint32_t consume, const char *name, Queue *frames) {
    int32_t reader = moq_consume_track(consume, name, strlen(name), NULL, on_code, frames);
    CHECK(reader > 0);
    return reader;
}

// The name of the first video rendition on `consume`'s catalog, once it has one.
static void video_name(uint32_t consume, char *name, size_t size) {
    Queue *catalogs = queue_new();
    int32_t task = moq_consume_catalog(consume, 0, on_code, catalogs);
    CHECK(task > 0);
    int32_t catalog = next(catalogs);
    CHECK(catalog > 0);
    moq_video_config config;
    memset(&config, 0, sizeof(config));
    CHECK(moq_consume_video_config((uint32_t)catalog, 0, &config) == 0);
    CHECK(config.name_len < size);
    memcpy(name, config.name, config.name_len);
    name[config.name_len] = 0;
    CHECK(moq_consume_catalog_free((uint32_t)catalog) == 0);
    CHECK(moq_consume_catalog_cancel((uint32_t)task) == 0);
}

int main(void) {
    memset(gray_rgba, 0x80, sizeof(gray_rgba));

    int32_t origin = moq_origin_create();
    CHECK(origin > 0);
    const char *path = "on-demand-free";
    int32_t route = moq_origin_route((uint32_t)origin, path, strlen(path), NULL);
    CHECK(route > 0);
    int32_t created = moq_route_broadcast((uint32_t)route);
    CHECK(created > 0);
    uint32_t broadcast = (uint32_t)created;

    requests = queue_new();
    int32_t requested = moq_publish_requests(broadcast, on_code, requests);
    CHECK(requested > 0);
    handler = (uint32_t)requested;

    // A reader joins through the origin, as a remote one does.
    Queue *broadcasts = queue_new();
    CHECK(moq_origin_request((uint32_t)origin, path, strlen(path), 0, on_code, broadcasts) > 0);
    int32_t joined = next(broadcasts);
    CHECK(joined > 0);
    uint32_t consume = (uint32_t)joined;

    // Raw tracks.
    int32_t track = moq_publish_track(broadcast, "idle", 4, NULL);
    CHECK(track > 0);
    check_idle(&TRACK, (uint32_t)track);

    Queue *track_frames = queue_new();
    track = moq_publish_track(broadcast, "wanted", 6, NULL);
    CHECK(track > 0);
    int32_t reader = join(consume, "wanted", track_frames);
    check_wanted(&TRACK, (uint32_t)track, "wanted", track_frames);
    CHECK(moq_consume_track_cancel((uint32_t)reader) == 0);

    // Media tracks, which a track request creates once the reader has asked for the name.
    moq_audio_init init = {MOQ_AUDIO_FORMAT_OPUS, OPUS_HEAD, sizeof(OPUS_HEAD), NULL, 0, {0}};
    int32_t media = moq_publish_audio(broadcast, &init);
    CHECK(media > 0);
    check_idle(&MEDIA, (uint32_t)media);

    Queue *media_frames = queue_new();
    reader = join(consume, "requested", media_frames);
    int32_t request = next(requests);
    CHECK(request > 0);
    media = moq_track_request_audio((uint32_t)request, &init);
    CHECK(media > 0);
    check_wanted(&MEDIA, (uint32_t)media, "requested", media_frames);
    CHECK(moq_consume_track_cancel((uint32_t)reader) == 0);

    // Encoded audio.
    int32_t audio = encode_audio(broadcast, "idle-audio");
    CHECK(audio > 0);
    check_idle(&AUDIO, (uint32_t)audio);

    Queue *audio_frames = queue_new();
    audio = encode_audio(broadcast, "wanted-audio");
    CHECK(audio > 0);
    reader = join(consume, "wanted-audio", audio_frames);
    check_wanted(&AUDIO, (uint32_t)audio, "wanted-audio", audio_frames);
    CHECK(moq_consume_track_cancel((uint32_t)reader) == 0);

    // Encoded video. A rendition is named by the catalog once the encoder has produced a keyframe,
    // so the idle one is checked first and the other is primed before its reader asks.
    int32_t video = encode_video(broadcast);
    CHECK(video > 0);
    check_idle(&VIDEO, (uint32_t)video);

    Queue *video_frames = queue_new();
    video = encode_video(broadcast);
    CHECK(video > 0);
    write_video((uint32_t)video);
    char name[64];
    video_name(consume, name, sizeof(name));
    reader = join(consume, name, video_frames);
    check_wanted(&VIDEO, (uint32_t)video, name, video_frames);
    CHECK(moq_consume_track_cancel((uint32_t)reader) == 0);

    CHECK(moq_publish_requests_cancel(handler) == 0);
    CHECK(moq_consume_close(consume) == 0);
    CHECK(moq_route_free((uint32_t)route) == 0);
    CHECK(moq_publish_close(broadcast) == 0);
    CHECK(moq_origin_close((uint32_t)origin) == 0);
    fprintf(stderr, "C *_free ends an idle track and hands a wanted one to the next request passed\n");
    return 0;
}
