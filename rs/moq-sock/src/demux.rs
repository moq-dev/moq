//! Splitting one UDP socket between QUIC and the WebRTC media path.
//!
//! [`classify`] reads a datagram's first byte by RFC 7983's partition, which QUIC
//! fills as long as its fixed bit is set: every endpoint served through this must
//! turn QUIC-bit greasing (RFC 9287) off. [`Flows`] pins a remote address to a
//! stack regardless of what it sends, and [`Queue`] buffers one stack's datagrams
//! with a per-source share. None of it touches a runtime, so the tokio and
//! io_uring listeners run the same rules.

use std::{
	collections::{HashMap, HashSet, VecDeque},
	net::SocketAddr,
	sync::{
		Arc, Mutex,
		atomic::{AtomicBool, AtomicU64, Ordering},
	},
};

use arc_swap::ArcSwap;

/// What a datagram carries, judged by its first byte and, for STUN, its attributes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
	/// A QUIC packet: the fixed bit is set, in a long (192 to 255) or short (64 to 127) header.
	Quic,
	/// ICE: a STUN Binding request carrying USERNAME (a connectivity check), or a
	/// Binding response, which answers a request only the WebRTC agent sends from
	/// this port, since a full ICE agent needs its own checks answered.
	Ice,
	/// Any other STUN message, such as a public Binding query or an indication.
	Stun,
	/// A DTLS record (20 to 63).
	Dtls,
	/// An RTP or RTCP packet (128 to 191).
	Rtp,
	/// Nothing this socket serves, including an empty datagram.
	Unknown,
}

const STUN_HEADER: usize = 20;
const STUN_COOKIE: [u8; 4] = [0x21, 0x12, 0xA4, 0x42];
const STUN_BINDING_REQUEST: [u8; 2] = [0x00, 0x01];
const STUN_BINDING_SUCCESS: [u8; 2] = [0x01, 0x01];
const STUN_BINDING_ERROR: [u8; 2] = [0x01, 0x11];
const STUN_USERNAME: u16 = 0x0006;

/// Classify a datagram by RFC 7983's first-byte ranges.
pub fn classify(datagram: &[u8]) -> Class {
	match datagram.first() {
		Some(0..=3) if is_ice(datagram) => Class::Ice,
		Some(0..=3) => Class::Stun,
		Some(20..=63) => Class::Dtls,
		Some(64..=127 | 192..=255) => Class::Quic,
		Some(128..=191) => Class::Rtp,
		_ => Class::Unknown,
	}
}

/// Whether `datagram` is a well-formed STUN Binding response, or Binding request
/// with a USERNAME attribute.
fn is_ice(datagram: &[u8]) -> bool {
	let Some((header, mut attributes)) = datagram.split_at_checked(STUN_HEADER) else {
		return false;
	};
	if header[4..8] != STUN_COOKIE || usize::from(u16::from_be_bytes([header[2], header[3]])) != attributes.len() {
		return false;
	}
	match [header[0], header[1]] {
		STUN_BINDING_SUCCESS | STUN_BINDING_ERROR => return true,
		STUN_BINDING_REQUEST => {}
		_ => return false,
	}

	while let Some((attribute, rest)) = attributes.split_at_checked(4) {
		let kind = u16::from_be_bytes([attribute[0], attribute[1]]);
		let len = usize::from(u16::from_be_bytes([attribute[2], attribute[3]]));
		if kind == STUN_USERNAME {
			return len <= rest.len();
		}
		// Values are padded to a multiple of four.
		let Some(rest) = rest.get(len.next_multiple_of(4)..) else {
			return false;
		};
		attributes = rest;
	}
	false
}

/// Where a datagram goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
	/// The QUIC endpoint.
	Quic,
	/// The WebRTC media path: ICE, DTLS, and RTP.
	WebRtc,
	/// Nowhere; counted under this reason.
	Discard(Discard),
}

/// Why a datagram was dropped instead of delivered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Discard {
	/// STUN that is not ICE, such as a public Binding query; the public
	/// responder binds its own port.
	Stun,
	/// Nothing this socket serves.
	Unknown,
	/// WebRTC traffic while nothing consumes it.
	Unclaimed,
	/// WebRTC traffic that found its queue full, or its source over its share.
	Overflow,
}

impl Discard {
	/// Every reason, in [`Counters`] order.
	pub const ALL: [Self; 4] = [Self::Stun, Self::Unknown, Self::Unclaimed, Self::Overflow];

	/// A stable lowercase label, for metrics.
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Stun => "stun",
			Self::Unknown => "unknown",
			Self::Unclaimed => "unclaimed",
			Self::Overflow => "overflow",
		}
	}
}

/// Datagrams dropped per [`Discard`] reason. Clones share the counts.
#[derive(Clone, Debug, Default)]
pub struct Counters(Arc<[AtomicU64; Discard::ALL.len()]>);

impl Counters {
	/// Count one datagram dropped for `reason`.
	pub fn discard(&self, reason: Discard) {
		self.0[reason as usize].fetch_add(1, Ordering::Relaxed);
	}

	/// How many datagrams were dropped for `reason`.
	pub fn get(&self, reason: Discard) -> u64 {
		self.0[reason as usize].load(Ordering::Relaxed)
	}
}

/// Remote addresses pinned to WebRTC, whatever they send. Clones share the table.
///
/// Keyed by the remote address alone: the local port is the socket's, and a
/// remote address reaching two of a wildcard socket's local addresses is
/// treated as one flow. IPv4-mapped addresses are folded to IPv4, as
/// `moq_ice::canonical` does, so a dual-stack socket's pins match.
#[derive(Clone, Debug, Default)]
pub struct Flows(Arc<FlowsState>);

#[derive(Debug, Default)]
struct FlowsState {
	/// Whether anything is pinned, so an unpinned listener skips the set entirely.
	any: AtomicBool,
	/// The pinned addresses, read without a lock on every datagram by every shard
	/// and replaced whole on the rare pin or release.
	pinned: ArcSwap<HashSet<SocketAddr>>,
	/// How many [`Pin`]s hold each address, which only pinning and releasing touch.
	pins: Mutex<HashMap<SocketAddr, usize>>,
}

impl FlowsState {
	fn publish(&self, pins: &HashMap<SocketAddr, usize>) {
		self.pinned.store(Arc::new(pins.keys().copied().collect()));
		self.any.store(!pins.is_empty(), Ordering::Release);
	}
}

impl Flows {
	/// Route `datagram` from `remote`: a pin wins, otherwise its [`Class`] decides.
	pub fn route(&self, remote: SocketAddr, datagram: &[u8]) -> Route {
		if self.is_pinned(remote) {
			return Route::WebRtc;
		}
		match classify(datagram) {
			Class::Quic => Route::Quic,
			Class::Ice | Class::Dtls | Class::Rtp => Route::WebRtc,
			Class::Stun => Route::Discard(Discard::Stun),
			Class::Unknown => Route::Discard(Discard::Unknown),
		}
	}

	/// Pin `remote` to WebRTC until the returned [`Pin`] drops.
	pub fn pin(&self, remote: SocketAddr) -> Pin {
		let remote = canonical(remote);
		let mut pins = self.0.pins.lock().unwrap();
		let count = pins.entry(remote).or_default();
		*count += 1;
		if *count == 1 {
			self.0.publish(&pins);
		}
		Pin {
			flows: self.clone(),
			remote,
		}
	}

	fn is_pinned(&self, remote: SocketAddr) -> bool {
		self.0.any.load(Ordering::Acquire) && self.0.pinned.load().contains(&canonical(remote))
	}
}

fn canonical(addr: SocketAddr) -> SocketAddr {
	SocketAddr::new(addr.ip().to_canonical(), addr.port())
}

/// Keeps a remote address pinned to WebRTC in its [`Flows`]; dropping it releases the pin.
#[derive(Debug)]
pub struct Pin {
	flows: Flows,
	remote: SocketAddr,
}

impl Drop for Pin {
	fn drop(&mut self) {
		let mut pins = self.flows.0.pins.lock().unwrap();
		if let Some(count) = pins.get_mut(&self.remote) {
			*count -= 1;
			if *count == 0 {
				pins.remove(&self.remote);
				self.flows.0.publish(&pins);
			}
		}
	}
}

/// A FIFO of one stack's datagrams, bounded by cost with a per-source share.
///
/// A source may hold at most [`Queue::SHARE`] of the capacity, so one flooding
/// sender is refused while the rest still get in; filling the queue takes that
/// many sources at once.
#[derive(Debug)]
pub struct Queue<T> {
	items: VecDeque<(SocketAddr, usize, T)>,
	/// The cost each source holds in `items`; a source holding nothing has no entry.
	sources: HashMap<SocketAddr, usize>,
	cost: usize,
	capacity: usize,
}

impl<T> Queue<T> {
	/// The fraction of the capacity, as a divisor, one source may hold.
	pub const SHARE: usize = 8;

	/// An empty queue holding up to `capacity` in total cost.
	pub fn new(capacity: usize) -> Self {
		Self {
			items: VecDeque::new(),
			sources: HashMap::new(),
			cost: 0,
			capacity,
		}
	}

	/// Append `item` from `source`, charged `cost`, or hand it back when the queue
	/// is full or `source` already holds its share.
	pub fn push(&mut self, source: SocketAddr, cost: usize, item: T) -> Result<(), T> {
		let held = self.sources.get(&source).copied().unwrap_or(0);
		if self.cost + cost > self.capacity || held + cost > self.capacity / Self::SHARE {
			return Err(item);
		}
		self.cost += cost;
		*self.sources.entry(source).or_default() += cost;
		self.items.push_back((source, cost, item));
		Ok(())
	}

	/// Remove the oldest item.
	pub fn pop(&mut self) -> Option<T> {
		let (source, cost, item) = self.items.pop_front()?;
		self.cost -= cost;
		if let Some(held) = self.sources.get_mut(&source) {
			*held -= cost;
			if *held == 0 {
				self.sources.remove(&source);
			}
		}
		Some(item)
	}

	/// Drop every item.
	pub fn clear(&mut self) {
		self.items.clear();
		self.sources.clear();
		self.cost = 0;
	}

	/// Whether nothing is queued.
	pub fn is_empty(&self) -> bool {
		self.items.is_empty()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A Binding request: header, then the given attributes.
	fn binding(attributes: &[(u16, &[u8])]) -> Vec<u8> {
		stun(STUN_BINDING_REQUEST, attributes)
	}

	/// A STUN message of `kind`: header, then the given attributes.
	fn stun(kind: [u8; 2], attributes: &[(u16, &[u8])]) -> Vec<u8> {
		let mut body = Vec::new();
		for (kind, value) in attributes {
			body.extend_from_slice(&kind.to_be_bytes());
			body.extend_from_slice(&(value.len() as u16).to_be_bytes());
			body.extend_from_slice(value);
			body.resize(body.len().next_multiple_of(4), 0);
		}
		let mut out = Vec::new();
		out.extend_from_slice(&kind);
		out.extend_from_slice(&(body.len() as u16).to_be_bytes());
		out.extend_from_slice(&STUN_COOKIE);
		out.extend_from_slice(&[7; 12]);
		out.extend_from_slice(&body);
		out
	}

	const PRIORITY: u16 = 0x0024;
	const XOR_MAPPED_ADDRESS: u16 = 0x0020;

	fn addr(port: u16) -> SocketAddr {
		SocketAddr::from(([192, 0, 2, 1], port))
	}

	#[test]
	fn first_byte_classes() {
		let check = binding(&[(PRIORITY, &[0, 0, 0, 1]), (STUN_USERNAME, b"abc:def")]);
		let cases: &[(&[u8], Class)] = &[
			(&check, Class::Ice),
			(
				&stun(STUN_BINDING_SUCCESS, &[(XOR_MAPPED_ADDRESS, &[0; 8])]),
				Class::Ice,
			),
			(&stun(STUN_BINDING_ERROR, &[]), Class::Ice),
			(&binding(&[(PRIORITY, &[0, 0, 0, 1])]), Class::Stun),
			// A Binding indication, which no ICE agent here waits on.
			(&stun([0x00, 0x11], &[]), Class::Stun),
			(&[0x01, 0x01, 0, 0], Class::Stun),
			(&[3], Class::Stun),
			(&[4], Class::Unknown),
			(&[19], Class::Unknown),
			(&[20], Class::Dtls),
			(&[0x16, 0xfe, 0xfd], Class::Dtls),
			(&[63], Class::Dtls),
			(&[64], Class::Quic),
			(&[127], Class::Quic),
			(&[128], Class::Rtp),
			(&[0x80, 0x00], Class::Rtp),
			(&[191], Class::Rtp),
			(&[192], Class::Quic),
			(&[255], Class::Quic),
			(&[], Class::Unknown),
		];
		for (datagram, class) in cases {
			assert_eq!(classify(datagram), *class, "{datagram:02x?}");
		}
	}

	#[test]
	fn malformed_checks_are_plain_stun() {
		let check = binding(&[(STUN_USERNAME, b"abc:def")]);

		let mut cookie = check.clone();
		cookie[4] ^= 1;
		assert_eq!(classify(&cookie), Class::Stun);

		// A response is held to the same framing as a check.
		let mut response = stun(STUN_BINDING_SUCCESS, &[(XOR_MAPPED_ADDRESS, &[0; 8])]);
		response.push(0);
		assert_eq!(classify(&response), Class::Stun);

		// The header's length must cover the attributes exactly.
		let mut long = check.clone();
		long.push(0);
		assert_eq!(classify(&long), Class::Stun);
		assert_eq!(classify(&check[..check.len() - 4]), Class::Stun);

		// An attribute running past the message hides any USERNAME after it.
		let mut truncated = binding(&[(PRIORITY, &[0, 0, 0, 1]), (STUN_USERNAME, b"abc:def")]);
		truncated[STUN_HEADER + 3] = 200;
		assert_eq!(classify(&truncated), Class::Stun);
	}

	#[test]
	fn routes_follow_the_class() {
		let flows = Flows::default();
		let check = binding(&[(STUN_USERNAME, b"abc:def")]);
		assert_eq!(flows.route(addr(1), &check), Route::WebRtc);
		assert_eq!(flows.route(addr(1), &stun(STUN_BINDING_SUCCESS, &[])), Route::WebRtc);
		assert_eq!(flows.route(addr(1), &[0x16]), Route::WebRtc);
		assert_eq!(flows.route(addr(1), &[0x80]), Route::WebRtc);
		assert_eq!(flows.route(addr(1), &[0x40]), Route::Quic);
		assert_eq!(flows.route(addr(1), &[0xc0]), Route::Quic);
		assert_eq!(flows.route(addr(1), &binding(&[])), Route::Discard(Discard::Stun));
		assert_eq!(flows.route(addr(1), &[5]), Route::Discard(Discard::Unknown));
	}

	#[test]
	fn a_pin_wins_until_every_holder_releases_it() {
		let flows = Flows::default();
		let mapped = SocketAddr::from((std::net::Ipv4Addr::new(192, 0, 2, 1).to_ipv6_mapped(), 1));

		let first = flows.pin(addr(1));
		let second = flows.pin(mapped);
		assert_eq!(flows.route(addr(1), &[0x40]), Route::WebRtc);
		assert_eq!(flows.route(mapped, &[5]), Route::WebRtc);
		assert_eq!(flows.route(addr(2), &[0x40]), Route::Quic);

		drop(first);
		assert_eq!(flows.route(addr(1), &[0x40]), Route::WebRtc);
		drop(second);
		assert_eq!(flows.route(addr(1), &[0x40]), Route::Quic);
	}

	#[test]
	fn counters_count_each_reason() {
		let counters = Counters::default();
		counters.clone().discard(Discard::Overflow);
		counters.discard(Discard::Overflow);
		counters.discard(Discard::Stun);
		assert_eq!(counters.get(Discard::Overflow), 2);
		assert_eq!(counters.get(Discard::Stun), 1);
		assert_eq!(counters.get(Discard::Unknown), 0);
	}

	#[test]
	fn a_flooding_source_is_held_to_its_share() {
		let mut queue = Queue::new(16);
		queue.push(addr(1), 1, 0).unwrap();
		queue.push(addr(1), 1, 1).unwrap();
		assert_eq!(queue.push(addr(1), 1, 2), Err(2));

		// Its share is spent, not the queue: everyone else still gets in.
		for port in 2..9 {
			queue.push(addr(port), 2, 10 + port).unwrap();
		}
		// Now the queue is full, whatever the source.
		assert_eq!(queue.push(addr(9), 1, 19), Err(19));

		// Popping refunds the oldest item's source.
		assert_eq!(queue.pop(), Some(0));
		queue.push(addr(1), 1, 2).unwrap();
		let drained: Vec<_> = std::iter::from_fn(|| queue.pop()).collect();
		assert_eq!(drained, [1, 12, 13, 14, 15, 16, 17, 18, 2]);
		assert!(queue.is_empty());
		assert!(queue.sources.is_empty());
	}

	#[test]
	fn clear_forgets_every_source() {
		let mut queue = Queue::new(16);
		queue.push(addr(1), 2, ()).unwrap();
		queue.clear();
		assert!(queue.is_empty());
		queue.push(addr(1), 2, ()).unwrap();
	}
}
