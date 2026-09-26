// SPDX-License-Identifier: GPL-2.0-or-later
#include <obs.hpp>

#include "moq-output.h"
#include "moq-describe.h"
#include "moq-settings.h"
#include "moq-url.h"
#include "logger.h"
#include "util/util_uint64.h"

#include <cstring>
#include <string>
#include <vector>

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

// Dial URL scheme only. https races WebTransport vs WebSocket, and the session
// does not report which won, so do not invent a transport name here.
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

MoQOutput::MoQOutput(obs_data_t *, obs_output_t *output) : output(output), path(), total_bytes_sent(0) {}

MoQOutput::~MoQOutput()
{
	// Retires the attempt, so a continuation that is already queued won't signal a
	// stop on an output that is going away.
	Stop();

	// Drops whatever is still queued and waits out a continuation in flight, which
	// may be parked on signal_mutex; so this must not hold it. Nothing touches this
	// object once it returns.
	worker.Stop();
}

template<typename Output, typename Callback>
std::function<void(Output)> MoQOutput::Current(const std::shared_ptr<Attempt> &current, Callback callback)
{
	return [this, weak = std::weak_ptr<Attempt>(current), callback = std::move(callback)](Output output) {
		// Held across the check and whatever the callback reports, so a Stop() can't
		// retire the attempt in between.
		std::lock_guard<std::recursive_mutex> signal_lock(signal_mutex);
		auto current = weak.lock();
		if (!current || current != attempt)
			return;
		callback(current, std::move(output));
	};
}

bool MoQOutput::Start()
{
	// OBS restarts a reconnecting output by calling start again with no stop in
	// between, so drop whatever the previous attempt left behind.
	Reset();

	obs_service_t *service = obs_output_get_service(output);
	if (!service) {
		LOG_ERROR("Failed to get service from output");
		std::lock_guard<std::recursive_mutex> signal_lock(signal_mutex);
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
	const std::string server = server_value ? server_value : "";
	if (server.empty()) {
		LOG_ERROR("Server URL is empty");
		std::lock_guard<std::recursive_mutex> signal_lock(signal_mutex);
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

	auto next = std::make_shared<Attempt>();
	next->client = moq::Client::init();
	next->url = server;

	// Advanced settings live on the service alongside the URL and path. With the group
	// switched off the client keeps the library defaults.
	OBSDataAutoRelease service_settings = obs_service_get_settings(service);
	std::string invalid;
	if (!MoQSettings::Configure(service_settings, *next->client, &invalid)) {
		// Refusing to start beats connecting with a setting the user asked for
		// quietly dropped.
		LOG_ERROR("Invalid advanced MoQ settings: %s", invalid.c_str());
		std::lock_guard<std::recursive_mutex> signal_lock(signal_mutex);
		obs_output_set_last_error(output, ("Invalid advanced MoQ settings: " + invalid).c_str());
		obs_output_signal_stop(output, OBS_OUTPUT_CONNECT_FAILED);
		return false;
	}

	// Create the broadcast on an origin of our own, then announce it so subscribers
	// can discover it. Stop() finishes it, so each Start creates a fresh one.
	LOG_INFO("Publishing broadcast: %s", path.c_str());
	auto next_origin = moq::OriginProducer::init(moq::OriginConfig{});
	auto next_broadcast = next_origin->create_broadcast(path);
	if (!next_broadcast) {
		LOG_ERROR("Failed to create broadcast: %s", MoQDescribe(next_broadcast.error()).c_str());
		return false;
	}
	if (auto announced = (*next_broadcast)->announce(moq::Route{}); !announced) {
		LOG_ERROR("Failed to announce broadcast: %s", MoQDescribe(announced.error()).c_str());
		return false;
	}
	if (auto wired = next->client->set_publish(next_origin); !wired) {
		LOG_ERROR("Failed to publish through the client: %s", MoQDescribe(wired.error()).c_str());
		return false;
	}

	LOG_INFO("Connecting to MoQ server: %s", MoQRedactUrl(server).c_str());

	// Held from the connect through obs_output_begin_data_capture. Every
	// continuation takes the same lock before reporting anything, so this attempt's
	// failure cannot signal against an output that is only half started: it waits
	// until the output is committed, and OBS then handles the stop through its
	// normal active-output path.
	std::lock_guard<std::recursive_mutex> signal_lock(signal_mutex);

	attempt = next;
	{
		std::lock_guard<std::mutex> lock(mutex);
		url = server;
	}
	{
		std::lock_guard<std::mutex> lock(media_mutex);
		origin = next_origin;
		broadcast = *next_broadcast;
	}

	// NOTE: You could publish the same broadcasts to multiple sessions if you want (redundant ingest).
	next->started = std::chrono::steady_clock::now();
	next->pending = next->client->connect(server).then(
		worker.Executor(), Current<moq::expected<std::shared_ptr<moq::Session>>>(
					   next, [this](const std::shared_ptr<Attempt> &current, auto result) {
						   OnConnect(current, std::move(result));
					   }));

	obs_output_begin_data_capture(output, 0);

	return true;
}

void MoQOutput::OnConnect(const std::shared_ptr<Attempt> &current, moq::expected<std::shared_ptr<moq::Session>> result)
{
	if (!result) {
		Fail(current, result.error());
		return;
	}

	auto elapsed = std::chrono::steady_clock::now() - current->started;
	auto ms = static_cast<int>(std::chrono::duration_cast<std::chrono::milliseconds>(elapsed).count());
	const int connect_epoch = static_cast<int>((*result)->epoch());

	current->session = *result;
	current->connected = true;
	current->live = true;
	{
		std::lock_guard<std::mutex> lock(mutex);
		session = current->session;
		connected = true;
		live = true;
		last_failure_code = MoQError::None;
		last_failure_reason.clear();
	}
	// OBS and older dock paths treat connect_time_ms == 0 as "never connected".
	// Clamp sub-millisecond connects to 1 so that sentinel stays honest.
	connect_time_ms = ms > 0 ? ms : 1;

	LOG_INFO("MoQ session connected (%d ms, epoch %d): %s", ms, connect_epoch, MoQRedactUrl(current->url).c_str());

	WatchStatus(current);
}

void MoQOutput::WatchStatus(const std::shared_ptr<Attempt> &current)
{
	// Replacing `pending` from inside its own continuation is fine: that call has
	// already completed, so dropping it cancels nothing.
	current->pending = current->session->status().then(
		worker.Executor(), Current<moq::expected<moq::ConnectionStatus>>(
					   current, [this](const std::shared_ptr<Attempt> &current, auto result) {
						   OnStatus(current, std::move(result));
					   }));
}

void MoQOutput::OnStatus(const std::shared_ptr<Attempt> &current, moq::expected<moq::ConnectionStatus> result)
{
	// The status stream ends in an error once reconnecting gives up for good.
	if (!result) {
		Fail(current, result.error());
		return;
	}

	switch (*result) {
	case moq::ConnectionStatus::kConnected: {
		const int connect_epoch = static_cast<int>(current->session->epoch());
		current->live = true;
		{
			std::lock_guard<std::mutex> lock(mutex);
			live = true;
			last_failure_code = MoQError::None;
			last_failure_reason.clear();
		}
		LOG_INFO("MoQ session reconnected (epoch %d): %s", connect_epoch, MoQRedactUrl(current->url).c_str());
		break;
	}
	case moq::ConnectionStatus::kDisconnected: {
		current->live = false;
		std::lock_guard<std::mutex> lock(mutex);
		live = false;
		LOG_WARNING("MoQ session dropped, reconnecting: %s", MoQRedactUrl(current->url).c_str());
		break;
	}
	case moq::ConnectionStatus::kMigrating:
		// The old session keeps serving while the replacement dials.
		LOG_INFO("MoQ session migrating: %s", MoQRedactUrl(current->url).c_str());
		break;
	}

	WatchStatus(current);
}

void MoQOutput::Fail(const std::shared_ptr<Attempt> &current, const moq::Error &error)
{
	const std::string reason = MoQDescribe(error);
	const bool was_connected = current->connected;

	// Retire the attempt, which also limits the failure signal to one per Start().
	// The broadcast stays until Reset(), as OBS stops or restarts the output next.
	attempt.reset();
	{
		std::lock_guard<std::mutex> lock(mutex);
		session.reset();
		connected = false;
		live = false;
		last_failure_code = MoQFailureCode(error);
		last_failure_reason = reason;
	}
	connect_time_ms = 0;

	LOG_ERROR("MoQ session failed: %s: %s", MoQRedactUrl(current->url).c_str(), reason.c_str());

	// Reconnection gave up, so nothing is reaching the server any more. Without
	// this OBS keeps encoding and reporting the stream as live forever.
	obs_output_set_last_error(output, reason.c_str());
	// CONNECT_FAILED is terminal for OBS. DISCONNECTED lets its own reconnect
	// logic retry, which is only worth offering once we know the server works.
	obs_output_signal_stop(output, was_connected ? OBS_OUTPUT_DISCONNECTED : OBS_OUTPUT_CONNECT_FAILED);
}

void MoQOutput::Stop(bool signal)
{
	std::lock_guard<std::recursive_mutex> signal_lock(signal_mutex);

	Reset();

	if (signal)
		obs_output_signal_stop(output, OBS_OUTPUT_SUCCESS);
}

void MoQOutput::Reset()
{
	// Excludes a continuation's report: retiring the attempt and signalling a
	// failure must not interleave, or a stop that already happened gets followed by
	// a failure OBS turns into a reconnect.
	std::lock_guard<std::recursive_mutex> signal_lock(signal_mutex);

	// Dropping the attempt cancels its pending connect or status call.
	if (attempt && attempt->session)
		attempt->session->shutdown();
	attempt.reset();

	{
		std::lock_guard<std::mutex> lock(mutex);
		session.reset();
		connected = false;
		live = false;
		url.clear();
		last_failure_code = MoQError::None;
		last_failure_reason.clear();
	}
	connect_time_ms = 0;

	std::lock_guard<std::mutex> lock(media_mutex);
	for (auto &[encoder, track] : video_tracks) {
		if (track)
			track->finish();
	}
	video_tracks.clear();

	for (auto &[encoder, track] : audio_tracks) {
		if (track)
			track->finish();
	}
	audio_tracks.clear();

	// Finish the broadcast so the origin unpublishes it immediately; Start()
	// creates a fresh one on restart.
	if (broadcast)
		broadcast->finish();
	broadcast.reset();
	origin.reset();
}

bool MoQOutput::TryGetConnectionStats(ConnectionStats *out)
{
	if (!out)
		return false;

	std::shared_ptr<moq::Session> current;
	std::string dial_url;
	{
		std::lock_guard<std::mutex> lock(mutex);
		if (!session)
			return false;
		if (!live) {
			// Between reconnects. Keep a more specific prior failure (unauthorized)
			// over a generic offline blip.
			if (last_failure_reason.empty() || LooksGenericOffline(last_failure_reason)) {
				last_failure_code = MoQError::Offline;
				last_failure_reason = "offline";
			}
			return false;
		}
		current = session;
		dial_url = url;
	}

	const moq::ConnectionStats raw = current->stats();

	ConnectionStats snapshot;
	snapshot.reconnects = GetReconnectCount();
	snapshot.rtt_valid = raw.rtt_us.has_value();
	snapshot.rtt_ms = raw.rtt_us ? static_cast<double>(*raw.rtt_us) / 1000.0 : 0;
	snapshot.estimated_send_rate_valid = raw.estimated_send_rate_bps.has_value();
	snapshot.estimated_send_rate_bps =
		raw.estimated_send_rate_bps ? static_cast<double>(*raw.estimated_send_rate_bps) : 0;
	snapshot.estimated_recv_rate_valid = raw.estimated_recv_rate_bps.has_value();
	snapshot.estimated_recv_rate_bps =
		raw.estimated_recv_rate_bps ? static_cast<double>(*raw.estimated_recv_rate_bps) : 0;
	snapshot.bytes_sent_valid = raw.bytes_sent.has_value();
	snapshot.bytes_sent = raw.bytes_sent.value_or(0);
	if (raw.packets_sent && raw.packets_lost && *raw.packets_sent > 0) {
		snapshot.loss_valid = true;
		snapshot.loss_pct =
			100.0 * static_cast<double>(*raw.packets_lost) / static_cast<double>(*raw.packets_sent);
	}
	snapshot.dial = DialSchemeLabel(dial_url);

	*out = std::move(snapshot);
	return true;
}

int MoQOutput::GetReconnectCount()
{
	std::shared_ptr<moq::Session> current;
	{
		std::lock_guard<std::mutex> lock(mutex);
		current = session;
	}
	// Read live rather than tracked from status(), which coalesces a drop that
	// reconnects before it is asked again.
	const uint64_t connect_epoch = current ? current->epoch() : 0;
	return connect_epoch > 1 ? static_cast<int>(connect_epoch - 1) : 0;
}

bool MoQOutput::IsLiveSession()
{
	std::lock_guard<std::mutex> lock(mutex);
	return connected && session != nullptr;
}

void MoQOutput::CopyLastFailure(int *code, std::string *reason)
{
	std::lock_guard<std::mutex> lock(mutex);
	if (code)
		*code = last_failure_code;
	if (reason)
		*reason = last_failure_reason;
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

// The packet's timestamp on the broadcast clock, or nothing when it falls before zero.
static std::optional<uint64_t> PacketTimestamp(const struct encoder_packet *packet)
{
	// Add ~1 second offset to handle negative PTS from audio priming frames, and
	// the same to video for A/V sync.
	// TODO: This is slightly wrong when den is not evenly divisible by num, but close enough.
	int64_t pts = packet->pts + packet->timebase_den / packet->timebase_num;
	if (pts < 0)
		return std::nullopt;
	return util_mul_div64(pts, 1000000ULL * packet->timebase_num, packet->timebase_den);
}

static moq::Frame PacketFrame(const struct encoder_packet *packet, uint64_t timestamp_us)
{
	return moq::Frame{std::vector<uint8_t>(packet->data, packet->data + packet->size), timestamp_us};
}

void MoQOutput::AudioData(struct encoder_packet *packet)
{
	std::lock_guard<std::mutex> lock(media_mutex);
	obs_encoder_t *encoder = packet->encoder;

	auto it = audio_tracks.find(encoder);
	if (it == audio_tracks.end()) {
		AudioInit(encoder);
		it = audio_tracks.find(encoder);
	}
	if (it == audio_tracks.end() || !it->second) {
		// We failed to initialize the audio track, so we can't write any data.
		return;
	}
	const auto &track = it->second;

	auto timestamp = PacketTimestamp(packet);
	if (!timestamp) {
		LOG_WARNING("Dropping audio frame with negative PTS: %lld", (long long)packet->pts);
		return;
	}

	if (auto result = track->write_frame(PacketFrame(packet, *timestamp)); !result) {
		LOG_ERROR("Failed to write audio frame: %s", MoQDescribe(result.error()).c_str());
		return;
	}

	// Audio has no keyframes, so it has no group boundary of its own: without this the whole
	// stream is one group. Cut per frame, which is one QUIC stream per packet forwarded without
	// waiting for the next, the right trade for live. Video groups at its own keyframes.
	// Cut before observing the flush so a failed observation never leaves the group open.
	if (auto result = track->cut(); !result) {
		LOG_ERROR("Failed to cut audio group: %s", MoQDescribe(result.error()).c_str());
		return;
	}

	if (auto result = track->flush(*timestamp); !result) {
		LOG_ERROR("Failed to observe audio encoder flush: %s", MoQDescribe(result.error()).c_str());
		return;
	}

	total_bytes_sent += packet->size;
}

void MoQOutput::VideoData(struct encoder_packet *packet)
{
	std::lock_guard<std::mutex> lock(media_mutex);
	obs_encoder_t *encoder = packet->encoder;

	auto it = video_tracks.find(encoder);
	if (it == video_tracks.end()) {
		VideoInit(encoder);
		it = video_tracks.find(encoder);
	}
	if (it == video_tracks.end() || !it->second)
		return;
	const auto &track = it->second;

	auto timestamp = PacketTimestamp(packet);
	if (!timestamp) {
		LOG_WARNING("Dropping video frame with negative PTS: %lld", (long long)packet->pts);
		return;
	}

	if (auto result = track->write_frame(PacketFrame(packet, *timestamp)); !result) {
		LOG_ERROR("Failed to write video frame: %s", MoQDescribe(result.error()).c_str());
		return;
	}

	if (auto result = track->flush(*timestamp); !result) {
		LOG_ERROR("Failed to observe video encoder flush: %s", MoQDescribe(result.error()).c_str());
		return;
	}

	total_bytes_sent += packet->size;
}

// NOTE: Caller must hold media_mutex.
void MoQOutput::VideoInit(obs_encoder_t *encoder)
{
	if (!encoder) {
		LOG_ERROR("Failed to get video encoder");
		return;
	}
	// Stopped, or not started yet: no broadcast to publish on.
	if (!broadcast)
		return;

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
	moq::VideoInit config{};
	if (strcmp(codec, "h264") == 0) {
		config.format = moq::VideoFormat::kAvc3;
	} else if (strcmp(codec, "hevc") == 0) {
		config.format = moq::VideoFormat::kHev1;
	} else if (strcmp(codec, "av1") == 0) {
		config.format = moq::VideoFormat::kAv01;
	} else {
		LOG_ERROR("Unsupported video codec: %s", codec);
		video_tracks[encoder] = nullptr;
		return;
	}

	if (extra_data && extra_size > 0)
		config.data.assign(extra_data, extra_data + extra_size);

	// Seed catalog fields a downstream moq-transcode needs before measured rates
	// arrive: coded size (also from SPS once parsed) and configured CBR bitrate so
	// same-height ladder rungs can undercut the mezzanine.
	moq::VideoHint hint{};
	if (video_width > 0 && video_height > 0)
		hint.coded = moq::Dimensions{video_width, video_height};
	const std::string rate_control = settings ? obs_data_get_string(settings, "rate_control") : "";
	if (video_bitrate_kbps > 0 && (rate_control == "CBR" || rate_control == "cbr"))
		hint.bitrate = (uint64_t)video_bitrate_kbps * 1000ULL;
	hint.optimize_for_latency = true;
	config.hint = hint;

	auto track = broadcast->publish_video(config);
	if (!track) {
		LOG_ERROR("Failed to initialize video track: %s", MoQDescribe(track.error()).c_str());
		video_tracks[encoder] = nullptr;
		return;
	}
	video_tracks[encoder] = *track;

	LOG_INFO("Video track initialized (%ux%u, %d kbps)", video_width, video_height, video_bitrate_kbps);
}

// NOTE: Caller must hold media_mutex.
void MoQOutput::AudioInit(obs_encoder_t *encoder)
{
	if (!encoder) {
		LOG_ERROR("Failed to get audio encoder");
		return;
	}
	if (!broadcast)
		return;

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

	// Mapping the codec here means OBS says which one it was, rather than the importer
	// failing on it deep inside.
	moq::AudioInit config{};
	if (strcmp(codec, "opus") == 0) {
		config.format = moq::AudioFormat::kOpus;
	} else if (strcmp(codec, "aac") == 0) {
		config.format = moq::AudioFormat::kAac;
	} else if (strcmp(codec, "flac") == 0) {
		config.format = moq::AudioFormat::kFlac;
	} else {
		LOG_ERROR("Unsupported audio codec: %s", codec);
		audio_tracks[encoder] = nullptr;
		return;
	}

	if (extra_data && extra_size > 0)
		config.data.assign(extra_data, extra_data + extra_size);

	auto track = broadcast->publish_audio(config);
	if (!track) {
		LOG_ERROR("Failed to initialize audio track: %s", MoQDescribe(track.error()).c_str());
		audio_tracks[encoder] = nullptr;
		return;
	}
	audio_tracks[encoder] = *track;

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
