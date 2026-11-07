use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use super::canonical;

/// One inbound UDP datagram and the address it came from.
pub type Packet = (Vec<u8>, SocketAddr);

/// Bound on a session's inbound datagram queue, sized like a socket buffer: past
/// it, datagrams are dropped rather than buffered, since ICE and SCTP tolerate loss
/// and a stalled session must not grow memory without limit.
pub const INBOX: usize = 256;

/// The UDP socket an [`super::Agent`] runs on: where it sends, what it receives,
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

/// The datagrams routed to one [`super::Agent`].
pub struct Inbound {
	/// Ends once every sender is dropped, which ends the agent.
	pub packets: mpsc::Receiver<Packet>,
}

/// Pump every datagram from a socket the caller owns 1:1 into a queue for
/// an agent, until the queue's receiver is dropped.
pub fn reader(socket: Arc<UdpSocket>) -> Inbound {
	let (tx, rx) = mpsc::channel(INBOX);
	tokio::spawn(async move {
		let mut buf = vec![0u8; 65_535];
		loop {
			tokio::select! {
				res = socket.recv_from(&mut buf) => match res {
					Ok((len, src)) => {
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
	Inbound { packets: rx }
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn construction_refuses_a_socket_without_advertised_candidates() {
		let udp = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
		let (_, packets) = mpsc::channel(INBOX);
		let inbound = Inbound { packets };
		let Err(err) = Socket::new(udp, Vec::new(), inbound) else {
			panic!("an unusable socket escaped construction");
		};
		assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
	}
}
