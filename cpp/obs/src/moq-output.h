// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once
#include <obs-module.h>

#include <moq/moq.hpp>

#include <atomic>
#include <chrono>
#include <cstdint>
#include <map>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <vector>
#include "logger.h"
#include "moq-worker.h"

class MoQOutput {
public:
	MoQOutput(obs_data_t *settings, obs_output_t *output);
	~MoQOutput();

	bool Start();
	void Stop(bool signal = true);
	void Data(struct encoder_packet *packet);

	inline size_t GetTotalBytes() { return total_bytes_sent; }

	inline int GetConnectTime() { return connect_time_ms; }

	// Point-in-time QUIC/WebTransport health for the live session. False when
	// there is no session or it is between reconnects (no live connection).
	// A failed read leaves the caller's snapshot unchanged.
	struct ConnectionStats {
		int reconnects = 0;
		bool rtt_valid = false;
		double rtt_ms = 0;
		bool estimated_send_rate_valid = false;
		double estimated_send_rate_bps = 0;
		bool estimated_recv_rate_valid = false;
		double estimated_recv_rate_bps = 0;
		bool bytes_sent_valid = false;
		uint64_t bytes_sent = 0;
		bool loss_valid = false;
		double loss_pct = 0;
		// Negotiated draft name (e.g. moq-lite-05), empty when unavailable.
		std::string protocol;
		// Dial URL scheme (https, wss, …). Not the negotiated carrier when https races.
		std::string dial;
	};
	bool TryGetConnectionStats(ConnectionStats *out);

	// Successful (re)connects after the first for this Start(); 0 until epoch >= 2.
	int GetReconnectCount();

	// True while the current Start() attempt has an open MoQ session.
	// Prefer this over obs_output_get_connect_time_ms, which stays 0 for sub-ms connects.
	bool IsLiveSession();

	// Most recent connect/reconnect failure for this Start(), or empty when none.
	// Thread-safe; the dock polls this while reconnecting and on stop.
	void CopyLastFailure(int *code, std::string *reason);

private:
	// One Start()'s connection: the client dialing, the session once it connects,
	// and the pending call whose continuation reports on it. Replaced wholesale by
	// the next Start() and dropped by Stop(), which cancels that call.
	struct Attempt {
		std::shared_ptr<moq::Client> client;
		std::shared_ptr<moq::Session> session;
		std::optional<moq::Continuation> pending;
		std::string url;
		std::chrono::steady_clock::time_point started;
		// Whether this attempt ever reached the server, which picks between telling
		// OBS the connection failed and telling it the stream dropped.
		bool connected = false;
		// False while the session is between reconnects.
		bool live = false;
	};

	void OnConnect(const std::shared_ptr<Attempt> &attempt, moq::expected<std::shared_ptr<moq::Session>> result);
	void OnStatus(const std::shared_ptr<Attempt> &attempt, moq::expected<moq::ConnectionStatus> result);
	void WatchStatus(const std::shared_ptr<Attempt> &attempt);
	void Fail(const std::shared_ptr<Attempt> &attempt, const moq::Error &error);

	// Runs `callback` on the worker if `attempt` is still the current one once it
	// holds signal_mutex. Cancelling the call on Stop() is not enough on its own: a
	// result that already completed may be queued on the worker by then.
	template<typename Output, typename Callback>
	std::function<void(Output)> Current(const std::shared_ptr<Attempt> &attempt, Callback callback);

	// Tear down the publish state without telling OBS.
	void Reset();

	void VideoInit(obs_encoder_t *encoder);
	void VideoData(struct encoder_packet *packet);
	void AudioInit(obs_encoder_t *encoder);
	void AudioData(struct encoder_packet *packet);

	obs_output_t *output;

	// Runs every continuation, so none runs once the destructor has stopped it.
	MoQWorker worker;

	// Serializes reporting the output's fate to OBS against tearing it down, and is
	// held across the OBS calls themselves. Without it a continuation can decide to
	// report a failure, lose the race to Stop(), and still signal:
	// OBS_OUTPUT_DISCONNECTED then makes OBS reconnect a stream the user just
	// stopped, since obs_output_signal_stop never checks whether the output is still
	// active. Start() also holds it from the connect through
	// obs_output_begin_data_capture, so a session that fails mid-startup cannot
	// report against an output that isn't committed yet.
	//
	// Recursive because obs_output_signal_stop and obs_output_begin_data_capture run
	// the frontend's handlers inline, and a frontend may call obs_output_stop
	// straight back into Stop() on this thread.
	//
	// Lock order: signal_mutex, then mutex, then media_mutex.
	std::recursive_mutex signal_mutex;

	// The current attempt, or null once it was stopped or failed. Guarded by
	// signal_mutex; the fields the dock polls are copied out under `mutex`.
	std::shared_ptr<Attempt> attempt;

	// Guards the group below, which the dock reads from the UI thread.
	std::mutex mutex;
	std::shared_ptr<moq::Session> session;
	bool connected = false;
	bool live = false;
	std::string url;
	int last_failure_code = 0;
	std::string last_failure_reason;

	// Written on the worker, read by GetConnectTime().
	std::atomic<int> connect_time_ms{0};

	// Guards the publishing side, which the encoder threads write through Data().
	// Never held across an OBS call that can wait on an encoder thread.
	std::mutex media_mutex;
	std::shared_ptr<moq::OriginProducer> origin;
	std::shared_ptr<moq::BroadcastProducer> broadcast;
	// An encoder maps to null when its track failed to initialize, so it isn't retried.
	std::map<obs_encoder_t *, std::shared_ptr<moq::MediaProducer>> video_tracks;
	std::map<obs_encoder_t *, std::shared_ptr<moq::MediaProducer>> audio_tracks;

	// Retired sessions still draining their finished tracks, which the destructor
	// waits out. Guarded by signal_mutex.
	std::vector<moq::Future<void>> draining;

	std::string path;

	size_t total_bytes_sent;
};

void register_moq_output();
