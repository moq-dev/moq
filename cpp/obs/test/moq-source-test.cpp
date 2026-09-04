// SPDX-License-Identifier: GPL-2.0-or-later
//
// Drives the real moq_source, reached through the obs_source_info it registers,
// against stubbed libobs, libmoq and FFmpeg. Everything the consume path can get
// wrong is an ordering: an announcement that arrives after the session says it
// is connected, a delivery belonging to a connection that has already been
// replaced, a subscription whose terminal callback is the only thing that
// releases its lifetime reference. libmoq delivers all of those from its runtime
// thread at a moment the plugin does not choose, so the stubs below reproduce
// that thread and let each test pick the interleaving.
//
// Two invariants are checked after every scenario rather than per assertion:
// every bmalloc is matched by a bfree (a lifetime reference that never comes
// back leaves ctx leaked, so the count stays non-zero), and destruction returns
// on the terminal callbacks rather than on its own two-second backstop.
//
// Run with `just obs test` (ThreadSanitizer) or `just obs ci` (plain). This is
// not part of the plugin build.
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <deque>
#include <functional>
#include <future>
#include <map>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

#include <obs.h>
#include <obs-module.h>

extern "C" {
#include <libavcodec/avcodec.h>
#include <libavutil/imgutils.h>
#include <libavutil/pixdesc.h>
#include <libswscale/swscale.h>
#include "moq.h"
}

#include "moq-source.h"

namespace {
int g_failures = 0;

// Anything a stub itself considers impossible: a frame freed twice, a catalog
// snapshot used after it was freed. Counted rather than asserted inline, because
// the stubs run on the fake runtime thread where a failing CHECK would race the
// test thread.
std::atomic<int> g_stub_errors{0};
} // namespace

#define CHECK(cond)                                                             \
	do {                                                                    \
		if (!(cond)) {                                                  \
			fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); \
			g_failures++;                                           \
		}                                                               \
	} while (0)

// ------------------------------------------------------- the fake runtime thread

namespace {
// Stands in for libmoq's runtime thread. Every plugin callback is delivered from
// here and never inline from a close(), which is the property the source's
// disconnect path leans on: it closes each handle while holding ctx->mutex, and
// the terminal's reference release needs that same mutex.
class Runtime {
public:
	Runtime() : thread_([this] { Loop(); }) {}

	~Runtime()
	{
		{
			std::lock_guard<std::mutex> lock(mutex_);
			stop_ = true;
		}
		cv_.notify_all();
		thread_.join();
	}

	Runtime(const Runtime &) = delete;
	Runtime &operator=(const Runtime &) = delete;

	// Queue work without waiting, the way a close() posts a terminal: the caller
	// holds the source's mutex and must not block on the callback.
	void Post(std::function<void()> fn)
	{
		{
			std::lock_guard<std::mutex> lock(mutex_);
			queue_.push_back(std::move(fn));
		}
		cv_.notify_one();
	}

	// Queue work and wait for it, so a test can order deliveries exactly while
	// still running them on a thread the plugin does not own. The queue is FIFO,
	// so this also drains everything posted before it.
	void Run(std::function<void()> fn)
	{
		std::promise<void> done;
		auto ready = done.get_future();
		Post([&fn, &done] {
			fn();
			done.set_value();
		});
		ready.wait();
	}

private:
	void Loop()
	{
		for (;;) {
			std::function<void()> fn;
			{
				std::unique_lock<std::mutex> lock(mutex_);
				cv_.wait(lock, [this] { return stop_ || !queue_.empty(); });
				if (queue_.empty())
					return;
				fn = std::move(queue_.front());
				queue_.pop_front();
			}
			fn();
		}
	}

	std::mutex mutex_;
	std::condition_variable cv_;
	std::deque<std::function<void()>> queue_;
	bool stop_ = false;
	std::thread thread_;
};

Runtime *g_runtime = nullptr;
} // namespace

// ------------------------------------------------------------------ libobs stubs

namespace {
// The settings object the plugin reads through obs_data_get_string. The test
// owns it and hands its address across as an opaque obs_data_t.
struct Settings {
	std::string url;
	std::string broadcast;
};

Settings g_settings;

// OBS copies the registration struct, which is a stack local in
// register_moq_source, so keeping the pointer would dangle.
struct obs_source_info g_info = {};
bool g_registered = false;

// Written from the runtime thread (the decode path outputs frames) and read from
// the test thread.
std::atomic<int> g_output_frames{0};
std::atomic<int> g_blank_calls{0};
std::atomic<uint64_t> g_last_timestamp{0};

// The bmalloc/bfree balance. The source's whole teardown contract reduces to
// this: ctx is freed only once every subscription has delivered its terminal, so
// a reference that never comes back shows up here as a leak.
std::atomic<long> g_live_allocs{0};
} // namespace

extern "C" {

// Deliberately silent: stdio locking would create happens-before edges between
// the test thread and the fake runtime thread and mask the races we're after.
void blog(int, const char *, ...) {}

void *bmalloc(size_t size)
{
	g_live_allocs++;
	return malloc(size ? size : 1);
}

void *brealloc(void *ptr, size_t size)
{
	if (!ptr)
		g_live_allocs++;
	return realloc(ptr, size ? size : 1);
}

void bfree(void *ptr)
{
	if (!ptr)
		return;
	g_live_allocs--;
	free(ptr);
}

void *bmemdup(const void *ptr, size_t size)
{
	void *out = bmalloc(size);
	if (ptr && out)
		memcpy(out, ptr, size);
	return out;
}

const char *obs_data_get_string(obs_data_t *data, const char *name)
{
	auto *settings = reinterpret_cast<Settings *>(data);
	if (!settings)
		return "";
	if (strcmp(name, "url") == 0)
		return settings->url.c_str();
	if (strcmp(name, "broadcast") == 0)
		return settings->broadcast.c_str();
	return "";
}

void obs_data_set_default_string(obs_data_t *data, const char *name, const char *val)
{
	auto *settings = reinterpret_cast<Settings *>(data);
	if (!settings || !val)
		return;
	if (strcmp(name, "url") == 0)
		settings->url = val;
	else if (strcmp(name, "broadcast") == 0)
		settings->broadcast = val;
}

obs_properties_t *obs_properties_create(void)
{
	return reinterpret_cast<obs_properties_t *>(0x11);
}

obs_property_t *obs_properties_add_text(obs_properties_t *, const char *, const char *, enum obs_text_type)
{
	return reinterpret_cast<obs_property_t *>(0x12);
}

void obs_source_output_video(obs_source_t *, const struct obs_source_frame *frame)
{
	if (!frame) {
		g_blank_calls++;
		return;
	}
	g_last_timestamp = frame->timestamp;
	g_output_frames++;
}

void obs_register_source_s(const struct obs_source_info *info, size_t size)
{
	memcpy(&g_info, info, size < sizeof(g_info) ? size : sizeof(g_info));
	g_registered = true;
}

} // extern "C"

// ----------------------------------------------------------------- FFmpeg stubs

// Stubbed rather than linked, so the decode path is deterministic and spawns no
// threads of its own. This test is about the subscription bookkeeping wrapped
// around the decoder, and a real decoder's worker pool would only add noise
// under ThreadSanitizer.

namespace {
std::atomic<long> g_av_allocs{0};

// Decode knobs. Only the failure scenarios move them.
bool g_find_decoder_ok = true;
int g_send_result = 0;
int g_receive_result = 0;
int g_decoded_width = 320;
int g_decoded_height = 240;
std::atomic<int> g_last_decoder_extradata{-1};

std::atomic<int> g_sws_scales{0};

AVCodec g_fake_codec{};
uint8_t g_fake_plane[320 * 240] = {};
} // namespace

extern "C" {

const AVCodec *avcodec_find_decoder(enum AVCodecID id)
{
	return (g_find_decoder_ok && id != AV_CODEC_ID_NONE) ? &g_fake_codec : nullptr;
}

AVCodecContext *avcodec_alloc_context3(const AVCodec *)
{
	g_av_allocs++;
	return static_cast<AVCodecContext *>(calloc(1, sizeof(AVCodecContext)));
}

void avcodec_free_context(AVCodecContext **avctx)
{
	if (!avctx || !*avctx)
		return;
	if ((*avctx)->extradata) {
		g_av_allocs--;
		free((*avctx)->extradata);
	}
	g_av_allocs--;
	free(*avctx);
	*avctx = nullptr;
}

int avcodec_open2(AVCodecContext *, const AVCodec *, AVDictionary **)
{
	return 0;
}

void avcodec_flush_buffers(AVCodecContext *) {}

int avcodec_send_packet(AVCodecContext *ctx, const AVPacket *)
{
	g_last_decoder_extradata = ctx->extradata_size > 0 ? ctx->extradata[0] : -1;
	return g_send_result;
}

int avcodec_receive_frame(AVCodecContext *, AVFrame *frame)
{
	if (g_receive_result < 0)
		return g_receive_result;

	frame->format = AV_PIX_FMT_YUV420P;
	frame->width = g_decoded_width;
	frame->height = g_decoded_height;
	for (int i = 0; i < 3; i++) {
		frame->data[i] = g_fake_plane;
		frame->linesize[i] = g_decoded_width;
	}
	return 0;
}

AVPacket *av_packet_alloc(void)
{
	g_av_allocs++;
	return static_cast<AVPacket *>(calloc(1, sizeof(AVPacket)));
}

void av_packet_free(AVPacket **pkt)
{
	if (!pkt || !*pkt)
		return;
	g_av_allocs--;
	free(*pkt);
	*pkt = nullptr;
}

AVFrame *av_frame_alloc(void)
{
	g_av_allocs++;
	return static_cast<AVFrame *>(calloc(1, sizeof(AVFrame)));
}

void av_frame_free(AVFrame **frame)
{
	if (!frame || !*frame)
		return;
	g_av_allocs--;
	free(*frame);
	*frame = nullptr;
}

void *av_mallocz(size_t size)
{
	g_av_allocs++;
	return calloc(1, size ? size : 1);
}

int av_strerror(int, char *errbuf, size_t errbuf_size)
{
	if (errbuf && errbuf_size)
		snprintf(errbuf, errbuf_size, "stub error");
	return 0;
}

const char *av_get_pix_fmt_name(enum AVPixelFormat)
{
	return "yuv420p";
}

struct SwsContext *sws_getContext(int, int, enum AVPixelFormat, int, int, enum AVPixelFormat, int, SwsFilter *,
				  SwsFilter *, const double *)
{
	g_av_allocs++;
	return static_cast<struct SwsContext *>(calloc(1, 64));
}

void sws_freeContext(struct SwsContext *ctx)
{
	if (!ctx)
		return;
	g_av_allocs--;
	free(ctx);
}

int sws_scale(struct SwsContext *, const uint8_t *const[], const int[], int, int srcSliceH, uint8_t *const[],
	      const int[])
{
	g_sws_scales++;
	return srcSliceH;
}

} // extern "C"

// ----------------------------------------------------------------- libmoq stubs

namespace {
enum class SubKind { Session, Announced, Catalog, Video };

// One outstanding libmoq subscription: the callback plus the user_data the
// plugin asked us to keep alive, and the two bits of state that make the
// contract checkable - closed at most once, terminal delivered at most once.
struct Sub {
	SubKind kind = SubKind::Session;
	void (*cb)(void *, int32_t) = nullptr;
	void *user_data = nullptr;
	bool closed = false;
	bool terminated = false;
	bool callback_complete = false;
};

// Guards every table below. Always taken *under* the source's own mutex (the
// plugin calls these stubs from inside its locked sections), never the other way
// round: a callback is always invoked with this released.
std::mutex g_subs_mutex;
std::map<int32_t, Sub> g_subs;
std::map<int32_t, bool> g_snapshots; // live catalog snapshot ids
std::map<int32_t, bool> g_frames;    // live frame ids -> keyframe

// Disjoint ranges, so a handle misread as a snapshot or a frame id fails loudly
// instead of resolving to something plausible.
int32_t g_next_sub = 100;
int32_t g_next_snapshot = 5000;
int32_t g_next_frame = 9000;

// Terminals are normally posted by close(). A test that needs a delivery to land
// between a close and its terminal sets this and drives them by hand.
std::atomic<bool> g_defer_terminals{false};
std::vector<int32_t> g_deferred;

// Opens the exact retirement window where libmoq has removed a task but has not
// yet invoked its terminal callback.
std::mutex g_terminal_gate_mutex;
std::condition_variable g_terminal_gate_cv;
bool g_pause_terminal = false;
bool g_terminal_marked = false;
bool g_release_terminal = false;
bool g_inflight_close_seen = false;

// Closing a handle after its terminal callback completed. A failed close while
// that callback is still in flight is an expected teardown race instead.
std::atomic<int> g_double_close{0};

std::atomic<int> g_origin_creates{0};
std::atomic<int> g_origin_closes{0};
std::atomic<int> g_session_connects{0};
std::atomic<int> g_announced_calls{0};
// moq_origin_request resolves only broadcasts that are already announced. The
// source must wait with moq_origin_consume_announced, so this stays zero.
std::atomic<int> g_request_calls{0};
std::atomic<int> g_catalog_calls{0};
std::atomic<int> g_video_calls{0};
std::atomic<int> g_snapshot_frees{0};
std::atomic<int> g_frame_frees{0};
std::atomic<int> g_consume_closes{0};
std::atomic<int32_t> g_last_consume_closed{0};

std::atomic<int32_t> g_last_session{0};
std::atomic<int32_t> g_last_announced{0};
std::atomic<int32_t> g_last_catalog{0};
std::atomic<int32_t> g_last_video{0};

// Immediate-failure injection, matching libmoq's negative return codes.
bool g_session_connect_early = false;
int g_origin_result = 0;
int g_session_result = 0;
int g_announced_result = 0;
int g_catalog_result = 0;
int g_video_result = 0;
int g_video_config_result = 0;

// What moq_consume_video_config hands back.
uint32_t g_coded_width = 320;
uint32_t g_coded_height = 240;
const char g_codec[] = "h264";
uint8_t g_description[] = {0x01, 0x64, 0x00, 0x1f};
bool g_describe = false;

int32_t addSub(SubKind kind, void (*cb)(void *, int32_t), void *user_data)
{
	std::lock_guard<std::mutex> lock(g_subs_mutex);
	int32_t handle = g_next_sub++;
	g_subs[handle] = Sub{kind, cb, user_data, false, false, false};
	return handle;
}

// libmoq runs every callback on one runtime thread, so a delivery and a terminal
// for the same subscription never overlap. The plugin's reference counting leans
// on that, so the stub keeps the guarantee even when a test drives deliveries
// from two threads at once.
std::mutex g_delivery_mutex;

// Invoke a subscription's callback the way libmoq does: from a thread the plugin
// does not own, exactly once with a terminal, and never again after it. Returns
// false when the subscription has already terminated, which is what a delivery
// racing a close looks like.
bool deliverStatus(int32_t handle, int32_t code)
{
	std::lock_guard<std::mutex> serialize(g_delivery_mutex);

	void (*cb)(void *, int32_t) = nullptr;
	void *user_data = nullptr;
	SubKind kind = SubKind::Session;
	{
		std::lock_guard<std::mutex> lock(g_subs_mutex);
		auto it = g_subs.find(handle);
		if (it == g_subs.end() || it->second.terminated)
			return false;
		cb = it->second.cb;
		user_data = it->second.user_data;
		kind = it->second.kind;
		if (code <= 0)
			it->second.terminated = true;
	}
	if (code <= 0) {
		std::unique_lock<std::mutex> lock(g_terminal_gate_mutex);
		if (g_pause_terminal) {
			g_terminal_marked = true;
			g_terminal_gate_cv.notify_all();
			g_terminal_gate_cv.wait(lock, [] { return g_release_terminal; });
		}
	}
	cb(user_data, code);
	if (code <= 0) {
		std::lock_guard<std::mutex> lock(g_subs_mutex);
		g_subs.at(handle).callback_complete = true;
	}

	// The announced wait is one-shot: after delivering its broadcast handle,
	// libmoq immediately follows with the terminal callback on the same thread.
	if (kind == SubKind::Announced && code > 0) {
		{
			std::lock_guard<std::mutex> lock(g_subs_mutex);
			auto it = g_subs.find(handle);
			if (it == g_subs.end() || it->second.terminated)
				return true;
			it->second.terminated = true;
		}
		cb(user_data, 0);
		std::lock_guard<std::mutex> lock(g_subs_mutex);
		g_subs.at(handle).callback_complete = true;
	}
	return true;
}

int32_t closeSub(int32_t handle)
{
	bool terminal_in_flight = false;
	{
		std::lock_guard<std::mutex> lock(g_subs_mutex);
		auto it = g_subs.find(handle);
		if (it == g_subs.end() || it->second.closed ||
		    (it->second.terminated && it->second.callback_complete)) {
			g_double_close++;
			return -1;
		}
		if (it->second.terminated) {
			terminal_in_flight = true;
		} else {
			it->second.closed = true;
			if (g_defer_terminals) {
				g_deferred.push_back(handle);
				return 0;
			}
		}
	}
	if (terminal_in_flight) {
		std::lock_guard<std::mutex> lock(g_terminal_gate_mutex);
		g_inflight_close_seen = true;
		g_terminal_gate_cv.notify_all();
		return -1;
	}
	// Posted, never called inline: the plugin closes each handle while holding
	// ctx->mutex, which the terminal's reference release also needs.
	g_runtime->Post([handle] { deliverStatus(handle, 0); });
	return 0;
}
} // namespace

extern "C" {

int32_t moq_origin_create(void)
{
	if (g_origin_result < 0)
		return g_origin_result;
	g_origin_creates++;
	std::lock_guard<std::mutex> lock(g_subs_mutex);
	return g_next_sub++;
}

int32_t moq_origin_close(uint32_t)
{
	g_origin_closes++;
	return 0;
}

int32_t moq_session_connect(const char *, uintptr_t, uint32_t, uint32_t, void (*on_status)(void *, int32_t),
			    void *user_data)
{
	if (g_session_result < 0)
		return g_session_result;
	g_session_connects++;
	int32_t handle = addSub(SubKind::Session, on_status, user_data);
	g_last_session = handle;
	if (g_session_connect_early)
		g_runtime->Run([handle] { deliverStatus(handle, 1); });
	return handle;
}

int32_t moq_session_close(uint32_t session)
{
	return closeSub(static_cast<int32_t>(session));
}

int32_t moq_origin_consume_announced(uint32_t, const char *, uintptr_t, void (*on_broadcast)(void *, int32_t),
				     void *user_data)
{
	if (g_announced_result < 0)
		return g_announced_result;
	g_announced_calls++;
	int32_t handle = addSub(SubKind::Announced, on_broadcast, user_data);
	g_last_announced = handle;
	return handle;
}

int32_t moq_origin_consume_announced_close(uint32_t task)
{
	return closeSub(static_cast<int32_t>(task));
}

// Keep the immediate lookup stub link-complete so g_request_calls can assert that
// the source uses the waiting API instead.
int32_t moq_origin_request(uint32_t, const char *, uintptr_t, void (*)(void *, int32_t), void *)
{
	g_request_calls++;
	return -1;
}

int32_t moq_consume_catalog(uint32_t, void (*on_catalog)(void *, int32_t), void *user_data)
{
	if (g_catalog_result < 0)
		return g_catalog_result;
	g_catalog_calls++;
	int32_t handle = addSub(SubKind::Catalog, on_catalog, user_data);
	g_last_catalog = handle;
	return handle;
}

int32_t moq_consume_catalog_close(uint32_t catalog)
{
	return closeSub(static_cast<int32_t>(catalog));
}

int32_t moq_consume_catalog_free(uint32_t catalog)
{
	std::lock_guard<std::mutex> lock(g_subs_mutex);
	auto it = g_snapshots.find(static_cast<int32_t>(catalog));
	if (it == g_snapshots.end()) {
		g_stub_errors++;
		return -1;
	}
	g_snapshots.erase(it);
	g_snapshot_frees++;
	return 0;
}

int32_t moq_consume_video_config(uint32_t catalog, uint32_t, struct moq_video_config *dst)
{
	{
		std::lock_guard<std::mutex> lock(g_subs_mutex);
		if (g_snapshots.find(static_cast<int32_t>(catalog)) == g_snapshots.end()) {
			g_stub_errors++;
			return -1;
		}
	}
	if (g_video_config_result < 0)
		return g_video_config_result;

	dst->name = "video";
	dst->name_len = 5;
	dst->codec = g_codec;
	dst->codec_len = sizeof(g_codec) - 1;
	dst->description = g_describe ? g_description : nullptr;
	dst->description_len = g_describe ? sizeof(g_description) : 0;
	dst->coded_width = &g_coded_width;
	dst->coded_height = &g_coded_height;
	dst->container.kind = MOQ_CONTAINER_KIND_LEGACY;
	dst->container.init = nullptr;
	dst->container.init_len = 0;
	return 0;
}

int32_t moq_consume_video(uint32_t catalog, uint32_t, uint64_t, void (*on_frame)(void *, int32_t), void *user_data)
{
	{
		std::lock_guard<std::mutex> lock(g_subs_mutex);
		if (g_snapshots.find(static_cast<int32_t>(catalog)) == g_snapshots.end()) {
			g_stub_errors++;
			return -1;
		}
	}
	if (g_video_result < 0)
		return g_video_result;
	g_video_calls++;
	int32_t handle = addSub(SubKind::Video, on_frame, user_data);
	g_last_video = handle;
	return handle;
}

int32_t moq_consume_video_close(uint32_t track)
{
	return closeSub(static_cast<int32_t>(track));
}

int32_t moq_consume_frame(uint32_t frame, struct moq_frame *dst)
{
	std::lock_guard<std::mutex> lock(g_subs_mutex);
	auto it = g_frames.find(static_cast<int32_t>(frame));
	if (it == g_frames.end()) {
		g_stub_errors++;
		return -1;
	}
	dst->payload = g_description;
	dst->payload_size = sizeof(g_description);
	dst->timestamp_us = 1000 * static_cast<uint64_t>(frame);
	dst->keyframe = it->second;
	return 0;
}

int32_t moq_consume_frame_free(uint32_t frame)
{
	std::lock_guard<std::mutex> lock(g_subs_mutex);
	auto it = g_frames.find(static_cast<int32_t>(frame));
	if (it == g_frames.end()) {
		g_stub_errors++;
		return -1;
	}
	g_frames.erase(it);
	g_frame_frees++;
	return 0;
}

int32_t moq_consume_close(uint32_t consume)
{
	g_consume_closes++;
	g_last_consume_closed = static_cast<int32_t>(consume);
	return 0;
}

} // extern "C"

// ---------------------------------------------------------------- test harness

namespace {
// The plugin only ever sees this as an opaque obs_data_t.
obs_data_t *settingsData()
{
	return reinterpret_cast<obs_data_t *>(&g_settings);
}

obs_source_t *fakeSource()
{
	return reinterpret_cast<obs_source_t *>(0x9);
}

// A broadcast handle, which libmoq draws from a different slab than the
// subscription handles, and which carries no callback of its own.
int32_t newBroadcast()
{
	std::lock_guard<std::mutex> lock(g_subs_mutex);
	return g_next_sub++;
}

int32_t newSnapshot()
{
	std::lock_guard<std::mutex> lock(g_subs_mutex);
	int32_t id = g_next_snapshot++;
	g_snapshots[id] = true;
	return id;
}

int32_t newFrame(bool keyframe)
{
	std::lock_guard<std::mutex> lock(g_subs_mutex);
	int32_t id = g_next_frame++;
	g_frames[id] = keyframe;
	return id;
}

void flushDeferred()
{
	std::vector<int32_t> pending;
	{
		std::lock_guard<std::mutex> lock(g_subs_mutex);
		pending.swap(g_deferred);
	}
	for (int32_t handle : pending)
		g_runtime->Run([handle] { deliverStatus(handle, 0); });
}

// Every subscription the plugin registered must have delivered its terminal by
// the time a scenario ends; one that hasn't is a reference the source would
// still be waiting on.
bool allTerminated()
{
	std::lock_guard<std::mutex> lock(g_subs_mutex);
	for (const auto &entry : g_subs) {
		if (!entry.second.terminated)
			return false;
	}
	return true;
}

void reset()
{
	{
		std::lock_guard<std::mutex> lock(g_subs_mutex);
		g_subs.clear();
		g_snapshots.clear();
		g_frames.clear();
		g_deferred.clear();
	}
	g_settings.url = "https://relay.example/anon";
	g_settings.broadcast = "obs/test";
	g_defer_terminals = false;
	{
		std::lock_guard<std::mutex> lock(g_terminal_gate_mutex);
		g_pause_terminal = false;
		g_terminal_marked = false;
		g_release_terminal = false;
		g_inflight_close_seen = false;
	}
	g_double_close = 0;
	g_stub_errors = 0;
	g_origin_creates = 0;
	g_origin_closes = 0;
	g_session_connects = 0;
	g_announced_calls = 0;
	g_catalog_calls = 0;
	g_video_calls = 0;
	g_snapshot_frees = 0;
	g_frame_frees = 0;
	g_consume_closes = 0;
	g_last_consume_closed = 0;
	g_last_session = 0;
	g_last_announced = 0;
	g_last_catalog = 0;
	g_last_video = 0;
	g_output_frames = 0;
	g_blank_calls = 0;
	g_sws_scales = 0;
	g_session_connect_early = false;
	g_origin_result = 0;
	g_session_result = 0;
	g_announced_result = 0;
	g_catalog_result = 0;
	g_video_result = 0;
	g_video_config_result = 0;
	g_find_decoder_ok = true;
	g_send_result = 0;
	g_receive_result = 0;
	g_describe = false;
	g_description[0] = 0x01;
	g_decoded_width = 320;
	g_decoded_height = 240;
	g_last_decoder_extradata = -1;
	g_live_allocs = 0;
	g_av_allocs = 0;
}

void *createSource()
{
	return g_info.create(settingsData(), fakeSource());
}

// Destroy, then check the invariants every scenario shares: teardown returned on
// the terminal callbacks rather than on its own bounded wait, every subscription
// ended, and nothing was left allocated.
void destroySource(void *source)
{
	auto start = std::chrono::steady_clock::now();
	g_info.destroy(source);
	auto elapsed = std::chrono::steady_clock::now() - start;

	// The destructor's backstop is two seconds. Landing anywhere near it means a
	// terminal never arrived, so a reference was never released.
	CHECK(elapsed < std::chrono::milliseconds(1500));
	CHECK(allTerminated());
	CHECK(g_live_allocs == 0);
	CHECK(g_av_allocs == 0);
	CHECK(g_double_close == 0);
	CHECK(g_stub_errors == 0);
}

// Close out a scenario, naming it and saying whether any of its assertions
// failed. The individual FAIL lines carry the line numbers; this is the index
// that says which scenario they belong to.
int g_reported = 0;

void report(const char *name)
{
	printf("%s: %s\n", name, g_failures == g_reported ? "ok" : "FAILED");
	g_reported = g_failures;
}

// Walk the source from "just created" to "subscribed to a video track", firing
// each callback from the runtime thread in the order libmoq would.
void subscribeVideo(int32_t broadcast)
{
	g_runtime->Run([] { deliverStatus(g_last_session, 1); });
	g_runtime->Run([broadcast] { deliverStatus(g_last_announced, broadcast); });
	{
		std::lock_guard<std::mutex> lock(g_subs_mutex);
		CHECK(g_subs.at(g_last_announced).terminated);
	}
	int32_t snapshot = newSnapshot();
	g_runtime->Run([snapshot] { deliverStatus(g_last_catalog, snapshot); });
}
} // namespace

int main()
{
	Runtime runtime;
	g_runtime = &runtime;

	register_moq_source();
	if (!g_registered) {
		fprintf(stderr, "FAIL: the source never registered\n");
		return 1;
	}
	CHECK(g_info.create && g_info.destroy && g_info.update);
	CHECK(g_info.get_defaults && g_info.get_properties && g_info.get_name);

	// The announcement arrives after the session reports connected, which is the
	// normal order on the wire. The source waits for it, then starts the catalog
	// subscription from the delivered broadcast.
	{
		reset();
		void *source = createSource();
		CHECK(g_session_connects == 1);
		CHECK(g_announced_calls == 0); // nothing is asked for before connect

		g_runtime->Run([] { deliverStatus(g_last_session, 1); });
		CHECK(g_announced_calls == 1);
		// A wait for a future announcement, not a lookup of what is announced now.
		CHECK(g_request_calls == 0);

		int32_t broadcast = newBroadcast();
		g_runtime->Run([broadcast] { deliverStatus(g_last_announced, broadcast); });
		{
			std::lock_guard<std::mutex> lock(g_subs_mutex);
			CHECK(g_subs.at(g_last_announced).terminated);
		}
		CHECK(g_catalog_calls == 1);

		int32_t snapshot = newSnapshot();
		g_runtime->Run([snapshot] { deliverStatus(g_last_catalog, snapshot); });
		CHECK(g_video_calls == 1);
		CHECK(g_snapshot_frees == 1); // the snapshot is freed, never closed

		// Only the blank from the initial connect: nothing here reported a failure.
		CHECK(g_blank_calls == 1);
		destroySource(source);
		CHECK(g_consume_closes == 1);
		CHECK(g_origin_closes == 1);
	}
	report("announcement after connect");

	// A broadcast that is never announced leaves the wait pending forever. Destroy
	// closes it, and that close is the only thing that makes the terminal fire and
	// release the reference teardown is waiting on.
	{
		reset();
		void *source = createSource();
		g_runtime->Run([] { deliverStatus(g_last_session, 1); });
		CHECK(g_announced_calls == 1);
		CHECK(g_catalog_calls == 0); // still waiting
		destroySource(source);
	}
	report("never announced, closed by destroy");

	// The same pending wait, ended by settings the source cannot connect with.
	// This is disconnect on a live source rather than on teardown, so the terminal
	// has to fire exactly once and destroy must not close the handle again.
	{
		reset();
		void *source = createSource();
		g_runtime->Run([] { deliverStatus(g_last_session, 1); });

		g_settings.url = "";
		g_info.update(source, settingsData());
		CHECK(g_blank_calls == 2); // the connect, then the disconnect
		CHECK(g_announced_calls == 1);

		destroySource(source);
		// The update closed the origin; destroy found nothing left to close.
		CHECK(g_origin_closes == 1);
		CHECK(g_consume_closes == 0);
	}
	report("disconnect closes the pending wait");

	// A reconnect bumps the generation while an older request still has a delivery
	// in flight. The stale broadcast handle is ours the moment it is delivered, so
	// it has to be closed rather than dropped, and it must not become the consume
	// handle the new generation owns.
	{
		reset();
		void *source = createSource();
		g_runtime->Run([] { deliverStatus(g_last_session, 1); });
		int32_t stale_request = g_last_announced;

		// Hold the terminals back so the stale delivery lands between the close and
		// its terminal, which is the window libmoq leaves open.
		g_defer_terminals = true;
		g_settings.broadcast = "obs/other";
		g_info.update(source, settingsData()); // generation 1 -> 2

		int32_t stale_broadcast = newBroadcast();
		g_runtime->Run([stale_request, stale_broadcast] { deliverStatus(stale_request, stale_broadcast); });
		CHECK(g_catalog_calls == 0); // the stale broadcast is not consumed
		CHECK(g_consume_closes == 1);
		CHECK(g_last_consume_closed == stale_broadcast);

		g_defer_terminals = false;
		flushDeferred();

		// The new generation still connects and subscribes normally.
		subscribeVideo(newBroadcast());
		CHECK(g_session_connects == 2);
		CHECK(g_video_calls == 1);

		destroySource(source);
	}
	report("stale delivery dropped on the generation check");

	// The delivered path: frames reach OBS, and every frame id is freed exactly
	// once.
	{
		reset();
		g_describe = true; // exercise the extradata copy too
		void *source = createSource();
		subscribeVideo(newBroadcast());

		int32_t first = newFrame(true);
		g_runtime->Run([first] { deliverStatus(g_last_video, first); });
		CHECK(g_output_frames == 1);
		CHECK(g_sws_scales == 1);
		// libmoq delivers microseconds; OBS wants nanoseconds.
		CHECK(g_last_timestamp == 1000ull * static_cast<uint64_t>(first) * 1000ull);

		int32_t second = newFrame(false);
		g_runtime->Run([second] { deliverStatus(g_last_video, second); });
		CHECK(g_output_frames == 2);

		CHECK(g_frame_frees == 2);
		destroySource(source);
	}
	report("delivered frames output and freed");

	// Frames that arrive before the first keyframe, and frames the decoder
	// rejects, take early returns out of the decode path. Each one still owns the
	// frame id it was handed.
	{
		reset();
		void *source = createSource();
		subscribeVideo(newBroadcast());

		int32_t skipped = newFrame(false); // no keyframe yet
		g_runtime->Run([skipped] { deliverStatus(g_last_video, skipped); });
		CHECK(g_output_frames == 0);

		g_send_result = -1;
		int32_t rejected = newFrame(true);
		g_runtime->Run([rejected] { deliverStatus(g_last_video, rejected); });
		CHECK(g_output_frames == 0);

		g_send_result = 0;
		g_receive_result = -1;
		int32_t undecoded = newFrame(true);
		g_runtime->Run([undecoded] { deliverStatus(g_last_video, undecoded); });
		CHECK(g_output_frames == 0);

		CHECK(g_frame_frees == 3);
		destroySource(source);
	}
	report("dropped frames freed");

	// A second catalog replaces the video track. The old subscription is closed so
	// its terminal releases its reference now rather than at teardown.
	{
		reset();
		void *source = createSource();
		subscribeVideo(newBroadcast());
		int32_t first_track = g_last_video;

		int32_t snapshot = newSnapshot();
		g_runtime->Run([snapshot] { deliverStatus(g_last_catalog, snapshot); });
		CHECK(g_video_calls == 2);
		CHECK(g_last_video != first_track);
		CHECK(g_snapshot_frees == 2);

		// Drain the terminal the swap posted, then confirm it landed.
		g_runtime->Run([] {});
		{
			std::lock_guard<std::mutex> lock(g_subs_mutex);
			CHECK(g_subs[first_track].terminated);
		}
		destroySource(source);
	}
	report("catalog update retires the old track");

	// A catalog update can advertise a replacement that libmoq rejects. The
	// existing video subscription remains current and must keep delivering frames.
	{
		reset();
		g_describe = true;
		void *source = createSource();
		subscribeVideo(newBroadcast());
		int32_t first_track = g_last_video;

		g_description[0] = 0x02;
		g_video_result = -77;
		int32_t snapshot = newSnapshot();
		g_runtime->Run([snapshot] { deliverStatus(g_last_catalog, snapshot); });
		CHECK(g_video_calls == 1);
		CHECK(g_last_video == first_track);
		CHECK(g_snapshot_frees == 2);

		int32_t frame = newFrame(true);
		g_runtime->Run([first_track, frame] { deliverStatus(first_track, frame); });
		CHECK(g_output_frames == 1);
		CHECK(g_frame_frees == 1);
		CHECK(g_last_decoder_extradata == 0x01);

		destroySource(source);
	}
	report("failed replacement keeps the old track current");

	// A session that fails for good tears down every subscription, not just the
	// session, so the catalog and video references come back immediately instead
	// of lingering until the source is destroyed.
	{
		reset();
		void *source = createSource();
		subscribeVideo(newBroadcast());

		g_runtime->Run([] { deliverStatus(g_last_session, -34); });
		CHECK(g_blank_calls == 2); // the connect, then the error
		CHECK(g_consume_closes == 1);
		CHECK(g_origin_closes == 1);
		g_runtime->Run([] {});
		CHECK(allTerminated());

		destroySource(source);
	}
	report("session error tears down the consume path");

	// A broadcast that resolves to an error, and a catalog subscription that ends
	// in one. Both blank the preview and leave the source disconnected.
	{
		reset();
		void *source = createSource();
		g_runtime->Run([] { deliverStatus(g_last_session, 1); });
		g_runtime->Run([] { deliverStatus(g_last_announced, -1); });
		CHECK(g_blank_calls == 2);
		CHECK(g_catalog_calls == 0);
		destroySource(source);
	}
	{
		reset();
		void *source = createSource();
		subscribeVideo(newBroadcast());
		g_runtime->Run([] { deliverStatus(g_last_catalog, -1); });
		CHECK(g_blank_calls == 2);
		g_runtime->Run([] { deliverStatus(g_last_video, -1); });
		destroySource(source);
	}
	report("error terminals release their references");

	// Every point where libmoq can refuse immediately. No subscription exists, so
	// no terminal will ever fire, and the reference the plugin pre-added has to be
	// undone by hand. A miss here is invisible until teardown hits its backstop,
	// which is what destroySource checks.
	{
		reset();
		g_origin_result = -1;
		void *source = createSource();
		CHECK(g_session_connects == 0);
		destroySource(source);
	}
	{
		reset();
		g_session_result = -1;
		void *source = createSource();
		CHECK(g_session_connects == 0);
		CHECK(g_origin_closes == 1); // the origin created for the attempt
		destroySource(source);
	}
	{
		reset();
		g_announced_result = -1;
		void *source = createSource();
		g_runtime->Run([] { deliverStatus(g_last_session, 1); });
		CHECK(g_announced_calls == 0);
		CHECK(g_blank_calls == 2);
		destroySource(source);
	}
	{
		reset();
		g_catalog_result = -1;
		void *source = createSource();
		g_runtime->Run([] { deliverStatus(g_last_session, 1); });
		int32_t broadcast = newBroadcast();
		g_runtime->Run([broadcast] { deliverStatus(g_last_announced, broadcast); });
		CHECK(g_catalog_calls == 0);
		CHECK(g_blank_calls == 2);
		destroySource(source);
	}
	{
		reset();
		g_video_result = -1;
		void *source = createSource();
		subscribeVideo(newBroadcast());
		CHECK(g_video_calls == 0);
		CHECK(g_snapshot_frees == 1); // the snapshot is freed even when the track isn't
		destroySource(source);
	}
	report("refused subscriptions undo their reference");

	// A connected callback can finish before moq_session_connect returns. If
	// consume setup then fails, the returned session belongs to the aborted origin
	// and must be closed instead of becoming the source's current session.
	{
		reset();
		g_session_connect_early = true;
		g_announced_result = -1;
		void *source = createSource();
		CHECK(g_origin_closes == 1);
		{
			std::lock_guard<std::mutex> lock(g_subs_mutex);
			CHECK(g_subs.at(g_last_session).closed);
		}
		g_runtime->Run([] {});
		destroySource(source);
	}
	report("early consume failure rejects the returned session");

	// A decoder that cannot be built leaves the catalog subscription alone: no
	// video track, the snapshot still freed, and nothing waiting on a terminal
	// that will never come.
	{
		reset();
		g_find_decoder_ok = false;
		void *source = createSource();
		subscribeVideo(newBroadcast());
		CHECK(g_video_calls == 0);
		CHECK(g_snapshot_frees == 1);
		destroySource(source);
	}
	{
		reset();
		g_video_config_result = -1;
		void *source = createSource();
		subscribeVideo(newBroadcast());
		CHECK(g_video_calls == 0);
		CHECK(g_snapshot_frees == 1);
		destroySource(source);
	}
	report("undecodable catalog leaves no dangling reference");

	// libmoq retires a task before entering its terminal callback. Teardown can
	// see the still-stored handle in that interval; a failed close is expected and
	// must not be mistaken for a close after the callback already retired it.
	{
		reset();
		void *source = createSource();
		subscribeVideo(newBroadcast());
		int32_t track = g_last_video;
		{
			std::lock_guard<std::mutex> lock(g_terminal_gate_mutex);
			g_pause_terminal = true;
		}

		std::thread terminal([track] { deliverStatus(track, 0); });
		{
			std::unique_lock<std::mutex> lock(g_terminal_gate_mutex);
			CHECK(g_terminal_gate_cv.wait_for(lock, std::chrono::seconds(1),
							  [] { return g_terminal_marked; }));
		}

		std::thread teardown([source] { destroySource(source); });
		{
			std::unique_lock<std::mutex> lock(g_terminal_gate_mutex);
			CHECK(g_terminal_gate_cv.wait_for(lock, std::chrono::seconds(1),
							  [] { return g_inflight_close_seen; }));
			g_release_terminal = true;
			g_terminal_gate_cv.notify_all();
		}
		terminal.join();
		teardown.join();
	}
	report("teardown accepts an in-flight terminal");

	// Terminal callbacks racing destruction, repeatedly. Whichever order they land
	// in, teardown returns on the callbacks and frees everything.
	{
		const int rounds = 100;
		for (int i = 0; i < rounds; i++) {
			reset();
			void *source = createSource();
			subscribeVideo(newBroadcast());

			int32_t track = g_last_video;
			int32_t catalog = g_last_catalog;
			std::thread racer([track, catalog] {
				deliverStatus(track, -1);
				deliverStatus(catalog, -1);
			});
			destroySource(source);
			racer.join();
		}
		report("terminals racing destroy");
	}

	// A frame in flight when the source is destroyed. Either it beats the close,
	// in which case the plugin owns the frame id and must free it, or the close
	// wins and libmoq never hands it over at all.
	{
		const int rounds = 100;
		int delivered = 0;
		for (int i = 0; i < rounds; i++) {
			reset();
			void *source = createSource();
			subscribeVideo(newBroadcast());

			int32_t track = g_last_video;
			int32_t frame = newFrame(true);
			std::atomic<bool> landed{false};
			std::thread racer([track, frame, &landed] { landed = deliverStatus(track, frame); });
			destroySource(source);
			racer.join();
			CHECK(g_frame_frees == (landed ? 1 : 0));
			delivered += landed ? 1 : 0;
		}
		char label[64];
		snprintf(label, sizeof(label), "frames racing destroy (%d of %d delivered)", delivered, rounds);
		report(label);
	}

	CHECK(g_request_calls == 0);

	g_runtime = nullptr;
	if (g_failures) {
		printf("\nFAILURES: %d\n", g_failures);
		return 1;
	}
	printf("\nall passed\n");
	return 0;
}
