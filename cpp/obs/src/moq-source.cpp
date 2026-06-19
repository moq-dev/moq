// SPDX-License-Identifier: GPL-2.0-or-later
#include <obs-module.h>
#include <util/threading.h>
#include <util/platform.h>
#include <util/darray.h>
#include <util/dstr.h>

#include <atomic>

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
	if ((len >= 4 && strncasecmp(codec, "h264", 4) == 0) ||
	    (len >= 3 && strncasecmp(codec, "avc", 3) == 0)) {
		return AV_CODEC_ID_H264;
	}

	// HEVC/H.265
	if ((len >= 4 && strncasecmp(codec, "hevc", 4) == 0) ||
	    (len >= 4 && strncasecmp(codec, "h265", 4) == 0) ||
	    (len >= 4 && strncasecmp(codec, "hev1", 4) == 0) ||
	    (len >= 4 && strncasecmp(codec, "hvc1", 4) == 0)) {
		return AV_CODEC_ID_HEVC;
	}

	// VP9
	if ((len >= 3 && strncasecmp(codec, "vp9", 3) == 0) ||
	    (len >= 4 && strncasecmp(codec, "vp09", 4) == 0)) {
		return AV_CODEC_ID_VP9;
	}

	// AV1
	if ((len >= 3 && strncasecmp(codec, "av1", 3) == 0) ||
	    (len >= 4 && strncasecmp(codec, "av01", 4) == 0)) {
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

	// Session handles (all negative = invalid)
	std::atomic<uint32_t> generation;  // Increments on reconnect
	bool reconnect_in_progress;    // True while reconnect is happening
	int32_t origin;
	int32_t session;
	int32_t consume;
	int32_t catalog_handle;
	int32_t video_track;

	// Decoder state
	AVCodecContext *codec_ctx;
	AVCodecID current_codec_id;            // Currently configured codec
	enum AVPixelFormat current_pix_fmt;    // Current pixel format for sws_ctx
	struct SwsContext *sws_ctx;
	bool got_keyframe;
	uint32_t frames_waiting_for_keyframe;  // Count of skipped frames while waiting
	uint32_t consecutive_decode_errors;    // Count of consecutive decode failures

	// Output frame buffer
	struct obs_source_frame frame;
	uint8_t *frame_buffer;

	// Threading
	pthread_mutex_t mutex;
};

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

	// Set shutdown flag first - callbacks will check this and exit early
	pthread_mutex_lock(&ctx->mutex);
	ctx->shutting_down = true;
	moq_source_disconnect_locked(ctx);
	pthread_mutex_unlock(&ctx->mutex);

	// Give MoQ callbacks time to drain - they check shutting_down and exit early.
	// This prevents use-after-free when async callbacks fire after ctx is freed.
	//
	// LIMITATION: This 100ms sleep is a timing-based workaround, not a synchronization
	// guarantee. If a callback is mid-execution when shutting_down is set AND takes
	// longer than 100ms to complete (after the mutex unlock), there is still a
	// potential race condition. In practice, our callbacks are fast (< 1ms typically)
	// and this delay provides sufficient margin. However, a more robust solution
	// would use reference counting:
	//   - Increment refcount when entering a callback
	//   - Decrement when exiting
	//   - Wait for refcount to reach zero before freeing ctx
	// This could be implemented using std::shared_ptr or a manual atomic refcount
	// with a condition variable for waiting.
	os_sleep_ms(100);

	bfree(ctx->url);
	bfree(ctx->broadcast);
	// Note: frame_buffer is already freed by moq_source_disconnect_locked

	pthread_mutex_destroy(&ctx->mutex);

	bfree(ctx);
}

static void moq_source_update(void *data, obs_data_t *settings)
{
	struct moq_source *ctx = (struct moq_source *)data;

	const char *url = obs_data_get_string(settings, "url");
	const char *broadcast = obs_data_get_string(settings, "broadcast");

	pthread_mutex_lock(&ctx->mutex);

	// Check if settings actually changed
	bool url_changed = (!ctx->url && url && strlen(url) > 0) ||
	                   (ctx->url && !url) ||
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
	bool valid = ctx->url && ctx->broadcast &&
	             strlen(ctx->url) > 0 && strlen(ctx->broadcast) > 0;

	pthread_mutex_unlock(&ctx->mutex);

	// If settings changed and are valid, reconnect
	if (settings_changed && valid) {
		LOG_INFO("Settings changed, reconnecting (url=%s, broadcast=%s)",
		         url ? url : "(null)", broadcast ? broadcast : "(null)");
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

	// Fast path: check atomic flag before taking lock
	if (ctx->shutting_down.load()) {
		LOG_DEBUG("Ignoring session status callback - shutting down");
		return;
	}

	pthread_mutex_lock(&ctx->mutex);
	// Double-check after acquiring lock (may have changed)
	if (ctx->shutting_down.load()) {
		pthread_mutex_unlock(&ctx->mutex);
		return;
	}
	if (ctx->session < 0) {
		LOG_DEBUG("Ignoring session status callback - already disconnected");
		pthread_mutex_unlock(&ctx->mutex);
		return;
	}
	uint32_t current_gen = ctx->generation;

	if (code == 0) {
		pthread_mutex_unlock(&ctx->mutex);
		LOG_INFO("MoQ session connected successfully (generation %u)", current_gen);
		// Now that we're connected, start consuming the broadcast
		moq_source_start_consume(ctx, current_gen);
	} else {
		// Connection failed - clean up the session and origin immediately
		LOG_ERROR("MoQ session failed with code: %d (generation %u)", code, current_gen);

		// Clean up failed session/origin to prevent further callbacks
		if (ctx->session >= 0) {
			moq_session_close(ctx->session);
			ctx->session = -1;
		}
		if (ctx->origin >= 0) {
			moq_origin_close(ctx->origin);
			ctx->origin = -1;
		}
		pthread_mutex_unlock(&ctx->mutex);

		// Blank the video to show error state
		moq_source_blank_video(ctx);
	}
}

static void on_catalog(void *user_data, int32_t catalog)
{
	struct moq_source *ctx = (struct moq_source *)user_data;

	LOG_INFO("Catalog callback received: %d", catalog);

	// Fast path: check atomic flag before taking lock
	if (ctx->shutting_down.load()) {
		LOG_DEBUG("Ignoring catalog callback - shutting down");
		if (catalog >= 0)
			moq_consume_catalog_close(catalog);
		return;
	}

	pthread_mutex_lock(&ctx->mutex);

	// Double-check after acquiring lock (may have changed)
	if (ctx->shutting_down.load()) {
		pthread_mutex_unlock(&ctx->mutex);
		if (catalog >= 0)
			moq_consume_catalog_close(catalog);
		return;
	}

	// Check if this callback is still valid (not from a stale connection)
	uint32_t current_gen = ctx->generation;
	if (ctx->consume < 0) {
		// We've been disconnected, ignore this callback
		pthread_mutex_unlock(&ctx->mutex);
		if (catalog >= 0)
			moq_consume_catalog_close(catalog);
		return;
	}

	pthread_mutex_unlock(&ctx->mutex);

	if (catalog < 0) {
		LOG_ERROR("Failed to get catalog: %d", catalog);
		// Catalog failed (likely invalid broadcast) - blank video
		moq_source_blank_video(ctx);
		return;
	}

	// Get video configuration
	struct moq_video_config video_config;
	if (moq_consume_video_config(catalog, 0, &video_config) < 0) {
		LOG_ERROR("Failed to get video config");
		moq_consume_catalog_close(catalog);
		return;
	}

	// Initialize decoder with the video config (takes mutex internally)
	if (!moq_source_init_decoder(ctx, &video_config)) {
		LOG_ERROR("Failed to initialize decoder");
		moq_consume_catalog_close(catalog);
		return;
	}

	// Subscribe to video track with minimal buffering
	// Note: moq_consume_video_ordered takes the catalog handle, not the consume handle
	int32_t track = moq_consume_video_ordered(catalog, 0, 0, on_video_frame, ctx);
	if (track < 0) {
		LOG_ERROR("Failed to subscribe to video track: %d", track);
		moq_consume_catalog_close(catalog);
		return;
	}

	pthread_mutex_lock(&ctx->mutex);
	if (ctx->generation == current_gen) {
		ctx->video_track = track;
		ctx->catalog_handle = catalog;
	} else {
		// Generation changed while we were setting up, clean up the track
		pthread_mutex_unlock(&ctx->mutex);
		moq_consume_video_close(track);
		moq_consume_catalog_close(catalog);
		return;
	}
	pthread_mutex_unlock(&ctx->mutex);

	LOG_INFO("Subscribed to video track successfully");
}

static void on_video_frame(void *user_data, int32_t frame_id)
{
	struct moq_source *ctx = (struct moq_source *)user_data;

	if (frame_id < 0) {
		LOG_ERROR("Video frame callback with error: %d", frame_id);
		return;
	}

	// Fast path: check atomic flag before taking lock
	if (ctx->shutting_down.load()) {
		moq_consume_frame_close(frame_id);
		return;
	}

	// Check if this callback is still valid using generation (not video_track)
	// Note: We can't check video_track here because frames may arrive before
	// the track handle is stored in on_catalog (race condition)
	pthread_mutex_lock(&ctx->mutex);
	// Double-check after acquiring lock (may have changed)
	if (ctx->shutting_down.load()) {
		pthread_mutex_unlock(&ctx->mutex);
		moq_consume_frame_close(frame_id);
		return;
	}
	if (ctx->consume < 0) {
		// We've been disconnected, ignore this callback
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

	// Small delay to allow MoQ library to fully clean up previous connection
	os_sleep_ms(50);

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

	// Connect to MoQ server (consume will happen in on_session_status callback)
	int32_t new_session = moq_session_connect(
		url_copy, strlen(url_copy),
		0, // origin_publish
		new_origin, // origin_consume
		on_session_status, ctx
	);
	bfree(url_copy);

	if (new_session < 0) {
		LOG_ERROR("Failed to connect to MoQ server: %d", new_session);
		moq_origin_close(new_origin);
		pthread_mutex_lock(&ctx->mutex);
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

	// Subscribe to catalog updates
	int32_t catalog_handle = moq_consume_catalog(consume, on_catalog, ctx);
	if (catalog_handle < 0) {
		LOG_ERROR("Failed to subscribe to catalog for '%s': %d", broadcast_copy, catalog_handle);
		bfree(broadcast_copy);
		// Failed to get catalog - clean up
		pthread_mutex_lock(&ctx->mutex);
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

	LOG_INFO("Consuming broadcast: %s", broadcast_copy);
	bfree(broadcast_copy);
}

// NOTE: Caller must hold ctx->mutex when calling this function
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
		new_codec_ctx->extradata = (uint8_t *)av_mallocz(config->description_len + AV_INPUT_BUFFER_PADDING_SIZE);
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
	ctx->current_pix_fmt = AV_PIX_FMT_NONE;  // Will be set on first frame
	ctx->sws_ctx = NULL;  // Will be created on first frame with actual pixel format
	ctx->frame_buffer = NULL;  // Will be allocated on first frame with actual dimensions
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
	LOG_INFO("Decoder initialized: codec=%s, dimensions=%ux%u (may be refined on first frame)",
	         codec_str, width, height);
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
		if (ctx->frames_waiting_for_keyframe == 1 ||
		    (ctx->frames_waiting_for_keyframe % 30) == 0) {
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
			LOG_INFO("Decoded frame dimensions changed: %ux%u -> %dx%d",
			         ctx->frame.width, ctx->frame.height, frame->width, frame->height);
		}
		if (pix_fmt_changed) {
			LOG_INFO("Decoded frame pixel format changed: %d -> %d (%s)",
			         ctx->current_pix_fmt, decoded_pix_fmt,
			         av_get_pix_fmt_name(decoded_pix_fmt) ? av_get_pix_fmt_name(decoded_pix_fmt) : "unknown");
		}

		// Validate that dimensions are positive and reasonable
		if (frame->width <= 0 || frame->height <= 0 ||
		    frame->width > 16384 || frame->height > 16384) {
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
		struct SwsContext *new_sws_ctx = sws_getContext(
			frame->width, frame->height, decoded_pix_fmt,
			frame->width, frame->height, AV_PIX_FMT_RGBA,
			SWS_BILINEAR, NULL, NULL, NULL
		);
		if (!new_sws_ctx) {
			LOG_ERROR("Failed to create scaling context for %dx%d pix_fmt=%d (%s)",
			          frame->width, frame->height, decoded_pix_fmt,
			          av_get_pix_fmt_name(decoded_pix_fmt) ? av_get_pix_fmt_name(decoded_pix_fmt) : "unknown");
			av_frame_free(&frame);
			pthread_mutex_unlock(&ctx->mutex);
			moq_consume_frame_close(frame_id);
			return;
		}

		// Reallocate frame buffer for new dimensions (width * height * 4 for RGBA)
		size_t new_buffer_size = (size_t)frame->width * (size_t)frame->height * 4;
		uint8_t *new_frame_buffer = (uint8_t *)bmalloc(new_buffer_size);
		if (!new_frame_buffer) {
			LOG_ERROR("Failed to allocate frame buffer for %dx%d (%zu bytes)",
			          frame->width, frame->height, new_buffer_size);
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

		LOG_INFO("Scaler initialized for %dx%d pix_fmt=%s",
		         frame->width, frame->height,
		         av_get_pix_fmt_name(decoded_pix_fmt) ? av_get_pix_fmt_name(decoded_pix_fmt) : "unknown");
	}

	// Convert YUV420P to RGBA
	uint8_t *dst_data[4] = {ctx->frame_buffer, NULL, NULL, NULL};
	int dst_linesize[4] = {static_cast<int>(ctx->frame.width * 4), 0, 0, 0};

	sws_scale(ctx->sws_ctx, (const uint8_t *const *)frame->data, frame->linesize,
	          0, ctx->frame.height, dst_data, dst_linesize);

	// Update OBS frame timestamp and output
	ctx->frame.timestamp = frame_data.timestamp_us;
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
