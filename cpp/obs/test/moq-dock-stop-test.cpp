// SPDX-License-Identifier: GPL-2.0-or-later
//
// Regression for the OnOutputStopped teardown race: OBS can deliver a deferred
// stop callback after MoQDock destruction starts. The bridge must refuse new
// work once closing, and closeAndWait must observe in-flight end().

#include "moq-dock-stop.h"

#include <atomic>
#include <chrono>
#include <cstdio>
#include <thread>

#define CHECK(cond)                                                                         \
	do {                                                                                \
		if (!(cond)) {                                                              \
			std::fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); \
			return 1;                                                           \
		}                                                                           \
	} while (0)

int main()
{
	{
		MoQDockStopBridge bridge;
		CHECK(bridge.begin());
		CHECK(bridge.active == 1);
		bridge.end();
		CHECK(bridge.active == 0);
		bridge.closeAndWait(std::chrono::milliseconds(50));
		CHECK(bridge.closing);
		CHECK(!bridge.begin());
	}
	printf("close refuses new begin: ok\n");

	{
		MoQDockStopBridge bridge;
		CHECK(bridge.begin());
		std::atomic<bool> ended{false};
		std::thread worker([&] {
			std::this_thread::sleep_for(std::chrono::milliseconds(30));
			bridge.end();
			ended.store(true);
		});
		const auto start = std::chrono::steady_clock::now();
		bridge.closeAndWait(std::chrono::milliseconds(500));
		const auto elapsed = std::chrono::steady_clock::now() - start;
		worker.join();
		CHECK(ended.load());
		CHECK(bridge.active == 0);
		CHECK(elapsed >= std::chrono::milliseconds(20));
		CHECK(elapsed < std::chrono::milliseconds(400));
		CHECK(!bridge.begin());
	}
	printf("close waits for in-flight end: ok\n");

	{
		MoQDockStopBridge bridge;
		CHECK(bridge.begin());
		bridge.markClosing();
		CHECK(bridge.closing);
		CHECK(bridge.active == 1);
		bridge.end();
		bridge.waitIdle(std::chrono::milliseconds(50));
		CHECK(bridge.active == 0);
		CHECK(!bridge.begin());
	}
	printf("markClosing then waitIdle: ok\n");

	{
		MoQDockStopBridge bridge;
		bridge.closeAndWait(std::chrono::milliseconds(10));
		CHECK(!bridge.begin());
		// end() after a refused begin must stay non-negative.
		bridge.end();
		CHECK(bridge.active == 0);
	}
	printf("end after close is a no-op: ok\n");

	{
		MoQDockStopBridge bridge;
		int reusedOutputStorage = 0;
		int *output = &reusedOutputStorage;
		const auto oldTicket = bridge.stopTicket();
		auto queuedStop = [&](uint64_t ticket) {
			if (bridge.acceptsStop(ticket)) {
				output = nullptr;
				bridge.invalidateStops();
			}
		};
		// Stop and restart can reuse the exact output address before Qt dispatches.
		bridge.invalidateStops();
		output = &reusedOutputStorage;
		const auto currentTicket = bridge.stopTicket();
		queuedStop(oldTicket);
		CHECK(output == &reusedOutputStorage);
		CHECK(bridge.acceptsStop(currentTicket));
		queuedStop(currentTicket);
		CHECK(output == nullptr);
		CHECK(!bridge.acceptsStop(currentTicket));
	}
	printf("queued stops reject reused output addresses: ok\n");

	printf("\nall passed\n");
	return 0;
}
