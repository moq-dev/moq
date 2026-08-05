// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once
#include <obs-module.h>

#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstdint>
#include <map>
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
	// Handed to libmoq as the status callback's user_data, so a callback can tell
	// which Start() attempt it belongs to. Freed by the terminal callback.
	struct SessionRef {
		MoQOutput *output;
		uint64_t attempt;
	};

	static void SessionStatus(void *user_data, int code);
	void SessionConnected(uint64_t attempt, int epoch);
	void SessionClosed(uint64_t attempt, int code);

	// Tear down the publish state without telling OBS.
	void Reset();

	void VideoInit(obs_encoder_t *encoder);
	void VideoData(struct encoder_packet *packet);
	void AudioInit(obs_encoder_t *encoder);
	void AudioData(struct encoder_packet *packet);

	obs_output_t *output;

	std::string server_url;
	std::string path;

	size_t total_bytes_sent;
	// Written by the session status callback (libmoq runtime thread), read by
	// GetConnectTime() (OBS thread); atomic to avoid a data race.
	std::atomic<int> connect_time_ms;
	std::chrono::steady_clock::time_point connect_start;

	int origin;
	int broadcast;

	// Session subscription lifetime. libmoq delivers a terminal status callback
	// (code <= 0) asynchronously on its runtime thread after moq_session_close,
	// and may touch `this` until then. outstanding_sessions counts sessions whose
	// terminal callback hasn't fired; the destructor waits for it to reach zero
	// so a late callback can't touch freed memory.
	//
	// The rest of this group is shared with that callback thread, so it is all
	// guarded by session_mutex. Note the handles above are not: only `session`
	// is reachable from a callback.
	std::mutex session_mutex;
	std::condition_variable session_cv;
	int outstanding_sessions;
	// The live session handle, or 0 once retired. libmoq frees the handle before
	// firing the terminal callback and may hand the same id to the next connect,
	// so a retired handle must never be closed.
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
