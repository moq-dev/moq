// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once
#include <obs-module.h>

#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstdint>
#include <map>
#include <memory>
#include <mutex>
#include <string>
#include "logger.h"

class MoQOutput {
public:
	MoQOutput(obs_data_t *settings, obs_output_t *output);
	~MoQOutput();

	bool Start();
	void Stop(bool signal = true);
	void Data(struct encoder_packet *packet);

	inline size_t GetTotalBytes() { return total_bytes_sent; }

	inline int GetConnectTime() { return connect_time_ms; }

private:
	// Handed to libmoq as the status callback's user_data. Carries everything a
	// callback needs about its own Start() attempt, so it never has to read a
	// member the OBS thread may already be rewriting for the next attempt.
	// Freed by the terminal callback.
	struct SessionRef {
		MoQOutput *output;
		uint64_t attempt;
		std::string url;
		std::chrono::steady_clock::time_point started;
	};

	static void SessionStatus(void *user_data, int code);
	void SessionConnected(const SessionRef &ref, int epoch);
	void SessionClosed(const SessionRef &ref, int code);

	// Tell OBS the output stopped. Every obs_output_signal_stop goes through here
	// or through an explicit signal_mutex hold.
	void SignalStop(int code);

	// Tear down the publish state without telling OBS.
	void Reset();

	void VideoInit(obs_encoder_t *encoder);
	void VideoData(struct encoder_packet *packet);
	void AudioInit(obs_encoder_t *encoder);
	void AudioData(struct encoder_packet *packet);

	obs_output_t *output;

	// Serializes reporting the output's fate to OBS against tearing it down, and
	// is held across the OBS calls themselves. Without it the status callback can
	// decide to report a failure, lose the race to Stop(), and still signal:
	// OBS_OUTPUT_DISCONNECTED then makes OBS reconnect a stream the user just
	// stopped, since obs_output_signal_stop never checks whether the output is
	// still active. Start() also holds it from the connect through
	// obs_output_begin_data_capture, so a session that dies mid-startup cannot
	// report against an output that isn't committed yet.
	//
	// Recursive because obs_output_signal_stop and obs_output_begin_data_capture
	// run the frontend's handlers inline, and a frontend may call obs_output_stop
	// straight back into Stop() on this thread.
	//
	// Lock order: signal_mutex first, then session_mutex. Never the reverse, and
	// never take signal_mutex from inside a libmoq call. That keeps it ordered
	// ahead of libmoq's runtime lock too, since the status callback runs with no
	// libmoq locks held.
	//
	// Two costs this buys, both bounded and deliberate:
	// - A frontend that re-enters this output from a *different* thread while we
	//   hold it would deadlock, which recursion does not help with. OBS reaches
	//   one such point, the pthread_join in end_data_capture_internal. Studio's
	//   frontend queues its handlers, so it does not arise there.
	// - libmoq's runtime is single threaded, so a terminal callback parked here
	//   stalls every MoQ session in the process. The holds are short, and the
	//   expensive part of starting (obs_output_initialize_encoders) is
	//   deliberately outside the lock.
	std::recursive_mutex signal_mutex;

	std::string server_url;
	std::string path;

	size_t total_bytes_sent;
	// Written by the session status callback (libmoq runtime thread), read by
	// GetConnectTime() (OBS thread); atomic to avoid a data race.
	std::atomic<int> connect_time_ms;

	int origin;
	int broadcast;

	// Session subscription lifetime. libmoq delivers a terminal status callback
	// (code <= 0) asynchronously on its runtime thread after moq_session_close,
	// and may touch `this` until then. outstanding_sessions counts sessions whose
	// terminal callback hasn't fired; the destructor waits for it to reach zero
	// so a late callback can't touch freed memory.
	//
	// The rest of this group is shared with that callback thread, so it is all
	// guarded by session_mutex. It is also the only state a callback may touch:
	// everything a callback needs about its own attempt is copied into its
	// SessionRef, since the members above belong to whichever attempt is current.
	std::mutex session_mutex;
	std::condition_variable session_cv;
	int outstanding_sessions;
	// The live session handle, or 0 when there is none. libmoq drops the handle
	// before firing the terminal callback, so it is retired there rather than
	// closed later (the close would just fail with "session not found").
	int session;
	// Bumped whenever the publish state is torn down or restarted. A status
	// callback stamped with an older value belongs to a superseded attempt and
	// must leave both OBS and `session` alone, which is also what limits the
	// failure signal to one per Start().
	uint64_t session_attempt;
	// Whether the current attempt ever reached the server, which picks between
	// telling OBS the connection failed and telling it the stream dropped.
	bool session_connected;

	std::map<obs_encoder_t *, int> video_tracks;
	std::map<obs_encoder_t *, int> audio_tracks;
};

void register_moq_output();
