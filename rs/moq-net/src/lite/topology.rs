//! The Topology stream (lite-07+): cluster sessions flood per-link liveness.
//!
//! The dialing side opens the stream. Each side sends one TOPOLOGY_DIGEST of
//! what it holds, then TOPOLOGY_REPORT batches: first everything the peer's
//! digest lacks, then every change it learns, held for [`topology::HOLD_DOWN`].

use std::{
	collections::BTreeMap,
	task::{Poll, ready},
};

use crate::{
	Error,
	coding::{Decode, DecodeError, Encode, EncodeError, Stream},
	runtime::{Deadline, Timers as _},
	time::Clock,
	topology::{self, Entry, Reported, Reports},
};

use super::{ControlType, Message, PeerSetup, Version};

/// The first message on each side of a Topology stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TopologyDigest(pub topology::Digest);

/// A batch of link reports, every later message on each side of a Topology stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TopologyReport(pub Reports);

/// A relay id off the wire: non-zero, since 0 identifies nobody.
fn decode_node<R: bytes::Buf>(r: &mut R, version: Version) -> Result<u64, DecodeError> {
	let id = u64::decode(r, version)?;
	crate::Hop::new(id).map_err(|_| DecodeError::InvalidValue)?;
	Ok(id)
}

/// Decode `Count` reporters, each with `Count` links, rejecting any repeat.
fn decode_reporters<R, T>(
	r: &mut R,
	version: Version,
	mut link: impl FnMut(&mut R) -> Result<T, DecodeError>,
) -> Result<BTreeMap<u64, Reported<T>>, DecodeError>
where
	R: bytes::Buf,
{
	let mut reporters = BTreeMap::new();
	for _ in 0..u64::decode(r, version)? {
		let node = decode_node(r, version)?;
		let incarnation = u64::decode(r, version)?;
		let mut links = BTreeMap::new();
		for _ in 0..u64::decode(r, version)? {
			let peer = decode_node(r, version)?;
			if peer == node {
				return Err(DecodeError::InvalidValue);
			}
			if links.insert(peer, link(r)?).is_some() {
				return Err(DecodeError::Duplicate);
			}
		}
		if reporters.insert(node, Reported { incarnation, links }).is_some() {
			return Err(DecodeError::Duplicate);
		}
	}
	Ok(reporters)
}

fn encode_reporters<W, T>(
	w: &mut W,
	version: Version,
	reporters: &BTreeMap<u64, Reported<T>>,
	mut link: impl FnMut(&mut W, &T) -> Result<(), EncodeError>,
) -> Result<(), EncodeError>
where
	W: bytes::BufMut,
{
	(reporters.len() as u64).encode(w, version)?;
	for (node, reported) in reporters {
		node.encode(w, version)?;
		reported.incarnation.encode(w, version)?;
		(reported.links.len() as u64).encode(w, version)?;
		for (peer, value) in &reported.links {
			peer.encode(w, version)?;
			link(w, value)?;
		}
	}
	Ok(())
}

impl Message for TopologyDigest {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		if !version.has_topology() {
			return Err(DecodeError::Version);
		}
		let node = decode_node(r, version)?;
		let reporters = decode_reporters(r, version, |r| u64::decode(r, version))?;
		Ok(Self(topology::Digest { node, reporters }))
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		if !version.has_topology() {
			return Err(EncodeError::Version);
		}
		self.0.node.encode(w, version)?;
		encode_reporters(w, version, &self.0.reporters, |w, seq| seq.encode(w, version))
	}
}

impl Message for TopologyReport {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		if !version.has_topology() {
			return Err(DecodeError::Version);
		}
		let reporters = decode_reporters(r, version, |r| {
			let seq = u64::decode(r, version)?;
			let up = u64::decode(r, version)?;
			let cost = u64::decode(r, version)?;
			let cost = match up {
				0 => None,
				1 => Some(cost),
				_ => return Err(DecodeError::InvalidValue),
			};
			Ok(Entry { seq, cost })
		})?;
		Ok(Self(reporters))
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		if !version.has_topology() {
			return Err(EncodeError::Version);
		}
		encode_reporters(w, version, &self.0, |w, entry| {
			entry.seq.encode(w, version)?;
			u64::from(entry.cost.is_some()).encode(w, version)?;
			entry.cost.unwrap_or(0).encode(w, version)
		})
	}
}

/// One session's side of the Topology stream.
///
/// Resolves `Ok` once the link is gone without fault (the peer finished or
/// reset the stream, or never took part), and `Err` only for a protocol
/// violation, which closes the session.
pub(crate) struct Topology<S: crate::transport::poll::Session> {
	database: topology::Database,
	version: Version,
	runtime: Clock,
	peer_setup: PeerSetup,
	/// What we charge for the link, when we priced it ourselves.
	cost: Option<u64>,
	state: State<S>,
}

enum State<S: crate::transport::poll::Session> {
	/// The dialing side, opening the stream.
	Open {
		session: S,
	},
	/// Waiting for the peer's SETUP, which names it and prices the link.
	Setup {
		stream: Stream<S, Version>,
	},
	/// Our digest is buffered; reading the peer's.
	Exchange {
		stream: Stream<S, Version>,
		peer: u64,
		cost: u64,
	},
	/// Linked: sending held reports and applying the peer's.
	Linked {
		stream: Stream<S, Version>,
		neighbor: topology::Neighbor,
		flush: Deadline<Clock>,
	},
	Done,
}

impl<S: crate::transport::poll::Session> Topology<S> {
	/// The dialing side, which opens the stream.
	pub fn open(
		database: topology::Database,
		session: S,
		version: Version,
		runtime: Clock,
		peer_setup: PeerSetup,
		cost: Option<u64>,
	) -> Self {
		Self {
			database,
			version,
			runtime,
			peer_setup,
			cost,
			state: State::Open { session },
		}
	}

	/// The accepting side, handed the stream once its type was read.
	pub fn accept(
		database: topology::Database,
		stream: Stream<S, Version>,
		version: Version,
		runtime: Clock,
		peer_setup: PeerSetup,
		cost: Option<u64>,
	) -> Self {
		Self {
			database,
			version,
			runtime,
			peer_setup,
			cost,
			state: State::Setup { stream },
		}
	}

	pub fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		let res = ready!(self.poll_run(waiter));
		let linked = matches!(self.state, State::Linked { .. });
		// Dropping the state drops the neighbor, which reports the link down.
		self.state = State::Done;
		match res {
			Ok(()) => {
				tracing::debug!("topology stream finished");
				Poll::Ready(Ok(()))
			}
			// A malformed message is the peer breaking the protocol, not the link failing.
			Err(err @ (Error::Decode(_) | Error::ProtocolViolation)) => Poll::Ready(Err(err)),
			Err(err) if linked => {
				tracing::info!(%err, "topology link lost");
				Poll::Ready(Ok(()))
			}
			// The peer never took part: this session is not a cluster link, so the
			// session carries on without one.
			Err(err) => {
				tracing::warn!(%err, "peer refused the topology stream; not a cluster link");
				Poll::Ready(Ok(()))
			}
		}
	}

	fn poll_run(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		let mut cx = waiter.context();
		loop {
			match &mut self.state {
				State::Open { session } => {
					let mut stream = ready!(Stream::poll_open(session, self.version, &mut cx))?;
					stream.writer.buffer(&ControlType::Topology)?;
					self.state = State::Setup { stream };
				}
				State::Setup { stream } => {
					let hop = ready!(self.peer_setup.poll_hop(waiter));
					let declared = ready!(self.peer_setup.poll_cost(waiter));
					let node = self.database.node();
					let Some(peer) = hop.filter(|peer| *peer != node) else {
						tracing::warn!(?hop, "cluster peer declared no Hop ID of its own");
						return Poll::Ready(Err(Error::ProtocolViolation));
					};
					// The same rule the subscriber prices routes by: our own price
					// when we set one, else what the dialer declared, else 1.
					let cost = self.cost.or(declared).unwrap_or(1);
					stream.writer.buffer(&TopologyDigest(self.database.digest()))?;
					let State::Setup { stream } = std::mem::replace(&mut self.state, State::Done) else {
						unreachable!()
					};
					self.state = State::Exchange {
						stream,
						peer: peer.id(),
						cost,
					};
				}
				State::Exchange { stream, peer, cost } => {
					if let Poll::Ready(res) = stream.writer.poll_flush(&mut cx) {
						res?;
					}
					let TopologyDigest(digest) = ready!(stream.reader.poll_decode(&mut cx))?;
					if digest.node != *peer {
						tracing::warn!(setup = *peer, digest = digest.node, "cluster peer named itself twice");
						return Poll::Ready(Err(Error::ProtocolViolation));
					}
					let neighbor = self.database.attach(*peer, *cost, &digest);
					tracing::debug!(peer = *peer, cost = *cost, "topology link up");
					let State::Exchange { stream, .. } = std::mem::replace(&mut self.state, State::Done) else {
						unreachable!()
					};
					// What the peer's digest lacks goes out at once; later changes wait
					// out the hold-down.
					self.state = State::Linked {
						stream,
						neighbor,
						flush: Deadline::at(&self.runtime, self.runtime.now()),
					};
				}
				State::Linked {
					stream,
					neighbor,
					flush,
				} => {
					loop {
						match stream.reader.poll_decode_maybe::<TopologyReport>(&mut cx)? {
							Poll::Ready(Some(TopologyReport(reports))) => neighbor.apply(reports),
							Poll::Ready(None) => return Poll::Ready(Ok(())),
							Poll::Pending => break,
						}
					}

					if flush.deadline().is_none() && neighbor.poll_pending(waiter).is_ready() {
						flush.set(Some(self.runtime.now() + topology::HOLD_DOWN));
					}
					if flush.poll(waiter).is_ready() {
						flush.set(None);
						let reports = neighbor.take();
						if !reports.is_empty() {
							stream.writer.buffer(&TopologyReport(reports))?;
						}
					}

					if let Poll::Ready(res) = stream.writer.poll_flush(&mut cx) {
						res?;
					}
					// Taking the reports woke our own parked poll, so the next wake
					// re-arms the hold-down if more arrived meanwhile.
					if flush.deadline().is_none() && neighbor.poll_pending(waiter).is_ready() {
						continue;
					}
					return Poll::Pending;
				}
				State::Done => return Poll::Ready(Ok(())),
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn round_trip<T: Message + PartialEq + std::fmt::Debug>(msg: &T) -> T {
		let mut buf = bytes::BytesMut::new();
		msg.encode(&mut buf, Version::Lite07).unwrap();
		let mut slice = &buf[..];
		let got = T::decode(&mut slice, Version::Lite07).unwrap();
		assert_eq!(bytes::Buf::remaining(&slice), 0, "trailing bytes after decode");
		got
	}

	#[test]
	fn digest_round_trips() {
		let digest = TopologyDigest(topology::Digest {
			node: 7,
			reporters: BTreeMap::from([(
				9,
				Reported {
					incarnation: 1_700_000_000_000,
					links: BTreeMap::from([(7, 3), (11, 1)]),
				},
			)]),
		});
		assert_eq!(round_trip(&digest), digest);
	}

	#[test]
	fn report_round_trips_up_and_down() {
		let report = TopologyReport(BTreeMap::from([(
			9,
			Reported {
				incarnation: 4,
				links: BTreeMap::from([(7, Entry { seq: 2, cost: Some(0) }), (11, Entry { seq: 5, cost: None })]),
			},
		)]));
		assert_eq!(round_trip(&report), report);
	}

	#[test]
	fn older_versions_carry_no_topology() {
		let mut buf = bytes::BytesMut::new();
		let err = TopologyReport(Reports::new()).encode(&mut buf, Version::Lite06);
		assert!(matches!(err, Err(EncodeError::Version)));
	}

	/// Hand-encode a report body so the decoder's refusals are tested on the wire
	/// shape, not on what our encoder would never produce.
	fn decode_report(fields: &[u64]) -> Result<TopologyReport, DecodeError> {
		let mut body = bytes::BytesMut::new();
		for field in fields {
			field.encode(&mut body, Version::Lite07).unwrap();
		}
		let mut buf = bytes::BytesMut::new();
		(body.len() as u64).encode(&mut buf, Version::Lite07).unwrap();
		buf.extend_from_slice(&body);
		TopologyReport::decode(&mut &buf[..], Version::Lite07)
	}

	#[test]
	fn malformed_reports_are_refused() {
		// One reporter (9) with one link (7): seq 1, up, cost 1.
		assert!(decode_report(&[1, 9, 0, 1, 7, 1, 1, 1]).is_ok());
		// Node 0 identifies nobody.
		assert!(matches!(
			decode_report(&[1, 0, 0, 1, 7, 1, 1, 1]),
			Err(DecodeError::InvalidValue)
		));
		// A relay's link to itself.
		assert!(matches!(
			decode_report(&[1, 9, 0, 1, 9, 1, 1, 1]),
			Err(DecodeError::InvalidValue)
		));
		// Up is a flag.
		assert!(matches!(
			decode_report(&[1, 9, 0, 1, 7, 1, 2, 1]),
			Err(DecodeError::InvalidValue)
		));
		// The same link twice.
		assert!(matches!(
			decode_report(&[1, 9, 0, 2, 7, 1, 1, 1, 7, 2, 1, 1]),
			Err(DecodeError::Duplicate)
		));
		// The same reporter twice.
		assert!(matches!(
			decode_report(&[2, 9, 0, 0, 9, 0, 0]),
			Err(DecodeError::Duplicate)
		));
	}
}
