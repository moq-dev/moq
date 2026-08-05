// SPDX-License-Identifier: GPL-2.0-or-later
#include <obs.hpp>

#include "moq-output.h"
#include "util/util_uint64.h"

extern "C" {
#include "moq.h"
}

MoQOutput::MoQOutput(obs_data_t *, obs_output_t *output)
	: output(output),
	  server_url(),
	  path(),
	  total_bytes_sent(0),
	  connect_time_ms(0),
	  origin(moq_origin_create()),
	  broadcast(0),
	  outstanding_sessions(0),
	  session(0),
	  session_attempt(0),
	  session_connected(false)
{
}

MoQOutput::~MoQOutput()
{
	// Retires the attempt, so a terminal callback that is still in flight won't
	// signal a stop on an output that is going away.
	Stop();

	// Reset() closed the session and finished the tracks and the broadcast, so
	// the origin has no children left.
	moq_origin_close(origin);

	// Wait for any outstanding session terminal callback to fire before `this`
	// is freed, so a late callback on the libmoq runtime thread can't touch freed
	// memory. Bounded so a missing terminal degrades to a warning, not a hang.
	std::unique_lock<std::mutex> lock(session_mutex);
	if (!session_cv.wait_for(lock, std::chrono::seconds(2), [this] { return outstanding_sessions == 0; }))
		LOG_WARNING("Output teardown timed out with %d MoQ session callback(s) outstanding",
			    outstanding_sessions);
}

bool MoQOutput::Start()
{
	// OBS restarts a reconnecting output by calling start again with no stop in
	// between, so drop whatever the previous attempt left behind.
	Reset();

	obs_service_t *service = obs_output_get_service(output);
	if (!service) {
		LOG_ERROR("Failed to get service from output");
		SignalStop(OBS_OUTPUT_ERROR);
		return false;
	}

	if (!obs_output_can_begin_data_capture(output, 0)) {
		LOG_ERROR("Cannot begin data capture");
		return false;
	}

	if (!obs_output_initialize_encoders(output, 0)) {
		LOG_ERROR("Failed to initialize encoders");
		return false;
	}

	const char *server_value = obs_service_get_connect_info(service, OBS_SERVICE_CONNECT_INFO_SERVER_URL);
	server_url = server_value ? server_value : "";
	if (server_url.empty()) {
		LOG_ERROR("Server URL is empty");
		SignalStop(OBS_OUTPUT_BAD_PATH);
		return false;
	}

	// Path (broadcast name) is optional; an empty string publishes to the unnamed broadcast.
	const char *path_value = obs_service_get_connect_info(service, OBS_SERVICE_CONNECT_INFO_STREAM_KEY);
	path = path_value ? path_value : "";

	bool found_encoder = false;
	for (uint32_t idx = 0; idx < MAX_OUTPUT_VIDEO_ENCODERS; idx++) {
		if (obs_output_get_video_encoder2(output, idx)) {
			found_encoder = true;
			break;
		}
	}

	if (!found_encoder) {
		LOG_ERROR("Failed to get video encoder");
		return false;
	}

	LOG_INFO("Connecting to MoQ server: %s", server_url.c_str());

	// Held from the connect through obs_output_begin_data_capture. The status
	// callback takes the same lock before reporting anything, so this attempt's
	// terminal cannot signal a failure against an output that is only half
	// started: it waits until the output is committed, and OBS then handles the
	// stop through its normal active-output path.
	std::lock_guard<std::recursive_mutex> signal_lock(signal_mutex);

	uint64_t attempt;
	{
		std::lock_guard<std::mutex> lock(session_mutex);
		attempt = session_attempt;

		// Pre-account for the session subscription before handing the reference to
		// libmoq: the connection can fail and fire its terminal callback before
		// connect returns, and the destructor must wait for that callback.
		outstanding_sessions++;
	}

	// Everything the callbacks need about this attempt, copied so they never read
	// a member the next Start() may be rewriting. Freed by the terminal callback,
	// so it outlives this scope.
	auto ref = new SessionRef{this, attempt, server_url, std::chrono::steady_clock::now()};

	// Start establishing a session with the MoQ server
	// NOTE: You could publish the same broadcasts to multiple sessions if you want (redundant ingest).
	int handle =
		moq_session_connect(server_url.data(), server_url.size(), origin, 0, MoQOutput::SessionStatus, ref);
	if (handle < 0) {
		const char *reason = moq_error();
		LOG_ERROR("Failed to initialize MoQ server: %d: %s", handle, reason ? reason : "unknown error");
		delete ref;
		// No subscription was created, so no terminal will fire; undo the ref.
		std::lock_guard<std::mutex> lock(session_mutex);
		if (--outstanding_sessions == 0)
			session_cv.notify_all();
		return false;
	}

	bool superseded;
	{
		std::lock_guard<std::mutex> lock(session_mutex);
		// Holding signal_mutex keeps this attempt current: libmoq delivers the
		// terminal on its runtime thread, which parks in SessionClosed until we
		// commit. The check stands guard over that assumption rather than trusting
		// libmoq's threading to stay as it is.
		superseded = session_attempt != attempt;
		if (!superseded)
			session = handle;
	}

	if (superseded) {
		// The session died during connect without going through the callback's
		// signal path. Refuse the start rather than capturing into a dead session;
		// OBS surfaces the last error we recorded when info.start returns false.
		LOG_ERROR("MoQ session failed before the output started: %s", server_url.c_str());
		return false;
	}

	LOG_INFO("Publishing broadcast: %s", path.c_str());

	// Create the broadcast on the origin we created; it starts live so the session
	// announces it. Stop() finishes it, so each Start creates a fresh one.
	broadcast = moq_origin_publish(origin, path.data(), path.size());
	if (broadcast < 0) {
		LOG_ERROR("Failed to publish broadcast to session: %d", broadcast);
		broadcast = 0;
		// The session connected above; close it so a retry on this same output
		// doesn't reuse the stale handle. Its terminal callback releases the
		// outstanding-session reference the destructor waits on.
		Stop(false);
		return false;
	}

	obs_output_begin_data_capture(output, 0);

	return true;
}

void MoQOutput::Stop(bool signal)
{
	std::lock_guard<std::recursive_mutex> signal_lock(signal_mutex);

	Reset();

	if (signal) {
		obs_output_signal_stop(output, OBS_OUTPUT_SUCCESS);
	}
}

void MoQOutput::SignalStop(int code)
{
	std::lock_guard<std::recursive_mutex> signal_lock(signal_mutex);
	obs_output_signal_stop(output, code);
}

void MoQOutput::Reset()
{
	// Excludes the status callback's report: retiring the attempt and signalling
	// a failure must not interleave, or a stop that already happened gets followed
	// by a failure OBS turns into a reconnect.
	std::lock_guard<std::recursive_mutex> signal_lock(signal_mutex);

	int stale;
	{
		std::lock_guard<std::mutex> lock(session_mutex);
		// Retire the attempt so an in-flight terminal callback stays out of the way.
		session_attempt++;
		session_connected = false;
		connect_time_ms = 0;
		stale = session;
		session = 0;
	}

	// Outside the lock: the status callback takes session_mutex, and libmoq is
	// free to run it on its own thread while we're here.
	if (stale > 0)
		moq_session_close(stale);

	for (auto &[encoder, handle] : video_tracks) {
		if (handle > 0)
			moq_publish_media_finish(handle);
	}
	video_tracks.clear();

	for (auto &[encoder, handle] : audio_tracks) {
		if (handle > 0)
			moq_publish_media_finish(handle);
	}
	audio_tracks.clear();

	// Finish the broadcast so the origin unpublishes it immediately; Start()
	// creates a fresh one on restart.
	if (broadcast > 0) {
		moq_publish_finish(broadcast);
		broadcast = 0;
	}
}

// libmoq status codes (>= 0.3.0): > 0 = (re)connected, carrying the connection
// epoch; 0 = closed cleanly (terminal); < 0 = fatal, reconnection gave up (terminal).
void MoQOutput::SessionStatus(void *user_data, int code)
{
	auto ref = static_cast<SessionRef *>(user_data);

	if (code > 0) {
		ref->output->SessionConnected(*ref, code);
		return;
	}

	// Terminal: libmoq never touches user_data again, so we own the reference.
	// SessionClosed is the last access to the output, but not to `ref`.
	std::unique_ptr<SessionRef> owned(ref);
	owned->output->SessionClosed(*owned, code);
}

void MoQOutput::SessionConnected(const SessionRef &ref, int epoch)
{
	auto elapsed = std::chrono::steady_clock::now() - ref.started;
	auto ms = static_cast<int>(std::chrono::duration_cast<std::chrono::milliseconds>(elapsed).count());

	{
		std::lock_guard<std::mutex> lock(session_mutex);
		if (session_attempt != ref.attempt)
			return;
		session_connected = true;
		// The dock reads this as the live connected indicator, so publish it under
		// the same lock as the stamp check: a restart must not be able to clear it
		// between the two and leave the old attempt's time showing.
		connect_time_ms = ms;
	}

	LOG_INFO("MoQ session connected (%d ms, epoch %d): %s", ms, epoch, ref.url.c_str());
}

void MoQOutput::SessionClosed(const SessionRef &ref, int code)
{
	// moq_error() only describes the most recent call on this thread, so copy the
	// reason out before anything below can overwrite it.
	std::string reason;
	if (code < 0) {
		const char *message = moq_error();
		reason = message ? message : "unknown error";
	}

	// Held across both the decision below and the signal itself. Taking it before
	// session_mutex is the required lock order, and holding it through the signal
	// is what stops a teardown from landing in between: without that, Stop() can
	// retire the attempt after we decide to report, and OBS turns the late
	// OBS_OUTPUT_DISCONNECTED into a reconnect of a stopped stream.
	std::unique_lock<std::recursive_mutex> signal_lock(signal_mutex);

	bool current;
	bool connected = false;
	{
		std::lock_guard<std::mutex> lock(session_mutex);
		current = session_attempt == ref.attempt;
		if (current) {
			connected = session_connected;
			// The session task has ended and dropped the handle, so retire it here
			// rather than closing a handle libmoq no longer knows about.
			session = 0;
			session_attempt++;
			session_connected = false;
			connect_time_ms = 0;
		}
	}

	if (code == 0) {
		LOG_INFO("MoQ session closed: %s", ref.url.c_str());
	} else {
		LOG_ERROR("MoQ session failed (%d): %s: %s", code, ref.url.c_str(), reason.c_str());
	}

	// Reconnection gave up, so nothing is reaching the server any more. Without
	// this OBS keeps encoding and reporting the stream as live forever. Only the
	// current attempt signals, which is what keeps it to one signal per Start().
	// `this` and `output` are still alive here because the destructor is blocked
	// on the reference released below.
	if (code < 0 && current) {
		obs_output_set_last_error(output, reason.c_str());
		// CONNECT_FAILED is terminal for OBS. DISCONNECTED lets its own reconnect
		// logic retry, which is only worth offering once we know the server works.
		obs_output_signal_stop(output, connected ? OBS_OUTPUT_DISCONNECTED : OBS_OUTPUT_CONNECT_FAILED);
	}

	// Drop it before the wait below: the destructor blocks on session_cv holding
	// session_mutex, and its Reset() wants signal_mutex.
	signal_lock.unlock();

	// Terminal callback: the session task has ended and will not touch `this`
	// again. Release the lifetime reference the destructor waits on. This is the
	// last access to `this`.
	std::lock_guard<std::mutex> lock(session_mutex);
	if (--outstanding_sessions == 0)
		session_cv.notify_all();
}

void MoQOutput::Data(struct encoder_packet *packet)
{
	if (!packet) {
		// One report for the pair, so a session failure can't slip between the
		// teardown and the encode error and report a second time.
		std::lock_guard<std::recursive_mutex> signal_lock(signal_mutex);
		Stop(false);
		obs_output_signal_stop(output, OBS_OUTPUT_ENCODE_ERROR);
		return;
	}

	if (packet->type == OBS_ENCODER_AUDIO) {
		AudioData(packet);
	} else if (packet->type == OBS_ENCODER_VIDEO) {
		VideoData(packet);
	}
}

void MoQOutput::AudioData(struct encoder_packet *packet)
{
	obs_encoder_t *encoder = packet->encoder;

	auto it = audio_tracks.find(encoder);
	if (it == audio_tracks.end()) {
		AudioInit(encoder);
		it = audio_tracks.find(encoder);
	}
	if (it == audio_tracks.end() || it->second < 0) {
		// We failed to initialize the audio track, so we can't write any data.
		return;
	}
	int handle = it->second;

	// Add ~1 second offset to handle negative PTS from audio priming frames.
	// TODO: This is slightly wrong when den is not evenly divisible by num, but close enough.
	int64_t pts = packet->pts + packet->timebase_den / packet->timebase_num;
	if (pts < 0) {
		LOG_WARNING("Dropping audio frame with negative PTS: %lld", (long long)packet->pts);
		return;
	}

	auto pts_us = util_mul_div64(pts, 1000000ULL * packet->timebase_num, packet->timebase_den);

	auto result = moq_publish_media_frame(handle, packet->data, packet->size, pts_us);
	if (result < 0) {
		LOG_ERROR("Failed to write audio frame: %d", result);
		return;
	}

	total_bytes_sent += packet->size;
}

void MoQOutput::VideoData(struct encoder_packet *packet)
{
	obs_encoder_t *encoder = packet->encoder;

	auto it = video_tracks.find(encoder);
	if (it == video_tracks.end()) {
		VideoInit(encoder);
		it = video_tracks.find(encoder);
	}
	if (it == video_tracks.end() || it->second < 0)
		return;
	int handle = it->second;

	// Add ~1 second offset to match audio for A/V sync.
	// TODO: This is slightly wrong when den is not evenly divisible by num, but close enough.
	int64_t pts = packet->pts + packet->timebase_den / packet->timebase_num;
	if (pts < 0) {
		LOG_WARNING("Dropping video frame with negative PTS: %lld", (long long)packet->pts);
		return;
	}

	auto pts_us = util_mul_div64(pts, 1000000ULL * packet->timebase_num, packet->timebase_den);

	auto result = moq_publish_media_frame(handle, packet->data, packet->size, pts_us);
	if (result < 0) {
		LOG_ERROR("Failed to write video frame: %d", result);
		return;
	}

	total_bytes_sent += packet->size;
}

void MoQOutput::VideoInit(obs_encoder_t *encoder)
{
	if (!encoder) {
		LOG_ERROR("Failed to get video encoder");
		return;
	}

	// TODO Pass these along to the video catalog somehow.
	/*
	OBSDataAutoRelease settings = obs_encoder_get_settings(encoder);
	if (!settings) {
		LOG_ERROR("Failed to get video encoder settings");
		return;
	}

	auto video_bitrate = (int)obs_data_get_int(settings, "bitrate");
	auto video_width = obs_encoder_get_width(encoder);
	auto video_height = obs_encoder_get_height(encoder);
	*/

	uint8_t *extra_data = nullptr;
	size_t extra_size = 0;

	// obs_encoder_get_extra_data may only return data after the first frame has been encoded.
	// For H.264, this returns the SPS/PPS
	if (!obs_encoder_get_extra_data(encoder, &extra_data, &extra_size)) {
		LOG_WARNING("Failed to get extra data");
	}

	const char *codec = obs_encoder_get_codec(encoder);

	// Transform codec string for MoQ
	const char *moq_codec = codec;
	if (strcmp(codec, "h264") == 0) {
		// H.264 with inline SPS/PPS
		moq_codec = "avc3";
	} else if (strcmp(codec, "hevc") == 0) {
		// H.265 with inline VPS/SPS/PPS
		moq_codec = "hev1";
	}

	// Intialize the media import module with the codec and initialization data.
	int handle = moq_publish_media(broadcast, moq_codec, strlen(moq_codec), extra_data, extra_size);
	video_tracks[encoder] = handle;
	if (handle < 0) {
		LOG_ERROR("Failed to initialize video track: %d", handle);
		return;
	}

	LOG_INFO("Video track initialized successfully");
}

void MoQOutput::AudioInit(obs_encoder_t *encoder)
{
	if (!encoder) {
		LOG_ERROR("Failed to get audio encoder");
		return;
	}

	// TODO Pass these along to the audio catalog somehow.
	/*
	OBSDataAutoRelease settings = obs_encoder_get_settings(encoder);
	if (!settings) {
		LOG_ERROR("Failed to get audio encoder settings");
		return;
	}

	auto audio_bitrate = (int)obs_data_get_int(settings, "bitrate");
	*/

	uint8_t *extra_data = nullptr;
	size_t extra_size = 0;

	// obs_encoder_get_extra_data may only return data after the first frame has been encoded.
	// For AAC, this returns 2 bytes containing the profile and the sample rate.
	if (!obs_encoder_get_extra_data(encoder, &extra_data, &extra_size)) {
		LOG_WARNING("Failed to get extra data");
	}

	const char *codec = obs_encoder_get_codec(encoder);

	int handle = moq_publish_media(broadcast, codec, strlen(codec), extra_data, extra_size);
	audio_tracks[encoder] = handle;
	if (handle < 0) {
		LOG_ERROR("Failed to initialize audio track: %d", handle);
		return;
	}

	LOG_INFO("Audio track initialized successfully");
}

void register_moq_output()
{
	const uint32_t base_flags = OBS_OUTPUT_ENCODED | OBS_OUTPUT_SERVICE | OBS_OUTPUT_MULTI_TRACK_VIDEO |
				    OBS_OUTPUT_MULTI_TRACK_AUDIO;

	const char *audio_codecs = "aac;opus";
	const char *video_codecs = "h264;hevc;av1";

	struct obs_output_info info = {};
	info.id = "moq_output";
	info.flags = OBS_OUTPUT_AV | base_flags;
	info.get_name = [](void *) -> const char * {
		return "MoQ Output";
	};
	info.create = [](obs_data_t *settings, obs_output_t *output) -> void * {
		return new MoQOutput(settings, output);
	};
	info.destroy = [](void *priv_data) {
		delete static_cast<MoQOutput *>(priv_data);
	};
	info.start = [](void *priv_data) -> bool {
		return static_cast<MoQOutput *>(priv_data)->Start();
	};
	info.stop = [](void *priv_data, uint64_t) {
		static_cast<MoQOutput *>(priv_data)->Stop();
	};
	info.encoded_packet = [](void *priv_data, struct encoder_packet *packet) {
		static_cast<MoQOutput *>(priv_data)->Data(packet);
	};
	info.get_total_bytes = [](void *priv_data) -> uint64_t {
		return (uint64_t)static_cast<MoQOutput *>(priv_data)->GetTotalBytes();
	};
	info.get_connect_time_ms = [](void *priv_data) -> int {
		return static_cast<MoQOutput *>(priv_data)->GetConnectTime();
	};
	info.encoded_video_codecs = video_codecs;
	info.encoded_audio_codecs = audio_codecs;
	info.protocols = "MoQ";

	obs_register_output(&info);

	info.id = "moq_output_video";
	info.flags = OBS_OUTPUT_VIDEO | base_flags;
	info.encoded_audio_codecs = nullptr;
	obs_register_output(&info);

	info.id = "moq_output_audio";
	info.flags = OBS_OUTPUT_AUDIO | base_flags;
	info.encoded_video_codecs = nullptr;
	info.encoded_audio_codecs = audio_codecs;
	obs_register_output(&info);
}
