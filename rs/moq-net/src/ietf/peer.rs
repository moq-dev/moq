//! What the peer declared in its SETUP, and the slot the session tasks read it from.

use super::{cluster, solicit};

/// The Setup Options the peer sent us.
///
/// One value per session: both extensions ride the same SETUP, so they arrive together
/// and are read together.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Peer {
	/// The MoQ Cluster options: the peer's Hop ID and what it charges to cross.
	pub cluster: cluster::Peer,

	/// The MoQ Solicit options: what the peer requires to be solicited.
	pub solicit: solicit::Solicit,
}

/// Shared slot for [`Peer`], filled when the peer's SETUP is read.
///
/// The publisher blocks on this before its first advertisement: the SETUP decides both
/// what an advertisement carries (MoQ Cluster) and whether one may be sent unasked (MoQ
/// Solicit), so nothing can go out until it arrives. Cheap to clone; every handle
/// shares the same slot.
#[derive(Clone, Default)]
pub(crate) struct PeerSetup(kio::Shared<Option<Peer>>);

impl PeerSetup {
	/// Record what the peer declared. A SETUP carrying no options records the default
	/// (no extension negotiated, no requirements), which is what unblocks a waiter.
	///
	/// First write wins. The announce loops read this once and hold it for their
	/// lifetime while subscription serving re-reads it, so letting a later SETUP
	/// overwrite the identity would advertise under one exclusion and serve under
	/// another, which is how a routing loop gets back in.
	pub fn set(&self, peer: Peer) {
		let mut slot = self.0.lock();
		if slot.is_none() {
			*slot = Some(peer);
		}
	}

	/// Await the peer's SETUP.
	///
	/// The peer MUST send exactly one, so this resolves once that stream is read. Waits
	/// forever if it never does; the caller is a session task, cancelled when the driver
	/// drops.
	pub async fn get(&self) -> Peer {
		let slot = self
			.0
			.wait(|peer| match peer.is_some() {
				true => std::task::Poll::Ready(()),
				false => std::task::Poll::Pending,
			})
			.await;
		(*slot).expect("waited for Some")
	}

	/// What the peer declared, if its SETUP has already been read.
	///
	/// For a caller that would rather act on the default than spend a round trip
	/// finding out: a server has the client's SETUP before the session starts, while a
	/// client may still be waiting on the server's.
	pub fn peek(&self) -> Option<Peer> {
		*self.0.lock()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The announce loops read the peer's declaration once and hold it, while
	/// subscription serving re-reads it. A second SETUP overwriting the identity would
	/// split those two apart, so the first write is the one that counts.
	#[tokio::test]
	async fn first_write_wins() {
		let first = Peer {
			cluster: cluster::Peer {
				origin: Some(crate::Origin::new(42).unwrap()),
				cost: Some(3),
			},
			solicit: solicit::Solicit::default(),
		};

		let slot = PeerSetup::default();
		slot.set(first);
		slot.set(Peer {
			cluster: cluster::Peer {
				origin: Some(crate::Origin::new(99).unwrap()),
				cost: Some(0),
			},
			solicit: solicit::Solicit {
				announce: true,
				interest: true,
			},
		});

		assert_eq!(slot.peek(), Some(first));
		assert_eq!(slot.get().await, first);
	}

	/// Nothing to report until the peer's SETUP arrives, which is what lets a caller
	/// skip the wait and use the default.
	#[test]
	fn peek_is_empty_until_set() {
		let slot = PeerSetup::default();
		assert_eq!(slot.peek(), None);

		slot.set(Peer::default());
		assert_eq!(slot.peek(), Some(Peer::default()));
	}
}
