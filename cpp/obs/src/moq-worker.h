// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once

#include <moq/moq.hpp>

#include <condition_variable>
#include <deque>
#include <memory>
#include <mutex>
#include <thread>
#include <utility>

// The thread one output or source runs its moq continuations on, in order.
//
// Owning one per object keeps continuations off the moq-ffi runtime, lets a slow
// one (an FFmpeg decode, an OBS signal the frontend handles inline) delay only its
// own object, and gives teardown a hard boundary: once Stop() returns from another
// thread, none of this object's continuations is running or will run.
class MoQWorker {
public:
	MoQWorker() : state(std::make_shared<State>()), thread([state = state] { Run(*state); }) {}

	~MoQWorker() { Stop(); }

	MoQWorker(const MoQWorker &) = delete;
	MoQWorker &operator=(const MoQWorker &) = delete;

	// The executor to hand Future::then. It refuses tasks once stopped, which drops them.
	moq::Executor Executor() const
	{
		return [state = state](moq::Task task) {
			std::lock_guard<std::mutex> lock(state->mutex);
			if (!state->accepting)
				return false;
			state->tasks.push_back(std::move(task));
			state->ready.notify_one();
			return true;
		};
	}

	// Drops the queued tasks and waits out a running one. Called from a continuation it
	// cannot wait for itself, so it only stops accepting and lets the thread finish.
	void Stop()
	{
		std::deque<moq::Task> dropped;
		{
			std::lock_guard<std::mutex> lock(state->mutex);
			state->accepting = false;
			dropped.swap(state->tasks);
		}
		state->ready.notify_all();
		// Released outside the lock: a task's captures may own moq objects.
		dropped.clear();

		if (!thread.joinable())
			return;
		if (thread.get_id() == std::this_thread::get_id())
			thread.detach();
		else
			thread.join();
	}

private:
	struct State {
		std::mutex mutex;
		std::condition_variable ready;
		std::deque<moq::Task> tasks;
		bool accepting = true;
	};

	static void Run(State &state)
	{
		for (;;) {
			moq::Task task;
			{
				std::unique_lock<std::mutex> lock(state.mutex);
				state.ready.wait(lock, [&] { return !state.tasks.empty() || !state.accepting; });
				if (!state.accepting)
					return;
				task = std::move(state.tasks.front());
				state.tasks.pop_front();
			}
			task();
		}
	}

	// Shared with the thread and every executor copy, so a late post after the owner
	// is gone finds a closed queue instead of freed memory.
	std::shared_ptr<State> state;
	std::thread thread;
};
