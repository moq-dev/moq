// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once

#include <chrono>
#include <condition_variable>
#include <mutex>

// Lifetime bridge for MoQDock::OnOutputStopped. OBS may defer
// signal_handler_disconnect while a stop signal is still dispatching, so a
// queued Qt lambda can outlive the dock unless we track in-flight work and
// refuse new work once destruction starts.
struct MoQDockStopBridge {
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
