// SPDX-License-Identifier: GPL-2.0-or-later
#include <obs-module.h>
#include <util/threading.h>
#include <util/platform.h>
#include <util/darray.h>
#include <util/dstr.h>

#include <moq/moq.hpp>

#include <algorithm>
#include <cstring>
#include <functional>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <tuple>
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
}

#include "moq-implementation.h"
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

namespace {

struct moq_source;

enum class Decoded { Pending, Ready, Failed };
// Full: the decoder refused the packet until its pending pictures are taken.
enum class Sent { Accepted, Full, Failed };

// A track owns its codec and converted output, including while it is only a candidate.
struct VideoDecoder {
	AVCodecContext *codec_ctx = nullptr;
	AVPixelFormat current_pix_fmt = AV_PIX_FMT_NONE;
	SwsContext *sws_ctx = nullptr;
	bool got_keyframe = false;
	// End can precede the old encoder's final packets, whose timestamps are below it.
	std::optional<uint64_t> end_us;
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

// How a decoded audio rendition reaches OBS: the channel count moq-audio remixes to, the
// OBS layout that places those channels, and the rate moq-audio resamples to.
struct AudioLayout {
	uint32_t channels = 0;
	enum speaker_layout speakers = SPEAKERS_UNKNOWN;
	uint32_t sample_rate = 0;
};

// One subscribed track: what it is, the consumer once subscribed, and the pending
// call that subscribes or reads its next frame. Dropping it cancels that call.
struct Track {
	Track(const char *kind, std::string id) : kind(kind), id(std::move(id)) {}

	// "Video" or "Audio", for the log.
	const char *kind;
	// The rendition's catalog ID.
	std::string id;
	// The publisher instance the track reads, so a newer one resubscribes even an unchanged rendition.
	uint64_t instance = 0;
	// The rendition the decoder was built from.
	std::optional<moq::Video> video_config;
	std::optional<moq::Audio> audio_config;
	std::unique_ptr<VideoDecoder> video_decoder;
	AudioLayout audio_layout;
	uint64_t audio_frames = 0;

	// Video reads encoded frames to decode here; audio reads PCM moq-audio decoded.
	std::shared_ptr<moq::MediaTrackConsumer> consumer;
	std::shared_ptr<moq::AudioConsumer> audio_consumer;
	std::optional<moq::Continuation> call;
};

// Everything one connect owns, from the client to the tracks. A reconnect, a
// settings change, a terminal failure, or destroy drops it whole, which cancels
// every pending call it holds.
struct Connection {
	uint64_t revision = 0;
	std::string url;
	std::string broadcast;

	std::shared_ptr<moq::Client> client;
	std::shared_ptr<moq::Session> session;
	std::shared_ptr<moq::Following> announced;

	// How many publisher instances `announced` has handed out; the newest is the one read.
	uint64_t instance = 0;
	std::shared_ptr<moq::BroadcastConsumer> consumer;
	std::shared_ptr<moq::MediaCatalogConsumer> catalog;

	// The connect, then the session's lifecycle revisions.
	std::optional<moq::Continuation> session_call;
	// Waiting for the next publisher instance.
	std::optional<moq::Continuation> available_call;
	// Subscribing to the instance's catalog, then its next update.
	std::optional<moq::Continuation> catalog_call;

	std::shared_ptr<Track> video;
	std::shared_ptr<Track> audio;
	std::shared_ptr<Track> pending_video;
	std::shared_ptr<Track> pending_audio;
};

struct moq_source {
	obs_source_t *source = nullptr;

	// Guards everything below. Every continuation takes it, so it also serializes
	// them against update and destroy on the OBS threads.
	std::mutex mutex;

	// Settings - current active connection settings
	std::string url;
	std::string broadcast;
	bool keylog = false;

	// The current connection, or null while disconnected. A continuation for any
	// other connection is stale and returns without touching anything.
	std::shared_ptr<Connection> connection;

	// The OBS mixer's rate, which audio is resampled to so OBS need not resample it
	// again. Zero when OBS has no audio output. Set once at create.
	uint32_t sample_rate = 0;

	// Runs every continuation; destroy stops it before freeing the source.
	MoQWorker worker;
};

} // namespace

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
static void moq_source_next_broadcast(struct moq_source *ctx, const std::shared_ptr<Connection> &conn);
static void moq_source_on_broadcast(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
				    moq::expected<std::shared_ptr<moq::BroadcastConsumer>> result);
static void moq_source_on_catalogs(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
				   moq::expected<std::shared_ptr<moq::MediaCatalogConsumer>> result);
static void moq_source_watch_state(struct moq_source *ctx, const std::shared_ptr<Connection> &conn);
static void moq_source_on_state(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
				moq::expected<moq::ConnectionState> result);
static void moq_source_on_catalog(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
				  moq::expected<std::optional<moq::Catalog>> result);
static void moq_source_next_catalog(struct moq_source *ctx, const std::shared_ptr<Connection> &conn);
static std::unique_ptr<VideoDecoder> moq_source_prepare_decoder(const moq::Video &config);
static void moq_source_clear_video_locked(struct moq_source *ctx);
static void moq_source_subscribe_video(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
				       const moq::Catalog &catalog);
static void moq_source_clear_audio_locked(struct moq_source *ctx);
static void moq_source_subscribe_audio(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
				       const moq::Catalog &catalog);
static Sent moq_source_send_packet(VideoDecoder &decoder, const moq::MediaFrame &frame_data);
static Decoded moq_source_receive_frame(VideoDecoder &decoder);

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

// Wraps a continuation so it also runs only while `conn` reads the publisher instance it
// was issued for. Replacing the instance cancels its calls, but a result may be queued.
template<typename Output>
static std::function<void(Output)>
moq_source_instance_current(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
			    void (*callback)(struct moq_source *, const std::shared_ptr<Connection> &, Output))
{
	return moq_source_current<Output>(ctx, conn,
					  [instance = conn->instance, callback](struct moq_source *ctx,
										const std::shared_ptr<Connection> &conn,
										Output output) {
						  if (conn->instance == instance)
							  callback(ctx, conn, std::move(output));
					  });
}

static void *moq_source_create(obs_data_t *settings, obs_source_t *source)
{
	auto *ctx = new moq_source();
	ctx->source = source;
	obs_audio_info audio = {};
	if (obs_get_audio_info(&audio))
		ctx->sample_rate = audio.samples_per_sec;

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
	const bool keylog = obs_data_get_bool(settings, "quic_keylog");

	bool settings_changed;
	{
		std::lock_guard<std::mutex> lock(ctx->mutex);
		settings_changed = url != ctx->url || broadcast != ctx->broadcast || keylog != ctx->keylog;
		ctx->url = url;
		ctx->broadcast = broadcast;
		ctx->keylog = keylog;
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
	obs_data_set_default_bool(settings, "quic_keylog", false);
}

static obs_properties_t *moq_source_properties(void *data)
{
	UNUSED_PARAMETER(data);

	obs_properties_t *props = obs_properties_create();

	obs_properties_add_text(props, "url", "URL", OBS_TEXT_DEFAULT);
	obs_properties_add_text(props, "broadcast", "Broadcast", OBS_TEXT_DEFAULT);
	// Writes TLS session secrets to the file SSLKEYLOGFILE names, to decrypt a packet
	// capture; without SSLKEYLOGFILE the dial fails.
	obs_properties_add_bool(props, "quic_keylog", "TLS key log");

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

// The source dials with the defaults, plus the TLS key log when the user opts in.
static moq::ClientConfig moq_source_client_config(bool keylog)
{
	moq::ClientConfig config;
	config.quic.keylog = keylog;
	config.implementation = MoQImplementation();
	return config;
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
	// Neither origin side is wired, so the session's consume side carries the
	// remote's announcements.
	auto client = moq::Client::init(moq_source_client_config(ctx->keylog));
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
	moq_source_on_state(ctx, conn, conn->session->state());
	if (ctx->connection != conn)
		return;

	// Wait for the broadcast to be announced. Announcements arrive over the session
	// after it connects, so resolving against only what is announced *now* would race
	// them and blank the source for a broadcast that is live.
	auto announced = conn->session->consume()->follow(conn->broadcast, std::nullopt);
	if (!announced) {
		LOG_ERROR("Failed to request broadcast '%s': %s", conn->broadcast.c_str(),
			  announced.error().to_string().c_str());
		moq_source_fail_locked(ctx);
		return;
	}
	conn->announced = *announced;
	LOG_INFO("Requesting broadcast: %s", conn->broadcast.c_str());

	moq_source_next_broadcast(ctx, conn);
}

// Waits for the next publisher instance: the first one announced, then each that takes over.
static void moq_source_next_broadcast(struct moq_source *ctx, const std::shared_ptr<Connection> &conn)
{
	conn->available_call = conn->announced->available().then(
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

	// Another instance took over, or the broadcast came back. The previous one's active tracks
	// keep playing until the new catalog's renditions commit, or until they end once its
	// route dies, holding the last frame.
	if (conn->instance++ > 0)
		LOG_INFO("Following a new publisher instance of broadcast: %s", conn->broadcast.c_str());
	conn->consumer = *result;
	conn->catalog.reset();
	conn->pending_video.reset();
	conn->pending_audio.reset();

	conn->catalog_call =
		moq::MediaCatalogConsumer::subscribe(conn->consumer)
			.then(ctx->worker.Executor(),
			      moq_source_instance_current<moq::expected<std::shared_ptr<moq::MediaCatalogConsumer>>>(
				      ctx, conn, moq_source_on_catalogs));
	moq_source_next_broadcast(ctx, conn);
}

static void moq_source_on_catalogs(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
				   moq::expected<std::shared_ptr<moq::MediaCatalogConsumer>> result)
{
	if (!result) {
		// Stop the instance this one replaced too, and wait for the next one.
		LOG_ERROR("Failed to subscribe to catalog: %s", result.error().to_string().c_str());
		moq_source_clear_video_locked(ctx);
		moq_source_clear_audio_locked(ctx);
		moq_source_blank_video(ctx);
		return;
	}

	conn->catalog = *result;
	LOG_INFO("Consuming broadcast: %s", conn->broadcast.c_str());
	moq_source_next_catalog(ctx, conn);
}

// Follows the session's connect and reconnect transitions until it gives up.
static void moq_source_watch_state(struct moq_source *ctx, const std::shared_ptr<Connection> &conn)
{
	conn->session_call =
		conn->session->state_changed(conn->revision)
			.then(ctx->worker.Executor(),
			      moq_source_current<moq::expected<moq::ConnectionState>>(ctx, conn, moq_source_on_state));
}

static void moq_source_on_state(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
				moq::expected<moq::ConnectionState> result)
{
	if (!result) {
		// Reconnecting gave up (or the relay refused us). Tear down every
		// subscription, not just the session, and blank to show the error.
		LOG_ERROR("MoQ session error: %s", result.error().to_string().c_str());
		moq_source_fail_locked(ctx);
		return;
	}

	conn->revision = result->revision;
	if (result->failure)
		LOG_WARNING("MoQ connection attempt failed: %s", result->failure->to_string().c_str());
	if (result->phase == moq::ConnectionPhase::kClosed) {
		moq_source_fail_locked(ctx);
		return;
	}
	if (result->phase == moq::ConnectionPhase::kConnected)
		LOG_INFO("MoQ session connected (connect %llu)", (unsigned long long)result->connects);

	moq_source_watch_state(ctx, conn);
}

static void moq_source_next_catalog(struct moq_source *ctx, const std::shared_ptr<Connection> &conn)
{
	conn->catalog_call = conn->catalog->next().then(
		ctx->worker.Executor(), moq_source_instance_current<moq::expected<std::optional<moq::Catalog>>>(
						ctx, conn, moq_source_on_catalog));
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

template<typename Rendition> using CatalogEntry = std::pair<const std::string, Rendition>;

// A catalog section's renditions best first, by the key `rank` gives each (highest first),
// with exact ties in name order: the order hang's `ranked()` gives.
template<typename Rendition, typename Rank>
static std::vector<const CatalogEntry<Rendition> *>
moq_source_ranked(const std::unordered_map<std::string, Rendition> &renditions, Rank rank)
{
	std::vector<const CatalogEntry<Rendition> *> ranked;
	ranked.reserve(renditions.size());
	for (const auto &entry : renditions)
		ranked.push_back(&entry);
	std::sort(ranked.begin(), ranked.end(), [&](const auto *a, const auto *b) {
		const auto x = rank(a->second);
		const auto y = rank(b->second);
		return x != y ? x > y : a->first < b->first;
	});
	return ranked;
}

// hang's `Video::ranked`: enabled, then preference, then coded area, then bitrate. An unknown
// size or bitrate ranks below a known one, and the area is 64-bit so a large picture never wraps.
static std::vector<const CatalogEntry<moq::Video> *>
moq_source_ranked_video(const std::unordered_map<std::string, moq::Video> &renditions)
{
	return moq_source_ranked(renditions, [](const moq::Video &config) {
		const uint64_t area = config.coded ? uint64_t{config.coded->width} * config.coded->height : 0;
		return std::make_tuple(config.enabled, config.preference, area, config.bitrate);
	});
}

// hang's `Audio::ranked`: enabled, then bitrate, then sample rate, then channel count.
static std::vector<const CatalogEntry<moq::Audio> *>
moq_source_ranked_audio(const std::unordered_map<std::string, moq::Audio> &renditions)
{
	return moq_source_ranked(renditions, [](const moq::Audio &config) {
		return std::make_tuple(config.enabled, config.bitrate, config.sample_rate, config.channel_count);
	});
}

// The ranked renditions to try in turn: the enabled ones. With none enabled, a consumer
// should not select a rendition but holds the one it has, so only those `held` accepts.
template<typename Rendition, typename Held>
static std::vector<const CatalogEntry<Rendition> *>
moq_source_candidates(std::vector<const CatalogEntry<Rendition> *> ranked, Held held)
{
	const bool any_enabled =
		std::any_of(ranked.begin(), ranked.end(), [](const auto *entry) { return entry->second.enabled; });
	ranked.erase(
		std::remove_if(ranked.begin(), ranked.end(),
			       [&](const auto *entry) { return any_enabled ? !entry->second.enabled : !held(*entry); }),
		ranked.end());
	return ranked;
}

static void moq_source_on_subscribed(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
				     const std::shared_ptr<Track> &track,
				     moq::expected<std::shared_ptr<moq::MediaTrackConsumer>> result);
static void moq_source_read_track(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
				  const std::shared_ptr<Track> &track);
static void moq_source_on_frame(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
				const std::shared_ptr<Track> &track,
				moq::expected<std::optional<moq::MediaRead>> result);

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

static void moq_source_on_subscribed(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
				     const std::shared_ptr<Track> &track,
				     moq::expected<std::shared_ptr<moq::MediaTrackConsumer>> result)
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
		ctx->worker.Executor(), moq_source_track_current<moq::expected<std::optional<moq::MediaRead>>>(
						ctx, conn, track, moq_source_on_frame));
}

// Hands OBS every picture the decoder has ready, in presentation order. False when a
// picture failed to decode or convert.
static bool moq_source_output_video(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
				    const std::shared_ptr<Track> &track)
{
	for (;;) {
		switch (moq_source_receive_frame(*track->video_decoder)) {
		case Decoded::Pending:
			return true;
		case Decoded::Failed:
			return false;
		case Decoded::Ready:
			// Commit only after decode and conversion succeed, under the same lock as output.
			if (track == conn->pending_video)
				conn->video = std::move(conn->pending_video);
			obs_source_output_video(ctx->source, &track->video_decoder->frame);
			break;
		}
	}
}

// Drain pictures from the old encoder before either kind of boundary.
static void moq_source_drain_video(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
				   const std::shared_ptr<Track> &track)
{
	AVCodecContext *codec = track->video_decoder->codec_ctx;
	int ended = avcodec_send_packet(codec, nullptr);
	if (ended == AVERROR(EAGAIN)) {
		moq_source_output_video(ctx, conn, track);
		ended = avcodec_send_packet(codec, nullptr);
	}
	if (ended == 0)
		moq_source_output_video(ctx, conn, track);
}

static void moq_source_on_frame(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
				const std::shared_ptr<Track> &track,
				moq::expected<std::optional<moq::MediaRead>> result)
{
	if (!result) {
		LOG_ERROR("%s track error: %s", track->kind, result.error().to_string().c_str());
		moq_source_discard_candidate(conn, track);
		return;
	}
	if (!*result) {
		LOG_DEBUG("%s track closed cleanly", track->kind);
		// An ended candidate must not replace playback that may still be live.
		if (track == conn->pending_video && conn->video) {
			moq_source_discard_candidate(conn, track);
			return;
		}
		// The decoder holds reordered pictures back until it learns the stream ended. Like a
		// packet, the end is refused while pictures are pending, so take them first.
		moq_source_drain_video(ctx, conn, track);
		moq_source_discard_candidate(conn, track);
		return;
	}
	auto &decoder = *track->video_decoder;
	const auto &variant = (*result)->get_variant();
	if (const auto *end = std::get_if<moq::MediaRead::kEnd>(&variant)) {
		decoder.end_us = end->end_us;
		moq_source_read_track(ctx, conn, track);
		return;
	}
	const auto *read = std::get_if<moq::MediaRead::kFrame>(&variant);
	if (!read || (decoder.end_us && read->frame.timestamp_us >= *decoder.end_us)) {
		// A gap loses references immediately. An end does so only at the fresh encoder,
		// after all old tail packets below its endpoint have entered the decoder.
		moq_source_drain_video(ctx, conn, track);
		avcodec_flush_buffers(decoder.codec_ctx);
		decoder.got_keyframe = false;
		decoder.end_us.reset();
	}
	if (!read) {
		moq_source_read_track(ctx, conn, track);
		return;
	}
	Sent sent = moq_source_send_packet(*track->video_decoder, read->frame);
	bool drained = true;
	if (sent == Sent::Full) {
		// A drain that failed partway left pictures behind; take them so the packet fits.
		drained = moq_source_output_video(ctx, conn, track);
		sent = moq_source_send_packet(*track->video_decoder, read->frame);
	}
	const bool decoded = sent == Sent::Accepted && moq_source_output_video(ctx, conn, track) && drained;
	if (!decoded && track == conn->pending_video) {
		moq_source_discard_candidate(conn, track);
		return;
	}
	moq_source_read_track(ctx, conn, track);
}

// Stops a failed audio track, leaving a pending replacement to take over.
static void moq_source_stop_audio(const std::shared_ptr<Connection> &conn, const std::shared_ptr<Track> &track)
{
	moq_source_discard_candidate(conn, track);
	if (track == conn->audio)
		conn->audio.reset();
}

static void moq_source_read_audio(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
				  const std::shared_ptr<Track> &track);

// Takes one decoded read under the source lock. A candidate's first frame commits it, and
// any failure stops the track. On a frame `audio` points into it, for output once the lock
// is released. A gap or encoder end needs nothing here: OBS buffers by timestamp, so the
// next frame resumes wherever its timestamp lands.
static bool moq_source_on_audio_read(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
				     const std::shared_ptr<Track> &track,
				     const moq::expected<std::optional<moq::AudioRead>> &result,
				     obs_source_audio &audio)
{
	if (!result) {
		LOG_ERROR("Audio track error: %s", result.error().to_string().c_str());
		moq_source_stop_audio(conn, track);
		return false;
	}
	if (!*result) {
		LOG_DEBUG("Audio track closed cleanly");
		moq_source_discard_candidate(conn, track);
		return false;
	}

	const auto *read = std::get_if<moq::AudioRead::kFrame>(&(*result)->get_variant());
	if (!read) {
		moq_source_read_audio(ctx, conn, track);
		return false;
	}

	// moq-audio delivers whole planar float samples in the layout the track asked for.
	const auto &frame = read->frame;
	const auto &layout = track->audio_layout;
	const size_t plane = frame.data.size() / layout.channels;
	if (plane == 0 || plane % sizeof(float) != 0 || plane * layout.channels != frame.data.size()) {
		LOG_ERROR("Decoded audio frame of %zu bytes is not whole %u-channel samples", frame.data.size(),
			  layout.channels);
		moq_source_stop_audio(conn, track);
		return false;
	}
	for (uint32_t i = 0; i < layout.channels; i++)
		audio.data[i] = frame.data.data() + i * plane;
	audio.frames = static_cast<uint32_t>(plane / sizeof(float));
	audio.speakers = layout.speakers;
	audio.format = AUDIO_FORMAT_FLOAT_PLANAR;
	audio.samples_per_sec = layout.sample_rate;
	// The same broadcast timeline video stamps, in the nanoseconds OBS expects.
	audio.timestamp = frame.timestamp_us * 1000;

	if (track == conn->pending_audio)
		conn->audio = std::move(conn->pending_audio);
	if (++track->audio_frames == 1)
		LOG_INFO("First audio frame output: %u samples, %u Hz, %u ch", audio.frames, layout.sample_rate,
			 layout.channels);
	moq_source_read_audio(ctx, conn, track);
	return true;
}

// Reads the audio track one decoded frame at a time. Unlike the other continuations, this
// one hands the frame to OBS after releasing the source lock, so OBS never runs under it.
static void moq_source_read_audio(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
				  const std::shared_ptr<Track> &track)
{
	track->call = track->audio_consumer->next().then(
		ctx->worker.Executor(),
		[ctx, weak_conn = std::weak_ptr<Connection>(conn),
		 weak_track = std::weak_ptr<Track>(track)](moq::expected<std::optional<moq::AudioRead>> result) {
			obs_source_audio audio = {};
			{
				std::lock_guard<std::mutex> lock(ctx->mutex);
				auto conn = weak_conn.lock();
				if (!conn || conn != ctx->connection)
					return;
				auto track = moq_source_track(*conn, weak_track);
				if (!track || !moq_source_on_audio_read(ctx, conn, track, result, audio))
					return;
			}
			// `result` owns the samples, and OBS copies them before returning. Destroy
			// waits out this continuation, so ctx is still alive.
			obs_source_output_audio(ctx->source, &audio);
		});
}

static void moq_source_on_audio_subscribed(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
					   const std::shared_ptr<Track> &track,
					   moq::expected<std::shared_ptr<moq::AudioConsumer>> result)
{
	if (!result) {
		LOG_ERROR("Failed to subscribe to Audio track: %s", result.error().to_string().c_str());
		moq_source_discard_candidate(conn, track);
		return;
	}
	track->audio_consumer = *result;
	LOG_INFO("Subscribed to Audio track successfully");
	moq_source_read_audio(ctx, conn, track);
}

// Whether moq-audio decodes the rendition: Opus, PCM, or mono or stereo AAC-LC (the one AAC
// profile and the layouts it has). moq-ffi reports a refusal only once the subscribe fails,
// as an audio error with no kind, so check here to let an unsupported rendition fall through.
// A configuration moq-audio still refuses, such as a malformed OpusHead, discards the
// candidate when it subscribes.
static bool moq_source_audio_decodes(const moq::Audio &config)
{
	const auto &codec = config.codec;
	if (codec == "opus" || codec == "pcm")
		return true;
	// hang reads the AAC profile as a decimal, so `mp4a.40.02` is AAC-LC too.
	const std::string aac = "mp4a.40.";
	if (codec.rfind(aac, 0) != 0)
		return false;
	const auto profile = codec.find_first_not_of('0', aac.size());
	return profile != std::string::npos && codec.compare(profile, std::string::npos, "2") == 0 &&
	       (config.channel_count == 1 || config.channel_count == 2);
}

// The channels moq-audio should remix a rendition to, and where OBS puts them. A count
// names moq-audio's default layout by the WAVE convention: 3 is 2.1, 4 quad, 5 5.0, 6 5.1,
// 7 6.1, 8 7.1. OBS has no quad, 5.0, or 6.1 (its 4.0 and 4.1 put a center where quad has
// a back pair), so those are upmixed to the smallest OBS layout holding every speaker.
// Any other count has no speaker positions to place.
static std::optional<AudioLayout> moq_source_audio_layout(uint32_t channels)
{
	switch (channels) {
	case 1:
		return AudioLayout{1, SPEAKERS_MONO};
	case 2:
		return AudioLayout{2, SPEAKERS_STEREO};
	case 3:
		return AudioLayout{3, SPEAKERS_2POINT1};
	case 4:
	case 5:
	case 6:
		return AudioLayout{6, SPEAKERS_5POINT1};
	case 7:
	case 8:
		return AudioLayout{8, SPEAKERS_7POINT1};
	default:
		return std::nullopt;
	}
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
static bool moq_source_same_video(const Track *track, const std::string &id, const moq::Video &config)
{
	if (!track)
		return false;
	const auto &old = *track->video_config;
	// The same track under a new catalog ID is the same rendition.
	if (old.track.value_or(track->id) != config.track.value_or(id))
		return false;
	const bool same_coded =
		old.coded.has_value() == config.coded.has_value() &&
		(!old.coded || (old.coded->width == config.coded->width && old.coded->height == config.coded->height));
	return old.broadcast == config.broadcast && old.epoch == config.epoch && old.codec == config.codec &&
	       old.description == config.description && same_coded &&
	       moq_source_same_container(old.container, config.container);
}

static bool moq_source_same_audio(const Track *track, const std::string &id, const moq::Audio &config)
{
	if (!track)
		return false;
	const auto &old = *track->audio_config;
	// The same track under a new catalog ID is the same rendition.
	if (old.track.value_or(track->id) != config.track.value_or(id))
		return false;
	return old.broadcast == config.broadcast && old.epoch == config.epoch && old.codec == config.codec &&
	       old.description == config.description && old.sample_rate == config.sample_rate &&
	       old.channel_count == config.channel_count && moq_source_same_container(old.container, config.container);
}

// Subscribes to the rendition (following its broadcast and track), then reads it, each
// as a call `track` owns, so replacing or dropping the track cancels whichever one is pending.
static void moq_source_subscribe_track(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
				       const std::shared_ptr<Track> &track, moq::Rendition rendition)
{
	track->call = moq::MediaTrackConsumer::subscribe(conn->consumer, {std::move(rendition)})
			      .then(ctx->worker.Executor(),
				    moq_source_track_current<moq::expected<std::shared_ptr<moq::MediaTrackConsumer>>>(
					    ctx, conn, track, moq_source_on_subscribed));
}

// Subscribe to the best video rendition FFmpeg can decode. The current track keeps
// playing until a replacement delivers, and for good when no rendition can replace it.
static void moq_source_subscribe_video(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
				       const moq::Catalog &catalog)
{
	const auto candidates = moq_source_candidates(moq_source_ranked_video(catalog.video), [&](const auto &entry) {
		return moq_source_same_video(conn->video.get(), entry.first, entry.second) ||
		       moq_source_same_video(conn->pending_video.get(), entry.first, entry.second);
	});
	if (candidates.empty()) {
		LOG_INFO("Catalog has no enabled video rendition; audio only");
		moq_source_clear_video_locked(ctx);
		moq_source_blank_video(ctx);
		return;
	}

	for (const auto *rendition : candidates) {
		const auto &id = rendition->first;
		if (moq_source_same_video(conn->video.get(), id, rendition->second) &&
		    conn->video->instance == conn->instance) {
			conn->pending_video.reset();
			return;
		}
		if (moq_source_same_video(conn->pending_video.get(), id, rendition->second))
			return;

		// An undecodable rendition falls through to the next one.
		auto decoder = moq_source_prepare_decoder(rendition->second);
		if (!decoder) {
			LOG_ERROR("Failed to initialize decoder for video rendition %s", id.c_str());
			continue;
		}

		auto track = std::make_shared<Track>("Video", id);
		track->instance = conn->instance;
		track->video_config = rendition->second;
		track->video_decoder = std::move(decoder);
		conn->pending_video = track;
		moq_source_subscribe_track(ctx, conn, track, moq::Rendition::kVideo{id, rendition->second});
		return;
	}
	conn->pending_video.reset();
}

// Subscribe to the best audio rendition OBS has a speaker layout for, decoded by moq-audio
// to OBS's rate. A broadcast with no audio rendition stays video-only (not an error), and
// the current track keeps playing when no rendition can replace it.
static void moq_source_subscribe_audio(struct moq_source *ctx, const std::shared_ptr<Connection> &conn,
				       const moq::Catalog &catalog)
{
	const auto candidates = moq_source_candidates(moq_source_ranked_audio(catalog.audio), [&](const auto &entry) {
		return moq_source_same_audio(conn->audio.get(), entry.first, entry.second) ||
		       moq_source_same_audio(conn->pending_audio.get(), entry.first, entry.second);
	});
	if (candidates.empty()) {
		LOG_INFO("Catalog has no enabled audio rendition; video only");
		moq_source_clear_audio_locked(ctx);
		return;
	}

	for (const auto *rendition : candidates) {
		const auto &id = rendition->first;
		const auto &config = rendition->second;
		if (moq_source_same_audio(conn->audio.get(), id, config) && conn->audio->instance == conn->instance) {
			conn->pending_audio.reset();
			return;
		}
		if (moq_source_same_audio(conn->pending_audio.get(), id, config))
			return;

		// A codec moq-audio can't decode, or a layout OBS can't place, falls through to the next rendition.
		if (!moq_source_audio_decodes(config)) {
			LOG_ERROR("Unsupported audio codec '%s' with %u channels in rendition %s", config.codec.c_str(),
				  config.channel_count, id.c_str());
			continue;
		}
		auto layout = moq_source_audio_layout(config.channel_count);
		if (!layout) {
			LOG_ERROR("Unsupported audio channel count %u in rendition %s: OBS has no layout for it",
				  config.channel_count, id.c_str());
			continue;
		}
		if (!ctx->sample_rate) {
			LOG_ERROR("OBS has no audio output to play into");
			break;
		}
		layout->sample_rate = ctx->sample_rate;

		LOG_INFO("Subscribing to audio track (%s, %u Hz, %u ch)", config.codec.c_str(), config.sample_rate,
			 config.channel_count);
		auto track = std::make_shared<Track>("Audio", id);
		track->instance = conn->instance;
		track->audio_config = config;
		track->audio_layout = *layout;
		conn->pending_audio = track;

		moq::AudioDecoderOutput output{};
		output.format = moq::AudioSampleFormat::kF32Planar;
		output.sample_rate = layout->sample_rate;
		output.channels = layout->channels;
		// Follows the rendition's broadcast and track itself, like moq_source_subscribe_track.
		track->call =
			moq::AudioConsumer::subscribe(conn->consumer, id, config, output)
				.then(ctx->worker.Executor(),
				      moq_source_track_current<moq::expected<std::shared_ptr<moq::AudioConsumer>>>(
					      ctx, conn, track, moq_source_on_audio_subscribed));
		return;
	}
	conn->pending_audio.reset();
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

	// Packets carry the frame's microsecond timestamp, which the decoder hands back on the
	// picture it belongs to.
	decoder->codec_ctx->pkt_timebase = AVRational{1, 1'000'000};

	// Open codec
	if (avcodec_open2(decoder->codec_ctx, codec, NULL) < 0) {
		LOG_ERROR("Failed to open codec");
		return nullptr;
	}

	return decoder;
}

// Feeds one frame to the decoder.
//
// NOTE: Caller must hold ctx->mutex when calling this function.
static Sent moq_source_send_packet(VideoDecoder &decoder, const moq::MediaFrame &frame_data)
{
	if (!decoder.codec_ctx)
		return Sent::Failed;

	// Skip non-keyframes until we get the first one
	if (!decoder.got_keyframe && !frame_data.keyframe) {
		decoder.frames_waiting_for_keyframe++;
		if (decoder.frames_waiting_for_keyframe == 1 || (decoder.frames_waiting_for_keyframe % 30) == 0) {
			LOG_INFO("Waiting for keyframe... (skipped %u frames so far)",
				 decoder.frames_waiting_for_keyframe);
		}
		return Sent::Accepted;
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
		return Sent::Failed;
	}

	packet->data = const_cast<uint8_t *>(frame_data.payload.data());
	packet->size = static_cast<int>(frame_data.payload.size());
	// The decoder hands this back on the picture it belongs to. Decode order has no
	// timestamp of its own here, so dts stays unset.
	packet->pts = static_cast<int64_t>(frame_data.timestamp_us);

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
		return ret == AVERROR(EAGAIN) ? Sent::Full : Sent::Failed;
	}
	return Sent::Accepted;
}

// Takes the decoder's next picture into decoder.frame, stamped with that picture's own
// timestamp: with B-frames it belongs to an earlier packet than the one just sent.
// Pending once the decoder needs more input or has drained.
//
// NOTE: Caller must hold ctx->mutex when calling this function.
static Decoded moq_source_receive_frame(VideoDecoder &decoder)
{
	// Receive decoded frames
	AVFrame *frame = av_frame_alloc();
	if (!frame) {
		return Decoded::Failed;
	}

	int ret = avcodec_receive_frame(decoder.codec_ctx, frame);
	if (ret < 0) {
		const bool done = ret == AVERROR(EAGAIN) || ret == AVERROR_EOF;
		if (!done) {
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
		return done ? Decoded::Pending : Decoded::Failed;
	}

	// Successfully decoded a frame - reset error counter
	decoder.consecutive_decode_errors = 0;

	if (frame->best_effort_timestamp == AV_NOPTS_VALUE) {
		LOG_ERROR("Decoded video frame has no timestamp");
		av_frame_free(&frame);
		return Decoded::Failed;
	}

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

	// OBS expects nanoseconds; packets carry microseconds.
	decoder.frame.timestamp = static_cast<uint64_t>(frame->best_effort_timestamp) * 1000;
	decoder.frame.format = VIDEO_FORMAT_RGBA;
	av_frame_free(&frame);
	return Decoded::Ready;
}

// NOTE: caller holds ctx->mutex.
static void moq_source_clear_audio_locked(struct moq_source *ctx)
{
	if (ctx->connection) {
		ctx->connection->pending_audio.reset();
		ctx->connection->audio.reset();
	}
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
