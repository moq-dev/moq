//! DNS-phase Happy Eyeballs (RFC 8305 section 3) for client dials.
//!
//! A dial runs two lookups at once: the full one every other program does, which
//! doesn't answer until both the A and AAAA records are in, and an IPv4-only one
//! that answers without waiting for AAAA. The full answer is authoritative and
//! normally arrives first or close enough, so the addresses and their order are
//! exactly what the platform would have handed us. When the AAAA record is slow
//! or silently dropped, which is the case Happy Eyeballs exists for, the
//! IPv4-only answer takes over after the Resolution Delay rather than the dial
//! waiting out the resolver's timeout.
//!
//! Both go through the system resolver, so `/etc/hosts`, nsswitch (mDNS and
//! friends), search domains and split-DNS resolve the way they do for everything
//! else on the host. Keeping the full lookup is what preserves the *cross-family*
//! half of that: its order is RFC 6724 destination selection, which knows things
//! this layer can't, like whether there is a usable IPv6 source address at all
//! and what `gai.conf` prefers. Each call blocks its thread, so they run on the
//! blocking pool.
//!
//! The one type callers see is [`Candidates`]: build it, hand it to the race,
//! and it yields addresses in the order to dial them.

use std::collections::{HashSet, VecDeque};
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use futures::FutureExt;

use crate::connect::DEFAULT_RESOLUTION_DELAY;

/// Which of the two lookups a query is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lookup {
	/// Every family, in the platform's own order. Authoritative, but it answers
	/// only once both records are in.
	Full,

	/// IPv4 only, which answers without waiting for the AAAA record.
	Ipv4,
}

impl Lookup {
	/// The `getaddrinfo` hints for this lookup.
	fn hints(self) -> dns_lookup::AddrInfoHints {
		dns_lookup::AddrInfoHints {
			address: match self {
				// Zero is `AF_UNSPEC`: whatever the host has.
				Self::Full => 0,
				Self::Ipv4 => dns_lookup::AddrFamily::Inet.into(),
			},
			// One entry per address rather than one per socket type: a UDP and a TCP
			// entry for the same address are the same candidate to us.
			socktype: dns_lookup::SockType::Stream.into(),
			..Default::default()
		}
	}
}

/// Which family an address belongs to, which is what decides dial order once the
/// addresses are in hand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Family {
	V6,
	V4,
}

impl Family {
	/// The family an address belongs to.
	fn of(addr: SocketAddr) -> Self {
		match addr.is_ipv4() {
			true => Self::V4,
			false => Self::V6,
		}
	}

	/// The other one.
	fn other(self) -> Self {
		match self {
			Self::V6 => Self::V4,
			Self::V4 => Self::V6,
		}
	}
}

/// One lookup, in flight until it answers.
#[derive(Default)]
struct Query {
	/// The `getaddrinfo` call, dropped once it answers.
	call: Option<tokio::task::JoinHandle<io::Result<Vec<SocketAddr>>>>,

	/// Why it answered with nothing.
	error: Option<io::Error>,
}

impl Query {
	/// Start a lookup on the blocking pool.
	fn start(host: &str, port: u16, lookup: Lookup) -> Self {
		let host = host.to_owned();
		let call = tokio::task::spawn_blocking(move || resolve(&host, port, lookup));

		Self {
			call: Some(call),
			..Default::default()
		}
	}

	/// Whether this lookup still owes an answer.
	fn pending(&self) -> bool {
		self.call.is_some()
	}

	/// Wait for the answer, recording a failure as no addresses.
	///
	/// Never resolves once there is no call left, so it can sit in a `select!` arm
	/// without spinning. Cancelling it loses nothing: the handle stays put and the
	/// answer is taken the next time it is awaited.
	async fn answer(&mut self, lookup: Lookup) -> Vec<SocketAddr> {
		let Some(call) = self.call.as_mut() else {
			return std::future::pending().await;
		};

		let res = call.await;
		self.record(lookup, res)
	}

	/// The answer if it has already landed, without waiting for one.
	///
	/// Polling with a no-op waker drops nothing: every caller follows up with
	/// [`answer`](Self::answer), which registers a real one.
	fn ready(&mut self, lookup: Lookup) -> Option<Vec<SocketAddr>> {
		let res = self.call.as_mut()?.now_or_never()?;
		Some(self.record(lookup, res))
	}

	/// Record what the call returned, closing it out.
	fn record(
		&mut self,
		lookup: Lookup,
		res: Result<io::Result<Vec<SocketAddr>>, tokio::task::JoinError>,
	) -> Vec<SocketAddr> {
		// The only `JoinError` reachable here is a panic in the lookup itself: the
		// handle is never aborted while it is still in `self`.
		let res = res.unwrap_or_else(|err| Err(io::Error::other(err)));
		self.call = None;

		match res {
			Ok(addrs) => {
				tracing::debug!(?lookup, count = addrs.len(), "resolved");
				addrs
			}
			// Not a warning: the IPv4-only lookup coming back empty is the normal
			// state of every host that has only a AAAA record. It matters only when
			// nothing resolves at all, and then it is in the returned error.
			Err(err) => {
				tracing::debug!(?lookup, %err, "lookup failed");
				self.error = Some(err);
				Vec::new()
			}
		}
	}
}

/// The addresses a dial should try, in the order to try them.
///
/// The families alternate, so attempt N+1 is always the other family from attempt
/// N while both have an address left. Which family leads is the platform's call,
/// not ours. Addresses arrive as their lookup answers, so a dial can start before
/// the full one is done.
pub(crate) struct Candidates {
	/// The all-families lookup.
	full: Query,

	/// The IPv4-only lookup.
	ipv4: Query,

	/// Addresses waiting to be dialed, split by family so attempts can alternate.
	/// Order within a family is the resolver's own.
	v6: VecDeque<SocketAddr>,
	v4: VecDeque<SocketAddr>,

	/// The family the next candidate should come from.
	next: Family,

	/// How long the first candidate waits for the full answer, and whether that
	/// wait has already happened.
	delay: Duration,
	delayed: bool,

	/// The local socket candidates are adapted to, and whether it is dual-stack.
	local: Option<(SocketAddr, bool)>,

	/// Candidates the local socket can't reach, kept as a last resort.
	unreachable: VecDeque<SocketAddr>,

	/// Every address handed out, so the two lookups can't dial one twice.
	seen: HashSet<SocketAddr>,

	/// Whether the local socket could reach any candidate, which decides whether
	/// the `unreachable` ones are worth dialing at all.
	usable: bool,

	/// How many candidates have been handed out, against `limit`.
	count: usize,
	limit: usize,
}

impl Default for Candidates {
	fn default() -> Self {
		Self {
			full: Query::default(),
			ipv4: Query::default(),
			v6: VecDeque::new(),
			v4: VecDeque::new(),
			// Only until an answer says otherwise. RFC 8305 section 4: absent a
			// preference, IPv6 goes first.
			next: Family::V6,
			delay: DEFAULT_RESOLUTION_DELAY,
			delayed: false,
			local: None,
			unreachable: VecDeque::new(),
			seen: HashSet::new(),
			usable: false,
			count: 0,
			limit: usize::MAX,
		}
	}
}

impl Candidates {
	/// Resolve `host`, running the full lookup and an IPv4-only one at once.
	///
	/// Both start here rather than on the first [`next`](Self::next), so they
	/// overlap whatever else the dial does first.
	///
	/// `delay` is how long the first candidate holds back an IPv4 address while the
	/// full answer is still outstanding.
	pub(crate) fn resolve(host: url::Host<&str>, port: u16, delay: Duration) -> Self {
		let domain = match host {
			// A literal needs no resolver, and handing one to `getaddrinfo` just
			// gets it back.
			url::Host::Ipv4(ip) => return Self::fixed([SocketAddr::new(ip.into(), port)]),
			url::Host::Ipv6(ip) => return Self::fixed([SocketAddr::new(ip.into(), port)]),
			// A non-special scheme (`moqt://192.0.2.1`) parses its host as a domain,
			// so the literal check can't be left to the URL parser.
			url::Host::Domain(domain) => match domain.parse::<IpAddr>() {
				Ok(ip) => return Self::fixed([SocketAddr::new(ip, port)]),
				Err(_) => domain,
			},
		};

		Self {
			full: Query::start(domain, port, Lookup::Full),
			ipv4: Query::start(domain, port, Lookup::Ipv4),
			delay,
			..Default::default()
		}
	}

	/// The addresses to dial when they are already known, in the caller's order.
	///
	/// The families still alternate, but the first address keeps its position, so a
	/// caller that has already chosen a preferred family keeps it.
	pub(crate) fn fixed(addrs: impl IntoIterator<Item = SocketAddr>) -> Self {
		let mut this = Self::default();

		for addr in addrs {
			if this.v6.is_empty() && this.v4.is_empty() {
				this.next = Family::of(addr);
			}
			this.queue(addr);
		}

		this
	}

	/// Adapt each candidate to the family of the `local` socket.
	///
	/// The QUIC backends send from one already-bound socket, so a candidate the
	/// socket can't reach is converted when the conversion is lossless (IPv4 to
	/// IPv4-mapped IPv6 for a dual-stack socket, and the reverse) and dropped when
	/// it isn't. `dual_stack` is [`crate::bind::udp_is_dual_stack`] for that
	/// socket. When every candidate would be dropped, the normalized candidates
	/// are handed out anyway so the dial surfaces the OS error instead of a
	/// confusing "no DNS entries". See <https://github.com/moq-dev/moq/issues/1375>
	/// for the Windows failure this family matching originally fixed.
	#[cfg(any(feature = "noq", feature = "quinn", feature = "quiche", test))]
	pub(crate) fn with_local(mut self, local: SocketAddr, dual_stack: bool) -> Self {
		self.local = Some((local, dual_stack));
		self
	}

	/// Hand out at most `max` candidates.
	///
	/// For a backend that can't run overlapping attempts: quiche binds a socket
	/// per attempt, and a pinned source port only fits one at a time. That one
	/// attempt goes to the platform's own first choice, so this must not be paired
	/// with an order we invented.
	#[cfg(any(feature = "quiche", test))]
	pub(crate) fn with_limit(mut self, max: usize) -> Self {
		self.limit = max;
		self
	}

	/// The next address to dial, or `None` once every answer has been handed out.
	pub(crate) async fn next(&mut self) -> Option<SocketAddr> {
		if self.count >= self.limit {
			return None;
		}

		loop {
			// Fold in whatever has answered since the last candidate went out. Without
			// this, a fast-lane address still sitting in the queue would be handed out
			// ahead of an authoritative answer that has already landed, and the order
			// the platform chose would only take effect once the fast lane ran dry.
			self.poll_answers();

			if let Some(addr) = self.take(self.next) {
				return Some(addr);
			}

			// The preferred family has nothing ready, so consider the other one.
			if !self.queued(self.next.other()).is_empty() {
				// RFC 8305 section 3: an IPv4-only answer that merely beat the full one
				// doesn't get to pick the family on its own, so hold it back briefly and
				// give the platform's own choice a chance to land. Only the first
				// candidate waits: after that the families alternate anyway, and the
				// stagger between attempts dwarfs this.
				if self.count == 0 && !self.delayed && self.full.pending() {
					self.delayed = true;
					let _ = tokio::time::timeout(self.delay, self.answer_full()).await;
					continue;
				}

				if let Some(addr) = self.take(self.next.other()) {
					return Some(addr);
				}
				continue;
			}

			if !self.full.pending() && !self.ipv4.pending() {
				// Nothing left to wait for. A candidate the local socket can't reach
				// still beats no candidate at all, so dial those rather than report an
				// empty answer we didn't get.
				if !self.usable
					&& let Some(addr) = self.unreachable.pop_front()
				{
					self.count += 1;
					return Some(addr);
				}

				return None;
			}

			self.answer_any().await;
		}
	}

	/// Why the dial had nothing to try, and `None` when the lookup simply answered
	/// with no addresses.
	///
	/// The full lookup speaks for the host: the IPv4-only one covers ground it
	/// already covers, so its error only stands in when the full one had none.
	///
	/// Only meaningful once [`next`](Self::next) has returned `None` without ever
	/// handing out an address. Consumes the error, so it reports once.
	pub(crate) fn failure(&mut self) -> Option<io::Error> {
		self.full.error.take().or_else(|| self.ipv4.error.take())
	}

	/// Hand out the next address from `family`, skipping duplicates and setting
	/// aside what the local socket can't reach.
	fn take(&mut self, family: Family) -> Option<SocketAddr> {
		loop {
			let addr = self.queued(family).pop_front()?;
			let addr = match self.local {
				Some((local, _)) => normalize_family(addr, local),
				None => addr,
			};

			// Duplicates cost a wasted dial and a repeated line in the error, and they
			// don't arrive adjacent: the families alternate, the two lookups overlap,
			// and normalizing collapses `1.2.3.4` and `::ffff:1.2.3.4` into one value
			// only afterwards. So dedup by value, keeping the first occurrence's
			// position.
			if !self.seen.insert(addr) {
				continue;
			}

			if let Some((local, dual_stack)) = self.local
				&& !addressable(addr, local, dual_stack)
			{
				self.unreachable.push_back(addr);
				continue;
			}

			self.usable = true;
			self.count += 1;
			self.next = family.other();
			return Some(addr);
		}
	}

	/// This family's queue.
	fn queued(&mut self, family: Family) -> &mut VecDeque<SocketAddr> {
		match family {
			Family::V6 => &mut self.v6,
			Family::V4 => &mut self.v4,
		}
	}

	/// Queue one address behind its family.
	fn queue(&mut self, addr: SocketAddr) {
		self.queued(Family::of(addr)).push_back(addr);
	}

	/// Fold in every answer that has already landed, without waiting for one.
	fn poll_answers(&mut self) {
		// Full first: accepting it closes out the IPv4-only lookup, which then has
		// nothing left to fold in.
		if let Some(addrs) = self.full.ready(Lookup::Full) {
			self.accept(Lookup::Full, addrs);
		}

		if let Some(addrs) = self.ipv4.ready(Lookup::Ipv4) {
			self.accept(Lookup::Ipv4, addrs);
		}
	}

	/// Wait for the full lookup to answer.
	async fn answer_full(&mut self) {
		let addrs = self.full.answer(Lookup::Full).await;
		self.accept(Lookup::Full, addrs);
	}

	/// Wait for whichever lookup answers next. Hangs when neither is in flight, so
	/// the caller checks first.
	async fn answer_any(&mut self) {
		// Split the borrow: each arm only touches its own query, and both are done
		// with it before `accept` takes `self` again.
		let (full, ipv4) = (&mut self.full, &mut self.ipv4);

		let (lookup, addrs) = tokio::select! {
			addrs = full.answer(Lookup::Full) => (Lookup::Full, addrs),
			addrs = ipv4.answer(Lookup::Ipv4) => (Lookup::Ipv4, addrs),
		};

		self.accept(lookup, addrs);
	}

	/// Fold a lookup's answer into the queues.
	fn accept(&mut self, lookup: Lookup, addrs: Vec<SocketAddr>) {
		// An empty answer from the full lookup means it failed, so leave the
		// IPv4-only one to speak for the host.
		if lookup == Lookup::Ipv4 || addrs.is_empty() {
			for addr in addrs {
				self.queue(addr);
			}
			return;
		}

		// The full answer supersedes the IPv4-only one: the same records, sorted by
		// the platform's own destination-address selection. Anything already dialed
		// is held by `seen`, so requeueing can't repeat it. The lookup still in
		// flight has nothing to add, and `getaddrinfo` can't be cancelled, so its
		// thread just finishes unobserved.
		self.ipv4.call = None;
		self.v6.clear();
		self.v4.clear();

		// Which family leads is RFC 8305 section 4's destination-address sort, which
		// the resolver has already done: a host with no usable IPv6 source address,
		// or a `gai.conf` that prefers IPv4, comes back IPv4 first. Only the first
		// attempt asks; after that the families alternate.
		if self.count == 0
			&& let Some(first) = addrs.first()
		{
			self.next = Family::of(*first);
		}

		for addr in addrs {
			self.queue(addr);
		}
	}
}

/// One `getaddrinfo` call, blocking until the resolver answers.
fn resolve(host: &str, port: u16, lookup: Lookup) -> io::Result<Vec<SocketAddr>> {
	// No service is passed (there is no `moq` in /etc/services); the port is
	// stamped on here instead.
	let answers = dns_lookup::getaddrinfo(Some(host), None, Some(lookup.hints())).map_err(io::Error::from)?;
	answers.map(|answer| Ok(with_port(answer?.sockaddr, port))).collect()
}

/// The address to dial for one answer: whatever the resolver returned, at our
/// port.
///
/// Rebuilding it from the IP alone would drop the rest of the `sockaddr`, and an
/// IPv6 one carries a scope id that a link-local address (`fe80::1%eth0`, which
/// is what mDNS hands back on a LAN) can't be reached without.
fn with_port(mut addr: SocketAddr, port: u16) -> SocketAddr {
	addr.set_port(port);
	addr
}

/// Whether a socket bound to `local` can send to `dest`.
///
/// Mostly this is the address family, but reaching IPv4 from an IPv6 socket has
/// a wrinkle: it means sending to an IPv4-mapped destination, which the kernel
/// turns back into a real IPv4 packet, and that needs an IPv4 source address. So
/// it takes both a socket that is actually dual-stack (`IPV6_V6ONLY` cleared,
/// which [`crate::bind::udp`] only attempts) and a bind that left an IPv4 source
/// to use: `[::]` does, since the kernel picks the source, and an IPv4-mapped
/// bind already is one, but a concrete IPv6 bind is not. The mirror holds too:
/// an IPv4-mapped bind can't reach a real IPv6 destination.
fn addressable(dest: SocketAddr, local: SocketAddr, dual_stack: bool) -> bool {
	let (SocketAddr::V6(dest), SocketAddr::V6(local)) = (dest, local) else {
		return dest.is_ipv4() == local.is_ipv4();
	};

	match (dest.ip().to_ipv4_mapped(), local.ip().to_ipv4_mapped()) {
		(Some(_), None) => dual_stack && local.ip().is_unspecified(),
		(None, Some(_)) => false,
		_ => true,
	}
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

#[cfg(test)]
impl Candidates {
	/// Answers that land after a delay, for tests (here and in [`crate::failover`])
	/// that need a dial to start before resolution has finished. The Resolution
	/// Delay is off: these exercise the caller rather than the family preference.
	pub(crate) fn slow(full: (&[SocketAddr], Duration), ipv4: (&[SocketAddr], Duration)) -> Self {
		Self {
			full: Query::slow(full.0, full.1),
			ipv4: Query::slow(ipv4.0, ipv4.1),
			delay: Duration::ZERO,
			..Default::default()
		}
	}
}

#[cfg(test)]
impl Query {
	/// A lookup that answers with `addrs` after `delay`.
	fn slow(addrs: &[SocketAddr], delay: Duration) -> Self {
		let addrs = addrs.to_vec();

		Self {
			call: Some(tokio::spawn(async move {
				tokio::time::sleep(delay).await;
				Ok(addrs)
			})),
			..Default::default()
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn addr(s: &str) -> SocketAddr {
		s.parse().unwrap()
	}

	fn addrs(list: &[&str]) -> Vec<SocketAddr> {
		list.iter().map(|s| addr(s)).collect()
	}

	/// Every candidate, in the order they're handed out.
	async fn drain(mut candidates: Candidates) -> Vec<SocketAddr> {
		let mut out = Vec::new();
		while let Some(addr) = candidates.next().await {
			out.push(addr);
		}
		out
	}

	/// A dial whose full lookup has already answered, in the resolver's order.
	fn answered(list: &[&str]) -> Candidates {
		let mut candidates = Candidates::default();
		candidates.accept(Lookup::Full, addrs(list));
		candidates
	}

	#[tokio::test]
	async fn alternates_families() {
		let candidates = answered(&["[2001:db8::1]:443", "[2001:db8::2]:443", "1.2.3.4:443", "5.6.7.8:443"]);
		assert_eq!(
			drain(candidates).await,
			addrs(&["[2001:db8::1]:443", "1.2.3.4:443", "[2001:db8::2]:443", "5.6.7.8:443",])
		);
	}

	/// The resolver sorts across families (RFC 6724 destination selection), and
	/// that choice is not ours to second-guess: a host with no usable IPv6 source
	/// address, or a `gai.conf` preferring IPv4, answers IPv4 first and has to be
	/// dialed IPv4 first.
	#[tokio::test]
	async fn the_resolver_picks_the_leading_family() {
		let candidates = answered(&["1.2.3.4:443", "[2001:db8::1]:443"]);
		assert_eq!(drain(candidates).await, addrs(&["1.2.3.4:443", "[2001:db8::1]:443"]));

		let candidates = answered(&["[2001:db8::1]:443", "1.2.3.4:443"]);
		assert_eq!(drain(candidates).await, addrs(&["[2001:db8::1]:443", "1.2.3.4:443"]));
	}

	/// quiche pins its source port and so gets one attempt only. It has to be the
	/// address the platform put first, or an IPv4-only host loses its only shot.
	#[tokio::test]
	async fn a_single_attempt_follows_the_resolver() {
		let candidates = answered(&["1.2.3.4:443", "[2001:db8::1]:443"]).with_limit(1);
		assert_eq!(drain(candidates).await, addrs(&["1.2.3.4:443"]));
	}

	#[tokio::test]
	async fn single_family_passthrough() {
		let list = ["1.2.3.4:443", "5.6.7.8:443"];
		assert_eq!(drain(answered(&list)).await, addrs(&list));
	}

	#[tokio::test]
	async fn fixed_keeps_the_leading_family() {
		// A caller that already ordered its addresses keeps the family it chose.
		let list = addrs(&["1.2.3.4:443", "[2001:db8::1]:443"]);
		assert_eq!(drain(Candidates::fixed(list.clone())).await, list);
	}

	#[tokio::test]
	async fn local_prefers_matching_family() {
		// IPv6 answered first, but the local socket is IPv4: only IPv4 is usable.
		let candidates = answered(&["[::1]:443", "127.0.0.1:443"]).with_local(addr("0.0.0.0:0"), false);
		assert_eq!(drain(candidates).await, addrs(&["127.0.0.1:443"]));

		// IPv4 wraps to IPv4-mapped for an IPv6 (dual-stack) socket.
		let candidates = answered(&["[::1]:443", "127.0.0.1:443"]).with_local(addr("[::]:0"), true);
		assert_eq!(drain(candidates).await, addrs(&["[::1]:443", "[::ffff:127.0.0.1]:443"]));
	}

	#[tokio::test]
	async fn local_skips_mapped_ipv4_on_a_v6_only_socket() {
		let candidates = answered(&["[2001:db8::1]:443", "192.0.2.1:443"]).with_local(addr("[::]:0"), false);
		assert_eq!(drain(candidates).await, addrs(&["[2001:db8::1]:443"]));
	}

	#[tokio::test]
	async fn local_skips_ipv4_for_a_concrete_v6_bind() {
		let candidates = answered(&["[2001:db8::1]:443", "192.0.2.1:443"]).with_local(addr("[2001:db8::5]:0"), true);
		assert_eq!(drain(candidates).await, addrs(&["[2001:db8::1]:443"]));
	}

	#[tokio::test]
	async fn local_keeps_the_normalized_fallback_when_none_are_usable() {
		let candidates = answered(&["192.0.2.1:443"]).with_local(addr("[::]:0"), false);
		assert_eq!(drain(candidates).await, addrs(&["[::ffff:192.0.2.1]:443"]));
	}

	#[tokio::test]
	async fn local_unwraps_v4_mapped_for_a_v4_socket() {
		let candidates = answered(&["[::ffff:127.0.0.1]:443"]).with_local(addr("0.0.0.0:0"), false);
		assert_eq!(drain(candidates).await, addrs(&["127.0.0.1:443"]));
	}

	#[tokio::test]
	async fn local_falls_back_for_an_unmappable_v6() {
		// IPv4 socket with only a true IPv6 entry: no conversion possible, keep it
		// so the OS surfaces a clear error.
		let candidates = answered(&["[2001:db8::1]:443"]).with_local(addr("0.0.0.0:0"), false);
		assert_eq!(drain(candidates).await, addrs(&["[2001:db8::1]:443"]));
	}

	#[tokio::test]
	async fn empty_yields_nothing() {
		assert!(drain(Candidates::fixed([])).await.is_empty());
		assert!(Candidates::fixed([]).failure().is_none());
	}

	#[tokio::test]
	async fn dedups_the_answer() {
		let candidates = answered(&["[2001:db8::1]:443", "1.2.3.4:443", "1.2.3.4:443"]);
		assert_eq!(drain(candidates).await, addrs(&["[2001:db8::1]:443", "1.2.3.4:443"]));
	}

	#[tokio::test]
	async fn dedups_normalized_forms() {
		// The same address twice, once already IPv4-mapped: two families going in,
		// one candidate coming out.
		let candidates = answered(&["[::ffff:1.2.3.4]:443", "1.2.3.4:443"]).with_local(addr("[::]:0"), true);
		assert_eq!(drain(candidates).await, addrs(&["[::ffff:1.2.3.4]:443"]));

		let candidates = answered(&["[::ffff:1.2.3.4]:443", "1.2.3.4:443"]).with_local(addr("0.0.0.0:0"), false);
		assert_eq!(drain(candidates).await, addrs(&["1.2.3.4:443"]));
	}

	/// An IP literal is dialed as-is. `https` parses one into `Host::Ipv4`, while
	/// a non-special scheme like `moqt` hands back a `Host::Domain` that happens
	/// to be numeric; both have to skip the resolver.
	#[tokio::test]
	async fn ip_literals_skip_the_resolver() {
		let cases = [
			("https://192.0.2.1", "192.0.2.1:443"),
			("moqt://192.0.2.1", "192.0.2.1:443"),
			("https://[2001:db8::1]", "[2001:db8::1]:443"),
			("moqt://[2001:db8::1]", "[2001:db8::1]:443"),
		];

		for (url, want) in cases {
			let url = url::Url::parse(url).unwrap();
			let candidates = Candidates::resolve(url.host().unwrap(), 443, DEFAULT_RESOLUTION_DELAY);
			assert_eq!(drain(candidates).await, vec![addr(want)], "{url}");
		}
	}

	/// The IPv4-only answer landing first must not pick the family on its own: the
	/// dial waits out the Resolution Delay to see what the platform says.
	#[tokio::test(start_paused = true)]
	async fn ipv4_waits_out_the_resolution_delay_for_the_full_answer() {
		let mut candidates = Candidates::slow(
			(
				&addrs(&["[2001:db8::1]:443", "1.2.3.4:443"]),
				DEFAULT_RESOLUTION_DELAY / 2,
			),
			(&addrs(&["1.2.3.4:443"]), Duration::ZERO),
		);
		candidates.delay = DEFAULT_RESOLUTION_DELAY;

		let start = tokio::time::Instant::now();
		assert_eq!(candidates.next().await, Some(addr("[2001:db8::1]:443")));
		assert_eq!(
			start.elapsed(),
			DEFAULT_RESOLUTION_DELAY / 2,
			"waited longer than the answer took"
		);
		assert_eq!(candidates.next().await, Some(addr("1.2.3.4:443")));
	}

	/// The same wait, but the full answer never lands: the IPv4-only one takes
	/// over once the Resolution Delay is up rather than waiting out the resolver.
	#[tokio::test(start_paused = true)]
	async fn ipv4_proceeds_once_the_resolution_delay_expires() {
		let mut candidates = Candidates::slow(
			(&[], Duration::from_secs(30)),
			(&addrs(&["1.2.3.4:443"]), Duration::ZERO),
		);
		candidates.delay = DEFAULT_RESOLUTION_DELAY;

		let start = tokio::time::Instant::now();
		assert_eq!(candidates.next().await, Some(addr("1.2.3.4:443")));
		assert_eq!(start.elapsed(), DEFAULT_RESOLUTION_DELAY);
	}

	/// A failed full lookup is an answer: there is nothing to hold the IPv4
	/// address back for, so it goes immediately.
	#[tokio::test(start_paused = true)]
	async fn ipv4_does_not_wait_for_a_failed_full_lookup() {
		let mut candidates = Candidates::slow((&[], Duration::ZERO), (&addrs(&["1.2.3.4:443"]), Duration::ZERO));
		candidates.delay = DEFAULT_RESOLUTION_DELAY;
		candidates.full.call = Some(tokio::spawn(async { Err(io::Error::other("nope")) }));

		let start = tokio::time::Instant::now();
		assert_eq!(candidates.next().await, Some(addr("1.2.3.4:443")));
		assert_eq!(start.elapsed(), Duration::ZERO);
	}

	/// Only the first candidate waits: once a dial is under way the families
	/// alternate on whatever has landed.
	#[tokio::test(start_paused = true)]
	async fn only_the_first_candidate_waits() {
		let mut candidates = Candidates::slow(
			(&[], Duration::from_secs(30)),
			(&addrs(&["1.2.3.4:443", "5.6.7.8:443"]), Duration::ZERO),
		);
		candidates.delay = DEFAULT_RESOLUTION_DELAY;

		assert_eq!(candidates.next().await, Some(addr("1.2.3.4:443")));

		let start = tokio::time::Instant::now();
		assert_eq!(candidates.next().await, Some(addr("5.6.7.8:443")));
		assert_eq!(start.elapsed(), Duration::ZERO);
	}

	/// A full answer arriving late still contributes what the IPv4-only one
	/// couldn't, without repeating what it already handed out.
	///
	/// The IPv4 addresses go out before it lands rather than waiting for it: the
	/// caller only asks for a candidate when it wants to dial one, so holding one
	/// back to keep the families alternating would idle a dial against a resolver
	/// that may never answer.
	#[tokio::test(start_paused = true)]
	async fn the_full_answer_supersedes_the_ipv4_one() {
		let mut candidates = Candidates::slow(
			(
				&addrs(&["1.2.3.4:443", "[2001:db8::1]:443", "5.6.7.8:443"]),
				Duration::from_secs(1),
			),
			(&addrs(&["1.2.3.4:443", "5.6.7.8:443"]), Duration::ZERO),
		);

		assert_eq!(candidates.next().await, Some(addr("1.2.3.4:443")));
		assert_eq!(candidates.next().await, Some(addr("5.6.7.8:443")));

		let start = tokio::time::Instant::now();
		assert_eq!(candidates.next().await, Some(addr("[2001:db8::1]:443")));
		assert_eq!(start.elapsed(), Duration::from_secs(1), "waited on the full answer");
		assert_eq!(candidates.next().await, None, "an address was dialed twice");
	}

	/// The full answer supersedes fast-lane addresses that are still queued, not
	/// just the ones after them.
	///
	/// A queued IPv4 address must not jump ahead of the one the platform put
	/// first once that answer has landed: with a broken IPv4 path, every one of
	/// them would burn a stagger before the IPv6 address sitting right there got
	/// its turn.
	#[tokio::test(start_paused = true)]
	async fn a_queued_address_yields_to_the_full_answer() {
		let mut candidates = Candidates::slow(
			(
				&addrs(&["[2001:db8::1]:443", "1.2.3.4:443", "5.6.7.8:443"]),
				Duration::from_millis(10),
			),
			(&addrs(&["1.2.3.4:443", "5.6.7.8:443"]), Duration::ZERO),
		);

		// The fast lane is all there is to go on at first.
		assert_eq!(candidates.next().await, Some(addr("1.2.3.4:443")));

		// That dial is in flight when the full answer lands, so the next candidate
		// comes from the platform's order rather than the fast lane's leftovers.
		tokio::time::sleep(Duration::from_millis(20)).await;
		assert_eq!(candidates.next().await, Some(addr("[2001:db8::1]:443")));
		assert_eq!(candidates.next().await, Some(addr("5.6.7.8:443")));
		assert_eq!(candidates.next().await, None);
	}

	/// A dial that resolves nothing reports why, and reports it once.
	#[tokio::test]
	async fn failure_reports_the_lookup_error() {
		let mut candidates = Candidates::default();
		candidates.full.error = Some(io::Error::other("no such host"));
		candidates.ipv4.error = Some(io::Error::other("no A record"));

		// The full lookup speaks for the host; the IPv4-only one only stands in when
		// it had nothing to say.
		let err = candidates.failure().expect("no failure reported");
		assert_eq!(err.to_string(), "no such host");
		assert_eq!(
			candidates.failure().map(|err| err.to_string()).as_deref(),
			Some("no A record")
		);
		assert!(candidates.failure().is_none());
	}

	/// A link-local answer is only routable with the scope id the resolver
	/// attached to it, so stamping our port on must not rebuild the address.
	#[test]
	fn the_port_is_stamped_without_losing_the_scope() {
		use std::net::{Ipv6Addr, SocketAddrV6};

		let answer = SocketAddrV6::new("fe80::1".parse::<Ipv6Addr>().unwrap(), 0, 7, 3);
		let dialed = with_port(SocketAddr::V6(answer), 443);

		let SocketAddr::V6(dialed) = dialed else {
			panic!("family changed: {dialed}");
		};
		assert_eq!(dialed.port(), 443);
		assert_eq!(dialed.scope_id(), 3, "dropped the interface scope");
		assert_eq!(dialed.flowinfo(), 7);
	}

	/// The end-to-end path through the real resolver: localhost is in
	/// `/etc/hosts` on every platform, so this exercises `getaddrinfo` without
	/// needing a network.
	#[tokio::test]
	async fn resolves_localhost() {
		let url = url::Url::parse("https://localhost").unwrap();
		let candidates = Candidates::resolve(url.host().unwrap(), 443, DEFAULT_RESOLUTION_DELAY);
		let addrs = drain(candidates).await;

		assert!(!addrs.is_empty(), "localhost resolved to nothing");
		assert!(addrs.iter().all(|addr| addr.ip().is_loopback() && addr.port() == 443));
	}

	/// A host the resolver rejects reports its error rather than an empty answer.
	///
	/// The name carries a NUL so both lookups fail without touching the network:
	/// an unresolvable name would do as well, but only on a resolver that answers
	/// NXDOMAIN rather than hijacking it.
	#[tokio::test]
	async fn a_rejected_host_reports_a_failure() {
		let mut candidates = Candidates {
			full: Query::start("no\0such\0host", 443, Lookup::Full),
			ipv4: Query::start("no\0such\0host", 443, Lookup::Ipv4),
			..Default::default()
		};

		assert_eq!(candidates.next().await, None);
		assert!(candidates.failure().is_some());
	}
}
