// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once
#include <obs-module.h>

#include <atomic>
#include <chrono>
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

	inline int GetConnectTime() { return state->connect_time_ms; }

	// Point-in-time QUIC/WebTransport health for the live session. False when
	// there is no session or libmoq is between reconnects (no live connection).
	// A failed read leaves the caller's snapshot unchanged.
	struct ConnectionStats {
		int reconnects = 0;
		bool rtt_valid = false;
		double rtt_ms = 0;
		bool send_rate_valid = false;
		double send_rate_bps = 0;
		bool recv_rate_valid = false;
		double recv_rate_bps = 0;
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
	inline int GetReconnectCount()
	{
		const int epoch = state->epoch.load(std::memory_order_relaxed);
		return epoch > 1 ? epoch - 1 : 0;
	}

	// True while the current Start() attempt has an open MoQ session.
	// Prefer this over obs_output_get_connect_time_ms, which stays 0 for sub-ms connects.
	bool IsLiveSession();

	// Most recent connect/reconnect failure for this Start(), or empty when none.
	// Thread-safe; the dock polls this while reconnecting and on stop.
	void CopyLastFailure(int *code, std::string *reason);

private:
	struct SessionRef;

	// Every piece of state a libmoq status callback touches, held by shared_ptr
	// so it outlives MoQOutput. libmoq delivers the terminal status callback
	// (code <= 0) asynchronously on its runtime thread after moq_session_close,
	// and OBS is free to destroy the output long before that runtime gets around
	// to it. Keeping this separate is what makes a late callback harmless: it
	// finds the state detached rather than freed, so teardown never has to
	// outwait a stalled runtime.
	//
	// It is also the only state a callback may touch: everything a callback needs
	// about its own attempt is copied into its SessionRef, since the members here
	// belong to whichever attempt is current.
	struct SessionState {
		explicit SessionState(obs_output_t *output) : output(output) {}

		// Stop reporting to OBS. Returns only once no callback is inside an OBS
		// call, so the output can be torn down behind it. The wait is bounded by
		// an OBS call rather than by anything libmoq does.
		void Detach();

		// Tell OBS the output stopped, unless it has already been detached. Every
		// obs_output_signal_stop goes through here or through an explicit
		// signal_mutex hold.
		void SignalStop(int code);

		void Connected(const SessionRef &ref, int connect_epoch);
		void Closed(const SessionRef &ref, int code);

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
		// Lock order: signal_mutex first, then mutex. Never the reverse, and never
		// take signal_mutex from inside a libmoq call. That keeps it ordered ahead
		// of libmoq's runtime lock too, since the status callback runs with no
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

		// The OBS output, or null once MoQOutput has been destroyed. Guarded by
		// signal_mutex, which every call made through it is also held under.
		obs_output_t *output;

		// Guards the group below, which the OBS thread and the libmoq runtime
		// thread both touch.
		std::mutex mutex;
		// The live session handle, or 0 when there is none. libmoq drops the handle
		// before firing the terminal callback, so it is retired there rather than
		// closed later (the close would just fail with "session not found").
		int session = 0;
		// Bumped whenever the publish state is torn down or restarted. A status
		// callback stamped with an older value belongs to a superseded attempt and
		// must leave both OBS and `session` alone, which is also what limits the
		// failure signal to one per Start().
		uint64_t attempt = 0;
		// Whether the current attempt ever reached the server, which picks between
		// telling OBS the connection failed and telling it the stream dropped.
		bool connected = false;
		// Dial URL of the current attempt, for the stats snapshot's scheme label.
		std::string url;
		int last_failure_code = 0;
		std::string last_failure_reason;

		// Written by the status callback (libmoq runtime thread), read by
		// GetConnectTime() and GetReconnectCount() (OBS thread); atomic to avoid a
		// data race.
		std::atomic<int> connect_time_ms{0};
		std::atomic<int> epoch{0};
	};

	// Handed to libmoq as the status callback's user_data. Carries everything a
	// callback needs about its own Start() attempt, so it never has to read a
	// member the OBS thread may already be rewriting for the next attempt, plus
	// the reference that keeps that state alive. Freed by the terminal callback.
	struct SessionRef {
		std::shared_ptr<SessionState> state;
		uint64_t attempt;
		std::string url;
		std::chrono::steady_clock::time_point started;
	};

	static void SessionStatus(void *user_data, int code);

	// Tear down the publish state without telling OBS.
	void Reset();

	void VideoInit(obs_encoder_t *encoder);
	void VideoData(struct encoder_packet *packet);
	void AudioInit(obs_encoder_t *encoder);
	void AudioData(struct encoder_packet *packet);

	// The OBS thread's view of the output, valid for this object's lifetime.
	// Reporting a stop goes through state->SignalStop instead, which is the only
	// path a callback thread may take.
	obs_output_t *output;

	const std::shared_ptr<SessionState> state;

	std::string path;

	size_t total_bytes_sent;

	int origin;
	int broadcast;

	std::map<obs_encoder_t *, int> video_tracks;
	std::map<obs_encoder_t *, int> audio_tracks;
};

void register_moq_output();
