// SPDX-License-Identifier: GPL-2.0-or-later
#include <obs.hpp>

#include "moq-output.h"
#include "moq-settings.h"
#include "moq-url.h"
#include "logger.h"
#include "util/util_uint64.h"

#include <cstring>
#include <string>

extern "C" {
#include "moq.h"
}

namespace {

bool LooksGenericOffline(const std::string &reason)
{
	if (reason.empty() || reason == "offline")
		return true;
	// Case-insensitive contains for "offline" only as a whole-ish token.
	for (size_t i = 0; i + 6 < reason.size() + 1; i++) {
		char buf[8] = {};
		for (int j = 0; j < 7 && i + j < reason.size(); j++) {
			char c = reason[i + j];
			buf[j] = (c >= 'A' && c <= 'Z') ? static_cast<char>(c - 'A' + 'a') : c;
		}
		if (std::strncmp(buf, "offline", 7) == 0)
			return true;
	}
	return false;
}

bool IsAuthFailure(int code, const std::string &reason)
{
	if (code == -34 || code == -35)
		return true;
	std::string lower = reason;
	for (char &c : lower) {
		if (c >= 'A' && c <= 'Z')
			c = static_cast<char>(c - 'A' + 'a');
	}
	return lower.find("unauthorized") != std::string::npos || lower.find("forbidden") != std::string::npos;
}

// Dial URL scheme only. https races WebTransport vs WebSocket in moq-native;
// libmoq does not yet expose which won, so do not invent a transport name here.
std::string DialSchemeLabel(const std::string &url)
{
	const auto colon = url.find(':');
	if (colon == std::string::npos || colon == 0)
		return {};
	std::string scheme = url.substr(0, colon);
	for (char &c : scheme) {
		if (c >= 'A' && c <= 'Z')
			c = static_cast<char>(c - 'A' + 'a');
	}
	if (scheme == "moqt" || scheme == "moql" || scheme == "quic")
		return "quic";
	if (scheme == "tcp")
		return "tcp";
	if (scheme == "unix")
		return "unix";
	return scheme;
}

} // namespace

MoQOutput::MoQOutput(obs_data_t *, obs_output_t *output)
	: output(output),
	  state(std::make_shared<SessionState>(output)),
	  path(),
	  total_bytes_sent(0),
	  origin(moq_origin_create()),
	  broadcast(0)
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

	// Give up the frontend. A terminal callback still in flight holds its own
	// reference to the shared state, so nothing here waits on the libmoq runtime:
	// whenever it arrives it finds the state detached and reports nothing.
	state->Detach();
}

void MoQOutput::SessionState::Detach()
{
	// A callback that has already decided to report parks here, so this returns
	// only once nothing is inside an OBS call and the output can be freed.
	std::lock_guard<std::recursive_mutex> signal_lock(signal_mutex);
	output = nullptr;
}

void MoQOutput::SessionState::SignalStop(int code)
{
	std::lock_guard<std::recursive_mutex> signal_lock(signal_mutex);
	if (output)
		obs_output_signal_stop(output, code);
}

bool MoQOutput::Start()
{
	// OBS restarts a reconnecting output by calling start again with no stop in
	// between, so drop whatever the previous attempt left behind.
	Reset();

	obs_service_t *service = obs_output_get_service(output);
	if (!service) {
		LOG_ERROR("Failed to get service from output");
		state->SignalStop(OBS_OUTPUT_ERROR);
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
	const std::string url = server_value ? server_value : "";
	if (url.empty()) {
		LOG_ERROR("Server URL is empty");
		state->SignalStop(OBS_OUTPUT_BAD_PATH);
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

	// Advanced settings live on the service alongside the URL and path. With the group
	// switched off Pointer() is NULL, which dials with the library defaults. The config
	// borrows its strings, so it has to outlive the connect below.
	OBSDataAutoRelease service_settings = obs_service_get_settings(service);
	MoQSettings::Config client;
	if (!MoQSettings::BuildConfig(service_settings, &client)) {
		// BuildConfig logged why. Refusing to start beats connecting with a setting
		// the user asked for quietly dropped.
		obs_output_set_last_error(output, "Invalid advanced MoQ settings; see the log for details.");
		state->SignalStop(OBS_OUTPUT_CONNECT_FAILED);
		return false;
	}

	LOG_INFO("Connecting to MoQ server: %s", MoQRedactUrl(url).c_str());

	// Held from the connect through obs_output_begin_data_capture. The status
	// callback takes the same lock before reporting anything, so this attempt's
	// terminal cannot signal a failure against an output that is only half
	// started: it waits until the output is committed, and OBS then handles the
	// stop through its normal active-output path.
	std::lock_guard<std::recursive_mutex> signal_lock(state->signal_mutex);

	uint64_t attempt;
	{
		std::lock_guard<std::mutex> lock(state->mutex);
		attempt = state->attempt;
		state->url = url;
	}

	// Everything the callbacks need about this attempt, copied so they never read
	// a member the next Start() may be rewriting, plus the reference that keeps
	// the shared state alive. Freed by the terminal callback, so it outlives this
	// scope.
	auto ref = new SessionRef{state, attempt, url, std::chrono::steady_clock::now()};

	// Start establishing a session with the MoQ server
	// NOTE: You could publish the same broadcasts to multiple sessions if you want (redundant ingest).
	int handle =
		moq_session_connect(url.data(), url.size(), client.Pointer(), origin, 0, MoQOutput::SessionStatus, ref);

	if (handle < 0) {
		const char *reason = moq_error();
		LOG_ERROR("Failed to initialize MoQ server: %d: %s", handle, reason ? reason : "unknown error");
		obs_output_set_last_error(output, reason ? reason : "Failed to initialize MoQ connection");
		// No subscription was created, so no terminal will fire; drop the reference.
		delete ref;
		return false;
	}

	bool superseded;
	{
		std::lock_guard<std::mutex> lock(state->mutex);
		// Holding signal_mutex keeps this attempt current: libmoq delivers the
		// terminal on its runtime thread, which parks in SessionState::Closed until
		// we commit. The check stands guard over that assumption rather than
		// trusting libmoq's threading to stay as it is.
		superseded = state->attempt != attempt;
		if (!superseded)
			state->session = handle;
	}

	if (superseded) {
		// The session died during connect without going through the callback's
		// signal path. Refuse the start rather than capturing into a dead session;
		// OBS surfaces the last error we recorded when info.start returns false.
		LOG_ERROR("MoQ session failed before the output started: %s", MoQRedactUrl(url).c_str());
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
	std::lock_guard<std::recursive_mutex> signal_lock(state->signal_mutex);

	Reset();

	if (signal) {
		state->SignalStop(OBS_OUTPUT_SUCCESS);
	}
}

void MoQOutput::Reset()
{
	// Excludes the status callback's report: retiring the attempt and signalling
	// a failure must not interleave, or a stop that already happened gets followed
	// by a failure OBS turns into a reconnect.
	std::lock_guard<std::recursive_mutex> signal_lock(state->signal_mutex);

	int stale;
	{
		std::lock_guard<std::mutex> lock(state->mutex);
		// Retire the attempt so an in-flight terminal callback stays out of the way.
		state->attempt++;
		state->connected = false;
		state->connect_time_ms = 0;
		state->epoch = 0;
		state->url.clear();
		state->last_failure_code = 0;
		state->last_failure_reason.clear();
		stale = state->session;
		state->session = 0;
	}

	// Outside the lock: the status callback takes state->mutex, and libmoq is
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

bool MoQOutput::TryGetConnectionStats(ConnectionStats *out)
{
	if (!out)
		return false;

	uint32_t handle;
	uint64_t attempt;
	std::string dialUrl;
	{
		std::lock_guard<std::mutex> lock(state->mutex);
		if (state->session == 0)
			return false;
		handle = static_cast<uint32_t>(state->session);
		attempt = state->attempt;
		dialUrl = state->url;
	}

	moq_connection_snapshot connection{};
	const int32_t rc = moq_session_snapshot(handle, &connection);
	if (rc != 0) {
		const char *message = moq_error();
		const std::string next = message && *message ? message : "offline";
		std::lock_guard<std::mutex> lock(state->mutex);
		if (state->attempt != attempt)
			return false;
		// Keep a more specific prior failure (unauthorized) over a generic offline blip.
		if (state->last_failure_reason.empty() || LooksGenericOffline(state->last_failure_reason) ||
		    !LooksGenericOffline(next) || IsAuthFailure(rc, next)) {
			state->last_failure_code = rc;
			state->last_failure_reason = next;
		}
		return false;
	}

	const auto &raw = connection.stats;
	ConnectionStats snapshot;
	snapshot.reconnects = GetReconnectCount();
	snapshot.rtt_valid = raw.rtt_valid;
	snapshot.rtt_ms = raw.rtt_valid ? static_cast<double>(raw.rtt_us) / 1000.0 : 0;
	snapshot.send_rate_valid = raw.send_rate_valid;
	snapshot.send_rate_bps = raw.send_rate_valid ? static_cast<double>(raw.send_rate_bps) : 0;
	snapshot.recv_rate_valid = raw.recv_rate_valid;
	snapshot.recv_rate_bps = raw.recv_rate_valid ? static_cast<double>(raw.recv_rate_bps) : 0;
	snapshot.bytes_sent_valid = raw.bytes_sent_valid;
	snapshot.bytes_sent = raw.bytes_sent_valid ? raw.bytes_sent : 0;
	if (raw.packets_sent_valid && raw.packets_lost_valid && raw.packets_sent > 0) {
		snapshot.loss_valid = true;
		snapshot.loss_pct =
			100.0 * static_cast<double>(raw.packets_lost) / static_cast<double>(raw.packets_sent);
	}
	snapshot.dial = DialSchemeLabel(dialUrl);

	snapshot.protocol.assign(connection.protocol.data, connection.protocol.len);

	{
		std::lock_guard<std::mutex> lock(state->mutex);
		if (state->attempt != attempt || static_cast<uint32_t>(state->session) != handle)
			return false;
		*out = std::move(snapshot);
	}

	return true;
}

bool MoQOutput::IsLiveSession()
{
	std::lock_guard<std::mutex> lock(state->mutex);
	return state->connected && state->session != 0;
}

void MoQOutput::CopyLastFailure(int *code, std::string *reason)
{
	std::lock_guard<std::mutex> lock(state->mutex);
	if (code)
		*code = state->last_failure_code;
	if (reason)
		*reason = state->last_failure_reason;
}

// libmoq status codes (>= 0.3.0): > 0 = (re)connected, carrying the connection
// epoch; 0 = closed cleanly (terminal); < 0 = fatal, reconnection gave up (terminal).
void MoQOutput::SessionStatus(void *user_data, int code)
{
	auto ref = static_cast<SessionRef *>(user_data);

	if (code > 0) {
		ref->state->Connected(*ref, code);
		return;
	}

	// Terminal: libmoq never touches user_data again, so we own the reference.
	// Destroying it drops the last hold on the shared state when the output is
	// already gone, so this is where a detached state is finally freed.
	std::unique_ptr<SessionRef> owned(ref);
	owned->state->Closed(*owned, code);
}

void MoQOutput::SessionState::Connected(const SessionRef &ref, int connect_epoch)
{
	auto elapsed = std::chrono::steady_clock::now() - ref.started;
	auto ms = static_cast<int>(std::chrono::duration_cast<std::chrono::milliseconds>(elapsed).count());

	{
		std::lock_guard<std::mutex> lock(mutex);
		if (attempt != ref.attempt)
			return;
		connected = true;
		// OBS and older dock paths treat connect_time_ms == 0 as "never connected".
		// Clamp sub-millisecond connects to 1 so that sentinel stays honest.
		connect_time_ms = ms > 0 ? ms : 1;
		epoch = connect_epoch;
		last_failure_code = 0;
		last_failure_reason.clear();
	}

	LOG_INFO("MoQ session connected (%d ms, epoch %d): %s", ms, connect_epoch, MoQRedactUrl(ref.url).c_str());
}

void MoQOutput::SessionState::Closed(const SessionRef &ref, int code)
{
	// moq_error() only describes the most recent call on this thread, so copy the
	// reason out before anything below can overwrite it.
	std::string reason;
	if (code < 0) {
		const char *message = moq_error();
		reason = message ? message : "unknown error";
	}

	// Held across both the decision below and the signal itself. Taking it before
	// mutex is the required lock order, and holding it through the signal is what
	// stops a teardown from landing in between: without that, Stop() can retire
	// the attempt after we decide to report, and OBS turns the late
	// OBS_OUTPUT_DISCONNECTED into a reconnect of a stopped stream. It is also
	// what Detach() waits on, so `output` cannot go away mid-report.
	std::lock_guard<std::recursive_mutex> signal_lock(signal_mutex);

	bool current;
	bool was_connected = false;
	{
		std::lock_guard<std::mutex> lock(mutex);
		current = attempt == ref.attempt;
		if (current) {
			was_connected = connected;
			// The session task has ended and dropped the handle, so retire it here
			// rather than closing a handle libmoq no longer knows about.
			session = 0;
			attempt++;
			connected = false;
			connect_time_ms = 0;
			epoch = 0;
			if (code < 0) {
				last_failure_code = code;
				last_failure_reason = reason;
			}
		}
	}

	if (code == 0) {
		LOG_INFO("MoQ session closed: %s", MoQRedactUrl(ref.url).c_str());
	} else {
		LOG_ERROR("MoQ session failed (%d): %s: %s", code, MoQRedactUrl(ref.url).c_str(), reason.c_str());
	}

	// Reconnection gave up, so nothing is reaching the server any more. Without
	// this OBS keeps encoding and reporting the stream as live forever. Only the
	// current attempt signals, which is what keeps it to one signal per Start().
	// A detached output has already been destroyed, so there is nobody to tell.
	if (code < 0 && current && output) {
		obs_output_set_last_error(output, reason.c_str());
		// CONNECT_FAILED is terminal for OBS. DISCONNECTED lets its own reconnect
		// logic retry, which is only worth offering once we know the server works.
		obs_output_signal_stop(output, was_connected ? OBS_OUTPUT_DISCONNECTED : OBS_OUTPUT_CONNECT_FAILED);
	}
}

void MoQOutput::Data(struct encoder_packet *packet)
{
	if (!packet) {
		// One report for the pair, so a session failure can't slip between the
		// teardown and the encode error and report a second time.
		std::lock_guard<std::recursive_mutex> signal_lock(state->signal_mutex);
		Stop(false);
		state->SignalStop(OBS_OUTPUT_ENCODE_ERROR);
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

	// Audio has no keyframes, so it has no group boundary of its own: without this the whole
	// stream is one group. Cut per frame, which is one QUIC stream per packet forwarded without
	// waiting for the next, the right trade for live. Video groups at its own keyframes.
	result = moq_publish_media_cut(handle);
	if (result < 0) {
		LOG_ERROR("Failed to cut audio group: %d", result);
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

	OBSDataAutoRelease settings = obs_encoder_get_settings(encoder);
	const auto video_width = obs_encoder_get_width(encoder);
	const auto video_height = obs_encoder_get_height(encoder);
	const int video_bitrate_kbps = settings ? (int)obs_data_get_int(settings, "bitrate") : 0;

	uint8_t *extra_data = nullptr;
	size_t extra_size = 0;

	// obs_encoder_get_extra_data may only return data after the first frame has been encoded.
	// For H.264, this returns the SPS/PPS
	if (!obs_encoder_get_extra_data(encoder, &extra_data, &extra_size)) {
		LOG_WARNING("Failed to get extra data");
	}

	const char *codec = obs_encoder_get_codec(encoder);

	// Map the OBS codec name onto a MoQ format. Both H.26x entries are the Annex-B framing
	// with inline parameter sets, which is what OBS hands us.
	moq_video_init config{};
	if (strcmp(codec, "h264") == 0) {
		config.format = MOQ_VIDEO_FORMAT_AVC3;
	} else if (strcmp(codec, "hevc") == 0) {
		config.format = MOQ_VIDEO_FORMAT_HEV1;
	} else if (strcmp(codec, "av1") == 0) {
		config.format = MOQ_VIDEO_FORMAT_AV01;
	} else {
		LOG_ERROR("Unsupported video codec: %s", codec);
		return;
	}

	config.init = extra_data;
	config.init_len = extra_size;

	// Seed catalog fields a downstream moq-transcode needs before measured rates
	// arrive: coded size (also from SPS once parsed) and configured CBR bitrate so
	// same-height ladder rungs can undercut the mezzanine.
	if (video_width > 0 && video_height > 0) {
		config.hint.coded_width = video_width;
		config.hint.coded_height = video_height;
		config.hint.has_coded = true;
	}
	const std::string rate_control = settings ? obs_data_get_string(settings, "rate_control") : "";
	if (video_bitrate_kbps > 0 && (rate_control == "CBR" || rate_control == "cbr")) {
		config.hint.bitrate = (uint64_t)video_bitrate_kbps * 1000ULL;
		config.hint.has_bitrate = true;
	}
	config.hint.optimize_for_latency = true;
	config.hint.has_optimize_for_latency = true;

	int handle = moq_publish_video(broadcast, &config);
	video_tracks[encoder] = handle;
	if (handle < 0) {
		LOG_ERROR("Failed to initialize video track: %d", handle);
		return;
	}

	LOG_INFO("Video track initialized (%ux%u, %d kbps)", video_width, video_height, video_bitrate_kbps);
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

	// The codec string used to go straight through, so an unsupported one failed deep in the
	// importer. Mapping it here means OBS says which codec it was.
	moq_audio_init config{};
	if (strcmp(codec, "opus") == 0) {
		config.format = MOQ_AUDIO_FORMAT_OPUS;
	} else if (strcmp(codec, "aac") == 0) {
		config.format = MOQ_AUDIO_FORMAT_AAC;
	} else if (strcmp(codec, "flac") == 0) {
		config.format = MOQ_AUDIO_FORMAT_FLAC;
	} else {
		LOG_ERROR("Unsupported audio codec: %s", codec);
		return;
	}

	config.init = extra_data;
	config.init_len = extra_size;
	int handle = moq_publish_audio(broadcast, &config);
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
