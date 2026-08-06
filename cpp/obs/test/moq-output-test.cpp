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
#include <mutex>
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
	// How many times capture had begun when this was signalled, so a test can tell
	// "committed the output, then reported" from "reported, then committed".
	int begin_capture_at;
};

// The plugin serializes its own signalling, but the tests read these from a
// different thread than the callback writes them, so the stubs carry their own
// lock. Always go through the helpers below.
std::mutex g_signals_mutex;
std::vector<RecordedSignal> g_signals;
std::string g_last_error;
std::atomic<int> g_begin_capture{0};
// Lets a test run something inside obs_output_signal_stop, standing in for a
// frontend that stops the output straight from the signal handler.
std::function<void()> g_on_signal;
// Released from inside Start(), just before it rewrites the members a stale
// callback might read, so the two genuinely overlap.
std::atomic<bool> *g_start_gate = nullptr;
// obs_output_set_last_error sits between deciding to report a failure and
// reporting it, so the stub announces that it is in that window and then holds
// it open. A test can use this to drive a concurrent Stop() into the gap: with
// signal_mutex held across the whole report the Stop() blocks, and without it
// the stale report lands after the stop, which is the bug.
std::atomic<bool> g_stall_last_error{false};
std::atomic<bool> g_in_report_window{false};
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
	{
		std::lock_guard<std::mutex> lock(g_signals_mutex);
		g_last_error = message ? message : "";
	}
	if (g_stall_last_error) {
		g_in_report_window.store(true, std::memory_order_relaxed);
		std::this_thread::sleep_for(std::chrono::milliseconds(5));
	}
}

void obs_output_signal_stop(obs_output_t *, int code)
{
	{
		std::lock_guard<std::mutex> lock(g_signals_mutex);
		g_signals.push_back({code, g_last_error, g_begin_capture});
	}
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
// libmoq is allowed to deliver the terminal before connect returns. "inline" is
// the defensive case (same thread); "thread" is what libmoq actually does, from
// its runtime thread, which is the ordering signal_mutex has to handle.
std::atomic<bool> g_connect_fires_terminal{false};
std::atomic<bool> g_connect_fires_terminal_threaded{false};
std::thread g_connect_terminal_thread;
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
	} else if (g_connect_fires_terminal_threaded) {
		auto cb = g_on_status;
		auto ud = g_user_data;
		g_on_status = nullptr;
		// One connect per test with this flag set: reset() joins between tests, and
		// moving onto a still-joinable thread would call std::terminate. Joining
		// here instead would deadlock, since Start() holds signal_mutex and the
		// terminal callback needs it.
		g_connect_terminal_thread = std::thread([cb, ud] { cb(ud, -34); });
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

// Spin until `ready`, giving up after a few seconds. A test that hangs reports
// nothing; one that fails names the assertion.
bool spinUntil(const std::atomic<bool> &ready)
{
	auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(5);
	while (!ready.load(std::memory_order_relaxed)) {
		if (std::chrono::steady_clock::now() > deadline)
			return false;
		std::this_thread::yield();
	}
	return true;
}

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

std::vector<RecordedSignal> signalsSince(size_t from)
{
	std::lock_guard<std::mutex> lock(g_signals_mutex);
	return {g_signals.begin() + static_cast<long>(from), g_signals.end()};
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
	if (g_connect_terminal_thread.joinable())
		g_connect_terminal_thread.join();
	{
		std::lock_guard<std::mutex> lock(g_signals_mutex);
		g_signals.clear();
		g_last_error.clear();
	}
	g_on_signal = nullptr;
	g_on_status = nullptr;
	g_closed_handle = 0;
	g_begin_capture = 0;
	g_start_gate = nullptr;
	g_connect_fires_terminal = false;
	g_connect_fires_terminal_threaded = false;
	g_stall_last_error = false;
	g_in_report_window = false;
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
		CHECK(signalCount() == 1);
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
		CHECK(signalCount() == 1);
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
		CHECK(signalCount() == 0);
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
		CHECK(signalCount() == 0);
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
		CHECK(signalCount() == 1);
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
		CHECK(signalCount() == 2);
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
		// Comfortably under the destructor's own 2s bounded wait, so this fails if
		// teardown returned on that timeout instead of on the callback.
		CHECK(elapsed < std::chrono::milliseconds(1500));
		CHECK(signalCount() == 1);
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
		CHECK(signalCount() == 1);
		g_closed_handle = 0;
		CHECK(o.Start());
		CHECK(g_closed_handle == 0); // the dead handle is retired, not closed
		fire(-34);
		CHECK(signalCount() == 2);
		CHECK(signalAt(1).code == OBS_OUTPUT_CONNECT_FAILED);
	}
	printf("OBS-driven restart: ok\n");

	// libmoq delivers the terminal on its own thread while Start() is still
	// committing. It must not report against a half-started output: signal_mutex
	// parks it until capture has begun, so OBS sees a started-then-stopped output
	// rather than an active one with no session.
	{
		reset();
		g_connect_fires_terminal_threaded = true;
		MoQOutput o(nullptr, out);
		CHECK(o.Start());
		if (g_connect_terminal_thread.joinable())
			g_connect_terminal_thread.join();
		// Capture began before the report, so OBS's normal stop path applies.
		// The report must land after capture began, so OBS sees an active output and
		// runs its normal stop path instead of one that was never committed.
		CHECK(signalAt(0).begin_capture_at == 1);
		CHECK(signalCount() == 1);
		CHECK(signalAt(0).code == OBS_OUTPUT_CONNECT_FAILED);
		CHECK(signalAt(0).last_error == "unauthorized");
	}
	printf("terminal during startup commit: ok\n");

	// A terminal racing a user-initiated Stop must not report afterwards: OBS
	// turns a late OBS_OUTPUT_DISCONNECTED into a reconnect of a stopped stream,
	// because obs_output_signal_stop never checks whether the output is active.
	{
		reset();
		g_stall_last_error = true;
		const int rounds = 50;
		int reconnect_after_stop = 0;
		for (int i = 0; i < rounds; i++) {
			MoQOutput o(nullptr, out);
			CHECK(o.Start());
			fire(1); // connected, so a failure would ask OBS to reconnect
			auto cb = g_on_status;
			auto ud = g_user_data;
			if (!cb)
				break;
			g_on_status = nullptr;

			size_t before = signalCount();
			g_in_report_window = false;
			std::thread terminal([cb, ud] { cb(ud, -34); });

			// Wait until the callback has committed to reporting a failure and is
			// sitting in the window, then stop from underneath it.
			CHECK(spinUntil(g_in_report_window));
			o.Stop(true); // the user pressed stop
			terminal.join();

			// Whatever the interleaving, a DISCONNECTED must never follow the
			// SUCCESS that reported the user's stop.
			bool stopped = false;
			for (const auto &s : signalsSince(before)) {
				if (s.code == OBS_OUTPUT_SUCCESS)
					stopped = true;
				else if (stopped && s.code == OBS_OUTPUT_DISCONNECTED)
					reconnect_after_stop++;
			}
		}
		CHECK(reconnect_after_stop == 0);
		printf("terminal racing user Stop: ok (%d late reconnect signals over %d rounds)\n",
		       reconnect_after_stop, rounds);
	}

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
			CHECK(o.Start());
			auto cb = g_on_status;
			auto ud = g_user_data;
			if (!cb)
				break;
			g_on_status = nullptr;
			std::thread terminal([cb, ud] { cb(ud, -34); });
			o.Stop(false);
			terminal.join();
		}
		size_t fatal = 0, success = 0;
		for (const auto &s : signalsSince(0)) {
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
		CHECK(signalCount() == success + fatal);
		printf("terminal racing Stop: ok (%zu of %d rounds signalled)\n", fatal, rounds);
	}

	// A stale terminal callback overlapping the next Start(), which rewrites the
	// members the callback used to read. Exercises the interleaving; note it is
	// not a proven detector, since session_mutex tends to order the two in
	// practice even when nothing guarantees it.
	{
		reset();
		MoQOutput o(nullptr, out);
		std::atomic<bool> stranded{false};
		for (int i = 0; i < 500; i++) {
			CHECK(o.Start());
			auto cb = g_on_status;
			auto ud = g_user_data;
			if (!cb)
				break;
			g_on_status = nullptr;
			std::atomic<bool> gate{false};
			std::thread stale([cb, ud, &gate, &stranded] {
				// The gate opens from inside the next Start(); if that never
				// happens, give up rather than hanging the run.
				if (!spinUntil(gate)) {
					stranded = true;
					return;
				}
				cb(ud, -34);
			});
			o.Stop(false);
			g_start_gate = &gate;
			bool restarted = o.Start();
			g_start_gate = nullptr;
			stale.join();
			CHECK(restarted);

			auto live = g_on_status;
			auto live_ud = g_user_data;
			if (!live)
				break;
			g_on_status = nullptr;
			o.Stop(false);
			live(live_ud, 0);
		}
		CHECK(!stranded);
	}
	printf("stale terminal racing restart: ok\n");

	if (g_failures) {
		printf("\nFAILURES: %d\n", g_failures);
		return 1;
	}
	printf("\nall passed\n");
	return 0;
}
