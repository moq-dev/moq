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
	  session(0),
	  broadcast(moq_publish_create()),
	  outstanding_sessions(0)
{
}

MoQOutput::~MoQOutput()
{
	moq_publish_close(broadcast);
	moq_origin_close(origin);

	Stop();

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
	obs_service_t *service = obs_output_get_service(output);
	if (!service) {
		LOG_ERROR("Failed to get service from output");
		obs_output_signal_stop(output, OBS_OUTPUT_ERROR);
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
		obs_output_signal_stop(output, OBS_OUTPUT_BAD_PATH);
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

	connect_start = std::chrono::steady_clock::now();

	// Create a callback to log when the session is connected or closed.
	// libmoq status codes (>= 0.3.0): > 0 = (re)connected (epoch), 0 = closed
	// cleanly (terminal), < 0 = fatal/reconnect-gave-up (terminal).
	auto session_connect_callback = [](void *user_data, int code) {
		auto self = static_cast<MoQOutput *>(user_data);

		if (code > 0) {
			auto elapsed = std::chrono::steady_clock::now() - self->connect_start;
			self->connect_time_ms = static_cast<int>(
				std::chrono::duration_cast<std::chrono::milliseconds>(elapsed).count());
			LOG_INFO("MoQ session connected (%d ms, epoch %d): %s", self->connect_time_ms.load(), code,
				 self->server_url.c_str());
		} else {
			// Terminal callback (0 = clean close, < 0 = fatal): the session task
			// has ended and will not touch `self` again. Release the lifetime
			// reference the destructor waits on. This is the last access to `self`.
			LOG_INFO("MoQ session closed (%d): %s", code, self->server_url.c_str());
			std::lock_guard<std::mutex> lock(self->session_mutex);
			if (--self->outstanding_sessions == 0)
				self->session_cv.notify_all();
		}
	};

	// Pre-account for the session subscription before handing `this` to libmoq:
	// the connection can fail and fire its terminal callback before connect
	// returns, and the destructor must wait for that callback.
	{
		std::lock_guard<std::mutex> lock(session_mutex);
		outstanding_sessions++;
	}

	// Start establishing a session with the MoQ server
	// NOTE: You could publish the same broadcasts to multiple sessions if you want (redundant ingest).
	session = moq_session_connect(server_url.data(), server_url.size(), origin, 0, session_connect_callback, this);
	if (session < 0) {
		LOG_ERROR("Failed to initialize MoQ server: %d", session);
		// No subscription was created, so no terminal will fire; undo the ref.
		std::lock_guard<std::mutex> lock(session_mutex);
		if (--outstanding_sessions == 0)
			session_cv.notify_all();
		return false;
	}

	LOG_INFO("Publishing broadcast: %s", path.c_str());

	// Publish the broadcast to the origin we created.
	// TODO: There is currently no unpublish function.
	auto result = moq_origin_publish(origin, path.data(), path.size(), broadcast);
	if (result < 0) {
		LOG_ERROR("Failed to publish broadcast to session: %d", result);
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
	// Close the session
	if (session > 0) {
		moq_session_close(session);
		session = 0;
	}

	for (auto &[encoder, handle] : video_tracks) {
		if (handle > 0)
			moq_publish_media_close(handle);
	}
	video_tracks.clear();

	for (auto &[encoder, handle] : audio_tracks) {
		if (handle > 0)
			moq_publish_media_close(handle);
	}
	audio_tracks.clear();

	if (signal) {
		obs_output_signal_stop(output, OBS_OUTPUT_SUCCESS);
	}

	return;
}

void MoQOutput::Data(struct encoder_packet *packet)
{
	if (!packet) {
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
	int handle = moq_publish_media_ordered(broadcast, moq_codec, strlen(moq_codec), extra_data, extra_size);
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

	int handle = moq_publish_media_ordered(broadcast, codec, strlen(codec), extra_data, extra_size);
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
