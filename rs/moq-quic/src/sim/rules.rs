//! QUIC's rules an on-path observer can check, judged from the datagrams alone
//!
//! It parses headers itself rather than through the crate's decoder, so a bug in that decoder
//! cannot hide a bug in what it judges. Each rule has a mutation under `test/drill/mutations/`
//! that breaks it and a test the checker then fails.
//!
//! Left out, and why:
//!
//! - Anything inside packet protection (flow control, stream limits, only CONNECTION_CLOSE once
//!   closing, ack-eliciting server Initials padded to 1200 bytes): the observer has no keys, and
//!   the sim's tests already assert flow control and stream limits from the receiving side.
//! - The amplification limit on a migrated path: path validation ends with an encrypted
//!   PATH_RESPONSE, so the observer cannot tell when the limit lifts.
//! - The fixed bit: both sides grease it by default (RFC 9287), and whether the peer allowed
//!   that is inside the encrypted transport parameters.
//! - No datagram past the path MTU: only the harness knows the path's MTU, so the test `Pair`
//!   owns that check (`quest/m4/quic-sim-mtu.md`).
//! - PTO backoff and the idle timeout: neither shows on the wire without the sender's timers.

use std::{collections::HashMap, fmt, net::SocketAddr};

use crate::Instant;

/// The smallest datagram that may carry a client's Initial packet (RFC 9000 §14.1)
const MIN_INITIAL_DATAGRAM: usize = 1200;

/// How many times what it received a server may send to an address it has not validated
/// (RFC 9000 §8.1)
const AMPLIFICATION: u64 = 3;

/// The QUIC version 1 wire number, whose long header packet types the rules know
const V1: u32 = 1;

/// Checks every datagram crossing a simulated network against QUIC's rules an on-path observer
/// can see: address validation's amplification limit, Initial padding, and how packets coalesce
///
/// Feed it each datagram an endpoint sends with [`Self::sent`] and each one an endpoint receives
/// with [`Self::received`], whatever path or impairment carried it.
#[derive(Debug, Default)]
pub struct Rules {
	/// Each pair of addresses that has carried a client's Initial, keyed by client then server
	paths: HashMap<(SocketAddr, SocketAddr), Path>,
}

/// What the observer knows of one client and server pair of addresses
#[derive(Debug, Default)]
struct Path {
	/// Whether the server may treat the client's address as validated, which ends its limit
	validated: bool,
	/// The bytes the server has received from the client
	received: u64,
	/// The bytes the server has sent to the client
	sent: u64,
	/// Datagrams on their way to the server that have not arrived yet: when each arrives, its
	/// size, and whether it validates the address
	arriving: Vec<(Instant, u64, bool)>,
}

impl Path {
	/// Counts every arrival at the server by `now`
	fn arrive(&mut self, now: Instant) {
		self.arriving.retain(|&(at, bytes, validates)| {
			if at > now {
				return true;
			}
			self.received += bytes;
			self.validated |= validates;
			false
		});
	}
}

impl Rules {
	/// An observer that has seen nothing yet
	pub fn new() -> Self {
		Self::default()
	}

	/// Checks `datagram`, which `from` sent to `to` at `now`, against every rule
	///
	/// Call it for each datagram an endpoint sends, before the path loses or copies it.
	pub fn sent(&mut self, now: Instant, from: SocketAddr, to: SocketAddr, datagram: &[u8]) -> Result<(), Violation> {
		let violation = |rule, detail| Err(Violation { from, to, rule, detail });
		let packets = match parse(datagram) {
			Ok(packets) => packets,
			Err(detail) => {
				return violation(
					"RFC 9000 §17.2: every long header parses within its datagram",
					format!("{detail}: {datagram:02x?}"),
				);
			}
		};

		// RFC 9000 §12.2: "Senders MUST NOT coalesce QUIC packets with different connection IDs
		// into a single UDP datagram." A short header's ID runs as long as the long header's
		// it shares, so the bytes it starts with are compared.
		if let Some((first, rest)) = packets.split_first() {
			for packet in rest {
				if !packet.dcid.starts_with(first.dcid) || (packet.long && packet.dcid.len() != first.dcid.len()) {
					return violation(
						"RFC 9000 §12.2: coalesced packets share one connection ID",
						format!("{:x?} coalesced behind {:x?}", packet.dcid, first.dcid),
					);
				}
			}
		}

		let initial = packets.iter().any(|packet| packet.kind == Kind::Initial);
		if initial && !self.paths.contains_key(&(to, from)) {
			// The first Initial between two addresses comes from the client
			self.paths.entry((from, to)).or_default();
		}

		// RFC 9000 §14.1: "A client MUST expand the payload of all UDP datagrams carrying Initial
		// packets to at least the smallest allowed maximum datagram size of 1200 bytes"
		if initial && datagram.len() < MIN_INITIAL_DATAGRAM && self.paths.contains_key(&(from, to)) {
			return violation(
				"RFC 9000 §14.1: a client pads each datagram carrying an Initial to 1200 bytes",
				format!("{} bytes", datagram.len()),
			);
		}

		if let Some(path) = self.paths.get_mut(&(to, from)) {
			path.arrive(now);
			if !path.validated {
				path.sent += datagram.len() as u64;
				// RFC 9000 §8.1: "Prior to validating the client address, servers MUST NOT send
				// more than three times as many bytes as the number of bytes they have received."
				if path.sent > AMPLIFICATION * path.received {
					return violation(
						"RFC 9000 §8.1: a server sends an unvalidated address at most three times what it received",
						format!("{} bytes sent for {} received", path.sent, path.received),
					);
				}
			}
		}
		Ok(())
	}

	/// Records that `datagram`, which `from` sent to `to`, arrives there at `at`
	///
	/// Call it for each copy the path delivers, including ones it corrupted, and for datagrams
	/// injected from `from` that no endpoint sent: the server counts them all.
	pub fn received(&mut self, at: Instant, from: SocketAddr, to: SocketAddr, datagram: &[u8]) {
		let Some(path) = self.paths.get_mut(&(from, to)) else {
			return;
		};
		if path.validated {
			return;
		}
		// RFC 9000 §8.1: a Handshake packet from the client validates its address, as does an
		// Initial carrying a token, if the server accepts it. Assuming it does only loosens the
		// limit, so the check never blames a server for a token it rejected.
		let validates = parse(datagram).is_ok_and(|packets| {
			packets
				.iter()
				.any(|packet| packet.kind == Kind::Handshake || (packet.kind == Kind::Initial && packet.token))
		});
		path.arriving.push((at, datagram.len() as u64, validates));
	}
}

/// A datagram that broke one of QUIC's rules
#[derive(Debug, Clone)]
pub struct Violation {
	/// The sender's address
	pub from: SocketAddr,
	/// The destination's address
	pub to: SocketAddr,
	/// The rule it broke, citing its RFC section
	pub rule: &'static str,
	/// What the datagram did
	pub detail: String,
}

impl fmt::Display for Violation {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(
			f,
			"QUIC rule broken from {} to {}: {}: {}",
			self.from, self.to, self.rule, self.detail
		)
	}
}

impl std::error::Error for Violation {}

/// A long header packet's type, as QUIC version 1 numbers it
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
	Initial,
	ZeroRtt,
	Handshake,
	Retry,
	/// A short header, a Version Negotiation packet, or another version's packet
	Other,
}

/// One packet of a datagram, as far as its header shows without keys
struct Packet<'a> {
	long: bool,
	kind: Kind,
	/// The destination connection ID; for a short header, every byte after the first, since
	/// the ID's length is not on the wire
	dcid: &'a [u8],
	/// Whether an Initial carries a token
	token: bool,
}

/// Splits a datagram into its coalesced packets
fn parse(datagram: &[u8]) -> Result<Vec<Packet<'_>>, &'static str> {
	let mut packets = Vec::new();
	let mut rest = datagram;
	while let Some(&first) = rest.first() {
		if first & 0x80 == 0 {
			// A short header runs to the end of the datagram
			packets.push(Packet {
				long: false,
				kind: Kind::Other,
				dcid: &rest[1..],
				token: false,
			});
			break;
		}

		let mut reader = Reader(&rest[1..]);
		let version = reader.take(4).ok_or("a long header ends in its version")?;
		let version = u32::from_be_bytes(version.try_into().unwrap());
		let dcid_len = reader.take(1).ok_or("a long header ends before its IDs")?[0];
		let dcid = reader
			.take(dcid_len.into())
			.ok_or("a long header ends in its destination ID")?;
		let scid_len = reader.take(1).ok_or("a long header ends before its source ID")?[0];
		reader
			.take(scid_len.into())
			.ok_or("a long header ends in its source ID")?;

		let kind = match version {
			V1 => match (first >> 4) & 0x03 {
				0 => Kind::Initial,
				1 => Kind::ZeroRtt,
				2 => Kind::Handshake,
				_ => Kind::Retry,
			},
			_ => Kind::Other,
		};
		let mut token = false;
		if kind == Kind::Initial {
			let len = reader.varint().ok_or("an Initial ends in its token length")?;
			token = len > 0;
			reader.take(len).ok_or("an Initial's token runs past its datagram")?;
		}
		let len = match kind {
			Kind::Initial | Kind::ZeroRtt | Kind::Handshake => {
				reader.varint().ok_or("a long header ends in its length")?
			}
			// A Retry, a Version Negotiation packet, or an unknown version's runs to the end
			Kind::Retry | Kind::Other => reader.0.len() as u64,
		};
		reader.take(len).ok_or("its Length runs past the datagram")?;

		packets.push(Packet {
			long: true,
			kind,
			dcid,
			token,
		});
		rest = reader.0;
	}
	Ok(packets)
}

/// Reads a header front to back
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
	fn take(&mut self, len: u64) -> Option<&'a [u8]> {
		let len = usize::try_from(len).ok().filter(|&len| len <= self.0.len())?;
		let (taken, rest) = self.0.split_at(len);
		self.0 = rest;
		Some(taken)
	}

	/// A variable-length integer (RFC 9000 §16): its first two bits give its length
	fn varint(&mut self) -> Option<u64> {
		let first = *self.0.first()?;
		let bytes = self.take(1 << (first >> 6))?;
		Some(
			bytes[1..]
				.iter()
				.fold(u64::from(first & 0x3f), |value, &byte| value << 8 | u64::from(byte)),
		)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::Duration;

	const CLIENT: SocketAddr = SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 1);
	const SERVER: SocketAddr = SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 2);

	/// A version 1 long header packet of `kind` (0 Initial, 2 Handshake) to `dcid`, `len` bytes
	/// after its Length field
	fn long(kind: u8, dcid: &[u8], len: usize) -> Vec<u8> {
		let mut packet = vec![0xc0 | kind << 4, 0, 0, 0, 1, dcid.len() as u8];
		packet.extend_from_slice(dcid);
		packet.push(0);
		if kind == 0 {
			packet.push(0);
		}
		packet.extend_from_slice(&(0x4000 | len as u16).to_be_bytes());
		packet.resize(packet.len() + len, 0);
		packet
	}

	/// A client Initial padded to `len` bytes
	fn initial(len: usize) -> Vec<u8> {
		long(0, &[1; 8], len - 18)
	}

	#[test]
	fn server_sends_three_times_what_arrived() {
		let mut rules = Rules::new();
		let now = Instant::ZERO;
		let client = initial(1200);
		rules.sent(now, CLIENT, SERVER, &client).unwrap();
		// Not counted until it arrives
		let arrival = now.saturating_add(Duration::from_millis(10));
		rules.received(arrival, CLIENT, SERVER, &client);
		assert!(rules.sent(now, SERVER, CLIENT, &long(0, &[2; 8], 100)).is_err());

		let mut rules = Rules::new();
		rules.sent(now, CLIENT, SERVER, &client).unwrap();
		rules.received(arrival, CLIENT, SERVER, &client);
		for _ in 0..3 {
			rules.sent(arrival, SERVER, CLIENT, &long(0, &[2; 8], 1182)).unwrap();
		}
		let violation = rules.sent(arrival, SERVER, CLIENT, &long(2, &[2; 8], 1)).unwrap_err();
		assert!(violation.rule.starts_with("RFC 9000 §8.1"), "{violation}");
	}

	#[test]
	fn a_client_handshake_lifts_the_limit() {
		let mut rules = Rules::new();
		let now = Instant::ZERO;
		let client = initial(1200);
		rules.sent(now, CLIENT, SERVER, &client).unwrap();
		rules.received(now, CLIENT, SERVER, &client);
		let handshake = long(2, &[2; 8], 30);
		rules.sent(now, CLIENT, SERVER, &handshake).unwrap();
		rules.received(now, CLIENT, SERVER, &handshake);
		for _ in 0..10 {
			rules.sent(now, SERVER, CLIENT, &long(2, &[1; 8], 1182)).unwrap();
		}
	}

	#[test]
	fn a_client_initial_is_padded() {
		let mut rules = Rules::new();
		let violation = rules.sent(Instant::ZERO, CLIENT, SERVER, &initial(1199)).unwrap_err();
		assert!(violation.rule.starts_with("RFC 9000 §14.1"), "{violation}");
		// The server's Initial, which may be an unpadded ACK, is not held to it
		let mut rules = Rules::new();
		rules.sent(Instant::ZERO, CLIENT, SERVER, &initial(1200)).unwrap();
		rules.received(Instant::ZERO, CLIENT, SERVER, &initial(1200));
		rules
			.sent(Instant::ZERO, SERVER, CLIENT, &long(0, &[1; 8], 40))
			.unwrap();
	}

	#[test]
	fn coalesced_packets_share_a_destination() {
		let mut rules = Rules::new();
		let mut datagram = long(2, &[1; 8], 20);
		// A short header to the same ID, then one to another
		let mut same = datagram.clone();
		same.extend_from_slice(&[0x40, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0]);
		rules.sent(Instant::ZERO, SERVER, CLIENT, &same).unwrap();
		datagram.extend_from_slice(&[0x40, 3, 3, 3, 3, 3, 3, 3, 3, 0, 0]);
		let violation = rules.sent(Instant::ZERO, SERVER, CLIENT, &datagram).unwrap_err();
		assert!(violation.rule.starts_with("RFC 9000 §12.2"), "{violation}");
	}

	#[test]
	fn a_length_past_the_datagram_is_caught() {
		let mut rules = Rules::new();
		let mut datagram = long(2, &[1; 8], 20);
		datagram.pop();
		let violation = rules.sent(Instant::ZERO, SERVER, CLIENT, &datagram).unwrap_err();
		assert!(violation.rule.starts_with("RFC 9000 §17.2"), "{violation}");
	}
}
