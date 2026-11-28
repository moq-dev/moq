//! Idle timeout, keep-alive, and RTT probing
//!
//! A reliable transport has no acknowledgments to show the peer is alive, so the idle
//! deadline follows records. Receiving one always restarts it. Sending restarts it at most
//! once per receive (RFC 9000 §10.1): counting every send would let our own keep-alives
//! restart the deadline they exist to test, and a peer that went silent while its socket
//! still accepts writes would never be reclaimed.
//!
//! Keep-alive QX_PINGs go out after a third of the idle window without receiving, so even a
//! one-way sender whose peer has nothing to say draws an answer before its deadline. RTT probes
//! go out on their own interval regardless, since an application that is busy talking is
//! when latency matters and the keep-alive never fires. While the transport refuses writes,
//! the peer is evidently there but slow, so the idle close is deferred by at most one more
//! window.
//!
//! A [`KeepAlive`] also pings every half of its `soft` limit while the peer stays unheard, marks
//! the peer stale once all of `soft` passes, and closes once `hard` does.

use std::collections::VecDeque;

use crate::connection::keep_alive::{KeepAlive, KeepAliveError, Liveness};
use crate::{Duration, Instant};

/// The most unanswered probes tracked. A peer that answers retires them at once, so this
/// only fills against one that keeps the session busy while ignoring QX_PING.
const MAX_OUTSTANDING: usize = 64;

#[derive(Debug)]
pub(super) struct Timer {
	/// Fails the handshake if the peer's parameters have not arrived by then
	handshake: Option<Instant>,
	/// The negotiated idle timeout, once the peer's parameters arrive
	idle: Option<Duration>,
	rtt_interval: Option<Duration>,

	last_recv: Instant,
	/// The send that last restarted the idle deadline
	send_reset: Instant,
	/// Whether a send may restart the deadline: a record arrived since the last one did
	send_credit: bool,
	last_ping: Instant,
	/// When the idle close began deferring for transport backpressure
	deferred: Option<Instant>,
	backpressured: bool,

	/// Probes sent and not yet answered, oldest first, with the time each was written
	outstanding: VecDeque<(u64, Instant)>,
	rtt: Option<Duration>,

	liveness: Option<Liveness>,
}

/// What [`Timer::poll`] found due
#[derive(Debug, Default)]
pub(super) struct Due {
	pub(super) handshake: bool,
	pub(super) idle: bool,
	pub(super) ping: bool,
	/// The peer just turned stale
	pub(super) stale: bool,
}

impl Timer {
	pub(super) fn new(handshake: Option<Duration>, now: Instant) -> Self {
		Self {
			handshake: handshake.map(|timeout| now.saturating_add(timeout)),
			idle: None,
			rtt_interval: None,
			last_recv: now,
			send_reset: now,
			send_credit: true,
			last_ping: now,
			deferred: None,
			backpressured: false,
			outstanding: VecDeque::new(),
			rtt: None,
			liveness: None,
		}
	}

	/// Start `keep_alive`, measured from the last record received; see [`Due::stale`]
	pub(super) fn set_keep_alive(&mut self, keep_alive: KeepAlive) -> Result<Option<Liveness>, KeepAliveError> {
		let liveness = Liveness::new(keep_alive, self.idle, self.last_recv)?;
		Ok(self.liveness.replace(liveness))
	}

	/// Whether a keep-alive found the peer unheard for its `soft` limit
	pub(super) fn is_stale(&self) -> bool {
		self.liveness.as_ref().is_some_and(Liveness::is_stale)
	}

	/// Arm the idle timeout and RTT probing once the peer's parameters arrive
	pub(super) fn established(&mut self, idle: Option<Duration>, rtt_interval: Option<Duration>, now: Instant) {
		self.handshake = None;
		self.idle = idle;
		self.rtt_interval = rtt_interval;
		self.last_recv = now;
		self.send_reset = now;
		self.send_credit = true;
		self.last_ping = now;
	}

	/// A record arrived at `now`; returns whether that ended a stale stretch
	pub(super) fn on_recv(&mut self, now: Instant) -> bool {
		self.last_recv = now;
		self.send_credit = true;
		self.deferred = None;
		self.liveness.as_mut().is_some_and(|liveness| liveness.heard(now))
	}

	pub(super) fn on_send(&mut self, now: Instant) {
		if self.send_credit {
			self.send_reset = now;
			self.send_credit = false;
		}
	}

	pub(super) fn set_backpressured(&mut self, backpressured: bool) {
		self.backpressured = backpressured;
	}

	fn last_activity(&self) -> Instant {
		self.last_recv.max(self.send_reset)
	}

	fn keepalive(&self) -> Option<Duration> {
		self.idle.map(|idle| (idle / 3).max(Duration::from_millis(1)))
	}

	/// The next instant [`Self::poll`] has something to do
	pub(super) fn next(&self) -> Option<Instant> {
		let idle = self.idle.map(|idle| match self.deferred {
			Some(since) => since.saturating_add(idle),
			None => self.last_activity().saturating_add(idle),
		});
		let keepalive = self
			.keepalive()
			.map(|every| self.last_recv.max(self.last_ping).saturating_add(every));
		let rtt = self.rtt_interval.map(|every| self.last_ping.saturating_add(every));
		let liveness = self.liveness.as_ref().map(|liveness| {
			let probe = liveness
				.probe_at()
				.max(self.last_ping.saturating_add(liveness.probe_after()));
			[Some(probe), liveness.stale_at(), Some(liveness.silent_at())]
				.into_iter()
				.flatten()
				.min()
				.expect("probe and silent are always set")
		});
		[self.handshake, idle, keepalive, rtt, liveness]
			.into_iter()
			.flatten()
			.min()
	}

	pub(super) fn poll(&mut self, now: Instant) -> Due {
		let mut due = Due {
			handshake: self.handshake.is_some_and(|at| now >= at),
			..Due::default()
		};

		let keepalive = self
			.keepalive()
			.is_some_and(|every| now >= self.last_recv.max(self.last_ping).saturating_add(every));
		let rtt = self
			.rtt_interval
			.is_some_and(|every| now >= self.last_ping.saturating_add(every));
		// Probe every half of `soft` while the peer stays unheard.
		let probe = self.liveness.as_ref().is_some_and(|liveness| {
			now >= liveness
				.probe_at()
				.max(self.last_ping.saturating_add(liveness.probe_after()))
		});
		if keepalive || rtt || probe {
			// A ping cannot get past a stalled transport, and must not pile up behind it
			due.ping = !self.backpressured;
			self.last_ping = now;
		}

		if let Some(idle) = self.idle
			&& now >= self.last_activity().saturating_add(idle)
		{
			match self.deferred {
				// A peer that died with our buffers full must not hang here forever
				Some(since) => due.idle = now >= since.saturating_add(idle),
				None if self.backpressured => self.deferred = Some(now),
				None => due.idle = true,
			}
		}
		// The hard limit closes whatever the transport's backpressure, unlike the idle timeout.
		if let Some(liveness) = &mut self.liveness {
			due.stale = liveness.expire(now);
			due.idle |= now >= liveness.silent_at();
		}
		due
	}

	/// Record a probe written at `now`
	pub(super) fn ping_sent(&mut self, seq: u64, now: Instant) {
		if self.outstanding.len() >= MAX_OUTSTANDING {
			self.outstanding.pop_front();
		}
		self.outstanding.push_back((seq, now));
	}

	/// Fold in the response to `seq` received at `now`
	///
	/// A responder may answer several requests with the largest sequence number, so this
	/// retires every request up to `seq` and measures from the newest of them: the one the
	/// peer had to receive before answering.
	pub(super) fn pong(&mut self, seq: u64, now: Instant) {
		let mut newest = None;
		while let Some(&(sent, at)) = self.outstanding.front() {
			if sent > seq {
				break;
			}
			newest = Some(at);
			self.outstanding.pop_front();
		}
		let Some(at) = newest else {
			return;
		};
		let sample = now.saturating_duration_since(at);
		self.rtt = Some(match self.rtt {
			// RFC 6298's smoothing
			Some(rtt) => (rtt * 7 + sample) / 8,
			None => sample,
		});
	}

	pub(super) fn rtt(&self) -> Option<Duration> {
		self.rtt
	}
}
