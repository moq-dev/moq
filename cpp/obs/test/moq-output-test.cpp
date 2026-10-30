// SPDX-License-Identifier: GPL-2.0-or-later
//
// Drives the real MoQOutput against stubbed libobs and the real moq-ffi, over a
// relay in the same process. Everything here is timing-sensitive in production:
// a connect or status result lands on the output's worker at a moment OBS does
// not choose, possibly during Start(), during a restart, during destruction, or
// racing a Stop(). The stubs record what OBS was told and when, and can hold an
// OBS call open so a test can drive the other side into it.
//
// Run with `just obs test` (ThreadSanitizer) or `just obs ci` (plain). This is not
// part of the plugin build.
#include <atomic>
#include <chrono>
#include <climits>
#include <cstdarg>
#include <cstdio>
#include <functional>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <thread>
#include <utility>
#include <vector>

#include <obs.h>
#include <obs-module.h>

#include "moq-output.h"
#include "moq-settings.h"
#include "moq-test-relay.h"

// ------------------------------------------------------------- libobs stubs

namespace {
struct RecordedSignal {
	int code;
	std::string last_error;
	// How many times capture had begun when this was signalled, so a test can tell
	// "committed the output, then reported" from "reported, then committed".
	int begin_capture_at;
};

// The plugin serializes its own signalling, but the tests read these from a
// different thread than the worker writes them, so the stubs carry their own
// lock. Always go through the helpers below.
std::mutex g_signals_mutex;
std::vector<RecordedSignal> g_signals;
std::string g_last_error;
std::atomic<int> g_begin_capture{0};
// Signals that reached OBS after the output was destroyed, which is a use after free.
std::atomic<int> g_signals_after_destroy{0};
std::atomic<bool> g_destroyed{false};

std::string g_url;
std::string g_rate_control = "CBR";
uint32_t g_encoder_caps = OBS_ENCODER_CAP_DYN_BITRATE;
// Every obs_encoder_update, as the encoder and the bitrate it was told in kbps.
std::mutex g_updates_mutex;
std::vector<std::pair<obs_encoder_t *, long long>> g_updates;
std::atomic<bool> g_settings_ok{true};
// A reason Configure reports itself, for a setting the config cannot express.
std::string g_settings_error;
// Lets a test run something inside obs_output_signal_stop, standing in for a
// frontend that stops the output straight from the signal handler.
std::function<void()> g_on_signal;
// obs_output_set_last_error sits between deciding to report a failure and
// reporting it, so the stub announces that it is in that window and then holds
// it open. A test drives a concurrent Stop() into the gap: with signal_mutex held
// across the whole report the Stop() blocks, and without it the stale report
// lands after the stop, which is the bug.
std::atomic<bool> g_stall_last_error{false};
std::atomic<bool> g_in_report_window{false};

const std::vector<uint8_t> g_h264_init = TestH264Init();
const std::vector<uint8_t> g_opus_head = TestOpusHead();

obs_encoder_t *const VIDEO_ENCODER = reinterpret_cast<obs_encoder_t *>(0x2);
obs_encoder_t *const AUDIO_ENCODER = reinterpret_cast<obs_encoder_t *>(0x3);
} // namespace

extern "C" {

// Deliberately silent: stdio locking would create happens-before edges between
// the OBS and worker threads and mask exactly the races we're looking for.
void blog(int, const char *, ...) {}

obs_service_t *obs_output_get_service(const obs_output_t *)
{
	return reinterpret_cast<obs_service_t *>(0x1);
}

bool obs_output_can_begin_data_capture(const obs_output_t *, uint32_t)
{
	return true;
}

bool obs_output_initialize_encoders(obs_output_t *, uint32_t)
{
	return true;
}

const char *obs_service_get_connect_info(const obs_service_t *, uint32_t type)
{
	return type == OBS_SERVICE_CONNECT_INFO_SERVER_URL ? g_url.c_str() : "room";
}

obs_data_t *obs_service_get_settings(const obs_service_t *)
{
	return reinterpret_cast<obs_data_t *>(0x3);
}

void obs_data_release(obs_data_t *) {}

obs_encoder_t *obs_output_get_video_encoder2(const obs_output_t *, size_t idx)
{
	return idx == 0 ? VIDEO_ENCODER : nullptr;
}

bool obs_output_begin_data_capture(obs_output_t *, uint32_t)
{
	g_begin_capture++;
	return true;
}

void obs_output_set_last_error(obs_output_t *, const char *message)
{
	{
		std::lock_guard<std::mutex> lock(g_signals_mutex);
		g_last_error = message ? message : "";
	}
	if (g_stall_last_error) {
		g_in_report_window.store(true);
		std::this_thread::sleep_for(std::chrono::milliseconds(50));
	}
}

void obs_output_signal_stop(obs_output_t *, int code)
{
	if (g_destroyed)
		g_signals_after_destroy++;
	{
		std::lock_guard<std::mutex> lock(g_signals_mutex);
		g_signals.push_back({code, g_last_error, g_begin_capture});
	}
	if (auto on_signal = std::exchange(g_on_signal, nullptr))
		on_signal();
}

void obs_register_output_s(const struct obs_output_info *, size_t) {}

bool obs_encoder_get_extra_data(const obs_encoder_t *encoder, uint8_t **data, size_t *size)
{
	const auto &extra = encoder == AUDIO_ENCODER ? g_opus_head : g_h264_init;
	*data = const_cast<uint8_t *>(extra.data());
	*size = extra.size();
	return true;
}

const char *obs_encoder_get_codec(const obs_encoder_t *encoder)
{
	return encoder == AUDIO_ENCODER ? "opus" : "h264";
}

// VideoInit reads coded size and CBR from the encoder. Any new libobs call in
// src/moq-output.cpp needs a stub here or `just obs ci` fails at link.
obs_data_t *obs_encoder_get_settings(const obs_encoder_t *)
{
	return reinterpret_cast<obs_data_t *>(0x4);
}

uint32_t obs_encoder_get_width(const obs_encoder_t *)
{
	return 1920;
}

uint32_t obs_encoder_get_height(const obs_encoder_t *)
{
	return 1080;
}

const char *obs_data_get_string(obs_data_t *, const char *)
{
	return g_rate_control.c_str();
}

long long obs_data_get_int(obs_data_t *, const char *)
{
	return 8000;
}

// The settings a retune builds carry only the bitrate, so the stub is that bitrate.
struct obs_data {
	long long bitrate = 0;
};

obs_data_t *obs_data_create()
{
	return new obs_data;
}

void obs_data_set_int(obs_data_t *data, const char *, long long value)
{
	data->bitrate = value;
}

uint32_t obs_encoder_get_caps(const obs_encoder_t *)
{
	return g_encoder_caps;
}

void obs_encoder_update(obs_encoder_t *encoder, obs_data_t *settings)
{
	std::lock_guard<std::mutex> lock(g_updates_mutex);
	g_updates.emplace_back(encoder, settings->bitrate);
	delete settings;
}

// Encoders outlive every test, so a weak ref is the encoder itself.
obs_weak_encoder_t *obs_encoder_get_weak_encoder(obs_encoder_t *encoder)
{
	return reinterpret_cast<obs_weak_encoder_t *>(encoder);
}

obs_encoder_t *obs_weak_encoder_get_encoder(obs_weak_encoder_t *weak)
{
	return reinterpret_cast<obs_encoder_t *>(weak);
}

obs_encoder_t *obs_encoder_get_ref(obs_encoder_t *encoder)
{
	return encoder;
}

void obs_weak_encoder_addref(obs_weak_encoder_t *) {}
void obs_weak_encoder_release(obs_weak_encoder_t *) {}
void obs_encoder_release(obs_encoder_t *) {}

} // extern "C"

// Stands in for the advanced settings: reconnect fast, so a dropped session is
// back within a test's patience. An unparseable bind address is what moq-ffi refuses
// when it builds the client.
namespace MoQSettings {
std::string Configure(obs_data_t *, moq::ClientConfig &config)
{
	if (!g_settings_ok)
		config.bind = "not an address";
	config.backoff.initial = std::chrono::milliseconds(10);
	config.backoff.max = std::chrono::milliseconds(50);
	return g_settings_error;
}
} // namespace MoQSettings

// -------------------------------------------------------------------- tests

namespace {
int g_failures = 0;

#define CHECK(cond)                                                                 \
	do {                                                                        \
		if (!(cond)) {                                                      \
			fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); \
			g_failures++;                                               \
		}                                                                   \
	} while (0)

obs_output_t *const OUTPUT = reinterpret_cast<obs_output_t *>(0x9);

// A URL moq-ffi rejects as soon as the connect runs, for a failure with no network in it.
const char *const BAD_URL = "not a url";

// Indexing directly turns a missing signal into a segfault, which hides which
// assertion actually regressed.
RecordedSignal signalAt(size_t i)
{
	std::lock_guard<std::mutex> lock(g_signals_mutex);
	return i < g_signals.size() ? g_signals[i] : RecordedSignal{INT_MIN, "<no signal>", -1};
}

size_t signalCount()
{
	std::lock_guard<std::mutex> lock(g_signals_mutex);
	return g_signals.size();
}

void reset(std::string url)
{
	{
		std::lock_guard<std::mutex> lock(g_signals_mutex);
		g_signals.clear();
		g_last_error.clear();
	}
	g_url = std::move(url);
	g_on_signal = nullptr;
	g_begin_capture = 0;
	g_settings_ok = true;
	g_settings_error.clear();
	g_rate_control = "CBR";
	g_encoder_caps = OBS_ENCODER_CAP_DYN_BITRATE;
	{
		std::lock_guard<std::mutex> lock(g_updates_mutex);
		g_updates.clear();
	}
	g_stall_last_error = false;
	g_in_report_window = false;
	g_destroyed = false;
}

encoder_packet videoPacket(std::vector<uint8_t> &payload, int64_t pts)
{
	encoder_packet packet{};
	packet.type = OBS_ENCODER_VIDEO;
	packet.encoder = VIDEO_ENCODER;
	packet.timebase_num = 1;
	packet.timebase_den = 30;
	packet.pts = pts;
	packet.data = payload.data();
	packet.size = payload.size();
	packet.keyframe = true;
	return packet;
}

encoder_packet audioPacket(std::vector<uint8_t> &payload, int64_t pts)
{
	encoder_packet packet{};
	packet.type = OBS_ENCODER_AUDIO;
	packet.encoder = AUDIO_ENCODER;
	packet.timebase_num = 1;
	packet.timebase_den = 48000;
	packet.pts = pts;
	packet.data = payload.data();
	packet.size = payload.size();
	return packet;
}

// The output's broadcast as the relay serves it, or null once the wait times out.
std::shared_ptr<moq::BroadcastConsumer> relayBroadcast(TestRelay &relay)
{
	auto announced = TestOk(relay.origin->consume()->follow("room", std::nullopt), "follow");
	auto available = announced->available();
	if (available.wait_for(std::chrono::seconds(10)) != std::future_status::ready)
		return nullptr;
	return TestOk(available.get(), "available");
}

// The broadcast's catalog, once it has video and audio.
std::optional<moq::Catalog> relayCatalog(const std::shared_ptr<moq::BroadcastConsumer> &broadcast)
{
	if (!broadcast)
		return std::nullopt;
	auto catalogs = TestOk(moq::MediaCatalogConsumer::subscribe(broadcast).get(), "catalog consumer");
	for (;;) {
		auto next = catalogs->next();
		if (next.wait_for(std::chrono::seconds(10)) != std::future_status::ready)
			return std::nullopt;
		auto catalog = TestOk(next.get(), "catalog next");
		if (!catalog)
			return std::nullopt;
		if (!catalog->video.empty() && !catalog->audio.empty())
			return catalog;
	}
}
} // namespace

// The production commit boundary lets a real stats call race retirement without
// callbacks or branches in the output's hot path.
struct MoQOutputStatsTest {
	static std::optional<std::string> Epoch(MoQOutput &output)
	{
		std::shared_ptr<moq::OriginProducer> origin;
		{
			std::lock_guard<std::mutex> lock(output.media_mutex);
			origin = output.origin;
		}
		auto announced = TestOk(origin->consume()->announced(moq::AnnounceConfig{}), "announced");
		auto next = announced->next();
		if (next.wait_for(std::chrono::seconds(10)) != std::future_status::ready)
			return std::nullopt;
		auto event = TestOk(next.get(), "announcement");
		if (!event)
			return std::nullopt;
		const auto *start = std::get_if<moq::AnnounceEvent::kStart>(&event->get_variant());
		return start ? start->announce.route.epoch : std::nullopt;
	}

	static std::shared_ptr<moq::Session> Session(MoQOutput &output)
	{
		std::lock_guard<std::mutex> lock(output.mutex);
		return output.session;
	}
	static bool Commit(MoQOutput &output, const std::shared_ptr<moq::Session> &session,
			   MoQOutput::ConnectionStats *accepted)
	{
		MoQOutput::ConnectionStats stale;
		stale.dial = "retired";
		stale.bytes_sent = 123;
		return output.CommitConnectionStats(session, std::move(stale), accepted);
	}
	static void Disconnect(MoQOutput &output)
	{
		std::lock_guard<std::mutex> lock(output.mutex);
		output.live = false;
	}
	// A fast first connect and drop can coalesce before the UI observes Connected.
	static bool ObserveCoalescedFirstConnect(MoQOutput &output)
	{
		std::lock_guard<std::recursive_mutex> signal_lock(output.signal_mutex);
		auto current = output.attempt;
		if (!current || !current->session)
			return false;
		current->pending.reset();
		current->connected = false;
		output.connect_time_ms = 0;
		auto state = current->session->state();
		state.phase = moq::ConnectionPhase::kDisconnected;
		state.version.reset();
		output.OnState(current, std::move(state));
		return current->connected && !current->live && output.GetConnectTime() > 0;
	}

	// Whether every audio and video track holds a reservation.
	static bool Reserved(MoQOutput &output)
	{
		std::lock_guard<std::mutex> lock(output.media_mutex);
		for (const auto *tracks : {&output.video_tracks, &output.audio_tracks}) {
			if (tracks->empty())
				return false;
			for (const auto &[encoder, track] : *tracks) {
				if (!track.reservation)
					return false;
			}
		}
		return true;
	}
	// Drive every following video track to its floor, as a collapsed uplink would.
	static void Collapse(MoQOutput &output)
	{
		std::lock_guard<std::mutex> lock(output.media_mutex);
		for (auto &[encoder, track] : output.video_tracks) {
			if (track.rate)
				track.rate->Update(1, MoQRate::Clock::now());
		}
	}
};

std::vector<std::pair<obs_encoder_t *, long long>> updates()
{
	std::lock_guard<std::mutex> lock(g_updates_mutex);
	return g_updates;
}

int main()
{
	{
		TestRelay relay(true, {"moq-transport-18"});
		reset(relay.Url());
		MoQOutput output(nullptr, OUTPUT);
		CHECK(output.Start());
		const auto first = MoQOutputStatsTest::Epoch(output);
		CHECK(first.has_value());
		CHECK(first == output.GetEpoch());
		CHECK(WaitFor([&] { return output.IsLiveSession() && relay.Accepted() == 1; }));
		// The plugin names itself ahead of the C++ package, so a relay can tell it apart.
		CHECK(relay.Advertised("moq-obs/dev moq-cpp/"));
		relay.DropSessions();
		CHECK(WaitFor([&] { return output.GetReconnectCount() == 1 && output.IsLiveSession(); }));
		CHECK(MoQOutputStatsTest::Epoch(output) == first);
		CHECK(first == output.GetEpoch());
		output.Stop();
		CHECK(output.GetEpoch().empty());
		CHECK(output.Start());
		const auto second = MoQOutputStatsTest::Epoch(output);
		CHECK(second.has_value());
		CHECK(second != first);
		CHECK(second == output.GetEpoch());
		output.Stop();
	}
	printf("each start announces a fresh epoch, reconnect preserves it: ok\n");

	{
		TestRelay relay;
		reset(relay.Url());
		MoQOutput output(nullptr, OUTPUT);
		CHECK(output.Start());
		CHECK(WaitFor([&] { return output.IsLiveSession(); }));
		CHECK(MoQOutputStatsTest::ObserveCoalescedFirstConnect(output));
		output.Stop();
	}
	printf("a coalesced first connection is remembered while offline: ok\n");

	for (int retirement = 0; retirement < 3; ++retirement) {
		TestRelay relay;
		reset(relay.Url());
		MoQOutput output(nullptr, OUTPUT);
		CHECK(output.Start());
		CHECK(WaitFor([&] { return output.IsLiveSession(); }));
		auto sampled = MoQOutputStatsTest::Session(output);
		CHECK(sampled != nullptr);
		if (!sampled)
			continue;
		// Use the generated bindings and real session before crossing the commit boundary.
		const auto raw = sampled->stats();
		(void)raw;
		MoQOutput::ConnectionStats accepted;
		accepted.dial = "accepted";
		accepted.bytes_sent = 456;
		if (retirement == 0) {
			output.Stop();
		} else if (retirement == 1) {
			output.Stop();
			CHECK(output.Start());
			CHECK(WaitFor([&] { return output.IsLiveSession(); }));
		} else {
			MoQOutputStatsTest::Disconnect(output);
		}
		CHECK(!MoQOutputStatsTest::Commit(output, sampled, &accepted));
		CHECK(accepted.dial == "accepted");
		CHECK(accepted.bytes_sent == 456);
		output.Stop();
	}
	printf("retired stats never replace the accepted sample: ok\n");

	// Publishes over a real session: connects, reports itself live, carries
	// stats, and lands video and audio renditions in the relay's catalog. Only CBR
	// publishes the configured bitrate as a hint.
	for (const char *mode : {"CBR", "VBR"}) {
		TestRelay relay;
		reset(relay.Url());
		g_rate_control = mode;
		{
			MoQOutput o(nullptr, OUTPUT);
			CHECK(o.Start());
			CHECK(WaitFor([&] { return o.IsLiveSession(); }));
			CHECK(o.GetConnectTime() > 0);
			CHECK(o.GetReconnectCount() == 0);

			MoQOutput::ConnectionStats stats;
			CHECK(o.TryGetConnectionStats(&stats));
			CHECK(stats.dial == "http");
			CHECK(!stats.protocol.empty());

			auto keyframe = TestH264Keyframe();
			std::vector<uint8_t> opus = {0xfc, 0xff, 0xfe};
			for (int64_t pts = 0; pts < 3; pts++) {
				auto video = videoPacket(keyframe, pts);
				o.Data(&video);
				auto audio = audioPacket(opus, pts * 960);
				o.Data(&audio);
			}
			CHECK(o.GetTotalBytes() == 3 * (keyframe.size() + opus.size()));

			auto catalog = relayCatalog(relayBroadcast(relay));
			CHECK(catalog.has_value());
			if (catalog) {
				CHECK(catalog->video.size() == 1);
				CHECK(catalog->audio.size() == 1);
				for (const auto &[name, video] : catalog->video) {
					CHECK(video.coded.has_value());
					CHECK(video.bitrate.has_value() == (g_rate_control == "CBR"));
				}
			}

			o.Stop();
			CHECK(signalCount() == 1);
			CHECK(signalAt(0).code == OBS_OUTPUT_SUCCESS);
			CHECK(!o.IsLiveSession());
			CHECK(o.GetConnectTime() == 0);
		}
		// The destructor stops too, which OBS ignores for an output already stopped.
		CHECK(signalCount() == 2);
	}
	printf("publishes, and only CBR hints its bitrate: ok\n");

	// Video follows the grant only when the encoder retunes live and the bitrate
	// drives it; every track reserves either way once the session connects.
	struct FollowCase {
		const char *rate_control;
		uint32_t caps;
		bool follows;
	};
	for (const auto &c : {FollowCase{"CBR", OBS_ENCODER_CAP_DYN_BITRATE, true},
			      FollowCase{"vbr", OBS_ENCODER_CAP_DYN_BITRATE, true},
			      FollowCase{"CQP", OBS_ENCODER_CAP_DYN_BITRATE, false}, FollowCase{"CBR", 0, false}}) {
		TestRelay relay;
		reset(relay.Url());
		g_rate_control = c.rate_control;
		g_encoder_caps = c.caps;
		MoQOutput o(nullptr, OUTPUT);
		CHECK(o.Start());
		CHECK(WaitFor([&] { return o.IsLiveSession(); }));

		auto keyframe = TestH264Keyframe();
		std::vector<uint8_t> opus = {0xfc, 0xff, 0xfe};
		auto video = videoPacket(keyframe, 0);
		o.Data(&video);
		auto audio = audioPacket(opus, 0);
		o.Data(&audio);
		CHECK(MoQOutputStatsTest::Reserved(o));

		MoQOutput::VideoRate rate;
		CHECK(o.TryGetVideoRate(&rate) == c.follows);
		if (c.follows) {
			CHECK(rate.configured_bps == 8'000'000);
			CHECK(rate.target_bps <= rate.configured_bps);
		}
		o.Stop();
	}
	printf("video follows only a live-retunable, bitrate-driven encoder: ok\n");

	// Stopping hands the configured bitrate back to an encoder the grant lowered,
	// so the next Start() reads the user's rate rather than the last grant.
	{
		TestRelay relay;
		reset(relay.Url());
		MoQOutput o(nullptr, OUTPUT);
		CHECK(o.Start());
		CHECK(WaitFor([&] { return o.IsLiveSession(); }));
		auto keyframe = TestH264Keyframe();
		auto video = videoPacket(keyframe, 0);
		o.Data(&video);
		MoQOutputStatsTest::Collapse(o);
		MoQOutput::VideoRate rate;
		CHECK(o.TryGetVideoRate(&rate));
		CHECK(rate.target_bps == 800'000);
		o.Stop();
		const auto applied = updates();
		CHECK(!applied.empty());
		CHECK(!applied.empty() && applied.back() == std::make_pair(VIDEO_ENCODER, 8000LL));
	}
	printf("stop restores the configured bitrate: ok\n");

	// Stop drains the session, even when the output is destroyed straight after: a
	// large frame written just before it still reaches the relay, and the track then
	// ends cleanly instead of being cut off with the session.
	{
		TestRelay relay;
		reset(relay.Url());
		auto o = std::make_unique<MoQOutput>(nullptr, OUTPUT);
		CHECK(o->Start());
		CHECK(WaitFor([&] { return o->IsLiveSession(); }));

		auto keyframe = TestH264Keyframe();
		std::vector<uint8_t> opus = {0xfc, 0xff, 0xfe};
		auto video = videoPacket(keyframe, 0);
		o->Data(&video);
		auto audio = audioPacket(opus, 0);
		o->Data(&audio);

		auto broadcast = relayBroadcast(relay);
		auto catalog = relayCatalog(broadcast);
		CHECK(catalog.has_value());
		if (catalog) {
			const auto &[name, rendition] = *catalog->video.begin();
			auto media = TestOk(
				moq::MediaTrackConsumer::subscribe(broadcast, {moq::Rendition::kVideo{name, rendition}})
					.get(),
				"container consumer");
			// Read a frame first, so the relay is subscribed before the last one is written.
			auto next = media->next();
			CHECK(next.wait_for(std::chrono::seconds(10)) == std::future_status::ready);
			auto first = TestOk(next.get(), "first frame");
			CHECK(first.has_value());
			const auto *first_frame = first ? std::get_if<moq::MediaRead::kFrame>(&first->get_variant())
							: nullptr;
			CHECK(first_frame != nullptr);

			// Large enough to be in flight when Stop() runs, so a Stop() that cancels
			// rather than drains cuts it off (256 KiB can land first under TSan). Small
			// enough to drain well inside moq-net's one-second drain deadline, which cuts
			// the track off by design, even under TSan on a loaded host.
			auto large = keyframe;
			large.resize(large.size() + 2 * 1024 * 1024, 0xff);
			auto last = videoPacket(large, 1);
			o->Data(&last);
			// OBS may destroy the output right after stopping it.
			o->Stop();
			o.reset();

			// Read once the relay has seen the session end, so it serves only what the
			// session delivered before closing.
			auto closed = relay.Session(0)->closed();
			CHECK(closed.wait_for(std::chrono::seconds(10)) == std::future_status::ready);

			// Everything up to the last frame, then the end of the track.
			bool ended = false;
			uint64_t last_timestamp = first_frame ? first_frame->frame.timestamp_us : 0;
			for (;;) {
				next = media->next();
				if (next.wait_for(std::chrono::seconds(10)) != std::future_status::ready)
					break;
				auto result = next.get();
				if (!result)
					break;
				if (!*result) {
					ended = true;
					break;
				}
				CHECK(!std::holds_alternative<moq::MediaRead::kGap>((*result)->get_variant()));
				if (const auto *frame = std::get_if<moq::MediaRead::kFrame>(&(*result)->get_variant()))
					last_timestamp = frame->frame.timestamp_us;
			}
			CHECK(ended);
			CHECK(first_frame && last_timestamp > first_frame->frame.timestamp_us);
		}
	}
	printf("stop drains the session: ok\n");

	// Invalid advanced settings refuse the start before any capture begins.
	{
		reset(BAD_URL);
		g_settings_ok = false;
		MoQOutput o(nullptr, OUTPUT);
		CHECK(!o.Start());
		CHECK(g_begin_capture == 0);
		CHECK(signalCount() == 1);
		CHECK(signalAt(0).code == OBS_OUTPUT_CONNECT_FAILED);
		CHECK(signalAt(0).last_error.find("bind") != std::string::npos);
	}
	{
		reset(BAD_URL);
		g_settings_error = "unknown congestion control: fast";
		MoQOutput o(nullptr, OUTPUT);
		CHECK(!o.Start());
		CHECK(g_begin_capture == 0);
		CHECK(signalCount() == 1);
		CHECK(signalAt(0).code == OBS_OUTPUT_CONNECT_FAILED);
		CHECK(signalAt(0).last_error.find("congestion") != std::string::npos);
	}
	printf("invalid advanced settings: ok\n");

	// Never reached the server, so OBS must not retry the endpoint, and the failure
	// lands after the output was committed, with moq-ffi's reason attached.
	{
		reset(BAD_URL);
		MoQOutput o(nullptr, OUTPUT);
		CHECK(o.Start());
		CHECK(WaitFor([] { return signalCount() == 1; }));
		CHECK(signalAt(0).code == OBS_OUTPUT_CONNECT_FAILED);
		CHECK(signalAt(0).begin_capture_at == 1);
		CHECK(signalAt(0).last_error.find("url") != std::string::npos);
		int code = 0;
		std::string reason;
		o.CopyLastFailure(&code, &reason);
		CHECK(!reason.empty());
		CHECK(!o.IsLiveSession());
		std::this_thread::sleep_for(std::chrono::milliseconds(50));
		CHECK(signalCount() == 1);
	}
	printf("fatal before connect: ok\n");

	// A dropped session reconnects on its own, and the dock sees the count.
	{
		TestRelay relay;
		reset(relay.Url());
		MoQOutput o(nullptr, OUTPUT);
		CHECK(o.Start());
		CHECK(WaitFor([&] { return o.IsLiveSession() && relay.Accepted() == 1; }));
		relay.DropSessions();
		CHECK(WaitFor([&] { return relay.Accepted() == 2; }));
		CHECK(WaitFor([&] { return o.GetReconnectCount() == 1; }));
		MoQOutput::ConnectionStats stats;
		CHECK(WaitFor([&] { return o.TryGetConnectionStats(&stats); }));
		CHECK(signalCount() == 0);
		o.Stop();
	}
	printf("reconnect after a drop: ok\n");

	// While a dropped session redials, the dock names why it dropped instead of a
	// bare "offline". The relay is gone, so nothing answers the redial.
	{
		auto relay = std::make_unique<TestRelay>();
		reset(relay->Url());
		MoQOutput o(nullptr, OUTPUT);
		CHECK(o.Start());
		CHECK(WaitFor([&] { return o.IsLiveSession(); }));
		relay.reset();
		MoQOutput::ConnectionStats stats;
		CHECK(WaitFor([&] { return !o.TryGetConnectionStats(&stats); }));
		// Still reconnecting rather than failed, so this is the reconnect path.
		CHECK(o.IsLiveSession());
		CHECK(signalCount() == 0);
		int code = 0;
		std::string reason;
		o.CopyLastFailure(&code, &reason);
		CHECK(!reason.empty());
		CHECK(reason != "offline");
		o.Stop();
	}
	printf("names the failure while reconnecting: ok\n");

	// While the first dial keeps failing, the dock names why instead of a bare
	// "Connecting…". The relay is gone before the start, so nothing answers.
	{
		std::string url;
		{
			TestRelay relay;
			url = relay.Url();
		}
		reset(url);
		MoQOutput o(nullptr, OUTPUT);
		CHECK(o.Start());
		std::string reason;
		CHECK(WaitFor([&] {
			int code = 0;
			o.CopyLastFailure(&code, &reason);
			return !reason.empty();
		}));
		// Still retrying rather than failed, so this is the first-dial path.
		CHECK(!o.IsLiveSession());
		CHECK(signalCount() == 0);
		o.Stop();
		int code = 0;
		o.CopyLastFailure(&code, &reason);
		CHECK(reason.empty());
	}
	printf("names the failure while the first dial retries: ok\n");

	// Stopping while the connect is still pending cancels it: nothing but the stop
	// reaches OBS, then or later.
	{
		TestRelay relay(false);
		reset(relay.Url());
		MoQOutput o(nullptr, OUTPUT);
		CHECK(o.Start());
		o.Stop();
		std::this_thread::sleep_for(std::chrono::milliseconds(100));
		CHECK(signalCount() == 1);
		CHECK(signalAt(0).code == OBS_OUTPUT_SUCCESS);
	}
	printf("stop during connect: ok\n");

	// A frontend that stops the output straight from the stop signal re-enters
	// Stop() on the worker. It must not deadlock against the report it is inside.
	{
		reset(BAD_URL);
		MoQOutput o(nullptr, OUTPUT);
		g_on_signal = [&o] {
			o.Stop();
		};
		CHECK(o.Start());
		CHECK(WaitFor([] { return signalCount() == 2; }));
		CHECK(signalAt(0).code == OBS_OUTPUT_CONNECT_FAILED);
		CHECK(signalAt(1).code == OBS_OUTPUT_SUCCESS);
	}
	printf("re-entrant Stop from the signal: ok\n");

	// A failure racing a user-initiated Stop must not report afterwards: OBS turns
	// a late OBS_OUTPUT_DISCONNECTED into a reconnect of a stopped stream, because
	// obs_output_signal_stop never checks whether the output is active. Holding the
	// report open shows the Stop() waits for it rather than slipping in between.
	{
		reset(BAD_URL);
		g_stall_last_error = true;
		MoQOutput o(nullptr, OUTPUT);
		CHECK(o.Start());
		CHECK(WaitFor([] { return g_in_report_window.load(); }));
		o.Stop();
		CHECK(signalCount() == 2);
		CHECK(signalAt(0).code == OBS_OUTPUT_CONNECT_FAILED);
		CHECK(signalAt(1).code == OBS_OUTPUT_SUCCESS);
	}
	printf("failure racing user Stop: ok\n");

	// OBS restarts a reconnecting output by calling start again with no stop in
	// between. Each Start() reports its own failure once, after its own commit.
	{
		reset(BAD_URL);
		MoQOutput o(nullptr, OUTPUT);
		CHECK(o.Start());
		CHECK(WaitFor([] { return signalCount() == 1; }));
		CHECK(o.Start());
		CHECK(WaitFor([] { return signalCount() == 2; }));
		CHECK(signalAt(0).begin_capture_at == 1);
		CHECK(signalAt(1).begin_capture_at == 2);
		std::this_thread::sleep_for(std::chrono::milliseconds(50));
		CHECK(signalCount() == 2);
	}
	printf("OBS-driven restart: ok\n");

	// A token edited mid-stream replaces the session's tokens, and presenting a
	// newer one or stopping drops the wait on the relay's answer still in flight.
	{
		TestRelay relay;
		reset(relay.Url());
		MoQOutput o(nullptr, OUTPUT);
		CHECK(!o.PresentToken("early"));
		CHECK(o.Start());
		CHECK(WaitFor([&] { return o.IsLiveSession(); }));
		CHECK(o.PresentToken("fresh"));
		CHECK(o.PresentToken("fresher"));
		// The default protocol carries no in-band AUTH, so the relay's refusal is
		// kept for the dock while the stream stays up.
		CHECK(WaitFor([&] { return !o.TokenRefusal().empty(); }));
		CHECK(o.IsLiveSession());
		CHECK(signalCount() == 0);
		o.Stop();
		CHECK(!o.PresentToken("late"));
	}
	printf("present a token mid-stream: ok\n");

	// Restarts and teardown racing failures still in flight, repeatedly. Whatever
	// the order, each Start() reports at most once, and nothing reports once the
	// output is gone.
	{
		const int rounds = 100;
		for (int round = 0; round < rounds; round++) {
			reset(BAD_URL);
			auto *o = new MoQOutput(nullptr, OUTPUT);
			CHECK(o->Start());
			if (round % 2)
				CHECK(o->Start());
			delete o;
			g_destroyed = true;
			// Start, [Start,] and the destructor's stop.
			const size_t starts = round % 2 ? 2 : 1;
			CHECK(signalCount() <= starts + 1);
		}
		std::this_thread::sleep_for(std::chrono::milliseconds(50));
		CHECK(g_signals_after_destroy == 0);
	}
	printf("failures outliving the output: ok\n");

	// An encoder failure stops the output once, with the encode error.
	{
		reset(BAD_URL);
		MoQOutput o(nullptr, OUTPUT);
		o.Data(nullptr);
		CHECK(signalCount() == 1);
		CHECK(signalAt(0).code == OBS_OUTPUT_ENCODE_ERROR);
	}
	printf("encode error: ok\n");

	if (g_failures) {
		fprintf(stderr, "%d failure(s)\n", g_failures);
		return 1;
	}
	printf("all MoQOutput tests passed\n");
	return 0;
}
