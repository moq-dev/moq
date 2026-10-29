// SPDX-License-Identifier: GPL-2.0-or-later
#include <obs.hpp>

#include "moq-output.h"
#include "moq-error.h"
#include "moq-implementation.h"
#include "moq-settings.h"
#include "moq-url.h"
#include "logger.h"
#include "util/util_uint64.h"

#include <algorithm>
#include <cstring>
#include <string>
#include <variant>
#include <vector>

namespace {

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

// The failure code the dock classifies a moq::Error by, beside its text.
MoQError::Code FailureCode(const moq::Error &error)
{
	const auto &variant = error.get_variant();
	if (std::holds_alternative<moq::Error::kUnauthorized>(variant) ||
	    std::holds_alternative<moq::Error::kExpired>(variant))
		return MoQError::Code::Unauthorized;
	if (std::holds_alternative<moq::Error::kForbidden>(variant))
		return MoQError::Code::Forbidden;
	if (std::holds_alternative<moq::Error::kConnect>(variant))
		return MoQError::Code::Connect;
	if (const auto *protocol = std::get_if<moq::Error::kProtocol>(&variant)) {
		if (protocol->details.kind == moq::ProtocolKind::kTooManyRequests)
			return MoQError::Code::TooManyRequests;
		if (protocol->details.kind == moq::ProtocolKind::kUnauthorized ||
		    protocol->details.kind == moq::ProtocolKind::kExpired)
			return MoQError::Code::Unauthorized;
	}
	return MoQError::Code::Other;
}

// Retune `encoder` to `bitrate_bps`, the way OBS's own RTMP dynamic bitrate does.
void SetBitrate(obs_encoder_t *encoder, uint64_t bitrate_bps)
{
	OBSDataAutoRelease settings = obs_data_create();
	obs_data_set_int(settings, "bitrate", (long long)(bitrate_bps / 1000));
	obs_encoder_update(encoder, settings);
}

// Whether `rate_control` lets the "bitrate" setting drive the encoder's output.
// Encoders without the setting are bitrate-driven by construction.
bool BitrateDriven(const std::string &rate_control)
{
	std::string mode = rate_control;
	for (char &c : mode)
		c = static_cast<char>(c >= 'a' && c <= 'z' ? c - 'a' + 'A' : c);
	return mode.empty() || mode == "CBR" || mode == "VBR" || mode == "ABR";
}

} // namespace

MoQOutput::MoQOutput(obs_data_t *, obs_output_t *output) : output(output), path(), total_bytes_sent(0) {}

MoQOutput::~MoQOutput()
{
	// Retires the attempt, so a continuation that is already queued won't signal a
	// stop on an output that is going away.
	Stop();

	// Wait out the drains: OBS unloads the module, stopping the runtime, right
	// after destroying its outputs on exit. moq-ffi bounds each drain to about a second.
	std::vector<moq::Future<void>> drains;
	{
		std::lock_guard<std::recursive_mutex> signal_lock(signal_mutex);
		drains.swap(draining);
	}
	for (auto &drain : drains)
		drain.wait();

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

	// Create the broadcast on an origin of our own, then announce it so subscribers
	// can discover it. Stop() finishes it, so each Start creates a fresh one. Only the
	// broadcast is kept: the route outlives the publication until the broadcast closes.
	LOG_INFO("Publishing broadcast: %s", path.c_str());
	auto next_origin = moq::OriginProducer::init(moq::OriginConfig{});
	auto publication = next_origin->create_broadcast(path);
	if (!publication) {
		LOG_ERROR("Failed to create broadcast: %s", publication.error().to_string().c_str());
		return false;
	}
	moq::AnnounceRoute route;
	route.epoch = moq::mint_epoch();
	if (auto announced = (*publication)->announce(route); !announced) {
		LOG_ERROR("Failed to announce broadcast: %s", announced.error().to_string().c_str());
		return false;
	}

	// Advanced settings live on the service alongside the URL and path. With the group
	// switched off the client keeps the library defaults.
	moq::ClientConfig config;
	config.publish = next_origin;
	config.implementation = MoQImplementation();
	// Not on the URL: a URL's ?jwt= overrides the client's tokens, so every reconnect
	// would dial it instead of the one PresentToken() set last.
	if (const char *token = obs_service_get_connect_info(service, OBS_SERVICE_CONNECT_INFO_BEARER_TOKEN);
	    token && *token)
		config.tokens = {token};
	OBSDataAutoRelease service_settings = obs_service_get_settings(service);
	std::string invalid = MoQSettings::Configure(service_settings, config);
	std::shared_ptr<moq::Client> client;
	if (invalid.empty()) {
		if (auto init = moq::Client::init(config))
			client = *init;
		else
			invalid = init.error().to_string();
	}
	if (!client) {
		// Refusing to start beats connecting with a setting the user asked for
		// quietly dropped.
		LOG_ERROR("Invalid advanced MoQ settings: %s", invalid.c_str());
		std::lock_guard<std::recursive_mutex> signal_lock(signal_mutex);
		obs_output_set_last_error(output, ("Invalid advanced MoQ settings: " + invalid).c_str());
		obs_output_signal_stop(output, OBS_OUTPUT_CONNECT_FAILED);
		return false;
	}

	auto next = std::make_shared<Attempt>();
	next->client = client;
	next->url = server;

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
		epoch = *route.epoch;
	}
	{
		std::lock_guard<std::mutex> lock(media_mutex);
		origin = next_origin;
		broadcast = (*publication)->broadcast();
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

	current->session = *result;
	{
		std::lock_guard<std::mutex> lock(media_mutex);
		allocator = current->session->allocator();
	}
	{
		std::lock_guard<std::mutex> lock(mutex);
		session = current->session;
		recv_bandwidth = session->recv_bandwidth();
	}
	OnState(current, current->session->state());
}

void MoQOutput::WatchState(const std::shared_ptr<Attempt> &current)
{
	current->pending = current->session->state_changed(current->revision)
				   .then(worker.Executor(),
					 Current<moq::expected<moq::ConnectionState>>(
						 current, [this](const std::shared_ptr<Attempt> &current, auto result) {
							 OnState(current, std::move(result));
						 }));
}

void MoQOutput::OnState(const std::shared_ptr<Attempt> &current, moq::expected<moq::ConnectionState> result)
{
	if (!result) {
		Fail(current, result.error());
		return;
	}
	const auto &state = *result;
	current->revision = state.revision;
	current->live = state.phase == moq::ConnectionPhase::kConnected ||
			state.phase == moq::ConnectionPhase::kMigrating;
	if (state.connects > 0 && !current->connected) {
		auto elapsed = std::chrono::steady_clock::now() - current->started;
		auto ms = std::chrono::duration_cast<std::chrono::milliseconds>(elapsed).count();
		connect_time_ms = ms > 0 ? static_cast<int>(ms) : 1;
		current->connected = true;
	}
	{
		std::lock_guard<std::mutex> lock(mutex);
		connected = current->connected;
		live = current->live;
		if (state.failure) {
			last_failure_code = FailureCode(*state.failure);
			last_failure_reason = state.failure->to_string();
		} else if (current->live) {
			last_failure_code = MoQError::None;
			last_failure_reason.clear();
		}
	}
	LOG_INFO("MoQ connection state changed (connect %llu): %s", (unsigned long long)state.connects,
		 MoQRedactUrl(current->url).c_str());
	// A second wait at the terminal revision returns its close reason through Fail.
	WatchState(current);
}

void MoQOutput::Fail(const std::shared_ptr<Attempt> &current, const moq::Error &error)
{
	const std::string reason = error.to_string();
	const bool was_connected = current->connected;

	// Retire the attempt, which also limits the failure signal to one per Start().
	// The broadcast stays until Reset(), as OBS stops or restarts the output next.
	attempt.reset();
	{
		std::lock_guard<std::mutex> lock(mutex);
		recv_bandwidth.reset();
		session.reset();
		connected = false;
		live = false;
		last_failure_code = FailureCode(error);
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
	std::shared_ptr<moq::Session> retired = attempt ? attempt->session : nullptr;
	attempt.reset();

	{
		std::lock_guard<std::mutex> lock(mutex);
		recv_bandwidth.reset();
		session.reset();
		connected = false;
		live = false;
		url.clear();
		epoch.clear();
		last_failure_code = MoQError::None;
		last_failure_reason.clear();
		token_refusal.clear();
	}
	connect_time_ms = 0;

	// Following the grant rewrote the encoder's bitrate, which the next Start()
	// reads back as the configured rate. Restored after media_mutex is released:
	// an idle encoder applies the update inline.
	std::vector<std::pair<OBSEncoder, uint64_t>> restore;

	std::unique_lock<std::mutex> lock(media_mutex);
	for (auto &[encoder, track] : video_tracks) {
		if (track.producer)
			track.producer->finish();
		if (track.rate && track.rate->Target() != track.rate->Max()) {
			if (OBSEncoder strong = OBSGetStrongRef(track.encoder))
				restore.emplace_back(std::move(strong), track.rate->Max());
		}
	}
	video_tracks.clear();

	for (auto &[encoder, track] : audio_tracks) {
		if (track.producer)
			track.producer->finish();
	}
	audio_tracks.clear();
	allocator.reset();

	// Close the broadcast so the origin retracts it immediately; Start()
	// creates a fresh one on restart.
	if (broadcast)
		broadcast->close();
	broadcast.reset();
	origin.reset();

	// The session owns the drain after this request, even if the output dies now.
	// Keep a waiter only so the destructor can hold off module unload, which
	// stops the runtime and would cancel the drain.
	if (retired) {
		retired->shutdown();
		draining.erase(std::remove_if(draining.begin(), draining.end(),
					      [](const moq::Future<void> &drain) {
						      return drain.wait_for(std::chrono::seconds(0)) ==
							     std::future_status::ready;
					      }),
			       draining.end());
		draining.push_back(retired->closed());
	}

	lock.unlock();

	for (auto &[encoder, bitrate] : restore)
		SetBitrate(encoder, bitrate);
}

std::string MoQOutput::GetEpoch()
{
	std::lock_guard<std::mutex> lock(mutex);
	return epoch;
}

bool MoQOutput::TryGetConnectionStats(ConnectionStats *out)
{
	if (!out)
		return false;

	std::shared_ptr<moq::Session> current;
	std::shared_ptr<moq::BandwidthConsumer> receive;
	std::string dial_url;
	bool current_live = false;
	{
		std::lock_guard<std::mutex> lock(mutex);
		if (!session)
			return false;
		current = session;
		receive = recv_bandwidth;
		dial_url = url;
		current_live = live;
	}

	if (!current_live) {
		return false;
	}

	const moq::ConnectionStats raw = current->stats();

	ConnectionStats snapshot;
	snapshot.reconnects = GetReconnectCount();
	snapshot.rtt_valid = raw.rtt.has_value();
	snapshot.rtt_ms = raw.rtt ? std::chrono::duration<double, std::milli>(*raw.rtt).count() : 0;
	snapshot.estimated_send_rate_valid = raw.estimated_send_rate_bps.has_value();
	snapshot.estimated_send_rate_bps =
		raw.estimated_send_rate_bps ? static_cast<double>(*raw.estimated_send_rate_bps) : 0;
	const auto recv_rate = receive ? receive->rate() : std::nullopt;
	snapshot.estimated_recv_rate_valid = recv_rate.has_value();
	snapshot.estimated_recv_rate_bps = recv_rate ? static_cast<double>(*recv_rate) : 0;
	snapshot.bytes_sent_valid = raw.bytes_sent.has_value();
	snapshot.bytes_sent = raw.bytes_sent.value_or(0);
	if (raw.packets_sent && raw.packets_lost && *raw.packets_sent > 0) {
		snapshot.loss_valid = true;
		snapshot.loss_pct =
			100.0 * static_cast<double>(*raw.packets_lost) / static_cast<double>(*raw.packets_sent);
	}
	snapshot.protocol = raw.version.value_or("");
	snapshot.dial = DialSchemeLabel(dial_url);

	return CommitConnectionStats(current, std::move(snapshot), out);
}

bool MoQOutput::CommitConnectionStats(const std::shared_ptr<moq::Session> &current, ConnectionStats snapshot,
				      ConnectionStats *out)
{
	// A Stop(), restart, or disconnect while stats() ran retired this session, so
	// its numbers no longer describe the output.
	std::lock_guard<std::mutex> lock(mutex);
	if (session != current || !live)
		return false;
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
	// The cumulative count survives a reconnect that finishes between UI reads.
	const uint64_t connects = current ? current->state().connects : 0;
	return connects > 1 ? static_cast<int>(connects - 1) : 0;
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

std::string MoQOutput::TokenRefusal()
{
	std::lock_guard<std::mutex> lock(mutex);
	return token_refusal;
}

bool MoQOutput::PresentToken(const std::string &token)
{
	std::lock_guard<std::recursive_mutex> signal_lock(signal_mutex);
	if (!attempt || !attempt->session)
		return false;
	// Only a server-accepted session has no tokens.
	std::shared_ptr<moq::Tokens> tokens = attempt->session->tokens();
	if (!tokens)
		return false;

	auto refused = [this](const std::string &reason) {
		LOG_WARNING("MoQ relay refused the new publish token: %s", reason.c_str());
		std::lock_guard<std::mutex> lock(mutex);
		token_refusal = reason;
	};
	{
		std::lock_guard<std::mutex> lock(mutex);
		token_refusal.clear();
	}
	if (auto set = tokens->set({token}); !set) {
		attempt->presenting.reset();
		refused(set.error().to_string());
		return true;
	}

	// Replacing `presenting` drops the wait on a token this one replaced.
	attempt->presenting = tokens->answered().then(
		worker.Executor(),
		Current<moq::expected<void>>(attempt, [refused](const std::shared_ptr<Attempt> &current, auto result) {
			if (!result) {
				refused(result.error().to_string());
				return;
			}
			LOG_INFO("MoQ relay accepted the new publish token: %s", MoQRedactUrl(current->url).c_str());
		}));
	return true;
}

bool MoQOutput::TryGetVideoRate(VideoRate *out)
{
	VideoRate rate;
	bool following = false;
	{
		std::lock_guard<std::mutex> lock(media_mutex);
		for (const auto &[encoder, track] : video_tracks) {
			if (!track.rate || !track.reservation)
				continue;
			following = true;
			rate.target_bps += track.rate->Target();
			rate.configured_bps += track.rate->Max();
		}
	}
	if (following && out)
		*out = rate;
	return following;
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
	if (it == audio_tracks.end())
		return;
	auto &track = it->second;
	if (!track.producer)
		return;
	Reserve(track);

	auto timestamp = PacketTimestamp(packet);
	if (!timestamp) {
		LOG_WARNING("Dropping audio frame with negative PTS: %lld", (long long)packet->pts);
		return;
	}

	if (auto result = track.producer->write_frame(PacketFrame(packet, *timestamp)); !result) {
		LOG_ERROR("Failed to write audio frame: %s", result.error().to_string().c_str());
		return;
	}

	// Audio has no keyframes, so it has no group boundary of its own: without this the whole
	// stream is one group. Cut per frame, which is one QUIC stream per packet forwarded without
	// waiting for the next, the right trade for live. Video groups at its own keyframes.
	if (auto result = track.producer->cut(); !result) {
		LOG_ERROR("Failed to cut audio group: %s", result.error().to_string().c_str());
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
	if (it == video_tracks.end() || !it->second.producer)
		return;
	auto &track = it->second;
	Reserve(track);

	// Read before the frame is written, so a retune lands on the encoder's next
	// frame. obs_encoder_update only flags an active encoder for its own thread.
	if (track.rate && track.reservation) {
		if (auto next = track.rate->Update(track.reservation->rate(), MoQRate::Clock::now()))
			SetBitrate(encoder, *next);
	}

	auto timestamp = PacketTimestamp(packet);
	if (!timestamp) {
		LOG_WARNING("Dropping video frame with negative PTS: %lld", (long long)packet->pts);
		return;
	}

	if (auto result = track.producer->write_frame(PacketFrame(packet, *timestamp)); !result) {
		LOG_ERROR("Failed to write video frame: %s", result.error().to_string().c_str());
		return;
	}

	total_bytes_sent += packet->size;
}

// NOTE: Caller must hold media_mutex.
void MoQOutput::Reserve(Track &track)
{
	if (track.reservation || !allocator || track.bitrate_bps == 0)
		return;
	auto demand = track.producer->demand();
	if (!demand) {
		LOG_ERROR("Failed to reserve bandwidth: %s", demand.error().to_string().c_str());
		track.bitrate_bps = 0;
		return;
	}
	track.reservation = allocator->reserve(*demand, track.bitrate_bps);
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
		video_tracks[encoder] = Track{};
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

	auto track = moq::MediaTrackProducer::video(broadcast, moq::MediaTarget::kNamed{}, config);
	if (!track) {
		LOG_ERROR("Failed to initialize video track: %s", track.error().to_string().c_str());
		video_tracks[encoder] = Track{};
		return;
	}

	Track entry;
	entry.producer = *track;
	if (video_bitrate_kbps > 0) {
		entry.bitrate_bps = (uint64_t)video_bitrate_kbps * 1000ULL;
		// Following needs an encoder that retunes live and a rate control the
		// bitrate drives; a quality target (CQP, CRF) only reserves.
		const bool dynamic = obs_encoder_get_caps(encoder) & OBS_ENCODER_CAP_DYN_BITRATE;
		if (dynamic && BitrateDriven(rate_control)) {
			entry.rate.emplace(entry.bitrate_bps);
			entry.encoder = OBSGetWeakRef(encoder);
		} else {
			LOG_INFO("Video bitrate stays at %d kbps: the encoder cannot follow the bandwidth grant (%s)",
				 video_bitrate_kbps, rate_control.empty() ? "no rate control" : rate_control.c_str());
		}
	}
	video_tracks[encoder] = std::move(entry);

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

	OBSDataAutoRelease settings = obs_encoder_get_settings(encoder);
	const int audio_bitrate_kbps = settings ? (int)obs_data_get_int(settings, "bitrate") : 0;

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
		audio_tracks[encoder] = Track{};
		return;
	}

	if (extra_data && extra_size > 0)
		config.data.assign(extra_data, extra_data + extra_size);

	auto track = moq::MediaTrackProducer::audio(broadcast, moq::MediaTarget::kNamed{}, config);
	if (!track) {
		LOG_ERROR("Failed to initialize audio track: %s", track.error().to_string().c_str());
		audio_tracks[encoder] = Track{};
		return;
	}
	// Audio reserves so video's grant leaves room for it, and never follows.
	Track entry;
	entry.producer = *track;
	if (audio_bitrate_kbps > 0)
		entry.bitrate_bps = (uint64_t)audio_bitrate_kbps * 1000ULL;
	audio_tracks[encoder] = std::move(entry);

	LOG_INFO("Audio track initialized successfully");
}

void register_moq_output()
{
	struct obs_output_info info = {};
	info.id = "moq_output";
	info.flags = OBS_OUTPUT_AV | OBS_OUTPUT_ENCODED | OBS_OUTPUT_SERVICE | OBS_OUTPUT_MULTI_TRACK_VIDEO |
		     OBS_OUTPUT_MULTI_TRACK_AUDIO;
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
	info.encoded_video_codecs = "h264;hevc;av1";
	info.encoded_audio_codecs = "aac;opus";
	info.protocols = "MoQ";

	obs_register_output(&info);
}
