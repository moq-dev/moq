// An on-demand publisher ends a track nobody wants by freeing its handle with `*_free`. Each
// handle that can serve one is checked both ways: freed idle, the track ends cleanly and the
// handle is released; freed while a reader that joined since still wants the track, the broadcast
// asks its request handler for the track again, the handler serves it with a handle of the same
// kind, and the reader reads what that one writes. The reader joins through an origin, as a
// remote one does.
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
    // Accept a track asked for again as the handle was published, which its reader already
    // holds, and write to it from SERVED_AT on, returning the new handle.
    int32_t (*serve)(uint32_t request);
    // End what `serve` returned.
    int32_t (*finish)(uint32_t handle);
} Handle;

// Where what a served handle writes starts, past anything the freed one wrote.
#define SERVED_AT 10000000

// hang's media track info: a microsecond timescale and a 30-second max age, at the kind's priority.
#define MEDIA_INFO(prio) {prio, 30000000, false, 1000000, true, 0, false, 0, false, MOQ_COMPRESSION_NONE}
static const moq_track_info AUDIO_INFO = MEDIA_INFO(80);

// Accept `request` as a raw track with `info`, and write one frame to it.
static int32_t serve_raw(uint32_t request, const moq_track_info *info) {
    int32_t track = moq_track_request_accept(request, info);
    CHECK(track > 0);
    moq_timestamp timestamp = {SERVED_AT, 1000000, true};
    CHECK(moq_publish_track_frame((uint32_t)track, (const uint8_t *)"served again", 12, &timestamp) == 0);
    return track;
}

// Whether `read` was written by a served handle.
static bool served(const moq_frame *read) {
    const moq_timestamp *at = read->timestamp;
    return at->has_scale && at->value * 1000000 >= (uint64_t)SERVED_AT * at->scale;
}

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

// A handle whose reader joined is freed: the broadcast asks for the track again, and the reader
// reads what the handle serving that request writes, past any group it already read.
static void check_wanted(const Handle *ops, uint32_t handle, const char *name, int32_t reader, Queue *frames) {
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
    int32_t next_handle = ops->serve((uint32_t)request);
    CHECK(next_handle > 0);

    // What the freed handle already published may come first, and a group it left open is a gap
    // the reader reads past.
    for (;;) {
        int32_t frame = next(frames);
        CHECK(frame > 0);
        moq_raw_read read;
        memset(&read, 0, sizeof(read));
        CHECK(moq_consume_track_read((uint32_t)frame, &read) == 0);
        bool done = read.kind == MOQ_RAW_READ_FRAME && served(&read.frame);
        CHECK(moq_consume_track_read_free((uint32_t)frame) == 0);
        if (done) break;
    }

    // The watcher follows the track, not the freed handle, so it is stopped here.
    CHECK(moq_publish_demand_cancel((uint32_t)watcher) == 0);
    while (next(demand) > 0) {}
    // Cancelled before the finish, which would otherwise end the reader first and leave nothing
    // to cancel.
    CHECK(moq_consume_track_cancel((uint32_t)reader) == 0);
    for (int32_t frame; (frame = next(frames)) > 0;) {
        CHECK(moq_consume_track_read_free((uint32_t)frame) == 0);
    }
    // The origin keeps a joined track wanted past the reader's leaving, so it is finished instead.
    CHECK(ops->finish((uint32_t)next_handle) == 0);
}

// ---- raw tracks ----

static int32_t track_demand(uint32_t track, moq_status_callback on_demand, void *user_data) {
    return moq_publish_track_demand(track, on_demand, user_data);
}

static int32_t serve_track(uint32_t request) {
    return serve_raw(request, NULL);
}

static const Handle TRACK = {moq_publish_track_free, track_demand, serve_track, moq_publish_track_finish};

// ---- media tracks ----

static const uint8_t OPUS_HEAD[19] = {'O', 'p', 'u', 's', 'H', 'e', 'a', 'd', 1, 2, 0, 0, 0x80, 0xbb, 0, 0, 0, 0, 0};

static int32_t media_demand(uint32_t media, moq_status_callback on_demand, void *user_data) {
    return moq_publish_media_demand(media, on_demand, user_data);
}

// A media track's renewal is accepted raw, with the info the importer published it with.
static int32_t serve_media(uint32_t request) {
    return serve_raw(request, &AUDIO_INFO);
}

static const Handle MEDIA = {moq_publish_media_free, media_demand, serve_media, moq_publish_track_finish};

// ---- encoded audio ----

static int32_t audio_demand(uint32_t producer, moq_status_callback on_demand, void *user_data) {
    return moq_encode_audio_demand(producer, on_demand, user_data);
}

static const moq_audio_encoder_input AUDIO_INPUT = {MOQ_AUDIO_SAMPLE_FORMAT_F32, 48000, 2};
static const moq_audio_encoder_output AUDIO_OUTPUT = {"pcm", 3, 0, 0, 0, 0};

static int32_t encode_audio(uint32_t broadcast, const char *name) {
    return moq_encode_audio(broadcast, name, strlen(name), &AUDIO_INPUT, &AUDIO_OUTPUT, 0);
}

// 20 ms of stereo silence.
static float silence[960 * 2];

static int32_t serve_audio(uint32_t request) {
    int32_t producer = moq_track_request_encode_audio(request, &AUDIO_INPUT, &AUDIO_OUTPUT, 0);
    CHECK(producer > 0);
    for (uint64_t i = 0; i < 3; i++) {
        moq_audio_frame frame = {{SERVED_AT + i * 20000, 1000000, true}, (const uint8_t *)silence, sizeof(silence)};
        CHECK(moq_encode_audio_frame((uint32_t)producer, &frame) == 0);
    }
    return producer;
}

static const Handle AUDIO = {moq_encode_audio_free, audio_demand, serve_audio, moq_encode_audio_finish};

// ---- encoded video ----

#define WIDTH 320
#define HEIGHT 240

static uint8_t gray_rgba[WIDTH * HEIGHT * 4];

static void write_video(uint32_t producer, uint64_t start) {
    for (uint64_t i = 0; i < 5; i++) {
        moq_video_encoder_frame frame = {{start + i * 33333, 1000000, true}, gray_rgba, sizeof(gray_rgba)};
        CHECK(moq_encode_video_frame(producer, &frame) == 0);
    }
}

static int32_t video_demand(uint32_t producer, moq_status_callback on_demand, void *user_data) {
    return moq_encode_video_demand(producer, on_demand, user_data);
}

static const moq_video_encoder_input VIDEO_INPUT = {MOQ_VIDEO_PIXEL_FORMAT_RGBA, WIDTH, HEIGHT, {30000, 1001}};
static const moq_video_encoder_output VIDEO_OUTPUT = {
    MOQ_VIDEO_CODEC_H264, 0, 0, MOQ_VIDEO_ENCODER_KIND_SOFTWARE, NULL, 0,
};

static int32_t encode_video(uint32_t broadcast) {
    return moq_encode_video(broadcast, &VIDEO_INPUT, &VIDEO_OUTPUT, NULL);
}

static int32_t serve_video(uint32_t request) {
    int32_t producer = moq_track_request_encode_video(request, &VIDEO_INPUT, &VIDEO_OUTPUT, NULL);
    CHECK(producer > 0);
    write_video((uint32_t)producer, SERVED_AT);
    return producer;
}

static const Handle VIDEO = {moq_encode_video_free, video_demand, serve_video, moq_encode_video_finish};

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
    check_wanted(&TRACK, (uint32_t)track, "wanted", reader, track_frames);

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
    check_wanted(&MEDIA, (uint32_t)media, "requested", reader, media_frames);

    // Encoded audio.
    int32_t audio = encode_audio(broadcast, "idle-audio");
    CHECK(audio > 0);
    check_idle(&AUDIO, (uint32_t)audio);

    Queue *audio_frames = queue_new();
    audio = encode_audio(broadcast, "wanted-audio");
    CHECK(audio > 0);
    reader = join(consume, "wanted-audio", audio_frames);
    check_wanted(&AUDIO, (uint32_t)audio, "wanted-audio", reader, audio_frames);

    // Encoded video. A rendition is named by the catalog once the encoder has produced a keyframe,
    // so the idle one is checked first and the other is primed before its reader asks.
    int32_t video = encode_video(broadcast);
    CHECK(video > 0);
    check_idle(&VIDEO, (uint32_t)video);

    Queue *video_frames = queue_new();
    video = encode_video(broadcast);
    CHECK(video > 0);
    write_video((uint32_t)video, 0);
    char name[64];
    video_name(consume, name, sizeof(name));
    reader = join(consume, name, video_frames);
    check_wanted(&VIDEO, (uint32_t)video, name, reader, video_frames);

    CHECK(moq_publish_requests_cancel(handler) == 0);
    CHECK(moq_consume_close(consume) == 0);
    CHECK(moq_route_free((uint32_t)route) == 0);
    CHECK(moq_publish_close(broadcast) == 0);
    CHECK(moq_origin_close((uint32_t)origin) == 0);
    fprintf(stderr, "C *_free ends an idle track and hands a wanted one to the next request passed\n");
    return 0;
}
