// SPDX-License-Identifier: GPL-2.0-or-later
#include <obs-module.h>
#include <util/threading.h>
#include <util/platform.h>
#include <util/darray.h>
#include <util/dstr.h>

#include <atomic>
#include <string>
#include <errno.h>
#include <time.h>

#ifdef _WIN32
#define strncasecmp _strnicmp
#endif
extern "C" {
#include <libavcodec/avcodec.h>
#include <libavutil/imgutils.h>
#include <libavutil/pixdesc.h>
#include <libswscale/swscale.h>
#include "moq.h"
}

#include "moq-source.h"
#include "logger.h"

// Map codec string from moq_video_config to FFmpeg codec ID
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

struct moq_source {
	obs_source_t *source;

	// Settings - current active connection settings
	char *url;
	char *broadcast;

	// Shutdown flag - set when destroy begins, callbacks should exit early
	std::atomic<bool> shutting_down;

	// Lifetime reference count, guarded by mutex. ctx must outlive every libmoq
	// subscription that was handed `ctx` as user_data: each delivers exactly one
	// terminal callback (status <= 0), the documented last touch of user_data
	// (libmoq >= 0.3.0). We hold one reference for the OBS-owned source plus one
	// per outstanding subscription (session, catalog, video track); a
	// subscription's reference is released by its terminal callback (see
	// subscription_ref). destroy drops the owner reference and waits for the
	// count to reach zero before freeing.
	int refs;
	pthread_cond_t refs_zero; // signaled when refs reaches 0

	// Session handles (all negative = invalid)
	std::atomic<uint32_t> generation; // Increments on reconnect
	bool reconnect_in_progress;       // True while reconnect is happening
	int32_t origin;
	int32_t session;
	int32_t consume;
	int32_t catalog_handle;
	int32_t video_track;

	// Decoder state
	AVCodecContext *codec_ctx;
	AVCodecID current_codec_id;         // Currently configured codec
	enum AVPixelFormat current_pix_fmt; // Current pixel format for sws_ctx
	struct SwsContext *sws_ctx;
	bool got_keyframe;
	uint32_t frames_waiting_for_keyframe; // Count of skipped frames while waiting
	uint32_t consecutive_decode_errors;   // Count of consecutive decode failures

	// Output frame buffer
	struct obs_source_frame frame;
	uint8_t *frame_buffer;

	// Threading
	pthread_mutex_t mutex;
};

// RAII helper that releases a subscription's lifetime reference when its async
// callback returns. libmoq (>= 0.3.0) hands `ctx` to each subscription as
// user_data and guarantees exactly one terminal callback (status code <= 0),
// after which user_data is never touched again. Each callback constructs one of
// these with `terminal` set on that terminal code; on scope exit it drops the
// matching reference and wakes moq_source_destroy when the last one is gone.
// Releasing on scope exit (not entry) keeps ctx valid for the whole callback
// body, including a terminal callback that still reads ctx before returning.
//
// Because libmoq runs all callbacks on a single runtime thread, a subscription's
// reference is held continuously from registration through its terminal
// callback, so ctx is always valid on entry to any of its callbacks - no
// shutting_down pre-check is needed for safety.
namespace {
struct subscription_ref {
	struct moq_source *ctx;
	bool terminal;

	subscription_ref(struct moq_source *c, bool is_terminal) : ctx(c), terminal(is_terminal) {}

	~subscription_ref()
	{
		if (!terminal)
			return;
		pthread_mutex_lock(&ctx->mutex);
		if (--ctx->refs == 0)
			pthread_cond_broadcast(&ctx->refs_zero);
		pthread_mutex_unlock(&ctx->mutex);
	}

	subscription_ref(const subscription_ref &) = delete;
	subscription_ref &operator=(const subscription_ref &) = delete;
};
} // namespace

// Forward declarations
static void moq_source_update(void *data, obs_data_t *settings);
static void moq_source_destroy(void *data);
static obs_properties_t *moq_source_properties(void *data);
static void moq_source_get_defaults(obs_data_t *settings);

// MoQ callbacks
static void on_session_status(void *user_data, int32_t code);
static void on_catalog(void *user_data, int32_t catalog);
static void on_video_frame(void *user_data, int32_t frame_id);

// Helper functions
static void moq_source_reconnect(struct moq_source *ctx);
static void moq_source_disconnect_locked(struct moq_source *ctx);
static void moq_source_blank_video(struct moq_source *ctx);
static bool moq_source_init_decoder(struct moq_source *ctx, const struct moq_video_config *config);
static void moq_source_destroy_decoder_locked(struct moq_source *ctx);
static void moq_source_decode_frame(struct moq_source *ctx, int32_t frame_id);

static void *moq_source_create(obs_data_t *settings, obs_source_t *source)
{
	struct moq_source *ctx = (struct moq_source *)bzalloc(sizeof(struct moq_source));
	ctx->source = source;

	// Initialize shutdown flag
	ctx->shutting_down = false;

	// One lifetime reference for the OBS-owned source itself; each subscription
	// adds its own while outstanding.
	ctx->refs = 1;
	pthread_cond_init(&ctx->refs_zero, NULL);

	// Initialize handles to invalid values
	ctx->generation = 0;
	ctx->reconnect_in_progress = false;
	ctx->origin = -1;
	ctx->session = -1;
	ctx->consume = -1;
	ctx->catalog_handle = -1;
	ctx->video_track = -1;

	// Initialize decoder state
	ctx->codec_ctx = NULL;
	ctx->current_codec_id = AV_CODEC_ID_NONE;
	ctx->current_pix_fmt = AV_PIX_FMT_NONE;
	ctx->sws_ctx = NULL;
	ctx->got_keyframe = false;
	ctx->frames_waiting_for_keyframe = 0;
	ctx->consecutive_decode_errors = 0;
	ctx->frame_buffer = NULL;

	// Initialize threading
	pthread_mutex_init(&ctx->mutex, NULL);

	// Initialize OBS frame structure - dimensions will be set dynamically from stream
	ctx->frame.width = 0;
	ctx->frame.height = 0;
	ctx->frame.format = VIDEO_FORMAT_RGBA;
	ctx->frame.linesize[0] = 0;

	// Load settings from OBS - this will auto-connect if settings are valid
	// (moq_source_update detects settings changed from NULL and reconnects)
	moq_source_update(ctx, settings);

	return ctx;
}

static void moq_source_destroy(void *data)
{
	struct moq_source *ctx = (struct moq_source *)data;

	pthread_mutex_lock(&ctx->mutex);
	ctx->shutting_down = true;

	// Close every subscription. Each was handed `ctx` as user_data and now
	// delivers its terminal callback, which releases the matching reference.
	// Closing via the handle makes that terminal fire promptly (libmoq's close
	// path wins over any pending update), so the wait below is real quiescence,
	// not a timing guess.
	moq_source_disconnect_locked(ctx);

	// Drop the owner reference, then wait for the outstanding subscriptions to
	// deliver their terminal callbacks before freeing ctx. The generous bounded
	// wait is a backstop against a subscription that never terminates (a libmoq
	// bug or an unaccounted handle): far better to log and proceed than to hang
	// OBS on source deletion. In normal operation the terminals arrive within
	// milliseconds and the timeout is never reached.
	bool timed_out = false;
	if (--ctx->refs > 0) {
		struct timespec deadline;
		timespec_get(&deadline, TIME_UTC);
		deadline.tv_sec += 2;
		while (ctx->refs > 0) {
			if (pthread_cond_timedwait(&ctx->refs_zero, &ctx->mutex, &deadline) == ETIMEDOUT) {
				LOG_WARNING("Teardown timed out with %d MoQ callback(s) still outstanding; "
					    "leaking ctx to avoid a use-after-free",
					    ctx->refs);
				timed_out = true;
				break;
			}
		}
	}
	pthread_mutex_unlock(&ctx->mutex);

	// A subscription callback still holds ctx (it references ctx->mutex,
	// ctx->refs, ctx->refs_zero). Freeing now would be a use-after-free when
	// that callback fires, so intentionally leak instead. This only happens on
	// the abnormal timeout path above.
	if (timed_out)
		return;

	bfree(ctx->url);
	bfree(ctx->broadcast);
	// Note: frame_buffer is already freed by moq_source_disconnect_locked

	pthread_cond_destroy(&ctx->refs_zero);
	pthread_mutex_destroy(&ctx->mutex);

	bfree(ctx);
}

// Relay URLs can embed credentials (userinfo) or a query/path token, and OBS
// logs are frequently shared for support. Reduce a URL to scheme://host[:port]
// for logging so secrets never reach persistent logs.
static std::string redact_url(const char *url)
{
	if (!url || !*url)
		return "(null)";

	std::string s(url);
	size_t scheme = s.find("://");
	std::string prefix = (scheme == std::string::npos) ? "" : s.substr(0, scheme + 3);
	size_t rest = (scheme == std::string::npos) ? 0 : scheme + 3;

	// The authority ends at the first '/', '?' or '#'.
	size_t auth_end = s.find_first_of("/?#", rest);
	std::string authority = s.substr(rest, auth_end == std::string::npos ? std::string::npos : auth_end - rest);

	// Drop any userinfo (user:pass@). Use the last '@' so an unescaped '@' in a
	// password can't leave part of it behind.
	size_t at = authority.rfind('@');
	if (at != std::string::npos)
		authority = authority.substr(at + 1);

	return prefix + authority;
}

static void moq_source_update(void *data, obs_data_t *settings)
{
	struct moq_source *ctx = (struct moq_source *)data;

	const char *url = obs_data_get_string(settings, "url");
	const char *broadcast = obs_data_get_string(settings, "broadcast");

	pthread_mutex_lock(&ctx->mutex);

	// Check if settings actually changed
	bool url_changed = (!ctx->url && url && strlen(url) > 0) || (ctx->url && !url) ||
			   (ctx->url && url && strcmp(ctx->url, url) != 0);
	bool broadcast_changed = (!ctx->broadcast && broadcast && strlen(broadcast) > 0) ||
				 (ctx->broadcast && !broadcast) ||
				 (ctx->broadcast && broadcast && strcmp(ctx->broadcast, broadcast) != 0);
	bool settings_changed = url_changed || broadcast_changed;

	// Store the new settings
	bfree(ctx->url);
	ctx->url = bstrdup(url);
	bfree(ctx->broadcast);
	ctx->broadcast = bstrdup(broadcast);

	// Check if new settings are valid for connection
	bool valid = ctx->url && ctx->broadcast && strlen(ctx->url) > 0 && strlen(ctx->broadcast) > 0;

	pthread_mutex_unlock(&ctx->mutex);

	// If settings changed and are valid, reconnect
	if (settings_changed && valid) {
		LOG_INFO("Settings changed, reconnecting (url=%s, broadcast=%s)", redact_url(url).c_str(),
			 broadcast ? broadcast : "(null)");
		moq_source_reconnect(ctx);
	} else if (settings_changed && !valid) {
		LOG_INFO("Settings changed but invalid - disconnecting");
		pthread_mutex_lock(&ctx->mutex);
		moq_source_disconnect_locked(ctx);
		pthread_mutex_unlock(&ctx->mutex);
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

// Forward declaration for use in callback
static void moq_source_start_consume(struct moq_source *ctx, uint32_t expected_gen);

// MoQ callback implementations
static void on_session_status(void *user_data, int32_t code)
{
	struct moq_source *ctx = (struct moq_source *)user_data;

	// Hold this session subscription's reference for the callback's lifetime. A
	// terminal status (<= 0) means the session task has ended and will not touch
	// ctx again, so the reference is released when `ref` goes out of scope.
	subscription_ref ref(ctx, code <= 0);

	pthread_mutex_lock(&ctx->mutex);
	if (ctx->shutting_down.load()) {
		// Teardown in progress; nothing to do (a terminal callback still
		// releases its reference via `ref`).
		pthread_mutex_unlock(&ctx->mutex);
		return;
	}
	if (ctx->session < 0) {
		LOG_DEBUG("Ignoring session status callback - already disconnected");
		pthread_mutex_unlock(&ctx->mutex);
		return;
	}
	uint32_t current_gen = ctx->generation;

	// libmoq status codes (>= 0.3.0):
	//   > 0 : (re)connected, carrying the connection epoch (1 = first connect,
	//         2 = first reconnect, ...). The session auto-reconnects internally.
	//   = 0 : closed cleanly via moq_session_close (terminal) - we initiated it.
	//   < 0 : reconnect permanently gave up or fatal error (terminal).
	if (code > 0) {
		pthread_mutex_unlock(&ctx->mutex);
		LOG_INFO("MoQ session connected (generation %u, epoch %d)", current_gen, code);
		// Start consuming only on the first connect. On later epochs libmoq has
		// re-subscribed our existing consumer automatically (the origin outlives
		// the connection), so recreating it would leak handles.
		if (code == 1) {
			moq_source_start_consume(ctx, current_gen);
		}
	} else if (code == 0) {
		// Clean close - we asked for this (disconnect/reconnect/destroy). The
		// handle is already being torn down; nothing to do here.
		pthread_mutex_unlock(&ctx->mutex);
		LOG_DEBUG("MoQ session closed cleanly (generation %u)", current_gen);
	} else {
		// Terminal error (e.g. auth failure, or reconnect gave up). Tear down
		// every subscription, not just the session, so the catalog/video
		// subscriptions also fire their terminal callbacks and release their
		// references promptly instead of lingering until the source is destroyed.
		LOG_ERROR("MoQ session error: %d (generation %u)", code, current_gen);
		moq_source_disconnect_locked(ctx);
		pthread_mutex_unlock(&ctx->mutex);

		// Blank the video to show error state
		moq_source_blank_video(ctx);
	}
}

static void on_catalog(void *user_data, int32_t catalog)
{
	struct moq_source *ctx = (struct moq_source *)user_data;

	// Hold the catalog subscription's reference for the callback's lifetime;
	// release it when this is the terminal callback (catalog <= 0).
	subscription_ref ref(ctx, catalog <= 0);

	if (catalog <= 0) {
		if (catalog < 0) {
			LOG_ERROR("Catalog subscription error: %d", catalog);
			// Surface the failure (e.g. invalid broadcast) by blanking, as the
			// old catalog-fetch-failed path did - but not during our own teardown.
			if (!ctx->shutting_down.load())
				moq_source_blank_video(ctx);
		} else {
			LOG_DEBUG("Catalog subscription closed cleanly");
		}
		return;
	}

	LOG_INFO("Catalog callback received: %d", catalog);

	// `catalog` is a catalog *snapshot* id (a different slab from the
	// subscription handle stored in ctx->catalog_handle). It must be freed with
	// moq_consume_catalog_free on every path below - never closed.
	pthread_mutex_lock(&ctx->mutex);
	bool stale = ctx->shutting_down.load() || ctx->consume < 0;
	uint32_t current_gen = ctx->generation;
	pthread_mutex_unlock(&ctx->mutex);
	if (stale) {
		// Disconnected or shutting down; ignore this snapshot.
		moq_consume_catalog_free(catalog);
		return;
	}

	// Get video configuration from the snapshot.
	struct moq_video_config video_config;
	if (moq_consume_video_config(catalog, 0, &video_config) < 0) {
		LOG_ERROR("Failed to get video config");
		moq_consume_catalog_free(catalog);
		return;
	}

	// Initialize decoder with the video config (takes mutex internally, and
	// copies the codec/description out of the snapshot).
	if (!moq_source_init_decoder(ctx, &video_config)) {
		LOG_ERROR("Failed to initialize decoder");
		moq_consume_catalog_free(catalog);
		return;
	}

	// Pre-account for the video track subscription before handing ctx to libmoq,
	// so its reference is in place the instant the subscription exists. Undone
	// below only if creation fails.
	pthread_mutex_lock(&ctx->mutex);
	ctx->refs++;
	pthread_mutex_unlock(&ctx->mutex);

	// Subscribe to the video track (index 0). This takes the catalog snapshot,
	// not the consume handle, and does not retain it - so free the snapshot
	// immediately after.
	int32_t track = moq_consume_video(catalog, 0, 0, on_video_frame, ctx);
	moq_consume_catalog_free(catalog);
	if (track < 0) {
		LOG_ERROR("Failed to subscribe to video track: %d", track);
		pthread_mutex_lock(&ctx->mutex);
		// No subscription was created, so no terminal will fire; undo the ref.
		if (--ctx->refs == 0)
			pthread_cond_broadcast(&ctx->refs_zero);
		pthread_mutex_unlock(&ctx->mutex);
		return;
	}

	// The video track subscription now exists and will deliver a terminal
	// on_video_frame (<= 0) that releases the reference added above - even on the
	// cleanup path below.
	pthread_mutex_lock(&ctx->mutex);
	if (ctx->generation == current_gen && !ctx->shutting_down.load()) {
		// A catalog update can arrive while a track is already subscribed; close
		// the previous one so its terminal callback releases its reference (else
		// it would linger until teardown's bounded wait).
		int32_t old_track = ctx->video_track;
		ctx->video_track = track;
		pthread_mutex_unlock(&ctx->mutex);
		if (old_track >= 0)
			moq_consume_video_close(old_track);
		LOG_INFO("Subscribed to video track successfully");
	} else {
		// Stale or shutting down: close the track we just created; its terminal
		// callback releases the reference added above.
		pthread_mutex_unlock(&ctx->mutex);
		moq_consume_video_close(track);
	}
}

static void on_video_frame(void *user_data, int32_t frame_id)
{
	struct moq_source *ctx = (struct moq_source *)user_data;

	// Hold the video track subscription's reference for the callback's lifetime
	// (which includes the FFmpeg decode in moq_source_decode_frame); release it
	// on the terminal callback (frame_id <= 0).
	subscription_ref ref(ctx, frame_id <= 0);

	if (frame_id <= 0) {
		if (frame_id < 0)
			LOG_ERROR("Video track error: %d", frame_id);
		else
			LOG_DEBUG("Video track closed cleanly");
		return;
	}

	pthread_mutex_lock(&ctx->mutex);
	if (ctx->shutting_down.load() || ctx->consume < 0) {
		// Shutting down or disconnected: drop the frame.
		pthread_mutex_unlock(&ctx->mutex);
		moq_consume_frame_close(frame_id);
		return;
	}
	pthread_mutex_unlock(&ctx->mutex);

	moq_source_decode_frame(ctx, frame_id);
}

// Helper function implementations
static void moq_source_reconnect(struct moq_source *ctx)
{
	// Increment generation to invalidate old callbacks
	pthread_mutex_lock(&ctx->mutex);

	// Never start a new connection once teardown has begun: it would register a
	// subscription after destroy already closed everything, leaking its
	// reference until the bounded wait times out. (OBS serializes update/destroy
	// so this is defense-in-depth.)
	if (ctx->shutting_down.load()) {
		pthread_mutex_unlock(&ctx->mutex);
		return;
	}

	// Check if reconnect is already in progress
	if (ctx->reconnect_in_progress) {
		LOG_DEBUG("Reconnect already in progress, skipping");
		pthread_mutex_unlock(&ctx->mutex);
		return;
	}

	ctx->reconnect_in_progress = true;
	uint32_t new_gen = ctx->generation.load() + 1;
	LOG_INFO("Reconnecting (generation %u -> %u)", ctx->generation.load(), new_gen);
	ctx->generation.store(new_gen);
	moq_source_disconnect_locked(ctx);

	// Copy URL while holding mutex for thread safety
	char *url_copy = bstrdup(ctx->url);
	pthread_mutex_unlock(&ctx->mutex);

	// Blank video while reconnecting to avoid showing stale frames
	moq_source_blank_video(ctx);

	// No delay needed before reconnecting: libmoq origins and sessions are fully
	// independent (each origin is a distinct random instance, each session its own
	// task), so the new connection shares no client-side state with the one we just
	// closed. moq_origin_close removes the origin synchronously, and moq_session_close
	// only signals the old session's task to wind down asynchronously on the libmoq
	// runtime thread - nothing the new origin/session can collide with. (The previous
	// os_sleep_ms(50) here was a timing band-aid that provided no real guarantee.)

	// Create origin for consuming (outside mutex since it may block)
	int32_t new_origin = moq_origin_create();
	if (new_origin < 0) {
		LOG_ERROR("Failed to create origin: %d", new_origin);
		bfree(url_copy);
		pthread_mutex_lock(&ctx->mutex);
		ctx->reconnect_in_progress = false;
		pthread_mutex_unlock(&ctx->mutex);
		return;
	}

	// Pre-account for the session subscription before handing ctx to libmoq: the
	// connection can fail and fire its terminal on_session_status from the
	// runtime thread immediately, and that must not decrement the reference
	// before we have added it.
	pthread_mutex_lock(&ctx->mutex);
	ctx->refs++;
	pthread_mutex_unlock(&ctx->mutex);

	// Connect to MoQ server (consume will happen in on_session_status callback)
	int32_t new_session = moq_session_connect(url_copy, strlen(url_copy),
						  0,          // origin_publish
						  new_origin, // origin_consume
						  on_session_status, ctx);
	bfree(url_copy);

	if (new_session < 0) {
		LOG_ERROR("Failed to connect to MoQ server: %d", new_session);
		moq_origin_close(new_origin);
		pthread_mutex_lock(&ctx->mutex);
		// No session subscription was created, so no terminal will fire; undo
		// the reference we pre-added above.
		if (--ctx->refs == 0)
			pthread_cond_broadcast(&ctx->refs_zero);
		ctx->reconnect_in_progress = false;
		pthread_mutex_unlock(&ctx->mutex);
		return;
	}

	// Now update ctx with the new handles, checking if generation changed
	pthread_mutex_lock(&ctx->mutex);
	if (ctx->generation != new_gen) {
		// Another reconnect happened while we were creating origin/session
		// Clean up our newly created resources
		ctx->reconnect_in_progress = false;
		pthread_mutex_unlock(&ctx->mutex);
		LOG_INFO("Generation changed during reconnect setup, cleaning up stale resources");
		moq_session_close(new_session);
		moq_origin_close(new_origin);
		return;
	}
	ctx->origin = new_origin;
	ctx->session = new_session;
	ctx->reconnect_in_progress = false;
	LOG_INFO("Connecting to MoQ server (generation %u)", new_gen);
	pthread_mutex_unlock(&ctx->mutex);
}

// Called after session is connected successfully
static void moq_source_start_consume(struct moq_source *ctx, uint32_t expected_gen)
{
	// Check if origin is still valid and generation matches
	pthread_mutex_lock(&ctx->mutex);
	if (ctx->origin < 0 || ctx->generation != expected_gen) {
		pthread_mutex_unlock(&ctx->mutex);
		LOG_INFO("Skipping stale consume (generation mismatch or invalid origin)");
		return;
	}
	// Capture values while holding mutex
	int32_t origin = ctx->origin;
	char *broadcast_copy = bstrdup(ctx->broadcast);
	pthread_mutex_unlock(&ctx->mutex);

	// Consume broadcast by path
	int32_t consume = moq_origin_consume(origin, broadcast_copy, strlen(broadcast_copy));
	if (consume < 0) {
		LOG_ERROR("Failed to consume broadcast '%s': %d", broadcast_copy, consume);
		bfree(broadcast_copy);
		// Failed to consume - clean up session/origin
		pthread_mutex_lock(&ctx->mutex);
		if (ctx->generation == expected_gen) {
			if (ctx->session >= 0) {
				moq_session_close(ctx->session);
				ctx->session = -1;
			}
			if (ctx->origin >= 0) {
				moq_origin_close(ctx->origin);
				ctx->origin = -1;
			}
		}
		pthread_mutex_unlock(&ctx->mutex);
		moq_source_blank_video(ctx);
		return;
	}

	pthread_mutex_lock(&ctx->mutex);
	// Verify generation hasn't changed while we were waiting
	if (ctx->generation != expected_gen) {
		pthread_mutex_unlock(&ctx->mutex);
		LOG_INFO("Generation changed during consume setup, cleaning up");
		moq_consume_close(consume);
		bfree(broadcast_copy);
		return;
	}
	ctx->consume = consume;
	pthread_mutex_unlock(&ctx->mutex);

	// Pre-account for the catalog subscription before handing ctx to libmoq, so
	// its reference is in place the instant the subscription exists (see the
	// note on subscription_ref). Undone below only if creation fails.
	pthread_mutex_lock(&ctx->mutex);
	ctx->refs++;
	pthread_mutex_unlock(&ctx->mutex);

	// Subscribe to catalog updates
	int32_t catalog_handle = moq_consume_catalog(consume, on_catalog, ctx);
	if (catalog_handle < 0) {
		LOG_ERROR("Failed to subscribe to catalog for '%s': %d", broadcast_copy, catalog_handle);
		bfree(broadcast_copy);
		// Failed to get catalog - clean up
		pthread_mutex_lock(&ctx->mutex);
		// No subscription was created, so no terminal will fire; undo the ref.
		if (--ctx->refs == 0)
			pthread_cond_broadcast(&ctx->refs_zero);
		if (ctx->generation == expected_gen) {
			if (ctx->consume >= 0) {
				moq_consume_close(ctx->consume);
				ctx->consume = -1;
			}
			if (ctx->session >= 0) {
				moq_session_close(ctx->session);
				ctx->session = -1;
			}
			if (ctx->origin >= 0) {
				moq_origin_close(ctx->origin);
				ctx->origin = -1;
			}
		}
		pthread_mutex_unlock(&ctx->mutex);
		moq_source_blank_video(ctx);
		return;
	}

	// The catalog subscription now exists and will deliver a terminal on_catalog
	// (<= 0) that releases the reference added above. Store the subscription
	// handle so disconnect can close it, which makes that terminal fire promptly.
	// (This is the real subscription handle, distinct from the catalog snapshot
	// ids delivered to on_catalog.)
	pthread_mutex_lock(&ctx->mutex);
	if (ctx->generation == expected_gen && !ctx->shutting_down.load()) {
		ctx->catalog_handle = catalog_handle;
		pthread_mutex_unlock(&ctx->mutex);
	} else {
		// Stale or shutting down: close it; its terminal releases the reference.
		pthread_mutex_unlock(&ctx->mutex);
		moq_consume_catalog_close(catalog_handle);
	}

	LOG_INFO("Consuming broadcast: %s", broadcast_copy);
	bfree(broadcast_copy);
}

// NOTE: Caller must hold ctx->mutex when calling this function.
//
// The moq_*_close calls below only *signal* each subscription to wind down;
// libmoq delivers the terminal callback asynchronously on its runtime thread,
// never synchronously from within close(). That is what lets us close under the
// mutex here without the terminal callback's subscription_ref re-entering the
// mutex on this same thread (which would self-deadlock).
static void moq_source_disconnect_locked(struct moq_source *ctx)
{
	if (ctx->video_track >= 0) {
		moq_consume_video_close(ctx->video_track);
		ctx->video_track = -1;
	}

	if (ctx->catalog_handle >= 0) {
		moq_consume_catalog_close(ctx->catalog_handle);
		ctx->catalog_handle = -1;
	}

	if (ctx->consume >= 0) {
		moq_consume_close(ctx->consume);
		ctx->consume = -1;
	}

	if (ctx->session >= 0) {
		moq_session_close(ctx->session);
		ctx->session = -1;
	}

	if (ctx->origin >= 0) {
		moq_origin_close(ctx->origin);
		ctx->origin = -1;
	}

	moq_source_destroy_decoder_locked(ctx);
	ctx->got_keyframe = false;
	ctx->frames_waiting_for_keyframe = 0;
	ctx->consecutive_decode_errors = 0;
}

// Blanks the video preview by outputting a NULL frame
static void moq_source_blank_video(struct moq_source *ctx)
{
	// Passing NULL to obs_source_output_video clears the current frame
	obs_source_output_video(ctx->source, NULL);
	LOG_DEBUG("Video preview blanked");
}

static bool moq_source_init_decoder(struct moq_source *ctx, const struct moq_video_config *config)
{
	// Map codec string to FFmpeg codec ID dynamically
	AVCodecID codec_id = codec_string_to_id(config->codec, config->codec_len);
	if (codec_id == AV_CODEC_ID_NONE) {
		// Log the codec string for debugging (may not be null-terminated)
		char codec_str[64] = {0};
		size_t copy_len = config->codec_len < sizeof(codec_str) - 1 ? config->codec_len : sizeof(codec_str) - 1;
		if (config->codec && copy_len > 0) {
			memcpy(codec_str, config->codec, copy_len);
		}
		LOG_ERROR("Unknown or unsupported codec: '%s'", codec_str);
		return false;
	}

	// Find decoder for the codec
	const AVCodec *codec = avcodec_find_decoder(codec_id);
	if (!codec) {
		LOG_ERROR("Decoder not found for codec ID: %d", codec_id);
		return false;
	}

	// Create codec context (can be done outside mutex)
	AVCodecContext *new_codec_ctx = avcodec_alloc_context3(codec);
	if (!new_codec_ctx) {
		LOG_ERROR("Failed to allocate codec context");
		return false;
	}

	// Get dimensions from config - required for buffer allocation
	uint32_t width = 0;
	uint32_t height = 0;

	if (config->coded_width && *config->coded_width > 0) {
		new_codec_ctx->width = *config->coded_width;
		width = *config->coded_width;
	}
	if (config->coded_height && *config->coded_height > 0) {
		new_codec_ctx->height = *config->coded_height;
		height = *config->coded_height;
	}

	// Use codec description as extradata (contains SPS/PPS for H.264, VPS/SPS/PPS for HEVC, etc.)
	if (config->description && config->description_len > 0) {
		new_codec_ctx->extradata =
			(uint8_t *)av_mallocz(config->description_len + AV_INPUT_BUFFER_PADDING_SIZE);
		if (new_codec_ctx->extradata) {
			memcpy(new_codec_ctx->extradata, config->description, config->description_len);
			new_codec_ctx->extradata_size = static_cast<int>(config->description_len);
		}
	}

	// Open codec
	if (avcodec_open2(new_codec_ctx, codec, NULL) < 0) {
		LOG_ERROR("Failed to open codec");
		avcodec_free_context(&new_codec_ctx);
		return false;
	}

	// If dimensions weren't in config, try to get them from the opened codec context
	// (may have been parsed from extradata)
	if (width == 0 && new_codec_ctx->width > 0) {
		width = new_codec_ctx->width;
	}
	if (height == 0 && new_codec_ctx->height > 0) {
		height = new_codec_ctx->height;
	}

	// Now take the mutex and swap in the new decoder state
	pthread_mutex_lock(&ctx->mutex);

	// Destroy old decoder state
	if (ctx->sws_ctx) {
		sws_freeContext(ctx->sws_ctx);
	}
	if (ctx->codec_ctx) {
		avcodec_free_context(&ctx->codec_ctx);
	}
	if (ctx->frame_buffer) {
		bfree(ctx->frame_buffer);
	}

	// Install new decoder state
	// Note: sws_ctx, frame_buffer, and frame dimensions will be initialized
	// dynamically on first decoded frame when we know the actual pixel format
	ctx->codec_ctx = new_codec_ctx;
	ctx->current_codec_id = codec_id;
	ctx->current_pix_fmt = AV_PIX_FMT_NONE; // Will be set on first frame
	ctx->sws_ctx = NULL;                    // Will be created on first frame with actual pixel format
	ctx->frame_buffer = NULL;               // Will be allocated on first frame with actual dimensions
	ctx->frame.width = width;
	ctx->frame.height = height;
	ctx->frame.linesize[0] = width * 4;
	ctx->frame.data[0] = NULL;
	ctx->frame.format = VIDEO_FORMAT_RGBA;
	ctx->frame.timestamp = 0;
	ctx->got_keyframe = false;
	ctx->frames_waiting_for_keyframe = 0;
	ctx->consecutive_decode_errors = 0;

	pthread_mutex_unlock(&ctx->mutex);

	// Log codec name for debugging
	char codec_str[64] = {0};
	size_t copy_len = config->codec_len < sizeof(codec_str) - 1 ? config->codec_len : sizeof(codec_str) - 1;
	if (config->codec && copy_len > 0) {
		memcpy(codec_str, config->codec, copy_len);
	}
	LOG_INFO("Decoder initialized: codec=%s, dimensions=%ux%u (may be refined on first frame)", codec_str, width,
		 height);
	return true;
}

// NOTE: Caller must hold ctx->mutex when calling this function
static void moq_source_destroy_decoder_locked(struct moq_source *ctx)
{
	if (ctx->sws_ctx) {
		sws_freeContext(ctx->sws_ctx);
		ctx->sws_ctx = NULL;
	}

	if (ctx->codec_ctx) {
		avcodec_free_context(&ctx->codec_ctx);
		ctx->codec_ctx = NULL;
	}

	if (ctx->frame_buffer) {
		bfree(ctx->frame_buffer);
		ctx->frame_buffer = NULL;
		ctx->frame.data[0] = NULL;
	}

	// Reset dynamic format tracking
	ctx->current_codec_id = AV_CODEC_ID_NONE;
	ctx->current_pix_fmt = AV_PIX_FMT_NONE;
}

static void moq_source_decode_frame(struct moq_source *ctx, int32_t frame_id)
{
	// Fast path: check atomic flag before taking lock
	if (ctx->shutting_down.load()) {
		moq_consume_frame_close(frame_id);
		return;
	}

	pthread_mutex_lock(&ctx->mutex);

	// Double-check after acquiring lock (may have changed)
	if (ctx->shutting_down.load()) {
		pthread_mutex_unlock(&ctx->mutex);
		moq_consume_frame_close(frame_id);
		return;
	}

	// Check if decoder is still valid (may have been destroyed during reconnect)
	// Note: sws_ctx and frame_buffer may be NULL on first frame - they're created dynamically
	if (!ctx->codec_ctx) {
		pthread_mutex_unlock(&ctx->mutex);
		moq_consume_frame_close(frame_id);
		return;
	}

	// Get frame data
	struct moq_frame frame_data;
	if (moq_consume_frame(frame_id, &frame_data) < 0) {
		LOG_ERROR("Failed to get frame data");
		pthread_mutex_unlock(&ctx->mutex);
		moq_consume_frame_close(frame_id);
		return;
	}

	// Skip non-keyframes until we get the first one
	if (!ctx->got_keyframe && !frame_data.keyframe) {
		ctx->frames_waiting_for_keyframe++;
		if (ctx->frames_waiting_for_keyframe == 1 || (ctx->frames_waiting_for_keyframe % 30) == 0) {
			LOG_INFO("Waiting for keyframe... (skipped %u frames so far)",
				 ctx->frames_waiting_for_keyframe);
		}
		pthread_mutex_unlock(&ctx->mutex);
		moq_consume_frame_close(frame_id);
		return;
	}

	// Mark that we've received a keyframe from the stream
	if (frame_data.keyframe) {
		if (!ctx->got_keyframe) {
			LOG_INFO("Got keyframe after waiting for %u frames, payload_size=%zu",
				 ctx->frames_waiting_for_keyframe, frame_data.payload_size);
			// Flush decoder to ensure clean state when starting from keyframe
			avcodec_flush_buffers(ctx->codec_ctx);
		}
		ctx->got_keyframe = true;
		ctx->frames_waiting_for_keyframe = 0;
		ctx->consecutive_decode_errors = 0;
	}

	// Create AVPacket from frame data
	AVPacket *packet = av_packet_alloc();
	if (!packet) {
		pthread_mutex_unlock(&ctx->mutex);
		moq_consume_frame_close(frame_id);
		return;
	}

	packet->data = (uint8_t *)frame_data.payload;
	packet->size = static_cast<int>(frame_data.payload_size);
	packet->pts = frame_data.timestamp_us / 1000; // Convert to milliseconds
	packet->dts = packet->pts;

	// Send packet to decoder
	int ret = avcodec_send_packet(ctx->codec_ctx, packet);
	av_packet_free(&packet);

	if (ret < 0) {
		if (ret != AVERROR(EAGAIN)) {
			ctx->consecutive_decode_errors++;
			char errbuf[AV_ERROR_MAX_STRING_SIZE];
			av_strerror(ret, errbuf, sizeof(errbuf));

			// If too many consecutive errors, flush decoder and wait for next keyframe
			if (ctx->consecutive_decode_errors >= 5) {
				LOG_WARNING("Too many send errors (%u), flushing decoder and waiting for keyframe",
					    ctx->consecutive_decode_errors);
				avcodec_flush_buffers(ctx->codec_ctx);
				ctx->got_keyframe = false;
				ctx->consecutive_decode_errors = 0;
			} else if (ctx->consecutive_decode_errors == 1) {
				LOG_ERROR("Error sending packet to decoder: %s", errbuf);
			}
		}
		pthread_mutex_unlock(&ctx->mutex);
		moq_consume_frame_close(frame_id);
		return;
	}

	// Receive decoded frames
	AVFrame *frame = av_frame_alloc();
	if (!frame) {
		pthread_mutex_unlock(&ctx->mutex);
		moq_consume_frame_close(frame_id);
		return;
	}

	ret = avcodec_receive_frame(ctx->codec_ctx, frame);
	if (ret < 0) {
		if (ret != AVERROR(EAGAIN)) {
			ctx->consecutive_decode_errors++;
			char errbuf[AV_ERROR_MAX_STRING_SIZE];
			av_strerror(ret, errbuf, sizeof(errbuf));

			// If too many consecutive errors, flush decoder and wait for next keyframe
			if (ctx->consecutive_decode_errors >= 5) {
				LOG_WARNING("Too many decode errors (%u), flushing decoder and waiting for keyframe",
					    ctx->consecutive_decode_errors);
				avcodec_flush_buffers(ctx->codec_ctx);
				ctx->got_keyframe = false;
				ctx->consecutive_decode_errors = 0;
			} else if (ctx->consecutive_decode_errors == 1) {
				// Only log first error in a sequence
				LOG_ERROR("Error receiving frame from decoder: %s", errbuf);
			}
		}
		av_frame_free(&frame);
		pthread_mutex_unlock(&ctx->mutex);
		moq_consume_frame_close(frame_id);
		return;
	}

	// Successfully decoded a frame - reset error counter
	ctx->consecutive_decode_errors = 0;

	// Check if we need to (re)initialize the scaler - either first frame, dimension change, or pixel format change
	enum AVPixelFormat decoded_pix_fmt = (enum AVPixelFormat)frame->format;
	bool dimensions_changed = (frame->width != (int)ctx->frame.width || frame->height != (int)ctx->frame.height);
	bool pix_fmt_changed = (decoded_pix_fmt != ctx->current_pix_fmt);
	bool need_reinit = (!ctx->sws_ctx || !ctx->frame_buffer || dimensions_changed || pix_fmt_changed);

	if (need_reinit) {
		if (dimensions_changed) {
			LOG_INFO("Decoded frame dimensions changed: %ux%u -> %dx%d", ctx->frame.width,
				 ctx->frame.height, frame->width, frame->height);
		}
		if (pix_fmt_changed) {
			LOG_INFO("Decoded frame pixel format changed: %d -> %d (%s)", ctx->current_pix_fmt,
				 decoded_pix_fmt,
				 av_get_pix_fmt_name(decoded_pix_fmt) ? av_get_pix_fmt_name(decoded_pix_fmt)
								      : "unknown");
		}

		// Validate that dimensions are positive and reasonable
		if (frame->width <= 0 || frame->height <= 0 || frame->width > 16384 || frame->height > 16384) {
			LOG_ERROR("Invalid decoded frame dimensions: %dx%d", frame->width, frame->height);
			av_frame_free(&frame);
			pthread_mutex_unlock(&ctx->mutex);
			moq_consume_frame_close(frame_id);
			return;
		}

		// Validate pixel format is supported by swscale
		if (decoded_pix_fmt == AV_PIX_FMT_NONE) {
			LOG_ERROR("Invalid decoded frame pixel format: %d", decoded_pix_fmt);
			av_frame_free(&frame);
			pthread_mutex_unlock(&ctx->mutex);
			moq_consume_frame_close(frame_id);
			return;
		}

		// Free old sws context
		if (ctx->sws_ctx) {
			sws_freeContext(ctx->sws_ctx);
			ctx->sws_ctx = NULL;
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
			pthread_mutex_unlock(&ctx->mutex);
			moq_consume_frame_close(frame_id);
			return;
		}

		// Reallocate frame buffer for new dimensions (width * height * 4 for RGBA)
		size_t new_buffer_size = (size_t)frame->width * (size_t)frame->height * 4;
		uint8_t *new_frame_buffer = (uint8_t *)bmalloc(new_buffer_size);
		if (!new_frame_buffer) {
			LOG_ERROR("Failed to allocate frame buffer for %dx%d (%zu bytes)", frame->width, frame->height,
				  new_buffer_size);
			sws_freeContext(new_sws_ctx);
			av_frame_free(&frame);
			pthread_mutex_unlock(&ctx->mutex);
			moq_consume_frame_close(frame_id);
			return;
		}

		// Free old frame buffer
		if (ctx->frame_buffer) {
			bfree(ctx->frame_buffer);
		}

		// Install new state
		ctx->sws_ctx = new_sws_ctx;
		ctx->current_pix_fmt = decoded_pix_fmt;
		ctx->frame_buffer = new_frame_buffer;
		ctx->frame.width = frame->width;
		ctx->frame.height = frame->height;
		ctx->frame.linesize[0] = frame->width * 4;
		ctx->frame.data[0] = new_frame_buffer;

		LOG_INFO("Scaler initialized for %dx%d pix_fmt=%s", frame->width, frame->height,
			 av_get_pix_fmt_name(decoded_pix_fmt) ? av_get_pix_fmt_name(decoded_pix_fmt) : "unknown");
	}

	// Convert YUV420P to RGBA
	uint8_t *dst_data[4] = {ctx->frame_buffer, NULL, NULL, NULL};
	int dst_linesize[4] = {static_cast<int>(ctx->frame.width * 4), 0, 0, 0};

	sws_scale(ctx->sws_ctx, (const uint8_t *const *)frame->data, frame->linesize, 0, ctx->frame.height, dst_data,
		  dst_linesize);

	// Update OBS frame timestamp and output. OBS expects nanoseconds; libmoq
	// delivers microseconds.
	ctx->frame.timestamp = frame_data.timestamp_us * 1000;
	obs_source_output_video(ctx->source, &ctx->frame);

	av_frame_free(&frame);
	pthread_mutex_unlock(&ctx->mutex);
	moq_consume_frame_close(frame_id);
}

// Registration function
void register_moq_source()
{
	struct obs_source_info info = {};
	info.id = "moq_source";
	info.type = OBS_SOURCE_TYPE_INPUT;
	info.output_flags = OBS_SOURCE_ASYNC_VIDEO | OBS_SOURCE_DO_NOT_DUPLICATE;
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
