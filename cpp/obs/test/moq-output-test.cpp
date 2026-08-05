// SPDX-License-Identifier: GPL-2.0-or-later
//
// Drives the real MoQOutput against stubbed libobs and libmoq, so the session
// status callback's orderings can be forced instead of waited for. Everything
// here is timing-sensitive in production: a terminal callback arrives on the
// libmoq runtime thread at a moment we don't control, possibly during Start(),
// during a restart, or during destruction.
//
// Run with `just obs test`. This is not part of the plugin build.
#include <atomic>
#include <chrono>
#include <cstdarg>
#include <climits>
#include <cstdio>
#include <functional>
#include <string>
#include <thread>
#include <vector>

#include <obs.h>
#include <obs-module.h>

extern "C" {
#include "moq.h"
}

// ------------------------------------------------------------- libobs stubs

namespace {
struct RecordedSignal {
	int code;
	std::string last_error;
};

std::vector<RecordedSignal> g_signals;
std::string g_last_error;
int g_begin_capture = 0;
// Lets a test run something inside obs_output_signal_stop, standing in for a
// frontend that stops the output straight from the signal handler.
std::function<void()> g_on_signal;
// Released from inside Start(), just before it rewrites the members a stale
// callback might read, so the two genuinely overlap.
std::atomic<bool> *g_start_gate = nullptr;
} // namespace

extern "C" {

// Deliberately silent: stdio locking would create happens-before edges between
// the OBS and libmoq threads and mask exactly the races we're looking for.
void blog(int, const char *, ...) {}

obs_service_t *obs_output_get_service(const obs_output_t *)
{
	if (g_start_gate)
		g_start_gate->store(true, std::memory_order_relaxed);
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
	return type == OBS_SERVICE_CONNECT_INFO_SERVER_URL ? "https://relay.example/anon" : "room";
}

obs_encoder_t *obs_output_get_video_encoder2(const obs_output_t *, size_t idx)
{
	return idx == 0 ? reinterpret_cast<obs_encoder_t *>(0x2) : nullptr;
}

bool obs_output_begin_data_capture(obs_output_t *, uint32_t)
{
	g_begin_capture++;
	return true;
}

void obs_output_set_last_error(obs_output_t *, const char *message)
{
	g_last_error = message ? message : "";
}

void obs_output_signal_stop(obs_output_t *, int code)
{
	g_signals.push_back({code, g_last_error});
	if (g_on_signal)
		g_on_signal();
}

void obs_register_output_s(const struct obs_output_info *, size_t) {}

bool obs_encoder_get_extra_data(const obs_encoder_t *, uint8_t **, size_t *)
{
	return false;
}

const char *obs_encoder_get_codec(const obs_encoder_t *)
{
	return "h264";
}

} // extern "C"

// ------------------------------------------------------------- libmoq stubs

namespace {
void (*g_on_status)(void *, int32_t) = nullptr;
void *g_user_data = nullptr;
std::atomic<int> g_next_handle{10};
std::atomic<int> g_last_handle{0};
std::atomic<int> g_closed_handle{0};
// libmoq is allowed to deliver the terminal before connect returns.
std::atomic<bool> g_connect_fires_terminal{false};
const char *g_error = "unauthorized";
} // namespace

extern "C" {

const char *moq_error(void)
{
	return g_error;
}

int32_t moq_origin_create(void)
{
	return 1;
}

int32_t moq_origin_close(uint32_t)
{
	return 0;
}

int32_t moq_origin_publish(uint32_t, const char *, size_t)
{
	return 5;
}

int32_t moq_publish_finish(uint32_t)
{
	return 0;
}

int32_t moq_publish_media(uint32_t, const char *, size_t, const uint8_t *, size_t)
{
	return 7;
}

int32_t moq_publish_media_finish(uint32_t)
{
	return 0;
}

int32_t moq_publish_media_frame(uint32_t, const uint8_t *, uintptr_t, uint64_t)
{
	return 0;
}

int32_t moq_session_connect(const char *, size_t, uint32_t, uint32_t, void (*on_status)(void *, int32_t),
			    void *user_data)
{
	g_on_status = on_status;
	g_user_data = user_data;
	int handle = g_next_handle++;
	g_last_handle = handle;
	if (g_connect_fires_terminal) {
		g_on_status(g_user_data, -34);
		g_on_status = nullptr;
	}
	return handle;
}

int32_t moq_session_close(uint32_t session)
{
	g_closed_handle = static_cast<int>(session);
	return 0;
}

} // extern "C"

// -------------------------------------------------------------------- tests

#include "moq-output.h"

namespace {
int g_failures = 0;

// Indexing g_signals directly turns a missing signal into a segfault, which
// hides which assertion actually regressed.
RecordedSignal signalAt(size_t i)
{
	return i < g_signals.size() ? g_signals[i] : RecordedSignal{INT_MIN, "<no signal>"};
}

void fire(int code)
{
	auto cb = g_on_status;
	auto ud = g_user_data;
	if (!cb) {
		fprintf(stderr, "FAIL: no status callback registered\n");
		g_failures++;
		return;
	}
	if (code <= 0)
		g_on_status = nullptr;
	cb(ud, code);
}

void reset()
{
	g_signals.clear();
	g_last_error.clear();
	g_on_signal = nullptr;
	g_on_status = nullptr;
	g_closed_handle = 0;
	g_begin_capture = 0;
	g_start_gate = nullptr;
	g_connect_fires_terminal = false;
}
} // namespace

#define CHECK(cond)                                                                 \
	do {                                                                        \
		if (!(cond)) {                                                      \
			fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); \
			g_failures++;                                               \
		}                                                                   \
	} while (0)

int main()
{
	auto out = reinterpret_cast<obs_output_t *>(0x9);

	// Reconnection gave up after the session had been up: OBS must be told, with
	// the libmoq reason attached, so it stops reporting a live stream.
	{
		reset();
		MoQOutput o(nullptr, out);
		CHECK(o.Start());
		fire(1);
		fire(-34);
		CHECK(g_signals.size() == 1);
		CHECK(signalAt(0).code == OBS_OUTPUT_DISCONNECTED);
		CHECK(signalAt(0).last_error == "unauthorized");
	}
	printf("fatal after connect: ok\n");

	// Never reached the server, so OBS should not retry the endpoint.
	{
		reset();
		MoQOutput o(nullptr, out);
		CHECK(o.Start());
		fire(-34);
		CHECK(g_signals.size() == 1);
		CHECK(signalAt(0).code == OBS_OUTPUT_CONNECT_FAILED);
	}
	printf("fatal before connect: ok\n");

	// A clean close is the expected end of a stopped output, not a failure.
	{
		reset();
		MoQOutput o(nullptr, out);
		CHECK(o.Start());
		fire(1);
		o.Stop(false);
		fire(0);
		CHECK(g_signals.empty());
	}
	printf("clean close: ok\n");

	// A terminal belonging to an attempt that has already been torn down must
	// not stop the stream that replaced it.
	{
		reset();
		MoQOutput o(nullptr, out);
		CHECK(o.Start());
		auto stale_cb = g_on_status;
		auto stale_ud = g_user_data;
		o.Stop(false);
		CHECK(o.Start());
		stale_cb(stale_ud, -34);
		CHECK(g_signals.empty());
		int live = g_last_handle;
		g_closed_handle = 0;
		o.Stop(false);
		CHECK(g_closed_handle == live);
		fire(0);
	}
	printf("superseded terminal ignored: ok\n");

	// The terminal can beat moq_session_connect's return. Start() must refuse
	// rather than capture into a session that is already gone, and the dead
	// handle must never be closed.
	{
		reset();
		g_connect_fires_terminal = true;
		MoQOutput o(nullptr, out);
		CHECK(!o.Start());
		CHECK(g_begin_capture == 0);
		CHECK(g_signals.size() == 1);
		CHECK(signalAt(0).code == OBS_OUTPUT_CONNECT_FAILED);
		g_closed_handle = 0;
		o.Stop(false);
		CHECK(g_closed_handle == 0);
	}
	printf("terminal before connect returns: ok\n");

	// A frontend that stops the output straight from the stop signal re-enters
	// Stop() on the libmoq thread. It must not deadlock against session_mutex.
	{
		reset();
		MoQOutput o(nullptr, out);
		bool reentered = false;
		g_on_signal = [&o, &reentered] {
			if (reentered)
				return;
			reentered = true;
			o.Stop(true);
		};
		CHECK(o.Start());
		fire(1);
		fire(-34);
		CHECK(g_signals.size() == 2);
		CHECK(signalAt(0).code == OBS_OUTPUT_DISCONNECTED);
		CHECK(signalAt(1).code == OBS_OUTPUT_SUCCESS);
	}
	printf("re-entrant Stop from the signal: ok\n");

	// The destructor waits for the terminal callback. It must return on the
	// callback rather than the bounded-wait timeout, and the callback must not
	// signal an output that is being destroyed.
	{
		reset();
		auto o = new MoQOutput(nullptr, out);
		CHECK(o->Start());
		fire(1);
		auto cb = g_on_status;
		auto ud = g_user_data;
		g_on_status = nullptr;

		std::thread late([cb, ud] {
			std::this_thread::sleep_for(std::chrono::milliseconds(200));
			cb(ud, -34);
		});
		auto start = std::chrono::steady_clock::now();
		delete o;
		auto elapsed = std::chrono::steady_clock::now() - start;
		late.join();
		CHECK(elapsed > std::chrono::milliseconds(100));
		CHECK(elapsed < std::chrono::seconds(2));
		CHECK(g_signals.size() == 1);
		CHECK(signalAt(0).code == OBS_OUTPUT_SUCCESS);
	}
	printf("terminal during destruction: ok\n");

	// OBS restarts a reconnecting output by calling start again with no stop in
	// between, so Start() has to drop the previous attempt itself.
	{
		reset();
		MoQOutput o(nullptr, out);
		CHECK(o.Start());
		fire(1);
		fire(-34);
		CHECK(g_signals.size() == 1);
		g_closed_handle = 0;
		CHECK(o.Start());
		CHECK(g_closed_handle == 0); // the dead handle is retired, not closed
		fire(-34);
		CHECK(g_signals.size() == 2);
		CHECK(signalAt(1).code == OBS_OUTPUT_CONNECT_FAILED);
	}
	printf("OBS-driven restart: ok\n");

	// The dock reads the connect time as the live "connected" indicator, so it
	// must not outlive the attempt it describes.
	{
		reset();
		MoQOutput o(nullptr, out);
		CHECK(o.Start());
		CHECK(o.GetConnectTime() == 0);
		fire(1);
		std::this_thread::sleep_for(std::chrono::milliseconds(2));
		fire(2); // a reconnect epoch, so the elapsed time is non-zero
		CHECK(o.GetConnectTime() > 0);
		fire(-34);
		CHECK(o.GetConnectTime() == 0);
		CHECK(o.Start());
		CHECK(o.GetConnectTime() == 0);
		fire(1);
		o.Stop(false);
		CHECK(o.GetConnectTime() == 0);
		fire(0);
	}
	printf("connect time cleared on teardown: ok\n");

	// The terminal callback racing a user-initiated stop, repeatedly. Whichever
	// wins, the failure signal fires at most once per Start().
	{
		reset();
		const int rounds = 200;
		for (int i = 0; i < rounds; i++) {
			MoQOutput o(nullptr, out);
			o.Start();
			auto cb = g_on_status;
			auto ud = g_user_data;
			g_on_status = nullptr;
			std::thread terminal([cb, ud] { cb(ud, -34); });
			o.Stop(false);
			terminal.join();
		}
		size_t fatal = 0, success = 0;
		for (auto &s : g_signals) {
			if (s.code == OBS_OUTPUT_SUCCESS)
				success++;
			else if (s.code == OBS_OUTPUT_DISCONNECTED || s.code == OBS_OUTPUT_CONNECT_FAILED)
				fatal++;
			else
				CHECK(false);
		}
		// Exactly one SUCCESS per round from the destructor's Stop(), plus a fatal
		// only where the terminal beat Stop() to the attempt.
		CHECK(success == static_cast<size_t>(rounds));
		CHECK(g_signals.size() == success + fatal);
		printf("terminal racing Stop: ok (%zu of %d rounds signalled)\n", fatal, rounds);
	}

	// A stale terminal callback overlapping the next Start(), which rewrites the
	// members the callback used to read. Exercises the interleaving; note it is
	// not a proven detector, since session_mutex tends to order the two in
	// practice even when nothing guarantees it.
	{
		reset();
		MoQOutput o(nullptr, out);
		for (int i = 0; i < 500; i++) {
			o.Start();
			auto cb = g_on_status;
			auto ud = g_user_data;
			g_on_status = nullptr;
			std::atomic<bool> gate{false};
			std::thread stale([cb, ud, &gate] {
				while (!gate.load(std::memory_order_relaxed))
					;
				cb(ud, -34);
			});
			o.Stop(false);
			g_start_gate = &gate;
			o.Start();
			g_start_gate = nullptr;
			stale.join();

			auto live = g_on_status;
			auto live_ud = g_user_data;
			g_on_status = nullptr;
			o.Stop(false);
			live(live_ud, 0);
		}
	}
	printf("stale terminal racing restart: ok\n");

	if (g_failures) {
		printf("\nFAILURES: %d\n", g_failures);
		return 1;
	}
	printf("\nall passed\n");
	return 0;
}
