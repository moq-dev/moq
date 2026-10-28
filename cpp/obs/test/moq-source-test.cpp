// SPDX-License-Identifier: GPL-2.0-or-later
//
// Drives the real moq_source, reached through the obs_source_info it registers,
// against stubbed libobs and FFmpeg video and the real moq-ffi, which decodes the
// audio, over a relay in the same process. What the consume path can get wrong is mostly an ordering: an
// announcement that arrives after the session connects, a result belonging to a
// connection that settings or teardown have already replaced, a destroy while a
// connect or a wait for the broadcast is still pending. The relay publishes real
// media, so each scenario runs the whole chain from connect to decoded frame.
//
// Every scenario ends by checking that destroy returned promptly and that nothing
// the source allocated outlived it.
//
// Run with `just obs test` (ThreadSanitizer) or `just obs ci` (plain). This is
// not part of the plugin build.
#include <algorithm>
#include <atomic>
#include <chrono>
#include <cstdarg>
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <deque>
#include <functional>
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
}

// Include the implementation so catalog-shape tests need no production test hooks.
#include "../src/moq-source.cpp"
#include "moq-test-relay.h"

namespace {
int g_failures = 0;
} // namespace

#define CHECK(cond)                                                             \
	do {                                                                    \
		if (!(cond)) {                                                  \
			fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); \
			g_failures++;                                           \
		}                                                               \
	} while (0)

// ------------------------------------------------------------------ libobs stubs

namespace {
// The settings object the plugin reads through obs_data_get_string. The test
// owns it and hands its address across as an opaque obs_data_t.
struct Settings {
	std::string url;
	std::string broadcast;
	bool keylog = false;
};

Settings g_settings;

// OBS copies the registration struct, which is a stack local in
// register_moq_source, so keeping the pointer would dangle.
struct obs_source_info g_info = {};
bool g_registered = false;

// Written from the source's worker (the decode path outputs frames) and read from
// the test thread.
std::atomic<int> g_output_frames{0};
std::atomic<int> g_blank_calls{0};
std::atomic<uint64_t> g_last_timestamp{0};
// Every video timestamp OBS was handed, in output order.
std::mutex g_video_mutex;
std::vector<uint64_t> g_video_timestamps;
// PCM buffers the audio decode path handed to OBS.
std::atomic<int> g_output_audio{0};
std::atomic<uint64_t> g_last_audio_timestamp{0};

// The last buffer OBS was handed, copied out: its shape, the first sample of each
// plane, and the loudest sample in it.
struct AudioOut {
	uint32_t frames = 0;
	enum speaker_layout speakers = SPEAKERS_UNKNOWN;
	enum audio_format format = AUDIO_FORMAT_UNKNOWN;
	uint32_t samples_per_sec = 0;
	uint64_t timestamp = 0;
	float first[MAX_AV_PLANES] = {};
	float peak = 0;
};
std::mutex g_audio_mutex;
AudioOut g_last_audio;

AudioOut lastAudio()
{
	std::lock_guard<std::mutex> lock(g_audio_mutex);
	return g_last_audio;
}

// The mixer rate obs_get_audio_info reports.
uint32_t g_obs_sample_rate = 48000;

// The bmalloc/bfree balance, so a frame buffer that outlives its decoder shows up
// as a leak.
std::atomic<long> g_live_allocs{0};

// Watch one attempt, not whichever replacement a later catalog snapshot may start.
// The logger runs under the source lock, before the failed attempt is released.
struct CandidateFailure {
	std::weak_ptr<Track> track;
	bool failed = false;
};
std::mutex g_candidate_mutex;
CandidateFailure g_candidate;
} // namespace

extern "C" {

// Deliberately silent: stdio locking would create happens-before edges between
// the test thread and the source's worker and mask the races we're after. Match
// the watched role while its attempt is alive; playback identity checks below
// distinguish an active-track error from the candidate's failure.
void blog(int, const char *format, ...)
{
	const char *kind = nullptr;
	if (strstr(format, "Audio track error") || strstr(format, "Failed to subscribe to Audio track")) {
		kind = "Audio";
	} else if (strstr(format, "Failed to subscribe to %s track")) {
		va_list args;
		va_start(args, format);
		kind = va_arg(args, const char *);
		va_end(args);
	}
	if (!kind)
		return;
	std::lock_guard<std::mutex> lock(g_candidate_mutex);
	auto candidate = g_candidate.track.lock();
	if (!candidate || strcmp(candidate->kind, kind) != 0)
		return;
	g_candidate.failed = true;
}

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

bool obs_data_get_bool(obs_data_t *data, const char *name)
{
	auto *settings = reinterpret_cast<Settings *>(data);
	return settings && strcmp(name, "quic_keylog") == 0 && settings->keylog;
}

void obs_data_set_default_bool(obs_data_t *, const char *, bool) {}

obs_property_t *obs_properties_add_bool(obs_properties_t *, const char *, const char *)
{
	return reinterpret_cast<obs_property_t *>(0x13);
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
	{
		std::lock_guard<std::mutex> lock(g_video_mutex);
		g_video_timestamps.push_back(frame->timestamp);
	}
	g_output_frames++;
}

void obs_source_output_audio(obs_source_t *, const struct obs_source_audio *audio)
{
	if (!audio)
		return;
	AudioOut out;
	out.frames = audio->frames;
	out.speakers = audio->speakers;
	out.format = audio->format;
	out.samples_per_sec = audio->samples_per_sec;
	out.timestamp = audio->timestamp;
	// Every plane of the layout must be there; reading each one whole lets ASan and
	// TSan see a buffer that is short or already freed.
	for (uint32_t i = 0; i < get_audio_planes(audio->format, audio->speakers); i++) {
		const auto *plane = reinterpret_cast<const float *>(audio->data[i]);
		out.first[i] = plane[0];
		for (uint32_t j = 0; j < audio->frames; j++)
			out.peak = std::max(out.peak, std::fabs(plane[j]));
	}
	{
		std::lock_guard<std::mutex> lock(g_audio_mutex);
		g_last_audio = out;
	}
	g_last_audio_timestamp = audio->timestamp;
	g_output_audio++;
}

bool obs_get_audio_info(struct obs_audio_info *oai)
{
	oai->samples_per_sec = g_obs_sample_rate;
	oai->speakers = SPEAKERS_STEREO;
	return true;
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

// Decode knobs. Only the failure and reordering scenarios move them.
std::atomic<bool> g_find_decoder_ok{true};
// How many pictures the decoder holds back to reorder, as H.264 does for B-frames.
std::atomic<size_t> g_reorder_delay{0};
// How many decoded pictures the decoder buffers before refusing input with EAGAIN.
std::atomic<size_t> g_decoder_capacity{SIZE_MAX};
// The one picture, by packet pts, whose first receive fails and leaves it in the decoder.
std::atomic<int64_t> g_unconverted_pts{-1};
int g_decoded_width = 320;
int g_decoded_height = 240;
std::atomic<int> g_last_decoder_extradata{-1};
std::atomic<int> g_decoder_opens{0};
// Only packets from the replacement use this timestamp; the old stream keeps decoding.
std::atomic<int> g_candidate_decode{0}; // 2: receive error, 3: needs more input
std::atomic<int> g_candidate_attempts{0};
// Holds packets' timestamps for reordering: each packet past the delay releases the
// earliest held one, and a flush releases the rest in order, then reports EOF.
struct DecodeState {
	int64_t last_pts = 0;
	std::vector<int64_t> held;
	std::deque<int64_t> ready;
	bool draining = false;
};

std::atomic<int> g_sws_scales{0};

AVCodec g_fake_codec{};
uint8_t g_fake_plane[320 * 240] = {};
} // namespace

extern "C" {

const AVCodec *avcodec_find_decoder(enum AVCodecID id)
{
	g_fake_codec.id = id;
	return (g_find_decoder_ok && id != AV_CODEC_ID_NONE) ? &g_fake_codec : nullptr;
}

AVCodecContext *avcodec_alloc_context3(const AVCodec *codec)
{
	g_av_allocs++;
	auto *ctx = static_cast<AVCodecContext *>(calloc(1, sizeof(AVCodecContext)));
	ctx->codec_id = codec ? codec->id : AV_CODEC_ID_NONE;
	ctx->opaque = new DecodeState{};
	return ctx;
}

void avcodec_free_context(AVCodecContext **avctx)
{
	if (!avctx || !*avctx)
		return;
	if ((*avctx)->extradata) {
		g_av_allocs--;
		free((*avctx)->extradata);
	}
	delete static_cast<DecodeState *>((*avctx)->opaque);
	g_av_allocs--;
	free(*avctx);
	*avctx = nullptr;
}

int avcodec_open2(AVCodecContext *, const AVCodec *, AVDictionary **)
{
	g_decoder_opens++;
	return 0;
}

void avcodec_flush_buffers(AVCodecContext *ctx)
{
	*static_cast<DecodeState *>(ctx->opaque) = DecodeState{};
}

int avcodec_send_packet(AVCodecContext *ctx, const AVPacket *packet)
{
	g_last_decoder_extradata = ctx->extradata_size > 0 ? ctx->extradata[0] : -1;
	auto &state = *static_cast<DecodeState *>(ctx->opaque);
	if (state.draining)
		return AVERROR_EOF;
	if (!packet) {
		state.draining = true;
		std::sort(state.held.begin(), state.held.end());
		state.ready.insert(state.ready.end(), state.held.begin(), state.held.end());
		state.held.clear();
		return 0;
	}
	if (state.ready.size() >= g_decoder_capacity)
		return AVERROR(EAGAIN);
	state.last_pts = packet->pts;
	if (state.last_pts == 1'000'000'000)
		g_candidate_attempts++;
	state.held.push_back(packet->pts);
	if (state.held.size() > g_reorder_delay) {
		const auto earliest = std::min_element(state.held.begin(), state.held.end());
		state.ready.push_back(*earliest);
		state.held.erase(earliest);
	}
	return 0;
}

int avcodec_receive_frame(AVCodecContext *ctx, AVFrame *frame)
{
	auto &state = *static_cast<DecodeState *>(ctx->opaque);
	if (state.last_pts == 1'000'000'000) {
		if (g_candidate_decode == 2)
			return AVERROR_INVALIDDATA;
		if (g_candidate_decode == 3)
			return AVERROR(EAGAIN);
	}
	if (state.ready.empty())
		return state.draining ? AVERROR_EOF : AVERROR(EAGAIN);
	int64_t unconverted = state.ready.front();
	if (g_unconverted_pts.compare_exchange_strong(unconverted, -1))
		return AVERROR_INVALIDDATA;

	frame->pts = state.ready.front();
	frame->best_effort_timestamp = frame->pts;
	state.ready.pop_front();
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

void reset(std::string url, std::string broadcast = "obs/test")
{
	g_settings.url = std::move(url);
	g_settings.broadcast = std::move(broadcast);
	g_settings.keylog = false;
	g_output_frames = 0;
	g_output_audio = 0;
	g_last_timestamp = 0;
	{
		std::lock_guard<std::mutex> lock(g_video_mutex);
		g_video_timestamps.clear();
	}
	g_last_audio_timestamp = 0;
	{
		std::lock_guard<std::mutex> lock(g_audio_mutex);
		g_last_audio = AudioOut{};
	}
	g_obs_sample_rate = 48000;
	g_blank_calls = 0;
	g_sws_scales = 0;
	g_find_decoder_ok = true;
	g_reorder_delay = 0;
	g_decoder_capacity = SIZE_MAX;
	g_unconverted_pts = -1;
	g_decoded_width = 320;
	g_decoded_height = 240;
	g_last_decoder_extradata = -1;
	g_decoder_opens = 0;
	g_candidate_decode = 0;
	g_candidate_attempts = 0;
	g_live_allocs = 0;
	g_av_allocs = 0;
}

void *createSource()
{
	return g_info.create(settingsData(), fakeSource());
}

// Destroy, then check the invariants every scenario shares: teardown returned
// promptly, without waiting on a call that was still pending, nothing reached OBS
// afterwards, and nothing was left allocated.
void destroySource(void *source)
{
	auto start = std::chrono::steady_clock::now();
	g_info.destroy(source);
	auto elapsed = std::chrono::steady_clock::now() - start;
	const int frames = g_output_frames;
	const int audio = g_output_audio;

	CHECK(elapsed < std::chrono::milliseconds(500));
	CHECK(g_live_allocs == 0);
	CHECK(g_av_allocs == 0);

	std::this_thread::sleep_for(std::chrono::milliseconds(20));
	CHECK(g_output_frames == frames);
	CHECK(g_output_audio == audio);
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

// A broadcast published on the relay, the way the MoQ output would publish it.
class Publisher {
public:
	// Frames start after `start_us`, so a scenario can tell two publishers' frames apart.
	Publisher(TestRelay &relay, const std::string &path, bool video = true, bool audio = true,
		  uint64_t start_us = 1'000'000)
		: timestamp_us(start_us)
	{
		auto publication = TestOk(relay.origin->create_broadcast(path), "create_broadcast");
		broadcast = publication->broadcast();
		if (video) {
			moq::VideoInit init{};
			init.format = moq::VideoFormat::kAvc3;
			init.data = TestH264Init();
			this->video =
				TestOk(moq::MediaTrackProducer::video(broadcast, moq::MediaTarget::kNamed{}, init),
				       "video producer");
		}
		if (audio) {
			moq::AudioInit init{};
			init.format = moq::AudioFormat::kOpus;
			init.data = TestOpusHead();
			this->audio =
				TestOk(moq::MediaTrackProducer::audio(broadcast, moq::MediaTarget::kNamed{}, init),
				       "audio producer");
		}
		TestOk(publication->announce(moq::AnnounceRoute{}), "announce");
	}

	~Publisher() { (void)broadcast->close(); }

	// Write one keyframe and one audio frame, each a group of its own.
	void Write()
	{
		timestamp_us += 20'000;
		if (video)
			TestOk(video->write_frame(moq::Frame{TestH264Keyframe(), timestamp_us}), "write video");
		if (audio) {
			TestOk(audio->write_frame(moq::Frame{{0xfc, 0xff, 0xfe}, timestamp_us}), "write audio");
			TestOk(audio->cut(), "cut audio");
		}
	}

	// Keep writing until `done`, since a subscriber only sees what arrives after it subscribes.
	bool WriteUntil(const std::function<bool()> &done, std::chrono::milliseconds timeout = std::chrono::seconds(10))
	{
		return WaitFor(
			[&] {
				Write();
				std::this_thread::sleep_for(std::chrono::milliseconds(10));
				return done();
			},
			timeout);
	}

	std::shared_ptr<moq::BroadcastProducer> broadcast;
	std::shared_ptr<moq::MediaTrackProducer> video;
	std::shared_ptr<moq::MediaTrackProducer> audio;

private:
	uint64_t timestamp_us;
};

// Whether OBS has been handed video and audio from a publisher that started at `start_us`.
bool PlaysFrom(uint64_t start_us)
{
	return g_last_timestamp > start_us * 1000 && g_last_audio_timestamp > start_us * 1000;
}

// A URL moq-ffi rejects as soon as the connect runs, for a failure with no network in it.
const char *const BAD_URL = "not a url";

// An Opus packet libopus refuses (OPUS_INVALID_PACKET). The importer passes it through.
const std::vector<uint8_t> BAD_OPUS = {0xff, 0xff, 0xff};

// Three AAC-LC frames of a 440 Hz full-scale sine, mono at 44.1 kHz, and the
// AudioSpecificConfig that opens them; the same fixture rs/moq-audio decodes.
const std::vector<uint8_t> AAC_CONFIG = {0x12, 0x08};
const std::vector<std::vector<uint8_t>> AAC_FRAMES = {
	{0x01, 0x52, 0xf2, 0x8b, 0x1a, 0xd7, 0x8e, 0x7b, 0xfd, 0xa7, 0xef, 0xe7, 0xe3, 0x55, 0xd3,
	 0x4d, 0x2f, 0x55, 0x2e, 0x47, 0x1c, 0x92, 0x49, 0x11, 0x20, 0x77, 0x3f, 0xbe, 0x74, 0xdd,
	 0x99, 0xb3, 0x7b, 0xfb, 0x90, 0xc9, 0xf0, 0x61, 0x9f, 0xdc, 0x0c, 0x9f, 0x06, 0x19, 0xfd,
	 0xe1, 0x1f, 0x1f, 0x00, 0x67, 0xf7, 0x03, 0x87, 0xc0, 0x19, 0xfd, 0xc0, 0xc9, 0xf0, 0x07},
	{0x01, 0x1e, 0x32, 0x89, 0xe2, 0x9d, 0x6b, 0x33, 0xe7, 0xff, 0xe2, 0xfe, 0xbf, 0xfa, 0xff, 0xe7,
	 0x2f, 0x8b, 0xd5, 0xd5, 0xe7, 0x5f, 0x3f, 0x59, 0xeb, 0xf1, 0xcb, 0xba, 0xa5, 0x5e, 0x52, 0x4a,
	 0xbd, 0x8d, 0x74, 0x50, 0x8c, 0x08, 0xa8, 0xa0, 0xd4, 0x51, 0x40, 0xa1, 0x86, 0x5d, 0x06, 0xb4,
	 0x6c, 0x32, 0xe6, 0x25, 0x9a, 0x66, 0x75, 0xcd, 0xf9, 0xbf, 0x6f, 0x83, 0xb7, 0x53, 0x80},
	{0x01, 0x1e, 0x32, 0x8a, 0x22, 0x7d, 0x40, 0x87, 0x48, 0xdb, 0xdf, 0xff, 0xf9, 0x4f, 0xff, 0x87, 0xde,
	 0xef, 0x8b, 0xeb, 0x1e, 0x77, 0x5d, 0xfc, 0x67, 0x8f, 0x8c, 0x77, 0x8a, 0xd6, 0x29, 0x96, 0x1f, 0x29,
	 0xe7, 0x39, 0xd4, 0x53, 0xcf, 0x3c, 0xf3, 0xce, 0x79, 0xd4, 0x27, 0x9c, 0xf5, 0x65, 0x2a, 0x9b, 0xe9,
	 0x80, 0xb7, 0xba, 0xa9, 0xf9, 0x58, 0xc7, 0x3c, 0x58, 0x27, 0x8a, 0x60, 0xa1, 0x57},
};

// A legacy-container PCM frame: the QUIC varint timestamp, then 20 ms of interleaved
// little-endian f32 at 48 kHz, every sample of channel `c` holding (c + 1) / 10.
std::vector<uint8_t> PcmFrame(uint64_t timestamp_us, uint32_t channels)
{
	std::vector<uint8_t> frame;
	for (int shift = 24; shift >= 0; shift -= 8)
		frame.push_back(static_cast<uint8_t>((shift == 24 ? 0x80 : 0) | (timestamp_us >> shift)));
	for (int sample = 0; sample < 960; sample++) {
		for (uint32_t c = 0; c < channels; c++) {
			const float value = static_cast<float>(c + 1) / 10;
			uint8_t bytes[sizeof(float)];
			memcpy(bytes, &value, sizeof(float));
			frame.insert(frame.end(), bytes, bytes + sizeof(float));
		}
	}
	return frame;
}

// The catalog the source is reading, fetched over a local consumer so a scenario can
// edit it and deliver it as if the publisher had.
moq::Catalog CatalogSnapshot(Publisher &publisher)
{
	auto consumer = TestOk(publisher.broadcast->consume(), "local broadcast consumer");
	auto catalogs = TestOk(moq::MediaCatalogConsumer::subscribe(consumer).get(), "local catalog");
	auto snapshot = TestOk(catalogs->next().get(), "catalog snapshot");
	CHECK(snapshot.has_value());
	return *snapshot;
}
} // namespace

int main()
{
	register_moq_source();
	CHECK(g_registered);

	// The TLS key log is off unless the toggle asks for it, and reaches the dial's QUIC config.
	{
		reset("");
		void *source = createSource();
		auto *ctx = static_cast<struct moq_source *>(source);
		CHECK(!ctx->keylog);
		CHECK(moq_source_client_config(ctx->keylog).quic.keylog != true);

		g_settings.keylog = true;
		g_info.update(source, settingsData());
		CHECK(ctx->keylog);
		CHECK(moq_source_client_config(ctx->keylog).quic.keylog == true);
		destroySource(source);
	}
	report("key log is opt-in");

	// Connect, wait for the announcement, read the catalog, and decode video and
	// audio from the tracks it names. moq-audio decodes the Opus to planar float at
	// the OBS rate, stamped on the same broadcast timeline as the video.
	{
		TestRelay relay(true, {"moq-transport-18"});
		reset(relay.Url());
		Publisher publisher(relay, "obs/test");
		void *source = createSource();
		CHECK(publisher.WriteUntil([] { return g_output_frames > 0 && g_output_audio > 0; }));
		CHECK(g_sws_scales > 0);
		const auto audio = lastAudio();
		CHECK(audio.format == AUDIO_FORMAT_FLOAT_PLANAR);
		CHECK(audio.speakers == SPEAKERS_STEREO);
		CHECK(audio.samples_per_sec == 48000);
		CHECK(audio.frames == 960);
		// Both land on the publisher's 20 ms grid, unshifted: the shared timebase.
		const auto on_grid = [](uint64_t ns) {
			return ns % 1000 == 0 && ns / 1000 > 1'000'000 && (ns / 1000 - 1'000'000) % 20'000 == 0;
		};
		CHECK(on_grid(g_last_timestamp));
		CHECK(on_grid(audio.timestamp));
		// The plugin names itself ahead of the C++ package, so a relay can tell it apart.
		CHECK(relay.Advertised("moq-obs/dev moq-cpp/"));
		destroySource(source);
	}
	report("plays video and audio");

	// Gaps reset references immediately. An encoder end keeps old non-key tail packets
	// decodable until a fresh frame reaches its endpoint, or until EOF drains the tail.
	for (int boundary : {0, 1, 2}) {
		const bool gap = boundary == 0;
		const bool eof = boundary == 2;
		TestRelay relay;
		reset(relay.Url());
		g_reorder_delay = 2;
		Publisher publisher(relay, "obs/test", true, false);
		void *source = createSource();
		CHECK(publisher.WriteUntil([] { return g_output_frames > 0; }));
		auto *ctx = static_cast<moq_source *>(source);
		std::shared_ptr<Track> held;
		{
			std::lock_guard<std::mutex> lock(ctx->mutex);
			const auto &conn = ctx->connection;
			held = conn->video;
			CHECK(held != nullptr);
			const int frames = g_output_frames;
			const auto *state = static_cast<DecodeState *>(held->video_decoder->codec_ctx->opaque);
			const auto pending = state->held.size();
			CHECK(pending > 0);
			const uint64_t end = static_cast<uint64_t>(state->last_pts) + 1'000'000;
			auto inject = [&](moq::MediaRead read) {
				moq_source_on_frame(ctx, conn, held, std::optional<moq::MediaRead>{std::move(read)});
			};
			if (gap) {
				inject(moq::MediaRead{moq::MediaRead::kGap{123456, std::chrono::microseconds(78901)}});
				CHECK(g_output_frames == frames + static_cast<int>(pending));
				CHECK(!held->video_decoder->got_keyframe);
			} else {
				inject(moq::MediaRead{moq::MediaRead::kEnd{end}});
				CHECK(held->video_decoder->got_keyframe);
				// Non-key tail packets below the endpoint still depend on the old references.
				inject(moq::MediaRead{
					moq::MediaRead::kFrame{moq::MediaFrame{TestH264Keyframe(), end - 1, false}}});
				CHECK(state->last_pts == static_cast<int64_t>(end - 1));
				if (eof) {
					moq_source_on_frame(ctx, conn, held, std::optional<moq::MediaRead>{});
				} else {
					inject(moq::MediaRead{moq::MediaRead::kFrame{
						moq::MediaFrame{TestH264Keyframe(), end, true}}});
					CHECK(state->last_pts == static_cast<int64_t>(end));
					CHECK(state->held.size() == 1);
				}
				CHECK(g_output_frames == frames + static_cast<int>(pending) + 1);
				std::lock_guard<std::mutex> output_lock(g_video_mutex);
				CHECK(std::count(g_video_timestamps.begin(), g_video_timestamps.end(),
						 (end - 1) * 1000) == 1);
			}
			CHECK(conn->video == held);
		}
		if (gap) {
			const int frames = g_output_frames;
			CHECK(publisher.WriteUntil([&] { return g_output_frames > frames; }));
		}
		held.reset();
		destroySource(source);
	}
	report("media breaks preserve decoder tail and subscription ownership");

	// H.264 with B-frames arrives in decode order. Each picture reaches OBS with its own
	// presentation timestamp, and the pictures the decoder still holds when the track
	// ends cleanly are drained rather than dropped.
	{
		TestRelay relay;
		reset(relay.Url());
		g_reorder_delay = 2;
		Publisher publisher(relay, "obs/test", true, false);
		void *source = createSource();
		auto *ctx = static_cast<moq_source *>(source);
		// Write only once the source reads the track, so it sees the whole group.
		CHECK(WaitFor([&] {
			std::lock_guard lock(ctx->mutex);
			const auto &conn = ctx->connection;
			return conn && conn->pending_video && conn->pending_video->consumer;
		}));

		// IPBB order: each P-frame is sent before the two B-frames it presents after.
		const int decode_order[] = {0, 3, 1, 2, 6, 4, 5, 9, 7, 8, 11, 10};
		const std::vector<uint8_t> delta = {0x00, 0x00, 0x00, 0x01, 0x41, 0x9a, 0x00, 0x33, 0xff};
		const auto timestamp_us = [](int picture) {
			return 2'000'000 + uint64_t(picture) * 20'000;
		};
		for (int picture : decode_order) {
			const auto payload = picture == 0 ? TestH264Keyframe() : delta;
			TestOk(publisher.video->write_frame(moq::Frame{payload, timestamp_us(picture)}), "write video");
		}
		TestOk(publisher.video->finish(), "finish video");

		std::vector<uint64_t> expected;
		for (int picture = 0; picture < 12; picture++)
			expected.push_back(timestamp_us(picture) * 1000);
		CHECK(WaitFor([] { return g_output_frames >= 12; }));
		destroySource(source);
		std::lock_guard<std::mutex> lock(g_video_mutex);
		CHECK(g_video_timestamps == expected);
	}
	report("reordered video keeps decoded timestamps and drains at eof");

	// A picture that fails to decode stays in the decoder, which then refuses the next
	// packet until that picture is taken. The refused packet is sent again, not skipped.
	{
		TestRelay relay;
		reset(relay.Url());
		g_decoder_capacity = 1;
		const auto timestamp_us = [](int picture) {
			return 2'000'000 + uint64_t(picture) * 20'000;
		};
		g_unconverted_pts = int64_t(timestamp_us(2));
		Publisher publisher(relay, "obs/test", true, false);
		void *source = createSource();
		auto *ctx = static_cast<moq_source *>(source);
		CHECK(WaitFor([&] {
			std::lock_guard lock(ctx->mutex);
			const auto &conn = ctx->connection;
			return conn && conn->pending_video && conn->pending_video->consumer;
		}));

		const std::vector<uint8_t> delta = {0x00, 0x00, 0x00, 0x01, 0x41, 0x9a, 0x00, 0x33, 0xff};
		std::vector<uint64_t> expected;
		for (int picture = 0; picture < 6; picture++) {
			const auto payload = picture == 0 ? TestH264Keyframe() : delta;
			TestOk(publisher.video->write_frame(moq::Frame{payload, timestamp_us(picture)}), "write video");
			expected.push_back(timestamp_us(picture) * 1000);
		}
		// Left unfinished: nothing is held back, and a finish appends an empty end-of-group frame.

		CHECK(WaitFor([] { return g_output_frames >= 6; }));
		destroySource(source);
		std::lock_guard<std::mutex> lock(g_video_mutex);
		CHECK(g_video_timestamps == expected);
	}
	report("a refused packet is resent once the decoder drains");

	// A replacement that ends before its first picture is dropped, not drained over the
	// video that is still playing.
	{
		TestRelay relay;
		reset(relay.Url());
		g_reorder_delay = 2;
		Publisher publisher(relay, "obs/test", true, false);
		void *source = createSource();
		auto *ctx = static_cast<moq_source *>(source);
		CHECK(publisher.WriteUntil([] { return g_output_frames > 0; }));
		moq::VideoInit init{};
		init.format = moq::VideoFormat::kAvc3;
		init.data = TestH264Init();
		auto replacement = TestOk(
			moq::MediaTrackProducer::video(publisher.broadcast,
						       moq::MediaTarget::kNamed{std::string("!replacement")}, init),
			"replacement video");
		const auto pending = [&] {
			std::lock_guard lock(ctx->mutex);
			const auto &candidate = ctx->connection->pending_video;
			return candidate && candidate->consumer;
		};
		CHECK(publisher.WriteUntil(pending));
		// One picture, which the decoder holds until a drain.
		TestOk(replacement->write_frame(moq::Frame{TestH264Keyframe(), 1'000'000'000}), "replacement frame");
		TestOk(replacement->finish(), "finish replacement");
		CHECK(publisher.WriteUntil([&] { return !pending(); }));
		const int frames = g_output_frames;
		CHECK(publisher.WriteUntil([&] { return g_output_frames >= frames + 3; }));
		destroySource(source);
		std::lock_guard<std::mutex> lock(g_video_mutex);
		CHECK(std::none_of(g_video_timestamps.begin(), g_video_timestamps.end(),
				   [](uint64_t ns) { return ns >= 1'000'000'000'000; }));
	}
	report("ended replacement leaves playback");

	// AAC-LC from an ingest decodes through moq-audio, resampled from 44.1 kHz to the
	// OBS rate and placed on the mono speaker.
	{
		TestRelay relay;
		reset(relay.Url());
		Publisher publisher(relay, "obs/test", false, false);
		moq::AudioInit init{};
		init.format = moq::AudioFormat::kAac;
		init.data = AAC_CONFIG;
		auto aac = TestOk(moq::MediaTrackProducer::audio(publisher.broadcast, moq::MediaTarget::kNamed{}, init),
				  "aac producer");
		void *source = createSource();
		uint64_t samples = 0;
		CHECK(WaitFor([&] {
			for (const auto &frame : AAC_FRAMES) {
				TestOk(aac->write_frame(moq::Frame{frame, 1'000'000 + samples * 1'000'000 / 44'100}),
				       "write aac");
				samples += 1024;
			}
			TestOk(aac->cut(), "cut aac");
			std::this_thread::sleep_for(std::chrono::milliseconds(10));
			return lastAudio().peak > 0.5f;
		}));
		const auto audio = lastAudio();
		CHECK(audio.speakers == SPEAKERS_MONO);
		CHECK(audio.samples_per_sec == 48000);
		CHECK(g_output_frames == 0);
		destroySource(source);
	}
	report("aac resampled to the obs rate");

	// PCM is placed by its WAVE-convention layout: 5.1 straight onto the OBS 5.1
	// speakers, quad upmixed onto the 5.1 surround pair, and nine channels refused
	// while the current audio keeps playing.
	for (uint32_t channels : {6u, 4u, 9u}) {
		TestRelay relay;
		reset(relay.Url());
		Publisher publisher(relay, "obs/test");
		void *source = createSource();
		CHECK(publisher.WriteUntil([] { return g_output_frames > 0 && g_output_audio > 0; }));
		auto pcm = TestOk(publisher.broadcast->publish_track("pcm", std::nullopt), "pcm track");
		auto snapshot = CatalogSnapshot(publisher);
		auto rendition = snapshot.audio.begin()->second;
		rendition.codec = "pcm";
		rendition.track = "pcm";
		rendition.description.reset();
		rendition.bitrate.reset();
		rendition.sample_rate = 48000;
		rendition.channel_count = channels;
		snapshot.audio.clear();
		snapshot.audio.emplace("pcm", rendition);
		auto *ctx = static_cast<moq_source *>(source);
		{
			std::lock_guard<std::mutex> lock(ctx->mutex);
			moq_source_subscribe_audio(ctx, ctx->connection, snapshot);
			CHECK((ctx->connection->pending_audio != nullptr) == (channels != 9));
		}
		if (channels == 9) {
			const int audio = g_output_audio;
			CHECK(publisher.WriteUntil([&] { return g_output_audio >= audio + 3; }));
		} else {
			uint64_t timestamp_us = 2'000'000;
			CHECK(WaitFor([&] {
				TestOk(pcm->write_frame(moq::Frame{PcmFrame(timestamp_us, channels), std::nullopt}),
				       "write pcm");
				timestamp_us += 20'000;
				std::this_thread::sleep_for(std::chrono::milliseconds(10));
				return lastAudio().speakers == SPEAKERS_5POINT1;
			}));
			const auto audio = lastAudio();
			CHECK(audio.samples_per_sec == 48000);
			CHECK(audio.frames == 960);
			// PCM has no codec delay, so each frame keeps its packet's timestamp.
			CHECK(audio.timestamp % 20'000'000 == 0 && audio.timestamp >= 2'000'000'000);
			const float six[] = {0.1f, 0.2f, 0.3f, 0.4f, 0.5f, 0.6f};
			const float quad[] = {0.1f, 0.2f, 0, 0, 0.3f, 0.4f};
			const float *expected = channels == 6 ? six : quad;
			for (int i = 0; i < 6; i++)
				CHECK(std::fabs(audio.first[i] - expected[i]) < 1e-6f);
		}
		destroySource(source);
		report(channels == 6   ? "pcm 5.1 placed on obs 5.1"
		       : channels == 4 ? "pcm quad upmixed to obs 5.1"
				       : "pcm with no obs layout refused");
	}

	// A metadata-only snapshot leaves the same rendition and decoder running.
	{
		TestRelay relay;
		reset(relay.Url());
		Publisher publisher(relay, "obs/test");
		void *source = createSource();
		CHECK(publisher.WriteUntil([] { return g_output_frames > 0 && g_output_audio > 0; }));
		const int opens = g_decoder_opens;
		const int frames = g_output_frames;
		auto catalog = TestOk(moq::MediaCatalogProducer::init(publisher.broadcast), "catalog");
		moq::VideoProperties properties{};
		properties.rotation = 90;
		TestOk(catalog->set_video_properties(properties), "catalog properties");
		CHECK(publisher.WriteUntil([&] { return g_output_frames >= frames + 10; }));
		CHECK(g_decoder_opens == opens);
		destroySource(source);
	}
	report("unchanged rendition keeps decoder");

	// The same track name with a new decoder configuration is a replacement; metadata alone is not.
	{
		TestRelay relay;
		reset(relay.Url());
		Publisher publisher(relay, "obs/test");
		void *source = createSource();
		CHECK(publisher.WriteUntil([] { return g_output_frames > 0 && g_output_audio > 0; }));
		auto consumer = TestOk(publisher.broadcast->consume(), "local broadcast consumer");
		auto catalogs = TestOk(moq::MediaCatalogConsumer::subscribe(consumer).get(), "local catalog");
		auto snapshot = TestOk(catalogs->next().get(), "catalog snapshot");
		CHECK(snapshot.has_value());
		auto &video = snapshot->video.begin()->second;
		auto &audio = snapshot->audio.begin()->second;
		auto *ctx = static_cast<moq_source *>(source);
		{
			std::lock_guard<std::mutex> lock(ctx->mutex);
			const auto &conn = ctx->connection;
			// The catalog consumer exposes the default, and preference-only metadata keeps the decoder.
			CHECK(video.preference == 0);
			video.label = "relabeled";
			video.bitrate = 1;
			video.preference = -1;
			audio.label = "relabeled";
			audio.bitrate = 1;
			moq_source_subscribe_video(ctx, conn, *snapshot);
			moq_source_subscribe_audio(ctx, conn, *snapshot);
			CHECK(!conn->pending_video && !conn->pending_audio);
			// Keyed apart from its track, the rendition still names the track already playing.
			auto rekeyed = *snapshot;
			auto entry = rekeyed.video.extract(rekeyed.video.begin());
			entry.mapped().track = entry.key();
			entry.key() = "main";
			rekeyed.video.insert(std::move(entry));
			moq_source_subscribe_video(ctx, conn, rekeyed);
			CHECK(!conn->pending_video);
			video.coded = moq::Dimensions{(video.coded ? video.coded->width : 0) + 16, 16};
			audio.sample_rate += 1;
			moq_source_subscribe_video(ctx, conn, *snapshot);
			moq_source_subscribe_audio(ctx, conn, *snapshot);
			CHECK(conn->pending_video && conn->pending_audio);
		}
		destroySource(source);
	}
	report("reshaped rendition replaces decoder");

	// A new rendition that has not delivered a frame cannot stop the old track.
	{
		TestRelay relay;
		reset(relay.Url());
		Publisher publisher(relay, "obs/test");
		void *source = createSource();
		CHECK(publisher.WriteUntil([] { return g_output_frames > 0; }));
		moq::VideoInit init{};
		init.format = moq::VideoFormat::kAvc3;
		init.data = TestH264Init();
		auto replacement = TestOk(
			moq::MediaTrackProducer::video(publisher.broadcast,
						       moq::MediaTarget::kNamed{std::string("!replacement")}, init),
			"replacement");
		const int frames = g_output_frames;
		CHECK(publisher.WriteUntil([&] { return g_output_frames >= frames + 10; }));
		CHECK(publisher.WriteUntil([&] {
			TestOk(replacement->write_frame(moq::Frame{TestH264Keyframe(), 2'000'000}),
			       "replacement frame");
			return g_last_timestamp == 2'000'000'000;
		}));
		destroySource(source);
	}
	report("replacement waits for first frame");

	// A keyframe flag or accepted packet is not proof that the replacement decodes.
	for (int failure : {2, 3}) {
		TestRelay relay;
		reset(relay.Url());
		Publisher publisher(relay, "obs/test");
		void *source = createSource();
		CHECK(publisher.WriteUntil([] { return g_output_frames > 0 && g_output_audio > 0; }));
		g_candidate_decode = failure;
		moq::VideoInit init{};
		init.format = moq::VideoFormat::kAvc3;
		init.data = TestH264Init();
		auto replacement = TestOk(
			moq::MediaTrackProducer::video(publisher.broadcast,
						       moq::MediaTarget::kNamed{std::string("!replacement")}, init),
			"replacement video");
		auto write = [&] {
			TestOk(replacement->write_frame(moq::Frame{TestH264Keyframe(), 1'000'000'000}),
			       "replacement frame");
		};
		CHECK(publisher.WriteUntil([&] {
			write();
			return g_candidate_attempts > 0;
		}));
		const int frames = g_output_frames;
		CHECK(publisher.WriteUntil([&] { return g_output_frames >= frames + 3; }));
		if (failure == 3) {
			// Metadata-only updates must preserve the candidate's accumulated decode state.
			auto snapshot = CatalogSnapshot(publisher);
			auto *ctx = static_cast<moq_source *>(source);
			{
				std::lock_guard lock(ctx->mutex);
				auto pending = ctx->connection->pending_video;
				CHECK(pending != nullptr);
				moq_source_subscribe_video(ctx, ctx->connection, snapshot);
				CHECK(ctx->connection->pending_video == pending);
			}
			g_candidate_decode = 0;
			CHECK(publisher.WriteUntil([&] {
				write();
				return g_last_timestamp == 1'000'000'000'000;
			}));
		}
		destroySource(source);
		report(failure == 2 ? "failed video candidate preserves playback" : "video waits for decoded output");
	}

	// An audio replacement whose packets moq-audio refuses is dropped, and the current
	// audio keeps playing.
	{
		TestRelay relay;
		reset(relay.Url());
		Publisher publisher(relay, "obs/test");
		void *source = createSource();
		CHECK(publisher.WriteUntil([] { return g_output_audio > 0; }));
		auto *ctx = static_cast<moq_source *>(source);
		std::weak_ptr<Track> active;
		{
			std::lock_guard<std::mutex> lock(ctx->mutex);
			active = ctx->connection->audio;
		}
		moq::AudioInit init{};
		init.format = moq::AudioFormat::kOpus;
		init.data = TestOpusHead();
		auto replacement = TestOk(
			moq::MediaTrackProducer::audio(publisher.broadcast,
						       moq::MediaTarget::kNamed{std::string("!replacement")}, init),
			"replacement audio");
		std::weak_ptr<Track> candidate;
		CHECK(publisher.WriteUntil([&] {
			std::lock_guard<std::mutex> lock(ctx->mutex);
			const auto &pending = ctx->connection->pending_audio;
			if (!pending || !pending->audio_consumer)
				return false;
			candidate = pending;
			std::lock_guard<std::mutex> failure_lock(g_candidate_mutex);
			g_candidate = {candidate, false};
			return true;
		}));
		CHECK(publisher.WriteUntil([&] {
			TestOk(replacement->write_frame(moq::Frame{BAD_OPUS, 1'000'000'000}), "replacement frame");
			TestOk(replacement->cut(), "replacement audio cut");
			std::lock_guard<std::mutex> lock(g_candidate_mutex);
			return g_candidate.failed && candidate.expired();
		}));
		{
			std::lock_guard<std::mutex> lock(ctx->mutex);
			auto playing = active.lock();
			CHECK(playing && ctx->connection->audio == playing);
		}
		const int audio = g_output_audio;
		CHECK(publisher.WriteUntil([&] { return g_output_audio >= audio + 3; }));
		CHECK(g_last_audio_timestamp < 1'000'000'000'000);
		destroySource(source);
	}
	report("failed audio candidate preserves playback");

	// A packet moq-audio refuses on the active audio track stops only that track; the
	// pending replacement still takes over without another catalog update.
	{
		TestRelay relay;
		reset(relay.Url());
		Publisher publisher(relay, "obs/test");
		void *source = createSource();
		CHECK(publisher.WriteUntil([] { return g_output_audio > 0; }));
		moq::AudioInit init{};
		init.format = moq::AudioFormat::kOpus;
		init.data = TestOpusHead();
		auto replacement = TestOk(
			moq::MediaTrackProducer::audio(publisher.broadcast,
						       moq::MediaTarget::kNamed{std::string("!replacement")}, init),
			"replacement audio");
		auto *ctx = static_cast<moq_source *>(source);
		std::shared_ptr<Track> pending;
		CHECK(WaitFor([&] {
			std::lock_guard<std::mutex> lock(ctx->mutex);
			pending = ctx->connection->pending_audio;
			return pending && pending->audio_consumer;
		}));
		TestOk(publisher.audio->write_frame(moq::Frame{BAD_OPUS, 900'000'000}), "malformed frame");
		TestOk(publisher.audio->cut(), "malformed cut");
		CHECK(WaitFor([&] {
			std::lock_guard<std::mutex> lock(ctx->mutex);
			return !ctx->connection->audio;
		}));
		{
			std::lock_guard<std::mutex> lock(ctx->mutex);
			CHECK(ctx->connection->pending_audio == pending);
		}
		pending.reset();
		uint64_t timestamp_us = 1'000'000'000;
		CHECK(WaitFor([&] {
			TestOk(replacement->write_frame(moq::Frame{{0xfc, 0xff, 0xfe}, timestamp_us}),
			       "replacement frame");
			TestOk(replacement->cut(), "replacement audio cut");
			timestamp_us += 20'000;
			std::this_thread::sleep_for(std::chrono::milliseconds(10));
			return g_last_audio_timestamp >= 1'000'000'000'000;
		}));
		destroySource(source);
	}
	report("failed active audio keeps pending replacement");

	// A decoded frame that is not whole samples of the track's layout stops the track
	// rather than reaching OBS.
	{
		TestRelay relay;
		reset(relay.Url());
		Publisher publisher(relay, "obs/test");
		void *source = createSource();
		CHECK(publisher.WriteUntil([] { return g_output_audio > 0; }));
		auto *ctx = static_cast<moq_source *>(source);
		{
			std::lock_guard<std::mutex> lock(ctx->mutex);
			auto conn = ctx->connection;
			auto active = conn->audio;
			CHECK(active != nullptr);
			obs_source_audio audio = {};
			moq::expected<std::optional<moq::AudioRead>> misaligned(
				moq::AudioRead::kFrame{moq::AudioFrame{1, {0, 0, 0}}});
			CHECK(!moq_source_on_audio_read(ctx, conn, active, misaligned, audio));
			CHECK(!conn->audio);
		}
		destroySource(source);
	}
	report("misaligned audio frame stops the track");

	// The public publishing wrapper cannot author a sibling-reference catalog, so deliver
	// that snapshot directly; resolution and subscription still cross the real relay.
	for (bool sibling : {false, true}) {
		TestRelay relay;
		reset(relay.Url());
		Publisher publisher(relay, "obs/test");
		void *source = createSource();
		CHECK(publisher.WriteUntil([] { return g_output_frames > 0; }));
		auto consumer = TestOk(publisher.broadcast->consume(), "local broadcast consumer");
		auto catalogs = TestOk(moq::MediaCatalogConsumer::subscribe(consumer).get(), "local catalog");
		auto snapshot = TestOk(catalogs->next().get(), "catalog snapshot");
		CHECK(snapshot.has_value());
		auto config = snapshot->video.begin()->second;
		std::shared_ptr<moq::Publication> hidden;
		if (sibling) {
			hidden = TestOk(relay.origin->create_broadcast("obs/hidden"), "unannounced sibling");
			config.broadcast = "./hidden";
			config.epoch = "01900000-0000-7000-8000-000000000001";
		}
		snapshot->video.clear();
		snapshot->video.emplace("!unavailable", config);
		auto *ctx = static_cast<moq_source *>(source);
		std::weak_ptr<Track> candidate;
		std::weak_ptr<Track> active;
		{
			std::lock_guard<std::mutex> lock(ctx->mutex);
			active = ctx->connection->video;
			moq_source_subscribe_video(ctx, ctx->connection, *snapshot);
			CHECK(ctx->connection->pending_video != nullptr);
			candidate = ctx->connection->pending_video;
			std::lock_guard<std::mutex> failure_lock(g_candidate_mutex);
			g_candidate = {candidate, false};
		}
		if (hidden) {
			// An unannounced sibling stays pending, without interrupting current playback.
			const int frames = g_output_frames;
			CHECK(publisher.WriteUntil([&] { return g_output_frames >= frames + 3; }));
			// A different publisher instance is a real resolution failure.
			moq::AnnounceRoute route;
			route.epoch = "01900000-0000-7000-8000-000000000002";
			TestOk(hidden->announce(route), "announce mismatched sibling");
		}
		CHECK(WaitFor([] {
			std::lock_guard<std::mutex> lock(g_candidate_mutex);
			return g_candidate.failed;
		}));
		{
			std::lock_guard<std::mutex> lock(ctx->mutex);
			auto playing = active.lock();
			CHECK(ctx->connection && playing && ctx->connection->video == playing);
			CHECK(candidate.expired());
		}
		const int frames = g_output_frames;
		CHECK(publisher.WriteUntil([&] { return g_output_frames >= frames + 3; }));
		destroySource(source);
		report(sibling ? "failed sibling resolution preserves playback"
			       : "rejected subscription preserves playback");
	}

	// A replacement that cannot prepare a decoder leaves current playback alive.
	{
		TestRelay relay;
		reset(relay.Url());
		Publisher publisher(relay, "obs/test");
		void *source = createSource();
		CHECK(publisher.WriteUntil([] { return g_output_frames > 0; }));
		g_find_decoder_ok = false;
		moq::VideoInit init{};
		init.format = moq::VideoFormat::kAvc3;
		init.data = TestH264Init();
		auto replacement = TestOk(
			moq::MediaTrackProducer::video(publisher.broadcast,
						       moq::MediaTarget::kNamed{std::string("!replacement")}, init),
			"failed replacement");
		const int frames = g_output_frames;
		CHECK(publisher.WriteUntil([&] { return g_output_frames >= frames + 10; }));
		destroySource(source);
	}
	report("failed replacement preserves playback");

	// A broadcast with no video rendition plays its audio and blanks the video.
	{
		TestRelay relay;
		reset(relay.Url());
		Publisher publisher(relay, "obs/test", false, true);
		void *source = createSource();
		const int blanks = g_blank_calls;
		CHECK(publisher.WriteUntil([] { return g_output_audio > 0; }));
		CHECK(g_blank_calls > blanks);
		CHECK(g_output_frames == 0);
		destroySource(source);
	}
	report("audio only");

	// A catalog naming a video codec FFmpeg can't open leaves the video blank, not half
	// installed, and the audio still plays.
	{
		TestRelay relay;
		reset(relay.Url());
		Publisher publisher(relay, "obs/test");
		g_find_decoder_ok = false;
		void *source = createSource();
		CHECK(publisher.WriteUntil([] { return g_output_audio > 0; }));
		CHECK(g_output_frames == 0);
		destroySource(source);
	}
	report("undecodable rendition");

	// Another publisher instance taking over the path while the source plays, as a GOAWAY
	// drain's replacement session does, plays without a blank. The old instance keeps playing
	// until the new one delivers, and the new one keeps playing once the old one ends.
	{
		TestRelay relay;
		reset(relay.Url());
		auto first = std::make_unique<Publisher>(relay, "obs/test");
		void *source = createSource();
		CHECK(first->WriteUntil([] { return g_output_frames > 0 && g_output_audio > 0; }));
		const int blanks = g_blank_calls;
		const int opens = g_decoder_opens;
		Publisher second(relay, "obs/test", true, true, 1'000'000'000);
		// The new instance's video candidate opens its own decoder while the old one still plays.
		CHECK(first->WriteUntil([&] { return g_decoder_opens == opens + 1; }));
		int frames = g_output_frames;
		int audio = g_output_audio;
		CHECK(first->WriteUntil(
			[&] { return g_output_frames >= frames + 10 && g_output_audio >= audio + 10; }));
		CHECK(!PlaysFrom(1'000'000'000));
		CHECK(second.WriteUntil([] { return PlaysFrom(1'000'000'000); }));
		first.reset();
		frames = g_output_frames;
		audio = g_output_audio;
		CHECK(second.WriteUntil(
			[&] { return g_output_frames >= frames + 10 && g_output_audio >= audio + 10; }));
		CHECK(g_blank_calls == blanks);
		CHECK(g_decoder_opens == opens + 1);
		destroySource(source);
	}
	report("follows a replacement instance");

	// A replacement instance whose catalog subscribe fails stops the old instance too, blanks,
	// and waits for the next instance.
	{
		TestRelay relay;
		reset(relay.Url());
		Publisher first(relay, "obs/test");
		void *source = createSource();
		CHECK(first.WriteUntil([] { return g_output_frames > 0 && g_output_audio > 0; }));
		const int blanks = g_blank_calls;
		// The relay cannot refuse a catalog, so deliver the failure once the replacement is
		// handed out; its own catalog never arrives, since it publishes no tracks.
		auto *ctx = static_cast<moq_source *>(source);
		Publisher broken(relay, "obs/test", false, false);
		CHECK(first.WriteUntil([&] {
			std::lock_guard<std::mutex> lock(ctx->mutex);
			return ctx->connection->instance == 2;
		}));
		{
			std::lock_guard<std::mutex> lock(ctx->mutex);
			CHECK(ctx->connection->video && ctx->connection->audio);
			moq_source_on_catalogs(ctx, ctx->connection,
					       moq::unexpected(moq::Error(moq::Error::kInternal{"catalog refused"})));
			CHECK(!ctx->connection->video && !ctx->connection->audio);
		}
		CHECK(g_blank_calls > blanks);
		Publisher third(relay, "obs/test", true, true, 1'000'000'000);
		CHECK(third.WriteUntil([] { return PlaysFrom(1'000'000'000); }));
		destroySource(source);
	}
	report("failed replacement catalog waits for the next instance");

	// A publisher that restarts, ending its broadcast before the new instance announces, plays
	// again once it returns.
	{
		TestRelay relay;
		reset(relay.Url());
		void *source = createSource();
		{
			Publisher first(relay, "obs/test");
			CHECK(first.WriteUntil([] { return g_output_frames > 0 && g_output_audio > 0; }));
		}
		Publisher second(relay, "obs/test", true, true, 1'000'000'000);
		CHECK(second.WriteUntil([] { return PlaysFrom(1'000'000'000); }));
		destroySource(source);
	}
	report("follows a publisher restart");

	// The relay dropping the session: the client reconnects, and the broadcast announced over
	// the new connect is another instance to play.
	{
		TestRelay relay;
		reset(relay.Url());
		Publisher publisher(relay, "obs/test");
		void *source = createSource();
		CHECK(publisher.WriteUntil([] { return g_output_frames > 0 && g_output_audio > 0; }));
		relay.DropSessions();
		CHECK(WaitFor([&] { return relay.Accepted() == 2; }));
		const int frames = g_output_frames;
		const int audio = g_output_audio;
		CHECK(publisher.WriteUntil(
			[&] { return g_output_frames >= frames + 10 && g_output_audio >= audio + 10; }));
		destroySource(source);
	}
	report("follows a session reconnect");

	// Changing the broadcast drops the old connection and plays the new one.
	{
		TestRelay relay;
		reset(relay.Url());
		Publisher first(relay, "obs/test");
		Publisher second(relay, "obs/other");
		void *source = createSource();
		CHECK(first.WriteUntil([] { return g_output_frames > 0; }));

		g_settings.broadcast = "obs/other";
		const int blanks = g_blank_calls;
		g_info.update(source, settingsData());
		CHECK(g_blank_calls > blanks);
		g_output_frames = 0;
		CHECK(second.WriteUntil([] { return g_output_frames > 0; }));
		destroySource(source);
	}
	report("settings change reconnects");

	// Invalid settings disconnect and blank without dialing.
	{
		TestRelay relay;
		reset(relay.Url());
		Publisher publisher(relay, "obs/test");
		void *source = createSource();
		CHECK(publisher.WriteUntil([] { return g_output_frames > 0; }));
		g_settings.broadcast = "";
		const int blanks = g_blank_calls;
		g_info.update(source, settingsData());
		CHECK(g_blank_calls == blanks + 1);
		const int frames = g_output_frames;
		for (int i = 0; i < 10; i++)
			publisher.Write();
		std::this_thread::sleep_for(std::chrono::milliseconds(50));
		CHECK(g_output_frames == frames);
		destroySource(source);
	}
	report("invalid settings disconnect");

	// A connect that fails blanks the source and leaves nothing pending.
	{
		reset(BAD_URL);
		void *source = createSource();
		const int blanks = g_blank_calls;
		CHECK(WaitFor([&] { return g_blank_calls > blanks; }));
		destroySource(source);
	}
	report("connect failure");

	// Destroy while the connect is still pending: it is cancelled, not waited on.
	{
		TestRelay relay(false);
		reset(relay.Url());
		void *source = createSource();
		std::this_thread::sleep_for(std::chrono::milliseconds(20));
		destroySource(source);
	}
	report("destroy during connect");

	// Destroy while waiting for a broadcast that is never announced.
	{
		TestRelay relay;
		reset(relay.Url(), "obs/missing");
		void *source = createSource();
		CHECK(WaitFor([&] { return relay.Accepted() == 1; }));
		std::this_thread::sleep_for(std::chrono::milliseconds(20));
		destroySource(source);
	}
	report("destroy waiting for the broadcast");

	// Destroy mid-stream, repeatedly, so frames are being decoded on the worker
	// while the source goes away.
	{
		TestRelay relay;
		Publisher publisher(relay, "obs/test");
		std::atomic<bool> writing{true};
		std::thread writer([&] {
			while (writing) {
				publisher.Write();
				std::this_thread::sleep_for(std::chrono::milliseconds(2));
			}
		});
		for (int round = 0; round < 10; round++) {
			reset(relay.Url());
			void *source = createSource();
			CHECK(WaitFor([] { return g_output_frames > 0; }));
			destroySource(source);
		}
		writing = false;
		writer.join();
	}
	report("destroy mid-stream");

	// The publisher ends the broadcast; the source keeps its last state and
	// tears down cleanly afterwards.
	{
		TestRelay relay;
		reset(relay.Url());
		void *source = createSource();
		{
			Publisher publisher(relay, "obs/test");
			CHECK(publisher.WriteUntil([] { return g_output_frames > 0; }));
		}
		std::this_thread::sleep_for(std::chrono::milliseconds(50));
		destroySource(source);
	}
	report("broadcast ends");

	if (g_failures) {
		fprintf(stderr, "%d failure(s)\n", g_failures);
		return 1;
	}
	printf("all moq_source tests passed\n");
	return 0;
}
