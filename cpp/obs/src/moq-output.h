// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once
#include <obs-module.h>

#include <atomic>
#include <chrono>
#include <condition_variable>
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
	int session;
	int broadcast;

	// Session subscription lifetime. libmoq delivers a terminal status callback
	// (code <= 0) asynchronously on its runtime thread after moq_session_close,
	// and may touch `this` until then. outstanding_sessions counts sessions whose
	// terminal callback hasn't fired; the destructor waits for it to reach zero
	// so a late callback can't touch freed memory.
	std::mutex session_mutex;
	std::condition_variable session_cv;
	int outstanding_sessions;

	std::map<obs_encoder_t *, int> video_tracks;
	std::map<obs_encoder_t *, int> audio_tracks;
};

void register_moq_output();
