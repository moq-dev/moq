// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once

#include <atomic>
#include <cstdint>
#include <chrono>
#include <condition_variable>
#include <mutex>

// Tracks OBS-thread work in MoQDock::OnOutputStopped. Queued Qt work must
// not hold an activity reference: dock destruction runs on that event loop.
struct MoQDockStopBridge {
private:
	std::atomic<uint64_t> generation{0};

public:
	uint64_t stopTicket() const { return generation.load(std::memory_order_relaxed); }
	bool acceptsStop(uint64_t ticket) const { return ticket == stopTicket(); }
	void invalidateStops() { generation.fetch_add(1, std::memory_order_relaxed); }

	std::mutex mutex;
	std::condition_variable cv;
	int active = 0;
	bool closing = false;

	// True when the caller may proceed (and must later call end()).
	bool begin()
	{
		std::lock_guard<std::mutex> lock(mutex);
		if (closing)
			return false;
		active++;
		return true;
	}

	void end()
	{
		std::lock_guard<std::mutex> lock(mutex);
		if (active > 0)
			active--;
		cv.notify_all();
	}

	void markClosing()
	{
		std::lock_guard<std::mutex> lock(mutex);
		closing = true;
		cv.notify_all();
	}

	void waitIdle(std::chrono::milliseconds timeout)
	{
		std::unique_lock<std::mutex> lock(mutex);
		cv.wait_for(lock, timeout, [this] { return active == 0; });
	}

	void closeAndWait(std::chrono::milliseconds timeout)
	{
		markClosing();
		waitIdle(timeout);
	}
};
