use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use crate::canonical;

/// One inbound UDP datagram and the address it came from.
pub type Packet = (Vec<u8>, SocketAddr);

/// Bound on a session's inbound datagram queue, sized like a socket buffer: past
/// it, datagrams are dropped rather than buffered, since ICE and SCTP tolerate loss
/// and a stalled session must not grow memory without limit.
pub const INBOX: usize = 256;

/// The UDP socket an [`crate::Agent`] runs on: where it sends, what it receives,
/// and the host candidates it is reachable at.
pub struct Socket {
	/// Sends datagrams, possibly shared by several agents (a demuxed server socket).
	pub(crate) udp: Arc<UdpSocket>,
	/// The host candidates advertised to the peer, not the socket's bind address.
	/// Must not be empty.
	pub(crate) locals: Vec<SocketAddr>,
	/// The datagrams routed to this agent.
	pub(crate) inbound: Inbound,
}

impl Socket {
	/// Build a socket with at least one advertised host candidate.
	pub fn new(udp: Arc<UdpSocket>, locals: Vec<SocketAddr>, inbound: Inbound) -> std::io::Result<Self> {
		if locals.is_empty() {
			return Err(std::io::Error::new(
				std::io::ErrorKind::InvalidInput,
				"no host candidate to receive on",
			));
		}
		Ok(Self { udp, locals, inbound })
	}

	/// The advertised host candidates, which are never empty.
	pub fn locals(&self) -> &[SocketAddr] {
		&self.locals
	}
}

/// The datagrams routed to one [`crate::Agent`], and the counters they are tallied in.
pub struct Inbound {
	/// Ends once every sender is dropped, which ends the agent.
	pub packets: mpsc::Receiver<Packet>,
	/// Counts `rx` where datagrams are routed, so a full queue's drops count too;
	/// the agent counts `tx`.
	pub wire: Wire,
}

/// Cumulative UDP payload bytes exchanged with one peer, excluding IP and UDP
/// headers. Clones share counters, which outlive the agent.
#[derive(Clone, Debug, Default)]
pub struct Wire(Arc<Counters>);

#[derive(Debug, Default)]
struct Counters {
	tx: AtomicU64,
	rx: AtomicU64,
}

/// A snapshot of a [`Wire`]'s counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
	/// Bytes the socket accepted, including ICE, DTLS, and retransmissions.
	pub tx: u64,
	/// Bytes routed to the peer, including those its full queue dropped.
	pub rx: u64,
}

impl Wire {
	/// Read both counters without consuming them.
	pub fn stats(&self) -> Stats {
		Stats {
			tx: self.0.tx.load(Ordering::Relaxed),
			rx: self.0.rx.load(Ordering::Relaxed),
		}
	}

	/// Count a datagram routed to this peer.
	pub fn received(&self, bytes: usize) {
		self.0.rx.fetch_add(bytes as u64, Ordering::Relaxed);
	}

	pub(crate) fn sent(&self, bytes: usize) {
		self.0.tx.fetch_add(bytes as u64, Ordering::Relaxed);
	}
}

/// Pump every datagram from a socket the caller owns 1:1 into a queue for
/// an agent, until the queue's receiver is dropped.
pub fn reader(socket: Arc<UdpSocket>) -> Inbound {
	let (tx, rx) = mpsc::channel(INBOX);
	let wire = Wire::default();
	let counted = wire.clone();
	tokio::spawn(async move {
		let mut buf = vec![0u8; 65_535];
		loop {
			tokio::select! {
				res = socket.recv_from(&mut buf) => match res {
					Ok((len, src)) => {
						counted.received(len);
						// Bounded like a socket buffer: drop on full.
						if let Err(mpsc::error::TrySendError::Closed(_)) = tx.try_send((buf[..len].to_vec(), canonical(src))) {
							break;
						}
					}
					Err(err) => {
						tracing::warn!(%err, "webrtc socket recv failed");
						break;
					}
				},
				_ = tx.closed() => break,
			}
		}
	});
	Inbound { packets: rx, wire }
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn construction_refuses_a_socket_without_advertised_candidates() {
		let udp = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
		let (_, packets) = mpsc::channel(INBOX);
		let inbound = Inbound {
			packets,
			wire: Wire::default(),
		};
		let Err(err) = Socket::new(udp, Vec::new(), inbound) else {
			panic!("an unusable socket escaped construction");
		};
		assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
	}
}
