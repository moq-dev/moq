//! Accept-loop health for the listeners that perform a real `accept(2)`.
//!
//! A listener loop cannot treat every `accept` failure the same way. The errnos
//! `accept` returns mix two families that want opposite handling, and getting them
//! backwards is not symmetric: pausing after a dead connection punishes the
//! connections queued behind it, while retrying instantly after fd exhaustion is a
//! hot loop burning the CPU the process needs in order to recover. [`Failure`] is
//! that classification and [`Health`] applies it, so a listener's failure arm is
//! one call rather than a policy each caller re-derives.
//!
//! [`Health`] is also the only thing that leaves the process when a listener goes
//! dark. The loops here never give up (an accept loop is a process-lifetime
//! supervisor with nobody to return an error to), so a node can be unable to accept
//! a single TCP connection while every other signal looks healthy. The cumulative
//! counters are the load-bearing half rather than [`stalled`](Health::stalled): a
//! process with no descriptors left cannot serve a metrics scrape either, so the
//! episode is often only visible once it is over, and a gauge read after recovery
//! reads a healthy nothing while a counter still shows the jump.
//!
//! Only a listener that performs a real `accept(2)` has anything to report here.
//! The QUIC backends multiplex every session over one UDP socket, so they never
//! call `accept` and exhaustion cannot reach them; registering one would publish a
//! permanently-zero counter, which reads as a watch that is passing when it is
//! really a watch that can never fire.

use std::{
	io,
	sync::{
		Arc,
		atomic::{AtomicU64, Ordering},
	},
	time::{Duration, Instant},
};

/// Delay after an accept failure that is not one connection's fault, escalating
/// while they continue and reset by the next successful accept.
///
/// Capped in seconds rather than minutes: the loop is a supervisor that must stay
/// responsive to a resource being returned, so it keeps asking at a rate that
/// recovers promptly without spinning.
const RETRY_MIN: Duration = Duration::from_millis(100);
const RETRY_MAX: Duration = Duration::from_secs(5);

/// What a failed `accept(2)` means for the listener that saw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Failure {
	/// One pending connection died on its way to us: the peer reset it before we
	/// dequeued it, a firewall rule dropped it, its handshake timed out.
	///
	/// Ordinary traffic, and never a fault of the listener. The queue entry is
	/// consumed, so the very next `accept` makes progress and delaying it would only
	/// punish the connections queued behind the dead one, at whatever rate a remote
	/// peer chooses to supply them.
	///
	/// How reachable that is depends on the platform. Linux surfaces a new socket's
	/// already-pending network errors through `accept` (which BSD does not, and which
	/// is why the list is as long as it is), but drops a connection reset before
	/// `accept` from the queue silently rather than reporting `ECONNABORTED`. So the
	/// simplest flood is louder on BSD than on Linux; the handling is the same either
	/// way.
	Connection,
	/// A resource `accept` needs is exhausted process-wide or host-wide: the process
	/// fd table (`EMFILE`), the system-wide one (`ENFILE`), or kernel memory for the
	/// new socket (`ENOBUFS`/`ENOMEM`).
	///
	/// The connection stays queued, so the listener is instantly readable and
	/// instantly failing until something outside this loop returns the resource.
	Exhausted,
	/// An errno we don't recognize. Worth backing off on, never worth claiming a
	/// cause for: an unrecognized failure on ordinary traffic could be driven by a
	/// remote peer, so escalating on it hands out a remote lever.
	Unknown,
}

impl Failure {
	/// Every class, for a caller enumerating [`Health::failures`] into a metrics
	/// endpoint.
	pub const ALL: [Failure; 3] = [Failure::Connection, Failure::Exhausted, Failure::Unknown];

	/// Classify a failed `accept(2)`.
	///
	/// Unrecognized is [`Unknown`](Self::Unknown), never [`Connection`](Self::Connection):
	/// the default has to be the one that paces, because guessing "per connection" on a
	/// listener-wide failure is the mistake that spins.
	pub fn classify(err: &io::Error) -> Self {
		match err.raw_os_error() {
			Some(code) if exhausted(code) => Self::Exhausted,
			Some(code) if per_connection(code) => Self::Connection,
			_ => Self::Unknown,
		}
	}

	/// The stable lowercase name used in logs and metric labels.
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::Connection => "connection",
			Self::Exhausted => "exhausted",
			Self::Unknown => "unknown",
		}
	}
}

impl std::fmt::Display for Failure {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str(self.as_str())
	}
}

/// Whether `accept` itself had nothing to work with, so the failure persists until
/// something outside the accept loop returns the resource.
#[cfg(unix)]
fn exhausted(code: i32) -> bool {
	[libc::EMFILE, libc::ENFILE, libc::ENOBUFS, libc::ENOMEM].contains(&code)
}

/// Whether the failure belongs to the one connection being dequeued rather than to
/// the listener.
///
/// The bulk of the list is the "already-pending network error on the new socket" set
/// `accept(2)` documents on Linux, which is a Linux behavior specifically (BSD holds
/// the error until the socket is used). Each reports the state of that connection,
/// never of the listener, so each is safe to retry at once.
#[cfg(unix)]
fn per_connection(code: i32) -> bool {
	let common = [
		libc::ECONNABORTED,
		libc::ECONNRESET,
		libc::ETIMEDOUT,
		// A netfilter/firewall rule rejected this connection.
		libc::EPERM,
		libc::EPROTO,
		libc::ENOPROTOOPT,
		libc::EOPNOTSUPP,
		libc::EHOSTDOWN,
		libc::EHOSTUNREACH,
		libc::ENETDOWN,
		libc::ENETUNREACH,
		// Not a connection error at all: the syscall was interrupted by a signal.
		// Grouped here because it wants the same handling (retry at once) and says
		// nothing about the listener.
		libc::EINTR,
	]
	.contains(&code);

	// `ENONET` rounds out the pending-error set above, and exists only where that set
	// does: it is absent from the BSD/Apple headers entirely.
	#[cfg(any(target_os = "linux", target_os = "android"))]
	let common = common || code == libc::ENONET;

	common
}

/// Winsock reports these under `WSAE*` codes rather than errnos. Spelled out because
/// the constants live in `windows-sys`, a large dependency to take on for four
/// integers; they are fixed by the Winsock ABI.
///
/// Deliberately narrower than the unix set. `accept` on Windows documents far fewer
/// per-connection failures, and the ones that look analogous are not: `WSAENETDOWN`
/// is "the network subsystem has failed", which is listener-wide and would spin if
/// retried at once. Anything not listed here lands in [`Failure::Unknown`], which
/// paces, so the narrow list fails safe.
#[cfg(windows)]
fn exhausted(code: i32) -> bool {
	[
		10024, // WSAEMFILE: no more socket descriptors available
		10055, // WSAENOBUFS: no buffer space available
	]
	.contains(&code)
}

#[cfg(windows)]
fn per_connection(code: i32) -> bool {
	[
		10053, // WSAECONNABORTED: software caused connection abort
		10054, // WSAECONNRESET: the peer terminated the indicated connection
	]
	.contains(&code)
}

/// No error table for a platform we have never run a listener on. Everything is
/// [`Failure::Unknown`], which paces and warns rather than claiming a cause.
#[cfg(not(any(unix, windows)))]
fn exhausted(_code: i32) -> bool {
	false
}

#[cfg(not(any(unix, windows)))]
fn per_connection(_code: i32) -> bool {
	false
}

/// One listener's accept-loop health: how to react to a failure, and what a
/// supervisor outside the process can read back.
///
/// Cheap to clone; every clone shares one listener's state. The listeners here
/// build their own and hand out a clone through their `accept_health()` method (as
/// does the relay's web server), while an embedder driving its own listener
/// constructs one with [`new`](Self::new).
///
/// This owns the backoff rather than each loop inlining its own, which is the one
/// place that indirection earns its keep: the classification above is the whole
/// point of the pacing, and five listeners that each re-derive it will not agree
/// for long.
#[derive(Clone)]
pub struct Health(Arc<Inner>);

struct Inner {
	listener: &'static str,
	connection: AtomicU64,
	exhausted: AtomicU64,
	unknown: AtomicU64,
	state: parking_lot::Mutex<State>,
}

struct State {
	/// When the current run of exhaustion failures began, if one is in progress.
	stall: Option<Instant>,
	/// Consecutive failures since the last successful accept.
	consecutive: u64,
	/// The next un-jittered delay, escalating while accepts keep failing.
	delay: Duration,
}

impl Health {
	/// Track a listener reported under `listener` (the name used in logs and as a
	/// metric label).
	pub fn new(listener: &'static str) -> Self {
		Self(Arc::new(Inner {
			listener,
			connection: AtomicU64::new(0),
			exhausted: AtomicU64::new(0),
			unknown: AtomicU64::new(0),
			state: parking_lot::Mutex::new(State {
				stall: None,
				consecutive: 0,
				delay: RETRY_MIN,
			}),
		}))
	}

	/// The name this listener is reported under.
	pub fn listener(&self) -> &'static str {
		self.0.listener
	}

	/// An accept succeeded: the listener is serving, so any stall is over and the
	/// next failure starts from the shortest delay again.
	pub fn accepted(&self) {
		let mut state = self.0.state.lock();
		if state.stall.take().is_some() {
			tracing::info!(listener = self.0.listener, "listener is accepting again");
		}
		state.consecutive = 0;
		state.delay = RETRY_MIN;
	}

	/// An accept failed: classify it, count it, log it, and return how long the loop
	/// should wait before asking again.
	///
	/// `None` means retry at once, which is what a [`Failure::Connection`] wants:
	/// the queue entry was consumed, so the listener has already made progress.
	#[must_use = "an accept failure that is not one connection's fault must be paced, or the loop spins"]
	pub fn failed(&self, err: &io::Error) -> Option<Duration> {
		let failure = Failure::classify(err);
		self.counter(failure).fetch_add(1, Ordering::Relaxed);

		if failure == Failure::Connection {
			// Ordinary traffic. Warning per occurrence would drown the log the moment
			// a scanner shows up, and there is nothing for an operator to do.
			tracing::debug!(listener = self.0.listener, %err, "dropped a connection before accepting it");
			return None;
		}

		let mut state = self.0.state.lock();
		state.consecutive += 1;
		let delay = jitter(state.delay);
		state.delay = (state.delay * 2).min(RETRY_MAX);

		let stalled = match failure {
			Failure::Exhausted => Some(state.stall.get_or_insert_with(Instant::now).elapsed()),
			_ => None,
		};

		tracing::warn!(
			listener = self.0.listener,
			%err,
			class = failure.as_str(),
			consecutive = state.consecutive,
			stalled_secs = stalled.map(|stalled| stalled.as_secs()),
			retry_in_ms = delay.as_millis(),
			"accept failed; the listener is not serving new connections"
		);

		Some(delay)
	}

	/// Failed accepts of this class since the process started.
	///
	/// Cumulative and never reset, so a scrape that lands after the episode still
	/// sees it. Classes are counted apart rather than totalled because they are not
	/// comparable: [`Failure::Connection`] tracks how much junk traffic the node is
	/// fielding, while a non-zero [`Failure::Exhausted`] means the process ran out of
	/// something it needs to serve anyone.
	pub fn failures(&self, failure: Failure) -> u64 {
		self.counter(failure).load(Ordering::Relaxed)
	}

	/// How long the listener has been unable to accept, when a
	/// [`Failure::Exhausted`] stall is in progress.
	///
	/// Only a successful accept clears it, so a listener with no traffic holds its
	/// last value rather than claiming a recovery it has no evidence for.
	pub fn stalled(&self) -> Option<Duration> {
		self.0.state.lock().stall.map(|since| since.elapsed())
	}

	fn counter(&self, failure: Failure) -> &AtomicU64 {
		match failure {
			Failure::Connection => &self.0.connection,
			Failure::Exhausted => &self.0.exhausted,
			Failure::Unknown => &self.0.unknown,
		}
	}
}

impl std::fmt::Debug for Health {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Health")
			.field("listener", &self.0.listener)
			.field("connection", &self.failures(Failure::Connection))
			.field("exhausted", &self.failures(Failure::Exhausted))
			.field("unknown", &self.failures(Failure::Unknown))
			.field("stalled", &self.stalled())
			.finish()
	}
}

/// Equal jitter: half the delay is a firm floor, so a flapping listener always
/// waits a meaningful amount, while the random half keeps a fleet that failed on
/// the same tick from retrying in lockstep.
fn jitter(delay: Duration) -> Duration {
	use rand::RngExt as _;
	delay.mul_f64(0.5 + rand::rng().random::<f64>() / 2.0)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn io(code: i32) -> io::Error {
		io::Error::from_raw_os_error(code)
	}

	#[cfg(unix)]
	#[test]
	fn classifies_exhaustion_apart_from_dead_connections() {
		// The whole point: a peer that reset before we accepted it is ordinary
		// traffic, so it must never read as exhaustion. Counting it as one hands a
		// remote peer a lever on whatever the exhaustion signal drives.
		for code in [
			libc::ECONNABORTED,
			libc::ECONNRESET,
			libc::EPERM,
			libc::EPROTO,
			libc::ETIMEDOUT,
			libc::EHOSTUNREACH,
			libc::ENETDOWN,
			libc::EINTR,
		] {
			assert_eq!(Failure::classify(&io(code)), Failure::Connection, "errno {code}");
		}

		for code in [libc::EMFILE, libc::ENFILE, libc::ENOBUFS, libc::ENOMEM] {
			assert_eq!(Failure::classify(&io(code)), Failure::Exhausted, "errno {code}");
		}

		// Unrecognized is Unknown, not Exhausted: a new failure mode earns a backoff
		// and a warning, never a claim about the cause.
		assert_eq!(Failure::classify(&io(libc::EINVAL)), Failure::Unknown);
		assert_eq!(Failure::classify(&io::Error::other("never seen")), Failure::Unknown);
	}

	/// `ENONET` belongs to the pending-error set the `classifies_*` test covers, and
	/// exists only on Linux, so it needs its own gate. Without it the errno lands in
	/// `Unknown` and a firewalled connection costs the listener a backoff.
	#[cfg(any(target_os = "linux", target_os = "android"))]
	#[test]
	fn linux_pending_network_errors_are_per_connection() {
		assert_eq!(Failure::classify(&io(libc::ENONET)), Failure::Connection);
	}

	/// Not compiled on a unix host; the Windows gates in `just rs windows` are the
	/// only thing that builds this.
	#[cfg(windows)]
	#[test]
	fn windows_subsystem_failure_is_not_per_connection() {
		// WSAENETDOWN is "the network subsystem has failed", which is listener-wide.
		// Retrying it at once is the spin this module exists to prevent, so it must
		// fall through to Unknown (which paces) rather than read as one dead peer.
		assert_eq!(Failure::classify(&io(10050)), Failure::Unknown);
		assert_eq!(Failure::classify(&io(10054)), Failure::Connection);
		assert_eq!(Failure::classify(&io(10024)), Failure::Exhausted);
	}

	#[cfg(unix)]
	#[test]
	fn a_dead_connection_never_pauses_the_listener() {
		let health = Health::new("test");

		// A pause here would let a peer opening and resetting connections in bulk
		// hold the listener at the retry cap, starving legitimate ones.
		assert_eq!(health.failed(&io(libc::ECONNABORTED)), None);
		assert_eq!(health.failures(Failure::Connection), 1);
		assert_eq!(health.stalled(), None, "a dead connection is not a stall");
	}

	#[cfg(unix)]
	#[test]
	fn exhaustion_escalates_and_caps() {
		let health = Health::new("test");

		// Equal jitter, so each delay lands in [d/2, d] for the un-jittered d.
		for expected in [RETRY_MIN, RETRY_MIN * 2, RETRY_MIN * 4] {
			let delay = health.failed(&io(libc::EMFILE)).expect("exhaustion must pace");
			assert!(
				delay >= expected / 2 && delay <= expected,
				"{delay:?} outside {expected:?}"
			);
		}

		// Keep failing and it settles at the cap rather than climbing into minutes:
		// the loop has to stay responsive to the resource coming back.
		for _ in 0..20 {
			let delay = health.failed(&io(libc::EMFILE)).expect("exhaustion must pace");
			assert!(delay <= RETRY_MAX);
		}

		assert!(health.stalled().is_some(), "exhaustion is a sustained condition");
		assert_eq!(health.failures(Failure::Exhausted), 23);
		assert_eq!(health.failures(Failure::Connection), 0);
	}

	#[cfg(unix)]
	#[test]
	fn a_successful_accept_ends_the_stall_and_the_backoff() {
		let health = Health::new("test");
		for _ in 0..5 {
			let _ = health.failed(&io(libc::EMFILE));
		}
		assert!(health.stalled().is_some());

		health.accepted();
		assert_eq!(health.stalled(), None);

		// Back to the shortest delay: the next episode is a new one, not a
		// continuation of a schedule that already climbed to the cap.
		let delay = health.failed(&io(libc::EMFILE)).expect("exhaustion must pace");
		assert!(delay <= RETRY_MIN);

		// The counters do NOT reset. They are what a scrape landing after the
		// episode has to see, since the process could not answer one during it.
		assert_eq!(health.failures(Failure::Exhausted), 6);
	}
}
