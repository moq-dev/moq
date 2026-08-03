//! Happy Eyeballs (RFC 8305) address failover for client dials.
//!
//! DNS often returns both IPv6 and IPv4 addresses, and either family can be
//! silently broken (an unrouted AAAA, a blocked v4 path). Instead of dialing a
//! single address and waiting out the handshake timeout, [`race`] staggers
//! attempts across the resolved addresses, alternating families per
//! [`interleave`], and hands back the first connection to complete.

use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use futures::StreamExt;
use futures::stream::FuturesUnordered;

/// How long to wait before also dialing the next address, unless overridden by
/// `--client-failover-delay`. RFC 8305's recommended Connection Attempt Delay.
pub(crate) const DEFAULT_DELAY: Duration = Duration::from_millis(250);

/// Order resolved addresses for racing: keep the resolver's order within each
/// family (the OS already applies RFC 6724 destination selection), but alternate
/// families so attempt N+1 is always the other family from attempt N when one is
/// available. The first address keeps its position, so the resolver still picks
/// the preferred family.
pub(crate) fn interleave(addrs: impl IntoIterator<Item = SocketAddr>) -> Vec<SocketAddr> {
	let (mut a, mut b): (Vec<SocketAddr>, Vec<SocketAddr>) = (Vec::new(), Vec::new());
	for addr in addrs {
		if a.is_empty() || a[0].is_ipv4() == addr.is_ipv4() {
			a.push(addr);
		} else {
			b.push(addr);
		}
	}

	let mut out = Vec::with_capacity(a.len() + b.len());
	let (mut a, mut b) = (a.into_iter(), b.into_iter());
	loop {
		match (a.next(), b.next()) {
			(Some(x), Some(y)) => {
				out.push(x);
				out.push(y);
			}
			(Some(x), None) => out.push(x),
			(None, Some(y)) => out.push(y),
			(None, None) => break,
		}
	}
	out
}

/// [`interleave`], then adapt each address to the family of the `local` socket.
///
/// The QUIC backends send from one already-bound socket, so a candidate the
/// socket can't reach is converted when the conversion is lossless (IPv4 to
/// IPv4-mapped IPv6 for a dual-stack socket, and the reverse) and dropped when
/// it isn't. When every candidate would be dropped, they are kept as-is so the
/// dial surfaces the OS error instead of a confusing "no DNS entries". See
/// <https://github.com/moq-dev/moq/issues/1375> for the Windows failure this
/// family matching originally fixed.
pub(crate) fn match_local(addrs: impl IntoIterator<Item = SocketAddr>, local: SocketAddr) -> Vec<SocketAddr> {
	let candidates = interleave(addrs);

	let mut usable: Vec<SocketAddr> = candidates
		.iter()
		.map(|addr| normalize_family(*addr, local))
		.filter(|addr| addr.is_ipv4() == local.is_ipv4())
		.collect();
	usable.dedup();

	if usable.is_empty() { candidates } else { usable }
}

/// Convert `addr` to match the family of `local` when the conversion is
/// lossless: unwrap IPv4-mapped IPv6 to IPv4, or wrap IPv4 as IPv4-mapped IPv6.
fn normalize_family(addr: SocketAddr, local: SocketAddr) -> SocketAddr {
	match (addr, local.is_ipv4()) {
		(SocketAddr::V6(v6), true) => match v6.ip().to_ipv4_mapped() {
			Some(v4) => SocketAddr::new(IpAddr::V4(v4), v6.port()),
			None => addr,
		},
		(SocketAddr::V4(v4), false) => SocketAddr::new(IpAddr::V6(v4.ip().to_ipv6_mapped()), v4.port()),
		_ => addr,
	}
}

/// Render each failed attempt as `addr: error`, joined by `; `.
pub(crate) fn describe<E: std::fmt::Display>(failures: &[(SocketAddr, E)]) -> String {
	failures
		.iter()
		.map(|(addr, err)| format!("{addr}: {err}"))
		.collect::<Vec<_>>()
		.join("; ")
}

/// Dial `candidates` in order, starting the next attempt `delay` after the
/// previous one (or immediately when it fails), and return the first success.
/// The remaining attempts are dropped, which aborts them.
///
/// A `delay` of zero dials every candidate at once. When every attempt fails,
/// every error is returned, ordered by candidate rather than by when it
/// finished. Singling one out means guessing, and both obvious guesses are
/// wrong in a case this exists to handle: the most preferred candidate is the
/// broken family failover routes around, while the last to finish is whichever
/// address blackholed until its timeout, and either can bury a rejected
/// certificate or a refused port that the caller could act on.
///
/// `candidates` must not be empty; callers map an empty DNS answer to their own
/// error before racing.
pub(crate) async fn race<C, E, F, Fut>(
	candidates: Vec<SocketAddr>,
	delay: Duration,
	mut dial: F,
) -> Result<C, Vec<(SocketAddr, E)>>
where
	F: FnMut(SocketAddr) -> Fut,
	Fut: Future<Output = Result<C, E>>,
	E: std::fmt::Display,
{
	let mut remaining = candidates.into_iter();
	let mut attempts = FuturesUnordered::new();
	let mut failures: Vec<(usize, SocketAddr, E)> = Vec::new();

	let mut next_index = 0;
	let mut start = |addr: SocketAddr, attempts: &mut FuturesUnordered<_>| {
		let index = next_index;
		next_index += 1;
		tracing::debug!(%addr, index, "dialing");
		let attempt = dial(addr);
		attempts.push(async move { (index, addr, attempt.await) });
	};

	let first = remaining.next().expect("no candidates to dial");
	start(first, &mut attempts);

	loop {
		tokio::select! {
			// Bias toward a finished attempt so a success that raced the timer wins
			// without dialing another address for nothing.
			biased;

			res = attempts.next() => {
				let (index, addr, res) = res.expect("attempts can't be empty here");
				match res {
					Ok(conn) => {
						tracing::debug!(%addr, index, "connected");
						return Ok(conn);
					}
					Err(err) => {
						tracing::warn!(%addr, %err, "connection attempt failed");
						failures.push((index, addr, err));
						// A failure starts the next candidate immediately (RFC 8305
						// section 5) rather than waiting out the stagger delay.
						if let Some(addr) = remaining.next() {
							start(addr, &mut attempts);
						} else if attempts.is_empty() {
							// Report in candidate order, not the order they finished,
							// so the same DNS answer always reads the same way.
							failures.sort_by_key(|(index, _, _)| *index);
							return Err(failures.into_iter().map(|(_, addr, err)| (addr, err)).collect());
						}
					}
				}
			}

			// Recreated each iteration, so the stagger measures from the most
			// recently started attempt.
			_ = tokio::time::sleep(delay), if remaining.len() > 0 => {
				let addr = remaining.next().expect("guarded by remaining.len()");
				start(addr, &mut attempts);
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::sync::Arc;
	use std::sync::atomic::{AtomicUsize, Ordering};

	fn v4(s: &str) -> SocketAddr {
		s.parse().unwrap()
	}

	/// What `race` hands back when every candidate fails.
	type Failures = Vec<(SocketAddr, &'static str)>;

	#[test]
	fn interleave_alternates_families() {
		let addrs = [
			v4("[2001:db8::1]:443"),
			v4("[2001:db8::2]:443"),
			v4("1.2.3.4:443"),
			v4("5.6.7.8:443"),
		];
		assert_eq!(
			interleave(addrs),
			vec![
				v4("[2001:db8::1]:443"),
				v4("1.2.3.4:443"),
				v4("[2001:db8::2]:443"),
				v4("5.6.7.8:443"),
			]
		);
	}

	#[test]
	fn interleave_keeps_the_resolver_preferred_family_first() {
		// IPv4 first in the answer stays first, even though IPv6 exists.
		let addrs = [v4("1.2.3.4:443"), v4("[2001:db8::1]:443")];
		assert_eq!(interleave(addrs), vec![v4("1.2.3.4:443"), v4("[2001:db8::1]:443")]);
	}

	#[test]
	fn interleave_single_family_passthrough() {
		let addrs = [v4("1.2.3.4:443"), v4("5.6.7.8:443")];
		assert_eq!(interleave(addrs), addrs.to_vec());
	}

	#[test]
	fn match_local_prefers_matching_family() {
		let a4 = v4("127.0.0.1:443");
		let a6 = v4("[::1]:443");

		// IPv6 listed first, but local socket is IPv4: only IPv4 is usable.
		assert_eq!(match_local([a6, a4], v4("0.0.0.0:0")), vec![a4]);
		// IPv4 wraps to IPv4-mapped for an IPv6 (dual-stack) socket.
		assert_eq!(
			match_local([a4, a6], v4("[::]:0")),
			vec![v4("[::ffff:127.0.0.1]:443"), a6]
		);
	}

	#[test]
	fn match_local_unwraps_v4_mapped_for_v4_socket() {
		let mapped = v4("[::ffff:127.0.0.1]:443");
		assert_eq!(match_local([mapped], v4("0.0.0.0:0")), vec![v4("127.0.0.1:443")]);
	}

	#[test]
	fn match_local_falls_back_for_unmappable_v6() {
		// IPv4 socket with only a true IPv6 entry: no conversion possible, keep it
		// so the OS surfaces a clear error.
		let a6 = v4("[2001:db8::1]:443");
		assert_eq!(match_local([a6], v4("0.0.0.0:0")), vec![a6]);
	}

	#[test]
	fn match_local_empty() {
		assert!(match_local(std::iter::empty(), v4("0.0.0.0:0")).is_empty());
	}

	#[tokio::test(start_paused = true)]
	async fn first_success_returns_immediately() {
		let dials = Arc::new(AtomicUsize::new(0));
		let counter = dials.clone();
		let res: Result<&str, Failures> = race(vec![v4("1.1.1.1:1"), v4("2.2.2.2:2")], DEFAULT_DELAY, move |_| {
			counter.fetch_add(1, Ordering::SeqCst);
			async { Ok("winner") }
		})
		.await;
		assert_eq!(res, Ok("winner"));
		assert_eq!(dials.load(Ordering::SeqCst), 1, "no second dial after a fast success");
	}

	#[tokio::test(start_paused = true)]
	async fn second_wins_when_first_hangs() {
		let start = tokio::time::Instant::now();
		let res: Result<&str, Failures> = race(
			vec![v4("1.1.1.1:1"), v4("2.2.2.2:2")],
			DEFAULT_DELAY,
			|addr| async move {
				if addr == v4("1.1.1.1:1") {
					std::future::pending().await
				} else {
					Ok("second")
				}
			},
		)
		.await;
		assert_eq!(res, Ok("second"));
		assert_eq!(start.elapsed(), DEFAULT_DELAY, "second dial waits out the stagger");
	}

	#[tokio::test(start_paused = true)]
	async fn failure_starts_the_next_attempt_immediately() {
		let start = tokio::time::Instant::now();
		let res: Result<&str, Failures> = race(
			vec![v4("1.1.1.1:1"), v4("2.2.2.2:2")],
			DEFAULT_DELAY,
			|addr| async move {
				if addr == v4("1.1.1.1:1") {
					Err("boom")
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
		let res: Result<&str, Failures> = race(
			vec![v4("1.1.1.1:1"), v4("2.2.2.2:2")],
			Duration::from_millis(10),
			|addr| async move {
				if addr == v4("1.1.1.1:1") {
					Err("network unreachable")
				} else {
					tokio::time::sleep(Duration::from_secs(1)).await;
					Err("invalid peer certificate")
				}
			},
		)
		.await;
		assert_eq!(
			res,
			Err(vec![
				(v4("1.1.1.1:1"), "network unreachable"),
				(v4("2.2.2.2:2"), "invalid peer certificate"),
			])
		);
	}

	/// The inverse: the preferred candidate blackholes until its timeout while
	/// the fallback reports the actionable error early. Reporting the last error
	/// to finish would bury it just as badly, so the order attempts finish in
	/// must not change what comes back.
	#[tokio::test(start_paused = true)]
	async fn all_failures_are_reported_when_the_preferred_times_out_last() {
		let res: Result<&str, Failures> = race(
			vec![v4("1.1.1.1:1"), v4("2.2.2.2:2")],
			Duration::from_millis(10),
			|addr| async move {
				if addr == v4("1.1.1.1:1") {
					tokio::time::sleep(Duration::from_secs(30)).await;
					Err("timed out")
				} else {
					Err("invalid peer certificate")
				}
			},
		)
		.await;
		assert_eq!(
			res,
			Err(vec![
				(v4("1.1.1.1:1"), "timed out"),
				(v4("2.2.2.2:2"), "invalid peer certificate"),
			])
		);
	}

	#[test]
	fn describe_lists_every_attempt() {
		let failures = [(v4("1.1.1.1:1"), "timed out"), (v4("2.2.2.2:2"), "bad cert")];
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
		let res: Result<&str, Failures> = race(vec![v4("1.1.1.1:1"), v4("2.2.2.2:2")], Duration::ZERO, move |addr| {
			let guard = Guard(count.clone());
			async move {
				if addr == v4("1.1.1.1:1") {
					let _guard = guard;
					std::future::pending().await
				} else {
					drop(guard);
					tokio::time::sleep(Duration::from_millis(1)).await;
					Ok("second")
				}
			}
		})
		.await;
		assert_eq!(res, Ok("second"));
		assert_eq!(dropped.load(Ordering::SeqCst), 2, "the hung attempt was not aborted");
	}

	#[tokio::test(start_paused = true)]
	async fn zero_delay_dials_all_at_once() {
		let start = tokio::time::Instant::now();
		let res: Result<&str, Failures> = race(
			vec![v4("1.1.1.1:1"), v4("2.2.2.2:2")],
			Duration::ZERO,
			|addr| async move {
				if addr == v4("1.1.1.1:1") {
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
}
