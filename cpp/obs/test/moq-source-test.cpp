// SPDX-License-Identifier: GPL-2.0-or-later
//
// Drives the real moq_source, reached through the obs_source_info it registers,
// against stubbed libobs and FFmpeg and the real moq-ffi, over a relay in the same
// process. What the consume path can get wrong is mostly an ordering: an
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
#include <atomic>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <cstring>
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
#include <libavutil/channel_layout.h>
#include <libavutil/samplefmt.h>
#include <libswscale/swscale.h>
}

#include "moq-source.h"
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
// PCM buffers the audio decode path handed to OBS.
std::atomic<int> g_output_audio{0};
std::atomic<uint64_t> g_last_audio_timestamp{0};
std::atomic<int> g_audio_frame_unrefs{0};

// The bmalloc/bfree balance, so a frame buffer that outlives its decoder shows up
// as a leak.
std::atomic<long> g_live_allocs{0};
} // namespace

extern "C" {

// Deliberately silent: stdio locking would create happens-before edges between
// the test thread and the source's worker and mask the races we're after.
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

void obs_source_output_audio(obs_source_t *, const struct obs_source_audio *audio)
{
	if (!audio)
		return;
	g_last_audio_timestamp = audio->timestamp;
	g_output_audio++;
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
int g_audio_send_result = 0;
int g_audio_receive_result = 0;
int g_decoded_width = 320;
int g_decoded_height = 240;
enum AVSampleFormat g_decoded_audio_format = AV_SAMPLE_FMT_FLTP;
int g_decoded_audio_samples = 960;
int64_t g_decoded_audio_pts = AV_NOPTS_VALUE;
int g_decoded_audio_channels = 2;
enum AVChannelOrder g_decoded_audio_order = AV_CHANNEL_ORDER_NATIVE;
uint64_t g_decoded_audio_layout = AV_CH_LAYOUT_STEREO;
bool g_audio_frame_pending = false;
bool g_audio_packet_owned = false;
bool g_audio_packet_padding_zero = false;
std::atomic<int> g_last_decoder_extradata{-1};

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
	g_av_allocs--;
	free(*avctx);
	*avctx = nullptr;
}

int avcodec_open2(AVCodecContext *, const AVCodec *, AVDictionary **)
{
	return 0;
}

void avcodec_flush_buffers(AVCodecContext *) {}

int avcodec_send_packet(AVCodecContext *ctx, const AVPacket *packet)
{
	g_last_decoder_extradata = ctx->extradata_size > 0 ? ctx->extradata[0] : -1;
	if (ctx->codec_id == AV_CODEC_ID_AAC || ctx->codec_id == AV_CODEC_ID_OPUS) {
		g_audio_packet_owned = packet->buf != nullptr;
		g_audio_packet_padding_zero = true;
		for (int i = 0; i < AV_INPUT_BUFFER_PADDING_SIZE; i++)
			g_audio_packet_padding_zero &= packet->data[packet->size + i] == 0;
		if (g_audio_send_result < 0)
			return g_audio_send_result;
		g_audio_frame_pending = true;
	}
	return g_send_result;
}

int avcodec_receive_frame(AVCodecContext *ctx, AVFrame *frame)
{
	if (ctx->codec_id == AV_CODEC_ID_AAC || ctx->codec_id == AV_CODEC_ID_OPUS) {
		if (g_audio_receive_result < 0)
			return g_audio_receive_result;
		if (!g_audio_frame_pending)
			return AVERROR(EAGAIN);
		g_audio_frame_pending = false;
		frame->format = g_decoded_audio_format;
		frame->sample_rate = ctx->sample_rate;
		frame->nb_samples = g_decoded_audio_samples;
		frame->pts = g_decoded_audio_pts;
		frame->ch_layout.order = g_decoded_audio_order;
		frame->ch_layout.nb_channels = g_decoded_audio_channels;
		frame->ch_layout.u.mask = g_decoded_audio_layout;
		for (int i = 0; i < frame->ch_layout.nb_channels; i++)
			frame->data[i] = g_fake_plane;
		return 0;
	}
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

int av_new_packet(AVPacket *pkt, int size)
{
	if (!pkt || size < 0)
		return AVERROR(EINVAL);
	pkt->data = static_cast<uint8_t *>(calloc(static_cast<size_t>(size) + AV_INPUT_BUFFER_PADDING_SIZE, 1));
	if (!pkt->data)
		return AVERROR(ENOMEM);
	g_av_allocs++;
	pkt->size = size;
	pkt->buf = reinterpret_cast<AVBufferRef *>(pkt->data);
	return 0;
}

void av_packet_free(AVPacket **pkt)
{
	if (!pkt || !*pkt)
		return;
	if ((*pkt)->buf) {
		g_av_allocs--;
		free((*pkt)->data);
	}
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

void av_frame_unref(AVFrame *)
{
	g_audio_frame_unrefs++;
}

void av_channel_layout_default(AVChannelLayout *ch_layout, int nb_channels)
{
	if (!ch_layout)
		return;
	*ch_layout = AVChannelLayout{};
	ch_layout->order = AV_CHANNEL_ORDER_NATIVE;
	ch_layout->nb_channels = nb_channels;
}

int av_sample_fmt_is_planar(enum AVSampleFormat sample_fmt)
{
	return sample_fmt == AV_SAMPLE_FMT_U8P || sample_fmt == AV_SAMPLE_FMT_S16P ||
			       sample_fmt == AV_SAMPLE_FMT_S32P || sample_fmt == AV_SAMPLE_FMT_FLTP ||
			       sample_fmt == AV_SAMPLE_FMT_DBLP
		       ? 1
		       : 0;
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
	g_output_frames = 0;
	g_output_audio = 0;
	g_last_timestamp = 0;
	g_last_audio_timestamp = 0;
	g_audio_frame_unrefs = 0;
	g_blank_calls = 0;
	g_sws_scales = 0;
	g_find_decoder_ok = true;
	g_send_result = 0;
	g_receive_result = 0;
	g_audio_send_result = 0;
	g_audio_receive_result = 0;
	g_decoded_width = 320;
	g_decoded_height = 240;
	g_audio_frame_pending = false;
	g_audio_packet_owned = false;
	g_audio_packet_padding_zero = false;
	g_last_decoder_extradata = -1;
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
	Publisher(TestRelay &relay, const std::string &path, bool video = true, bool audio = true)
	{
		broadcast = TestOk(relay.origin->create_broadcast(path), "create_broadcast");
		if (video) {
			moq::VideoInit init{};
			init.format = moq::VideoFormat::kAvc3;
			init.data = TestH264Init();
			this->video = TestOk(broadcast->publish_video(init), "publish_video");
		}
		if (audio) {
			moq::AudioInit init{};
			init.format = moq::AudioFormat::kOpus;
			init.data = TestOpusHead();
			this->audio = TestOk(broadcast->publish_audio(init), "publish_audio");
		}
		TestOk(broadcast->announce(moq::Route{}), "announce");
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
	std::shared_ptr<moq::MediaProducer> video;
	std::shared_ptr<moq::MediaProducer> audio;

private:
	uint64_t timestamp_us = 1'000'000;
};

// A URL moq-ffi rejects as soon as the connect runs, for a failure with no network in it.
const char *const BAD_URL = "not a url";
} // namespace

int main()
{
	register_moq_source();
	CHECK(g_registered);

	// Connect, wait for the announcement, read the catalog, and decode video and
	// audio from the tracks it names. Audio is copied into a padded packet FFmpeg
	// owns, and carries the broadcast's timestamps into OBS.
	{
		TestRelay relay;
		reset(relay.Url());
		Publisher publisher(relay, "obs/test");
		void *source = createSource();
		CHECK(publisher.WriteUntil([] { return g_output_frames > 0 && g_output_audio > 0; }));
		CHECK(g_sws_scales > 0);
		CHECK(g_last_timestamp > 0);
		CHECK(g_last_audio_timestamp > 0);
		CHECK(g_audio_packet_owned);
		CHECK(g_audio_packet_padding_zero);
		destroySource(source);
	}
	report("plays video and audio");

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

	// A catalog naming a codec FFmpeg can't open leaves the video blank, not half
	// installed, and the audio still plays.
	{
		TestRelay relay;
		reset(relay.Url());
		Publisher publisher(relay, "obs/test");
		g_find_decoder_ok = false;
		void *source = createSource();
		CHECK(!publisher.WriteUntil([] { return g_output_frames > 0 || g_output_audio > 0; },
					    std::chrono::milliseconds(500)));
		CHECK(g_output_frames == 0);
		destroySource(source);
	}
	report("undecodable rendition");

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
