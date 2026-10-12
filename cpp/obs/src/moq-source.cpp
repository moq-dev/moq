// SPDX-License-Identifier: GPL-2.0-or-later
#include <obs-module.h>
#include <util/threading.h>
#include <util/platform.h>
#include <util/darray.h>
#include <util/dstr.h>

#include <moq/moq.hpp>

#include <algorithm>
#include <climits>
#include <cstring>
#include <functional>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <unordered_map>
#include <utility>
#include <variant>
#include <vector>

#ifdef _WIN32
#define strncasecmp _strnicmp
#endif
extern "C" {
#include <libavcodec/avcodec.h>
#include <libavutil/imgutils.h>
#include <libavutil/pixdesc.h>
#include <libswscale/swscale.h>
#include <libavutil/channel_layout.h>
#include <libavutil/samplefmt.h>
}

#include "moq-source.h"
#include "moq-url.h"
#include "moq-worker.h"
#include "logger.h"

// Map a catalog video codec string to an FFmpeg codec ID
static AVCodecID codec_string_to_id(const char *codec, size_t len)
{
	if (!codec || len == 0) {
		return AV_CODEC_ID_NONE;
	}

	// H.264/AVC
	if ((len >= 4 && strncasecmp(codec, "h264", 4) == 0) || (len >= 3 && strncasecmp(codec, "avc", 3) == 0)) {
		return AV_CODEC_ID_H264;
	}

	// HEVC/H.265
	if ((len >= 4 && strncasecmp(codec, "hevc", 4) == 0) || (len >= 4 && strncasecmp(codec, "h265", 4) == 0) ||
	    (len >= 4 && strncasecmp(codec, "hev1", 4) == 0) || (len >= 4 && strncasecmp(codec, "hvc1", 4) == 0)) {
		return AV_CODEC_ID_HEVC;
	}

	// VP9
	if ((len >= 3 && strncasecmp(codec, "vp9", 3) == 0) || (len >= 4 && strncasecmp(codec, "vp09", 4) == 0)) {
		return AV_CODEC_ID_VP9;
	}

	// AV1
	if ((len >= 3 && strncasecmp(codec, "av1", 3) == 0) || (len >= 4 && strncasecmp(codec, "av01", 4) == 0)) {
		return AV_CODEC_ID_AV1;
	}

	// VP8
	if (len >= 3 && strncasecmp(codec, "vp8", 3) == 0) {
		return AV_CODEC_ID_VP8;
	}

	return AV_CODEC_ID_NONE;
}

// Map a catalog audio codec string to an FFmpeg codec ID. Catalog codec strings follow the
// WebCodecs registry: "mp4a.40.2" (AAC-LC), "mp4a.40.5"/"mp4a.40.29" (HE-AAC v1/v2), "opus".
static AVCodecID audio_codec_string_to_id(const char *codec, size_t len)
{
	if (!codec || len == 0)
		return AV_CODEC_ID_NONE;
	if ((len == 3 && strncasecmp(codec, "aac", 3) == 0) ||
	    (len == 9 && (strncasecmp(codec, "mp4a.40.2", 9) == 0 || strncasecmp(codec, "mp4a.40.5", 9) == 0)) ||
	    (len == 10 && strncasecmp(codec, "mp4a.40.29", 10) == 0))
		return AV_CODEC_ID_AAC;
	if (len == 4 && strncasecmp(codec, "opus", 4) == 0)
		return AV_CODEC_ID_OPUS;
	return AV_CODEC_ID_NONE;
}

// Map an FFmpeg sample format to the OBS audio format OBS ingests directly (obs_source_output_audio
// converts to the mixer format itself).
static enum audio_format av_sample_fmt_to_obs(enum AVSampleFormat fmt)
{
	switch (fmt) {
	case AV_SAMPLE_FMT_U8:
		return AUDIO_FORMAT_U8BIT;
	case AV_SAMPLE_FMT_S16:
		return AUDIO_FORMAT_16BIT;
	case AV_SAMPLE_FMT_S32:
		return AUDIO_FORMAT_32BIT;
	case AV_SAMPLE_FMT_FLT:
		return AUDIO_FORMAT_FLOAT;
	case AV_SAMPLE_FMT_U8P:
		return AUDIO_FORMAT_U8BIT_PLANAR;
	case AV_SAMPLE_FMT_S16P:
		return AUDIO_FORMAT_16BIT_PLANAR;
	case AV_SAMPLE_FMT_S32P:
		return AUDIO_FORMAT_32BIT_PLANAR;
	case AV_SAMPLE_FMT_FLTP:
		return AUDIO_FORMAT_FLOAT_PLANAR;
	default:
		return AUDIO_FORMAT_UNKNOWN;
	}
}

static enum speaker_layout audio_layout_to_speakers(const AVChannelLayout *layout)
{
	if (layout->order != AV_CHANNEL_ORDER_NATIVE)
		return SPEAKERS_UNKNOWN;

	switch (layout->u.mask) {
	case AV_CH_LAYOUT_MONO:
		return layout->nb_channels == 1 ? SPEAKERS_MONO : SPEAKERS_UNKNOWN;
	case AV_CH_LAYOUT_STEREO:
		return layout->nb_channels == 2 ? SPEAKERS_STEREO : SPEAKERS_UNKNOWN;
	case AV_CH_LAYOUT_2POINT1:
		return layout->nb_channels == 3 ? SPEAKERS_2POINT1 : SPEAKERS_UNKNOWN;
	case AV_CH_LAYOUT_4POINT0:
		return layout->nb_channels == 4 ? SPEAKERS_4POINT0 : SPEAKERS_UNKNOWN;
	case AV_CH_LAYOUT_4POINT1:
		return layout->nb_channels == 5 ? SPEAKERS_4POINT1 : SPEAKERS_UNKNOWN;
	case AV_CH_LAYOUT_5POINT1_BACK:
		return layout->nb_channels == 6 ? SPEAKERS_5POINT1 : SPEAKERS_UNKNOWN;
	case AV_CH_LAYOUT_7POINT1:
		return layout->nb_channels == 8 ? SPEAKERS_7POINT1 : SPEAKERS_UNKNOWN;
	default:
		return SPEAKERS_UNKNOWN;
	}
}

struct moq_source;

namespace {

enum class Decoded { Pending, Ready, Failed };

// A track owns its codec and converted output, including while it is only a candidate.
struct VideoDecoder {
	AVCodecContext *codec_ctx = nullptr;
	AVPixelFormat current_pix_fmt = AV_PIX_FMT_NONE;
	SwsContext *sws_ctx = nullptr;
	bool got_keyframe = false;
	uint32_t frames_waiting_for_keyframe = 0;
	uint32_t consecutive_decode_errors = 0;
	obs_source_frame frame = {};
	uint8_t *frame_buffer = nullptr;

	~VideoDecoder()
	{
		if (sws_ctx)
			sws_freeContext(sws_ctx);
		if (codec_ctx)
			avcodec_free_context(&codec_ctx);
		bfree(frame_buffer);
	}
};

struct AudioDecoder {
	AVCodecContext *codec_ctx = nullptr;
	uint32_t sample_rate = 0;
	uint32_t channels = 0;
	uint64_t frames_output = 0;
	std::vector<AVFrame *> frames;

	void clear()
	{
		for (auto *frame : frames)
			av_frame_free(&frame);
		frames.clear();
	}

	~AudioDecoder()
	{
		clear();
		if (codec_ctx)
			avcodec_free_context(&codec_ctx);
	}
};

// One subscribed track: what it is, the consumer once subscribed, and the pending
// call that resolves, subscribes, or reads its next frame. Dropping it cancels that call.
struct Track {
	Track(const char *kind, std::string name, moq::Container container)
		: kind(kind),
		  name(std::move(name)),
		  container(std::move(container))
	{
	}

	// "Video" or "Audio", for the log.
	const char *kind;
	std::string name;
	// The rendition the decoder was built from.
	std::optional<moq::Video> video_config;
	std::optional<moq::Audio> audio_config;
	std::unique_ptr<VideoDecoder> video_decoder;
	std::unique_ptr<AudioDecoder> audio_decoder;
	moq::Container container;

	std::shared_ptr<moq::MediaContainerConsumer> consumer;
	std::optional<moq::Continuation> call;
};

// Everything one connect owns, from the client to the tracks. A reconnect, a
// settings change, a terminal failure, or destroy drops it whole, which cancels
// every pending call it holds.
struct Connection {
	std::string url;
	std::string broadcast;

	std::shared_ptr<moq::Client> client;
	std::shared_ptr<moq::Session> session;
	std::shared_ptr<moq::AnnouncedBroadcast> announced;
	std::shared_ptr<moq::BroadcastConsumer> consumer;
	std::shared_ptr<moq::MediaCatalogConsumer> catalog;

	// The connect, then the session's status transitions.
	std::optional<moq::Continuation> session_call;
	// Waiting for the broadcast to be announced, then subscribing to its catalog.
	std::optional<moq::Continuation> broadcast_call;
	// The next catalog update.
	std::optional<moq::Continuation> catalog_call;

	std::shared_ptr<Track> video;
	std::shared_ptr<Track> audio;
	std::shared_ptr<Track> pending_video;
	std::shared_ptr<Track> pending_audio;
};

} // namespace

struct moq_source {
	obs_source_t *source = nullptr;

	// Guards everything below. Every continuation takes it, so it also serializes
	// them against update and destroy on the OBS threads.
	std::mutex mutex;

	// Settings - current active connection settings
	std::string url;
	std::string broadcast;

	// The current connection, or null while disconnected. A continuation for any
	// other connection is stale and returns without touching anything.
	std::shared_ptr<Connection> connection;

	// Runs every continuation; destroy stops it before freeing the source.
	MoQWorker worker;
};

// Forward declarations
static void moq_source_update(void *data, obs_data_t *settings);
static void moq_source_destroy(void *data);
static obs_properties_t *moq_source_properties(void *data);
static void moq_source_get_defaults(obs_data_t *settings);

// Helper functions
static void moq_source_reconnect(struct moq_source *ctx);
static void moq_source_disconnect_locked(struct moq_source *ctx);
static void moq_source_blank_video(struct moq_source *ctx);
static void moq_source_on_connect(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
				  moq::expected<std::shared_ptr<moq::Session>> result);
static void moq_source_on_broadcast(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
				    moq::expected<std::shared_ptr<moq::BroadcastConsumer>> result);
static void moq_source_on_catalogs(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
				   moq::expected<std::shared_ptr<moq::MediaCatalogConsumer>> result);
static void moq_source_watch_status(struct moq_source *ctx, const std::shared_ptr<Connection> &conn);
static void moq_source_on_status(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
				 moq::expected<moq::ConnectionStatus> result);
static void moq_source_on_catalog(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
				  moq::expected<std::optional<moq::Catalog>> result);
static void moq_source_next_catalog(struct moq_source *ctx, const std::shared_ptr<Connection> &conn);
static std::unique_ptr<VideoDecoder> moq_source_prepare_decoder(const moq::Video &config);
static void moq_source_clear_video_locked(struct moq_source *ctx);
static void moq_source_subscribe_video(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
				       const moq::Catalog &catalog);
static std::unique_ptr<AudioDecoder> moq_source_prepare_audio_decoder(const moq::Audio &config);
static void moq_source_clear_audio_locked(struct moq_source *ctx);
static Decoded moq_source_decode_audio_frame(AudioDecoder &decoder, const moq::MediaFrame &frame_data);
static void moq_source_output_audio(struct moq_source *ctx, AudioDecoder &decoder);
static void moq_source_subscribe_audio(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
				       const moq::Catalog &catalog);
static Decoded moq_source_decode_frame(VideoDecoder &decoder, const moq::MediaFrame &frame_data);

// Wraps a continuation so it runs under ctx->mutex, and only while `conn` is still
// the source's connection. Dropping a connection cancels its calls, but a result
// that already completed may be queued on the worker by then.
template<typename Output, typename Callback>
static std::function<void(Output)> moq_source_current(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
						      Callback callback)
{
	return [ctx, weak = std::weak_ptr<Connection>(conn), callback = std::move(callback)](Output output) {
		std::lock_guard<std::mutex> lock(ctx->mutex);
		auto current = weak.lock();
		if (!current || current != ctx->connection)
			return;
		callback(ctx, current, std::move(output));
	};
}

static void *moq_source_create(obs_data_t *settings, obs_source_t *source)
{
	auto *ctx = new moq_source();
	ctx->source = source;

	// Dimensions will be set dynamically from the stream.

	// Load settings from OBS - this will auto-connect if settings are valid
	// (moq_source_update detects settings changed from empty and reconnects)
	moq_source_update(ctx, settings);

	return ctx;
}

static void moq_source_destroy(void *data)
{
	auto *ctx = static_cast<struct moq_source *>(data);

	{
		// Dropping the connection cancels every call it had pending.
		std::lock_guard<std::mutex> lock(ctx->mutex);
		moq_source_disconnect_locked(ctx);
	}

	// Drops whatever is still queued and waits out a continuation in flight, which
	// may be parked on ctx->mutex; so this must not hold it. Nothing touches ctx
	// once it returns.
	ctx->worker.Stop();

	delete ctx;
}

// Relay URLs can embed credentials (userinfo) or a query/path token; MoQRedactUrl
// strips those for logging (see moq-url.h).

static void moq_source_update(void *data, obs_data_t *settings)
{
	auto *ctx = static_cast<struct moq_source *>(data);

	const char *url_value = obs_data_get_string(settings, "url");
	const char *broadcast_value = obs_data_get_string(settings, "broadcast");
	const std::string url = url_value ? url_value : "";
	const std::string broadcast = broadcast_value ? broadcast_value : "";

	bool settings_changed;
	{
		std::lock_guard<std::mutex> lock(ctx->mutex);
		settings_changed = url != ctx->url || broadcast != ctx->broadcast;
		ctx->url = url;
		ctx->broadcast = broadcast;
	}

	// Check if new settings are valid for connection
	const bool valid = !url.empty() && !broadcast.empty();

	// If settings changed and are valid, reconnect
	if (settings_changed && valid) {
		LOG_INFO("Settings changed, reconnecting (url=%s, broadcast=%s)", MoQRedactUrl(url).c_str(),
			 broadcast.c_str());
		moq_source_reconnect(ctx);
	} else if (settings_changed && !valid) {
		LOG_INFO("Settings changed but invalid - disconnecting");
		{
			std::lock_guard<std::mutex> lock(ctx->mutex);
			moq_source_disconnect_locked(ctx);
		}
		moq_source_blank_video(ctx);
	}
}

static void moq_source_get_defaults(obs_data_t *settings)
{
	obs_data_set_default_string(settings, "url", "http://localhost:4443");
	obs_data_set_default_string(settings, "broadcast", "obs/test");
}

static obs_properties_t *moq_source_properties(void *data)
{
	UNUSED_PARAMETER(data);

	obs_properties_t *props = obs_properties_create();

	obs_properties_add_text(props, "url", "URL", OBS_TEXT_DEFAULT);
	obs_properties_add_text(props, "broadcast", "Broadcast", OBS_TEXT_DEFAULT);

	return props;
}

// Tear down the connection after a failure to reach playable media, leaving the
// source disconnected so the next update/reconnect starts clean.
//
// NOTE: Caller must hold ctx->mutex.
static void moq_source_fail_locked(struct moq_source *ctx)
{
	moq_source_disconnect_locked(ctx);
	moq_source_blank_video(ctx);
}

static void moq_source_reconnect(struct moq_source *ctx)
{
	std::lock_guard<std::mutex> lock(ctx->mutex);
	moq_source_disconnect_locked(ctx);

	// Blank video while reconnecting to avoid showing stale frames
	moq_source_blank_video(ctx);

	auto conn = std::make_shared<Connection>();
	conn->url = ctx->url;
	conn->broadcast = ctx->broadcast;
	// The source dials with the defaults. Neither origin side is wired, so the
	// session's consume side carries the remote's announcements.
	auto client = moq::Client::init(moq::ClientConfig{});
	if (!client) {
		// Already disconnected and blanked above.
		LOG_ERROR("Failed to create MoQ client: %s", client.error().to_string().c_str());
		return;
	}
	conn->client = *client;
	ctx->connection = conn;

	LOG_INFO("Connecting to MoQ server: %s", MoQRedactUrl(conn->url).c_str());

	conn->session_call = conn->client->connect(conn->url).then(
		ctx->worker.Executor(),
		moq_source_current<moq::expected<std::shared_ptr<moq::Session>>>(ctx, conn, moq_source_on_connect));
}

static void moq_source_on_connect(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
				  moq::expected<std::shared_ptr<moq::Session>> result)
{
	if (!result) {
		LOG_ERROR("MoQ session error: %s", result.error().to_string().c_str());
		moq_source_fail_locked(ctx);
		return;
	}

	conn->session = *result;
	LOG_INFO("MoQ session connected (epoch %llu)", (unsigned long long)conn->session->epoch());
	moq_source_watch_status(ctx, conn);

	// Wait for the broadcast to be announced. Announcements arrive over the session
	// after it connects, so resolving against only what is announced *now* would race
	// them and blank the source for a broadcast that is live.
	auto announced = conn->session->consume()->announced_broadcast(conn->broadcast);
	if (!announced) {
		LOG_ERROR("Failed to request broadcast '%s': %s", conn->broadcast.c_str(),
			  announced.error().to_string().c_str());
		moq_source_fail_locked(ctx);
		return;
	}
	conn->announced = *announced;
	LOG_INFO("Requesting broadcast: %s", conn->broadcast.c_str());

	conn->broadcast_call = conn->announced->available().then(
		ctx->worker.Executor(), moq_source_current<moq::expected<std::shared_ptr<moq::BroadcastConsumer>>>(
						ctx, conn, moq_source_on_broadcast));
}

static void moq_source_on_broadcast(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
				    moq::expected<std::shared_ptr<moq::BroadcastConsumer>> result)
{
	if (!result) {
		LOG_ERROR("Failed to resolve broadcast '%s': %s", conn->broadcast.c_str(),
			  result.error().to_string().c_str());
		moq_source_fail_locked(ctx);
		return;
	}

	conn->consumer = *result;
	conn->broadcast_call =
		moq::MediaCatalogConsumer::subscribe(conn->consumer)
			.then(ctx->worker.Executor(),
			      moq_source_current<moq::expected<std::shared_ptr<moq::MediaCatalogConsumer>>>(
				      ctx, conn, moq_source_on_catalogs));
}

static void moq_source_on_catalogs(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
				   moq::expected<std::shared_ptr<moq::MediaCatalogConsumer>> result)
{
	if (!result) {
		LOG_ERROR("Failed to subscribe to catalog: %s", result.error().to_string().c_str());
		moq_source_fail_locked(ctx);
		return;
	}

	conn->catalog = *result;
	LOG_INFO("Consuming broadcast: %s", conn->broadcast.c_str());
	moq_source_next_catalog(ctx, conn);
}

// Follows the session's connect and reconnect transitions until it gives up.
static void moq_source_watch_status(struct moq_source *ctx, const std::shared_ptr<Connection> &conn)
{
	conn->session_call = conn->session->status().then(
		ctx->worker.Executor(),
		moq_source_current<moq::expected<moq::ConnectionStatus>>(ctx, conn, moq_source_on_status));
}

static void moq_source_on_status(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
				 moq::expected<moq::ConnectionStatus> result)
{
	if (!result) {
		// Reconnecting gave up (or the relay refused us). Tear down every
		// subscription, not just the session, and blank to show the error.
		LOG_ERROR("MoQ session error: %s", result.error().to_string().c_str());
		moq_source_fail_locked(ctx);
		return;
	}

	switch (*result) {
	case moq::ConnectionStatus::kConnected:
		// The existing subscriptions ride out the gap; nothing to redo.
		LOG_INFO("MoQ session reconnected (epoch %llu)", (unsigned long long)conn->session->epoch());
		break;
	case moq::ConnectionStatus::kDisconnected:
		LOG_WARNING("MoQ session dropped, reconnecting");
		break;
	case moq::ConnectionStatus::kMigrating:
		LOG_INFO("MoQ session migrating");
		break;
	}
	moq_source_watch_status(ctx, conn);
}

static void moq_source_next_catalog(struct moq_source *ctx, const std::shared_ptr<Connection> &conn)
{
	conn->catalog_call = conn->catalog->next().then(
		ctx->worker.Executor(),
		moq_source_current<moq::expected<std::optional<moq::Catalog>>>(ctx, conn, moq_source_on_catalog));
}

static void moq_source_on_catalog(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
				  moq::expected<std::optional<moq::Catalog>> result)
{
	if (!result) {
		LOG_ERROR("Catalog subscription error: %s", result.error().to_string().c_str());
		moq_source_blank_video(ctx);
		return;
	}
	if (!*result) {
		LOG_DEBUG("Catalog subscription closed cleanly");
		return;
	}

	LOG_INFO("Catalog update received");
	// Audio and video are independent catalog sections. Attempt both from this
	// update, even when either rendition is absent or unsupported.
	moq_source_subscribe_video(ctx, conn, **result);
	moq_source_subscribe_audio(ctx, conn, **result);
	moq_source_next_catalog(ctx, conn);
}

// The rendition a catalog lists first by name, matching the order the catalog keeps.
template<typename Rendition>
static const std::pair<const std::string, Rendition> *
moq_source_first_rendition(const std::unordered_map<std::string, Rendition> &renditions)
{
	const std::pair<const std::string, Rendition> *first = nullptr;
	for (const auto &entry : renditions) {
		if (!first || entry.first < first->first)
			first = &entry;
	}
	return first;
}

static void moq_source_on_subscribed(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
				     const std::shared_ptr<Track> &track,
				     moq::expected<std::shared_ptr<moq::MediaContainerConsumer>> result);
static void moq_source_read_track(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
				  const std::shared_ptr<Track> &track);
static void moq_source_on_frame(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
				const std::shared_ptr<Track> &track,
				moq::expected<std::optional<moq::MediaFrame>> result);

// The track, while it is still the connection's current video or audio track.
static std::shared_ptr<Track> moq_source_track(const Connection &conn, const std::weak_ptr<Track> &weak)
{
	auto track = weak.lock();
	return track && (track == conn.video || track == conn.audio || track == conn.pending_video ||
			 track == conn.pending_audio)
		       ? track
		       : nullptr;
}

// Wraps a track's continuation so it runs only while `track` is still current.
template<typename Output>
static std::function<void(Output)>
moq_source_track_current(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
			 const std::shared_ptr<Track> &track,
			 void (*callback)(struct moq_source *, const std::shared_ptr<Connection> &,
					  const std::shared_ptr<Track> &, Output))
{
	return moq_source_current<Output>(ctx, conn,
					  [weak = std::weak_ptr<Track>(track),
					   callback](struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
						     Output output) {
						  if (auto track = moq_source_track(*conn, weak))
							  callback(ctx, conn, track, std::move(output));
					  });
}

// A failed candidate owns no active playback state and can be released immediately.
static void moq_source_discard_candidate(const std::shared_ptr<Connection> &conn, const std::shared_ptr<Track> &track)
{
	if (track == conn->pending_video)
		conn->pending_video.reset();
	if (track == conn->pending_audio)
		conn->pending_audio.reset();
}

static void moq_source_on_resolved(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
				   const std::shared_ptr<Track> &track,
				   moq::expected<std::shared_ptr<moq::BroadcastConsumer>> result)
{
	if (!result) {
		LOG_ERROR("Failed to resolve %s track broadcast: %s", track->kind, result.error().to_string().c_str());
		moq_source_discard_candidate(conn, track);
		return;
	}
	track->call =
		moq::MediaContainerConsumer::subscribe(*result, {track->name, track->container})
			.then(ctx->worker.Executor(),
			      moq_source_track_current<moq::expected<std::shared_ptr<moq::MediaContainerConsumer>>>(
				      ctx, conn, track, moq_source_on_subscribed));
}

static void moq_source_on_subscribed(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
				     const std::shared_ptr<Track> &track,
				     moq::expected<std::shared_ptr<moq::MediaContainerConsumer>> result)
{
	if (!result) {
		LOG_ERROR("Failed to subscribe to %s track: %s", track->kind, result.error().to_string().c_str());
		moq_source_discard_candidate(conn, track);
		return;
	}
	track->consumer = *result;
	LOG_INFO("Subscribed to %s track successfully", track->kind);
	moq_source_read_track(ctx, conn, track);
}

// Reads the track one frame at a time into its decoder.
static void moq_source_read_track(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
				  const std::shared_ptr<Track> &track)
{
	track->call = track->consumer->next().then(
		ctx->worker.Executor(), moq_source_track_current<moq::expected<std::optional<moq::MediaFrame>>>(
						ctx, conn, track, moq_source_on_frame));
}

static void moq_source_on_frame(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
				const std::shared_ptr<Track> &track,
				moq::expected<std::optional<moq::MediaFrame>> result)
{
	if (!result) {
		LOG_ERROR("%s track error: %s", track->kind, result.error().to_string().c_str());
		moq_source_discard_candidate(conn, track);
		return;
	}
	if (!*result) {
		LOG_DEBUG("%s track closed cleanly", track->kind);
		moq_source_discard_candidate(conn, track);
		return;
	}
	const auto decoded = track->video_decoder ? moq_source_decode_frame(*track->video_decoder, **result)
						  : moq_source_decode_audio_frame(*track->audio_decoder, **result);
	if (decoded == Decoded::Failed) {
		if (track == conn->pending_video || track == conn->pending_audio) {
			moq_source_discard_candidate(conn, track);
			return;
		}
		// Stop only the failed audio track, so a pending replacement can still take over.
		if (track == conn->audio) {
			conn->audio.reset();
			return;
		}
	} else if (decoded == Decoded::Ready) {
		// Commit only after decode and conversion succeed, under the same lock as output.
		if (track == conn->pending_video)
			conn->video = std::move(conn->pending_video);
		else if (track == conn->pending_audio)
			conn->audio = std::move(conn->pending_audio);
		if (track->video_decoder)
			obs_source_output_video(ctx->source, &track->video_decoder->frame);
		else
			moq_source_output_audio(ctx, *track->audio_decoder);
	}
	// The frame may have stopped the track (an audio decode failure).
	if (moq_source_track(*conn, track) == track)
		moq_source_read_track(ctx, conn, track);
}

static bool moq_source_same_container(const moq::Container &a, const moq::Container &b)
{
	const auto &x = a.get_variant();
	const auto &y = b.get_variant();
	if (x.index() != y.index())
		return false;
	const auto *cmaf = std::get_if<moq::Container::kCmaf>(&x);
	return !cmaf || cmaf->init == std::get<moq::Container::kCmaf>(y).init;
}

// Whether `track` already plays this rendition. Metadata such as the label or bitrate may
// change; the track, its broadcast and everything its decoder was built from may not.
static bool moq_source_same_video(const Track *track, const std::string &name, const moq::Video &config)
{
	if (!track || track->name != name)
		return false;
	const auto &old = *track->video_config;
	const bool same_coded =
		old.coded.has_value() == config.coded.has_value() &&
		(!old.coded || (old.coded->width == config.coded->width && old.coded->height == config.coded->height));
	return old.broadcast == config.broadcast && old.codec == config.codec &&
	       old.description == config.description && same_coded &&
	       moq_source_same_container(old.container, config.container);
}

static bool moq_source_same_audio(const Track *track, const std::string &name, const moq::Audio &config)
{
	if (!track || track->name != name)
		return false;
	const auto &old = *track->audio_config;
	return old.broadcast == config.broadcast && old.codec == config.codec &&
	       old.description == config.description && old.sample_rate == config.sample_rate &&
	       old.channel_count == config.channel_count && moq_source_same_container(old.container, config.container);
}

// Resolves the rendition's broadcast (it may live in a sibling), subscribes to the
// track, then reads it, each as a call `track` owns, so replacing or dropping the
// track cancels whichever one is pending.
static void moq_source_subscribe_track(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
				       const std::shared_ptr<Track> &track, const std::optional<std::string> &broadcast)
{
	track->call = conn->consumer->resolve(broadcast).then(
		ctx->worker.Executor(),
		moq_source_track_current<moq::expected<std::shared_ptr<moq::BroadcastConsumer>>>(
			ctx, conn, track, moq_source_on_resolved));
}

static void moq_source_subscribe_video(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
				       const moq::Catalog &catalog)
{
	const auto *rendition = moq_source_first_rendition(catalog.video);
	if (!rendition) {
		LOG_INFO("Catalog has no video rendition; audio only");
		moq_source_clear_video_locked(ctx);
		moq_source_blank_video(ctx);
		return;
	}

	if (moq_source_same_video(conn->video.get(), rendition->first, rendition->second)) {
		conn->pending_video.reset();
		return;
	}
	if (moq_source_same_video(conn->pending_video.get(), rendition->first, rendition->second))
		return;
	conn->pending_video.reset();

	// An unusable replacement leaves the current track and decoder playing.
	auto decoder = moq_source_prepare_decoder(rendition->second);
	if (!decoder) {
		LOG_ERROR("Failed to initialize decoder");
		return;
	}

	auto track = std::make_shared<Track>("Video", rendition->first, rendition->second.container);
	track->video_config = rendition->second;
	track->video_decoder = std::move(decoder);
	conn->pending_video = track;
	moq_source_subscribe_track(ctx, conn, track, rendition->second.broadcast);
}

// Subscribe to the first audio rendition and install the matching decoder. A
// broadcast with no audio rendition stays video-only (not an error).
static void moq_source_subscribe_audio(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
				       const moq::Catalog &catalog)
{
	const auto *rendition = moq_source_first_rendition(catalog.audio);
	if (!rendition) {
		LOG_INFO("Catalog has no audio rendition; video only");
		moq_source_clear_audio_locked(ctx);
		return;
	}
	if (moq_source_same_audio(conn->audio.get(), rendition->first, rendition->second)) {
		conn->pending_audio.reset();
		return;
	}
	if (moq_source_same_audio(conn->pending_audio.get(), rendition->first, rendition->second))
		return;
	conn->pending_audio.reset();
	auto decoder = moq_source_prepare_audio_decoder(rendition->second);
	if (!decoder) {
		LOG_ERROR("Failed to initialize replacement audio decoder");
		return;
	}

	LOG_INFO("Subscribing to audio track (%u Hz, %u ch)", decoder->sample_rate, decoder->channels);
	auto track = std::make_shared<Track>("Audio", rendition->first, rendition->second.container);
	track->audio_config = rendition->second;
	track->audio_decoder = std::move(decoder);
	conn->pending_audio = track;
	moq_source_subscribe_track(ctx, conn, track, rendition->second.broadcast);
}

// NOTE: Caller must hold ctx->mutex.
static void moq_source_clear_video_locked(struct moq_source *ctx)
{
	if (ctx->connection) {
		ctx->connection->pending_video.reset();
		ctx->connection->video.reset();
	}
}

// NOTE: Caller must hold ctx->mutex.
static void moq_source_disconnect_locked(struct moq_source *ctx)
{
	moq_source_clear_video_locked(ctx);
	moq_source_clear_audio_locked(ctx);

	// Dropping the connection cancels whatever it was waiting on, including a wait
	// for a broadcast that is never announced. A subscriber has nothing to drain, so
	// the session closes at once.
	if (ctx->connection && ctx->connection->session)
		ctx->connection->session->cancel(0);
	ctx->connection.reset();
}

// Blanks the video preview by outputting a NULL frame
static void moq_source_blank_video(struct moq_source *ctx)
{
	// Passing NULL to obs_source_output_video clears the current frame
	obs_source_output_video(ctx->source, NULL);
	LOG_DEBUG("Video preview blanked");
}

static std::unique_ptr<VideoDecoder> moq_source_prepare_decoder(const moq::Video &config)
{
	// Map codec string to FFmpeg codec ID dynamically
	AVCodecID codec_id = codec_string_to_id(config.codec.data(), config.codec.size());
	if (codec_id == AV_CODEC_ID_NONE) {
		LOG_ERROR("Unknown or unsupported codec: '%s'", config.codec.c_str());
		return nullptr;
	}

	// Find decoder for the codec
	const AVCodec *codec = avcodec_find_decoder(codec_id);
	if (!codec) {
		LOG_ERROR("Decoder not found for codec ID: %d", codec_id);
		return nullptr;
	}

	auto decoder = std::make_unique<VideoDecoder>();
	decoder->codec_ctx = avcodec_alloc_context3(codec);
	if (!decoder->codec_ctx) {
		LOG_ERROR("Failed to allocate codec context");
		return nullptr;
	}

	// Get dimensions from config - required for buffer allocation. Absent means the
	// catalog didn't declare it; the codec context keeps its own.
	if (config.coded && config.coded->width > 0) {
		decoder->codec_ctx->width = (int)config.coded->width;
	}
	if (config.coded && config.coded->height > 0) {
		decoder->codec_ctx->height = (int)config.coded->height;
	}

	// Use codec description as extradata (contains SPS/PPS for H.264, VPS/SPS/PPS for HEVC, etc.)
	if (config.description && !config.description->empty()) {
		const auto &description = *config.description;
		decoder->codec_ctx->extradata =
			(uint8_t *)av_mallocz(description.size() + AV_INPUT_BUFFER_PADDING_SIZE);
		if (decoder->codec_ctx->extradata) {
			memcpy(decoder->codec_ctx->extradata, description.data(), description.size());
			decoder->codec_ctx->extradata_size = static_cast<int>(description.size());
		}
	}

	// Open codec
	if (avcodec_open2(decoder->codec_ctx, codec, NULL) < 0) {
		LOG_ERROR("Failed to open codec");
		return nullptr;
	}

	return decoder;
}

// NOTE: Caller must hold ctx->mutex when calling this function.
static Decoded moq_source_decode_frame(VideoDecoder &decoder, const moq::MediaFrame &frame_data)
{
	// Conversion buffers are allocated when the decoder first produces a frame.
	if (!decoder.codec_ctx)
		return Decoded::Failed;

	// Skip non-keyframes until we get the first one
	if (!decoder.got_keyframe && !frame_data.keyframe) {
		decoder.frames_waiting_for_keyframe++;
		if (decoder.frames_waiting_for_keyframe == 1 || (decoder.frames_waiting_for_keyframe % 30) == 0) {
			LOG_INFO("Waiting for keyframe... (skipped %u frames so far)",
				 decoder.frames_waiting_for_keyframe);
		}
		return Decoded::Pending;
	}

	// Mark that we've received a keyframe from the stream
	if (frame_data.keyframe) {
		if (!decoder.got_keyframe) {
			LOG_INFO("Got keyframe after waiting for %u frames, payload_size=%zu",
				 decoder.frames_waiting_for_keyframe, frame_data.payload.size());
			// Flush decoder to ensure clean state when starting from keyframe
			avcodec_flush_buffers(decoder.codec_ctx);
		}
		decoder.got_keyframe = true;
		decoder.frames_waiting_for_keyframe = 0;
		decoder.consecutive_decode_errors = 0;
	}

	// Create AVPacket from frame data
	AVPacket *packet = av_packet_alloc();
	if (!packet) {
		return Decoded::Failed;
	}

	packet->data = const_cast<uint8_t *>(frame_data.payload.data());
	packet->size = static_cast<int>(frame_data.payload.size());
	packet->pts = frame_data.timestamp_us / 1000; // Convert to milliseconds
	packet->dts = packet->pts;

	// Send packet to decoder
	int ret = avcodec_send_packet(decoder.codec_ctx, packet);
	av_packet_free(&packet);

	if (ret < 0) {
		if (ret != AVERROR(EAGAIN)) {
			decoder.consecutive_decode_errors++;
			char errbuf[AV_ERROR_MAX_STRING_SIZE];
			av_strerror(ret, errbuf, sizeof(errbuf));

			// If too many consecutive errors, flush decoder and wait for next keyframe
			if (decoder.consecutive_decode_errors >= 5) {
				LOG_WARNING("Too many send errors (%u), flushing decoder and waiting for keyframe",
					    decoder.consecutive_decode_errors);
				avcodec_flush_buffers(decoder.codec_ctx);
				decoder.got_keyframe = false;
				decoder.consecutive_decode_errors = 0;
			} else if (decoder.consecutive_decode_errors == 1) {
				LOG_ERROR("Error sending packet to decoder: %s", errbuf);
			}
		}
		return ret == AVERROR(EAGAIN) ? Decoded::Pending : Decoded::Failed;
	}

	// Receive decoded frames
	AVFrame *frame = av_frame_alloc();
	if (!frame) {
		return Decoded::Failed;
	}

	ret = avcodec_receive_frame(decoder.codec_ctx, frame);
	if (ret < 0) {
		if (ret != AVERROR(EAGAIN)) {
			decoder.consecutive_decode_errors++;
			char errbuf[AV_ERROR_MAX_STRING_SIZE];
			av_strerror(ret, errbuf, sizeof(errbuf));

			// If too many consecutive errors, flush decoder and wait for next keyframe
			if (decoder.consecutive_decode_errors >= 5) {
				LOG_WARNING("Too many decode errors (%u), flushing decoder and waiting for keyframe",
					    decoder.consecutive_decode_errors);
				avcodec_flush_buffers(decoder.codec_ctx);
				decoder.got_keyframe = false;
				decoder.consecutive_decode_errors = 0;
			} else if (decoder.consecutive_decode_errors == 1) {
				// Only log first error in a sequence
				LOG_ERROR("Error receiving frame from decoder: %s", errbuf);
			}
		}
		av_frame_free(&frame);
		return ret == AVERROR(EAGAIN) ? Decoded::Pending : Decoded::Failed;
	}

	// Successfully decoded a frame - reset error counter
	decoder.consecutive_decode_errors = 0;

	// Check if we need to (re)initialize the scaler - either first frame, dimension change, or pixel format change
	enum AVPixelFormat decoded_pix_fmt = (enum AVPixelFormat)frame->format;
	bool dimensions_changed =
		(frame->width != (int)decoder.frame.width || frame->height != (int)decoder.frame.height);
	bool pix_fmt_changed = (decoded_pix_fmt != decoder.current_pix_fmt);
	bool need_reinit = (!decoder.sws_ctx || !decoder.frame_buffer || dimensions_changed || pix_fmt_changed);

	if (need_reinit) {
		if (dimensions_changed) {
			LOG_INFO("Decoded frame dimensions changed: %ux%u -> %dx%d", decoder.frame.width,
				 decoder.frame.height, frame->width, frame->height);
		}
		if (pix_fmt_changed) {
			LOG_INFO("Decoded frame pixel format changed: %d -> %d (%s)", decoder.current_pix_fmt,
				 decoded_pix_fmt,
				 av_get_pix_fmt_name(decoded_pix_fmt) ? av_get_pix_fmt_name(decoded_pix_fmt)
								      : "unknown");
		}

		// Validate that dimensions are positive and reasonable
		if (frame->width <= 0 || frame->height <= 0 || frame->width > 16384 || frame->height > 16384) {
			LOG_ERROR("Invalid decoded frame dimensions: %dx%d", frame->width, frame->height);
			av_frame_free(&frame);
			return Decoded::Failed;
		}

		// Validate pixel format is supported by swscale
		if (decoded_pix_fmt == AV_PIX_FMT_NONE) {
			LOG_ERROR("Invalid decoded frame pixel format: %d", decoded_pix_fmt);
			av_frame_free(&frame);
			return Decoded::Failed;
		}

		// Free old sws context
		if (decoder.sws_ctx) {
			sws_freeContext(decoder.sws_ctx);
			decoder.sws_ctx = NULL;
		}

		// Create new scaling context with the actual pixel format from the decoded frame
		struct SwsContext *new_sws_ctx = sws_getContext(frame->width, frame->height, decoded_pix_fmt,
								frame->width, frame->height, AV_PIX_FMT_RGBA,
								SWS_BILINEAR, NULL, NULL, NULL);
		if (!new_sws_ctx) {
			LOG_ERROR("Failed to create scaling context for %dx%d pix_fmt=%d (%s)", frame->width,
				  frame->height, decoded_pix_fmt,
				  av_get_pix_fmt_name(decoded_pix_fmt) ? av_get_pix_fmt_name(decoded_pix_fmt)
								       : "unknown");
			av_frame_free(&frame);
			return Decoded::Failed;
		}

		// Reallocate frame buffer for new dimensions (width * height * 4 for RGBA)
		size_t new_buffer_size = (size_t)frame->width * (size_t)frame->height * 4;
		uint8_t *new_frame_buffer = (uint8_t *)bmalloc(new_buffer_size);
		if (!new_frame_buffer) {
			LOG_ERROR("Failed to allocate frame buffer for %dx%d (%zu bytes)", frame->width, frame->height,
				  new_buffer_size);
			sws_freeContext(new_sws_ctx);
			av_frame_free(&frame);
			return Decoded::Failed;
		}

		// Free old frame buffer
		if (decoder.frame_buffer) {
			bfree(decoder.frame_buffer);
		}

		// Install new state
		decoder.sws_ctx = new_sws_ctx;
		decoder.current_pix_fmt = decoded_pix_fmt;
		decoder.frame_buffer = new_frame_buffer;
		decoder.frame.width = frame->width;
		decoder.frame.height = frame->height;
		decoder.frame.linesize[0] = frame->width * 4;
		decoder.frame.data[0] = new_frame_buffer;

		LOG_INFO("Scaler initialized for %dx%d pix_fmt=%s", frame->width, frame->height,
			 av_get_pix_fmt_name(decoded_pix_fmt) ? av_get_pix_fmt_name(decoded_pix_fmt) : "unknown");
	}

	// Convert YUV420P to RGBA
	uint8_t *dst_data[4] = {decoder.frame_buffer, NULL, NULL, NULL};
	int dst_linesize[4] = {static_cast<int>(decoder.frame.width * 4), 0, 0, 0};

	if (sws_scale(decoder.sws_ctx, (const uint8_t *const *)frame->data, frame->linesize, 0, decoder.frame.height,
		      dst_data, dst_linesize) != static_cast<int>(decoder.frame.height)) {
		LOG_ERROR("Failed to convert decoded video frame");
		av_frame_free(&frame);
		return Decoded::Failed;
	}

	// Update OBS frame timestamp and output. OBS expects nanoseconds; frames
	// carry microseconds.
	decoder.frame.timestamp = frame_data.timestamp_us * 1000;
	decoder.frame.format = VIDEO_FORMAT_RGBA;
	av_frame_free(&frame);
	return Decoded::Ready;
}

// ---- Audio -------------------------------------------------------------------
static std::unique_ptr<AudioDecoder> moq_source_prepare_audio_decoder(const moq::Audio &config)
{
	AVCodecID codec_id = audio_codec_string_to_id(config.codec.data(), config.codec.size());
	if (codec_id == AV_CODEC_ID_NONE) {
		LOG_ERROR("Unknown or unsupported audio codec: '%s'", config.codec.c_str());
		return nullptr;
	}
	const AVCodec *codec = avcodec_find_decoder(codec_id);
	if (!codec) {
		LOG_ERROR("Audio decoder not found for codec ID: %d", codec_id);
		return nullptr;
	}
	auto decoder = std::make_unique<AudioDecoder>();
	decoder->codec_ctx = avcodec_alloc_context3(codec);
	if (!decoder->codec_ctx) {
		LOG_ERROR("Failed to allocate audio codec context");
		return nullptr;
	}
	decoder->sample_rate = config.sample_rate;
	decoder->channels = config.channel_count;
	decoder->codec_ctx->sample_rate = static_cast<int>(config.sample_rate);
	av_channel_layout_default(&decoder->codec_ctx->ch_layout, static_cast<int>(config.channel_count));
	decoder->codec_ctx->pkt_timebase = AVRational{1, 1000000}; // frame timestamps are microseconds
	if (config.description && !config.description->empty()) {
		const auto &description = *config.description;
		decoder->codec_ctx->extradata =
			(uint8_t *)av_mallocz(description.size() + AV_INPUT_BUFFER_PADDING_SIZE);
		if (decoder->codec_ctx->extradata) {
			memcpy(decoder->codec_ctx->extradata, description.data(), description.size());
			decoder->codec_ctx->extradata_size = static_cast<int>(description.size());
		}
	}
	if (avcodec_open2(decoder->codec_ctx, codec, NULL) < 0) {
		LOG_ERROR("Failed to open audio codec");
		return nullptr;
	}
	return decoder;
}

// NOTE: caller holds ctx->mutex.
static void moq_source_clear_audio_locked(struct moq_source *ctx)
{
	if (ctx->connection) {
		ctx->connection->pending_audio.reset();
		ctx->connection->audio.reset();
	}
}

// Decode a packet into owned frames before any candidate output can reach OBS.
static Decoded moq_source_decode_audio_frame(AudioDecoder &decoder, const moq::MediaFrame &frame_data)
{
	decoder.clear();
	if (frame_data.payload.size() > INT_MAX) {
		LOG_ERROR("Audio frame is too large to decode: %zu bytes", frame_data.payload.size());
		return Decoded::Failed;
	}
	AVPacket *packet = av_packet_alloc();
	if (!packet || av_new_packet(packet, static_cast<int>(frame_data.payload.size())) < 0) {
		LOG_ERROR("Failed to allocate audio packet");
		av_packet_free(&packet);
		return Decoded::Failed;
	}
	if (!frame_data.payload.empty())
		memcpy(packet->data, frame_data.payload.data(), frame_data.payload.size());
	packet->pts = static_cast<int64_t>(frame_data.timestamp_us);
	packet->dts = packet->pts;
	int ret = avcodec_send_packet(decoder.codec_ctx, packet);
	av_packet_free(&packet);
	if (ret < 0) {
		LOG_ERROR("Failed to send audio packet to decoder: %d", ret);
		return Decoded::Failed;
	}
	for (;;) {
		AVFrame *frame = av_frame_alloc();
		if (!frame)
			return Decoded::Failed;
		ret = avcodec_receive_frame(decoder.codec_ctx, frame);
		if (ret < 0) {
			av_frame_free(&frame);
			if (ret == AVERROR(EAGAIN))
				return decoder.frames.empty() ? Decoded::Pending : Decoded::Ready;
			LOG_ERROR("Failed to receive decoded audio frame: %d", ret);
			return Decoded::Failed;
		}
		const auto format = av_sample_fmt_to_obs(static_cast<AVSampleFormat>(frame->format));
		const auto speakers = audio_layout_to_speakers(&frame->ch_layout);
		if (format == AUDIO_FORMAT_UNKNOWN || speakers == SPEAKERS_UNKNOWN || frame->sample_rate <= 0 ||
		    frame->nb_samples <= 0) {
			LOG_ERROR("Unsupported decoded audio layout: fmt=%d channels=%d rate=%d", frame->format,
				  frame->ch_layout.nb_channels, frame->sample_rate);
			av_frame_free(&frame);
			return Decoded::Failed;
		}
		if (frame->pts == AV_NOPTS_VALUE)
			frame->pts = static_cast<int64_t>(frame_data.timestamp_us);
		decoder.frames.push_back(frame);
	}
}

static void moq_source_output_audio(struct moq_source *ctx, AudioDecoder &decoder)
{
	for (auto *frame : decoder.frames) {
		obs_source_audio audio = {};
		const auto format = static_cast<AVSampleFormat>(frame->format);
		const int channels = frame->ch_layout.nb_channels;
		const int planes = av_sample_fmt_is_planar(format) ? channels : 1;
		for (int i = 0; i < planes && i < MAX_AV_PLANES; i++)
			audio.data[i] = frame->data[i];
		audio.frames = static_cast<uint32_t>(frame->nb_samples);
		audio.speakers = audio_layout_to_speakers(&frame->ch_layout);
		audio.format = av_sample_fmt_to_obs(format);
		audio.samples_per_sec = static_cast<uint32_t>(frame->sample_rate);
		audio.timestamp = static_cast<uint64_t>(frame->pts) * 1000ULL;
		obs_source_output_audio(ctx->source, &audio);
		if (++decoder.frames_output == 1)
			LOG_INFO("First audio frame output: %d samples, %d Hz, %d ch, fmt=%d", frame->nb_samples,
				 frame->sample_rate, channels, frame->format);
	}
	decoder.clear();
}

// Registration function
void register_moq_source()
{
	struct obs_source_info info = {};
	info.id = "moq_source";
	info.type = OBS_SOURCE_TYPE_INPUT;
	info.output_flags = OBS_SOURCE_ASYNC_VIDEO | OBS_SOURCE_AUDIO | OBS_SOURCE_DO_NOT_DUPLICATE;
	info.get_name = [](void *) -> const char * {
		return "Moq Source (MoQ)";
	};
	info.create = moq_source_create;
	info.destroy = moq_source_destroy;
	info.update = moq_source_update;
	info.get_defaults = moq_source_get_defaults;
	info.get_properties = moq_source_properties;

	obs_register_source(&info);
}
