//! Connection-phase Happy Eyeballs (RFC 8305 section 5) for client dials.
//!
//! DNS often returns both IPv6 and IPv4 addresses, and either family can be
//! silently broken (an unrouted AAAA, a blocked v4 path). Rather than dial one
//! address and wait out the handshake timeout, a dial staggers attempts across
//! every resolved address, alternating families, and takes the first connection
//! to complete. The stagger is [`connect::Config::race`](crate::connect::Config::race).
//!
//! The addresses arrive from the DNS phase as they resolve, and in the order the
//! platform's own resolver put them in, so a lookup still waiting on its AAAA
//! record no longer holds up the first attempt. How long that first attempt
//! holds back for the full answer is [`crate::connect::Config::resolution_delay`].
//!
//! Nothing here needs calling: every client dial goes through it. The one type
//! a consumer sees is [`Failure`], which the backend `Error` types carry when
//! the race loses every attempt.

use std::fmt;
use std::future::Future;
use std::net::SocketAddr;
use std::time::Duration;

use futures::StreamExt;
use futures::stream::FuturesUnordered;

use crate::resolve::Candidates;

/// One failed connection attempt, naming the address it dialed.
///
/// Carried by each backend's `Error::Failover` variant, one per attempt, when
/// the address race loses all of them. A dial that had only one address to try
/// reports that error directly instead, so this never stands alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure<E> {
	/// The address that was dialed.
	pub addr: SocketAddr,

	/// Why that attempt failed.
	pub error: E,
}

impl<E: fmt::Display> fmt::Display for Failure<E> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "{}: {}", self.addr, self.error)
	}
}

impl<E: std::error::Error + 'static> std::error::Error for Failure<E> {
	fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
		Some(&self.error)
	}
}

/// An error type that can fold several failed attempts into one value, so
/// [`race`] can report an address race that lost every attempt.
///
/// Implemented by each backend's error enum over its own `Failover` variant.
pub(crate) trait Aggregate: Sized {
	/// Fold two or more failed attempts into a single error.
	///
	/// Never called with fewer than two: a lone attempt is no race, so [`race`]
	/// hands that error back untouched rather than burying it in an aggregate.
	fn aggregate(failures: Vec<Failure<Self>>) -> Self;

	/// The error for a dial that never had an address to try: the DNS failure
	/// when there was one, and the backend's empty-answer error when both queries
	/// simply came back with nothing.
	fn resolve(error: Option<std::io::Error>) -> Self;
}

/// Render each failed attempt as `addr: error`, joined by `; `.
pub(crate) fn describe<E: fmt::Display>(failures: &[Failure<E>]) -> String {
	failures.iter().map(|f| f.to_string()).collect::<Vec<_>>().join("; ")
}

/// Dial `candidates` in order, starting the next attempt `delay` after the
/// previous one (or immediately when it fails), and return the first success.
/// The remaining attempts are dropped, which aborts them.
///
/// Candidates are pulled as they resolve, so the first attempt starts on the
/// first answer rather than on the last, and a stagger that elapses while the
/// other family is still being resolved starts its attempt the moment an address
/// lands.
///
/// A `delay` of zero dials every candidate at once, as fast as they resolve.
///
/// A single candidate is not a race, so its error comes back untouched: an IP
/// literal or a host with one address still reports the backend's own error,
/// source chain and all. Once there are two or more, every error is folded in
/// via [`Aggregate`], ordered by candidate rather than by when it finished.
/// Singling one out means guessing, and both obvious guesses are wrong in a case
/// this exists to handle: the most preferred candidate is the broken family
/// failover routes around, while the last to finish is whichever address
/// blackholed until its timeout, and either can bury a rejected certificate or a
/// refused port that the caller could act on.
///
/// A resolution that never yields an address reports [`Aggregate::resolve`]
/// instead, since there was no attempt to report.
pub(crate) async fn race<C, E, F, Fut>(mut candidates: Candidates, delay: Duration, mut dial: F) -> Result<C, E>
where
	F: FnMut(SocketAddr) -> Fut,
	Fut: Future<Output = Result<C, E>>,
	E: Aggregate + fmt::Display,
{
	let mut attempts = FuturesUnordered::new();
	let mut failures: Vec<(usize, Failure<E>)> = Vec::new();
	let mut exhausted = false;

	// When the next attempt may start: the first as soon as an address resolves,
	// each later one a stagger after the one before it.
	let mut ready = tokio::time::Instant::now();

	let mut next_index = 0;
	let mut start = |addr: SocketAddr, attempts: &mut FuturesUnordered<_>| {
		let index = next_index;
		next_index += 1;
		tracing::debug!(%addr, index, "dialing");
		let attempt = dial(addr);
		attempts.push(async move { (index, addr, attempt.await) });
	};

	loop {
		if exhausted && attempts.is_empty() {
			if failures.is_empty() {
				return Err(E::resolve(candidates.failure()));
			}

			// Report in candidate order, not the order they finished, so the same DNS
			// answer always reads the same way.
			failures.sort_by_key(|(index, _)| *index);
			return Err(collapse(failures.into_iter().map(|(_, failure)| failure).collect()));
		}

		tokio::select! {
			// Bias toward a finished attempt so a success that raced the timer wins
			// without dialing another address for nothing.
			biased;

			Some((index, addr, res)) = attempts.next(), if !attempts.is_empty() => {
				match res {
					Ok(conn) => {
						tracing::debug!(%addr, index, "connected");
						return Ok(conn);
					}
					Err(err) => {
						// Debug, not warn: routing around a broken family is the normal
						// condition this exists for, so an attempt that loses is only
						// interesting when the whole race fails. Then it comes back in
						// the returned error, which the caller logs.
						tracing::debug!(%addr, index, %err, "connection attempt failed");
						failures.push((index, Failure { addr, error: err }));
						// A failure starts the next candidate immediately (RFC 8305
						// section 5) rather than waiting out the stagger delay.
						ready = tokio::time::Instant::now();
					}
				}
			}

			// The deadline is absolute, so cancelling this arm (which every finished
			// attempt does) resumes the same stagger rather than restarting it.
			addr = pull(&mut candidates, ready), if !exhausted => {
				match addr {
					Some(addr) => {
						start(addr, &mut attempts);
						ready = tokio::time::Instant::now() + delay;
					}
					None => exhausted = true,
				}
			}
		}
	}
}

/// The next candidate to dial, once the stagger has elapsed.
///
/// Resolution can outlast the stagger, in which case the attempt starts as soon
/// as an address lands.
async fn pull(candidates: &mut Candidates, ready: tokio::time::Instant) -> Option<SocketAddr> {
	tokio::time::sleep_until(ready).await;
	candidates.next().await
}

/// Fold the failed attempts into one error, leaving a lone attempt's error
/// exactly as the backend produced it.
fn collapse<E: Aggregate>(mut failures: Vec<Failure<E>>) -> E {
	match failures.len() {
		1 => failures.pop().expect("checked len").error,
		_ => E::aggregate(failures),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::connect::DEFAULT_RACE;
	use std::sync::Arc;
	use std::sync::atomic::{AtomicUsize, Ordering};

	fn addr(s: &str) -> SocketAddr {
		s.parse().unwrap()
	}

	/// Stands in for a backend error enum: one variant per dial failure, one that
	/// aggregates them the way a backend's `Failover` variant does.
	#[derive(Debug, PartialEq, Eq)]
	enum TestError {
		Dial(&'static str),
		All(Vec<Failure<TestError>>),
	}

	impl fmt::Display for TestError {
		fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
			match self {
				Self::Dial(err) => write!(f, "{err}"),
				Self::All(failures) => write!(f, "all {} attempts failed: {}", failures.len(), describe(failures)),
			}
		}
	}

	impl Aggregate for TestError {
		fn aggregate(failures: Vec<Failure<Self>>) -> Self {
			Self::All(failures)
		}

		fn resolve(error: Option<std::io::Error>) -> Self {
			match error {
				Some(_) => Self::Dial("lookup failed"),
				None => Self::Dial("no addresses"),
			}
		}
	}

	fn failed(dest: &str, err: &'static str) -> Failure<TestError> {
		Failure {
			addr: addr(dest),
			error: TestError::Dial(err),
		}
	}

	#[tokio::test(start_paused = true)]
	async fn first_success_returns_immediately() {
		let dials = Arc::new(AtomicUsize::new(0));
		let counter = dials.clone();
		let res: Result<&str, TestError> = race(
			Candidates::fixed([addr("1.1.1.1:1"), addr("2.2.2.2:2")]),
			DEFAULT_RACE,
			move |_| {
				counter.fetch_add(1, Ordering::SeqCst);
				async { Ok("winner") }
			},
		)
		.await;
		assert_eq!(res, Ok("winner"));
		assert_eq!(dials.load(Ordering::SeqCst), 1, "no second dial after a fast success");
	}

	#[tokio::test(start_paused = true)]
	async fn second_wins_when_first_hangs() {
		let start = tokio::time::Instant::now();
		let res: Result<&str, TestError> = race(
			Candidates::fixed([addr("1.1.1.1:1"), addr("2.2.2.2:2")]),
			DEFAULT_RACE,
			|dest| async move {
				if dest == addr("1.1.1.1:1") {
					std::future::pending().await
				} else {
					Ok("second")
				}
			},
		)
		.await;
		assert_eq!(res, Ok("second"));
		assert_eq!(start.elapsed(), DEFAULT_RACE, "second dial waits out the stagger");
	}

	#[tokio::test(start_paused = true)]
	async fn failure_starts_the_next_attempt_immediately() {
		let start = tokio::time::Instant::now();
		let res: Result<&str, TestError> = race(
			Candidates::fixed([addr("1.1.1.1:1"), addr("2.2.2.2:2")]),
			DEFAULT_RACE,
			|dest| async move {
				if dest == addr("1.1.1.1:1") {
					Err(TestError::Dial("boom"))
				} else {
					Ok("second")
				}
			},
		)
		.await;
		assert_eq!(res, Ok("second"));
		assert_eq!(start.elapsed(), Duration::ZERO, "failure must not wait for the timer");
	}

	/// The preferred candidate fails instantly, the way an unroutable address
	/// does, and the fallback that reached the server fails later. Reporting the
	/// most preferred error alone would bury the actionable one.
	#[tokio::test(start_paused = true)]
	async fn all_failures_are_reported_when_the_preferred_fails_first() {
		let res: Result<&str, TestError> = race(
			Candidates::fixed([addr("1.1.1.1:1"), addr("2.2.2.2:2")]),
			Duration::from_millis(10),
			|dest| async move {
				if dest == addr("1.1.1.1:1") {
					Err(TestError::Dial("network unreachable"))
				} else {
					tokio::time::sleep(Duration::from_secs(1)).await;
					Err(TestError::Dial("invalid peer certificate"))
				}
			},
		)
		.await;
		assert_eq!(
			res,
			Err(TestError::All(vec![
				failed("1.1.1.1:1", "network unreachable"),
				failed("2.2.2.2:2", "invalid peer certificate"),
			]))
		);
	}

	/// The inverse: the preferred candidate blackholes until its timeout while
	/// the fallback reports the actionable error early. Reporting the last error
	/// to finish would bury it just as badly, so the order attempts finish in
	/// must not change what comes back.
	#[tokio::test(start_paused = true)]
	async fn all_failures_are_reported_when_the_preferred_times_out_last() {
		let res: Result<&str, TestError> = race(
			Candidates::fixed([addr("1.1.1.1:1"), addr("2.2.2.2:2")]),
			Duration::from_millis(10),
			|dest| async move {
				if dest == addr("1.1.1.1:1") {
					tokio::time::sleep(Duration::from_secs(30)).await;
					Err(TestError::Dial("timed out"))
				} else {
					Err(TestError::Dial("invalid peer certificate"))
				}
			},
		)
		.await;
		assert_eq!(
			res,
			Err(TestError::All(vec![
				failed("1.1.1.1:1", "timed out"),
				failed("2.2.2.2:2", "invalid peer certificate"),
			]))
		);
	}

	/// One candidate is no race, so the caller keeps the error the backend
	/// produced (variant, source chain and all) instead of an aggregate of one.
	#[tokio::test(start_paused = true)]
	async fn a_lone_failure_is_returned_unwrapped() {
		let res: Result<&str, TestError> = race(Candidates::fixed([addr("1.1.1.1:1")]), DEFAULT_RACE, |_| async {
			Err(TestError::Dial("invalid peer certificate"))
		})
		.await;
		assert_eq!(res, Err(TestError::Dial("invalid peer certificate")));
	}

	#[test]
	fn describe_lists_every_attempt() {
		let failures = [failed("1.1.1.1:1", "timed out"), failed("2.2.2.2:2", "bad cert")];
		assert_eq!(describe(&failures), "1.1.1.1:1: timed out; 2.2.2.2:2: bad cert");
	}

	#[tokio::test(start_paused = true)]
	async fn losers_are_dropped_on_success() {
		// The pending loser holds a guard; race() returning must drop it.
		struct Guard(Arc<AtomicUsize>);
		impl Drop for Guard {
			fn drop(&mut self) {
				self.0.fetch_add(1, Ordering::SeqCst);
			}
		}

		let dropped = Arc::new(AtomicUsize::new(0));
		let count = dropped.clone();
		let res: Result<&str, TestError> = race(
			Candidates::fixed([addr("1.1.1.1:1"), addr("2.2.2.2:2")]),
			Duration::ZERO,
			move |dest| {
				let guard = Guard(count.clone());
				async move {
					if dest == addr("1.1.1.1:1") {
						let _guard = guard;
						std::future::pending().await
					} else {
						drop(guard);
						tokio::time::sleep(Duration::from_millis(1)).await;
						Ok("second")
					}
				}
			},
		)
		.await;
		assert_eq!(res, Ok("second"));
		assert_eq!(dropped.load(Ordering::SeqCst), 2, "the hung attempt was not aborted");
	}

	#[tokio::test(start_paused = true)]
	async fn zero_delay_dials_all_at_once() {
		let start = tokio::time::Instant::now();
		let res: Result<&str, TestError> = race(
			Candidates::fixed([addr("1.1.1.1:1"), addr("2.2.2.2:2")]),
			Duration::ZERO,
			|dest| async move {
				if dest == addr("1.1.1.1:1") {
					std::future::pending().await
				} else {
					Ok("second")
				}
			},
		)
		.await;
		assert_eq!(res, Ok("second"));
		assert_eq!(start.elapsed(), Duration::ZERO);
	}

	/// Nothing resolved, so there is no attempt to report and the resolution says
	/// why instead.
	#[tokio::test(start_paused = true)]
	async fn an_empty_resolution_reports_why() {
		let res: Result<&str, TestError> = race(Candidates::fixed([]), DEFAULT_RACE, |_| async {
			unreachable!("dialed without an address")
		})
		.await;
		assert_eq!(res, Err(TestError::Dial("no addresses")));
	}

	/// The first attempt goes out on the first answer, not the last: the AAAA
	/// query here is the one that never lands, which is exactly the case the
	/// parallel queries exist for.
	#[tokio::test(start_paused = true)]
	async fn dials_the_first_address_to_resolve() {
		let start = tokio::time::Instant::now();
		let res: Result<&str, TestError> = race(
			Candidates::slow(
				(&[], Duration::from_secs(30)),
				(&[addr("1.1.1.1:1")], Duration::from_millis(100)),
			),
			DEFAULT_RACE,
			|_| async { Ok("winner") },
		)
		.await;
		assert_eq!(res, Ok("winner"));
		assert_eq!(
			start.elapsed(),
			Duration::from_millis(100),
			"waited for the other query"
		);
	}

	/// A candidate that resolves after the stagger has already elapsed starts its
	/// attempt the moment it lands, rather than waiting out another stagger.
	///
	/// The IPv4-only answer is here at once and hangs when dialed; the full one,
	/// carrying the address that works, takes a second.
	#[tokio::test(start_paused = true)]
	async fn a_late_candidate_starts_as_soon_as_it_resolves() {
		let start = tokio::time::Instant::now();
		let res: Result<&str, TestError> = race(
			Candidates::slow(
				(&[addr("[2001:db8::1]:1"), addr("1.1.1.1:1")], Duration::from_secs(1)),
				(&[addr("1.1.1.1:1")], Duration::ZERO),
			),
			DEFAULT_RACE,
			|dest| async move {
				match dest.is_ipv6() {
					true => Ok("second"),
					false => std::future::pending().await,
				}
			},
		)
		.await;
		assert_eq!(res, Ok("second"));
		assert_eq!(start.elapsed(), Duration::from_secs(1));
	}
}
